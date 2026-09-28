mod chrome;
mod listing;
mod projects;
mod relay;
mod router;
pub mod scripting;
mod terminal;
mod tree;

use std::env;
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::cli::{Grouping, NewArgs};
use crate::cluster::{Address, LAN_PORT_FILE};
use crate::config;
use crate::lua::{self, ConfigPaths, LuaScripting, Process};
use crate::paths;
use crate::project::{self, Detected};
use crate::protocol::{
    self, is_locale_variable, ClientMessage, DebugCommand, Duplex, IncompatibleServer, NewSession,
    ProjectRef, Role, ServerMessage, ServerView, SessionInfo, Version,
};
use crate::settings::{Keymap, ServerConfig, Settings};
use crate::target::{self, Target};
use relay::Relay;
use scripting::Scripting;
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

struct ClientConfig {
    settings: Arc<Settings>,
    keymap: Arc<Keymap>,
    scripting: Box<dyn Scripting>,
}

impl ClientConfig {
    fn load(endpoint: &Endpoint) -> Result<Self> {
        let loaded = lua::load(&config_paths(endpoint)?, Process::Client)?;
        Ok(Self {
            settings: Arc::new(loaded.settings.clone()),
            keymap: Arc::new(loaded.keymap.clone()),
            scripting: Box::new(LuaScripting::new(loaded)),
        })
    }
}

pub async fn attach_or_create(endpoint: &Endpoint) -> Result<()> {
    let config = ClientConfig::load(endpoint)?;
    let server = handshake(connect_or_start_server(endpoint).await?).await?;
    if request_sessions(server).await?.is_empty() {
        new_session_with(endpoint, NewArgs::default(), config).await
    } else {
        attach_session_with(endpoint, None, config).await
    }
}

pub async fn new_session(endpoint: &Endpoint, args: NewArgs) -> Result<()> {
    new_session_with(endpoint, args, ClientConfig::load(endpoint)?).await
}

async fn new_session_with(endpoint: &Endpoint, args: NewArgs, config: ClientConfig) -> Result<()> {
    if let Some(name) = &args.name {
        target::validate_session_name(name)?;
    }
    let cwd = env::current_dir()?;
    let (project, branch) = binding(endpoint, &args, &cwd).await?;
    let (welcome, server) = greet_server(connect_or_start_server(endpoint).await?).await?;
    let request = ClientMessage::NewSession(NewSession {
        on: args.on,
        cwd: Some(cwd),
        env: locale(),
        project,
        branch,
        clone: args.clone,
        ..NewSession::new(args.name, terminal::session_size(&config.settings.status)?)
    });
    attach(server, &welcome.server_name, request, config).await
}

async fn binding(
    endpoint: &Endpoint,
    args: &NewArgs,
    cwd: &Path,
) -> Result<(Option<ProjectRef>, Option<String>)> {
    if let Some(wanted) = &args.project {
        let server = handshake(connect_or_start_server(endpoint).await?).await?;
        if let Some(project) = projects::find(&request_cluster(server).await?, wanted)? {
            return Ok((Some(project), args.branch.clone()));
        }
        let here = detect(cwd)
            .await?
            .filter(|detected| detected.name == *wanted || detected.id.as_str() == wanted);
        let Some(detected) = here else {
            bail!(
                "unknown project: {wanted}; `amux projects` lists the projects in the cluster, \
                 and `amux project add <path>` registers one"
            );
        };
        return Ok((
            Some(projects::from_detected(&detected)),
            args.branch.clone(),
        ));
    }

    let needs_project = args.branch.is_some() || args.clone;
    let detected = match detect(cwd).await {
        Ok(detected) => detected,
        Err(err) if !needs_project => {
            eprintln!("amux: starting a session without a project: {err:#}");
            None
        }
        Err(err) => return Err(err),
    };
    let Some(detected) = detected else {
        if needs_project {
            bail!(
                "-b and --clone need a project: run amux new inside a git repository, or pass -p"
            );
        }
        return Ok((None, None));
    };
    let branch = args.branch.clone().or_else(|| detected.branch.clone());
    if branch.is_none() && !args.clone {
        return Ok((None, None));
    }
    Ok((Some(projects::from_detected(&detected)), branch))
}

async fn detect(cwd: &Path) -> Result<Option<Detected>> {
    let cwd = cwd.to_owned();
    tokio::task::spawn_blocking(move || project::detect(&cwd))
        .await
        .context("detecting the project failed")?
}

pub async fn attach_session(endpoint: &Endpoint, target: Option<String>) -> Result<()> {
    attach_session_with(endpoint, target, ClientConfig::load(endpoint)?).await
}

async fn attach_session_with(
    endpoint: &Endpoint,
    target: Option<String>,
    config: ClientConfig,
) -> Result<()> {
    let target = parse_target(target)?;
    let (welcome, server) = greet_server(connect_stream(&endpoint.socket).await?).await?;
    let request = ClientMessage::Attach {
        target,
        size: terminal::session_size(&config.settings.status)?,
    };
    attach(server, &welcome.server_name, request, config).await
}

pub async fn kill_session(
    endpoint: &Endpoint,
    target: Option<String>,
    remove_worktree: bool,
) -> Result<()> {
    let target = parse_target(target)?;
    let server = connect(&endpoint.socket).await?;
    let request = ClientMessage::KillSession {
        target,
        remove_worktree,
    };
    expect_done(request_reply(server, request).await?)
}

pub async fn rename_session(
    endpoint: &Endpoint,
    target: Option<String>,
    name: String,
) -> Result<()> {
    let target = parse_target(target)?;
    target::validate_session_name(&name)?;
    let server = connect(&endpoint.socket).await?;
    expect_done(request_reply(server, ClientMessage::RenameSession { target, name }).await?)
}

fn parse_target(target: Option<String>) -> Result<Target> {
    target.as_deref().unwrap_or_default().parse()
}

fn locale() -> Vec<(String, String)> {
    env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| is_locale_variable(key))
        .collect()
}

pub async fn list_cluster(endpoint: &Endpoint, by: Grouping) -> Result<()> {
    let servers = request_cluster(connect(&endpoint.socket).await?).await?;
    let now = SystemTime::now();
    match by {
        Grouping::Server => print!("{}", listing::sessions(&servers, now)),
        Grouping::Project => print!("{}", listing::sessions_by_project(&servers)),
    }
    Ok(())
}

pub async fn list_projects(endpoint: &Endpoint) -> Result<()> {
    let servers = request_cluster(connect(&endpoint.socket).await?).await?;
    print!("{}", listing::projects(&servers));
    Ok(())
}

pub async fn add_project(endpoint: &Endpoint, path: Option<PathBuf>) -> Result<()> {
    let cwd = env::current_dir()?;
    let path = match path {
        Some(path) => std::path::absolute(cwd.join(path))?,
        None => cwd,
    };
    let server = handshake(connect_or_start_server(endpoint).await?).await?;
    match request_reply(server, ClientMessage::AddProject { path }).await? {
        ServerMessage::Project(project) => {
            println!(
                "registered {} ({}) at {}",
                project.name,
                project.id,
                project.path.display()
            );
            Ok(())
        }
        other => bail!("unexpected reply from server: {other:?}"),
    }
}

pub async fn list_servers(endpoint: &Endpoint) -> Result<()> {
    let servers = request_cluster(connect(&endpoint.socket).await?).await?;
    print!("{}", listing::servers(&servers, SystemTime::now()));
    Ok(())
}

pub async fn add_server(endpoint: &Endpoint, name: String, server: ServerConfig) -> Result<()> {
    server.address.parse::<Address>()?;
    config::add_server(&config_paths(endpoint)?, &name, &server)?;
    notify_running_server(endpoint, ClientMessage::AddServer { name, server }).await
}

pub async fn remove_server(endpoint: &Endpoint, name: String) -> Result<()> {
    config::remove_server(&config_paths(endpoint)?, &name)?;
    notify_running_server(endpoint, ClientMessage::RemoveServer { name }).await
}

pub async fn forget_server(endpoint: &Endpoint, server: String) -> Result<()> {
    let connection = handshake(connect_or_start_server(endpoint).await?).await?;
    match request_reply(connection, ClientMessage::ForgetServer { name: server }).await? {
        ServerMessage::Done => Ok(()),
        ServerMessage::Notice(notice) => {
            eprintln!("amux: {notice}");
            Ok(())
        }
        other => bail!("unexpected reply from server: {other:?}"),
    }
}

pub async fn discover(endpoint: &Endpoint) -> Result<()> {
    let server = connect(&endpoint.socket).await?;
    match request_reply(server, ClientMessage::Discover).await? {
        ServerMessage::Discovery(report) => {
            print!("{}", listing::discovery(&report));
            Ok(())
        }
        other => bail!("unexpected reply from server: {other:?}"),
    }
}

pub async fn pair(
    endpoint: &Endpoint,
    code: Option<String>,
    host: Option<String>,
    new_key: bool,
) -> Result<()> {
    let request = match code {
        Some(code) => ClientMessage::JoinPairing {
            code,
            host,
            new_key,
        },
        None => ClientMessage::OpenPairing { new_key },
    };
    let Duplex {
        mut incoming,
        outgoing,
    } = handshake(connect_or_start_server(endpoint).await?).await?;
    println!("{}", listing::PAIRING_WARNING);
    send(&outgoing, request).await?;
    loop {
        match incoming.recv().await {
            Some(ServerMessage::PairingOpen {
                code,
                expires_in_secs,
            }) => print!(
                "{}",
                listing::pairing_instructions(&code, expires_in_secs, lan_port(endpoint))
            ),
            Some(ServerMessage::PairingAttemptFailed {
                reason,
                attempts_left,
            }) => println!("{}", listing::pairing_attempt(&reason, attempts_left)),
            Some(ServerMessage::Paired {
                name,
                id,
                fingerprint,
            }) => {
                println!("paired with {name} ({id}), key fingerprint {fingerprint}");
                return Ok(());
            }
            Some(ServerMessage::PairingClosed { reason }) => bail!(reason),
            Some(ServerMessage::Error(message)) => bail!(message),
            Some(other) => bail!("unexpected reply from server: {other:?}"),
            None => bail!("server closed the connection"),
        }
    }
}

fn lan_port(endpoint: &Endpoint) -> Option<u16> {
    let path = paths::state_dir(&endpoint.socket).ok()?.join(LAN_PORT_FILE);
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub async fn debug_links(endpoint: &Endpoint) -> Result<()> {
    let server = connect(&endpoint.socket).await?;
    let ServerMessage::Links(links) =
        request_reply(server, ClientMessage::Debug(DebugCommand::Links)).await?
    else {
        bail!("unexpected reply from server");
    };
    for link in links {
        let direction = if link.dialed { "dialed" } else { "accepted" };
        println!(
            "{} {} {direction} {} {} {}",
            link.peer, link.name, link.incarnation, link.state, link.transport
        );
    }
    Ok(())
}

pub async fn drop_link(endpoint: &Endpoint, peer: String) -> Result<()> {
    let server = connect(&endpoint.socket).await?;
    expect_done(request_reply(server, ClientMessage::Debug(DebugCommand::DropLink(peer))).await?)
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

pub async fn handshake(stream: UnixStream) -> Result<ServerConnection> {
    Ok(greet_server(stream).await?.1)
}

async fn greet_server(mut stream: UnixStream) -> Result<(protocol::Welcome, ServerConnection)> {
    let welcome = protocol::greet(&mut stream, Role::Client, &Version::current()).await?;
    Ok((welcome, into_connection(stream)))
}

async fn request_reply(server: ServerConnection, message: ClientMessage) -> Result<ServerMessage> {
    let Duplex {
        mut incoming,
        outgoing,
    } = server;
    send(&outgoing, message).await?;
    match incoming.recv().await {
        Some(ServerMessage::Error(message)) => bail!(message),
        Some(reply) => Ok(reply),
        None => bail!("server closed the connection"),
    }
}

async fn request_sessions(server: ServerConnection) -> Result<Vec<SessionInfo>> {
    match request_reply(server, ClientMessage::ListSessions).await? {
        ServerMessage::Sessions(sessions) => Ok(sessions),
        other => bail!("unexpected reply from server: {other:?}"),
    }
}

async fn request_cluster(server: ServerConnection) -> Result<Vec<ServerView>> {
    match request_reply(server, ClientMessage::ListCluster).await? {
        ServerMessage::Cluster(servers) => Ok(servers),
        other => bail!("unexpected reply from server: {other:?}"),
    }
}

fn expect_done(reply: ServerMessage) -> Result<()> {
    match reply {
        ServerMessage::Done => Ok(()),
        other => bail!("unexpected reply from server: {other:?}"),
    }
}

async fn notify_running_server(endpoint: &Endpoint, message: ClientMessage) -> Result<()> {
    let Ok(stream) = UnixStream::connect(&endpoint.socket).await else {
        return Ok(());
    };
    let server = handshake(stream).await?;
    expect_done(request_reply(server, message).await?)
}

fn config_paths(endpoint: &Endpoint) -> Result<ConfigPaths> {
    paths::config_paths(endpoint.config.as_deref())
}

async fn attach(
    server: ServerConnection,
    local: &str,
    request: ClientMessage,
    config: ClientConfig,
) -> Result<()> {
    let Duplex {
        mut incoming,
        outgoing,
    } = server;
    send(&outgoing, request).await?;
    let attached = match incoming.recv().await {
        Some(ServerMessage::Attached(attached)) => attached,
        Some(ServerMessage::Error(message)) => bail!(message),
        Some(other) => bail!("unexpected reply from server: {other:?}"),
        None => bail!("server closed the connection"),
    };

    let mut relay = Relay::new(
        local.to_owned(),
        attached,
        terminal::size()?,
        config.settings,
        config.keymap,
        Some(config.scripting),
    );
    let outcome = {
        let _terminal = RawTerminal::enter()?;
        relay::run(incoming, outgoing, &mut relay).await?
    };
    println!("[{outcome} (from session {})]", relay.label());
    Ok(())
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

pub async fn connect_stream(socket: &Path) -> Result<UnixStream> {
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
