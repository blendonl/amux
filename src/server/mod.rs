mod connection;
mod pane;
mod session;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, IsTerminal};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{anyhow, bail, Context, Result};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Notify;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::{Config, ServerIdentity};
use crate::paths;
use crate::protocol::{self, Role, SessionInfo, Size, Version};
use session::Session;

const LOG_FILTER_ENV: &str = "AMUX_LOG";

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
    let identity = ServerIdentity::load(&paths::state_dir(socket)?, config.name.clone())?;
    info!(
        name = %identity.name,
        id = %identity.id,
        incarnation = %identity.incarnation,
        "server identity"
    );

    let listener = bind(socket)?;
    let mut terminate = signal(SignalKind::terminate())?;
    let server = Arc::new(Server::new(identity, config));
    info!(socket = %socket.display(), version = %Version::current(), "server started");

    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let server = Arc::clone(&server);
                    tokio::spawn(async move {
                        if let Err(err) = serve(server, stream).await {
                            warn!("client connection failed: {err:#}");
                        }
                    });
                }
                Err(err) => warn!("accept failed: {err}"),
            },
            () = server.shutdown.notified() => break,
            _ = terminate.recv() => break,
        }
    }

    server.sessions().clear();
    if let Err(err) = fs::remove_file(socket) {
        warn!("removing {} failed: {err}", socket.display());
    }
    info!("server stopped");
    Ok(())
}

async fn serve(server: Arc<Server>, mut stream: UnixStream) -> Result<()> {
    let role = protocol::accept(&mut stream, &Version::current(), &server.identity.name).await?;
    match role {
        Some(Role::Client) => {
            let (reader, writer) = stream.into_split();
            connection::handle(server, protocol::duplex(reader, writer)).await
        }
        Some(Role::Peer) => bail!("this server does not accept peer links yet"),
        None => Ok(()),
    }
}

pub struct Server {
    identity: ServerIdentity,
    config: Config,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    shutdown: Notify,
}

impl Server {
    fn new(identity: ServerIdentity, config: Config) -> Self {
        Self {
            identity,
            config,
            sessions: Mutex::default(),
            shutdown: Notify::new(),
        }
    }

    pub fn identity(&self) -> &ServerIdentity {
        &self.identity
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn create_session(
        self: &Arc<Self>,
        name: Option<String>,
        cwd: &Path,
        size: Size,
    ) -> Result<Arc<Session>> {
        let mut sessions = self.sessions();
        let name = match name {
            Some(name) if sessions.contains_key(&name) => bail!("duplicate session: {name}"),
            Some(name) => name,
            None => next_free_name(&sessions),
        };
        let session = Arc::new(Session::spawn(name.clone(), cwd, size)?);
        sessions.insert(name.clone(), Arc::clone(&session));
        drop(sessions);
        info!(session = %name, "session created");

        let mut updates = session.active_pane().subscribe();
        let server = Arc::clone(self);
        tokio::spawn(async move {
            while updates.changed().await.is_ok() {}
            server.sessions().remove(&name);
            info!(session = %name, "session exited");
        });

        Ok(session)
    }

    fn find_session(&self, target: Option<&str>) -> Result<Arc<Session>> {
        let sessions = self.sessions();
        let session = match target {
            Some(name) => sessions
                .get(name)
                .ok_or_else(|| anyhow!("can't find session: {name}"))?,
            None => sessions
                .values()
                .max_by_key(|session| session.created_at())
                .ok_or_else(|| anyhow!("no sessions"))?,
        };
        Ok(Arc::clone(session))
    }

    fn list_sessions(&self) -> Vec<SessionInfo> {
        self.sessions()
            .values()
            .map(|session| session.info())
            .collect()
    }

    fn shut_down(&self) {
        self.shutdown.notify_one();
    }

    fn sessions(&self) -> MutexGuard<'_, BTreeMap<String, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
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
