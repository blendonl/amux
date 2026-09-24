#![allow(dead_code)]

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use amux::protocol::{
    self, ClientMessage, Duplex, Role, ServerMessage, SessionInfo, Size, Version, Welcome,
};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub const AMUX: &str = env!("CARGO_BIN_EXE_amux");
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const SIZE: Size = Size { rows: 24, cols: 80 };
pub const DETACH: &str = "\x02d";
const POLL: Duration = Duration::from_millis(10);
const SOCKET_NAME: &str = "amux.sock";

pub struct TestServerBuilder {
    name: String,
    config: String,
    env: Vec<(String, String)>,
}

impl TestServerBuilder {
    pub fn name(mut self, name: &str) -> Self {
        self.name = name.to_owned();
        self
    }

    pub fn config(mut self, extra: &str) -> Self {
        self.config.push_str(extra);
        self.config.push('\n');
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn start(self) -> TestServer {
        let mut server = self.prepare();
        server.start_process();
        server
    }

    pub fn prepare(self) -> TestServer {
        let root = tempfile::tempdir().expect("creating a temp dir");
        let dir = |name: &str| {
            let path = root.path().join(name);
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&path)
                .expect("creating a test dir");
            path
        };
        let home = dir("home");
        let runtime = dir("runtime");
        let state = dir("state");
        let config = dir("config");
        fs::create_dir_all(config.join("amux")).expect("creating the config dir");
        fs::write(
            config.join("amux").join("config.toml"),
            format!("name = {:?}\n{}", self.name, self.config),
        )
        .expect("writing the config");

        let mut env: Vec<(String, OsString)> = vec![
            ("HOME".into(), home.into()),
            ("XDG_RUNTIME_DIR".into(), runtime.into()),
            ("XDG_STATE_HOME".into(), state.into()),
            ("XDG_CONFIG_HOME".into(), config.into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("AMUX_LOG".into(), "debug".into()),
        ];
        env.extend(
            self.env
                .into_iter()
                .map(|(key, value)| (key, OsString::from(value))),
        );

        TestServer {
            socket: root.path().join(SOCKET_NAME),
            root,
            name: self.name,
            env,
            process: None,
        }
    }
}

pub struct TestServer {
    root: TempDir,
    name: String,
    socket: PathBuf,
    env: Vec<(String, OsString)>,
    process: Option<Child>,
}

impl TestServer {
    pub fn builder() -> TestServerBuilder {
        TestServerBuilder {
            name: "test".into(),
            config: String::new(),
            env: Vec::new(),
        }
    }

    pub fn start() -> Self {
        Self::builder().start()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn home(&self) -> PathBuf {
        self.root().join("home")
    }

    pub fn config_path(&self) -> PathBuf {
        self.root().join("config").join("amux").join("config.toml")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.root().join("state").join("amux").join(SOCKET_NAME)
    }

    pub fn log_path(&self) -> PathBuf {
        self.root().join(format!("{SOCKET_NAME}.log"))
    }

    pub fn env(&self) -> &[(String, OsString)] {
        &self.env
    }

    pub fn log(&self) -> String {
        fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(AMUX);
        command
            .env_clear()
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .current_dir(self.home())
            .arg("-S")
            .arg(&self.socket);
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        let child = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("running amux");
        wait_with_output(child, args)
    }

    pub fn run_ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "amux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf-8 output")
    }

    pub fn start_process(&mut self) {
        assert!(self.process.is_none(), "the server is already running");
        let log = File::options()
            .create(true)
            .append(true)
            .open(self.log_path())
            .expect("opening the server log");
        let child = self
            .command()
            .arg("server")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("starting the server");
        self.process = Some(child);
        self.wait_until_listening();
    }

    pub fn stop_process(&mut self) {
        let Some(child) = self.process.take() else {
            return;
        };
        stop(child);
    }

    pub fn restart(&mut self) {
        self.stop_process();
        self.start_process();
    }

    pub fn is_listening(&self) -> bool {
        StdUnixStream::connect(&self.socket).is_ok()
    }

    pub fn wait_for_log(&self, text: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let log = self.log();
            if log.contains(text) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {text:?} in the server log:\n{log}"
            );
            thread::sleep(POLL);
        }
    }

    pub async fn client(&self) -> TestClient {
        TestClient::connect(&self.socket, &self.home()).await
    }

    pub fn terminal(&self, args: &[&str]) -> TerminalClient {
        TerminalClient::spawn(self, args)
    }

    fn wait_until_listening(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if self.is_listening() {
                return;
            }
            let child = self.process.as_mut().expect("a server process");
            if let Some(status) = child.try_wait().expect("polling the server") {
                self.process = None;
                panic!("the server exited with {status}:\n{}", self.log());
            }
            assert!(
                Instant::now() < deadline,
                "the server did not start:\n{}",
                self.log()
            );
            thread::sleep(POLL);
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        match self.process.take() {
            Some(child) => stop(child),
            None if self.is_listening() => {
                if let Ok(child) = self
                    .command()
                    .arg("kill-server")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    stop_after_timeout(child);
                }
            }
            None => {}
        }
    }
}

fn stop(mut child: Child) {
    if matches!(child.try_wait(), Ok(None)) {
        let _ = kill(Pid::from_raw(child.id() as i32), Signal::SIGTERM);
    }
    stop_after_timeout(child);
}

fn stop_after_timeout(mut child: Child) {
    if wait_for_exit(&mut child).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn wait_for_exit(child: &mut Child) -> Option<ExitStatus> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            _ => return None,
        }
    }
}

fn wait_with_output(mut child: Child, args: &[&str]) -> Output {
    if wait_for_exit(&mut child).is_none() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("amux {args:?} did not finish within {TIMEOUT:?}");
    }
    child.wait_with_output().expect("collecting amux output")
}

pub struct TestClient {
    welcome: Welcome,
    cwd: PathBuf,
    incoming: mpsc::Receiver<ServerMessage>,
    outgoing: mpsc::Sender<ClientMessage>,
    screen: vt100::Parser,
}

impl TestClient {
    pub async fn connect(socket: &Path, cwd: &Path) -> Self {
        let mut stream = UnixStream::connect(socket)
            .await
            .expect("connecting to the server");
        let welcome = protocol::greet(&mut stream, Role::Client, &Version::current())
            .await
            .expect("greeting the server");
        let (reader, writer) = stream.into_split();
        let Duplex { incoming, outgoing } = protocol::duplex(reader, writer);
        Self {
            welcome,
            cwd: cwd.to_owned(),
            incoming,
            outgoing,
            screen: vt100::Parser::new(SIZE.rows, SIZE.cols, 0),
        }
    }

    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    pub fn contents(&self) -> String {
        self.screen.screen().contents()
    }

    pub async fn send(&self, message: ClientMessage) {
        tokio::time::timeout(TIMEOUT, self.outgoing.send(message))
            .await
            .expect("timed out sending to the server")
            .expect("the server closed the connection");
    }

    pub async fn recv(&mut self) -> Option<ServerMessage> {
        let message = tokio::time::timeout(TIMEOUT, self.incoming.recv())
            .await
            .expect("timed out waiting for the server");
        if let Some(ServerMessage::Output(bytes)) = &message {
            self.screen.process(bytes);
        }
        message
    }

    pub async fn new_session(&mut self, name: Option<&str>) -> String {
        self.send(ClientMessage::NewSession {
            name: name.map(str::to_owned),
            cwd: self.cwd.clone(),
            size: SIZE,
        })
        .await;
        self.expect_attached().await
    }

    pub async fn attach(&mut self, target: Option<&str>) -> String {
        self.send(ClientMessage::Attach {
            target: target.map(str::to_owned),
            size: SIZE,
        })
        .await;
        self.expect_attached().await
    }

    pub async fn list_sessions(&mut self) -> Vec<SessionInfo> {
        self.send(ClientMessage::ListSessions).await;
        match self.recv().await {
            Some(ServerMessage::Sessions(sessions)) => sessions,
            other => panic!("expected a session list, got {other:?}"),
        }
    }

    pub async fn type_text(&self, text: &str) {
        self.send(ClientMessage::Input(text.as_bytes().to_vec()))
            .await;
    }

    pub async fn next_output(&mut self) -> Vec<u8> {
        match self.recv().await {
            Some(ServerMessage::Output(bytes)) => bytes,
            other => panic!("expected output, got {other:?}"),
        }
    }

    pub async fn wait_for_text(&mut self, text: &str) -> String {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let contents = self.contents();
            if contents.contains(text) {
                return contents;
            }
            let message = tokio::time::timeout_at(deadline, self.incoming.recv())
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {text:?}; screen:\n{contents}"));
            match message {
                Some(ServerMessage::Output(bytes)) => self.screen.process(&bytes),
                other => panic!("expected output while waiting for {text:?}, got {other:?}"),
            }
        }
    }

    pub async fn detach(&mut self) {
        self.send(ClientMessage::Detach).await;
        loop {
            match self.recv().await {
                Some(ServerMessage::Detached) => return,
                Some(ServerMessage::Output(_)) => {}
                other => panic!("expected to detach, got {other:?}"),
            }
        }
    }

    async fn expect_attached(&mut self) -> String {
        match self.recv().await {
            Some(ServerMessage::Attached { session }) => session,
            other => panic!("expected to attach, got {other:?}"),
        }
    }
}

pub struct TerminalClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: bool,
    input: Box<dyn Write + Send>,
    output: std_mpsc::Receiver<Vec<u8>>,
    screen: vt100::Parser,
    _master: Box<dyn MasterPty + Send>,
}

impl TerminalClient {
    fn spawn(server: &TestServer, args: &[&str]) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: SIZE.rows,
                cols: SIZE.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("opening a pty");

        let mut command = CommandBuilder::new(AMUX);
        command.env_clear();
        for (key, value) in server.env() {
            command.env(key, value);
        }
        command.env("TERM", "xterm-256color");
        command.cwd(server.home());
        command.arg("-S");
        command.arg(server.socket());
        command.args(args);

        let child = pair
            .slave
            .spawn_command(command)
            .expect("starting amux in a pty");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("reading the pty");
        let input = pair.master.take_writer().expect("writing the pty");
        let (sender, output) = std_mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(len @ 1..) = reader.read(&mut buffer) {
                if sender.send(buffer[..len].to_vec()).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            exited: false,
            input,
            output,
            screen: vt100::Parser::new(SIZE.rows, SIZE.cols, 0),
            _master: pair.master,
        }
    }

    pub fn contents(&self) -> String {
        self.screen.screen().contents()
    }

    pub fn type_text(&mut self, text: &str) {
        self.input
            .write_all(text.as_bytes())
            .and_then(|()| self.input.flush())
            .expect("typing into the pty");
    }

    pub fn wait_for_text(&mut self, text: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let contents = self.contents();
            if contents.contains(text) {
                return contents;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(remaining) {
                Ok(bytes) => self.screen.process(&bytes),
                Err(_) => panic!("timed out waiting for {text:?}; screen:\n{contents}"),
            }
        }
    }

    pub fn wait_for_exit(&mut self) -> portable_pty::ExitStatus {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("polling amux") {
                self.exited = true;
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "amux did not exit; screen:\n{}",
                self.contents()
            );
            thread::sleep(POLL);
        }
    }
}

impl Drop for TerminalClient {
    fn drop(&mut self) {
        if !self.exited && matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
