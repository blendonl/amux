mod common;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::hint::black_box;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use amux::protocol::{self, ServerMessage};
use common::{settled_pair, TestServer, TestServerBuilder, AMUX};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use tempfile::TempDir;

const ROWS: u16 = 50;
const COLS: u16 = 200;
const READ_BUFFER_LEN: usize = 64 * 1024;
const WAIT: Duration = Duration::from_secs(120);
const QUIET: Duration = Duration::from_millis(300);
const KEYSTROKES: usize = 200;
const KEYSTROKE_GAP: Duration = Duration::from_millis(50);
const KEYSTROKES_PER_LINE: usize = 40;
const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
const KILL_LINE: &[u8] = b"\x15";
const LOCAL_SEQ_LINES: u64 = 2_000_000;
const REMOTE_SEQ_LINES: u64 = 500_000;
const FLOODS: [&str; 2] = ["yes", "base64 /dev/urandom"];
const FLOOD_WARMUP: Duration = Duration::from_secs(1);
const SLOW_READ_PAUSE: Duration = Duration::from_millis(5);
const SLOW_FLOOD: Duration = Duration::from_secs(2);
const INTERRUPTS: usize = 5;
const CODEC_PAYLOAD_LEN: usize = 100 * 1024;
const CODEC_ROUNDS: usize = 1000;
const SHELL: &str = "amux.opt.pane.shell = { \"/bin/sh\" }";
const LOG_LEVEL: &str = "info";
const CLIENT_TERM: &str = "xterm-256color";
const TMUX_SOCKET: &str = "amuxbench";
const LABEL_WIDTH: usize = 38;
const COLUMN_WIDTH: usize = 14;

struct Chunk {
    arrived: Instant,
    bytes: Vec<u8>,
}

struct BenchTerminal {
    child: Box<dyn Child + Send + Sync>,
    input: Box<dyn Write + Send>,
    output: mpsc::Receiver<Chunk>,
    screen: vt100::Parser,
    received: usize,
    _pty: Box<dyn MasterPty + Send>,
}

impl BenchTerminal {
    fn amux(server: &TestServer, args: &[&str]) -> Self {
        Self::amux_with_read_pause(server, args, Duration::ZERO)
    }

    fn amux_with_read_pause(server: &TestServer, args: &[&str], read_pause: Duration) -> Self {
        let mut command = CommandBuilder::new(AMUX);
        command.env_clear();
        for (key, value) in server.env() {
            command.env(key, value);
        }
        command.env("TERM", CLIENT_TERM);
        command.cwd(server.home());
        command.arg("-S");
        command.arg(server.socket());
        command.args(args);
        Self::spawn(command, read_pause)
    }

    fn spawn(command: CommandBuilder, read_pause: Duration) -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("opening a pty");
        let child = pair
            .slave
            .spawn_command(command)
            .expect("starting a client in a pty");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("reading the pty");
        let input = pair.master.take_writer().expect("writing the pty");
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = vec![0; READ_BUFFER_LEN];
            while let Ok(len @ 1..) = reader.read(&mut buffer) {
                let chunk = Chunk {
                    arrived: Instant::now(),
                    bytes: buffer[..len].to_vec(),
                };
                if sender.send(chunk).is_err() {
                    break;
                }
                thread::sleep(read_pause);
            }
        });

        Self {
            child,
            input,
            output,
            screen: vt100::Parser::new(ROWS, COLS, 0),
            received: 0,
            _pty: pair.master,
        }
    }

    fn pid(&self) -> u32 {
        self.child.process_id().expect("the client's pid")
    }

    fn screen(&self) -> &vt100::Screen {
        self.screen.screen()
    }

    fn write(&mut self, bytes: &[u8]) -> Instant {
        let sent = Instant::now();
        self.input
            .write_all(bytes)
            .and_then(|()| self.input.flush())
            .expect("typing into the pty");
        sent
    }

    fn absorb(&mut self, chunk: Chunk) -> Instant {
        self.received += chunk.bytes.len();
        self.screen.process(&chunk.bytes);
        chunk.arrived
    }

    fn drain(&mut self) {
        while let Ok(chunk) = self.output.try_recv() {
            self.absorb(chunk);
        }
    }

    fn absorb_for(&mut self, duration: Duration) {
        let until = Instant::now() + duration;
        while let Ok(chunk) = self
            .output
            .recv_timeout(until.saturating_duration_since(Instant::now()))
        {
            self.absorb(chunk);
        }
    }

    fn settle(&mut self) {
        let deadline = Instant::now() + WAIT;
        while let Ok(chunk) = self.output.recv_timeout(QUIET) {
            self.absorb(chunk);
            assert!(
                Instant::now() < deadline,
                "the terminal never went quiet; screen:\n{}",
                self.screen().contents()
            );
        }
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&vt100::Screen) -> bool) -> Instant {
        let deadline = Instant::now() + WAIT;
        let mut arrived = Instant::now();
        while !done(self.screen()) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let chunk = self.output.recv_timeout(remaining).unwrap_or_else(|_| {
                panic!(
                    "timed out waiting for {what}; screen:\n{}",
                    self.screen().contents()
                )
            });
            arrived = self.absorb(chunk);
        }
        arrived
    }

    fn wait_for_prompt(&mut self) {
        self.wait_until("a shell prompt", at_prompt);
        self.settle();
    }

    fn wait_for_output(&mut self, output: &str) -> Instant {
        self.wait_until(output, |screen| shows_output(screen, output))
    }
}

impl Drop for BenchTerminal {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Tmux {
    dir: TempDir,
}

impl Tmux {
    fn isolated() -> Option<Self> {
        let installed = Command::new("tmux")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        installed.then(|| Self {
            dir: tempfile::tempdir().expect("creating a tmux dir"),
        })
    }

    fn env(&self) -> Vec<(&'static str, OsString)> {
        vec![
            ("HOME", self.dir.path().into()),
            ("TMUX_TMPDIR", self.dir.path().into()),
            ("PATH", env::var_os("PATH").unwrap_or_default()),
            ("SHELL", "/bin/sh".into()),
            ("TERM", CLIENT_TERM.into()),
        ]
    }

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command
            .env_clear()
            .envs(self.env())
            .args(["-L", TMUX_SOCKET]);
        command
    }

    fn client(&self) -> BenchTerminal {
        let mut command = CommandBuilder::new("tmux");
        command.env_clear();
        for (key, value) in self.env() {
            command.env(key, value);
        }
        command.cwd(self.dir.path());
        command.args([
            "-L",
            TMUX_SOCKET,
            "-f",
            "/dev/null",
            "new-session",
            "/bin/sh",
        ]);
        BenchTerminal::spawn(command, Duration::ZERO)
    }

    fn server_pid(&self) -> u32 {
        let output = self
            .command()
            .args(["list-sessions", "-F", "#{pid}"])
            .output()
            .expect("asking tmux for its pid");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .and_then(|pid| pid.trim().parse().ok())
            .expect("tmux's server pid")
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self
            .command()
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct Latencies(Vec<Duration>);

impl Latencies {
    fn new(mut samples: Vec<Duration>) -> Self {
        samples.sort();
        Self(samples)
    }

    fn percentile(&self, percent: usize) -> Duration {
        let index = (self.0.len() * percent / 100).min(self.0.len() - 1);
        self.0[index]
    }

    fn row(&self) -> Vec<String> {
        [50, 90, 99]
            .map(|percent| millis(self.percentile(percent)))
            .to_vec()
    }
}

struct Throughput {
    wall: Duration,
    bytes: usize,
    cpu: Vec<Duration>,
}

impl Throughput {
    fn row(&self) -> Vec<String> {
        let mut cells = vec![
            format!("{:.2}", self.wall.as_secs_f64()),
            format!("{:.2}", self.bytes as f64 / 1e6),
        ];
        cells.extend(
            self.cpu
                .iter()
                .map(|cpu| format!("{:.2}", cpu.as_secs_f64())),
        );
        cells
    }
}

fn bench_server(name: &str) -> TestServerBuilder {
    TestServer::builder()
        .name(name)
        .config(SHELL)
        .env("AMUX_LOG", LOG_LEVEL)
}

fn remote_pair() -> [TestServer; 2] {
    settled_pair(bench_server("a"), bench_server("b"))
}

fn server_pid(server: &TestServer) -> u32 {
    u32::try_from(server.pid().as_raw()).expect("a positive pid")
}

fn cell_text(screen: &vt100::Screen, row: u16, col: u16) -> &str {
    screen.cell(row, col).map_or("", vt100::Cell::contents)
}

fn at_prompt(screen: &vt100::Screen) -> bool {
    let (row, col) = screen.cursor_position();
    col >= 2
        && cell_text(screen, row, col - 2) == "$"
        && cell_text(screen, row, col - 1).trim().is_empty()
}

fn shows_output(screen: &vt100::Screen, output: &str) -> bool {
    let after_a_prompt = format!("$ {output}");
    screen
        .contents()
        .lines()
        .map(str::trim_end)
        .any(|line| line == output || line.ends_with(&after_a_prompt))
}

fn echo_latencies(terminal: &mut BenchTerminal, mut between_keystrokes: impl FnMut()) -> Latencies {
    terminal.wait_for_prompt();
    let mut samples = Vec::with_capacity(KEYSTROKES);
    for keystroke in 0..KEYSTROKES {
        if keystroke > 0 && keystroke % KEYSTROKES_PER_LINE == 0 {
            terminal.write(KILL_LINE);
            terminal.wait_until("the cleared line", at_prompt);
        }
        thread::sleep(KEYSTROKE_GAP);
        between_keystrokes();
        terminal.drain();
        let (row, col) = terminal.screen().cursor_position();
        let index = keystroke % LETTERS.len();
        let letter = &LETTERS[index..=index];
        let typed = terminal.write(letter.as_bytes());
        let echoed =
            terminal.wait_until("the echo", |screen| cell_text(screen, row, col) == letter);
        samples.push(echoed.saturating_duration_since(typed));
    }
    terminal.write(KILL_LINE);
    terminal.wait_until("the cleared line", at_prompt);
    Latencies::new(samples)
}

fn time_seq(terminal: &mut BenchTerminal, lines: u64, processes: &[u32]) -> Throughput {
    terminal.wait_for_prompt();
    let marker = format!("seq-done-{lines}");
    let cpu_before: Vec<Duration> = processes.iter().map(|&pid| cpu_time(pid)).collect();
    let received_before = terminal.received;
    let sent = terminal.write(format!("seq 1 {lines}; echo seq-done-$(({lines}+0))\r").as_bytes());
    let shown = terminal.wait_for_output(&marker);
    let cpu = processes
        .iter()
        .zip(cpu_before)
        .map(|(&pid, before)| cpu_time(pid).saturating_sub(before))
        .collect();
    Throughput {
        wall: shown.saturating_duration_since(sent),
        bytes: terminal.received - received_before,
        cpu,
    }
}

fn start_flood(terminal: &mut BenchTerminal, flood: &str, warmup: Duration) {
    terminal.wait_for_prompt();
    terminal.write(format!("{flood}\r").as_bytes());
    terminal.absorb_for(warmup);
}

fn interrupt(terminal: &mut BenchTerminal, tag: &str, run: usize) -> Duration {
    let marker = format!("{tag}-{run}");
    let sent = terminal.write(format!("\x03echo {tag}-$(({run}+0))\r").as_bytes());
    let shown = terminal.wait_for_output(&marker);
    terminal.wait_for_prompt();
    shown.saturating_duration_since(sent)
}

fn cpu_time(pid: u32) -> Duration {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("reading /proc/<pid>/stat");
    let after_name = &stat[stat.rfind(')').expect("a process name") + 1..];
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    let ticks: u64 = [11, 12]
        .iter()
        .map(|&field| fields[field].parse::<u64>().expect("a tick count"))
        .sum();
    Duration::from_secs_f64(ticks as f64 / clock_ticks_per_second())
}

fn clock_ticks_per_second() -> f64 {
    static TICKS: OnceLock<f64> = OnceLock::new();
    *TICKS.get_or_init(|| {
        let output = Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .expect("running getconf CLK_TCK");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .expect("a clock tick rate")
    })
}

fn millis(duration: Duration) -> String {
    format!("{:.2}", duration.as_secs_f64() * 1e3)
}

fn print_table(title: &str, columns: &[&str], rows: &[(impl AsRef<str>, Vec<String>)]) {
    let line = |label: &str, cells: &mut dyn Iterator<Item = &str>| {
        print!("{label:<LABEL_WIDTH$}");
        for cell in cells {
            print!("{cell:>COLUMN_WIDTH$}");
        }
        println!();
    };
    println!();
    line(title, &mut columns.iter().copied());
    for (label, cells) in rows {
        line(label.as_ref(), &mut cells.iter().map(String::as_str));
    }
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn echo_latency() {
    let server = bench_server("local").start();
    let mut rows = Vec::new();
    {
        let mut terminal = BenchTerminal::amux(&server, &["new", "-s", "echo"]);
        rows.push(("amux", echo_latencies(&mut terminal, || {}).row()));
    }
    if let Some(tmux) = Tmux::isolated() {
        let mut terminal = tmux.client();
        rows.push(("tmux", echo_latencies(&mut terminal, || {}).row()));
    }
    print_table(
        &format!("echo, {KEYSTROKES} keys, {COLS}x{ROWS}"),
        &["p50 ms", "p90 ms", "p99 ms"],
        &rows,
    );
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn seq_throughput() {
    let server = bench_server("local").start();
    let mut rows = Vec::new();
    {
        let mut terminal = BenchTerminal::amux(&server, &["new", "-s", "seq"]);
        let processes = [server_pid(&server), terminal.pid()];
        rows.push((
            "amux",
            time_seq(&mut terminal, LOCAL_SEQ_LINES, &processes).row(),
        ));
    }
    if let Some(tmux) = Tmux::isolated() {
        let mut terminal = tmux.client();
        terminal.wait_for_prompt();
        let processes = [tmux.server_pid(), terminal.pid()];
        rows.push((
            "tmux",
            time_seq(&mut terminal, LOCAL_SEQ_LINES, &processes).row(),
        ));
    }
    print_table(
        &format!("seq 1 {LOCAL_SEQ_LINES}, {COLS}x{ROWS}"),
        &["wall s", "MB out", "server cpu s", "client cpu s"],
        &rows,
    );
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn remote_echo_latency() {
    let [a, b] = remote_pair();
    b.create_session("echo");
    b.create_session("busy");
    a.create_session("busy");
    a.wait_for_ls("both sessions on b", |ls| ls.sessions("b").len() == 2);
    b.wait_for_ls("the session on a", |ls| ls.sessions("a") == ["busy"]);

    let mut echo = BenchTerminal::amux(&a, &["attach", "-t", "echo@b"]);
    let mut rows = vec![("idle".to_owned(), echo_latencies(&mut echo, || {}).row())];
    let mut stopped = 0;
    for flood in FLOODS {
        for (viewer, target, frames) in [(&a, "busy@b", "b to a"), (&b, "busy@a", "a to b")] {
            let mut flooder = BenchTerminal::amux(viewer, &["attach", "-t", target]);
            start_flood(&mut flooder, flood, FLOOD_WARMUP);
            let latencies = echo_latencies(&mut echo, || flooder.drain());
            interrupt(&mut flooder, "stopped", stopped);
            stopped += 1;
            rows.push((format!("{flood}, frames {frames}"), latencies.row()));
        }
    }
    print_table(
        &format!("echo@b from a, {KEYSTROKES} keys"),
        &["p50 ms", "p90 ms", "p99 ms"],
        &rows,
    );
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn remote_seq_throughput() {
    let [a, b] = remote_pair();
    b.create_session("seq");
    a.wait_for_ls("the session on b", |ls| ls.sessions("b") == ["seq"]);
    let mut terminal = BenchTerminal::amux(&a, &["attach", "-t", "seq@b"]);
    let processes = [server_pid(&a), server_pid(&b), terminal.pid()];
    let throughput = time_seq(&mut terminal, REMOTE_SEQ_LINES, &processes);
    print_table(
        &format!("seq 1 {REMOTE_SEQ_LINES}, seq@b from a"),
        &["wall s", "MB out", "a cpu s", "b cpu s", "client cpu s"],
        &[("amux", throughput.row())],
    );
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn remote_interrupt_behind_a_slow_reader() {
    let [a, b] = remote_pair();
    b.create_session("slow");
    a.wait_for_ls("the session on b", |ls| ls.sessions("b") == ["slow"]);
    let mut terminal =
        BenchTerminal::amux_with_read_pause(&a, &["attach", "-t", "slow@b"], SLOW_READ_PAUSE);
    let mut rows = Vec::new();
    let mut run = 0;
    for flood in FLOODS {
        let mut samples = Vec::with_capacity(INTERRUPTS);
        for _ in 0..INTERRUPTS {
            start_flood(&mut terminal, flood, SLOW_FLOOD);
            samples.push(interrupt(&mut terminal, "caught-up", run));
            run += 1;
        }
        let latencies = Latencies::new(samples);
        rows.push((
            flood,
            vec![
                millis(latencies.percentile(50)),
                millis(latencies.percentile(100)),
            ],
        ));
    }
    print_table(
        &format!(
            "^C to marker, reads every {} ms",
            SLOW_READ_PAUSE.as_millis()
        ),
        &["p50 ms", "max ms"],
        &rows,
    );
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn output_frame_codec() {
    let message = ServerMessage::Output(
        (0..CODEC_PAYLOAD_LEN)
            .map(|index| (index % 251) as u8)
            .collect(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("building a runtime");
    let (encode, decode) = runtime.block_on(async {
        let mut frame = Vec::new();
        let mut encode = Duration::ZERO;
        let mut decode = Duration::ZERO;
        for _ in 0..CODEC_ROUNDS {
            frame.clear();
            let started = Instant::now();
            protocol::write_message(&mut frame, black_box(&message))
                .await
                .expect("encoding the frame");
            encode += started.elapsed();
            let started = Instant::now();
            let decoded: Option<ServerMessage> = protocol::read_message(&mut frame.as_slice())
                .await
                .expect("decoding the frame");
            decode += started.elapsed();
            black_box(decoded);
        }
        (encode, decode)
    });
    let per_round =
        |total: Duration| format!("{:.1}", total.as_secs_f64() * 1e6 / CODEC_ROUNDS as f64);
    print_table(
        &format!(
            "{} KiB Output, {CODEC_ROUNDS} rounds",
            CODEC_PAYLOAD_LEN / 1024
        ),
        &["encode µs", "decode µs", "total µs"],
        &[(
            "protocol frame",
            vec![
                per_round(encode),
                per_round(decode),
                per_round(encode + decode),
            ],
        )],
    );
}
