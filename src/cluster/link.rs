use std::env;
use std::ffi::OsString;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{self, AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, watch, Notify};
use tokio::task::JoinSet;
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use super::channel::Channels;
use super::{Cluster, LinkGuard, Rejection, StateSource, TransportAuth};
use crate::identity::ServerId;
use crate::protocol::{
    self, read_frame, write_message, Farewell, Hello, IncompatibleServer, PeerMessage, Refusal,
    Role, Welcome,
};
use crate::settings::ClusterSettings;

const PING_INTERVAL_ENV: &str = "AMUX_PING_INTERVAL_MS";
const CONTROL_CAPACITY: usize = 64;
const INTERACTIVE_CAPACITY: usize = 64;
const BULK_CAPACITY: usize = 16;

pub fn with_env(settings: ClusterSettings) -> Result<ClusterSettings> {
    with_ping_interval(settings, env::var_os(PING_INTERVAL_ENV))
}

fn with_ping_interval(
    mut settings: ClusterSettings,
    value: Option<OsString>,
) -> Result<ClusterSettings> {
    match value {
        Some(value) => {
            settings.ping_interval_ms = value
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|millis| *millis > 0)
                .with_context(|| format!("{PING_INTERVAL_ENV} must be a positive number"))?;
        }
        None if settings.ping_interval_ms == 0 => {
            bail!("cluster.ping_interval_ms must be a positive number")
        }
        None => {}
    }
    Ok(settings)
}

pub enum Handshake<R, W> {
    Linked(Link<R, W>),
    Refused {
        reason: Refusal,
        by_peer: bool,
        peer: Option<Hello>,
    },
    Stopping(Option<Hello>),
    Incompatible(Welcome),
}

pub struct Link<R, W> {
    reader: R,
    writer: W,
    peer: Hello,
    guard: LinkGuard,
    lanes: Lanes,
}

impl<R, W> Link<R, W> {
    pub fn peer(&self) -> &Hello {
        &self.peer
    }
}

pub(super) struct LinkHandle {
    pub control: mpsc::Sender<PeerMessage>,
    pub stop: Arc<Notify>,
    pub finish: watch::Sender<bool>,
    pub stats: Arc<LinkStats>,
    pub channels: Arc<Channels>,
}

struct Lanes {
    control: mpsc::Sender<PeerMessage>,
    bulk: mpsc::Sender<PeerMessage>,
    outbound: Outbound,
    stop: Arc<Notify>,
    finishing: watch::Receiver<bool>,
    stats: Arc<LinkStats>,
    channels: Arc<Channels>,
}

struct Outbound {
    control: mpsc::Receiver<PeerMessage>,
    interactive: mpsc::Receiver<PeerMessage>,
    bulk: mpsc::Receiver<PeerMessage>,
}

fn new_lanes() -> (LinkHandle, Lanes) {
    let (control, control_lane) = mpsc::channel(CONTROL_CAPACITY);
    let (interactive, interactive_lane) = mpsc::channel(INTERACTIVE_CAPACITY);
    let (bulk, bulk_lane) = mpsc::channel(BULK_CAPACITY);
    let (finish, finishing) = watch::channel(false);
    let stop = Arc::new(Notify::new());
    let stats = Arc::new(LinkStats::new());
    let channels = Channels::new(
        bulk.clone(),
        interactive,
        control.clone(),
        Arc::clone(&stop),
    );
    let handle = LinkHandle {
        control: control.clone(),
        stop: Arc::clone(&stop),
        finish,
        stats: Arc::clone(&stats),
        channels: Arc::clone(&channels),
    };
    let lanes = Lanes {
        control,
        bulk,
        outbound: Outbound {
            control: control_lane,
            interactive: interactive_lane,
            bulk: bulk_lane,
        },
        stop,
        finishing,
        stats,
        channels,
    };
    (handle, lanes)
}

pub async fn dial<R, W>(
    cluster: &Arc<Cluster>,
    reader: R,
    writer: W,
    auth: TransportAuth,
) -> Result<Handshake<R, W>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut stream = io::join(reader, writer);
    if let Err(err) = protocol::greet(&mut stream, Role::Peer, cluster.version()).await {
        return match err.downcast::<IncompatibleServer>() {
            Ok(IncompatibleServer {
                server: Some(welcome),
                ..
            }) => Ok(Handshake::Incompatible(welcome)),
            Ok(IncompatibleServer { server: None, .. }) => {
                bail!("the server closed the connection before answering the greeting")
            }
            Err(err) => Err(err),
        };
    }
    let (reader, writer) = stream.into_inner();
    exchange(cluster, reader, writer, true, auth).await
}

pub async fn accept<R, W>(
    cluster: &Arc<Cluster>,
    reader: R,
    writer: W,
    auth: TransportAuth,
) -> Result<Handshake<R, W>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    exchange(cluster, reader, writer, false, auth).await
}

async fn exchange<R, W>(
    cluster: &Arc<Cluster>,
    mut reader: R,
    mut writer: W,
    dialed: bool,
    auth: TransportAuth,
) -> Result<Handshake<R, W>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if let Err(reason) = cluster.admit(&auth) {
        let _ = write_message(&mut writer, &PeerMessage::Refused(reason)).await;
        return Ok(Handshake::Refused {
            reason,
            by_peer: false,
            peer: None,
        });
    }
    write_message(&mut writer, &PeerMessage::Hello(cluster.hello())).await?;
    let peer = match receive(&mut reader).await? {
        PeerMessage::Hello(hello) => hello,
        PeerMessage::Refused(reason) => return Ok(refused_by_peer(reason, None)),
        PeerMessage::Goodbye(Farewell::Stopped) => return Ok(Handshake::Stopping(None)),
        other => bail!("expected a hello, got {other:?}"),
    };

    if let Err(reason) = cluster.check(&peer, &auth) {
        let _ = write_message(&mut writer, &PeerMessage::Refused(reason)).await;
        return Ok(Handshake::Refused {
            reason,
            by_peer: false,
            peer: Some(peer),
        });
    }

    let (handle, lanes) = new_lanes();
    let decided = if cluster.id() < peer.id {
        if let Some(outcome) = await_verdict(cluster, &mut reader, &peer).await? {
            return Ok(outcome);
        }
        cluster
            .decide(&peer, dialed, handle, &auth)
            .map(|(guard, generation)| (guard, Some(generation)))
    } else {
        let generation = cluster.next_generation();
        let _ = write_message(&mut writer, &PeerMessage::LinkConfirmed { generation }).await;
        let generation = match receive(&mut reader).await? {
            PeerMessage::LinkConfirmed { generation } => generation,
            PeerMessage::Refused(reason) => return Ok(refused_by_peer(reason, Some(peer))),
            PeerMessage::Goodbye(Farewell::Stopped) => return Ok(stopping(cluster, peer)),
            other => bail!("expected the link verdict, got {other:?}"),
        };
        cluster
            .adopt(&peer, dialed, generation, handle, &auth)
            .map(|guard| (guard, None))
    };

    match decided {
        Ok((guard, confirmation)) => {
            if let Some(generation) = confirmation {
                write_message(&mut writer, &PeerMessage::LinkConfirmed { generation }).await?;
            }
            Ok(Handshake::Linked(Link {
                reader,
                writer,
                peer,
                guard,
                lanes,
            }))
        }
        Err(Rejection::Refused(reason)) => {
            let _ = write_message(&mut writer, &PeerMessage::Refused(reason)).await;
            Ok(Handshake::Refused {
                reason,
                by_peer: false,
                peer: Some(peer),
            })
        }
        Err(Rejection::Stopping) => {
            let _ = write_message(&mut writer, &PeerMessage::Goodbye(Farewell::Stopped)).await;
            bail!("this server is shutting down")
        }
    }
}

async fn await_verdict<R, W>(
    cluster: &Arc<Cluster>,
    reader: &mut R,
    peer: &Hello,
) -> Result<Option<Handshake<R, W>>>
where
    R: AsyncRead + Unpin,
{
    match receive(reader).await? {
        PeerMessage::LinkConfirmed { .. } => Ok(None),
        PeerMessage::Refused(reason) => Ok(Some(refused_by_peer(reason, Some(peer.clone())))),
        PeerMessage::Goodbye(Farewell::Stopped) => Ok(Some(stopping(cluster, peer.clone()))),
        other => bail!("expected the peer's approval, got {other:?}"),
    }
}

fn refused_by_peer<R, W>(reason: Refusal, peer: Option<Hello>) -> Handshake<R, W> {
    Handshake::Refused {
        reason,
        by_peer: true,
        peer,
    }
}

fn stopping<R, W>(cluster: &Cluster, peer: Hello) -> Handshake<R, W> {
    cluster.mark_stopped(peer.id);
    Handshake::Stopping(Some(peer))
}

async fn receive<R: AsyncRead + Unpin>(reader: &mut R) -> Result<PeerMessage> {
    let payload = read_frame(reader)
        .await?
        .context("the peer closed the link during the handshake")?;
    postcard::from_bytes(&payload).context("decoding a handshake message")
}

#[derive(Debug)]
pub enum LinkEnd {
    Closed,
    Goodbye,
    Silent(Duration),
    Overflow,
    Stopped,
    Finished,
    ServerGone,
    Failed(anyhow::Error),
}

impl fmt::Display for LinkEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("the peer closed the link"),
            Self::Goodbye => f.write_str("the peer stopped"),
            Self::Silent(limit) => write!(f, "the peer was silent for {limit:?}"),
            Self::Overflow => f.write_str("the peer stopped reading"),
            Self::Stopped => f.write_str("the link was closed here"),
            Self::Finished => f.write_str("this server said goodbye"),
            Self::ServerGone => f.write_str("this server is going away"),
            Self::Failed(err) => write!(f, "{err:#}"),
        }
    }
}

pub async fn run<R, W>(link: Link<R, W>, cluster: &Arc<Cluster>) -> LinkEnd
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let Link {
        reader,
        writer,
        peer,
        guard,
        lanes,
    } = link;
    let Lanes {
        control,
        bulk,
        outbound,
        stop,
        finishing,
        stats,
        channels,
    } = lanes;

    let mut tasks = JoinSet::new();
    tasks.spawn(write_lanes(writer, outbound, finishing));
    tasks.spawn(read_frames(
        reader,
        Reader {
            cluster: Arc::clone(cluster),
            peer: peer.id,
            control: control.clone(),
            stats: Arc::clone(&stats),
            channels: Arc::clone(&channels),
        },
    ));
    tasks.spawn(publish(cluster.source(), bulk));
    let settings = cluster.settings();
    tasks.spawn(keep_alive(
        control,
        stats,
        settings.ping_interval(),
        settings.silence_limit(),
    ));

    let end = tokio::select! {
        Some(joined) = tasks.join_next() => joined
            .unwrap_or_else(|err| LinkEnd::Failed(anyhow!("a link task failed: {err}"))),
        () = stop.notified() => LinkEnd::Stopped,
    };
    tasks.shutdown().await;
    channels.link_down();
    drop(guard);
    end
}

async fn write_lanes<W>(
    mut writer: W,
    outbound: Outbound,
    mut finishing: watch::Receiver<bool>,
) -> LinkEnd
where
    W: AsyncWrite + Unpin,
{
    let Outbound {
        mut control,
        mut interactive,
        mut bulk,
    } = outbound;
    loop {
        let message = tokio::select! {
            biased;
            Some(message) = control.recv() => message,
            requested = finish_requested(&mut finishing) => {
                if !requested {
                    return LinkEnd::Stopped;
                }
                while let Ok(message) = control.try_recv() {
                    if let Err(err) = write_message(&mut writer, &message).await {
                        return LinkEnd::Failed(err.context("writing to the link"));
                    }
                }
                let _ = writer.shutdown().await;
                return LinkEnd::Finished;
            }
            Some(message) = interactive.recv() => message,
            message = bulk.recv() => match message {
                Some(message) => message,
                None => return LinkEnd::ServerGone,
            },
        };
        if let Err(err) = write_message(&mut writer, &message).await {
            return LinkEnd::Failed(err.context("writing to the link"));
        }
    }
}

async fn finish_requested(finishing: &mut watch::Receiver<bool>) -> bool {
    finishing.wait_for(|finishing| *finishing).await.is_ok()
}

struct Reader {
    cluster: Arc<Cluster>,
    peer: ServerId,
    control: mpsc::Sender<PeerMessage>,
    stats: Arc<LinkStats>,
    channels: Arc<Channels>,
}

async fn read_frames<R>(mut reader: R, state: Reader) -> LinkEnd
where
    R: AsyncRead + Unpin,
{
    let Reader {
        cluster,
        peer,
        control,
        stats,
        channels,
    } = state;
    loop {
        let payload = match read_frame(&mut reader).await {
            Ok(Some(payload)) => payload,
            Ok(None) => return LinkEnd::Closed,
            Err(err) => return LinkEnd::Failed(err.context("reading from the link")),
        };
        stats.heard();
        let len = payload.len();
        let message = match postcard::from_bytes::<PeerMessage>(&payload) {
            Ok(message) => message,
            Err(err) => {
                warn!(%peer, len, "dropping a peer frame that failed to decode: {err}");
                continue;
            }
        };
        let kept_up = match message {
            PeerMessage::Ping(nonce) => match control.try_send(PeerMessage::Pong(nonce)) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => false,
                Err(TrySendError::Closed(_)) => return LinkEnd::Closed,
            },
            PeerMessage::Pong(nonce) => {
                stats.ponged(nonce);
                true
            }
            PeerMessage::Snapshot(snapshot) => {
                cluster.apply_snapshot(peer, snapshot);
                true
            }
            PeerMessage::Event(event) => {
                cluster.apply_event(peer, event);
                true
            }
            PeerMessage::Goodbye(Farewell::Stopped) => {
                channels.host_stopped();
                cluster.mark_stopped(peer);
                return LinkEnd::Goodbye;
            }
            PeerMessage::ChannelOpen { id, first } => {
                if let Some(channel) = channels.accept(id, first, len) {
                    if let Some(source) = cluster.source().upgrade() {
                        source.serve_channel(channel);
                    }
                }
                true
            }
            PeerMessage::ChannelToHost { id, message } => channels.to_host(id, message, len),
            PeerMessage::ChannelToClient { id, message } => channels.to_client(id, message, len),
            PeerMessage::ChannelCredit { id, credit } => {
                channels.credit(id, credit);
                true
            }
            PeerMessage::ChannelClose { id, from_opener } => {
                channels.closed(id, from_opener);
                true
            }
            PeerMessage::Trust(update) => {
                debug!(
                    %peer,
                    trusted = update.trusted.len(),
                    forgotten = update.forgotten.len(),
                    "received a trust update"
                );
                cluster.merge_trust(peer, &update);
                true
            }
            other => {
                warn!(%peer, "ignoring {other:?} on an established link");
                true
            }
        };
        if !kept_up {
            return LinkEnd::Overflow;
        }
    }
}

async fn publish(source: Weak<dyn StateSource>, bulk: mpsc::Sender<PeerMessage>) -> LinkEnd {
    loop {
        let Some(state) = source.upgrade() else {
            return LinkEnd::ServerGone;
        };
        let (snapshot, mut events) = state.subscribe();
        drop(state);
        let mut seq = snapshot.seq;
        if bulk.send(PeerMessage::Snapshot(snapshot)).await.is_err() {
            return LinkEnd::Closed;
        }
        loop {
            match events.recv().await {
                Ok(event) if event.seq > seq => {
                    seq = event.seq;
                    if bulk.send(PeerMessage::Event(event)).await.is_err() {
                        return LinkEnd::Closed;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    debug!(missed, "resending a snapshot after falling behind");
                    break;
                }
                Err(RecvError::Closed) => return LinkEnd::ServerGone,
            }
        }
    }
}

async fn keep_alive(
    control: mpsc::Sender<PeerMessage>,
    stats: Arc<LinkStats>,
    interval: Duration,
    limit: Duration,
) -> LinkEnd {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut nonce = 0;
    loop {
        ticks.tick().await;
        if stats.silent_for() > limit {
            return LinkEnd::Silent(limit);
        }
        nonce += 1;
        stats.pinged(nonce);
        if let Err(TrySendError::Closed(_)) = control.try_send(PeerMessage::Ping(nonce)) {
            return LinkEnd::Closed;
        }
    }
}

#[derive(Debug)]
pub(super) struct LinkStats {
    inner: Mutex<StatsInner>,
}

#[derive(Debug)]
struct StatsInner {
    last_heard: Instant,
    ping: Option<(u64, Instant)>,
    latency: Option<Duration>,
}

impl LinkStats {
    fn new() -> Self {
        Self {
            inner: Mutex::new(StatsInner {
                last_heard: Instant::now(),
                ping: None,
                latency: None,
            }),
        }
    }

    pub fn latency(&self) -> Option<Duration> {
        self.lock().latency
    }

    fn heard(&self) {
        self.lock().last_heard = Instant::now();
    }

    fn silent_for(&self) -> Duration {
        self.lock().last_heard.elapsed()
    }

    fn pinged(&self, nonce: u64) {
        self.lock().ping = Some((nonce, Instant::now()));
    }

    fn ponged(&self, nonce: u64) {
        let mut inner = self.lock();
        if let Some((sent, at)) = inner.ping {
            if sent == nonce {
                inner.latency = Some(at.elapsed());
                inner.ping = None;
            }
        }
    }

    fn lock(&self) -> MutexGuard<'_, StatsInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tokio::io::{duplex, split, DuplexStream, ReadHalf, WriteHalf};
    use tokio::sync::broadcast;

    use super::*;
    use crate::cluster::channel::CREDIT_BYTES;
    use crate::cluster::noise;
    use crate::cluster::{
        Channel, ChannelEnd, ClusterOptions, NoiseKey, TrustStore, Voucher, CREDIT_WINDOW,
    };
    use crate::identity::{Incarnation, ServerIdentity};
    use crate::protocol::{
        read_message, ChannelId, ClientMessage, Duplex, Event, ServerMessage, ServerState,
        ServerStatus, SessionId, SessionInfo, Snapshot, StateEvent, TcpKind, Version,
        WindowSummary, PROTOCOL_MAJOR,
    };
    use crate::settings::DiscoverySettings;

    type HostEnd = Duplex<ClientMessage, ServerMessage>;

    type Reader = ReadHalf<DuplexStream>;
    type Writer = WriteHalf<DuplexStream>;
    type Outcome = Handshake<Reader, Writer>;

    const LOWER: &str = "00000000000000000000000000000001";
    const HIGHER: &str = "00000000000000000000000000000002";
    const HIGHEST: &str = "00000000000000000000000000000003";
    const PIPE_CAPACITY: usize = 64 * 1024;
    const PATIENCE: Duration = Duration::from_secs(10);

    struct FakeServer {
        incarnation: Incarnation,
        sessions: Vec<SessionInfo>,
        events: broadcast::Sender<Event>,
        hosted: mpsc::UnboundedSender<HostEnd>,
    }

    impl StateSource for FakeServer {
        fn subscribe(&self) -> (Snapshot, broadcast::Receiver<Event>) {
            let snapshot = Snapshot {
                incarnation: self.incarnation,
                seq: 0,
                state: ServerState {
                    sessions: self.sessions.clone(),
                    ..ServerState::default()
                },
            };
            (snapshot, self.events.subscribe())
        }

        fn refresh_peers(&self) {}

        fn serve_channel(self: Arc<Self>, channel: HostEnd) {
            let _ = self.hosted.send(channel);
        }
    }

    struct Node {
        cluster: Arc<Cluster>,
        server: Arc<FakeServer>,
        hosted: tokio::sync::Mutex<mpsc::UnboundedReceiver<HostEnd>>,
    }

    impl Node {
        async fn next_hosted(&self) -> HostEnd {
            tokio::time::timeout(PATIENCE, self.hosted.lock().await.recv())
                .await
                .expect("timed out waiting for a hosted channel")
                .expect("the fake server is gone")
        }
    }

    struct NodeBuilder {
        id: &'static str,
        name: &'static str,
        incarnation: Incarnation,
        version: Version,
        settings: ClusterSettings,
        sessions: Vec<SessionInfo>,
    }

    impl NodeBuilder {
        fn incarnation(mut self, incarnation: Incarnation) -> Self {
            self.incarnation = incarnation;
            self
        }

        fn version(mut self, version: Version) -> Self {
            self.version = version;
            self
        }

        fn settings(mut self, settings: ClusterSettings) -> Self {
            self.settings = settings;
            self
        }

        fn session(mut self, name: &str) -> Self {
            self.sessions
                .push(session(self.sessions.len() as u64 + 1, name));
            self
        }

        fn build(self) -> Node {
            let (hosted_sender, hosted) = mpsc::unbounded_channel();
            let server = Arc::new(FakeServer {
                incarnation: self.incarnation,
                sessions: self.sessions,
                events: broadcast::channel(16).0,
                hosted: hosted_sender,
            });
            let weak: Weak<FakeServer> = Arc::downgrade(&server);
            let options = ClusterOptions {
                identity: ServerIdentity {
                    id: self.id.parse().unwrap(),
                    name: self.name.into(),
                    incarnation: self.incarnation,
                },
                version: self.version,
                socket_name: "test".into(),
                settings: self.settings,
                state_dir: None,
                servers: BTreeMap::new(),
                discovery: DiscoverySettings::default(),
                trust: TrustStore::default(),
                key: NoiseKey::generate().unwrap(),
            };
            Node {
                cluster: Cluster::new(options, weak),
                server,
                hosted: tokio::sync::Mutex::new(hosted),
            }
        }
    }

    fn node(id: &'static str, name: &'static str) -> NodeBuilder {
        NodeBuilder {
            id,
            name,
            incarnation: Incarnation::random().unwrap(),
            version: Version::current(),
            settings: ClusterSettings::default(),
            sessions: Vec::new(),
        }
    }

    fn session(id: u64, name: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            name: name.into(),
            windows: vec![WindowSummary {
                index: 0,
                name: "sh".into(),
                panes: 1,
            }],
            attached_clients: 0,
            last_activity: std::time::SystemTime::UNIX_EPOCH,
            project: None,
            branch: None,
        }
    }

    fn accepting(
        acceptor: &Arc<Cluster>,
        stream: DuplexStream,
    ) -> tokio::task::JoinHandle<Outcome> {
        let acceptor = Arc::clone(acceptor);
        tokio::spawn(async move {
            let mut stream = stream;
            let name = acceptor.identity.name.clone();
            let role = protocol::accept(&mut stream, acceptor.version(), &name)
                .await
                .unwrap();
            assert_eq!(role, Some(Role::Peer));
            let (reader, writer) = split(stream);
            accept(&acceptor, reader, writer, TransportAuth::Ssh)
                .await
                .unwrap()
        })
    }

    async fn connect(dialer: &Node, acceptor: &Node) -> (Outcome, Outcome) {
        let (dial_end, accept_end) = duplex(PIPE_CAPACITY);
        let accepted = accepting(&acceptor.cluster, accept_end);
        let (reader, writer) = split(dial_end);
        let dialed = dial(&dialer.cluster, reader, writer, TransportAuth::Ssh)
            .await
            .unwrap();
        (dialed, accepted.await.unwrap())
    }

    fn describe(outcome: &Outcome) -> String {
        match outcome {
            Handshake::Linked(link) => format!("linked to {}", link.peer().name),
            Handshake::Refused {
                reason, by_peer, ..
            } => format!("refused ({reason}), by peer: {by_peer}"),
            Handshake::Stopping(_) => "stopping".into(),
            Handshake::Incompatible(welcome) => format!("incompatible with {}", welcome.version),
        }
    }

    fn linked(outcome: Outcome) -> Link<Reader, Writer> {
        match outcome {
            Handshake::Linked(link) => link,
            other => panic!("expected a link, got {}", describe(&other)),
        }
    }

    fn refusal(outcome: &Outcome) -> (Refusal, bool) {
        match outcome {
            Handshake::Refused {
                reason, by_peer, ..
            } => (*reason, *by_peer),
            other => panic!("expected a refusal, got {}", describe(other)),
        }
    }

    fn links(node: &Node) -> Vec<(String, bool, Incarnation)> {
        node.cluster
            .links()
            .into_iter()
            .map(|link| (link.name, link.dialed, link.incarnation))
            .collect()
    }

    async fn stopped<R, W>(link: &Link<R, W>) -> bool {
        tokio::time::timeout(Duration::ZERO, link.lanes.stop.notified())
            .await
            .is_ok()
    }

    async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn read_peer_message(stream: &mut DuplexStream) -> PeerMessage {
        let payload = read_frame(stream).await.unwrap().unwrap();
        postcard::from_bytes(&payload).unwrap()
    }

    fn raw_hello(id: &str, name: &str, version: Version) -> PeerMessage {
        PeerMessage::Hello(Hello {
            id: id.parse().unwrap(),
            incarnation: Incarnation::random().unwrap(),
            name: name.into(),
            version,
            peers: Vec::new(),
            public_key: None,
        })
    }

    async fn raw_peer(node: &Node, capacity: usize) -> (DuplexStream, Link<Reader, Writer>) {
        let (mut raw, ours) = duplex(capacity);
        let accepted = accepting(&node.cluster, ours);
        protocol::greet(&mut raw, Role::Peer, &Version::current())
            .await
            .unwrap();
        write_message(&mut raw, &raw_hello(HIGHEST, "raw", Version::current()))
            .await
            .unwrap();
        assert!(matches!(
            read_peer_message(&mut raw).await,
            PeerMessage::Hello(_)
        ));
        write_message(&mut raw, &PeerMessage::LinkConfirmed { generation: 1 })
            .await
            .unwrap();
        assert!(matches!(
            read_peer_message(&mut raw).await,
            PeerMessage::LinkConfirmed { .. }
        ));
        (raw, linked(accepted.await.unwrap()))
    }

    #[tokio::test]
    async fn a_handshake_links_both_sides_exactly_once() {
        for (dialer, acceptor) in [(LOWER, HIGHER), (HIGHER, LOWER)] {
            let dialer = node(dialer, "dialer").build();
            let acceptor = node(acceptor, "acceptor").build();

            let (dialed, accepted) = connect(&dialer, &acceptor).await;
            let (dialed, accepted) = (linked(dialed), linked(accepted));

            assert_eq!(dialed.peer().name, "acceptor");
            assert_eq!(accepted.peer().name, "dialer");
            assert_eq!(
                links(&dialer),
                vec![("acceptor".into(), true, acceptor.server.incarnation)]
            );
            assert_eq!(
                links(&acceptor),
                vec![("dialer".into(), false, dialer.server.incarnation)]
            );

            drop((dialed, accepted));
            assert!(dialer.cluster.links().is_empty());
            assert!(acceptor.cluster.links().is_empty());
        }
    }

    #[tokio::test]
    async fn a_noise_link_vouched_by_pairing_trusts_both_keys_first_hand() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let (dial_end, accept_end) = duplex(PIPE_CAPACITY);
        let acceptor = Arc::clone(&higher.cluster);
        let accepted = tokio::spawn(async move {
            let mut stream = accept_end;
            let kind = noise::read_opening(&mut stream).await?.unwrap();
            let secured = noise::respond(stream, &acceptor.noise_key(), kind).await?;
            acceptor.accept_noise(secured, Voucher::Pairing).await
        });

        let secured = noise::initiate(dial_end, &lower.cluster.noise_key(), TcpKind::Link)
            .await
            .unwrap();
        let auth = TransportAuth::Noise {
            key: secured.remote,
            vouched_by: Voucher::Pairing,
        };
        let (reader, writer) = split(secured.stream);
        let link = linked(dial(&lower.cluster, reader, writer, auth).await.unwrap());

        let trusted = |node: &Node, peer: &Node| {
            node.cluster.members().trust.key_of(peer.cluster.id())
                == Some(peer.cluster.public_key())
        };
        assert!(trusted(&lower, &higher));
        eventually("the acceptor to trust the dialer", || {
            trusted(&higher, &lower)
        })
        .await;
        let transports: Vec<String> = lower
            .cluster
            .links()
            .iter()
            .map(|link| format!("{} {:?}", link.transport, link.key))
            .collect();
        assert_eq!(
            transports,
            [format!("noise {:?}", Some(higher.cluster.public_key()))]
        );

        drop(link);
        tokio::time::timeout(PATIENCE, accepted)
            .await
            .expect("the accepted link never ended")
            .unwrap()
            .unwrap();
        assert!(higher.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn a_different_major_in_the_greeting_is_incompatible() {
        let future = Version {
            release: "9.0.0".into(),
            major: PROTOCOL_MAJOR + 1,
            minor: 0,
        };
        let dialer = node(LOWER, "future").version(future).build();
        let acceptor = node(HIGHER, "current").build();
        let (dial_end, mut accept_end) = duplex(PIPE_CAPACITY);
        let refused = tokio::spawn(async move {
            protocol::accept(&mut accept_end, &Version::current(), "current").await
        });

        let (reader, writer) = split(dial_end);
        match dial(&dialer.cluster, reader, writer, TransportAuth::Ssh)
            .await
            .unwrap()
        {
            Handshake::Incompatible(welcome) => {
                assert_eq!(welcome.server_name, "current");
                assert_eq!(welcome.version, Version::current());
            }
            other => panic!("expected an incompatible peer, got {}", describe(&other)),
        }
        assert!(refused.await.unwrap().is_err());
        assert!(acceptor.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn a_hello_with_another_major_is_refused_as_incompatible() {
        let acceptor = node(HIGHER, "current").build();
        let (mut raw, ours) = duplex(PIPE_CAPACITY);
        let accepted = accepting(&acceptor.cluster, ours);

        protocol::greet(&mut raw, Role::Peer, &Version::current())
            .await
            .unwrap();
        let future = Version {
            major: PROTOCOL_MAJOR + 1,
            ..Version::current()
        };
        write_message(&mut raw, &raw_hello(LOWER, "future", future))
            .await
            .unwrap();

        assert!(matches!(
            read_peer_message(&mut raw).await,
            PeerMessage::Hello(_)
        ));
        assert_eq!(
            read_peer_message(&mut raw).await,
            PeerMessage::Refused(Refusal::Incompatible)
        );
        assert_eq!(
            refusal(&accepted.await.unwrap()),
            (Refusal::Incompatible, false)
        );
        assert!(acceptor.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn a_name_held_by_another_online_server_is_refused() {
        let hub = node(LOWER, "hub").build();
        let first = node(HIGHER, "laptop").build();
        let second = node(HIGHEST, "laptop").build();
        let (dialed, accepted) = connect(&first, &hub).await;
        let _first_link = (linked(dialed), linked(accepted));

        let (dialed, accepted) = connect(&second, &hub).await;

        assert_eq!(refusal(&dialed), (Refusal::NameTaken, true));
        assert_eq!(refusal(&accepted), (Refusal::NameTaken, false));
        assert_eq!(
            links(&hub),
            vec![("laptop".into(), false, first.server.incarnation)]
        );
        assert!(second.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn two_servers_with_the_same_name_refuse_each_other() {
        let lower = node(LOWER, "desk").build();
        let higher = node(HIGHER, "desk").build();

        let (dialed, accepted) = connect(&lower, &higher).await;

        assert_eq!(refusal(&dialed).0, Refusal::NameTaken);
        assert_eq!(refusal(&accepted).0, Refusal::NameTaken);
        assert!(lower.cluster.links().is_empty());
        assert!(higher.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn dialing_this_server_is_refused_as_a_self_dial() {
        let lonely = node(LOWER, "lonely").build();

        let (dialed, accepted) = connect(&lonely, &lonely).await;

        assert_eq!(refusal(&dialed).0, Refusal::SelfDial);
        assert_eq!(refusal(&accepted).0, Refusal::SelfDial);
        assert!(lonely.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn a_second_link_is_refused_when_the_lower_id_dialed_the_first() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let (dialed, accepted) = connect(&lower, &higher).await;
        let first = (linked(dialed), linked(accepted));

        let (dialed, accepted) = connect(&higher, &lower).await;

        assert_eq!(refusal(&dialed), (Refusal::Duplicate, true));
        assert_eq!(refusal(&accepted), (Refusal::Duplicate, false));
        assert_eq!(
            links(&lower),
            vec![("high".into(), true, higher.server.incarnation)]
        );
        assert_eq!(
            links(&higher),
            vec![("low".into(), false, lower.server.incarnation)]
        );
        assert!(!stopped(&first.0).await);
    }

    #[tokio::test]
    async fn the_link_the_lower_id_dials_replaces_one_the_higher_id_dialed() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let (dialed, accepted) = connect(&higher, &lower).await;
        let (old_higher_side, old_lower_side) = (linked(dialed), linked(accepted));
        assert!(!links(&lower)[0].1);

        let (dialed, accepted) = connect(&lower, &higher).await;
        let _new = (linked(dialed), linked(accepted));

        assert!(stopped(&old_lower_side).await);
        assert!(stopped(&old_higher_side).await);
        drop((old_higher_side, old_lower_side));
        assert_eq!(
            links(&lower),
            vec![("high".into(), true, higher.server.incarnation)]
        );
        assert_eq!(
            links(&higher),
            vec![("low".into(), false, lower.server.incarnation)]
        );
    }

    #[tokio::test]
    async fn a_confirmation_older_than_the_current_link_is_ignored() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let hello = lower.cluster.hello();

        let ssh = TransportAuth::Ssh;
        let (handle, _newer_lanes) = new_lanes();
        let _newer = higher
            .cluster
            .adopt(&hello, false, 7, handle, &ssh)
            .ok()
            .unwrap();
        let (handle, _stale_lanes) = new_lanes();
        let stale = higher.cluster.adopt(&hello, true, 3, handle, &ssh);

        assert!(matches!(stale, Err(Rejection::Refused(Refusal::Duplicate))));
        assert_eq!(
            links(&higher),
            vec![("low".into(), false, lower.server.incarnation)]
        );
    }

    #[tokio::test]
    async fn a_restarted_peer_replaces_its_half_open_link_at_once() {
        for restarted_id in [LOWER, HIGHER] {
            let (stable_id, stable_name) = if restarted_id == LOWER {
                (HIGHER, "stable")
            } else {
                (LOWER, "stable")
            };
            let stable = node(stable_id, stable_name).build();
            let before = node(restarted_id, "restarted").build();
            let (dialed, accepted) = connect(&before, &stable).await;
            let (_half_open, stale) = (linked(dialed), linked(accepted));

            let after = node(restarted_id, "restarted")
                .incarnation(Incarnation::random().unwrap())
                .build();
            let (dialed, accepted) = connect(&after, &stable).await;
            let _fresh = (linked(dialed), linked(accepted));

            assert!(stopped(&stale).await, "restarted id {restarted_id}");
            drop(stale);
            assert_eq!(
                links(&stable),
                vec![("restarted".into(), false, after.server.incarnation)]
            );
        }
    }

    #[tokio::test]
    async fn a_server_that_is_shutting_down_says_goodbye_instead_of_linking() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        lower.cluster.shutdown().await;

        let (dial_end, accept_end) = duplex(PIPE_CAPACITY);
        let refused = tokio::spawn({
            let lower = Arc::clone(&lower.cluster);
            async move {
                let mut stream = accept_end;
                protocol::accept(&mut stream, lower.version(), "low")
                    .await
                    .unwrap();
                let (reader, writer) = split(stream);
                accept(&lower, reader, writer, TransportAuth::Ssh)
                    .await
                    .map(|_| ())
            }
        });
        let (reader, writer) = split(dial_end);
        let outcome = dial(&higher.cluster, reader, writer, TransportAuth::Ssh)
            .await
            .unwrap();

        assert!(matches!(outcome, Handshake::Stopping(Some(_))));
        assert!(refused.await.unwrap().is_err());
        assert!(higher.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn linked_peers_cache_each_others_snapshots_and_events() {
        let lower = node(LOWER, "low").session("work").build();
        let higher = node(HIGHER, "high").build();
        let (dialed, accepted) = connect(&lower, &higher).await;
        let lower_run = tokio::spawn({
            let cluster = Arc::clone(&lower.cluster);
            let link = linked(dialed);
            async move { run(link, &cluster).await }
        });
        let higher_run = tokio::spawn({
            let cluster = Arc::clone(&higher.cluster);
            let link = linked(accepted);
            async move { run(link, &cluster).await }
        });

        let sessions_on_low = |higher: &Node| -> Vec<String> {
            higher
                .cluster
                .view()
                .into_iter()
                .filter(|server| server.name == "low")
                .flat_map(|server| server.sessions)
                .map(|session| session.name)
                .collect()
        };
        eventually("the snapshot", || sessions_on_low(&higher) == ["work"]).await;

        lower
            .server
            .events
            .send(Event {
                incarnation: lower.server.incarnation,
                seq: 1,
                event: StateEvent::SessionCreated(session(2, "later")),
            })
            .unwrap();
        eventually("the event", || {
            sessions_on_low(&higher) == ["later", "work"]
        })
        .await;
        eventually("a latency sample", || {
            higher
                .cluster
                .view()
                .iter()
                .any(|server| matches!(server.status, ServerStatus::Online { latency: Some(_) }))
        })
        .await;

        lower.cluster.drop_link("high").unwrap();
        assert!(matches!(lower_run.await.unwrap(), LinkEnd::Stopped));
        assert!(matches!(higher_run.await.unwrap(), LinkEnd::Closed));
        assert!(matches!(
            higher.cluster.view()[0].status,
            ServerStatus::Offline { stopped: false, .. }
        ));
    }

    #[tokio::test]
    async fn goodbye_marks_the_peer_stopped() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let (dialed, accepted) = connect(&lower, &higher).await;
        let lower_run = tokio::spawn({
            let cluster = Arc::clone(&lower.cluster);
            let link = linked(dialed);
            async move { run(link, &cluster).await }
        });
        let higher_run = tokio::spawn({
            let cluster = Arc::clone(&higher.cluster);
            let link = linked(accepted);
            async move { run(link, &cluster).await }
        });

        lower.cluster.shutdown().await;

        assert!(matches!(lower_run.await.unwrap(), LinkEnd::Finished));
        assert!(matches!(higher_run.await.unwrap(), LinkEnd::Goodbye));
        assert!(matches!(
            higher.cluster.view()[0].status,
            ServerStatus::Offline { stopped: true, .. }
        ));
    }

    #[tokio::test]
    async fn the_writer_sends_control_then_channel_input_then_bulk_messages() {
        let (control, control_lane) = mpsc::channel(CONTROL_CAPACITY);
        let (interactive, interactive_lane) = mpsc::channel(INTERACTIVE_CAPACITY);
        let (bulk, bulk_lane) = mpsc::channel(BULK_CAPACITY);
        let (_finish, finishing) = watch::channel(false);
        let id = ChannelId(1);
        let frame = |index: u8| PeerMessage::ChannelToClient {
            id,
            message: ServerMessage::Output(vec![index]),
        };
        let input = |index: u8| PeerMessage::ChannelToHost {
            id,
            message: ClientMessage::Input(vec![index]),
        };
        let credit = |index: u8| PeerMessage::ChannelCredit {
            id,
            credit: u32::from(index),
        };
        for index in 0..3 {
            bulk.send(frame(index)).await.unwrap();
            interactive.send(input(index)).await.unwrap();
            control.send(credit(index)).await.unwrap();
        }
        drop((control, interactive, bulk));

        let mut written = Vec::new();
        let outbound = Outbound {
            control: control_lane,
            interactive: interactive_lane,
            bulk: bulk_lane,
        };
        let end = write_lanes(&mut written, outbound, finishing).await;

        assert!(matches!(end, LinkEnd::ServerGone), "{end}");
        let mut frames = written.as_slice();
        let mut sent = Vec::new();
        while let Some(message) = read_message::<_, PeerMessage>(&mut frames).await.unwrap() {
            sent.push(message);
        }
        let expected: Vec<PeerMessage> = (0..3)
            .map(credit)
            .chain((0..3).map(input))
            .chain((0..3).map(frame))
            .collect();
        assert_eq!(sent, expected);
    }

    #[tokio::test]
    async fn a_silent_peer_is_dropped_after_the_missed_pings() {
        let settings = ClusterSettings {
            ping_interval_ms: 20,
            ..ClusterSettings::default()
        };
        let ours = node(LOWER, "ours").settings(settings).build();
        let (_raw, link) = raw_peer(&ours, PIPE_CAPACITY).await;

        let end = tokio::time::timeout(PATIENCE, run(link, &ours.cluster))
            .await
            .unwrap();

        assert!(matches!(end, LinkEnd::Silent(_)), "{end}");
        assert!(ours.cluster.links().is_empty());
    }

    #[tokio::test]
    async fn the_ping_interval_and_missed_pings_come_from_the_settings() {
        let settings = ClusterSettings {
            ping_interval_ms: 15,
            missed_pings: 2,
            ..ClusterSettings::default()
        };
        let ours = node(LOWER, "ours").settings(settings).build();
        let (_raw, link) = raw_peer(&ours, PIPE_CAPACITY).await;

        let started = Instant::now();
        let end = tokio::time::timeout(PATIENCE, run(link, &ours.cluster))
            .await
            .unwrap();

        match end {
            LinkEnd::Silent(limit) => assert_eq!(limit, Duration::from_millis(30)),
            other => panic!("expected a silent peer, got {other}"),
        }
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_ping_interval_variable_wins_over_the_settings() {
        let configured = ClusterSettings {
            ping_interval_ms: 7000,
            ..ClusterSettings::default()
        };
        let overridden = with_ping_interval(configured.clone(), Some("100".into())).unwrap();
        assert_eq!(overridden.ping_interval(), Duration::from_millis(100));
        assert_eq!(
            ClusterSettings {
                ping_interval_ms: 7000,
                ..overridden
            },
            configured
        );
        assert_eq!(
            with_ping_interval(configured.clone(), None).unwrap(),
            configured
        );

        for value in ["0", "fast", ""] {
            let error = with_ping_interval(configured.clone(), Some(value.into())).unwrap_err();
            assert!(
                error.to_string().contains(PING_INTERVAL_ENV),
                "{value:?}: {error}"
            );
        }
        let never = ClusterSettings {
            ping_interval_ms: 0,
            ..ClusterSettings::default()
        };
        let error = with_ping_interval(never.clone(), None).unwrap_err();
        assert!(
            error.to_string().contains("cluster.ping_interval_ms"),
            "{error}"
        );
        assert!(with_ping_interval(never, Some("100".into())).is_ok());
    }

    #[tokio::test]
    async fn the_reader_keeps_reading_while_the_peer_does_not() {
        let ours = node(LOWER, "ours").build();
        let (raw, link) = raw_peer(&ours, 4096).await;
        let (_unread, mut raw_writer) = split(raw);
        tokio::spawn(async move {
            let snapshot = PeerMessage::Snapshot(Snapshot {
                incarnation: Incarnation::random().unwrap(),
                seq: 1,
                state: ServerState {
                    sessions: vec![session(1, "remote")],
                    ..ServerState::default()
                },
            });
            write_message(&mut raw_writer, &snapshot).await?;
            for nonce in 0..100_000 {
                write_message(&mut raw_writer, &PeerMessage::Ping(nonce)).await?;
            }
            anyhow::Ok(())
        });

        let end = tokio::time::timeout(PATIENCE, run(link, &ours.cluster))
            .await
            .unwrap();

        assert!(matches!(end, LinkEnd::Overflow), "{end}");
        let cached: Vec<String> = ours
            .cluster
            .view()
            .into_iter()
            .flat_map(|server| server.sessions)
            .map(|session| session.name)
            .collect();
        assert_eq!(cached, ["remote"]);
    }

    type LinkRun = tokio::task::JoinHandle<LinkEnd>;

    async fn running(dialer: &Node, acceptor: &Node) -> (LinkRun, LinkRun) {
        let (dialed, accepted) = connect(dialer, acceptor).await;
        let spawn_run = |cluster: &Arc<Cluster>, link: Link<Reader, Writer>| {
            let cluster = Arc::clone(cluster);
            tokio::spawn(async move { run(link, &cluster).await })
        };
        (
            spawn_run(&dialer.cluster, linked(dialed)),
            spawn_run(&acceptor.cluster, linked(accepted)),
        )
    }

    async fn open(opener: &Node, host: &Node, first: ClientMessage) -> Channel {
        let peer = host.cluster.identity.id;
        opener.cluster.open_channel(peer, first).await.unwrap()
    }

    async fn received<T>(receiver: &mut mpsc::Receiver<T>) -> Option<T> {
        tokio::time::timeout(PATIENCE, receiver.recv())
            .await
            .expect("timed out waiting on a channel")
    }

    async fn from_host(channel: &mut Channel) -> Option<ServerMessage> {
        tokio::time::timeout(PATIENCE, channel.recv())
            .await
            .expect("timed out waiting for the host")
    }

    fn output(index: usize) -> ServerMessage {
        ServerMessage::Output(format!("frame {index}").into_bytes())
    }

    async fn all_channels_closed(nodes: &[&Node]) {
        eventually("every channel to close", || {
            nodes
                .iter()
                .all(|node| node.cluster.open_channels() == (0, 0))
        })
        .await;
    }

    #[tokio::test]
    async fn a_channel_carries_client_messages_to_the_host_and_replies_back() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;

        let mut channel = open(&lower, &higher, ClientMessage::ListSessions).await;
        let mut host = higher.next_hosted().await;
        assert_eq!(
            received(&mut host.incoming).await,
            Some(ClientMessage::ListSessions)
        );

        channel
            .send(ClientMessage::Input(b"ls\r".to_vec()))
            .await
            .unwrap();
        assert_eq!(
            received(&mut host.incoming).await,
            Some(ClientMessage::Input(b"ls\r".to_vec()))
        );

        host.outgoing
            .send(ServerMessage::Sessions(Vec::new()))
            .await
            .unwrap();
        assert_eq!(
            from_host(&mut channel).await,
            Some(ServerMessage::Sessions(Vec::new()))
        );

        drop(host);
        assert_eq!(from_host(&mut channel).await, None);
        assert_eq!(channel.end(), ChannelEnd::ClosedByHost);
        drop(channel);
        all_channels_closed(&[&lower, &higher]).await;
    }

    #[tokio::test]
    async fn both_servers_can_open_channels_to_each_other_over_one_link() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;

        let mut up = open(&lower, &higher, ClientMessage::ListSessions).await;
        let mut down = open(&higher, &lower, ClientMessage::ListCluster).await;
        assert_eq!(up.id(), down.id());

        let mut on_higher = higher.next_hosted().await;
        let mut on_lower = lower.next_hosted().await;
        assert_eq!(
            received(&mut on_higher.incoming).await,
            Some(ClientMessage::ListSessions)
        );
        assert_eq!(
            received(&mut on_lower.incoming).await,
            Some(ClientMessage::ListCluster)
        );
        on_higher.outgoing.send(output(1)).await.unwrap();
        on_lower.outgoing.send(output(2)).await.unwrap();
        assert_eq!(from_host(&mut up).await, Some(output(1)));
        assert_eq!(from_host(&mut down).await, Some(output(2)));
    }

    #[tokio::test]
    async fn the_host_sends_only_as_many_messages_as_the_opener_has_delivered() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;
        let mut channel = open(&lower, &higher, ClientMessage::ListSessions).await;
        let host = higher.next_hosted().await;

        let window = CREDIT_WINDOW as usize;
        for index in 0..window {
            host.outgoing.send(output(index)).await.unwrap();
        }
        for index in 0..window {
            assert_eq!(from_host(&mut channel).await, Some(output(index)));
        }
        host.outgoing.send(output(window)).await.unwrap();
        assert!(
            host.outgoing.try_send(output(window + 1)).is_err(),
            "the host rendered past its credit"
        );

        channel.delivered();
        assert_eq!(from_host(&mut channel).await, Some(output(window)));
        host.outgoing.send(output(window + 1)).await.unwrap();
        channel.delivered();
        assert_eq!(from_host(&mut channel).await, Some(output(window + 1)));
    }

    #[tokio::test]
    async fn the_host_stops_at_its_byte_budget_before_its_message_window() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;
        let mut channel = open(&lower, &higher, ClientMessage::ListSessions).await;
        let host = higher.next_hosted().await;

        let half_budget = |index: u8| ServerMessage::Output(vec![index; CREDIT_BYTES / 2]);
        for index in 0..2 {
            host.outgoing.send(half_budget(index)).await.unwrap();
        }
        for index in 0..2 {
            assert_eq!(from_host(&mut channel).await, Some(half_budget(index)));
        }
        host.outgoing.send(half_budget(2)).await.unwrap();
        assert!(
            host.outgoing.try_send(half_budget(3)).is_err(),
            "the host rendered past its byte budget"
        );

        channel.delivered();
        assert_eq!(from_host(&mut channel).await, Some(half_budget(2)));
        host.outgoing.send(half_budget(3)).await.unwrap();
        assert!(
            host.outgoing.try_send(half_budget(4)).is_err(),
            "the host rendered past its byte budget"
        );
        channel.delivered();
        assert_eq!(from_host(&mut channel).await, Some(half_budget(3)));
    }

    #[tokio::test]
    async fn a_channel_whose_host_falls_behind_closes_without_the_link() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;
        let mut flooded = open(&lower, &higher, ClientMessage::ListSessions).await;
        let _unread = higher.next_hosted().await;

        let paste = vec![b'x'; 1024 * 1024];
        let flood = async {
            while flooded
                .send(ClientMessage::Input(paste.clone()))
                .await
                .is_ok()
            {
                tokio::task::yield_now().await;
                if lower.cluster.open_channels().0 == 0 {
                    break;
                }
            }
        };
        tokio::time::timeout(PATIENCE, flood)
            .await
            .expect("the flooded channel never closed");
        assert_eq!(from_host(&mut flooded).await, None);
        assert_eq!(flooded.end(), ChannelEnd::ClosedByHost);

        let mut healthy = open(&lower, &higher, ClientMessage::ListCluster).await;
        let mut host = higher.next_hosted().await;
        assert_eq!(
            received(&mut host.incoming).await,
            Some(ClientMessage::ListCluster)
        );
        host.outgoing.send(output(1)).await.unwrap();
        assert_eq!(from_host(&mut healthy).await, Some(output(1)));
        assert_eq!(links(&lower).len(), 1);
    }

    #[tokio::test]
    async fn closing_a_channel_ends_the_hosted_side() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;
        let channel = open(&lower, &higher, ClientMessage::ListSessions).await;
        let mut host = higher.next_hosted().await;
        assert!(received(&mut host.incoming).await.is_some());

        drop(channel);

        assert_eq!(received(&mut host.incoming).await, None);
        drop(host);
        all_channels_closed(&[&lower, &higher]).await;
    }

    #[tokio::test]
    async fn a_dropped_link_ends_its_channels_on_both_sides() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;
        let mut channel = open(&lower, &higher, ClientMessage::ListSessions).await;
        let mut host = higher.next_hosted().await;
        assert!(received(&mut host.incoming).await.is_some());

        lower.cluster.drop_link("high").unwrap();

        assert_eq!(from_host(&mut channel).await, None);
        assert_eq!(channel.end(), ChannelEnd::LinkDown);
        assert_eq!(received(&mut host.incoming).await, None);
        assert!(lower
            .cluster
            .open_channel(higher.cluster.identity.id, ClientMessage::ListSessions)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_stopping_host_ends_the_channels_opened_to_it() {
        let lower = node(LOWER, "low").build();
        let higher = node(HIGHER, "high").build();
        let _runs = running(&lower, &higher).await;
        let mut channel = open(&lower, &higher, ClientMessage::ListSessions).await;
        let _host = higher.next_hosted().await;

        higher.cluster.shutdown().await;

        assert_eq!(from_host(&mut channel).await, None);
        assert_eq!(channel.end(), ChannelEnd::HostStopped);
    }

    #[tokio::test]
    async fn a_host_that_ignores_its_credit_loses_the_channel_but_not_the_link() {
        let ours = node(LOWER, "ours").build();
        let (raw, link) = raw_peer(&ours, PIPE_CAPACITY).await;
        let _run = tokio::spawn({
            let cluster = Arc::clone(&ours.cluster);
            async move { run(link, &cluster).await }
        });
        let (mut raw_reader, mut raw_writer) = split(raw);
        let peer: ServerId = HIGHEST.parse().unwrap();

        let mut channel = ours
            .cluster
            .open_channel(peer, ClientMessage::ListSessions)
            .await
            .unwrap();
        let id = loop {
            let payload = read_frame(&mut raw_reader).await.unwrap().unwrap();
            if let PeerMessage::ChannelOpen { id, .. } = postcard::from_bytes(&payload).unwrap() {
                break id;
            }
        };
        let frame = ServerMessage::Output(vec![b'y'; 1024 * 1024]);
        let flood = tokio::spawn(async move {
            for _ in 0..32 {
                let message = PeerMessage::ChannelToClient {
                    id,
                    message: frame.clone(),
                };
                write_message(&mut raw_writer, &message).await.unwrap();
            }
            raw_writer
        });
        let _raw_writer = tokio::time::timeout(PATIENCE, flood)
            .await
            .expect("the flood stalled")
            .unwrap();

        let mut delivered = 0;
        while from_host(&mut channel).await.is_some() {
            delivered += 1;
        }
        assert!(delivered < 32, "{delivered}");
        assert_eq!(channel.end(), ChannelEnd::Overflow);
        let closed = loop {
            let payload = read_frame(&mut raw_reader).await.unwrap().unwrap();
            if let PeerMessage::ChannelClose { id, from_opener } =
                postcard::from_bytes(&payload).unwrap()
            {
                break (id, from_opener);
            }
        };
        assert_eq!(closed, (id, true));
        assert_eq!(links(&ours).len(), 1);
    }
}
