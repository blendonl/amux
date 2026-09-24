mod keys;
mod terminal;

use std::env;
use std::fmt;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::signal::unix::{signal, SignalKind};

use crate::paths;
use crate::protocol::{self, ClientMessage, ServerMessage};
use keys::{Action, PrefixRouter, DEFAULT_PREFIX};
use terminal::RawTerminal;

const SERVER_START_ATTEMPTS: u32 = 50;
const SERVER_START_POLL: Duration = Duration::from_millis(20);

pub async fn new_session(socket: &Path, name: Option<String>) -> Result<()> {
    let stream = connect_or_start_server(socket).await?;
    let request = ClientMessage::NewSession {
        name,
        cwd: env::current_dir()?,
        size: terminal::size()?,
    };
    attach(stream, request).await
}

pub async fn attach_session(socket: &Path, target: Option<String>) -> Result<()> {
    let stream = connect(socket).await?;
    let request = ClientMessage::Attach {
        target,
        size: terminal::size()?,
    };
    attach(stream, request).await
}

pub async fn list_sessions(socket: &Path) -> Result<()> {
    let mut stream = connect(socket).await?;
    protocol::write_message(&mut stream, &ClientMessage::ListSessions).await?;
    match protocol::read_message(&mut stream).await? {
        Some(ServerMessage::Sessions(sessions)) => {
            for session in sessions {
                let status = if session.attached_clients > 0 {
                    " (attached)"
                } else {
                    ""
                };
                println!("{}{status}", session.name);
            }
            Ok(())
        }
        Some(ServerMessage::Error(message)) => bail!(message),
        Some(other) => bail!("unexpected reply from server: {other:?}"),
        None => bail!("server closed the connection"),
    }
}

pub async fn kill_server(socket: &Path) -> Result<()> {
    let mut stream = connect(socket).await?;
    protocol::write_message(&mut stream, &ClientMessage::KillServer).await?;
    while protocol::read_message::<_, ServerMessage>(&mut stream)
        .await?
        .is_some()
    {}
    Ok(())
}

async fn attach(stream: UnixStream, request: ClientMessage) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    protocol::write_message(&mut writer, &request).await?;
    let session = match protocol::read_message(&mut reader).await? {
        Some(ServerMessage::Attached { session }) => session,
        Some(ServerMessage::Error(message)) => bail!(message),
        Some(other) => bail!("unexpected reply from server: {other:?}"),
        None => bail!("server closed the connection"),
    };

    let outcome = {
        let _terminal = RawTerminal::enter()?;
        relay(reader, writer).await?
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

async fn relay(reader: OwnedReadHalf, mut writer: OwnedWriteHalf) -> Result<Outcome> {
    let mut incoming = protocol::incoming::<_, ServerMessage>(reader);
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
                    protocol::write_message(&mut writer, &ClientMessage::Detach).await?;
                    continue;
                };
                for action in router.route(&chunk) {
                    let message = match action {
                        Action::Forward(bytes) => ClientMessage::Input(bytes),
                        Action::Detach => ClientMessage::Detach,
                    };
                    protocol::write_message(&mut writer, &message).await?;
                }
            }
            _ = resizes.recv() => {
                let resize = ClientMessage::Resize(terminal::size()?);
                protocol::write_message(&mut writer, &resize).await?;
            }
        }
    }
}

async fn connect(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket)
        .await
        .with_context(|| format!("no server running on {}", socket.display()))
}

async fn connect_or_start_server(socket: &Path) -> Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket).await {
        return Ok(stream);
    }
    start_server(socket)?;
    for _ in 0..SERVER_START_ATTEMPTS {
        tokio::time::sleep(SERVER_START_POLL).await;
        if let Ok(stream) = UnixStream::connect(socket).await {
            return Ok(stream);
        }
    }
    bail!(
        "server did not start, see {}",
        paths::log_path(socket).display()
    )
}

fn start_server(socket: &Path) -> Result<()> {
    let log_path = paths::log_path(socket);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;

    let mut command = Command::new(env::current_exe()?);
    command
        .arg("--socket-path")
        .arg(socket)
        .arg("server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    unsafe {
        command.pre_exec(|| nix::unistd::setsid().map(drop).map_err(Into::into));
    }
    command.spawn().context("starting the server")?;
    Ok(())
}
