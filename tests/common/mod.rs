#![allow(dead_code)]

pub mod git;
pub mod releases;

use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, Permissions};
use std::io::{Read, Write};
use std::ops::Range;
use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use amux::lua::emit;
use amux::protocol::{
    self, AttachedSession, CellPixels, ClientMessage, ClientTerminal, ClusterStatus, Duplex,
    NewSession, Role, ServerMessage, SessionCommand, SessionInfo, SessionState, Size, Version,
    Welcome, WindowSummary,
};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use nix::sys::signal::{kill, Signal};
use nix::unistd::{getuid, Pid};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub const AMUX: &str = env!("CARGO_BIN_EXE_amux");
pub const TIMEOUT: Duration = Duration::from_secs(10);
pub const SIZE: Size = Size { rows: 24, cols: 80 };
pub const DETACH: &str = "\x02d";
const POLL: Duration = Duration::from_millis(10);
const COMMAND_POLL: Duration = Duration::from_millis(50);
const SOCKET_NAME: &str = "amux.sock";
const INIT_FILE: &str = "init.lua";
const SERVERS_FILE: &str = "servers.lua";
const LAN_PORT_FILE: &str = "lan-port";
const WATCH_INTERVAL_MS: u64 = 50;
const FAST_DISCOVERY_MS: &str = "100";
const REMOTE_PATH: &str = "PATH=/usr/bin:/bin";
const OSC52: &[u8] = b"\x1b]52;c;";
const BEL: u8 = 0x07;
pub const KITTY: ClientTerminal = ClientTerminal {
    graphics: true,
    cell_pixels: Some(CellPixels {
        width: 10,
        height: 21,
    }),
};
pub const PLAIN: ClientTerminal = ClientTerminal {
    graphics: false,
    cell_pixels: None,
};
pub const DISCOVERY_OFF: &str =
    "amux.opt.discovery.tailscale = false\namux.opt.discovery.lan = false\n";

const FAKE_SSH: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case $1 in
    -o|-p|-l|-i|-F|-J) shift 2 ;;
    -*) shift ;;
    *) break ;;
  esac
done
host=${1#*@}
shift
command="$*"
hosts=HOSTS
if [ ! -f "$hosts/$host" ]; then
  echo "ssh: Could not resolve hostname $host: Name or service not known" >&2
  exit 255
fi
. "$hosts/$host"
exec env -i "$@" sh -c "$command"
"#;

const FAKE_TAILSCALE: &str = r#"#!/bin/sh
dir=DIR
case $1 in
  status)
    exec cat "$dir/status.json" ;;
  whois)
    shift
    [ "$1" = --json ] && shift
    [ -f "$dir/whois/$1.json" ] || exit 1
    exec cat "$dir/whois/$1.json" ;;
esac
echo "fake tailscale: unsupported command: $*" >&2
exit 1
"#;

static SERVER_COUNT: AtomicUsize = AtomicUsize::new(0);

pub struct Resume(pub Pid);

impl Drop for Resume {
    fn drop(&mut self) {
        let _ = kill(self.0, Signal::SIGCONT);
    }
}

pub struct TestServerBuilder {
    name: String,
    config: String,
    env: Vec<(String, String)>,
    tailscale: bool,
    lan: bool,
    tailscale_tags: Vec<String>,
    tailscale_port: Option<u16>,
    watch_config: bool,
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

    pub fn peer(self, peer: &TestServer) -> Self {
        let entry = peer_entry(peer);
        self.config(&entry)
    }

    pub fn ssh(self, network: &FakeNetwork) -> Self {
        self.env("AMUX_SSH", &network.ssh().display().to_string())
    }

    pub fn tailscale(mut self, tailnet: &FakeTailnet) -> Self {
        self.tailscale = true;
        self.env("AMUX_TAILSCALE", &tailnet.program().display().to_string())
            .env("AMUX_DISCOVERY_INTERVAL_MS", FAST_DISCOVERY_MS)
    }

    pub fn tailscale_tags(mut self, tags: &[&str]) -> Self {
        self.tailscale_tags = tags.iter().map(|tag| tag.to_string()).collect();
        self
    }

    pub fn tailscale_port(mut self, port: u16) -> Self {
        self.tailscale_port = Some(port);
        self
    }

    pub fn watch_config(mut self) -> Self {
        self.watch_config = true;
        self
    }

    pub fn lan(mut self, lan: &FakeLan) -> Self {
        self.lan = true;
        self.env("AMUX_LAN_DIR", &lan.path().display().to_string())
            .env("AMUX_DISCOVERY_INTERVAL_MS", FAST_DISCOVERY_MS)
    }

    pub fn mdns(mut self, service: &str) -> Self {
        self.lan = true;
        self.env("AMUX_LAN_DIR", "")
            .env("AMUX_MDNS_SERVICE", service)
    }

    pub fn start(self) -> TestServer {
        let mut server = self.prepare();
        server.start_process();
        server
    }

    pub fn prepare(self) -> TestServer {
        assert!(
            !self.config.contains("amux.opt.discovery"),
            "turn discovery sources on with the builder, not with amux.opt.discovery"
        );
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
        let mut discovery = String::new();
        if !self.tailscale_tags.is_empty() {
            discovery.push_str(&format!(
                "amux.opt.discovery.tailscale_tags = {}\n",
                lua(&self.tailscale_tags)
            ));
        }
        if let Some(port) = self.tailscale_port {
            discovery.push_str(&format!("amux.opt.discovery.tailscale_port = {port}\n"));
        }
        discovery.push_str(&format!(
            "amux.opt.discovery.tailscale = {}\namux.opt.discovery.lan = {}\n",
            self.tailscale, self.lan
        ));
        discovery.push_str(&format!(
            "amux.opt.reload.watch = {}\namux.opt.reload.interval_ms = {WATCH_INTERVAL_MS}\n",
            self.watch_config
        ));
        fs::write(
            config.join("amux").join(INIT_FILE),
            format!(
                "amux.opt.name = {}\n{}\n{discovery}",
                lua(&self.name),
                self.config
            ),
        )
        .expect("writing init.lua");

        let mut env: Vec<(String, OsString)> = vec![
            ("HOME".into(), home.into()),
            ("XDG_RUNTIME_DIR".into(), runtime.into()),
            ("XDG_STATE_HOME".into(), state.into()),
            ("XDG_CONFIG_HOME".into(), config.into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("AMUX_LOG".into(), "debug".into()),
            (
                "AMUX_TAILSCALE".into(),
                root.path().join("no-tailscale").into(),
            ),
            ("AMUX_LAN_DIR".into(), root.path().join("no-lan").into()),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLine {
    pub peer: String,
    pub name: String,
    pub dialed: bool,
    pub incarnation: String,
    pub state: String,
    pub transport: String,
}

pub fn linked<const N: usize>(builders: [TestServerBuilder; N]) -> [TestServer; N] {
    let servers = builders.map(TestServerBuilder::start);
    for (index, server) in servers.iter().enumerate() {
        for (other, peer) in servers.iter().enumerate() {
            if index != other {
                server.run_ok(&["servers", "add", peer.name(), &peer.bridge_address()]);
            }
        }
    }
    servers
}

pub fn settled_pair(first: TestServerBuilder, second: TestServerBuilder) -> [TestServer; 2] {
    let [first, second] = linked([first, second]);
    let (lower, higher) = if first.server_id() < second.server_id() {
        (&first, &second)
    } else {
        (&second, &first)
    };
    lower.wait_for_links("the link dialed by the lower id", |links| {
        links.len() == 1 && links[0].dialed && links[0].state == "up"
    });
    higher.wait_for_links("the link accepted by the higher id", |links| {
        links.len() == 1 && !links[0].dialed && links[0].state == "up"
    });
    [first, second]
}

fn peer_entry(peer: &TestServer) -> String {
    format!(
        "\namux.opt.servers[{}] = {{ address = {} }}\n",
        lua(peer.name()),
        lua(&peer.bridge_address())
    )
}

pub fn lua<T: serde::Serialize + ?Sized>(value: &T) -> String {
    emit::literal(value).expect("writing a Lua literal")
}

impl TestServer {
    pub fn builder() -> TestServerBuilder {
        let count = SERVER_COUNT.fetch_add(1, Ordering::Relaxed);
        TestServerBuilder {
            name: format!("test-{count}"),
            config: String::new(),
            env: Vec::new(),
            tailscale: false,
            lan: false,
            tailscale_tags: Vec::new(),
            tailscale_port: None,
            watch_config: false,
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
        self.root().join("config").join("amux").join(INIT_FILE)
    }

    pub fn servers_path(&self) -> PathBuf {
        self.root().join("config").join("amux").join(SERVERS_FILE)
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

    pub fn lan_port(&self) -> Option<u16> {
        fs::read_to_string(self.state_dir().join(LAN_PORT_FILE))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn wait_for_lan_port(&self) -> u16 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(port) = self.lan_port() {
                return port;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the LAN port in {}:\n{}",
                self.state_dir().join(LAN_PORT_FILE).display(),
                self.log()
            );
            thread::sleep(POLL);
        }
    }

    pub fn server_id(&self) -> String {
        fs::read_to_string(self.state_dir().join("server-id"))
            .expect("reading the server id")
            .trim()
            .to_owned()
    }

    pub fn pid(&self) -> Pid {
        let child = self.process.as_ref().expect("a server process");
        Pid::from_raw(child.id() as i32)
    }

    pub fn bridge_address(&self) -> String {
        let root = self.root();
        let argv = [
            "env".to_owned(),
            format!("HOME={}", self.home().display()),
            format!("XDG_RUNTIME_DIR={}", root.join("runtime").display()),
            format!("XDG_STATE_HOME={}", root.join("state").display()),
            format!("XDG_CONFIG_HOME={}", root.join("config").display()),
            format!("AMUX_CONFIG={}", self.config_path().display()),
            AMUX.to_owned(),
            "-S".to_owned(),
            self.socket.display().to_string(),
            "bridge".to_owned(),
        ];
        format!("exec:{}", shell_words::join(argv))
    }

    pub fn add_peer(&self, peer: &TestServer) {
        let mut config = File::options()
            .append(true)
            .open(self.config_path())
            .expect("opening init.lua");
        config
            .write_all(peer_entry(peer).as_bytes())
            .expect("adding a peer to init.lua");
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
        self.run_in(&self.home(), args)
    }

    pub fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let child = self
            .command()
            .current_dir(cwd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("running amux");
        wait_with_output(child, args)
    }

    pub fn run_ok(&self, args: &[&str]) -> String {
        self.run_ok_in(&self.home(), args)
    }

    pub fn run_ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.run_in(cwd, args);
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

    pub fn wait_for_log_count(&self, text: &str, count: usize) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let log = self.log();
            if log.matches(text).count() >= count {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {count} of {text:?} in the server log:\n{log}"
            );
            thread::sleep(POLL);
        }
    }

    pub fn wait_for_output(
        &self,
        args: &[&str],
        what: &str,
        done: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let output = self.run(args);
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if output.status.success() && done(&stdout) {
                return stdout;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; `amux {}` printed:\n{stdout}{}\nserver log:\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr),
                self.log()
            );
            thread::sleep(COMMAND_POLL);
        }
    }

    pub fn wait_for_ls(&self, what: &str, done: impl Fn(&Listing) -> bool) -> Listing {
        Listing::parse(&self.wait_for_output(&["ls"], what, |ls| done(&Listing::parse(ls))))
    }

    pub fn links(&self) -> Vec<LinkLine> {
        parse_links(&self.run_ok(&["debug", "links"]))
    }

    pub fn wait_for_links(&self, what: &str, done: impl Fn(&[LinkLine]) -> bool) -> Vec<LinkLine> {
        parse_links(
            &self.wait_for_output(&["debug", "links"], what, |links| done(&parse_links(links))),
        )
    }

    pub fn create_session(&self, name: &str) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("building a runtime");
        runtime.block_on(async {
            let mut client = self.client().await;
            client.new_session(Some(name)).await;
            client.detach().await;
        });
    }

    pub async fn client(&self) -> TestClient {
        TestClient::connect(&self.socket, &self.home()).await
    }

    pub async fn windows_of(&self, session: &str) -> Option<Vec<WindowSummary>> {
        self.client()
            .await
            .list_sessions()
            .await
            .into_iter()
            .find(|info| info.name == session)
            .map(|info| info.windows)
    }

    pub async fn wait_for_windows(&self, session: &str, expected: &[WindowSummary]) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let windows = self.windows_of(session).await;
            if windows.as_deref() == Some(expected) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for windows {expected:?} of {session}, got {windows:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }

    pub fn terminal(&self, args: &[&str]) -> TerminalClient {
        TerminalClient::spawn(self, &self.home(), args, &[])
    }

    pub fn terminal_in(&self, cwd: &Path, args: &[&str]) -> TerminalClient {
        TerminalClient::spawn(self, cwd, args, &[])
    }

    pub fn terminal_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> TerminalClient {
        TerminalClient::spawn(self, &self.home(), args, env)
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
        if let Some(child) = self.process.take() {
            stop(child);
        }
        if self.is_listening() {
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
    }
}

pub fn screen_region(screen: &vt100::Screen, cols: Range<u16>) -> String {
    screen
        .rows(cols.start, cols.end - cols.start)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn screen_row(screen: &vt100::Screen, row: u16) -> String {
    let (_, cols) = screen.size();
    screen
        .contents_between(row, 0, row, cols)
        .trim_end()
        .to_owned()
}

pub fn screen_column(screen: &vt100::Screen, col: u16) -> String {
    let (rows, _) = screen.size();
    (0..rows)
        .map(|row| {
            screen
                .cell(row, col)
                .map(|cell| cell.contents().to_owned())
                .unwrap_or_default()
        })
        .collect()
}

pub fn window_summary(index: usize, panes: usize) -> WindowSummary {
    WindowSummary {
        index,
        name: "sh".into(),
        panes,
    }
}

pub fn terminal_log(session: &str, terminal: ClientTerminal) -> String {
    format!("session={session} terminal={terminal:?}")
}

fn parse_links(output: &str) -> Vec<LinkLine> {
    output
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [peer, name, direction, incarnation, state, transport] = fields[..] else {
                panic!("unexpected link line {line:?}");
            };
            LinkLine {
                peer: peer.to_owned(),
                name: name.to_owned(),
                dialed: direction == "dialed",
                incarnation: incarnation.to_owned(),
                state: state.to_owned(),
                transport: transport.to_owned(),
            }
        })
        .collect()
}

pub struct FakeNetwork {
    root: TempDir,
}

impl FakeNetwork {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("creating the fake network");
        let hosts = root.path().join("hosts");
        fs::create_dir_all(&hosts).expect("creating the fake hosts");
        let script = FAKE_SSH.replace("HOSTS", &shell_words::quote(&hosts.display().to_string()));
        write_script(&root.path().join("ssh"), &script);
        Self { root }
    }

    pub fn ssh(&self) -> PathBuf {
        self.root.path().join("ssh")
    }

    pub fn add_host(&self, host: &str, server: &TestServer) {
        let bin = server.home().join(".cargo").join("bin");
        fs::create_dir_all(&bin).expect("creating ~/.cargo/bin");
        link_once(Path::new(AMUX), &bin.join("amux"));

        let runtime = server.root().join("runtime");
        let sockets = runtime.join(format!("amux-{}", getuid()));
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&sockets)
            .expect("creating the runtime socket dir");
        link_once(server.socket(), &sockets.join(SOCKET_NAME));

        let env = [
            format!("HOME={}", server.home().display()),
            format!("XDG_RUNTIME_DIR={}", runtime.display()),
            format!("XDG_STATE_HOME={}", server.root().join("state").display()),
            format!("XDG_CONFIG_HOME={}", server.root().join("config").display()),
            format!("AMUX_CONFIG={}", server.config_path().display()),
            REMOTE_PATH.to_owned(),
        ];
        fs::write(
            self.root.path().join("hosts").join(host),
            format!("set -- {}\n", shell_words::join(env)),
        )
        .expect("registering a fake host");
    }
}

pub struct FakeTailnet {
    root: TempDir,
}

impl FakeTailnet {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("creating the fake tailnet");
        fs::create_dir_all(root.path().join("whois")).expect("creating the whois dir");
        let dir = shell_words::quote(&root.path().display().to_string()).into_owned();
        write_script(
            &root.path().join("tailscale"),
            &FAKE_TAILSCALE.replace("DIR", &dir),
        );
        Self { root }
    }

    pub fn program(&self) -> PathBuf {
        self.root.path().join("tailscale")
    }

    pub fn set_status(&self, status: &serde_json::Value) {
        replace_json(&self.root.path().join("status.json"), status);
    }

    pub fn set_whois(&self, ip: &str, whois: &serde_json::Value) {
        replace_json(&self.whois_path(ip), whois);
    }

    pub fn remove_whois(&self, ip: &str) {
        let _ = fs::remove_file(self.whois_path(ip));
    }

    fn whois_path(&self, ip: &str) -> PathBuf {
        self.root.path().join("whois").join(format!("{ip}.json"))
    }
}

pub struct FakeLan {
    dir: TempDir,
}

impl FakeLan {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("creating the fake LAN"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

fn write_script(path: &Path, script: &str) {
    fs::write(path, script).expect("writing a fake program");
    fs::set_permissions(path, Permissions::from_mode(0o755)).expect("making it executable");
}

fn link_once(target: &Path, link: &Path) {
    if fs::symlink_metadata(link).is_err() {
        symlink(target, link).expect("creating a symlink");
    }
}

fn replace_json(path: &Path, value: &serde_json::Value) {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, value.to_string()).expect("writing fake JSON");
    fs::rename(&temporary, path).expect("replacing fake JSON");
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub servers: Vec<ListedServer>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListedServer {
    pub name: String,
    pub status: String,
    pub sessions: Vec<ListedSession>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListedSession {
    pub name: String,
    pub details: String,
}

impl Listing {
    pub fn parse(output: &str) -> Self {
        let mut servers: Vec<ListedServer> = Vec::new();
        for line in output.lines() {
            match line.strip_prefix("  ") {
                Some(session) => {
                    let (name, details) = split_first_word(session);
                    servers
                        .last_mut()
                        .expect("a session line before any server")
                        .sessions
                        .push(ListedSession { name, details });
                }
                None => {
                    let (name, status) = split_first_word(line);
                    servers.push(ListedServer {
                        name,
                        status,
                        sessions: Vec::new(),
                    });
                }
            }
        }
        Self { servers }
    }

    pub fn server(&self, name: &str) -> Option<&ListedServer> {
        self.servers.iter().find(|server| server.name == name)
    }

    pub fn status(&self, name: &str) -> Option<&str> {
        self.server(name).map(|server| server.status.as_str())
    }

    pub fn sessions(&self, name: &str) -> Vec<&str> {
        self.server(name)
            .map(|server| {
                server
                    .sessions
                    .iter()
                    .map(|session| session.name.as_str())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_online(&self, name: &str) -> bool {
        self.status(name)
            .is_some_and(|status| status == "online" || status.ends_with(" ms"))
    }
}

fn split_first_word(text: &str) -> (String, String) {
    match text.split_once(' ') {
        Some((first, rest)) => (first.to_owned(), rest.trim().to_owned()),
        None => (text.to_owned(), String::new()),
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
    session_state: Option<SessionState>,
    cluster_statuses: Vec<ClusterStatus>,
    clipboard: Vec<String>,
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
            session_state: None,
            cluster_statuses: Vec::new(),
            clipboard: Vec::new(),
        }
    }

    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    pub fn contents(&self) -> String {
        self.screen.screen().contents()
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.screen.screen()
    }

    pub async fn send(&self, message: ClientMessage) {
        tokio::time::timeout(TIMEOUT, self.outgoing.send(message))
            .await
            .expect("timed out sending to the server")
            .expect("the server closed the connection");
    }

    pub fn session_state(&self) -> Option<&SessionState> {
        self.session_state.as_ref()
    }

    pub fn cluster_statuses(&self) -> &[ClusterStatus] {
        &self.cluster_statuses
    }

    pub fn clipboard(&self) -> &[String] {
        &self.clipboard
    }

    pub async fn recv(&mut self) -> Option<ServerMessage> {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let message = self
            .recv_until(deadline)
            .await
            .expect("timed out waiting for the server");
        if let Some(ServerMessage::Output(bytes)) = &message {
            self.screen.process(bytes);
        }
        message
    }

    async fn recv_until(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<Option<ServerMessage>, tokio::time::error::Elapsed> {
        loop {
            let message = tokio::time::timeout_at(deadline, self.incoming.recv()).await?;
            if let Some(message) = self.absorb_state(message) {
                return Ok(message);
            }
        }
    }

    fn absorb_state(&mut self, message: Option<ServerMessage>) -> Option<Option<ServerMessage>> {
        match message {
            Some(ServerMessage::SessionState(state)) => self.session_state = Some(state),
            Some(ServerMessage::ClusterStatus(status)) => self.cluster_statuses.push(status),
            Some(ServerMessage::Clipboard(text)) => self.clipboard.push(text),
            other => return Some(other),
        }
        None
    }

    pub async fn wait_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while !done(self) {
            let message = tokio::time::timeout_at(deadline, self.incoming.recv())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {what}; state {:?}, cluster {:?}",
                        self.session_state, self.cluster_statuses
                    )
                });
            match self.absorb_state(message) {
                None => {}
                Some(Some(ServerMessage::Output(bytes))) => self.screen.process(&bytes),
                Some(other) => {
                    panic!("expected output or state while waiting for {what}, got {other:?}")
                }
            }
        }
    }

    pub fn session_request(&self, name: Option<&str>) -> NewSession {
        NewSession {
            cwd: Some(self.cwd.clone()),
            ..NewSession::new(name.map(str::to_owned), SIZE)
        }
    }

    pub async fn new_session(&mut self, name: Option<&str>) -> String {
        let request = self.session_request(name);
        self.create(request).await.session
    }

    pub async fn create(&mut self, request: NewSession) -> AttachedSession {
        self.send(ClientMessage::NewSession(request)).await;
        self.expect_attached().await
    }

    pub async fn attach(&mut self, target: Option<&str>) -> String {
        self.attach_to(target.unwrap_or_default()).await.session
    }

    pub async fn attach_to(&mut self, target: &str) -> AttachedSession {
        self.send(ClientMessage::Attach {
            target: target.parse().expect("a valid target"),
            size: SIZE,
        })
        .await;
        self.expect_attached().await
    }

    pub async fn expect_error(&mut self) -> String {
        match self.recv().await {
            Some(ServerMessage::Error(message)) => message,
            other => panic!("expected an error, got {other:?}"),
        }
    }

    pub async fn next_non_output(&mut self) -> Option<ServerMessage> {
        loop {
            match self.recv().await {
                Some(ServerMessage::Output(_)) => {}
                other => return other,
            }
        }
    }

    pub fn reset_screen(&mut self) {
        self.screen = vt100::Parser::new(SIZE.rows, SIZE.cols, 0);
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

    pub async fn command(&self, command: SessionCommand) {
        self.send(ClientMessage::Command(command)).await;
    }

    pub async fn next_output(&mut self) -> Vec<u8> {
        match self.recv().await {
            Some(ServerMessage::Output(bytes)) => bytes,
            other => panic!("expected output, got {other:?}"),
        }
    }

    pub async fn wait_for_text(&mut self, text: &str) -> String {
        self.wait_for_screen(&format!("{text:?}"), |screen| {
            screen.contents().contains(text)
        })
        .await;
        self.contents()
    }

    pub async fn wait_for_screen(&mut self, what: &str, done: impl Fn(&vt100::Screen) -> bool) {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if done(self.screen.screen()) {
                return;
            }
            let contents = self.contents();
            let message = self
                .recv_until(deadline)
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}; screen:\n{contents}"));
            match message {
                Some(ServerMessage::Output(bytes)) => self.screen.process(&bytes),
                other => panic!("expected output while waiting for {what}, got {other:?}"),
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

    pub async fn expect_attached(&mut self) -> AttachedSession {
        match self.recv().await {
            Some(ServerMessage::Attached(attached)) => attached,
            other => panic!("expected to attach, got {other:?}"),
        }
    }
}

type ModesCheck = Box<dyn Fn(&dyn MasterPty) -> bool>;

pub struct TerminalClient {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    exited: bool,
    input: Box<dyn Write + Send>,
    output: std_mpsc::Receiver<Vec<u8>>,
    raw: Vec<u8>,
    screen: vt100::Parser,
    master: Box<dyn MasterPty + Send>,
    modes_unchanged: ModesCheck,
}

impl TerminalClient {
    fn spawn(server: &TestServer, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: SIZE.rows,
                cols: SIZE.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("opening a pty");
        let modes = pair.master.get_termios().expect("reading the pty modes");

        let mut command = CommandBuilder::new(AMUX);
        command.env_clear();
        for (key, value) in server.env() {
            command.env(key, value);
        }
        command.env("TERM", "xterm-256color");
        for (key, value) in env {
            command.env(key, value);
        }
        command.cwd(cwd);
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
            raw: Vec::new(),
            screen: vt100::Parser::new(SIZE.rows, SIZE.cols, 0),
            master: pair.master,
            modes_unchanged: Box::new(move |master| master.get_termios().as_ref() == Some(&modes)),
        }
    }

    pub fn terminal_modes_unchanged(&self) -> bool {
        (self.modes_unchanged)(self.master.as_ref())
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
        self.wait_for(text, |contents| contents.contains(text))
    }

    pub fn wait_for(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        self.wait_for_screen(what, |screen| done(&screen.contents()));
        self.contents()
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.screen.screen()
    }

    pub fn status_line(&self) -> String {
        screen_row(self.screen(), SIZE.rows - 1)
    }

    pub fn wait_for_status(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        self.wait_for_screen(what, |screen| done(&screen_row(screen, SIZE.rows - 1)));
        self.status_line()
    }

    pub fn wait_for_screen(&mut self, what: &str, done: impl Fn(&vt100::Screen) -> bool) {
        self.wait_until(what, |terminal| done(terminal.screen.screen()));
    }

    pub fn wait_for_raw(&mut self, what: &str, done: impl Fn(&[u8]) -> bool) -> &[u8] {
        self.wait_until(what, |terminal| done(&terminal.raw));
        &self.raw
    }

    pub fn clipboard(&self) -> Vec<String> {
        copied_texts(&self.raw)
    }

    pub fn wait_for_clipboard(
        &mut self,
        what: &str,
        done: impl Fn(&[String]) -> bool,
    ) -> Vec<String> {
        self.wait_until(what, |terminal| done(&terminal.clipboard()));
        self.clipboard()
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if done(self) {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.output.recv_timeout(remaining) {
                Ok(bytes) => {
                    self.raw.extend_from_slice(&bytes);
                    self.screen.process(&bytes);
                }
                Err(_) => panic!("timed out waiting for {what}; screen:\n{}", self.contents()),
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

fn copied_texts(raw: &[u8]) -> Vec<String> {
    let mut texts = Vec::new();
    let mut rest = raw;
    while let Some(start) = rest.windows(OSC52.len()).position(|bytes| bytes == OSC52) {
        rest = &rest[start + OSC52.len()..];
        let Some(end) = rest.iter().position(|&byte| byte == BEL) else {
            break;
        };
        let decoded = STANDARD
            .decode(&rest[..end])
            .expect("an OSC 52 payload in base64");
        texts.push(String::from_utf8(decoded).expect("copied text in UTF-8"));
        rest = &rest[end + 1..];
    }
    texts
}

impl Drop for TerminalClient {
    fn drop(&mut self) {
        if !self.exited && matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
