mod channel;
mod link;
pub mod ssh;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, watch, Notify};
use tracing::{debug, info, warn};

use crate::config::{Incarnation, ServerConfig, ServerId, ServerIdentity};
use crate::protocol::{
    ClientMessage, Duplex, Event, Farewell, Hello, LinkInfo, LinkState, PeerAddress, PeerMessage,
    Refusal, ServerMessage, ServerState, ServerStatus, ServerView, Snapshot, StateEvent, Version,
};
pub use channel::{Channel, ChannelEnd, CREDIT_WINDOW};
pub use link::LinkSettings;
use link::{Handshake, LinkHandle};
use ssh::Address;

const CACHE_FILE: &str = "cluster-cache";
const CACHE_MAGIC: &[u8; 8] = b"AMUXCL01";
const SAVE_DELAY: Duration = Duration::from_millis(500);
const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const GOSSIP_EXPIRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
const BRIDGE_EXIT_GRACE: Duration = Duration::from_secs(2);

pub trait StateSource: Send + Sync {
    fn subscribe(&self) -> (Snapshot, broadcast::Receiver<Event>);
    fn refresh_peers(&self);
    fn serve_channel(self: Arc<Self>, channel: Duplex<ClientMessage, ServerMessage>);
}

pub struct ClusterOptions {
    pub identity: ServerIdentity,
    pub version: Version,
    pub socket_name: String,
    pub settings: LinkSettings,
    pub state_dir: Option<PathBuf>,
    pub servers: BTreeMap<String, ServerConfig>,
}

pub struct Cluster {
    identity: ServerIdentity,
    version: Version,
    socket_name: String,
    settings: LinkSettings,
    cache_path: Option<PathBuf>,
    source: Weak<dyn StateSource>,
    members: Mutex<Members>,
    changes: watch::Sender<()>,
    dirty: Notify,
    saving: Mutex<()>,
}

#[derive(Default)]
struct Members {
    links: BTreeMap<ServerId, LinkEntry>,
    peers: BTreeMap<ServerId, Peer>,
    targets: BTreeMap<String, Target>,
    transports: usize,
    last_link: u64,
    generation: u64,
    stopping: bool,
}

struct LinkEntry {
    id: u64,
    name: String,
    incarnation: Incarnation,
    dialed: bool,
    generation: u64,
    closing: bool,
    handle: LinkHandle,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Peer {
    name: String,
    version: Option<Version>,
    last_seen: Option<SystemTime>,
    stopped: bool,
    state: Option<CachedState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedState {
    incarnation: Incarnation,
    seq: u64,
    state: ServerState,
}

struct Target {
    origin: Origin,
    peer: Option<ServerId>,
    verified: bool,
    last_seen: SystemTime,
    incompatible: Option<Version>,
    is_self: bool,
    stop: Arc<Notify>,
}

enum Origin {
    Configured { name: String, server: ServerConfig },
    Gossiped { name: String },
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    peers: BTreeMap<ServerId, Peer>,
    targets: Vec<CachedTarget>,
}

#[derive(Serialize, Deserialize)]
struct CachedTarget {
    address: String,
    peer: Option<ServerId>,
    gossiped_name: Option<String>,
    verified: bool,
    last_seen: SystemTime,
}

enum Rejection {
    Refused(Refusal),
    Stopping,
}

enum DialPlan {
    Stop,
    Wait,
    Dial(Vec<String>),
}

impl Target {
    fn new(origin: Origin, peer: Option<ServerId>, verified: bool, last_seen: SystemTime) -> Self {
        Self {
            origin,
            peer,
            verified,
            last_seen,
            incompatible: None,
            is_self: false,
            stop: Arc::new(Notify::new()),
        }
    }

    fn name(&self) -> &str {
        match &self.origin {
            Origin::Configured { name, .. } | Origin::Gossiped { name } => name,
        }
    }

    fn configured_name(&self) -> Option<&str> {
        match &self.origin {
            Origin::Configured { name, .. } => Some(name),
            Origin::Gossiped { .. } => None,
        }
    }

    fn is_expired(&self, now: SystemTime) -> bool {
        matches!(self.origin, Origin::Gossiped { .. })
            && now
                .duration_since(self.last_seen)
                .is_ok_and(|age| age > GOSSIP_EXPIRY)
    }

    fn command(&self, address: &str, socket_name: &str, no_start: bool) -> Result<Vec<String>> {
        let parsed: Address = address.parse()?;
        let (amux_path, socket) = match &self.origin {
            Origin::Configured { server, .. } => {
                (server.amux_path.as_deref(), server.socket.as_deref())
            }
            Origin::Gossiped { .. } => (None, None),
        };
        Ok(parsed.bridge_command(amux_path, socket.unwrap_or(socket_name), no_start))
    }
}

impl Cluster {
    pub fn new(options: ClusterOptions, source: Weak<dyn StateSource>) -> Arc<Self> {
        let cache_path = options.state_dir.map(|dir| dir.join(CACHE_FILE));
        let cache = cache_path.as_deref().map(load_cache).unwrap_or_default();
        let now = SystemTime::now();
        let mut members = Members {
            peers: cache.peers,
            ..Members::default()
        };

        for (name, server) in options.servers {
            if name == options.identity.name {
                info!(server = %name, "not dialing a configured server that names this server");
                continue;
            }
            let cached = cache
                .targets
                .iter()
                .find(|cached| cached.address == server.address);
            let target = Target::new(
                Origin::Configured {
                    name,
                    server: server.clone(),
                },
                cached.and_then(|cached| cached.peer),
                cached.is_some_and(|cached| cached.verified),
                now,
            );
            members.targets.insert(server.address, target);
        }
        for cached in cache.targets {
            let Some(name) = cached.gossiped_name else {
                continue;
            };
            let target = Target::new(
                Origin::Gossiped { name },
                cached.peer,
                cached.verified,
                cached.last_seen,
            );
            if members.targets.contains_key(&cached.address)
                || members.names_configured(target.name())
                || target.is_expired(now)
            {
                continue;
            }
            members.targets.insert(cached.address, target);
        }

        Arc::new(Self {
            identity: options.identity,
            version: options.version,
            socket_name: options.socket_name,
            settings: options.settings,
            cache_path,
            source,
            members: Mutex::new(members),
            changes: watch::channel(()).0,
            dirty: Notify::new(),
            saving: Mutex::new(()),
        })
    }

    pub fn start(self: &Arc<Self>) {
        let addresses: Vec<String> = self.members().targets.keys().cloned().collect();
        for address in addresses {
            self.spawn_dial_loop(address);
        }
        if self.cache_path.is_some() {
            tokio::spawn(Arc::clone(self).persist());
        }
        self.peers_changed();
    }

    pub async fn accept<R, W>(self: &Arc<Self>, reader: R, writer: W) -> Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let handshake = tokio::time::timeout(
            self.settings.handshake_timeout,
            link::accept(self, reader, writer),
        )
        .await
        .context("the peer handshake timed out")??;
        self.conclude(handshake, None).await;
        Ok(())
    }

    pub fn view(&self) -> Vec<ServerView> {
        let members = self.members();
        let mut servers: Vec<ServerView> = members
            .peers
            .iter()
            .map(|(id, peer)| {
                let target = members
                    .targets
                    .iter()
                    .find(|(_, target)| target.peer == Some(*id));
                let incompatible = target.and_then(|(_, target)| target.incompatible.clone());
                let status = match (members.links.get(id), incompatible) {
                    (Some(link), _) => ServerStatus::Online {
                        latency: link.handle.stats.latency(),
                    },
                    (None, Some(version)) => ServerStatus::Incompatible { version },
                    (None, None) => ServerStatus::Offline {
                        last_seen: peer.last_seen,
                        stopped: peer.stopped,
                    },
                };
                let state = peer
                    .state
                    .as_ref()
                    .map(|cached| cached.state.clone())
                    .unwrap_or_default();
                let mut sessions = state.sessions;
                sessions.sort_by(|a, b| a.name.cmp(&b.name));
                ServerView {
                    id: Some(*id),
                    name: peer.name.clone(),
                    address: target.map(|(address, _)| address.clone()),
                    version: peer.version.clone(),
                    status,
                    sessions,
                    projects: state.projects,
                }
            })
            .collect();

        servers.extend(
            members
                .targets
                .iter()
                .filter(|(_, target)| {
                    !target.is_self
                        && target
                            .peer
                            .is_none_or(|id| !members.peers.contains_key(&id))
                })
                .map(|(address, target)| ServerView {
                    id: target.peer,
                    name: target.name().to_owned(),
                    address: Some(address.clone()),
                    version: target.incompatible.clone(),
                    status: match &target.incompatible {
                        Some(version) => ServerStatus::Incompatible {
                            version: version.clone(),
                        },
                        None => ServerStatus::Offline {
                            last_seen: None,
                            stopped: false,
                        },
                    },
                    sessions: Vec::new(),
                    projects: Vec::new(),
                }),
        );
        servers.sort_by(|a, b| a.name.cmp(&b.name));
        servers
    }

    pub fn links(&self) -> Vec<LinkInfo> {
        self.members()
            .links
            .iter()
            .map(|(peer, link)| LinkInfo {
                peer: *peer,
                name: link.name.clone(),
                dialed: link.dialed,
                incarnation: link.incarnation,
                state: if link.closing {
                    LinkState::Closing
                } else {
                    LinkState::Up
                },
            })
            .collect()
    }

    pub fn gossip(&self) -> Vec<PeerAddress> {
        let members = self.members();
        members
            .targets
            .iter()
            .filter(|(_, target)| target.verified && !target.is_self)
            .filter_map(|(address, target)| {
                let id = target.peer?;
                let name = members
                    .peers
                    .get(&id)
                    .map_or_else(|| target.name().to_owned(), |peer| peer.name.clone());
                Some(PeerAddress {
                    id,
                    name,
                    address: address.clone(),
                })
            })
            .collect()
    }

    pub async fn open_channel(&self, peer: ServerId, first: ClientMessage) -> Result<Channel> {
        let channels = {
            let members = self.members();
            let link = members
                .links
                .get(&peer)
                .filter(|link| !link.closing && !members.stopping)
                .ok_or_else(|| anyhow!("no link to {peer}"))?;
            Arc::clone(&link.handle.channels)
        };
        channels.open(first).await
    }

    pub fn watch(&self) -> watch::Receiver<()> {
        self.changes.subscribe()
    }

    pub fn is_linked(&self, peer: ServerId) -> bool {
        self.members()
            .links
            .get(&peer)
            .is_some_and(|link| !link.closing)
    }

    pub fn is_stopped(&self, peer: ServerId) -> bool {
        self.members()
            .peers
            .get(&peer)
            .is_some_and(|record| record.stopped)
    }

    pub fn open_channels(&self) -> (usize, usize) {
        self.members()
            .links
            .values()
            .map(|link| link.handle.channels.open_count())
            .fold((0, 0), |(opened, hosted), (more_opened, more_hosted)| {
                (opened + more_opened, hosted + more_hosted)
            })
    }

    pub fn drop_link(&self, peer: &str) -> Result<()> {
        let members = self.members();
        let (_, link) = members
            .links
            .iter()
            .find(|(id, link)| link.name == peer || id.to_string() == peer)
            .ok_or_else(|| anyhow!("no link to {peer}"))?;
        info!(peer = %link.name, "dropping the link on request");
        link.handle.stop.notify_one();
        Ok(())
    }

    pub fn add_server(self: &Arc<Self>, name: String, server: ServerConfig) -> Result<()> {
        server.address.parse::<Address>()?;
        if name == self.identity.name {
            bail!("{name} is the name of this server");
        }
        let address = server.address.clone();
        let spawn = {
            let mut members = self.members();
            if members.names_configured(&name) {
                bail!("{name} is already configured");
            }
            if let Some(other) = members
                .targets
                .get(&address)
                .and_then(Target::configured_name)
            {
                bail!("{address} is already configured as {other}");
            }
            members.forget_gossip_named(&name);
            let origin = Origin::Configured {
                name: name.clone(),
                server,
            };
            match members.targets.get_mut(&address) {
                Some(target) => {
                    target.origin = origin;
                    false
                }
                None => {
                    let target = Target::new(origin, None, false, SystemTime::now());
                    members.targets.insert(address.clone(), target);
                    true
                }
            }
        };
        info!(server = %name, %address, "server added");
        if spawn {
            self.spawn_dial_loop(address);
        }
        self.dirty.notify_one();
        self.peers_changed();
        Ok(())
    }

    pub fn remove_server(&self, name: &str) -> Result<()> {
        {
            let mut members = self.members();
            let address = members
                .targets
                .iter()
                .find(|(_, target)| target.configured_name() == Some(name))
                .map(|(address, _)| address.clone())
                .ok_or_else(|| anyhow!("no server named {name} is configured"))?;
            let target = members
                .targets
                .remove(&address)
                .expect("the target was just found");
            target.stop.notify_one();
            if let Some(peer) = target.peer {
                let reached_elsewhere = members.links.get(&peer).is_some_and(|link| !link.dialed);
                if !reached_elsewhere {
                    members.peers.remove(&peer);
                }
            }
            info!(server = %name, %address, "server removed");
        }
        self.changed();
        self.dirty.notify_one();
        self.peers_changed();
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut changes = self.changes.subscribe();
        {
            let mut members = self.members();
            members.stopping = true;
            for link in members.links.values_mut() {
                link.closing = true;
                let _ = link
                    .handle
                    .control
                    .try_send(PeerMessage::Goodbye(Farewell::Stopped));
                link.handle.finish.send_replace(true);
            }
            info!(links = members.links.len(), "saying goodbye to peers");
        }
        self.changed();

        let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
        loop {
            changes.borrow_and_update();
            let settled = {
                let members = self.members();
                members.links.is_empty() && members.transports == 0
            };
            if settled {
                break;
            }
            if tokio::time::timeout_at(deadline, changes.changed())
                .await
                .is_err()
            {
                warn!("gave up waiting for peer links to close");
                break;
            }
        }
        self.save();
    }

    fn id(&self) -> ServerId {
        self.identity.id
    }

    fn version(&self) -> &Version {
        &self.version
    }

    fn settings(&self) -> &LinkSettings {
        &self.settings
    }

    fn source(&self) -> Weak<dyn StateSource> {
        self.source.clone()
    }

    fn hello(&self) -> Hello {
        Hello {
            id: self.identity.id,
            incarnation: self.identity.incarnation,
            name: self.identity.name.clone(),
            version: self.version.clone(),
            peers: self.gossip(),
        }
    }

    fn check(&self, hello: &Hello) -> Result<(), Refusal> {
        if hello.id == self.identity.id {
            return Err(Refusal::SelfDial);
        }
        if !self.version.is_compatible_with(hello.version.major) {
            return Err(Refusal::Incompatible);
        }
        if hello.name == self.identity.name {
            return Err(Refusal::NameTaken);
        }
        let members = self.members();
        if members
            .links
            .iter()
            .any(|(id, link)| *id != hello.id && link.name == hello.name)
        {
            return Err(Refusal::NameTaken);
        }
        Ok(())
    }

    fn next_generation(&self) -> u64 {
        let mut members = self.members();
        members.generation += 1;
        members.generation
    }

    fn decide(
        self: &Arc<Self>,
        hello: &Hello,
        dialed: bool,
        handle: LinkHandle,
    ) -> Result<(LinkGuard, u64), Rejection> {
        let mut members = self.members();
        if members.stopping {
            return Err(Rejection::Stopping);
        }
        if let Some(existing) = members.links.get(&hello.id) {
            let keep_existing =
                existing.incarnation == hello.incarnation && (existing.dialed || !dialed);
            if keep_existing {
                return Err(Rejection::Refused(Refusal::Duplicate));
            }
        }
        members.generation += 1;
        let generation = members.generation;
        let guard = self.register(&mut members, hello, dialed, generation, handle);
        Ok((guard, generation))
    }

    fn adopt(
        self: &Arc<Self>,
        hello: &Hello,
        dialed: bool,
        generation: u64,
        handle: LinkHandle,
    ) -> Result<LinkGuard, Rejection> {
        let mut members = self.members();
        if members.stopping {
            return Err(Rejection::Stopping);
        }
        if let Some(existing) = members.links.get(&hello.id) {
            if existing.incarnation == hello.incarnation && existing.generation > generation {
                return Err(Rejection::Refused(Refusal::Duplicate));
            }
        }
        Ok(self.register(&mut members, hello, dialed, generation, handle))
    }

    fn register(
        self: &Arc<Self>,
        members: &mut Members,
        hello: &Hello,
        dialed: bool,
        generation: u64,
        handle: LinkHandle,
    ) -> LinkGuard {
        members.last_link += 1;
        let id = members.last_link;
        if let Some(replaced) = members.links.remove(&hello.id) {
            info!(peer = %hello.name, "replacing the previous link");
            replaced.handle.stop.notify_one();
        }
        members.links.insert(
            hello.id,
            LinkEntry {
                id,
                name: hello.name.clone(),
                incarnation: hello.incarnation,
                dialed,
                generation,
                closing: false,
                handle,
            },
        );
        let peer = members.peers.entry(hello.id).or_insert_with(|| Peer {
            name: hello.name.clone(),
            version: None,
            last_seen: None,
            stopped: false,
            state: None,
        });
        peer.name = hello.name.clone();
        peer.version = Some(hello.version.clone());
        peer.last_seen = Some(SystemTime::now());
        peer.stopped = false;
        for target in members.targets.values_mut() {
            if target.peer == Some(hello.id) {
                target.incompatible = None;
            }
        }
        info!(peer = %hello.name, id = %hello.id, incarnation = %hello.incarnation, dialed, "link up");
        self.changed();
        self.dirty.notify_one();
        LinkGuard {
            cluster: Arc::clone(self),
            peer: hello.id,
            link: id,
        }
    }

    fn unregister(&self, peer: ServerId, link: u64) {
        {
            let mut members = self.members();
            if members
                .links
                .get(&peer)
                .is_some_and(|entry| entry.id == link)
            {
                members.links.remove(&peer);
                if let Some(record) = members.peers.get_mut(&peer) {
                    record.last_seen = Some(SystemTime::now());
                }
            }
        }
        self.changed();
        self.dirty.notify_one();
    }

    fn apply_snapshot(self: &Arc<Self>, peer: ServerId, snapshot: Snapshot) {
        let gossip = {
            let mut members = self.members();
            let Some(record) = members.peers.get_mut(&peer) else {
                return;
            };
            if let Some(cached) = &record.state {
                if cached.incarnation == snapshot.incarnation && cached.seq >= snapshot.seq {
                    return;
                }
            }
            let gossip = snapshot.state.peers.clone();
            record.state = Some(CachedState {
                incarnation: snapshot.incarnation,
                seq: snapshot.seq,
                state: snapshot.state,
            });
            gossip
        };
        self.dirty.notify_one();
        self.learn(&gossip);
    }

    fn apply_event(self: &Arc<Self>, peer: ServerId, event: Event) {
        let gossip = {
            let mut members = self.members();
            let Some(cached) = members
                .peers
                .get_mut(&peer)
                .and_then(|record| record.state.as_mut())
            else {
                return;
            };
            if cached.incarnation != event.incarnation || event.seq <= cached.seq {
                return;
            }
            cached.seq = event.seq;
            let gossip = match &event.event {
                StateEvent::PeersChanged(peers) => Some(peers.clone()),
                _ => None,
            };
            cached.state.apply(event.event);
            gossip
        };
        self.dirty.notify_one();
        if let Some(gossip) = gossip {
            self.learn(&gossip);
        }
    }

    fn mark_stopped(&self, peer: ServerId) {
        if let Some(record) = self.members().peers.get_mut(&peer) {
            record.stopped = true;
            info!(peer = %record.name, "the peer stopped");
        }
        self.dirty.notify_one();
    }

    fn learn(self: &Arc<Self>, peers: &[PeerAddress]) {
        let mut added = Vec::new();
        {
            let mut members = self.members();
            if members.stopping {
                return;
            }
            let now = SystemTime::now();
            for peer in peers {
                if peer.id == self.identity.id
                    || peer.name == self.identity.name
                    || members.names_configured(&peer.name)
                {
                    continue;
                }
                if let Some(target) = members.targets.get_mut(&peer.address) {
                    if let Origin::Gossiped { .. } = target.origin {
                        target.last_seen = now;
                    }
                    target.peer.get_or_insert(peer.id);
                    continue;
                }
                if let Err(err) = peer.address.parse::<Address>() {
                    debug!(peer = %peer.name, "ignoring a gossiped address: {err:#}");
                    continue;
                }
                let origin = Origin::Gossiped {
                    name: peer.name.clone(),
                };
                members.targets.insert(
                    peer.address.clone(),
                    Target::new(origin, Some(peer.id), false, now),
                );
                info!(peer = %peer.name, address = %peer.address, "learned a peer address");
                added.push(peer.address.clone());
            }
        }
        if !added.is_empty() {
            self.dirty.notify_one();
        }
        for address in added {
            self.spawn_dial_loop(address);
        }
    }

    fn spawn_dial_loop(self: &Arc<Self>, address: String) {
        let Some(stop) = self
            .members()
            .targets
            .get(&address)
            .map(|target| Arc::clone(&target.stop))
        else {
            return;
        };
        tokio::spawn(Arc::clone(self).dial_loop(address, stop));
    }

    async fn dial_loop(self: Arc<Self>, address: String, stop: Arc<Notify>) {
        let mut changes = self.changes.subscribe();
        let mut backoff = MIN_BACKOFF;
        loop {
            let command = loop {
                changes.borrow_and_update();
                match self.dial_plan(&address) {
                    DialPlan::Stop => return,
                    DialPlan::Wait => tokio::select! {
                        _ = changes.changed() => {}
                        () = stop.notified() => return,
                    },
                    DialPlan::Dial(command) => break command,
                }
            };
            let linked = tokio::select! {
                linked = self.dial(&address, &command) => linked,
                () = stop.notified() => return,
            };
            if linked {
                backoff = MIN_BACKOFF;
            }
            tokio::select! {
                () = tokio::time::sleep(jittered(backoff)) => {}
                () = stop.notified() => return,
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    fn dial_plan(&self, address: &str) -> DialPlan {
        let mut members = self.members();
        if members.stopping {
            return DialPlan::Stop;
        }
        let Some(target) = members.targets.get(address) else {
            return DialPlan::Stop;
        };
        if target.is_self {
            return DialPlan::Stop;
        }
        if target.is_expired(SystemTime::now()) {
            info!(%address, "forgetting a gossiped address that has not been seen for a week");
            members.targets.remove(address);
            self.dirty.notify_one();
            return DialPlan::Stop;
        }
        if let Some(peer) = target.peer {
            if members.links.contains_key(&peer) {
                return DialPlan::Wait;
            }
        }
        let stopped = target
            .peer
            .and_then(|peer| members.peers.get(&peer))
            .is_some_and(|peer| peer.stopped);
        match target.command(address, &self.socket_name, stopped) {
            Ok(command) => DialPlan::Dial(command),
            Err(err) => {
                warn!(%address, "not dialing: {err:#}");
                DialPlan::Stop
            }
        }
    }

    async fn dial(self: &Arc<Self>, address: &str, command: &[String]) -> bool {
        debug!(%address, command = %shell_words::join(command), "dialing");
        let transport = match ssh::spawn(command, address) {
            Ok(transport) => transport,
            Err(err) => {
                warn!(%address, "dialing failed: {err:#}");
                return false;
            }
        };
        let ssh::Transport {
            mut child,
            reader,
            writer,
        } = transport;

        let handshake = tokio::time::timeout(
            self.settings.handshake_timeout,
            link::dial(self, reader, writer),
        )
        .await;
        let handshake = match handshake {
            Ok(Ok(handshake)) => handshake,
            Ok(Err(err)) => {
                info!(%address, "linking failed: {err:#}");
                return false;
            }
            Err(_) => {
                warn!(%address, "the peer handshake timed out");
                return false;
            }
        };
        if !matches!(handshake, Handshake::Linked(_)) {
            return self.conclude(handshake, Some(address)).await;
        }
        let _transport = self.count_transport();
        let linked = self.conclude(handshake, Some(address)).await;
        let _ = tokio::time::timeout(BRIDGE_EXIT_GRACE, child.wait()).await;
        linked
    }

    async fn conclude<R, W>(
        self: &Arc<Self>,
        handshake: Handshake<R, W>,
        address: Option<&str>,
    ) -> bool
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        match handshake {
            Handshake::Linked(link) => {
                let peer = link.peer().clone();
                if let Some(address) = address {
                    self.target_linked(address, &peer);
                }
                self.learn(&peer.peers);
                let end = link::run(link, self).await;
                info!(peer = %peer.name, "link down: {end}");
                true
            }
            Handshake::Refused {
                reason,
                by_peer,
                peer,
            } => {
                if let Some(address) = address {
                    self.target_refused(address, reason, peer.as_ref());
                }
                let name = peer
                    .map(|peer| peer.name)
                    .or_else(|| address.map(str::to_owned))
                    .unwrap_or_default();
                match (by_peer, reason) {
                    (_, Refusal::Duplicate) => {
                        debug!(peer = %name, by_peer, "link refused: {reason}")
                    }
                    (true, _) => warn!(peer = %name, "the peer refused the link: {reason}"),
                    (false, _) => warn!(peer = %name, "refused a link: {reason}"),
                }
                false
            }
            Handshake::Stopping(peer) => {
                let name = peer.map(|peer| peer.name).unwrap_or_default();
                info!(peer = %name, "the peer is shutting down");
                false
            }
            Handshake::Incompatible(welcome) => {
                warn!(
                    peer = %welcome.server_name,
                    version = %welcome.version,
                    "the peer speaks an incompatible protocol"
                );
                if let Some(address) = address {
                    if let Some(target) = self.members().targets.get_mut(address) {
                        target.incompatible = Some(welcome.version);
                    }
                }
                false
            }
        }
    }

    fn target_linked(&self, address: &str, peer: &Hello) {
        {
            let mut members = self.members();
            let Some(target) = members.targets.get_mut(address) else {
                return;
            };
            target.peer = Some(peer.id);
            target.verified = true;
            target.last_seen = SystemTime::now();
            target.incompatible = None;
        }
        self.dirty.notify_one();
        self.peers_changed();
    }

    fn target_refused(&self, address: &str, reason: Refusal, peer: Option<&Hello>) {
        let mut members = self.members();
        let Some(target) = members.targets.get_mut(address) else {
            return;
        };
        if let Some(peer) = peer {
            target.peer = Some(peer.id);
        }
        if reason == Refusal::SelfDial {
            info!(%address, "not dialing an address that leads back to this server");
            target.is_self = true;
        }
    }

    fn count_transport(self: &Arc<Self>) -> TransportGuard {
        self.members().transports += 1;
        TransportGuard {
            cluster: Arc::clone(self),
        }
    }

    fn peers_changed(&self) {
        if let Some(source) = self.source.upgrade() {
            source.refresh_peers();
        }
    }

    fn changed(&self) {
        self.changes.send_replace(());
    }

    async fn persist(self: Arc<Self>) {
        loop {
            self.dirty.notified().await;
            tokio::time::sleep(SAVE_DELAY).await;
            self.save();
        }
    }

    fn save(&self) {
        let Some(path) = &self.cache_path else {
            return;
        };
        let _saving = self.saving.lock().unwrap_or_else(PoisonError::into_inner);
        let cache = {
            let members = self.members();
            let now = SystemTime::now();
            let mut peers = members.peers.clone();
            for id in members.links.keys() {
                if let Some(peer) = peers.get_mut(id) {
                    peer.last_seen = Some(now);
                }
            }
            let targets = members
                .targets
                .iter()
                .map(|(address, target)| CachedTarget {
                    address: address.clone(),
                    peer: target.peer,
                    gossiped_name: match &target.origin {
                        Origin::Gossiped { name } => Some(name.clone()),
                        Origin::Configured { .. } => None,
                    },
                    verified: target.verified,
                    last_seen: target.last_seen,
                })
                .collect();
            Cache { peers, targets }
        };
        if let Err(err) = write_cache(path, &cache) {
            warn!("saving the cluster cache failed: {err:#}");
        }
    }

    fn members(&self) -> MutexGuard<'_, Members> {
        self.members.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Members {
    fn names_configured(&self, name: &str) -> bool {
        self.targets
            .values()
            .any(|target| target.configured_name() == Some(name))
    }

    fn forget_gossip_named(&mut self, name: &str) {
        self.targets.retain(|_, target| {
            let forget =
                matches!(&target.origin, Origin::Gossiped { name: gossiped } if gossiped == name);
            if forget {
                target.stop.notify_one();
            }
            !forget
        });
    }
}

struct LinkGuard {
    cluster: Arc<Cluster>,
    peer: ServerId,
    link: u64,
}

impl Drop for LinkGuard {
    fn drop(&mut self) {
        self.cluster.unregister(self.peer, self.link);
    }
}

struct TransportGuard {
    cluster: Arc<Cluster>,
}

impl Drop for TransportGuard {
    fn drop(&mut self) {
        self.cluster.members().transports -= 1;
        self.cluster.changed();
    }
}

fn load_cache(path: &Path) -> Cache {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Cache::default(),
        Err(err) => {
            warn!(
                "reading {} failed, starting without it: {err}",
                path.display()
            );
            return Cache::default();
        }
    };
    let decoded = bytes
        .strip_prefix(CACHE_MAGIC)
        .context("unknown format")
        .and_then(|payload| postcard::from_bytes(payload).context("decoding"));
    match decoded {
        Ok(cache) => cache,
        Err(err) => {
            warn!("dropping the cluster cache {}: {err:#}", path.display());
            Cache::default()
        }
    }
}

fn write_cache(path: &Path, cache: &Cache) -> Result<()> {
    let mut bytes = CACHE_MAGIC.to_vec();
    bytes.extend(postcard::to_stdvec(cache)?);
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

fn jittered(delay: Duration) -> Duration {
    let mut bytes = [0; 8];
    let fraction = match getrandom::fill(&mut bytes) {
        Ok(()) => u64::from_ne_bytes(bytes) as f64 / u64::MAX as f64,
        Err(_) => 1.0,
    };
    delay.mul_f64(0.5 + fraction / 2.0)
}
