mod connection;
#[cfg_attr(not(test), allow(dead_code))]
mod layout;
#[cfg_attr(not(test), allow(dead_code))]
mod mouse;
mod pane;
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
mod render;
mod session;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, Notify};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::cluster::{Cluster, ClusterOptions, LinkSettings, StateSource};
use crate::config::{Config, ServerIdentity};
use crate::paths;
use crate::protocol::{
    self, Event, PeerAddress, Role, ServerState, ServerStatus, ServerView, SessionId, SessionInfo,
    Size, Snapshot, StateEvent, Version,
};
use session::Session;

const LOG_FILTER_ENV: &str = "AMUX_LOG";
const EVENT_CAPACITY: usize = 256;
const ACTIVITY_INTERVAL: Duration = Duration::from_secs(5);

pub async fn run(socket: &Path, config_path: Option<&Path>) -> Result<()> {
    init_logging();
    let config_path = match config_path {
        Some(path) => path.to_owned(),
        None => paths::config_path()?,
    };
    let config = Config::load(&config_path)?;
    info!(
        path = %config_path.display(),
        servers = config.servers.len(),
        projects = config.projects.len(),
        "config loaded"
    );
    let state_dir = paths::state_dir(socket)?;
    let identity = ServerIdentity::load(&state_dir, config.name.clone())?;
    info!(
        name = %identity.name,
        id = %identity.id,
        incarnation = %identity.incarnation,
        "server identity"
    );

    let listener = bind(socket)?;
    let mut terminate = signal(SignalKind::terminate())?;
    let options = ClusterOptions {
        identity: identity.clone(),
        version: Version::current(),
        socket_name: paths::socket_name(socket)?,
        settings: LinkSettings::from_env()?,
        state_dir: Some(state_dir),
        servers: config.servers.clone(),
    };
    let server = Server::new(identity, config, options);
    server.cluster.start();
    info!(socket = %socket.display(), version = %Version::current(), "server started");

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let server = Arc::clone(&server);
                    tokio::spawn(async move {
                        if let Err(err) = serve(server, stream).await {
                            warn!("connection failed: {err:#}");
                        }
                    });
                }
                Err(err) => warn!("accept failed: {err}"),
            },
            () = server.shutdown.notified() => break,
            _ = terminate.recv() => break,
        }
    }

    server.cluster.shutdown().await;
    server.state().sessions.clear();
    if let Err(err) = fs::remove_file(socket) {
        warn!("removing {} failed: {err}", socket.display());
    }
    info!("server stopped");
    Ok(())
}

async fn serve(server: Arc<Server>, mut stream: UnixStream) -> Result<()> {
    let role = protocol::accept(&mut stream, &Version::current(), &server.identity.name).await?;
    let (reader, writer) = stream.into_split();
    match role {
        Some(Role::Client) => connection::handle(server, protocol::duplex(reader, writer)).await,
        Some(Role::Peer) => server.cluster.accept(reader, writer).await,
        None => Ok(()),
    }
}

pub struct Server {
    identity: ServerIdentity,
    config: Config,
    state: Mutex<LocalState>,
    events: broadcast::Sender<Event>,
    cluster: Arc<Cluster>,
    shutdown: Notify,
}

#[derive(Default)]
struct LocalState {
    sessions: BTreeMap<String, Arc<Session>>,
    last_session_id: u64,
    seq: u64,
    peers: Vec<PeerAddress>,
}

impl Server {
    fn new(identity: ServerIdentity, config: Config, options: ClusterOptions) -> Arc<Self> {
        Arc::new_cyclic(|server: &Weak<Self>| {
            let source: Weak<dyn StateSource> = server.clone();
            Self {
                identity,
                config,
                state: Mutex::default(),
                events: broadcast::channel(EVENT_CAPACITY).0,
                cluster: Cluster::new(options, source),
                shutdown: Notify::new(),
            }
        })
    }

    pub fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn cluster(&self) -> &Arc<Cluster> {
        &self.cluster
    }

    fn create_session(
        self: &Arc<Self>,
        name: Option<String>,
        cwd: &Path,
        size: Size,
    ) -> Result<Arc<Session>> {
        let mut state = self.state();
        let name = match name {
            Some(name) if state.sessions.contains_key(&name) => bail!("duplicate session: {name}"),
            Some(name) => name,
            None => next_free_name(&state.sessions),
        };
        state.last_session_id += 1;
        let id = SessionId(state.last_session_id);
        let session = Arc::new(Session::spawn(id, name.clone(), cwd, size)?);
        state.sessions.insert(name.clone(), Arc::clone(&session));
        self.publish(&mut state, StateEvent::SessionCreated(session.info()));
        drop(state);
        info!(session = %name, %id, "session created");

        tokio::spawn(Arc::clone(self).watch_session(Arc::clone(&session)));
        Ok(session)
    }

    async fn watch_session(self: Arc<Self>, session: Arc<Session>) {
        let mut updates = session.active_pane().subscribe();
        let mut published = Instant::now();
        let mut pending = false;
        loop {
            let flush = tokio::time::sleep_until((published + ACTIVITY_INTERVAL).into());
            tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    session.touch();
                    pending = true;
                }
                () = session.input_recorded() => pending = true,
                () = flush, if pending => {
                    self.session_changed(&session);
                    published = Instant::now();
                    pending = false;
                }
            }
        }
        self.close_session(&session);
        info!(session = %session.name(), "session exited");
    }

    fn find_session(&self, target: Option<&str>) -> Result<Arc<Session>> {
        let state = self.state();
        let session = match target {
            Some(name) => state
                .sessions
                .get(name)
                .ok_or_else(|| anyhow!("can't find session: {name}"))?,
            None => state
                .sessions
                .values()
                .max_by_key(|session| session.created_at())
                .ok_or_else(|| anyhow!("no sessions"))?,
        };
        Ok(Arc::clone(session))
    }

    fn list_sessions(&self) -> Vec<SessionInfo> {
        self.state()
            .sessions
            .values()
            .map(|session| session.info())
            .collect()
    }

    fn cluster_view(&self) -> Vec<ServerView> {
        let local = ServerView {
            id: Some(self.identity.id),
            name: self.identity.name.clone(),
            address: None,
            version: Some(Version::current()),
            status: ServerStatus::Local,
            sessions: self.list_sessions(),
            projects: Vec::new(),
        };
        let mut servers = vec![local];
        servers.extend(self.cluster.view());
        servers
    }

    fn track_client(self: &Arc<Self>, session: &Arc<Session>) -> AttachedClient {
        session.client_attached();
        self.session_changed(session);
        AttachedClient {
            server: Arc::clone(self),
            session: Arc::clone(session),
        }
    }

    fn session_changed(&self, session: &Session) {
        let mut state = self.state();
        if state
            .sessions
            .values()
            .any(|known| known.id() == session.id())
        {
            self.publish(&mut state, StateEvent::SessionChanged(session.info()));
        }
    }

    fn close_session(&self, session: &Session) {
        let mut state = self.state();
        state.sessions.retain(|_, known| known.id() != session.id());
        self.publish(&mut state, StateEvent::SessionClosed(session.id()));
    }

    fn publish(&self, state: &mut LocalState, event: StateEvent) {
        state.seq += 1;
        let _ = self.events.send(Event {
            incarnation: self.identity.incarnation,
            seq: state.seq,
            event,
        });
    }

    fn shut_down(&self) {
        self.shutdown.notify_one();
    }

    fn state(&self) -> MutexGuard<'_, LocalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl StateSource for Server {
    fn subscribe(&self) -> (Snapshot, broadcast::Receiver<Event>) {
        let state = self.state();
        let events = self.events.subscribe();
        let snapshot = Snapshot {
            incarnation: self.identity.incarnation,
            seq: state.seq,
            state: ServerState {
                sessions: state
                    .sessions
                    .values()
                    .map(|session| session.info())
                    .collect(),
                projects: Vec::new(),
                peers: state.peers.clone(),
            },
        };
        (snapshot, events)
    }

    fn refresh_peers(&self) {
        let mut state = self.state();
        let peers = self.cluster.gossip();
        if peers != state.peers {
            state.peers = peers.clone();
            self.publish(&mut state, StateEvent::PeersChanged(peers));
        }
    }
}

struct AttachedClient {
    server: Arc<Server>,
    session: Arc<Session>,
}

impl Drop for AttachedClient {
    fn drop(&mut self) {
        self.session.client_detached();
        self.server.session_changed(&self.session);
    }
}

fn next_free_name(sessions: &BTreeMap<String, Arc<Session>>) -> String {
    (0..)
        .map(|index: usize| index.to_string())
        .find(|name| !sessions.contains_key(name))
        .expect("an unused session index always exists")
}

fn bind(socket: &Path) -> Result<UnixListener> {
    if StdUnixStream::connect(socket).is_ok() {
        bail!("a server is already listening on {}", socket.display());
    }
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("removing stale {}", socket.display()))
        }
    }
    UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))
}

fn init_logging() {
    let filter = EnvFilter::try_from_env(LOG_FILTER_ENV).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal())
        .init();
}
