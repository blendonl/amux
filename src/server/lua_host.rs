use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock, Weak};
use std::thread;

use anyhow::{anyhow, Context, Result};
use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::sync::{broadcast, watch};
use tracing::{debug, warn};

use super::connection::Origin;
use super::session::Session;
use super::Server;
use crate::cluster::{Cluster, StateSource};
use crate::lua::{self, ConfigPaths, LuaHooks, Process};
use crate::protocol::{Event, ServerStatus, SessionCommand, SessionId, SessionInfo, StateEvent};
use crate::settings::Settings;

const QUEUE_CAPACITY: usize = 1024;
const MAX_DEPTH: u32 = 3;
const THREAD_NAME: &str = "amux-lua-hooks";

thread_local! {
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HookEvent {
    SessionCreated {
        session: String,
    },
    SessionClosed {
        session: String,
    },
    SessionRenamed {
        session: String,
        old: String,
    },
    WindowCreated {
        session: String,
        window: usize,
        name: String,
    },
    WindowClosed {
        session: String,
        window: usize,
        name: String,
    },
    PaneExited {
        session: String,
        window: usize,
        pane: u32,
        status: Option<u32>,
        signal: Option<String>,
    },
    ClientAttached {
        session: String,
        #[serde(serialize_with = "origin_name")]
        origin: Origin,
        clients: usize,
    },
    ClientDetached {
        session: String,
        #[serde(serialize_with = "origin_name")]
        origin: Origin,
        clients: usize,
    },
    PeerOnline {
        peer: String,
    },
    PeerOffline {
        peer: String,
    },
    ServerStarted {
        server: String,
    },
}

impl HookEvent {
    pub fn name(&self) -> &'static str {
        match self {
            Self::SessionCreated { .. } => "session_created",
            Self::SessionClosed { .. } => "session_closed",
            Self::SessionRenamed { .. } => "session_renamed",
            Self::WindowCreated { .. } => "window_created",
            Self::WindowClosed { .. } => "window_closed",
            Self::PaneExited { .. } => "pane_exited",
            Self::ClientAttached { .. } => "client_attached",
            Self::ClientDetached { .. } => "client_detached",
            Self::PeerOnline { .. } => "peer_online",
            Self::PeerOffline { .. } => "peer_offline",
            Self::ServerStarted { .. } => "server_started",
        }
    }
}

fn origin_name<S: Serializer>(origin: &Origin, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(match origin {
        Origin::Local => "local",
        Origin::Peer => "peer",
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum HookAction {
    NewWindow {
        session: String,
    },
    RenameSession {
        session: String,
        name: String,
    },
    RenameWindow {
        session: String,
        window: usize,
        name: String,
    },
    SendKeys {
        session: String,
        window: Option<usize>,
        pane: Option<usize>,
        #[serde(deserialize_with = "keys")]
        keys: Vec<u8>,
    },
    KillSession {
        session: String,
    },
}

fn keys<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    struct Keys;

    impl Visitor<'_> for Keys {
        type Value = Vec<u8>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string of keys")
        }

        fn visit_str<E: de::Error>(self, keys: &str) -> Result<Vec<u8>, E> {
            Ok(keys.as_bytes().to_vec())
        }

        fn visit_bytes<E: de::Error>(self, keys: &[u8]) -> Result<Vec<u8>, E> {
            Ok(keys.to_vec())
        }
    }

    deserializer.deserialize_bytes(Keys)
}

pub trait HookState: Send + Sync {
    fn server_name(&self) -> String;
    fn sessions(&self) -> Vec<SessionInfo>;
}

struct Envelope {
    event: HookEvent,
    depth: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    action: HookAction,
    depth: u32,
}

pub struct LuaHost {
    events: SyncSender<Envelope>,
    requests: UnboundedReceiver<Request>,
    state: Arc<OnceLock<Weak<dyn HookState>>>,
}

impl LuaHost {
    pub fn start(paths: ConfigPaths) -> Result<(Settings, Self)> {
        let (events, inbox) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (outbox, requests) = unbounded_channel();
        let state = Arc::new(OnceLock::new());
        let (loaded, settings) = mpsc::channel();
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        let shared = Arc::clone(&state);
        thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    match lua::load(&paths, Process::Server) {
                        Ok(config) => {
                            if loaded.send(Ok(config.settings.clone())).is_ok() {
                                serve(&LuaHooks::new(config), &inbox, &outbox, &shared);
                            }
                        }
                        Err(err) => {
                            let _ = loaded.send(Err(err));
                        }
                    }
                });
            })
            .context("starting the lua hook thread")?;
        let settings = settings
            .recv()
            .map_err(|_| anyhow!("the lua hook thread stopped while loading the config"))??;
        Ok((
            settings,
            Self {
                events,
                requests,
                state,
            },
        ))
    }
}

fn serve(
    hooks: &LuaHooks,
    inbox: &Receiver<Envelope>,
    outbox: &UnboundedSender<Request>,
    state: &OnceLock<Weak<dyn HookState>>,
) {
    for Envelope { event, depth } in inbox {
        if depth > MAX_DEPTH {
            warn!(
                event = event.name(),
                depth, "hooks kept setting each other off, not running them"
            );
            continue;
        }
        for run in hooks.run(&event, state.get()) {
            match run.result {
                Ok(actions) => {
                    for action in actions {
                        let request = Request {
                            action,
                            depth: depth + 1,
                        };
                        if let Err(unsent) = outbox.send(request) {
                            debug!(request = ?unsent.0, "no server is taking hook requests");
                        }
                    }
                }
                Err(message) => warn!(event = event.name(), hook = %run.hook, "{message}"),
            }
        }
    }
}

#[derive(Clone, Default)]
pub struct HookSink(Arc<OnceLock<SyncSender<Envelope>>>);

impl HookSink {
    pub fn emit(&self, event: HookEvent) {
        self.send(event, DEPTH.get());
    }

    fn send(&self, event: HookEvent, depth: u32) {
        let Some(events) = self.0.get() else {
            return;
        };
        match events.try_send(Envelope { event, depth }) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(dropped)) => {
                warn!(
                    event = dropped.event.name(),
                    "the lua hooks are behind, dropping the event"
                );
            }
        }
    }

    fn connect(&self, events: SyncSender<Envelope>) -> bool {
        self.0.set(events).is_ok()
    }
}

impl Server {
    pub fn attach_lua(self: &Arc<Self>, host: LuaHost) {
        let LuaHost {
            events,
            requests,
            state,
        } = host;
        if !self.hooks.connect(events) {
            warn!("a lua host is already attached");
            return;
        }
        let server: Weak<Self> = Arc::downgrade(self);
        let view: Weak<dyn HookState> = server.clone();
        if state.set(view).is_err() {
            warn!("the lua host already serves another server");
        }
        let (snapshot, events) = self.subscribe();
        let relay = Relay {
            server,
            hooks: self.hooks.clone(),
            names: session_names(snapshot.state.sessions),
            online: online_peers(&self.cluster),
        };
        tokio::spawn(relay.run(events, requests, self.cluster.watch()));
    }

    fn apply_hook(&self, action: &HookAction) -> Result<()> {
        match action {
            HookAction::NewWindow { session } => {
                self.local_session(session)?.run(SessionCommand::NewWindow)
            }
            HookAction::RenameSession { session, name } => {
                self.rename_session(&self.local_session(session)?, name.clone())
            }
            HookAction::RenameWindow {
                session,
                window,
                name,
            } => self
                .local_session(session)?
                .rename_window(Some(*window), name.clone()),
            HookAction::SendKeys {
                session,
                window,
                pane,
                keys,
            } => self
                .local_session(session)?
                .send_keys(*window, *pane, keys.clone()),
            HookAction::KillSession { session } => {
                let session = self.local_session(session)?;
                self.kill_session(&session);
                Ok(())
            }
        }
    }

    fn local_session(&self, name: &str) -> Result<Arc<Session>> {
        self.state()
            .sessions
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("can't find session: {name}"))
    }
}

impl HookState for Server {
    fn server_name(&self) -> String {
        self.identity.name.clone()
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        self.list_sessions()
    }
}

struct Relay {
    server: Weak<Server>,
    hooks: HookSink,
    names: BTreeMap<SessionId, String>,
    online: BTreeSet<String>,
}

impl Relay {
    async fn run(
        mut self,
        mut events: broadcast::Receiver<Event>,
        mut requests: UnboundedReceiver<Request>,
        mut peers: watch::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                biased;
                event = events.recv() => match event {
                    Ok(event) => self.forward(event.event, 0),
                    Err(RecvError::Lagged(skipped)) => self.lagged(skipped),
                    Err(RecvError::Closed) => break,
                },
                Ok(()) = peers.changed() => self.peers_changed(),
                request = requests.recv() => match request {
                    Some(request) => self.apply(&request, &mut events),
                    None => break,
                },
            }
        }
    }

    fn apply(&mut self, request: &Request, events: &mut broadcast::Receiver<Event>) {
        let Some(server) = self.server.upgrade() else {
            return;
        };
        let outer = DEPTH.replace(request.depth);
        let applied = server.apply_hook(&request.action);
        DEPTH.set(outer);
        if let Err(err) = applied {
            warn!(action = ?request.action, "a hook request failed: {err:#}");
        }
        loop {
            match events.try_recv() {
                Ok(event) => self.forward(event.event, request.depth),
                Err(TryRecvError::Lagged(skipped)) => self.lagged(skipped),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
        }
    }

    fn forward(&mut self, event: StateEvent, depth: u32) {
        let hook = match event {
            StateEvent::SessionCreated(info) => {
                self.names.insert(info.id, info.name.clone());
                HookEvent::SessionCreated { session: info.name }
            }
            StateEvent::SessionChanged(info) => {
                match self.names.insert(info.id, info.name.clone()) {
                    Some(old) if old != info.name => HookEvent::SessionRenamed {
                        session: info.name,
                        old,
                    },
                    _ => return,
                }
            }
            StateEvent::SessionClosed(id) => match self.names.remove(&id) {
                Some(session) => HookEvent::SessionClosed { session },
                None => return,
            },
            StateEvent::ProjectsChanged(_) | StateEvent::PeersChanged(_) => return,
        };
        self.hooks.send(hook, depth);
    }

    fn lagged(&mut self, skipped: u64) {
        warn!(skipped, "the lua hooks missed session events");
        if let Some(server) = self.server.upgrade() {
            self.names = session_names(server.list_sessions());
        }
    }

    fn peers_changed(&mut self) {
        let Some(server) = self.server.upgrade() else {
            return;
        };
        let online = online_peers(&server.cluster);
        for peer in online.difference(&self.online) {
            let peer = peer.clone();
            self.hooks.send(HookEvent::PeerOnline { peer }, 0);
        }
        for peer in self.online.difference(&online) {
            let peer = peer.clone();
            self.hooks.send(HookEvent::PeerOffline { peer }, 0);
        }
        self.online = online;
    }
}

fn session_names(sessions: Vec<SessionInfo>) -> BTreeMap<SessionId, String> {
    sessions
        .into_iter()
        .map(|session| (session.id, session.name))
        .collect()
}

fn online_peers(cluster: &Cluster) -> BTreeSet<String> {
    cluster
        .view()
        .into_iter()
        .filter(|view| matches!(view.status, ServerStatus::Online { .. }))
        .map(|view| view.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::super::{SessionName, SessionSpec};
    use super::*;
    use crate::cluster::{ClusterOptions, LinkSettings, NoiseKey, TrustStore};
    use crate::config::{Config, Incarnation, ServerId, ServerIdentity};
    use crate::discovery::DiscoveryOptions;
    use crate::lua::{EVENTS, INIT_FILE};
    use crate::project::Registry;
    use crate::protocol::{Size, Version};

    const WAIT: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(10);
    const SERVER_NAME: &str = "hooks";
    const DEPTH_WARNING: &str = "hooks kept setting each other off";
    const DESCRIBE: &str = "local function describe(ev)\n\
                              local keys = {}\n\
                              for key in pairs(ev) do keys[#keys + 1] = key end\n\
                              table.sort(keys)\n\
                              for i, key in ipairs(keys) do keys[i] = key .. '=' .. tostring(ev[key]) end\n\
                              return table.concat(keys, ' ')\n\
                            end\n";

    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Logs {
        fn subscriber(&self) -> impl tracing::Subscriber {
            let writer = self.clone();
            tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_ansi(false)
                .finish()
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    struct Hosted {
        dir: TempDir,
        settings: Settings,
        host: LuaHost,
        logs: Logs,
    }

    impl Hosted {
        fn feed(&self, event: HookEvent, depth: u32) {
            self.host.events.send(Envelope { event, depth }).unwrap();
        }

        fn next_request(&mut self) -> Request {
            let deadline = Instant::now() + WAIT;
            loop {
                match self.host.requests.try_recv() {
                    Ok(request) => return request,
                    Err(_) if Instant::now() < deadline => thread::sleep(POLL),
                    Err(err) => panic!("no hook request arrived: {err}\n{}", self.logs.text()),
                }
            }
        }
    }

    fn start(source: &str) -> Hosted {
        start_in(tempfile::tempdir().unwrap(), source)
    }

    fn start_in(dir: TempDir, source: &str) -> Hosted {
        let init = dir.path().join(INIT_FILE);
        fs::write(&init, source).unwrap();
        let paths = ConfigPaths {
            dir: dir.path().to_owned(),
            init: Some(init),
        };
        let logs = Logs::default();
        let (settings, host) =
            tracing::subscriber::with_default(logs.subscriber(), || LuaHost::start(paths)).unwrap();
        Hosted {
            dir,
            settings,
            host,
            logs,
        }
    }

    fn quoted(path: &Path) -> String {
        format!("{:?}", path.to_str().unwrap())
    }

    fn created(session: &str) -> HookEvent {
        HookEvent::SessionCreated {
            session: session.into(),
        }
    }

    fn closed(session: &str) -> HookEvent {
        HookEvent::SessionClosed {
            session: session.into(),
        }
    }

    fn window_created(session: &str) -> HookEvent {
        HookEvent::WindowCreated {
            session: session.into(),
            window: 1,
            name: "sh".into(),
        }
    }

    fn request(action: HookAction, depth: u32) -> Request {
        Request { action, depth }
    }

    fn new_window(session: &str) -> HookAction {
        HookAction::NewWindow {
            session: session.into(),
        }
    }

    fn kill_session(session: &str) -> HookAction {
        HookAction::KillSession {
            session: session.into(),
        }
    }

    fn every_event() -> Vec<HookEvent> {
        vec![
            created("work"),
            closed("work"),
            HookEvent::SessionRenamed {
                session: "play".into(),
                old: "work".into(),
            },
            HookEvent::WindowCreated {
                session: "work".into(),
                window: 1,
                name: "logs".into(),
            },
            HookEvent::WindowClosed {
                session: "work".into(),
                window: 1,
                name: "logs".into(),
            },
            HookEvent::PaneExited {
                session: "work".into(),
                window: 2,
                pane: 7,
                status: None,
                signal: Some("Hangup".into()),
            },
            HookEvent::ClientAttached {
                session: "work".into(),
                origin: Origin::Local,
                clients: 2,
            },
            HookEvent::ClientDetached {
                session: "work".into(),
                origin: Origin::Peer,
                clients: 0,
            },
            HookEvent::PeerOnline {
                peer: "laptop".into(),
            },
            HookEvent::PeerOffline {
                peer: "laptop".into(),
            },
            HookEvent::ServerStarted {
                server: "desktop".into(),
            },
        ]
    }

    #[test]
    fn every_event_is_named_after_its_hook() {
        let names: Vec<&str> = every_event().iter().map(HookEvent::name).collect();
        assert_eq!(names, EVENTS);
    }

    #[test]
    fn each_hook_receives_its_event_payload() {
        let events: Vec<String> = EVENTS.iter().map(|event| format!("'{event}'")).collect();
        let mut hosted = start(&format!(
            "{DESCRIBE}\
             for _, event in ipairs({{ {} }}) do\n\
               amux.on(event, function(ev) amux.send_keys {{ session = event, keys = describe(ev) }} end)\n\
             end",
            events.join(", ")
        ));
        for event in every_event() {
            hosted.feed(event, 0);
        }
        let described: Vec<(String, String)> = EVENTS
            .iter()
            .map(|_| match hosted.next_request().action {
                HookAction::SendKeys { session, keys, .. } => {
                    (session, String::from_utf8(keys).unwrap())
                }
                other => panic!("{other:?}"),
            })
            .collect();
        let expected = [
            "event=session_created session=work",
            "event=session_closed session=work",
            "event=session_renamed old=work session=play",
            "event=window_created name=logs session=work window=1",
            "event=window_closed name=logs session=work window=1",
            "event=pane_exited pane=7 session=work signal=Hangup window=2",
            "clients=2 event=client_attached origin=local session=work",
            "clients=0 event=client_detached origin=peer session=work",
            "event=peer_online peer=laptop",
            "event=peer_offline peer=laptop",
            "event=server_started server=desktop",
        ];
        let expected: Vec<(String, String)> = EVENTS
            .iter()
            .zip(expected)
            .map(|(event, payload)| ((*event).to_owned(), payload.to_owned()))
            .collect();
        assert_eq!(described, expected);
    }

    #[test]
    fn hook_requests_come_out_one_level_deeper() {
        let mut hosted = start(
            "amux.on('session_created', function(ev)\n\
               amux.new_window { session = ev.session }\n\
               amux.kill_session('old')\n\
             end)",
        );
        hosted.feed(created("work"), 0);
        hosted.feed(created("deep"), 2);
        let requests: Vec<Request> = (0..4).map(|_| hosted.next_request()).collect();
        assert_eq!(
            requests,
            [
                request(new_window("work"), 1),
                request(kill_session("old"), 1),
                request(new_window("deep"), 3),
                request(kill_session("old"), 3),
            ]
        );
    }

    #[test]
    fn a_runaway_hook_is_cut_off() {
        let mut hosted = start(
            "amux.on('session_created', function()\n\
               while true do end\n\
             end)\n\
             amux.on('session_created', function(ev) amux.kill_session(ev.session) end)",
        );
        let started = Instant::now();
        hosted.feed(created("work"), 0);
        assert_eq!(hosted.next_request(), request(kill_session("work"), 1));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(5),
            "{elapsed:?}"
        );
        let logs = hosted.logs.text();
        assert!(
            logs.contains("init.lua:2: a hook ran past its 1000 ms budget"),
            "{logs}"
        );
        assert!(logs.contains("init.lua:1"), "{logs}");
        assert!(logs.contains("session_created"), "{logs}");
    }

    #[test]
    fn a_failing_hook_is_logged_and_later_events_still_run() {
        let mut hosted = start(
            "amux.on('session_created', function(ev)\n\
               amux.kill_session(ev.session)\n\
               error('boom in ' .. ev.session)\n\
             end)\n\
             amux.on('session_closed', function(ev) amux.kill_session(ev.session) end)",
        );
        hosted.feed(created("work"), 0);
        hosted.feed(closed("next"), 0);
        assert_eq!(hosted.next_request(), request(kill_session("next"), 1));
        let logs = hosted.logs.text();
        assert!(logs.contains("init.lua:3: boom in work"), "{logs}");
        assert!(logs.contains("init.lua:1"), "{logs}");
        assert!(logs.contains("session_created"), "{logs}");
    }

    #[test]
    fn hooks_stop_running_past_the_depth_limit() {
        let mut hosted = start(
            "amux.on('window_created', function(ev) amux.new_window { session = ev.session } end)\n\
             amux.on('session_closed', function(ev) amux.kill_session(ev.session) end)",
        );
        hosted.feed(window_created("work"), MAX_DEPTH);
        hosted.feed(window_created("work"), MAX_DEPTH + 1);
        hosted.feed(closed("next"), 0);
        assert_eq!(
            hosted.next_request(),
            request(new_window("work"), MAX_DEPTH + 1)
        );
        assert_eq!(hosted.next_request(), request(kill_session("next"), 1));
        let logs = hosted.logs.text();
        assert!(logs.contains(DEPTH_WARNING), "{logs}");
    }

    #[test]
    fn the_sink_tags_events_with_the_depth_of_the_request_being_applied() {
        let mut hosted = start(
            "amux.on('window_created', function(ev) amux.new_window { session = ev.session } end)",
        );
        let sink = HookSink::default();
        sink.emit(window_created("unattached"));
        assert!(sink.connect(hosted.host.events.clone()));
        assert!(!sink.connect(hosted.host.events.clone()));
        sink.emit(window_created("top"));
        let outer = DEPTH.replace(2);
        sink.emit(window_created("nested"));
        DEPTH.set(outer);
        assert_eq!(hosted.next_request(), request(new_window("top"), 1));
        assert_eq!(hosted.next_request(), request(new_window("nested"), 3));
    }

    #[test]
    fn emitting_never_waits_for_a_busy_host() {
        let mut hosted = start(
            "amux.on('session_created', function() while true do end end)\n\
             amux.on('session_created', function(ev) amux.kill_session(ev.session) end)",
        );
        let sink = HookSink::default();
        assert!(sink.connect(hosted.host.events.clone()));
        sink.emit(created("busy"));
        let started = Instant::now();
        tracing::subscriber::with_default(hosted.logs.subscriber(), || {
            for _ in 0..=QUEUE_CAPACITY {
                sink.emit(HookEvent::PeerOnline {
                    peer: "laptop".into(),
                });
            }
        });
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
        assert_eq!(hosted.next_request(), request(kill_session("busy"), 1));
        let logs = hosted.logs.text();
        assert!(logs.contains("dropping the event"), "{logs}");
    }

    fn test_server(dir: &Path, settings: Settings) -> Arc<Server> {
        let name = SERVER_NAME.to_owned();
        let identity = ServerIdentity {
            id: ServerId::random().unwrap(),
            name: name.clone(),
            incarnation: Incarnation::random().unwrap(),
        };
        let config = Config::parse("", dir, &name).unwrap();
        let registry = Registry::load(&dir.join("projects.toml")).unwrap();
        let discovery = DiscoveryOptions {
            config: config.discovery.clone(),
            lan: config.lan.clone(),
            socket_name: name.clone(),
            state_dir: None,
        };
        let options = ClusterOptions {
            identity: identity.clone(),
            key: NoiseKey::generate().unwrap(),
            version: Version::current(),
            socket_name: name,
            settings: LinkSettings::default(),
            state_dir: None,
            servers: BTreeMap::new(),
            discovery: config.discovery.clone(),
            trust: TrustStore::default(),
        };
        Server::new(
            identity,
            config,
            settings,
            dir.join(INIT_FILE),
            registry,
            options,
            discovery,
        )
    }

    fn spawn(server: &Arc<Server>, name: &str, cwd: &Path) -> Arc<Session> {
        server
            .spawn_session(SessionSpec {
                name: SessionName::Given(name.into()),
                cwd,
                size: Size { rows: 24, cols: 80 },
                env: &[],
                binding: None,
            })
            .unwrap()
    }

    fn listed(session: &Session) -> Vec<(usize, String)> {
        session
            .windows()
            .into_iter()
            .map(|window| (window.index, window.name))
            .collect()
    }

    async fn until(what: &str, logs: &Logs, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\n{}",
                logs.text()
            );
            tokio::time::sleep(POLL).await;
        }
    }

    #[tokio::test]
    async fn a_session_created_hook_writes_a_file_and_opens_a_window() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("created.txt");
        let Hosted {
            dir,
            settings,
            host,
            logs,
        } = start_in(
            dir,
            &format!(
                "amux.opt.pane.shell = {{ '/bin/sh' }}\n\
                 amux.opt.window.name = 'shell'\n\
                 amux.on('session_created', function(ev)\n\
                   local windows = #amux.session(ev.session).windows\n\
                   local file = assert(io.open({note}, 'w'))\n\
                   file:write(ev.session, ' on ', amux.server_name(), ' had ', windows, ' window')\n\
                   file:close()\n\
                   amux.new_window {{ session = ev.session }}\n\
                 end)",
                note = quoted(&note)
            ),
        );
        let server = test_server(dir.path(), settings);
        server.attach_lua(host);
        let session = spawn(&server, "work", dir.path());

        until("the hook's window", &logs, || session.windows().len() == 2).await;
        assert_eq!(
            fs::read_to_string(&note).unwrap(),
            "work on hooks had 1 window"
        );
        assert_eq!(
            listed(&session),
            [(0, "shell".to_owned()), (1, "shell".to_owned())]
        );
        session.kill();
    }

    #[tokio::test]
    async fn a_window_created_hook_that_opens_a_window_stops_at_the_depth_limit() {
        let Hosted {
            dir,
            settings,
            host,
            logs,
        } = start(
            "amux.opt.pane.shell = { '/bin/sh' }\n\
             amux.on('session_created', function(ev) amux.new_window { session = ev.session } end)\n\
             amux.on('window_created', function(ev) amux.new_window { session = ev.session } end)",
        );
        let server = test_server(dir.path(), settings);
        server.attach_lua(host);
        let session = spawn(&server, "work", dir.path());

        until("the depth warning", &logs, || {
            logs.text().contains(DEPTH_WARNING)
        })
        .await;
        let indexes: Vec<usize> = listed(&session)
            .into_iter()
            .map(|(index, _)| index)
            .collect();
        assert_eq!(indexes, [0, 1, 2, 3, 4]);
        session.kill();
    }

    #[tokio::test]
    async fn send_keys_reaches_the_pane_and_its_exit_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal.txt");
        let Hosted {
            dir,
            settings,
            host,
            logs,
        } = start_in(
            dir,
            &format!(
                "amux.opt.pane.shell = {{ '/bin/sh' }}\n\
                 amux.opt.window.name = 'shell'\n\
                 local function note(...)\n\
                   local file = assert(io.open({journal}, 'a'))\n\
                   file:write(table.concat({{ ... }}, ' '), '\\n')\n\
                   file:close()\n\
                 end\n\
                 amux.on('session_created', function(ev)\n\
                   amux.new_window {{ session = ev.session }}\n\
                   amux.send_keys {{ session = ev.session, window = 0, keys = 'exit 3\\r' }}\n\
                 end)\n\
                 amux.on('pane_exited', function(ev)\n\
                   note(ev.event, ev.session, ev.window, ev.pane, tostring(ev.status), tostring(ev.signal))\n\
                 end)\n\
                 amux.on('window_closed', function(ev) note(ev.event, ev.session, ev.window, ev.name) end)",
                journal = quoted(&journal)
            ),
        );
        let server = test_server(dir.path(), settings);
        server.attach_lua(host);
        let session = spawn(&server, "work", dir.path());

        let written = || fs::read_to_string(&journal).unwrap_or_default();
        until("the exit to be reported", &logs, || {
            written().lines().count() == 2
        })
        .await;
        assert_eq!(
            written(),
            "pane_exited work 0 0 3 nil\nwindow_closed work 0 shell\n"
        );
        assert_eq!(listed(&session), [(1, "shell".to_owned())]);
        session.kill();
    }
}
