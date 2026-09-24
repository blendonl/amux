mod keys;
mod terminal;

use std::env;
use std::fmt;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tokio::net::UnixStream;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use crate::paths;
use crate::protocol::{
    self, ClientMessage, Duplex, IncompatibleServer, Role, ServerMessage, SessionInfo, Version,
};
use keys::{Action, PrefixRouter, DEFAULT_PREFIX};
use terminal::RawTerminal;

const SERVER_START_ATTEMPTS: u32 = 50;
const SERVER_STOP_ATTEMPTS: u32 = 250;
const SERVER_POLL: Duration = Duration::from_millis(20);

pub type ServerConnection = Duplex<ServerMessage, ClientMessage>;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub socket: PathBuf,
    pub config: Option<PathBuf>,
}

pub async fn attach_or_create(endpoint: &Endpoint) -> Result<()> {
    let server = handshake(connect_or_start_server(endpoint).await?).await?;
    if request_sessions(server).await?.is_empty() {
        new_session(endpoint, None).await
    } else {
        attach_session(endpoint, None).await
    }
}

pub async fn new_session(endpoint: &Endpoint, name: Option<String>) -> Result<()> {
    let server = handshake(connect_or_start_server(endpoint).await?).await?;
    let request = ClientMessage::NewSession {
        name,
        cwd: env::current_dir()?,
        size: terminal::size()?,
    };
    attach(server, request).await
}

pub async fn attach_session(endpoint: &Endpoint, target: Option<String>) -> Result<()> {
    let server = connect(&endpoint.socket).await?;
    let request = ClientMessage::Attach {
        target,
        size: terminal::size()?,
    };
    attach(server, request).await
}

pub async fn list_sessions(endpoint: &Endpoint) -> Result<()> {
    let server = connect(&endpoint.socket).await?;
    for session in request_sessions(server).await? {
        let status = if session.attached_clients > 0 {
            " (attached)"
        } else {
            ""
        };
        println!("{}{status}", session.name);
    }
    Ok(())
}

pub async fn kill_server(endpoint: &Endpoint) -> Result<()> {
    let socket = &endpoint.socket;
    let mut stream = connect_stream(socket).await?;
    match protocol::greet(&mut stream, Role::Client, &Version::current()).await {
        Ok(_) => {
            let Duplex {
                mut incoming,
                outgoing,
            } = into_connection(stream);
            send(&outgoing, ClientMessage::KillServer).await?;
            while incoming.recv().await.is_some() {}
        }
        Err(err) if err.is::<IncompatibleServer>() => terminate_incompatible(&stream)?,
        Err(err) => return Err(err),
    }
    wait_until_stopped(socket).await
}

pub async fn connect_or_start_server(endpoint: &Endpoint) -> Result<UnixStream> {
    let socket = &endpoint.socket;
    if let Ok(stream) = UnixStream::connect(socket).await {
        return Ok(stream);
    }
    let mut server = start_server(endpoint)?;
    for _ in 0..SERVER_START_ATTEMPTS {
        let exited = server.try_wait()?.is_some();
        if let Ok(stream) = UnixStream::connect(socket).await {
            return Ok(stream);
        }
        if exited {
            break;
        }
        tokio::time::sleep(SERVER_POLL).await;
    }
    bail!(
        "server did not start, see {}",
        paths::log_path(socket).display()
    )
}

pub async fn handshake(mut stream: UnixStream) -> Result<ServerConnection> {
    protocol::greet(&mut stream, Role::Client, &Version::current()).await?;
    Ok(into_connection(stream))
}

async fn request_sessions(server: ServerConnection) -> Result<Vec<SessionInfo>> {
    let Duplex {
        mut incoming,
        outgoing,
    } = server;
    send(&outgoing, ClientMessage::ListSessions).await?;
    match incoming.recv().await {
        Some(ServerMessage::Sessions(sessions)) => Ok(sessions),
        Some(ServerMessage::Error(message)) => bail!(message),
        Some(other) => bail!("unexpected reply from server: {other:?}"),
        None => bail!("server closed the connection"),
    }
}

async fn attach(server: ServerConnection, request: ClientMessage) -> Result<()> {
    let Duplex {
        mut incoming,
        outgoing,
    } = server;
    send(&outgoing, request).await?;
    let session = match incoming.recv().await {
        Some(ServerMessage::Attached { session }) => session,
        Some(ServerMessage::Error(message)) => bail!(message),
        Some(other) => bail!("unexpected reply from server: {other:?}"),
        None => bail!("server closed the connection"),
    };

    let outcome = {
        let _terminal = RawTerminal::enter()?;
        relay(incoming, outgoing).await?
    };
    println!("[{outcome} (from session {session})]");
    Ok(())
}

enum Outcome {
    Detached,
    Exited,
    ServerExited,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Detached => "detached",
            Self::Exited => "exited",
            Self::ServerExited => "server exited",
        })
    }
}

async fn relay(
    mut incoming: mpsc::Receiver<ServerMessage>,
    outgoing: mpsc::Sender<ClientMessage>,
) -> Result<Outcome> {
    let mut stdin = terminal::stdin_chunks();
    let mut resizes = signal(SignalKind::window_change())?;
    let mut router = PrefixRouter::new(DEFAULT_PREFIX);
    let mut stdin_open = true;

    loop {
        tokio::select! {
            message = incoming.recv() => match message {
                Some(ServerMessage::Output(bytes)) => terminal::write_output(&bytes)?,
                Some(ServerMessage::Detached) => return Ok(Outcome::Detached),
                Some(ServerMessage::Exited) => return Ok(Outcome::Exited),
                Some(ServerMessage::Error(message)) => bail!(message),
                Some(other) => bail!("unexpected message from server: {other:?}"),
                None => return Ok(Outcome::ServerExited),
            },
            chunk = stdin.recv(), if stdin_open => {
                let Some(chunk) = chunk else {
                    stdin_open = false;
                    send(&outgoing, ClientMessage::Detach).await?;
                    continue;
                };
                for action in router.route(&chunk) {
                    let message = match action {
                        Action::Forward(bytes) => ClientMessage::Input(bytes),
                        Action::Detach => ClientMessage::Detach,
                    };
                    send(&outgoing, message).await?;
                }
            }
            _ = resizes.recv() => {
                send(&outgoing, ClientMessage::Resize(terminal::size()?)).await?;
            }
        }
    }
}

async fn send(outgoing: &mpsc::Sender<ClientMessage>, message: ClientMessage) -> Result<()> {
    outgoing
        .send(message)
        .await
        .map_err(|_| anyhow!("server closed the connection"))
}

async fn connect(socket: &Path) -> Result<ServerConnection> {
    handshake(connect_stream(socket).await?).await
}

async fn connect_stream(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket)
        .await
        .with_context(|| format!("no server running on {}", socket.display()))
}

fn into_connection(stream: UnixStream) -> ServerConnection {
    let (reader, writer) = stream.into_split();
    protocol::duplex(reader, writer)
}

fn terminate_incompatible(stream: &UnixStream) -> Result<()> {
    let pid = stream
        .peer_cred()?
        .pid()
        .context("the server's process id is unknown")?;
    kill(Pid::from_raw(pid), Signal::SIGTERM)
        .with_context(|| format!("stopping the incompatible server (pid {pid})"))?;
    eprintln!("stopped an incompatible amux server (pid {pid})");
    Ok(())
}

async fn wait_until_stopped(socket: &Path) -> Result<()> {
    for _ in 0..SERVER_STOP_ATTEMPTS {
        if UnixStream::connect(socket).await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(SERVER_POLL).await;
    }
    bail!("the server on {} did not stop", socket.display())
}

fn start_server(endpoint: &Endpoint) -> Result<Child> {
    let log_path = paths::log_path(&endpoint.socket);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;

    let mut command = Command::new(env::current_exe()?);
    command.arg("--socket-path").arg(&endpoint.socket);
    if let Some(config) = &endpoint.config {
        command.arg("--config").arg(config);
    }
    command
        .arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    unsafe {
        command.pre_exec(|| nix::unistd::setsid().map(drop).map_err(Into::into));
    }
    command.spawn().context("starting the server")
}
