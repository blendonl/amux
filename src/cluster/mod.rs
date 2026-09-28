mod cache;
mod channel;
mod link;
pub mod listener;
pub mod noise;
pub mod ssh;
pub mod transport;
pub mod trust;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, watch, Notify, Semaphore};
use tracing::{debug, info, warn};

use crate::discovery::tailscale::Tailnet;
use crate::identity::{Incarnation, ServerId, ServerIdentity};
use crate::pairing::Pairing;
use crate::protocol::{
    self, ClientMessage, DiscoveryStatus, DiscoveryView, Duplex, Event, Farewell, Hello, LinkInfo,
    LinkState, LinkTransport, PeerAddress, PeerMessage, PublicKey, Refusal, Role, ServerMessage,
    ServerState, ServerStatus, ServerView, Snapshot, StateEvent, TcpKind, TrustUpdate, Version,
    Via,
};
use crate::settings::ServerConfig;
use crate::settings::{ClusterSettings, DiscoverySettings, SshSettings};
use cache::{Cache, CachedOrigin, CachedTarget, CACHE_FILE};
pub use channel::{Channel, ChannelEnd, CREDIT_WINDOW};
pub use link::with_env;
use link::{Handshake, LinkHandle};
pub use listener::{LanListener, LanOptions, Listener, LAN_PORT_FILE};
pub use noise::{NoiseKey, Secured, NOISE_KEY_FILE};
pub use transport::{Address, Connect, TransportAuth, Voucher};
pub use trust::{TrustStore, Witnessed, TRUST_FILE};

const SAVE_DELAY: Duration = Duration::from_millis(500);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
const FLUSH_GRACE: Duration = Duration::from_secs(2);

type NoiseHandshake = Handshake<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

pub trait StateSource: Send + Sync {
    fn subscribe(&self) -> (Snapshot, broadcast::Receiver<Event>);
    fn refresh_peers(&self);
    fn serve_channel(self: Arc<Self>, channel: Duplex<ClientMessage, ServerMessage>);
}

pub struct ClusterOptions {
    pub identity: ServerIdentity,
    pub version: Version,
    pub socket_name: String,
    pub settings: ClusterSettings,
    pub state_dir: Option<PathBuf>,
    pub servers: BTreeMap<String, ServerConfig>,
    pub discovery: DiscoverySettings,
    pub trust: TrustStore,
    pub key: NoiseKey,
}

pub struct Cluster {
    identity: ServerIdentity,
    version: Version,
    socket_name: String,
    settings: Mutex<Arc<ClusterSettings>>,
    state_dir: Option<PathBuf>,
    cache_path: Option<PathBuf>,
    trust_path: Option<PathBuf>,
    key: Mutex<NoiseKey>,
    handshakes: Arc<Semaphore>,
    tailnet: OnceLock<Arc<Tailnet>>,
    source: Weak<dyn StateSource>,
    members: Mutex<Members>,
    changes: watch::Sender<()>,
    dirty: Notify,
    saving: Mutex<()>,
    saving_trust: Mutex<()>,
    pairing: Pairing,
}

#[derive(Default)]
struct Members {
    links: BTreeMap<ServerId, LinkEntry>,
    peers: BTreeMap<ServerId, Peer>,
    targets: BTreeMap<String, Target>,
    trust: TrustStore,
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
    transport: LinkTransport,
    key: Option<PublicKey>,
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
    last_error: Option<String>,
    found: Option<Found>,
    stop: Arc<Notify>,
    wake: Arc<Notify>,
}

enum Origin {
    Configured { name: String, server: ServerConfig },
    Gossiped { name: String },
    Discovered { name: String, via: Via },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub address: String,
    pub server: Option<ServerId>,
    pub key: Option<PublicKey>,
    pub pairing: Option<String>,
    pub dialable: bool,
    pub endpoints: Vec<SocketAddr>,
}

impl Candidate {
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            address: address.into(),
            server: None,
            key: None,
            pairing: None,
            dialable: true,
            endpoints: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    via: Via,
    candidate: Candidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forgotten {
    pub name: String,
    pub id: Option<ServerId>,
    pub configured: Vec<String>,
}

enum Rejection {
    Refused(Refusal),
    Stopping,
}

enum DialPlan {
    Stop,
    Wait,
    Dial {
        connect: Connect,
        max_backoff: Duration,
    },
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
            last_error: None,
            found: None,
            stop: Arc::new(Notify::new()),
            wake: Arc::new(Notify::new()),
        }
    }

    fn restored(origin: Origin, cached: &CachedTarget) -> Self {
        Self {
            is_self: cached.is_self,
            last_error: cached.last_error.clone(),
            ..Self::new(origin, cached.peer, cached.verified, cached.last_seen)
        }
    }

    fn name(&self) -> &str {
        match &self.origin {
            Origin::Configured { name, .. }
            | Origin::Gossiped { name }
            | Origin::Discovered { name, .. } => name,
        }
    }

    fn configured_name(&self) -> Option<&str> {
        match &self.origin {
            Origin::Configured { name, .. } => Some(name),
            Origin::Gossiped { .. } | Origin::Discovered { .. } => None,
        }
    }

    fn is_configured(&self) -> bool {
        matches!(self.origin, Origin::Configured { .. })
    }

    fn is_discovered(&self) -> bool {
        matches!(self.origin, Origin::Discovered { .. })
    }

    fn is_absent(&self) -> bool {
        self.is_discovered() && self.found.is_none()
    }

    fn is_hidden(&self) -> bool {
        self.is_discovered() && !self.verified
    }

    fn is_dialable(&self) -> bool {
        !self.is_discovered()
            || self
                .found
                .as_ref()
                .is_some_and(|found| found.candidate.dialable)
    }

    fn is_expired(&self, now: SystemTime, expiry: Duration) -> bool {
        let expires = match self.origin {
            Origin::Configured { .. } => false,
            Origin::Gossiped { .. } => true,
            Origin::Discovered { .. } => self.found.is_none() && !self.is_self,
        };
        expires
            && now
                .duration_since(self.last_seen)
                .is_ok_and(|age| age > expiry)
    }

    fn connect(
        &self,
        ssh: &SshSettings,
        address: &str,
        socket_name: &str,
        no_start: bool,
    ) -> Result<Option<Connect>> {
        let parsed: Address = address.parse()?;
        let (amux_path, socket) = match &self.origin {
            Origin::Configured { server, .. } => {
                (server.amux_path.as_deref(), server.socket.as_deref())
            }
            Origin::Gossiped { .. } | Origin::Discovered { .. } => (None, None),
        };
        let endpoints = self
            .found
            .as_ref()
            .map(|found| found.candidate.endpoints.as_slice())
            .unwrap_or_default();
        Ok(parsed.connect(
            ssh,
            amux_path,
            socket.unwrap_or(socket_name),
            no_start,
            endpoints,
        ))
    }

    fn cached(&self, address: &str) -> CachedTarget {
        CachedTarget {
            address: address.to_owned(),
            peer: self.peer,
            origin: match &self.origin {
                Origin::Configured { .. } => CachedOrigin::Configured,
                Origin::Gossiped { name } => CachedOrigin::Gossiped { name: name.clone() },
                Origin::Discovered { name, via } => CachedOrigin::Discovered {
                    name: name.clone(),
                    via: *via,
                },
            },
            verified: self.verified,
            last_seen: self.last_seen,
            is_self: self.is_self,
            last_error: self.last_error.clone(),
        }
    }

    fn discovery_view(&self, address: &str, linked: bool) -> Option<DiscoveryView> {
        let (via, name) = match (&self.found, &self.origin) {
            (Some(found), _) => (found.via, found.candidate.name.clone()),
            (None, Origin::Discovered { name, via }) => (*via, name.clone()),
            (None, _) => return None,
        };
        if self.is_self {
            return None;
        }
        let status = match &self.found {
            _ if linked => DiscoveryStatus::Linked,
            None => DiscoveryStatus::Absent,
            Some(found) if !found.candidate.dialable => match found.candidate.pairing {
                Some(_) => DiscoveryStatus::PairingOpen,
                None => DiscoveryStatus::NotPaired,
            },
            Some(_) if self.last_error.is_some() => DiscoveryStatus::Failing,
            Some(_) => DiscoveryStatus::Trying,
        };
        Some(DiscoveryView {
            via,
            name,
            address: address.to_owned(),
            server: self.peer,
            status,
            last_error: self.last_error.clone(),
        })
    }
}

impl Cluster {
    pub fn new(options: ClusterOptions, source: Weak<dyn StateSource>) -> Arc<Self> {
        let cache_path = options.state_dir.as_ref().map(|dir| dir.join(CACHE_FILE));
        let trust_path = options.state_dir.as_ref().map(|dir| dir.join(TRUST_FILE));
        let Cache {
            mut peers,
            targets: cached_targets,
        } = cache_path.as_deref().map(cache::load).unwrap_or_default();
        peers.retain(|id, _| !options.trust.is_forgotten(*id, None));
        let now = SystemTime::now();
        let expiry = options.settings.address_expiry();
        let mut members = Members {
            peers,
            trust: options.trust,
            ..Members::default()
        };

        for (name, server) in options.servers {
            if name == options.identity.name {
                info!(server = %name, "not dialing a configured server that names this server");
                continue;
            }
            let origin = Origin::Configured {
                name,
                server: server.clone(),
            };
            let target = match cached_targets
                .iter()
                .find(|cached| cached.address == server.address)
            {
                Some(cached) => Target {
                    last_seen: now,
                    is_self: false,
                    ..Target::restored(origin, cached)
                },
                None => Target::new(origin, None, false, now),
            };
            members.targets.insert(server.address, target);
        }
        for cached in &cached_targets {
            let origin = match &cached.origin {
                CachedOrigin::Configured => continue,
                CachedOrigin::Gossiped { name } => Origin::Gossiped { name: name.clone() },
                CachedOrigin::Discovered { name, via } => {
                    let enabled = match via {
                        Via::Tailscale => options.discovery.tailscale,
                        Via::Lan => options.discovery.lan,
                    };
                    if !enabled || !(cached.verified || cached.is_self) {
                        continue;
                    }
                    Origin::Discovered {
                        name: name.clone(),
                        via: *via,
                    }
                }
            };
            let target = Target::restored(origin, cached);
            let forgotten = cached
                .peer
                .is_some_and(|peer| members.trust.is_forgotten(peer, None));
            let name_configured = matches!(target.origin, Origin::Gossiped { .. })
                && members.names_configured(target.name());
            if members.targets.contains_key(&cached.address)
                || forgotten
                || name_configured
                || target.is_expired(now, expiry)
            {
                continue;
            }
            members.targets.insert(cached.address.clone(), target);
        }

        Arc::new(Self {
            identity: options.identity,
            version: options.version,
            socket_name: options.socket_name,
            settings: Mutex::new(Arc::new(options.settings)),
            state_dir: options.state_dir,
            cache_path,
            trust_path,
            key: Mutex::new(options.key),
            handshakes: Arc::new(Semaphore::new(listener::MAX_PENDING_HANDSHAKES)),
            tailnet: OnceLock::new(),
            source,
            members: Mutex::new(members),
            changes: watch::channel(()).0,
            dirty: Notify::new(),
            saving: Mutex::new(()),
            saving_trust: Mutex::new(()),
            pairing: Pairing::default(),
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

    pub async fn accept<R, W>(
        self: &Arc<Self>,
        reader: R,
        writer: W,
        auth: TransportAuth,
    ) -> Result<()>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let handshake = tokio::time::timeout(
            self.settings().handshake_timeout(),
            link::accept(self, reader, writer, auth),
        )
        .await
        .context("the peer handshake timed out")??;
        self.conclude(handshake, None).await;
        Ok(())
    }

    pub async fn accept_noise(
        self: &Arc<Self>,
        secured: Secured,
        vouched_by: Voucher,
    ) -> Result<()> {
        let Secured {
            remote,
            stream,
            flushed,
            ..
        } = secured;
        let handshake = tokio::time::timeout(
            self.settings().handshake_timeout(),
            self.noise_handshake(stream, remote, vouched_by),
        )
        .await
        .context("the peer handshake timed out")??;
        let _transport = self.count_transport();
        self.conclude(handshake, None).await;
        let _ = tokio::time::timeout(FLUSH_GRACE, flushed).await;
        Ok(())
    }

    pub async fn dial_secured(
        self: &Arc<Self>,
        secured: Secured,
        vouched_by: Voucher,
    ) -> Result<()> {
        let Secured {
            remote,
            stream,
            flushed,
            ..
        } = secured;
        let auth = TransportAuth::Noise {
            key: remote,
            vouched_by,
        };
        let (reader, writer) = tokio::io::split(stream);
        let handshake = tokio::time::timeout(
            self.settings().handshake_timeout(),
            link::dial(self, reader, writer, auth),
        )
        .await
        .context("the peer handshake timed out")??;
        let _transport = self.count_transport();
        self.conclude(handshake, None).await;
        let _ = tokio::time::timeout(FLUSH_GRACE, flushed).await;
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
                    .filter(|(_, target)| target.peer == Some(*id))
                    .min_by_key(|(_, target)| !target.verified);
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
                        && !target.is_hidden()
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
                transport: link.transport,
                key: link.key,
            })
            .collect()
    }

    pub fn discovery_view(&self) -> Vec<DiscoveryView> {
        let members = self.members();
        let mut views: Vec<DiscoveryView> = members
            .targets
            .iter()
            .filter_map(|(address, target)| {
                let linked = target
                    .peer
                    .is_some_and(|peer| members.links.contains_key(&peer));
                target.discovery_view(address, linked)
            })
            .collect();
        views.sort_by(|a, b| (a.via, &a.name, &a.address).cmp(&(b.via, &b.name, &b.address)));
        views
    }

    pub fn noise_key(&self) -> NoiseKey {
        self.key
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn public_key(&self) -> PublicKey {
        self.noise_key().public()
    }

    pub fn rotate_key(&self) -> Result<PublicKey> {
        let key = match &self.state_dir {
            Some(dir) => NoiseKey::rotate(dir)?,
            None => NoiseKey::generate()?,
        };
        let public = key.public();
        *self.key.lock().unwrap_or_else(PoisonError::into_inner) = key;
        info!(key = %public, "rotated the noise key");
        Ok(public)
    }

    pub async fn voucher(&self, key: &PublicKey, remote: IpAddr) -> Voucher {
        let trusted = self.members().trust.is_trusted(key);
        if trusted {
            return Voucher::TrustStore;
        }
        match self.tailnet.get() {
            Some(tailnet) if tailnet.vouches_for(remote).await => Voucher::Tailnet,
            _ => Voucher::Nobody,
        }
    }

    pub fn use_tailnet(&self, tailnet: Arc<Tailnet>) {
        if self.tailnet.set(tailnet).is_err() {
            warn!("ignoring a second tailnet");
        }
    }

    pub fn trusts_anyone(&self) -> bool {
        !self.members().trust.trusted.is_empty()
    }

    pub fn trusts(&self, id: ServerId, key: &PublicKey) -> bool {
        self.members().trust.key_of(id) == Some(*key)
    }

    pub fn forgot(&self, key: &PublicKey) -> bool {
        self.members().trust.is_key_forgotten(key)
    }

    pub fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    pub fn pairing(&self) -> &Pairing {
        &self.pairing
    }

    pub fn witness(&self, id: ServerId, name: &str, key: PublicKey) -> Witnessed {
        let witnessed = self.members().trust.witness(id, name, key);
        self.witnessed(id, name, key, witnessed);
        witnessed
    }

    pub fn unforget(&self, id: ServerId) -> bool {
        let unforgotten = self.members().trust.unforget(id);
        if unforgotten {
            info!(%id, "no longer refusing a forgotten server");
            self.trust_changed();
        }
        unforgotten
    }

    pub fn gossip(&self) -> Vec<PeerAddress> {
        let members = self.members();
        members
            .targets
            .iter()
            .filter(|(_, target)| target.verified && !target.is_self && !target.is_discovered())
            .filter(|(address, _)| is_shareable(address))
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

    pub fn set_settings(&self, settings: ClusterSettings) {
        *self.settings.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(settings);
    }

    pub fn configured_servers(&self) -> BTreeMap<String, ServerConfig> {
        self.members()
            .targets
            .values()
            .filter_map(|target| match &target.origin {
                Origin::Configured { name, server } => Some((name.clone(), server.clone())),
                _ => None,
            })
            .collect()
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

    pub fn forget(&self, server: &str) -> Result<Forgotten> {
        let forgotten = {
            let mut members = self.members();
            if server == self.identity.name || server == self.identity.id.to_string() {
                bail!("{server} is this server");
            }
            let names = |id: &ServerId, name: &str| name == server || id.to_string() == server;
            let named: Vec<String> = members
                .targets
                .iter()
                .filter(|(_, target)| {
                    !target.is_self
                        && (target.name() == server
                            || target
                                .found
                                .as_ref()
                                .is_some_and(|found| found.candidate.name == server))
                })
                .map(|(address, _)| address.clone())
                .collect();
            let mut ids: BTreeSet<ServerId> = members
                .peers
                .iter()
                .filter(|(id, peer)| names(id, &peer.name))
                .map(|(id, _)| *id)
                .collect();
            ids.extend(
                members
                    .links
                    .iter()
                    .filter(|(id, link)| names(id, &link.name))
                    .map(|(id, _)| *id),
            );
            ids.extend(
                members
                    .targets
                    .iter()
                    .filter(|(address, target)| {
                        named.contains(address)
                            || target.peer.is_some_and(|id| id.to_string() == server)
                    })
                    .filter_map(|(_, target)| target.peer),
            );
            ids.remove(&self.identity.id);
            let id = match ids.len() {
                0 => None,
                1 => ids.first().copied(),
                _ => {
                    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
                    bail!(
                        "{server} names more than one server, pass one of these ids: {}",
                        ids.join(", ")
                    )
                }
            };
            if id.is_none() && named.is_empty() {
                bail!("no server named {server} is known");
            }

            let dropped: Vec<String> = members
                .targets
                .iter()
                .filter(|(address, target)| {
                    !target.is_self
                        && (named.contains(address) || (id.is_some() && target.peer == id))
                })
                .map(|(address, _)| address.clone())
                .collect();
            let mut configured = Vec::new();
            for address in dropped {
                let target = members
                    .targets
                    .remove(&address)
                    .expect("the target was just found");
                target.stop.notify_one();
                if let Some(name) = target.configured_name() {
                    configured.push(name.to_owned());
                }
            }
            let mut name = server.to_owned();
            if let Some(id) = id {
                let key = members
                    .links
                    .get(&id)
                    .and_then(|link| link.key)
                    .or_else(|| members.trust.key_of(id));
                if let Some(link) = members.links.get(&id) {
                    name = link.name.clone();
                    link.handle.stop.notify_one();
                }
                if let Some(peer) = members.peers.remove(&id) {
                    name = peer.name;
                }
                members.trust.forget(id, key);
            }
            info!(server = %name, id = ?id, "server forgotten");
            Forgotten {
                name,
                id,
                configured,
            }
        };
        let saved = self.save_trust();
        self.share_trust();
        self.changed();
        self.dirty.notify_one();
        self.peers_changed();
        saved.map(|()| forgotten)
    }

    pub fn discovered(self: &Arc<Self>, via: Via, candidates: Vec<Candidate>) {
        let mut added = Vec::new();
        {
            let mut members = self.members();
            if members.stopping {
                return;
            }
            let now = SystemTime::now();
            let mut listed = BTreeSet::new();
            let mut woken = BTreeSet::new();
            for candidate in candidates {
                let forgotten = candidate
                    .server
                    .is_some_and(|id| members.trust.is_forgotten(id, candidate.key.as_ref()))
                    || candidate
                        .key
                        .is_some_and(|key| members.trust.is_key_forgotten(&key));
                if candidate.server == Some(self.identity.id) || forgotten {
                    continue;
                }
                if let Err(err) = candidate.address.parse::<Address>() {
                    debug!(%via, name = %candidate.name, "ignoring a discovered address: {err:#}");
                    continue;
                }
                if !listed.insert(candidate.address.clone()) {
                    continue;
                }
                let address = candidate.address.clone();
                let found = Found { via, candidate };
                match members.targets.get_mut(&address) {
                    Some(target) => {
                        match &mut target.origin {
                            Origin::Gossiped { .. } => {
                                target.origin = Origin::Discovered {
                                    name: found.candidate.name.clone(),
                                    via,
                                };
                            }
                            Origin::Discovered { name, .. } => {
                                name.clone_from(&found.candidate.name);
                            }
                            Origin::Configured { .. } => {}
                        }
                        if let Some(id) = found.candidate.server {
                            target.peer.get_or_insert(id);
                        }
                        target.last_seen = now;
                        if target.found.as_ref() != Some(&found) {
                            target.wake.notify_one();
                            woken.extend(target.peer);
                        }
                        target.found = Some(found);
                    }
                    None => {
                        let origin = Origin::Discovered {
                            name: found.candidate.name.clone(),
                            via,
                        };
                        info!(%via, name = %found.candidate.name, %address, "discovered a server");
                        let mut target = Target::new(origin, found.candidate.server, false, now);
                        target.found = Some(found);
                        members.targets.insert(address.clone(), target);
                        added.push(address);
                    }
                }
            }

            let mut gone = Vec::new();
            for (address, target) in &mut members.targets {
                let from_here = target.found.as_ref().is_some_and(|found| found.via == via);
                if !from_here || listed.contains(address) {
                    continue;
                }
                target.found = None;
                if target.is_discovered() && !target.verified && !target.is_self {
                    gone.push(address.clone());
                }
            }
            for address in gone {
                if let Some(target) = members.targets.remove(&address) {
                    debug!(%via, %address, "dropping a discovered address that was never reached");
                    target.stop.notify_one();
                }
            }
            for target in members.targets.values() {
                if target.peer.is_some_and(|peer| woken.contains(&peer)) {
                    target.wake.notify_one();
                }
            }
        }
        self.changed();
        self.dirty.notify_one();
        for address in added {
            self.spawn_dial_loop(address);
        }
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

    fn settings(&self) -> Arc<ClusterSettings> {
        Arc::clone(&self.settings.lock().unwrap_or_else(PoisonError::into_inner))
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
            public_key: Some(self.public_key()),
        }
    }

    fn admit(&self, auth: &TransportAuth) -> Result<(), Refusal> {
        let TransportAuth::Noise { key, vouched_by } = auth else {
            return Ok(());
        };
        let members = self.members();
        if members.trust.is_key_forgotten(key) {
            return Err(Refusal::Forgotten);
        }
        match vouched_by {
            Voucher::Tailnet | Voucher::Pairing => Ok(()),
            Voucher::Nobody | Voucher::TrustStore if members.trust.is_trusted(key) => Ok(()),
            Voucher::Nobody | Voucher::TrustStore => Err(Refusal::Untrusted),
        }
    }

    fn check(&self, hello: &Hello, auth: &TransportAuth) -> Result<(), Refusal> {
        if hello.id == self.identity.id {
            return Err(Refusal::SelfDial);
        }
        let witnessed = {
            let mut members = self.members();
            let key_forgotten = auth
                .key()
                .is_some_and(|key| members.trust.is_key_forgotten(&key));
            if key_forgotten
                || members
                    .trust
                    .is_forgotten(hello.id, hello.public_key.as_ref())
            {
                return Err(Refusal::Forgotten);
            }
            if let TransportAuth::Noise {
                key,
                vouched_by: Voucher::Nobody | Voucher::TrustStore,
            } = auth
            {
                if members.trust.key_of(hello.id) != Some(*key) {
                    return Err(Refusal::Untrusted);
                }
            }
            if !self.version.is_compatible_with(hello.version.major) {
                return Err(Refusal::Incompatible);
            }
            if hello.name == self.identity.name {
                return Err(Refusal::NameTaken);
            }
            if members
                .links
                .iter()
                .any(|(id, link)| *id != hello.id && link.name == hello.name)
            {
                return Err(Refusal::NameTaken);
            }
            let evidence = match auth {
                TransportAuth::Ssh => hello.public_key,
                TransportAuth::Noise {
                    key,
                    vouched_by: Voucher::Tailnet | Voucher::Pairing,
                } => Some(*key),
                TransportAuth::Noise { .. } => None,
            };
            match evidence.map(|key| (key, members.trust.witness(hello.id, &hello.name, key))) {
                Some((_, Witnessed::HeldBy(_))) if auth.key().is_some() => {
                    return Err(Refusal::Untrusted);
                }
                witnessed => witnessed,
            }
        };
        if let Some((key, witnessed)) = witnessed {
            self.witnessed(hello.id, &hello.name, key, witnessed);
        }
        Ok(())
    }

    fn witnessed(&self, id: ServerId, name: &str, key: PublicKey, witnessed: Witnessed) {
        match witnessed {
            Witnessed::Trusted => info!(server = %name, %id, %key, "trusting the server's key"),
            Witnessed::Replaced(old) => {
                info!(server = %name, %id, %old, new = %key, "the server's key changed");
            }
            Witnessed::Confirmed => {
                debug!(server = %name, %id, %key, "saw the server's key first hand")
            }
            Witnessed::HeldBy(holder) => {
                warn!(server = %name, %id, %key, %holder, "not trusting a key that another server holds");
            }
            Witnessed::Renamed | Witnessed::Unchanged => {}
        }
        if witnessed.changed() {
            self.trust_changed();
        }
    }

    fn merge_trust(&self, sender: ServerId, update: &TrustUpdate) {
        let merged = {
            let mut members = self.members();
            if members.stopping {
                return;
            }
            let merged = members.trust.merge(update, sender, self.identity.id);
            let by = members
                .links
                .get(&sender)
                .map_or_else(|| sender.to_string(), |link| link.name.clone());
            for id in &merged.forgotten {
                let name = members.drop_server(*id).unwrap_or_else(|| id.to_string());
                info!(server = %name, %id, %by, "forgetting a server that a peer forgot");
            }
            merged
        };
        if merged.changed {
            self.trust_changed();
        }
        if !merged.forgotten.is_empty() {
            self.dirty.notify_one();
            self.peers_changed();
        }
    }

    fn trust_changed(&self) {
        if let Err(err) = self.save_trust() {
            warn!("saving the trust store failed: {err:#}");
        }
        self.share_trust();
        self.changed();
    }

    fn share_trust(&self) {
        let members = self.members();
        let update = members.trust.update();
        for (id, link) in &members.links {
            if members.trust.is_forgotten(*id, link.key.as_ref()) {
                link.handle.stop.notify_one();
                continue;
            }
            let unbound = link.transport == LinkTransport::Noise
                && (link.key.is_none() || members.trust.key_of(*id) != link.key);
            if unbound {
                info!(peer = %link.name, "dropping a link whose key is no longer trusted");
                link.handle.stop.notify_one();
                continue;
            }
            if link
                .handle
                .control
                .try_send(PeerMessage::Trust(update.clone()))
                .is_err()
            {
                debug!(peer = %link.name, "the link had no room for a trust update");
            }
        }
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
        auth: &TransportAuth,
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
        let guard = self.register(&mut members, hello, dialed, generation, handle, auth);
        Ok((guard, generation))
    }

    fn adopt(
        self: &Arc<Self>,
        hello: &Hello,
        dialed: bool,
        generation: u64,
        handle: LinkHandle,
        auth: &TransportAuth,
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
        Ok(self.register(&mut members, hello, dialed, generation, handle, auth))
    }

    fn register(
        self: &Arc<Self>,
        members: &mut Members,
        hello: &Hello,
        dialed: bool,
        generation: u64,
        handle: LinkHandle,
        auth: &TransportAuth,
    ) -> LinkGuard {
        members.last_link += 1;
        let id = members.last_link;
        let _ = handle
            .control
            .try_send(PeerMessage::Trust(members.trust.update()));
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
                transport: auth.transport(),
                key: auth.key().or(hello.public_key),
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
        info!(
            peer = %hello.name,
            id = %hello.id,
            incarnation = %hello.incarnation,
            dialed,
            transport = %auth.transport(),
            "link up"
        );
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
                    || members.trust.is_forgotten(peer.id, None)
                    || !is_shareable(&peer.address)
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
        let Some((stop, wake)) = self
            .members()
            .targets
            .get(&address)
            .map(|target| (Arc::clone(&target.stop), Arc::clone(&target.wake)))
        else {
            return;
        };
        tokio::spawn(Arc::clone(self).dial_loop(address, stop, wake));
    }

    async fn dial_loop(self: Arc<Self>, address: String, stop: Arc<Notify>, wake: Arc<Notify>) {
        let mut changes = self.changes.subscribe();
        let mut backoff = self.settings().backoff_min();
        loop {
            let (connect, max_backoff) = loop {
                changes.borrow_and_update();
                match self.dial_plan(&address) {
                    DialPlan::Stop => return,
                    DialPlan::Wait => tokio::select! {
                        _ = changes.changed() => {}
                        () = wake.notified() => backoff = self.settings().backoff_min(),
                        () = stop.notified() => return,
                    },
                    DialPlan::Dial {
                        connect,
                        max_backoff,
                    } => break (connect, max_backoff),
                }
            };
            let linked = tokio::select! {
                linked = self.dial(&address, &connect) => linked,
                () = stop.notified() => return,
            };
            if linked {
                backoff = self.settings().backoff_min();
            }
            tokio::select! {
                () = tokio::time::sleep(jittered(backoff)) => {
                    backoff = (backoff * 2).min(max_backoff);
                }
                () = wake.notified() => backoff = self.settings().backoff_min(),
                () = stop.notified() => return,
            }
        }
    }

    fn dial_plan(&self, address: &str) -> DialPlan {
        let settings = self.settings();
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
        if target.is_expired(SystemTime::now(), settings.address_expiry()) {
            info!(
                %address,
                hours = settings.address_expiry_hours,
                "forgetting an address that has not been seen for too long"
            );
            members.targets.remove(address);
            self.dirty.notify_one();
            return DialPlan::Stop;
        }
        if let Some(peer) = target.peer {
            if members.links.contains_key(&peer) {
                return DialPlan::Wait;
            }
        }
        let absent = target.is_absent()
            || (!target.is_configured() && target.peer.is_some_and(|peer| members.is_absent(peer)));
        if absent || !target.is_dialable() {
            return DialPlan::Wait;
        }
        let stopped = target
            .peer
            .and_then(|peer| members.peers.get(&peer))
            .is_some_and(|peer| peer.stopped);
        let max_backoff = if target.is_hidden() {
            settings.max_unverified_backoff()
        } else {
            settings.backoff_max()
        };
        let no_start = stopped || target.is_discovered();
        match target.connect(&settings.ssh, address, &self.socket_name, no_start) {
            Ok(Some(connect)) => DialPlan::Dial {
                connect,
                max_backoff,
            },
            Ok(None) => DialPlan::Wait,
            Err(err) => {
                warn!(%address, "not dialing: {err:#}");
                DialPlan::Stop
            }
        }
    }

    async fn dial(self: &Arc<Self>, address: &str, connect: &Connect) -> bool {
        match connect {
            Connect::Command(command) => self.dial_command(address, command).await,
            Connect::Tcp(endpoints) => self.dial_tcp(address, endpoints).await,
        }
    }

    async fn dial_tcp(self: &Arc<Self>, address: &str, endpoints: &[String]) -> bool {
        let mut failure = "there is no endpoint to dial".to_owned();
        for endpoint in endpoints {
            debug!(%address, %endpoint, "dialing");
            let connected =
                tokio::time::timeout(self.settings().connect_timeout(), self.connect(endpoint))
                    .await;
            match connected {
                Ok(Ok(stream)) => return self.dial_noise(address, stream).await,
                Ok(Err(err)) => failure = format!("connecting to {endpoint}: {err}"),
                Err(_) => failure = format!("connecting to {endpoint} timed out"),
            }
        }
        info!(%address, "dialing failed: {failure}");
        self.record_error(address, failure);
        false
    }

    async fn connect(&self, endpoint: &str) -> io::Result<TcpStream> {
        match self.tailnet.get() {
            Some(tailnet) => tailnet.connect(endpoint).await,
            None => TcpStream::connect(endpoint).await,
        }
    }

    async fn dial_noise(self: &Arc<Self>, address: &str, stream: TcpStream) -> bool {
        let _ = stream.set_nodelay(true);
        let key = self.noise_key();
        let handshake = tokio::time::timeout(self.settings().handshake_timeout(), async {
            let dialed = stream.peer_addr().context("reading the dialed address")?;
            let Secured {
                remote,
                stream,
                flushed,
                ..
            } = noise::initiate(stream, &key, TcpKind::Link).await?;
            let auth = TransportAuth::Noise {
                key: remote,
                vouched_by: self.voucher(&remote, dialed.ip()).await,
            };
            let (reader, writer) = tokio::io::split(stream);
            let handshake = link::dial(self, reader, writer, auth).await?;
            anyhow::Ok((handshake, flushed))
        })
        .await;
        let failure = match handshake {
            Ok(Ok((handshake, flushed))) => {
                let _transport = self.count_transport();
                let linked = self.conclude(handshake, Some(address)).await;
                let _ = tokio::time::timeout(FLUSH_GRACE, flushed).await;
                return linked;
            }
            Ok(Err(err)) => {
                info!(%address, "linking failed: {err:#}");
                format!("{err:#}")
            }
            Err(_) => {
                warn!(%address, "the peer handshake timed out");
                "the peer handshake timed out".to_owned()
            }
        };
        self.record_error(address, failure);
        false
    }

    async fn noise_handshake(
        self: &Arc<Self>,
        mut stream: DuplexStream,
        remote: PublicKey,
        vouched_by: Voucher,
    ) -> Result<NoiseHandshake> {
        match protocol::accept(&mut stream, &self.version, &self.identity.name).await? {
            Some(Role::Peer) => {}
            Some(Role::Client) => bail!("refused a client greeting over tcp"),
            None => bail!("the connection closed before the greeting"),
        }
        let auth = TransportAuth::Noise {
            key: remote,
            vouched_by,
        };
        let (reader, writer) = tokio::io::split(stream);
        link::accept(self, reader, writer, auth).await
    }

    async fn dial_command(self: &Arc<Self>, address: &str, command: &[String]) -> bool {
        debug!(%address, command = %shell_words::join(command), "dialing");
        let transport = match ssh::spawn(command, address) {
            Ok(transport) => transport,
            Err(err) => {
                warn!(%address, "dialing failed: {err:#}");
                self.record_error(address, format!("{err:#}"));
                return false;
            }
        };
        let ssh::Transport {
            mut child,
            reader,
            writer,
            stderr,
        } = transport;

        let handshake = tokio::time::timeout(
            self.settings().handshake_timeout(),
            link::dial(self, reader, writer, TransportAuth::Ssh),
        )
        .await;
        let failure = match handshake {
            Ok(Ok(handshake)) if matches!(handshake, Handshake::Linked(_)) => {
                let _transport = self.count_transport();
                let linked = self.conclude(handshake, Some(address)).await;
                let _ = tokio::time::timeout(ssh::BRIDGE_EXIT_GRACE, child.wait()).await;
                return linked;
            }
            Ok(Ok(handshake)) => return self.conclude(handshake, Some(address)).await,
            Ok(Err(err)) => {
                info!(%address, "linking failed: {err:#}");
                format!("{err:#}")
            }
            Err(_) => {
                warn!(%address, "the peer handshake timed out");
                "the peer handshake timed out".to_owned()
            }
        };
        let reason = ssh::last_words(&mut child, stderr).await.unwrap_or(failure);
        self.record_error(address, reason);
        false
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
                if let Some(address) = address {
                    self.record_error(address, "the peer is shutting down".to_owned());
                }
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
                        target.last_error = Some(format!(
                            "the peer runs {}, which speaks an incompatible protocol",
                            welcome.version
                        ));
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
            target.last_error = None;
        }
        self.dirty.notify_one();
        self.peers_changed();
    }

    fn target_refused(&self, address: &str, reason: Refusal, peer: Option<&Hello>) {
        let newly_verified = {
            let mut members = self.members();
            let Some(target) = members.targets.get_mut(address) else {
                return;
            };
            let mut newly_verified = false;
            if let Some(peer) = peer {
                target.peer = Some(peer.id);
                if reason != Refusal::SelfDial {
                    newly_verified = !target.verified;
                    target.verified = true;
                }
            }
            match reason {
                Refusal::SelfDial => {
                    info!(%address, "not dialing an address that leads back to this server");
                    target.is_self = true;
                }
                Refusal::Duplicate => {}
                reason => target.last_error = Some(reason.to_string()),
            }
            newly_verified
        };
        self.dirty.notify_one();
        if newly_verified {
            self.changed();
            self.peers_changed();
        }
    }

    fn record_error(&self, address: &str, error: String) {
        if let Some(target) = self.members().targets.get_mut(address) {
            target.last_error = Some(error);
        }
        self.dirty.notify_one();
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
                .map(|(address, target)| target.cached(address))
                .collect();
            Cache { peers, targets }
        };
        if let Err(err) = cache::write(path, &cache) {
            warn!("saving the cluster cache failed: {err:#}");
        }
    }

    fn save_trust(&self) -> Result<()> {
        let Some(path) = &self.trust_path else {
            return Ok(());
        };
        let _saving = self
            .saving_trust
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let trust = self.members().trust.clone();
        trust.save(path)
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

    fn is_absent(&self, peer: ServerId) -> bool {
        let mut discovered = self
            .targets
            .values()
            .filter(|target| target.peer == Some(peer) && target.is_discovered())
            .peekable();
        discovered.peek().is_some() && discovered.all(Target::is_absent)
    }

    fn drop_server(&mut self, id: ServerId) -> Option<String> {
        let mut name = None;
        self.targets.retain(|_, target| {
            let dropped = !target.is_self && target.peer == Some(id);
            if dropped {
                target.stop.notify_one();
                name.get_or_insert_with(|| target.name().to_owned());
            }
            !dropped
        });
        if let Some(link) = self.links.get(&id) {
            name = Some(link.name.clone());
            link.handle.stop.notify_one();
        }
        if let Some(peer) = self.peers.remove(&id) {
            name = Some(peer.name);
        }
        name
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

fn is_shareable(address: &str) -> bool {
    address
        .parse::<Address>()
        .is_ok_and(|address| !address.is_local_only())
}

fn jittered(delay: Duration) -> Duration {
    let mut bytes = [0; 8];
    let fraction = match getrandom::fill(&mut bytes) {
        Ok(()) => u64::from_ne_bytes(bytes) as f64 / u64::MAX as f64,
        Err(_) => 1.0,
    };
    delay.mul_f64(0.5 + fraction / 2.0)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::protocol::TrustedPeer;

    const PATIENCE: Duration = Duration::from_secs(10);
    const DESK: &str = "000000000000000000000000000000d5";
    const LAPTOP: &str = "000000000000000000000000000000a7";
    const OTHER: &str = "000000000000000000000000000000b3";

    struct Quiet(broadcast::Sender<Event>);

    impl StateSource for Quiet {
        fn subscribe(&self) -> (Snapshot, broadcast::Receiver<Event>) {
            let snapshot = Snapshot {
                incarnation: Incarnation::random().unwrap(),
                seq: 0,
                state: ServerState::default(),
            };
            (snapshot, self.0.subscribe())
        }

        fn refresh_peers(&self) {}

        fn serve_channel(self: Arc<Self>, _channel: Duplex<ClientMessage, ServerMessage>) {}
    }

    struct Fixture {
        cluster: Arc<Cluster>,
        _source: Arc<Quiet>,
    }

    struct Setup {
        servers: Vec<(&'static str, String)>,
        trust: TrustStore,
        discovery: DiscoverySettings,
        settings: ClusterSettings,
        state_dir: Option<PathBuf>,
    }

    fn setup() -> Setup {
        Setup {
            servers: Vec::new(),
            trust: TrustStore::default(),
            discovery: DiscoverySettings::default(),
            settings: ClusterSettings::default(),
            state_dir: None,
        }
    }

    impl Setup {
        fn server(mut self, name: &'static str, address: &str) -> Self {
            self.servers.push((name, address.to_owned()));
            self
        }

        fn build(self) -> Fixture {
            let source = Arc::new(Quiet(broadcast::channel(4).0));
            let weak: Weak<Quiet> = Arc::downgrade(&source);
            let servers = self
                .servers
                .into_iter()
                .map(|(name, address)| {
                    let server = ServerConfig {
                        address,
                        amux_path: None,
                        socket: None,
                    };
                    (name.to_owned(), server)
                })
                .collect();
            let options = ClusterOptions {
                identity: ServerIdentity {
                    id: ServerId::random().unwrap(),
                    name: "here".into(),
                    incarnation: Incarnation::random().unwrap(),
                },
                version: Version::current(),
                socket_name: "test".into(),
                settings: self.settings,
                state_dir: self.state_dir,
                servers,
                discovery: self.discovery,
                trust: self.trust,
                key: NoiseKey::generate().unwrap(),
            };
            Fixture {
                cluster: Cluster::new(options, weak),
                _source: source,
            }
        }
    }

    fn id(text: &str) -> ServerId {
        text.parse().unwrap()
    }

    fn found_on(server: ServerId, name: &str, address: &str) -> Candidate {
        Candidate {
            server: Some(server),
            ..Candidate::new(name, address)
        }
    }

    fn hello(id: ServerId, name: &str) -> Hello {
        Hello {
            id,
            incarnation: Incarnation::random().unwrap(),
            name: name.into(),
            version: Version::current(),
            peers: Vec::new(),
            public_key: None,
        }
    }

    fn keyed(id: ServerId, name: &str, key: PublicKey) -> Hello {
        Hello {
            public_key: Some(key),
            ..hello(id, name)
        }
    }

    fn noise(key: PublicKey, vouched_by: Voucher) -> TransportAuth {
        TransportAuth::Noise { key, vouched_by }
    }

    fn peer_record(name: &str) -> Peer {
        Peer {
            name: name.into(),
            version: None,
            last_seen: None,
            stopped: false,
            state: None,
        }
    }

    fn statuses(cluster: &Cluster) -> Vec<(String, DiscoveryStatus)> {
        cluster
            .discovery_view()
            .into_iter()
            .map(|view| (view.address, view.status))
            .collect()
    }

    fn plan(cluster: &Cluster, address: &str) -> &'static str {
        match cluster.dial_plan(address) {
            DialPlan::Stop => "stop",
            DialPlan::Wait => "wait",
            DialPlan::Dial { .. } => "dial",
        }
    }

    fn verify(cluster: &Cluster, address: &str, peer: ServerId) {
        let mut members = cluster.members();
        let target = members.targets.get_mut(address).unwrap();
        target.verified = true;
        target.peer = Some(peer);
    }

    async fn eventually(what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_failing_discovered_target_is_hidden_and_reports_why() {
        let fixture = setup().build();
        let cluster = &fixture.cluster;
        let script = format!("echo '{}' >&2; exit 127", ssh::NOT_INSTALLED);
        let address = format!("exec:sh -c {}", shell_words::quote(&script));

        cluster.discovered(Via::Tailscale, vec![Candidate::new("phone", &address)]);

        eventually("the bridge's last stderr line", || {
            cluster
                .discovery_view()
                .first()
                .is_some_and(|view| view.last_error.as_deref() == Some(ssh::NOT_INSTALLED))
        })
        .await;
        assert_eq!(
            cluster.discovery_view(),
            vec![DiscoveryView {
                via: Via::Tailscale,
                name: "phone".into(),
                address,
                server: None,
                status: DiscoveryStatus::Failing,
                last_error: Some(ssh::NOT_INSTALLED.into()),
            }]
        );
        assert!(cluster.view().is_empty());
        assert!(cluster.gossip().is_empty());
    }

    #[tokio::test]
    async fn a_discovered_tcp_target_that_refuses_connections_says_why() {
        let fixture = setup().build();
        let cluster = &fixture.cluster;
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();

        cluster.discovered(
            Via::Tailscale,
            vec![Candidate::new("desk", format!("tcp://127.0.0.1:{port}"))],
        );

        eventually("the connection error", || {
            cluster.discovery_view().first().is_some_and(|view| {
                view.last_error.as_deref().is_some_and(|error| {
                    error.starts_with(&format!("connecting to 127.0.0.1:{port}"))
                })
            })
        })
        .await;
        assert!(cluster.view().is_empty());
    }

    #[tokio::test]
    async fn an_unreached_address_that_leaves_its_source_is_dropped() {
        let fixture = setup().build();
        let cluster = &fixture.cluster;
        let desk = Candidate::new("desk", "tcp://100.64.0.2:7447");
        cluster.discovered(Via::Tailscale, vec![desk.clone()]);
        cluster.discovered(Via::Lan, Vec::new());
        assert_eq!(cluster.discovery_view().len(), 1);

        cluster.discovered(Via::Tailscale, Vec::new());

        assert!(cluster.discovery_view().is_empty());
        assert!(cluster.members().targets.is_empty());
    }

    #[tokio::test]
    async fn a_peer_whose_discovered_targets_are_all_absent_is_not_dialed() {
        let desk = id(DESK);
        let fixture = setup().server("desk", "tcp://192.168.0.5:7447").build();
        let cluster = &fixture.cluster;
        let tailnet = "tcp://100.64.0.2:7447";
        let lan = format!("lan://{desk}");
        let gossiped = "tcp://desk.example:7447";
        let configured = "tcp://192.168.0.5:7447";
        let lan_candidate = Candidate {
            endpoints: vec!["192.168.0.5:40000".parse().unwrap()],
            ..found_on(desk, "desk", &lan)
        };
        cluster.discovered(Via::Tailscale, vec![found_on(desk, "desk", tailnet)]);
        cluster.discovered(Via::Lan, vec![lan_candidate]);
        verify(cluster, tailnet, desk);
        verify(cluster, configured, desk);
        cluster.learn(&[PeerAddress {
            id: desk,
            name: "desk-elsewhere".into(),
            address: gossiped.into(),
        }]);
        assert_eq!(plan(cluster, gossiped), "dial");

        cluster.discovered(Via::Tailscale, Vec::new());
        assert_eq!(plan(cluster, gossiped), "dial");
        assert_eq!(plan(cluster, tailnet), "wait");

        cluster.discovered(Via::Lan, Vec::new());
        assert_eq!(plan(cluster, gossiped), "wait");
        assert_eq!(plan(cluster, configured), "dial");
        assert_eq!(
            statuses(cluster),
            vec![(tailnet.to_owned(), DiscoveryStatus::Absent)]
        );

        cluster.discovered(Via::Tailscale, vec![found_on(desk, "desk", tailnet)]);
        assert_eq!(plan(cluster, gossiped), "dial");
        assert_eq!(plan(cluster, tailnet), "dial");
    }

    #[tokio::test]
    async fn only_reachable_dialable_candidates_are_dialed() {
        let desk = id(DESK);
        let fixture = setup().build();
        let cluster = &fixture.cluster;
        let lan = format!("lan://{desk}");
        let untrusted = format!("lan://{}", id(LAPTOP));
        let pairing = Candidate {
            dialable: false,
            pairing: Some("k7".into()),
            ..found_on(id(LAPTOP), "laptop", &untrusted)
        };
        cluster.discovered(Via::Lan, vec![found_on(desk, "desk", &lan), pairing]);

        assert_eq!(plan(cluster, &lan), "wait");
        assert_eq!(plan(cluster, &untrusted), "wait");
        assert_eq!(
            statuses(cluster),
            vec![
                (lan.clone(), DiscoveryStatus::Trying),
                (untrusted.clone(), DiscoveryStatus::PairingOpen),
            ]
        );

        let reachable = Candidate {
            endpoints: vec!["192.168.0.5:40000".parse().unwrap()],
            ..found_on(desk, "desk", &lan)
        };
        let not_paired = Candidate {
            dialable: false,
            ..found_on(id(LAPTOP), "laptop", &untrusted)
        };
        cluster.discovered(Via::Lan, vec![reachable, not_paired]);

        match cluster.dial_plan(&lan) {
            DialPlan::Dial {
                connect,
                max_backoff,
            } => {
                assert_eq!(connect, Connect::Tcp(vec!["192.168.0.5:40000".into()]));
                assert_eq!(max_backoff, Duration::from_secs(10 * 60));
            }
            _ => panic!("expected to dial {lan}"),
        }
        assert_eq!(plan(cluster, &untrusted), "wait");
        assert_eq!(statuses(cluster)[1].1, DiscoveryStatus::NotPaired);
    }

    #[tokio::test]
    async fn the_backoff_ceilings_come_from_the_settings() {
        let fixture = Setup {
            settings: ClusterSettings {
                backoff_max_ms: 5000,
                max_unverified_backoff_ms: 20_000,
                ..ClusterSettings::default()
            },
            ..setup()
        }
        .server("laptop", "ssh://laptop")
        .build();
        let cluster = &fixture.cluster;
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let discovered = format!("tcp://127.0.0.1:{port}");
        cluster.discovered(Via::Tailscale, vec![Candidate::new("phone", &discovered)]);

        let max_backoff = |address: &str| match cluster.dial_plan(address) {
            DialPlan::Dial { max_backoff, .. } => max_backoff,
            _ => panic!("expected to dial {address}"),
        };
        assert_eq!(max_backoff("ssh://laptop"), Duration::from_secs(5));
        assert_eq!(max_backoff(&discovered), Duration::from_secs(20));
    }

    #[tokio::test]
    async fn a_failing_server_is_redialed_after_the_minimum_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let attempts = dir.path().join("attempts");
        let script = format!(
            "echo dialed >> {}; exit 1",
            shell_words::quote(&attempts.display().to_string())
        );
        let address = format!("exec:sh -c {}", shell_words::quote(&script));
        let fixture = Setup {
            settings: ClusterSettings {
                backoff_min_ms: 10,
                backoff_max_ms: 20,
                ..ClusterSettings::default()
            },
            ..setup()
        }
        .server("flaky", &address)
        .build();
        fixture.cluster.start();

        let dialed = || {
            std::fs::read_to_string(&attempts)
                .map(|text| text.lines().count())
                .unwrap_or(0)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while dialed() < 5 {
            assert!(Instant::now() < deadline, "dialed only {} times", dialed());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn woken(cluster: &Cluster, address: &str) -> bool {
        let wake = Arc::clone(&cluster.members().targets[address].wake);
        tokio::time::timeout(Duration::ZERO, wake.notified())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_changed_candidate_wakes_every_target_of_its_peer() {
        let desk = id(DESK);
        let fixture = setup()
            .server("desk", "tcp://10.0.0.1:7447")
            .server("desk-too", "tcp://10.0.0.2:7447")
            .build();
        let cluster = &fixture.cluster;
        verify(cluster, "tcp://10.0.0.1:7447", desk);
        verify(cluster, "tcp://10.0.0.2:7447", desk);
        let candidate = found_on(desk, "desk", "tcp://10.0.0.1:7447");

        cluster.discovered(Via::Tailscale, vec![candidate.clone()]);
        assert!(woken(cluster, "tcp://10.0.0.1:7447").await);
        assert!(woken(cluster, "tcp://10.0.0.2:7447").await);

        cluster.discovered(Via::Tailscale, vec![candidate.clone()]);
        assert!(!woken(cluster, "tcp://10.0.0.1:7447").await);
        assert!(!woken(cluster, "tcp://10.0.0.2:7447").await);

        cluster.discovered(Via::Tailscale, Vec::new());
        cluster.discovered(Via::Tailscale, vec![candidate]);
        assert!(woken(cluster, "tcp://10.0.0.1:7447").await);
    }

    #[tokio::test]
    async fn discovered_commands_never_start_a_server() {
        let fixture = setup().server("desk", "exec:amux bridge").build();
        let cluster = &fixture.cluster;
        cluster.discovered(Via::Lan, vec![Candidate::new("desk", "exec:amux  bridge")]);

        let command = |address: &str| match cluster.dial_plan(address) {
            DialPlan::Dial {
                connect: Connect::Command(argv),
                ..
            } => argv,
            _ => panic!("expected a command for {address}"),
        };
        assert_eq!(command("exec:amux bridge"), ["amux", "bridge"]);
        assert_eq!(
            command("exec:amux  bridge"),
            ["amux", "bridge", "--no-start"]
        );
    }

    #[tokio::test]
    async fn configured_beats_discovered_beats_gossiped() {
        let desk = id(DESK);
        let fixture = setup().server("laptop", "tcp://10.0.0.1:7447").build();
        let cluster = &fixture.cluster;
        let gossip = |address: &str| PeerAddress {
            id: desk,
            name: "desk".into(),
            address: address.into(),
        };
        cluster.learn(&[gossip("tcp://10.0.0.2:7447")]);

        cluster.discovered(
            Via::Tailscale,
            vec![
                Candidate::new("laptop-ts", "tcp://10.0.0.1:7447"),
                Candidate::new("desk-ts", "tcp://10.0.0.2:7447"),
            ],
        );
        cluster.learn(&[gossip("tcp://10.0.0.2:7447")]);

        {
            let members = cluster.members();
            let laptop = &members.targets["tcp://10.0.0.1:7447"];
            assert_eq!(laptop.configured_name(), Some("laptop"));
            let desk = &members.targets["tcp://10.0.0.2:7447"];
            assert!(
                matches!(&desk.origin, Origin::Discovered { name, via: Via::Tailscale } if name == "desk-ts")
            );
        }
        verify(cluster, "tcp://10.0.0.1:7447", id(LAPTOP));
        verify(cluster, "tcp://10.0.0.2:7447", desk);
        let gossiped: Vec<String> = cluster
            .gossip()
            .into_iter()
            .map(|peer| peer.address)
            .collect();
        assert_eq!(gossiped, ["tcp://10.0.0.1:7447"]);
        let names: Vec<String> = cluster
            .discovery_view()
            .into_iter()
            .map(|view| view.name)
            .collect();
        assert_eq!(names, ["desk-ts", "laptop-ts"]);

        cluster
            .add_server(
                "desk".into(),
                ServerConfig {
                    address: "tcp://10.0.0.2:7447".into(),
                    amux_path: None,
                    socket: None,
                },
            )
            .unwrap();
        assert_eq!(
            cluster.members().targets["tcp://10.0.0.2:7447"].configured_name(),
            Some("desk")
        );
    }

    #[tokio::test]
    async fn a_refusal_with_a_hello_verifies_the_address_but_a_self_dial_does_not() {
        let fixture = setup().build();
        let cluster = &fixture.cluster;
        let addresses = ["tcp://10.0.0.1:1", "tcp://10.0.0.2:1", "tcp://10.0.0.3:1"];
        let candidates = addresses
            .iter()
            .map(|address| Candidate::new("x", *address))
            .collect();
        cluster.discovered(Via::Tailscale, candidates);
        let own = cluster.hello();

        cluster.target_refused(
            addresses[0],
            Refusal::Duplicate,
            Some(&hello(id(DESK), "desk")),
        );
        cluster.target_refused(
            addresses[1],
            Refusal::NameTaken,
            Some(&hello(id(LAPTOP), "x")),
        );
        cluster.target_refused(addresses[2], Refusal::SelfDial, Some(&own));

        let members = cluster.members();
        let target = |address: &str| &members.targets[address];
        assert!(target(addresses[0]).verified);
        assert_eq!(target(addresses[0]).peer, Some(id(DESK)));
        assert!(target(addresses[1]).verified);
        assert_eq!(
            target(addresses[1]).last_error.as_deref(),
            Some(Refusal::NameTaken.to_string().as_str())
        );
        assert!(!target(addresses[2]).verified);
        assert!(target(addresses[2]).is_self);
    }

    #[tokio::test]
    async fn forgotten_servers_are_refused_and_never_learned_or_discovered() {
        let desk = id(DESK);
        let mut trust = TrustStore::default();
        trust.forget(desk, Some(PublicKey([4; 32])));
        let fixture = Setup { trust, ..setup() }.build();
        let cluster = &fixture.cluster;

        cluster.learn(&[PeerAddress {
            id: desk,
            name: "desk".into(),
            address: "tcp://10.0.0.2:7447".into(),
        }]);
        cluster.discovered(
            Via::Lan,
            vec![
                found_on(desk, "desk", &format!("lan://{desk}")),
                Candidate {
                    key: Some(PublicKey([4; 32])),
                    ..Candidate::new("desk-again", "tcp://10.0.0.3:7447")
                },
            ],
        );

        assert!(cluster.members().targets.is_empty());
        assert_eq!(
            cluster.check(&hello(desk, "desk"), &TransportAuth::Ssh),
            Err(Refusal::Forgotten)
        );
        let rekeyed = TransportAuth::Noise {
            key: PublicKey([4; 32]),
            vouched_by: Voucher::Nobody,
        };
        assert_eq!(
            cluster.check(&hello(id(LAPTOP), "laptop"), &rekeyed),
            Err(Refusal::Forgotten)
        );
        assert_eq!(
            cluster.check(&hello(id(LAPTOP), "laptop"), &TransportAuth::Ssh),
            Ok(())
        );
    }

    #[tokio::test]
    async fn a_noise_hello_must_come_from_the_id_its_key_is_trusted_for() {
        let (desk, laptop) = (id(DESK), id(LAPTOP));
        let (desk_key, laptop_key) = (PublicKey([1; 32]), PublicKey([2; 32]));
        let mut trust = TrustStore::default();
        trust.witness(desk, "desk", desk_key);
        trust.witness(laptop, "laptop", laptop_key);
        let fixture = Setup { trust, ..setup() }.build();
        let cluster = &fixture.cluster;
        let stranger = PublicKey([3; 32]);

        assert_eq!(
            cluster.check(&hello(desk, "desk"), &noise(desk_key, Voucher::TrustStore)),
            Ok(())
        );
        assert_eq!(
            cluster.check(
                &hello(desk, "desk"),
                &noise(laptop_key, Voucher::TrustStore)
            ),
            Err(Refusal::Untrusted)
        );
        assert_eq!(
            cluster.check(&hello(laptop, "laptop"), &noise(desk_key, Voucher::Nobody)),
            Err(Refusal::Untrusted)
        );
        assert_eq!(
            cluster.check(
                &hello(id(OTHER), "other"),
                &noise(stranger, Voucher::Nobody)
            ),
            Err(Refusal::Untrusted)
        );
        assert_eq!(
            cluster.admit(&noise(stranger, Voucher::Nobody)),
            Err(Refusal::Untrusted)
        );
        assert_eq!(cluster.admit(&noise(desk_key, Voucher::TrustStore)), Ok(()));
        assert_eq!(cluster.admit(&noise(stranger, Voucher::Tailnet)), Ok(()));
        assert_eq!(cluster.admit(&TransportAuth::Ssh), Ok(()));
    }

    #[tokio::test]
    async fn ssh_and_vouched_links_record_their_key_as_direct_evidence() {
        let (desk, laptop) = (id(DESK), id(LAPTOP));
        let dir = tempfile::tempdir().unwrap();
        let fixture = Setup {
            state_dir: Some(dir.path().to_owned()),
            ..setup()
        }
        .build();
        let cluster = &fixture.cluster;

        let first = PublicKey([1; 32]);
        assert_eq!(
            cluster.check(&keyed(desk, "desk", first), &TransportAuth::Ssh),
            Ok(())
        );
        let rotated = PublicKey([2; 32]);
        assert_eq!(
            cluster.check(&keyed(desk, "desk", rotated), &TransportAuth::Ssh),
            Ok(())
        );
        let vouched = PublicKey([3; 32]);
        assert_eq!(
            cluster.check(&hello(laptop, "laptop"), &noise(vouched, Voucher::Tailnet)),
            Ok(())
        );
        assert_eq!(
            cluster.check(
                &hello(id(OTHER), "other"),
                &noise(rotated, Voucher::Pairing)
            ),
            Err(Refusal::Untrusted)
        );

        let saved = TrustStore::load(&dir.path().join(TRUST_FILE)).unwrap();
        assert_eq!(saved, cluster.members().trust);
        assert_eq!(
            saved.trusted,
            [
                TrustedPeer {
                    id: desk,
                    name: "desk".into(),
                    key: rotated,
                    introduced_by: None,
                    direct: true,
                },
                TrustedPeer {
                    id: laptop,
                    name: "laptop".into(),
                    key: vouched,
                    introduced_by: None,
                    direct: true,
                },
            ]
        );
        assert!(cluster.trusts_anyone());
        let tailnet_ip = IpAddr::from([100, 64, 0, 2]);
        assert_eq!(
            cluster.voucher(&rotated, tailnet_ip).await,
            Voucher::TrustStore
        );
        assert_eq!(cluster.voucher(&first, tailnet_ip).await, Voucher::Nobody);
    }

    #[tokio::test]
    async fn a_rotated_key_is_saved_and_carried_by_later_hellos() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Setup {
            state_dir: Some(dir.path().to_owned()),
            ..setup()
        }
        .build();
        let cluster = &fixture.cluster;
        let before = cluster.hello().public_key.unwrap();

        let rotated = cluster.rotate_key().unwrap();

        assert_ne!(rotated, before);
        assert_eq!(cluster.hello().public_key, Some(rotated));
        assert_eq!(cluster.public_key(), rotated);
        assert_eq!(
            NoiseKey::load_or_create(dir.path()).unwrap().public(),
            rotated
        );
    }

    #[tokio::test]
    async fn loopback_tcp_addresses_are_never_gossiped_or_learned() {
        let desk = id(DESK);
        let fixture = setup()
            .server("desk", "tcp://192.168.0.5:7447")
            .server("desk-here", "tcp://127.0.0.1:7447")
            .build();
        let cluster = &fixture.cluster;
        verify(cluster, "tcp://192.168.0.5:7447", desk);
        verify(cluster, "tcp://127.0.0.1:7447", desk);

        let gossiped: Vec<String> = cluster
            .gossip()
            .into_iter()
            .map(|peer| peer.address)
            .collect();
        assert_eq!(gossiped, ["tcp://192.168.0.5:7447"]);

        cluster.learn(&[PeerAddress {
            id: id(LAPTOP),
            name: "laptop".into(),
            address: "tcp://0.0.0.0:7447".into(),
        }]);
        assert_eq!(cluster.members().targets.len(), 2);
    }

    #[tokio::test]
    async fn a_gossiped_tombstone_drops_the_server_like_forget() {
        let (desk, laptop) = (id(DESK), id(LAPTOP));
        let desk_key = PublicKey([1; 32]);
        let mut trust = TrustStore::default();
        trust.witness(desk, "desk", desk_key);
        let fixture = Setup {
            trust,
            ..setup().server("desk", "tcp://192.168.0.5:7447")
        }
        .build();
        let cluster = &fixture.cluster;
        verify(cluster, "tcp://192.168.0.5:7447", desk);
        cluster.members().peers.insert(desk, peer_record("desk"));
        let tombstone = TrustUpdate {
            trusted: Vec::new(),
            forgotten: vec![crate::protocol::ForgottenPeer {
                id: desk,
                key: Some(desk_key),
            }],
        };

        cluster.merge_trust(laptop, &tombstone);

        assert!(cluster.members().targets.is_empty());
        assert!(cluster.view().is_empty());
        assert!(!cluster.trusts_anyone());
        assert_eq!(
            cluster.check(&hello(desk, "desk"), &TransportAuth::Ssh),
            Err(Refusal::Forgotten)
        );
    }

    async fn closed_by_the_server(stream: &mut TcpStream) -> bool {
        let mut byte = [0; 1];
        matches!(
            tokio::time::timeout(PATIENCE, stream.read(&mut byte)).await,
            Ok(Ok(0) | Err(_))
        )
    }

    #[tokio::test]
    async fn the_listener_drops_excess_handshakes_and_closes_pairing_connections() {
        let fixture = setup().build();
        let cluster = &fixture.cluster;
        let listener = Listener::bind(cluster, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let address = listener.local_addr();
        let mut pending = Vec::new();
        for _ in 0..listener::MAX_PENDING_HANDSHAKES {
            pending.push(TcpStream::connect(address).await.unwrap());
        }
        eventually("every handshake slot to fill", || {
            cluster.handshakes.available_permits() == 0
        })
        .await;

        let mut excess = TcpStream::connect(address).await.unwrap();
        assert!(closed_by_the_server(&mut excess).await);

        drop(pending);
        eventually("the handshake slots to free up", || {
            cluster.handshakes.available_permits() == listener::MAX_PENDING_HANDSHAKES
        })
        .await;
        let mut pairing = TcpStream::connect(address).await.unwrap();
        protocol::write_message(&mut pairing, &crate::protocol::TcpOpen::new(TcpKind::Pair))
            .await
            .unwrap();
        assert!(closed_by_the_server(&mut pairing).await);
    }

    #[tokio::test]
    async fn the_listener_answers_only_peer_greetings() {
        let fixture = setup().build();
        let listener = Listener::bind(&fixture.cluster, "127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let stream = TcpStream::connect(listener.local_addr()).await.unwrap();
        let key = NoiseKey::generate().unwrap();
        let mut secured = noise::initiate(stream, &key, TcpKind::Link).await.unwrap();
        assert_eq!(secured.remote, fixture.cluster.public_key());

        let welcome = protocol::greet(&mut secured.stream, Role::Client, &Version::current())
            .await
            .unwrap();

        assert_eq!(welcome.server_name, "here");
        let closed = tokio::time::timeout(PATIENCE, protocol::read_frame(&mut secured.stream))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(closed, None);
    }

    #[tokio::test]
    async fn forgetting_drops_every_target_and_the_peer_record() {
        let desk = id(DESK);
        let dir = tempfile::tempdir().unwrap();
        let fixture = Setup {
            state_dir: Some(dir.path().to_owned()),
            ..setup().server("desk", "tcp://192.168.0.5:7447")
        }
        .build();
        let cluster = &fixture.cluster;
        cluster.discovered(
            Via::Tailscale,
            vec![found_on(desk, "desk-ts", "tcp://100.64.0.2:7447")],
        );
        cluster.learn(&[PeerAddress {
            id: desk,
            name: "desk".into(),
            address: "tcp://desk.example:7447".into(),
        }]);
        verify(cluster, "tcp://192.168.0.5:7447", desk);
        cluster.members().peers.insert(desk, peer_record("desk"));
        assert!(cluster.forget("here").is_err());
        assert!(cluster.forget("nobody").is_err());

        let forgotten = cluster.forget("desk").unwrap();

        assert_eq!(
            forgotten,
            Forgotten {
                name: "desk".into(),
                id: Some(desk),
                configured: vec!["desk".into()],
            }
        );
        assert!(cluster.members().targets.is_empty());
        assert!(cluster.view().is_empty());
        let saved = TrustStore::load(&dir.path().join(TRUST_FILE)).unwrap();
        assert!(saved.is_forgotten(desk, None));
    }

    #[tokio::test]
    async fn a_server_that_was_never_reached_can_be_forgotten_by_its_configured_name() {
        let fixture = setup().server("nowhere", "tcp://192.0.2.1:7447").build();
        let cluster = &fixture.cluster;

        let forgotten = cluster.forget("nowhere").unwrap();

        assert_eq!(forgotten.id, None);
        assert_eq!(forgotten.configured, ["nowhere"]);
        assert!(cluster.members().trust.forgotten.is_empty());
    }

    #[tokio::test]
    async fn cached_discovered_targets_load_absent_and_only_if_reached() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let target = |address: &str, via: Via, verified: bool, is_self: bool| CachedTarget {
            address: address.into(),
            peer: Some(id(DESK)),
            origin: CachedOrigin::Discovered {
                name: "desk".into(),
                via,
            },
            verified,
            last_seen: now,
            is_self,
            last_error: Some("connection refused".into()),
        };
        let cached = Cache {
            peers: BTreeMap::from([(id(DESK), peer_record("desk"))]),
            targets: vec![
                target("tcp://100.64.0.2:7447", Via::Tailscale, true, false),
                target("tcp://100.64.0.3:7447", Via::Tailscale, false, false),
                target("tcp://100.64.0.4:7447", Via::Tailscale, false, true),
                target(
                    "lan://000000000000000000000000000000d5",
                    Via::Lan,
                    true,
                    false,
                ),
            ],
        };
        cache::write(&dir.path().join(CACHE_FILE), &cached).unwrap();

        let fixture = Setup {
            state_dir: Some(dir.path().to_owned()),
            discovery: DiscoverySettings {
                lan: false,
                ..DiscoverySettings::default()
            },
            ..setup()
        }
        .build();

        let members = fixture.cluster.members();
        let addresses: Vec<&str> = members.targets.keys().map(String::as_str).collect();
        assert_eq!(
            addresses,
            ["tcp://100.64.0.2:7447", "tcp://100.64.0.4:7447"]
        );
        let kept = &members.targets["tcp://100.64.0.2:7447"];
        assert!(kept.is_absent());
        assert_eq!(kept.last_error.as_deref(), Some("connection refused"));
        assert!(members.targets["tcp://100.64.0.4:7447"].is_self);
        assert!(members.is_absent(id(DESK)));
    }
}
