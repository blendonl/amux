mod connection;
mod forward;
mod layout;
mod mouse;
mod pane;
mod projects;
mod render;
mod session;
mod window;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, Notify};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::cluster::{Cluster, ClusterOptions, LinkSettings, StateSource};
use crate::config::{Config, Incarnation, ServerId, ServerIdentity};
use crate::paths;
use crate::project::Registry;
use crate::protocol::{
    self, ClientMessage, Duplex, Event, NewSession, PeerAddress, ProjectCheckout, Role,
    ServerMessage, ServerState, ServerStatus, ServerView, SessionId, SessionInfo, Size, Snapshot,
    StateEvent, Version, WindowSummary,
};
use crate::target::{self, Candidate, Target};
use connection::Origin;
use forward::{Host, RemoteSession};
use projects::{blocking, Projects, REGISTRY_FILE};
use session::{Binding, Session};

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

    let registry_path = state_dir.join(REGISTRY_FILE);
    let registry = blocking(move || Registry::load(&registry_path)).await?;
    info!(
        projects = registry.projects().len(),
        "project registry loaded"
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
    let server = Server::new(identity, config, registry, options);
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
        Some(Role::Client) => {
            connection::handle(server, protocol::duplex(reader, writer), Origin::Local).await
        }
        Some(Role::Peer) => server.cluster.accept(reader, writer).await,
        None => Ok(()),
    }
}

enum Resolved {
    Local(Arc<Session>),
    Remote(RemoteSession),
}

pub struct Server {
    identity: ServerIdentity,
    config: Config,
    state: Mutex<LocalState>,
    events: broadcast::Sender<Event>,
    cluster: Arc<Cluster>,
    projects: Projects,
    shutdown: Notify,
}

#[derive(Default)]
struct LocalState {
    sessions: BTreeMap<String, Arc<Session>>,
    last_session_id: u64,
    seq: u64,
    peers: Vec<PeerAddress>,
    projects: Vec<ProjectCheckout>,
}

enum SessionName {
    Given(String),
    Derived(String),
    Numbered,
}

struct SessionSpec<'a> {
    name: SessionName,
    cwd: &'a Path,
    size: Size,
    env: &'a [(String, String)],
    binding: Option<Binding>,
}

impl Server {
    fn new(
        identity: ServerIdentity,
        config: Config,
        registry: Registry,
        options: ClusterOptions,
    ) -> Arc<Self> {
        let state = LocalState {
            projects: projects::checkouts(&registry),
            ..LocalState::default()
        };
        Arc::new_cyclic(|server: &Weak<Self>| {
            let source: Weak<dyn StateSource> = server.clone();
            Self {
                identity,
                config,
                state: Mutex::new(state),
                events: broadcast::channel(EVENT_CAPACITY).0,
                cluster: Cluster::new(options, source),
                projects: Projects::new(registry),
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

    fn incarnation(&self) -> Incarnation {
        self.identity.incarnation
    }

    async fn create_session(self: &Arc<Self>, request: &NewSession) -> Result<Arc<Session>> {
        if let Some(name) = &request.name {
            target::validate_session_name(name)?;
        }
        if let Some(project) = &request.project {
            return self.create_project_session(request, project).await;
        }
        if request.branch.is_some() || request.clone {
            bail!("a branch or --clone needs a project");
        }
        let cwd = match &request.cwd {
            Some(cwd) => cwd.clone(),
            None => paths::home_dir()?,
        };
        self.spawn_session(SessionSpec {
            name: request
                .name
                .clone()
                .map_or(SessionName::Numbered, SessionName::Given),
            cwd: &cwd,
            size: request.size,
            env: &request.env,
            binding: None,
        })
    }

    fn spawn_session(self: &Arc<Self>, spec: SessionSpec<'_>) -> Result<Arc<Session>> {
        let mut state = self.state();
        let name = match spec.name {
            SessionName::Given(name) if state.sessions.contains_key(&name) => {
                bail!("duplicate session: {name}")
            }
            SessionName::Given(name) => name,
            SessionName::Derived(base) => free_name_like(&state.sessions, base),
            SessionName::Numbered => next_free_name(&state.sessions),
        };
        state.last_session_id += 1;
        let id = SessionId(state.last_session_id);
        let session = Session::spawn(
            id,
            name.clone(),
            spec.cwd,
            spec.size,
            spec.env,
            spec.binding,
        )?;
        state.sessions.insert(name.clone(), Arc::clone(&session));
        self.publish(&mut state, StateEvent::SessionCreated(session.info()));
        drop(state);
        info!(session = %name, %id, cwd = %spec.cwd.display(), "session created");

        tokio::spawn(Arc::clone(self).watch_session(Arc::clone(&session)));
        Ok(session)
    }

    async fn watch_session(self: Arc<Self>, session: Arc<Session>) {
        let mut windows = session.watch_windows();
        let mut published = Instant::now();
        let mut pending = false;
        loop {
            let flush = tokio::time::sleep_until((published + ACTIVITY_INTERVAL).into());
            tokio::select! {
                changed = windows.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    self.session_changed(&session);
                    published = Instant::now();
                    pending = false;
                }
                () = session.activity() => pending = true,
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

    fn resolve(&self, target: &Target, origin: Origin) -> Result<Resolved> {
        let local_name = &self.identity.name;
        let local: Vec<(String, Arc<Session>)> = self
            .state()
            .sessions
            .iter()
            .map(|(name, session)| (name.clone(), Arc::clone(session)))
            .collect();
        let mut servers = vec![local_name.clone()];
        let mut candidates: Vec<Candidate> = local
            .iter()
            .map(|(name, session)| Candidate {
                server: local_name.clone(),
                session: name.clone(),
                last_activity: millis(session.last_activity()),
                is_local: true,
            })
            .collect();
        let views: Vec<ServerView> = match origin {
            Origin::Local => self
                .cluster
                .view()
                .into_iter()
                .filter(|view| view.name != *local_name)
                .collect(),
            Origin::Peer => Vec::new(),
        };
        let mut peers = Vec::new();
        for view in &views {
            candidates.extend(view.sessions.iter().map(|session| Candidate {
                server: view.name.clone(),
                session: session.name.clone(),
                last_activity: millis(session.last_activity),
                is_local: false,
            }));
            servers.push(view.name.clone());
            peers.push((view.name.clone(), view.id));
        }

        let found = target::resolve_with_servers(target, &servers, &candidates)?;
        if found.is_local {
            let session = local
                .into_iter()
                .find(|(name, _)| *name == found.session)
                .map(|(_, session)| session)
                .ok_or_else(|| anyhow!("can't find session: {}", found.session))?;
            check_position(target, &found.session, &session.windows())?;
            return Ok(Resolved::Local(session));
        }
        let windows = views
            .iter()
            .filter(|view| view.name == found.server)
            .flat_map(|view| &view.sessions)
            .find(|session| session.name == found.session)
            .map(|session| session.windows.as_slice())
            .unwrap_or_default();
        check_position(target, &found.to_string(), windows)?;
        let peer = self.reachable_peer(&found.server, &peers)?;
        Ok(Resolved::Remote(RemoteSession {
            host: Host {
                peer,
                name: found.server.clone(),
            },
            target: Target {
                session: Some(found.session.clone()),
                server: Some(found.server.clone()),
                ..target.clone()
            },
        }))
    }

    fn host_for_new_session(&self, request: &NewSession, origin: Origin) -> Result<Option<Host>> {
        let on = match (&request.on, origin) {
            (Some(on), _) => Some(on.as_str()),
            (None, Origin::Local) => self.default_server(request.project.as_ref()),
            (None, Origin::Peer) => None,
        };
        let Some(on) = on.filter(|on| *on != self.identity.name) else {
            return Ok(None);
        };
        if origin == Origin::Peer {
            bail!("unknown server: {on}");
        }
        let peers: Vec<(String, Option<ServerId>)> = self
            .cluster
            .view()
            .into_iter()
            .map(|view| (view.name, view.id))
            .collect();
        if !peers.iter().any(|(name, _)| name == on) {
            bail!("unknown server: {on}");
        }
        let peer = self.reachable_peer(on, &peers)?;
        Ok(Some(Host {
            peer,
            name: on.to_owned(),
        }))
    }

    fn reachable_peer(&self, name: &str, peers: &[(String, Option<ServerId>)]) -> Result<ServerId> {
        peers
            .iter()
            .filter(|(peer, _)| peer == name)
            .filter_map(|(_, id)| *id)
            .find(|id| self.cluster.is_linked(*id))
            .ok_or_else(|| anyhow!("server {name} is offline"))
    }

    fn find_by_id(&self, incarnation: Incarnation, id: SessionId) -> Option<Arc<Session>> {
        if incarnation != self.identity.incarnation {
            return None;
        }
        self.state()
            .sessions
            .values()
            .find(|session| session.id() == id)
            .cloned()
    }

    fn kill_session(&self, session: &Session) {
        let mut state = self.state();
        let before = state.sessions.len();
        state.sessions.retain(|_, known| known.id() != session.id());
        if state.sessions.len() != before {
            self.publish(&mut state, StateEvent::SessionClosed(session.id()));
        }
        drop(state);
        session.kill();
        info!(session = %session.name(), "session killed");
    }

    fn rename_session(&self, session: &Arc<Session>, name: String) -> Result<()> {
        target::validate_session_name(&name)?;
        let mut state = self.state();
        let old = state
            .sessions
            .iter()
            .find(|(_, known)| known.id() == session.id())
            .map(|(old, _)| old.clone())
            .ok_or_else(|| anyhow!("can't find session: {}", session.name()))?;
        if old == name {
            return Ok(());
        }
        if state.sessions.contains_key(&name) {
            bail!("duplicate session: {name}");
        }
        let renamed = state
            .sessions
            .remove(&old)
            .expect("the session was just found");
        renamed.rename(name.clone());
        state.sessions.insert(name.clone(), Arc::clone(&renamed));
        self.publish(&mut state, StateEvent::SessionChanged(renamed.info()));
        drop(state);
        info!(from = %old, to = %name, "session renamed");
        Ok(())
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
            projects: self.state().projects.clone(),
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
        let before = state.sessions.len();
        state.sessions.retain(|_, known| known.id() != session.id());
        if state.sessions.len() != before {
            self.publish(&mut state, StateEvent::SessionClosed(session.id()));
        }
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
                projects: state.projects.clone(),
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

    fn serve_channel(self: Arc<Self>, channel: Duplex<ClientMessage, ServerMessage>) {
        tokio::spawn(async move {
            if let Err(err) = connection::handle(self, channel, Origin::Peer).await {
                warn!("channel failed: {err:#}");
            }
        });
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

fn check_position(target: &Target, session: &str, windows: &[WindowSummary]) -> Result<()> {
    let Some(index) = target.window else {
        return Ok(());
    };
    let window = windows
        .iter()
        .find(|window| window.index == target_index(index))
        .ok_or_else(|| anyhow!("can't find window {index} in session {session}"))?;
    match target.pane {
        Some(pane) if target_index(pane) >= window.panes => {
            bail!("can't find pane {pane} in window {index} of session {session}")
        }
        _ => Ok(()),
    }
}

fn target_index(index: u32) -> usize {
    usize::try_from(index).unwrap_or(usize::MAX)
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn free_name_like(sessions: &BTreeMap<String, Arc<Session>>, base: String) -> String {
    if !sessions.contains_key(&base) {
        return base;
    }
    (2..)
        .map(|suffix: usize| format!("{base}-{suffix}"))
        .find(|name| !sessions.contains_key(name))
        .expect("an unused suffix always exists")
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
