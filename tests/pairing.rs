mod common;

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::process::{Child, ChildStderr, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use amux::cluster::{NoiseKey, TrustStore, NOISE_KEY_FILE, TRUST_FILE};
use amux::identity::ServerId;
use amux::protocol::{PublicKey, TrustedPeer};
use common::{FakeLan, FakeNetwork, TestServer, TIMEOUT};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

const POLL: Duration = Duration::from_millis(50);
const FORGOTTEN: &str = "the peer refused the link: the server was forgotten";

struct PairingHost {
    child: Child,
    stderr: Option<ChildStderr>,
    lines: mpsc::Receiver<String>,
    printed: Vec<String>,
}

impl PairingHost {
    fn start(server: &TestServer, args: &[&str]) -> Self {
        let mut child = server
            .command()
            .arg("pair")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("running amux pair");
        let stdout = child.stdout.take().expect("the stdout of amux pair");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            stderr: child.stderr.take(),
            child,
            lines,
            printed: Vec::new(),
        }
    }

    fn wait_for_line(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(line) = self.printed.iter().find(|line| done(line)) {
                return line.clone();
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(remaining) {
                Ok(line) => self.printed.push(line),
                Err(_) => panic!(
                    "timed out waiting for {what}; `amux pair` printed:\n{}",
                    self.printed.join("\n")
                ),
            }
        }
    }

    fn code(&mut self) -> String {
        let line = self.wait_for_line("the pairing code", |line| line.starts_with("pairing code "));
        line.split_whitespace()
            .nth(2)
            .expect("a code after `pairing code`")
            .trim_end_matches(',')
            .to_owned()
    }

    fn interrupt(&self) {
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGINT)
            .expect("interrupting amux pair");
    }

    fn finish(mut self) -> (bool, String, String) {
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("polling amux pair") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "amux pair did not finish; it printed:\n{}",
                self.printed.join("\n")
            );
            thread::sleep(POLL);
        };
        while let Ok(line) = self.lines.recv_timeout(POLL) {
            self.printed.push(line);
        }
        let mut stderr = String::new();
        if let Some(mut pipe) = self.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        (status.success(), self.printed.join("\n"), stderr)
    }
}

impl Drop for PairingHost {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Machines {
    a: TestServer,
    b: TestServer,
    c: TestServer,
    _networks: [FakeNetwork; 3],
}

fn lan_server(name: &str, lan: &FakeLan) -> TestServer {
    TestServer::builder().name(name).lan(lan).start()
}

fn a_and_b_linked_over_ssh_and_c_alone(lan: &FakeLan) -> Machines {
    let networks = [(); 3].map(|()| FakeNetwork::new());
    let server = |name: &str, network: &FakeNetwork| {
        TestServer::builder()
            .name(name)
            .ssh(network)
            .lan(lan)
            .start()
    };
    let a = server("a", &networks[0]);
    let b = server("b", &networks[1]);
    let c = server("c", &networks[2]);
    networks[0].add_host("b", &b);
    a.run_ok(&["servers", "add", "b", "ssh://b"]);
    wait_for_trust(&a, "b's key", |trust| trusts_directly(trust, &b));
    wait_for_trust(&b, "a's key", |trust| trusts_directly(trust, &a));
    Machines {
        a,
        b,
        c,
        _networks: networks,
    }
}

fn id_of(server: &TestServer) -> ServerId {
    server.server_id().parse().expect("a server id")
}

fn key_of(server: &TestServer) -> PublicKey {
    NoiseKey::load_or_create(&server.state_dir())
        .expect("reading the noise key")
        .public()
}

fn trust_of(server: &TestServer) -> TrustStore {
    TrustStore::load(&server.state_dir().join(TRUST_FILE)).expect("reading the trust store")
}

fn wait_for_trust(
    server: &TestServer,
    what: &str,
    done: impl Fn(&TrustStore) -> bool,
) -> TrustStore {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let trust = trust_of(server);
        if done(&trust) {
            return trust;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; trust store:\n{trust:#?}\nserver log:\n{}",
            server.log()
        );
        thread::sleep(POLL);
    }
}

fn entry<'a>(trust: &'a TrustStore, server: &TestServer) -> Option<&'a TrustedPeer> {
    let id = id_of(server);
    trust.trusted.iter().find(|trusted| trusted.id == id)
}

fn trusts_directly(trust: &TrustStore, server: &TestServer) -> bool {
    entry(trust, server).is_some_and(|trusted| {
        trusted.direct && trusted.introduced_by.is_none() && trusted.key == key_of(server)
    })
}

fn trusts_through(trust: &TrustStore, server: &TestServer, introducer: &TestServer) -> bool {
    entry(trust, server).is_some_and(|trusted| {
        !trusted.direct
            && trusted.introduced_by == Some(id_of(introducer))
            && trusted.key == key_of(server)
    })
}

fn wait_for_noise_link(server: &TestServer, peer: &TestServer) {
    server.wait_for_links(&format!("a noise link to {}", peer.name()), |links| {
        links
            .iter()
            .any(|link| link.name == peer.name() && link.state == "up" && link.transport == "noise")
    });
}

fn paired_line(server: &TestServer) -> String {
    format!(
        "paired with {} ({}), key fingerprint {}",
        server.name(),
        id_of(server),
        key_of(server).fingerprint()
    )
}

fn pair(host: &TestServer, joiner: &TestServer, join_args: &[&str]) {
    let mut pairing = PairingHost::start(host, &[]);
    let code = pairing.code();
    let mut args = vec!["pair", code.as_str()];
    args.extend_from_slice(join_args);

    let joined = joiner.run_ok(&args);

    let (hosted, printed, stderr) = pairing.finish();
    assert!(hosted, "amux pair on {} failed: {stderr}", host.name());
    assert!(joined.contains(&paired_line(host)), "{joined}");
    assert!(printed.contains(&paired_line(joiner)), "{printed}");
}

fn wrong(code: &str) -> String {
    let (rest, last) = code.split_at(code.len() - 1);
    let digit = last.parse::<u8>().expect("a code ending in a digit");
    format!("{rest}{}", (digit + 1) % 10)
}

#[test]
fn pairing_links_two_servers_over_noise_and_trusts_each_directly() {
    let lan = FakeLan::new();
    let a = lan_server("a", &lan);
    let b = lan_server("b", &lan);
    let mut pairing = PairingHost::start(&a, &[]);
    let code = pairing.code();
    let warning = pairing.wait_for_line("the warning", |line| line.starts_with("pairing merges"));
    assert!(warning.contains("full access"), "{warning}");
    let port = a.wait_for_lan_port();
    pairing.wait_for_line("the fallback", |line| {
        line.starts_with(&format!("  amux pair {code} --host "))
            && line.ends_with(&format!(":{port}"))
    });
    let address_of_a = format!("lan://{}", id_of(&a));
    b.wait_for_output(
        &["discover"],
        "a with its pairing window open",
        |discover| {
            discover
                .lines()
                .any(|line| line.contains(&address_of_a) && line.ends_with("pairing open"))
        },
    );

    let joined = b.run_ok(&["pair", &code]);

    assert!(joined.starts_with("pairing merges"), "{joined}");
    assert!(joined.contains(&paired_line(&a)), "{joined}");
    let (hosted, printed, stderr) = pairing.finish();
    assert!(hosted, "{stderr}");
    assert!(printed.contains(&paired_line(&b)), "{printed}");
    wait_for_noise_link(&a, &b);
    wait_for_noise_link(&b, &a);
    wait_for_trust(&a, "b's key, seen first hand", |trust| {
        trusts_directly(trust, &b)
    });
    wait_for_trust(&b, "a's key, seen first hand", |trust| {
        trusts_directly(trust, &a)
    });
    b.wait_for_output(&["discover"], "a as a paired LAN server", |discover| {
        discover
            .lines()
            .any(|line| line.contains(&address_of_a) && line.ends_with("paired, linked"))
    });
}

#[test]
fn wrong_codes_fail_and_the_third_closes_the_window() {
    let lan = FakeLan::new();
    let a = lan_server("a", &lan);
    let b = lan_server("b", &lan);
    let mut pairing = PairingHost::start(&a, &[]);
    let code = pairing.code();
    let wrong = wrong(&code);

    for left in ["2 attempts left", "1 attempt left"] {
        let output = b.run(&["pair", &wrong]);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("wrong code"), "{stderr}");
        pairing.wait_for_line(left, |line| {
            line == format!("a wrong code was tried, {left}")
        });
    }
    let output = b.run(&["pair", &wrong]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("wrong code"), "{stderr}");
    let (hosted, _, stderr) = pairing.finish();
    assert!(!hosted);
    assert!(stderr.contains("the last of 3 attempts"), "{stderr}");

    let output = b.run(&["pair", &code]);

    assert!(!output.status.success());
    assert!(a.links().is_empty());
    assert!(entry(&trust_of(&a), &b).is_none());
    assert!(entry(&trust_of(&b), &a).is_none());
}

#[test]
fn a_joiner_without_lan_discovery_pairs_through_host() {
    let lan = FakeLan::new();
    let a = lan_server("a", &lan);
    let b = TestServer::builder().name("b").start();
    let mut pairing = PairingHost::start(&a, &[]);
    let code = pairing.code();
    let host = format!("127.0.0.1:{}", a.wait_for_lan_port());

    let joined = b.run_ok(&["pair", &code, "--host", &host]);

    assert!(joined.contains(&paired_line(&a)), "{joined}");
    let (hosted, printed, stderr) = pairing.finish();
    assert!(hosted, "{stderr}");
    assert!(printed.contains(&paired_line(&b)), "{printed}");
    wait_for_noise_link(&a, &b);
    wait_for_noise_link(&b, &a);
    wait_for_trust(&b, "a's key, seen first hand", |trust| {
        trusts_directly(trust, &a)
    });
}

#[test]
fn a_joiner_through_host_saves_the_host_and_links_to_it_again() {
    let lan = FakeLan::new();
    let a = lan_server("a", &lan);
    let b = TestServer::builder().name("b").start();
    let mut pairing = PairingHost::start(&a, &[]);
    let code = pairing.code();
    let host = format!("127.0.0.1:{}", a.wait_for_lan_port());
    let address = format!("tcp://{host}");

    let joined = b.run_ok(&["pair", &code, "--host", &host]);

    assert!(
        joined.contains(&format!(
            "saved a as a server at {address}, so this machine links to it again after the link drops\n{}",
            paired_line(&a)
        )),
        "{joined}"
    );
    let (hosted, _, stderr) = pairing.finish();
    assert!(hosted, "{stderr}");
    let servers = fs::read_to_string(b.servers_path()).unwrap();
    assert!(servers.contains(&common::lua(&address)), "{servers}");
    wait_for_noise_link(&b, &a);
    let links_up = b.log().matches("link up").count();

    b.run_ok(&["debug", "drop-link", "a"]);

    b.wait_for_log_count("link up", links_up + 1);
    wait_for_noise_link(&b, &a);
}

#[test]
fn verbose_pairing_prints_each_step_on_both_machines() {
    let lan = FakeLan::new();
    let a = lan_server("a", &lan);
    let b = TestServer::builder().name("b").start();
    let mut pairing = PairingHost::start(&a, &["--verbose"]);
    let code = pairing.code();
    let port = a.wait_for_lan_port();
    let host = format!("127.0.0.1:{port}");

    let joined = b.run_ok(&["pair", &code, "--host", &host, "--verbose"]);

    for step in [
        format!("] dialing {host}, the address --host {host} names"),
        format!("] connecting to {host} from 127.0.0.1"),
        format!("] connected to {host} in "),
        format!(
            "] finished the noise handshake, the other machine's key fingerprint is {}",
            key_of(&a).fingerprint()
        ),
        format!(
            "] the other machine knows the code, it is a ({})",
            id_of(&a)
        ),
        "] linking to a over the pairing connection".to_owned(),
    ] {
        assert!(joined.contains(&step), "missing {step:?} in:\n{joined}");
    }
    assert!(joined.contains(&paired_line(&a)), "{joined}");
    let (hosted, printed, stderr) = pairing.finish();
    assert!(hosted, "{stderr}");
    for step in [
        format!("] listening for the other machine on 0.0.0.0:{port}"),
        "] accepted a tcp connection from 127.0.0.1:".to_owned(),
        " opened a pairing connection".to_owned(),
        "] the other machine is b (".to_owned(),
    ] {
        assert!(printed.contains(&step), "missing {step:?} in:\n{printed}");
    }
    assert!(printed.contains(&paired_line(&b)), "{printed}");
}

#[test]
fn verbose_pairing_shows_the_connection_that_failed() {
    let b = TestServer::builder().name("b").start();
    let closed = TcpListener::bind("127.0.0.1:0").expect("binding a free port");
    let host = closed.local_addr().expect("the free port").to_string();
    drop(closed);

    let output = b.run(&["pair", "k7-4821-9930", "--host", &host, "--verbose"]);

    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains(&format!("] connecting to {host} from 127.0.0.1")),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("] connecting to {host} failed after ")),
        "{stdout}"
    );
    assert!(
        stderr.contains(&format!("connecting to {host} failed after ")),
        "{stderr}"
    );
}

#[test]
fn pairing_merges_two_clusters() {
    let lan = FakeLan::new();
    let Machines { a, b, c, .. } = &a_and_b_linked_over_ssh_and_c_alone(&lan);

    pair(a, c, &[]);

    wait_for_trust(b, "c's key, introduced by a", |trust| {
        trusts_through(trust, c, a)
    });
    wait_for_trust(c, "b's key, introduced by a", |trust| {
        trusts_through(trust, b, a)
    });
    wait_for_noise_link(b, c);
    wait_for_noise_link(c, b);
}

#[test]
fn a_forgotten_server_pairs_again_with_a_new_key_but_its_old_key_stays_out() {
    let lan = FakeLan::new();
    let machines = a_and_b_linked_over_ssh_and_c_alone(&lan);
    let Machines { a, b, c, .. } = &machines;
    pair(a, c, &[]);
    wait_for_noise_link(b, c);
    let old_key = key_of(c);
    let old_key_file =
        fs::read_to_string(c.state_dir().join(NOISE_KEY_FILE)).expect("reading c's key");

    a.run_ok(&["servers", "forget", "c"]);

    wait_for_trust(b, "c forgotten", |trust| {
        trust.is_key_forgotten(&old_key) && entry(trust, c).is_none()
    });
    b.wait_for_links("the link to c to close", |links| {
        links.iter().all(|link| link.name != "c")
    });

    pair(a, c, &["--new-key"]);

    assert_ne!(key_of(c), old_key);
    wait_for_noise_link(a, c);
    wait_for_trust(a, "c's new key, seen first hand", |trust| {
        trusts_directly(trust, c)
    });
    let trust = wait_for_trust(b, "c's new key, introduced by a", |trust| {
        trusts_through(trust, c, a)
    });
    assert!(trust.is_key_forgotten(&old_key), "{trust:#?}");
    assert!(!trust.is_forgotten(id_of(c), None), "{trust:#?}");
    wait_for_noise_link(b, c);
    wait_for_noise_link(c, b);

    let mut stale = TestServer::builder().name("stale").prepare();
    fs::create_dir_all(stale.state_dir()).expect("creating the state dir");
    fs::write(stale.state_dir().join(NOISE_KEY_FILE), old_key_file).expect("copying c's old key");
    let trusts_b = TrustStore {
        trusted: vec![TrustedPeer {
            id: id_of(b),
            name: "b".into(),
            key: key_of(b),
            introduced_by: None,
            direct: true,
        }],
        forgotten: Vec::new(),
    };
    trusts_b
        .save(&stale.state_dir().join(TRUST_FILE))
        .expect("trusting b");
    stale.start_process();
    assert_eq!(key_of(&stale), old_key);
    let address = format!("tcp://127.0.0.1:{}", b.wait_for_lan_port());
    stale.run_ok(&["servers", "add", "b", &address]);
    stale.wait_for_log(FORGOTTEN);
    assert!(stale.links().is_empty());
    assert!(b.links().iter().all(|link| link.name != "stale"));
}

#[test]
fn leaving_amux_pair_closes_the_window() {
    let lan = FakeLan::new();
    let a = lan_server("a", &lan);
    let b = lan_server("b", &lan);
    let mut pairing = PairingHost::start(&a, &[]);
    let code = pairing.code();
    a.wait_for_lan_port();
    let second = a.run(&["pair"]);
    assert!(!second.status.success());
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("already waiting"), "{stderr}");

    pairing.interrupt();

    let (hosted, _, _) = pairing.finish();
    assert!(!hosted);
    a.wait_for_log("closed the pairing window");
    a.wait_for_log("closed the LAN listener");
    assert_eq!(a.lan_port(), None);
    let output = b.run(&["pair", &code]);
    assert!(!output.status.success());
    assert!(entry(&trust_of(&a), &b).is_none());
}

#[test]
#[ignore = "needs multicast on a real network interface, which lo does not have"]
fn servers_pair_and_link_over_real_mdns() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .subsec_nanos();
    let service = format!("_amux-{nanos:08x}._tcp.local.");
    let a = TestServer::builder().name("a").mdns(&service).start();
    let b = TestServer::builder().name("b").mdns(&service).start();

    pair(&a, &b, &[]);

    wait_for_noise_link(&a, &b);
    wait_for_noise_link(&b, &a);
    b.wait_for_output(&["discover"], "a as a paired LAN server", |discover| {
        discover.contains("paired, linked")
    });
    for server in [&a, &b] {
        server.wait_for_log(&format!("browsing for servers over mDNS service={service}"));
    }
}
