mod common;

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::process;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::Duration;

use amux::cluster::{TrustStore, TRUST_FILE};
use common::{FakeTailnet, TestServer, TestServerBuilder};
use serde_json::{json, Map, Value};

const ME: i64 = 5998409928532361;
const SOMEONE_ELSE: i64 = 7001;
const TAGGED_DEVICES: i64 = 9001;
const UNTRUSTED: &str = "refused a link: the server's key is not trusted";
const REFUSED_BY_PEER: &str = "the peer refused the link: the server's key is not trusted";
const SETTLE: Duration = Duration::from_millis(1500);

static SUBNETS: AtomicU32 = AtomicU32::new(1);

struct Tailnet {
    prefix: String,
    port: u16,
}

#[derive(Clone)]
struct Machine {
    name: String,
    ip: String,
    number: u8,
    user: i64,
    os: &'static str,
    online: bool,
    tags: Vec<String>,
}

impl Tailnet {
    fn new() -> Self {
        let subnet = SUBNETS.fetch_add(1, Ordering::Relaxed);
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("finding a free port")
            .port();
        Self {
            prefix: format!("127.{}.{subnet}", 1 + process::id() % 250),
            port,
        }
    }

    fn machine(&self, number: u8, name: &str) -> Machine {
        Machine {
            name: name.to_owned(),
            ip: format!("{}.{number}", self.prefix),
            number,
            user: ME,
            os: "linux",
            online: true,
            tags: Vec::new(),
        }
    }

    fn view(&self, own: &Machine, peers: &[&Machine]) -> FakeTailnet {
        let view = FakeTailnet::new();
        view.set_status(&status(own, peers));
        for peer in peers {
            view.set_whois(&peer.ip, &peer.whois());
        }
        view
    }

    fn server(&self, machine: &Machine, view: &FakeTailnet) -> TestServerBuilder {
        TestServer::builder()
            .name(&machine.name)
            .tailscale(view)
            .tailscale_port(self.port)
    }

    fn address(&self, machine: &Machine) -> String {
        format!("tcp://{}:{}", machine.ip, self.port)
    }

    fn eavesdrop(&self, machine: &Machine) -> TcpListener {
        let listener = TcpListener::bind((machine.ip.as_str(), self.port))
            .expect("listening where a server would");
        listener
            .set_nonblocking(true)
            .expect("making the listener non-blocking");
        listener
    }
}

impl Machine {
    fn user(self, user: i64) -> Self {
        Self { user, ..self }
    }

    fn os(self, os: &'static str) -> Self {
        Self { os, ..self }
    }

    fn offline(self) -> Self {
        Self {
            online: false,
            ..self
        }
    }

    fn tagged(self, tag: &str) -> Self {
        Self {
            user: TAGGED_DEVICES,
            tags: vec![tag.to_owned()],
            ..self
        }
    }

    fn node(&self) -> String {
        format!("n{}CNTRL", self.name)
    }

    fn tags(&self) -> Value {
        match self.tags.is_empty() {
            true => Value::Null,
            false => json!(self.tags),
        }
    }

    fn status(&self) -> Value {
        json!({
            "ID": self.node(),
            "HostName": self.name,
            "DNSName": format!("{}.tail0.ts.net.", self.name),
            "OS": self.os,
            "UserID": self.user,
            "TailscaleIPs": [self.ip, format!("fd7a:115c:a1e0::{}", self.number)],
            "Online": self.online,
            "Tags": self.tags(),
        })
    }

    fn whois(&self) -> Value {
        json!({
            "Node": {
                "ID": 1000 + i64::from(self.number),
                "StableID": self.node(),
                "Name": format!("{}.tail0.ts.net.", self.name),
                "User": self.user,
                "Tags": self.tags(),
                "Addresses": [format!("{}/32", self.ip)],
            },
            "UserProfile": {"ID": self.user, "LoginName": "someone@example.com"},
            "CapMap": {},
        })
    }
}

fn status(own: &Machine, peers: &[&Machine]) -> Value {
    let peers: Map<String, Value> = peers
        .iter()
        .map(|peer| (format!("nodekey:{}", peer.number), peer.status()))
        .collect();
    json!({
        "BackendState": "Running",
        "Self": own.status(),
        "Peer": peers,
    })
}

fn servers_line(server: &TestServer, name: &str) -> Option<String> {
    server
        .run_ok(&["servers"])
        .lines()
        .find(|line| line.starts_with(&format!("{name} ")))
        .map(str::to_owned)
}

fn wait_for_servers_line(
    server: &TestServer,
    name: &str,
    what: &str,
    done: impl Fn(&str) -> bool,
) -> String {
    let prefix = format!("{name} ");
    let servers = server.wait_for_output(&["servers"], what, |servers| {
        servers
            .lines()
            .any(|line| line.starts_with(&prefix) && done(line))
    });
    servers
        .lines()
        .find(|line| line.starts_with(&prefix))
        .expect("the line just matched")
        .to_owned()
}

fn wait_for_noise_link(server: &TestServer, peer: &str) {
    server.wait_for_links(&format!("a noise link to {peer}"), |links| {
        links
            .iter()
            .any(|link| link.name == peer && link.state == "up" && link.transport == "noise")
    });
}

fn wait_for_tailnet_link(server: &TestServer, net: &Tailnet, peer: &Machine) {
    wait_for_noise_link(server, &peer.name);
    let address = net.address(peer);
    let what = format!("{} online at {address}", peer.name);
    wait_for_servers_line(server, &peer.name, &what, |line| {
        line.contains("online") && line.contains(&address)
    });
}

fn trust_of(server: &TestServer) -> TrustStore {
    TrustStore::load(&server.state_dir().join(TRUST_FILE)).expect("reading the trust store")
}

fn trusts(server: &TestServer, peer: &TestServer) -> bool {
    let id = peer.server_id().parse().expect("a server id");
    trust_of(server)
        .trusted
        .iter()
        .any(|trusted| trusted.id == id && trusted.direct)
}

fn assert_never_dialed(listener: &TcpListener, machine: &Machine) {
    match listener.accept() {
        Err(err) if err.kind() == ErrorKind::WouldBlock => {}
        Ok((_, from)) => panic!("{} was dialed from {from}", machine.name),
        Err(err) => panic!("checking whether {} was dialed: {err}", machine.name),
    }
}

#[test]
fn two_servers_on_one_tailnet_link_on_their_own() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let laptop = net.machine(2, "laptop");
    let desk_view = net.view(&desk, &[&laptop]);
    let laptop_view = net.view(&laptop, &[&desk]);

    let a = net.server(&desk, &desk_view).start();
    let b = net.server(&laptop, &laptop_view).start();

    wait_for_tailnet_link(&a, &net, &laptop);
    wait_for_tailnet_link(&b, &net, &desk);
    assert!(trusts(&a, &b), "{:#?}", trust_of(&a));
    assert!(trusts(&b, &a), "{:#?}", trust_of(&b));
    a.wait_for_log(&format!(
        "listening for peers on the tailnet address={}",
        desk.ip
    ));

    let discover = a.wait_for_output(&["discover"], "laptop linked", |discover| {
        discover.contains("linked")
    });
    let tailscale = discover.lines().next().unwrap_or_default();
    assert_eq!(tailscale, "tailscale  running with 1 machine", "{discover}");
    let laptop_line = discover
        .lines()
        .find(|line| line.trim_start().starts_with("laptop "))
        .unwrap_or_else(|| panic!("{discover}"));
    assert!(laptop_line.contains(&net.address(&laptop)), "{discover}");
    assert!(laptop_line.ends_with("linked"), "{discover}");
}

#[test]
fn phones_strangers_and_sleeping_machines_are_never_dialed() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let laptop = net.machine(2, "laptop");
    let phone = net.machine(3, "phone").os("android");
    let stranger = net.machine(4, "stranger").user(SOMEONE_ELSE);
    let asleep = net.machine(5, "asleep").offline();
    let ignored = [&phone, &stranger, &asleep];
    let listeners = ignored.map(|machine| net.eavesdrop(machine));
    let desk_view = net.view(&desk, &[&laptop, &phone, &stranger, &asleep]);
    let laptop_view = net.view(&laptop, &[&desk]);

    let a = net.server(&desk, &desk_view).start();
    let _b = net.server(&laptop, &laptop_view).start();

    wait_for_noise_link(&a, "laptop");
    thread::sleep(SETTLE);
    for (listener, machine) in listeners.iter().zip(ignored) {
        assert_never_dialed(listener, machine);
    }
    let servers = a.run_ok(&["servers"]);
    for machine in ignored {
        assert!(
            servers
                .lines()
                .all(|line| !line.starts_with(&format!("{} ", machine.name))),
            "{servers}"
        );
    }
    let discover = a.run_ok(&["discover"]);
    assert!(
        discover.starts_with("tailscale  running with 1 machine\n"),
        "{discover}"
    );
    for machine in ignored {
        assert!(!discover.contains(&machine.ip), "{discover}");
    }
}

#[test]
fn a_machine_that_goes_offline_is_not_redialed() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let laptop = net.machine(2, "laptop");
    let desk_view = net.view(&desk, &[&laptop]);
    let laptop_view = net.view(&laptop, &[&desk]);
    let a = net.server(&desk, &desk_view).start();
    let b = net.server(&laptop, &laptop_view).start();
    wait_for_tailnet_link(&a, &net, &laptop);
    wait_for_tailnet_link(&b, &net, &desk);

    desk_view.set_status(&status(&desk, &[&laptop.clone().offline()]));
    laptop_view.set_status(&status(&laptop, &[&desk.clone().offline()]));
    for server in [&a, &b] {
        server.wait_for_output(
            &["discover"],
            "the peer gone from the tailnet",
            |discover| discover.starts_with("tailscale  running with 0 machines\n"),
        );
    }
    let accepted =
        |server: &TestServer| server.log().matches("accepted a noise connection").count();
    let before = [accepted(&a), accepted(&b)];
    a.run_ok(&["debug", "drop-link", "laptop"]);

    let line = wait_for_servers_line(&a, "laptop", "laptop offline", |line| {
        line.contains("offline")
    });
    assert!(!line.contains("online"), "{line}");
    thread::sleep(SETTLE);
    assert!(a.links().is_empty(), "{:?}", a.links());
    assert!(b.links().is_empty(), "{:?}", b.links());
    assert_eq!([accepted(&a), accepted(&b)], before);
    let line = servers_line(&a, "laptop").expect("laptop is still listed");
    assert!(line.contains("offline"), "{line}");
    let discover = a.run_ok(&["discover"]);
    assert!(
        discover.contains(&format!("{}   absent", net.address(&laptop))),
        "{discover}"
    );
}

#[test]
fn a_failed_whois_refuses_the_link_and_trusts_nobody() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let laptop = net.machine(2, "laptop");
    let desk_view = net.view(&desk, &[&laptop]);
    let laptop_view = net.view(&laptop, &[&desk]);
    laptop_view.remove_whois(&desk.ip);

    let a = net.server(&desk, &desk_view).start();
    let b = net.server(&laptop, &laptop_view).start();

    b.wait_for_log(UNTRUSTED);
    a.wait_for_log(REFUSED_BY_PEER);
    b.wait_for_log("tailscale whois failed");
    a.wait_for_output(&["discover"], "laptop failing as untrusted", |discover| {
        discover.contains("failing: the server's key is not trusted")
    });
    assert!(a.links().is_empty(), "{:?}", a.links());
    assert!(b.links().is_empty(), "{:?}", b.links());
    assert!(trust_of(&a).trusted.is_empty(), "{:#?}", trust_of(&a));
    assert!(trust_of(&b).trusted.is_empty(), "{:#?}", trust_of(&b));
    assert!(servers_line(&a, "laptop").is_none());
    assert!(servers_line(&b, "desk").is_none());
}

#[test]
fn a_connection_from_this_machines_own_node_is_refused() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let laptop = net.machine(2, "laptop");
    let desk_view = net.view(&desk, &[&laptop]);
    let laptop_view = net.view(&laptop, &[&desk]);
    desk_view.set_whois(&laptop.ip, &desk.whois());

    let a = net.server(&desk, &desk_view).start();
    let b = net.server(&laptop, &laptop_view).start();

    a.wait_for_log("not vouching for this machine's own tailnet node");
    a.wait_for_log(UNTRUSTED);
    b.wait_for_log(REFUSED_BY_PEER);
    assert!(a.links().is_empty(), "{:?}", a.links());
    assert!(b.links().is_empty(), "{:?}", b.links());
    assert!(trust_of(&a).trusted.is_empty(), "{:#?}", trust_of(&a));
    assert!(trust_of(&b).trusted.is_empty(), "{:#?}", trust_of(&b));
}

#[test]
fn tagged_machines_link_when_their_tag_is_allowed() {
    let net = Tailnet::new();
    let build = net.machine(1, "build").tagged("tag:amux");
    let runner = net.machine(2, "runner").tagged("tag:amux");
    let build_view = net.view(&build, &[&runner]);
    let runner_view = net.view(&runner, &[&build]);

    let a = net
        .server(&build, &build_view)
        .tailscale_tags(&["tag:amux"])
        .start();
    let b = net
        .server(&runner, &runner_view)
        .tailscale_tags(&["tag:amux"])
        .start();

    wait_for_noise_link(&a, "runner");
    wait_for_noise_link(&b, "build");
    assert!(trusts(&a, &b), "{:#?}", trust_of(&a));
    assert!(trusts(&b, &a), "{:#?}", trust_of(&b));
}

#[test]
fn tagged_machines_without_an_allowed_tag_are_neither_dialed_nor_trusted() {
    let net = Tailnet::new();
    let build = net.machine(1, "build").tagged("tag:ci");
    let runner = net.machine(2, "runner").tagged("tag:ci");
    let build_view = net.view(&build, &[&runner]);
    let runner_view = net.view(&runner, &[&build]);

    let a = net
        .server(&build, &build_view)
        .tailscale_tags(&["tag:amux"])
        .start();
    let b = net
        .server(&runner, &runner_view)
        .tailscale_tags(&["tag:ci"])
        .start();

    a.wait_for_log(
        "not vouching for a tailnet node that has neither this machine's owner nor an allowed tag",
    );
    a.wait_for_log(UNTRUSTED);
    b.wait_for_log(REFUSED_BY_PEER);
    let discover = a.run_ok(&["discover"]);
    assert!(
        discover.starts_with("tailscale  running with 0 machines\nlan "),
        "{discover}"
    );
    assert!(a.links().is_empty(), "{:?}", a.links());
    assert!(b.links().is_empty(), "{:?}", b.links());
    assert!(trust_of(&a).trusted.is_empty(), "{:#?}", trust_of(&a));
    assert!(trust_of(&b).trusted.is_empty(), "{:#?}", trust_of(&b));
}

#[test]
fn the_tailnet_listener_follows_this_machines_address() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let moved = net.machine(9, "desk");
    let view = net.view(&desk, &[]);
    let stopped = |own: &Machine| {
        let mut stopped = status(own, &[]);
        stopped["BackendState"] = json!("Stopped");
        stopped
    };
    view.set_status(&stopped(&desk));
    let a = net.server(&desk, &view).start();
    a.wait_for_output(&["discover"], "tailscale not running", |discover| {
        discover.starts_with("tailscale  not running\n")
    });

    view.set_status(&status(&desk, &[]));
    a.wait_for_log(&format!(
        "listening for peers on the tailnet address={}:{}",
        desk.ip, net.port
    ));
    a.wait_for_output(&["discover"], "tailscale running", |discover| {
        discover.starts_with("tailscale  running with 0 machines\n")
    });

    view.set_status(&status(&moved, &[]));
    a.wait_for_log(&format!(
        "closed the tailnet listener address={}:{}",
        desk.ip, net.port
    ));
    a.wait_for_log(&format!(
        "listening for peers on the tailnet address={}:{}",
        moved.ip, net.port
    ));
    assert!(TcpStream::connect((moved.ip.as_str(), net.port)).is_ok());
    assert!(TcpStream::connect((desk.ip.as_str(), net.port)).is_err());

    view.set_status(&stopped(&moved));
    a.wait_for_log(&format!(
        "closed the tailnet listener address={}:{}",
        moved.ip, net.port
    ));
    a.wait_for_output(&["discover"], "tailscale not running", |discover| {
        discover.starts_with("tailscale  not running\n")
    });
    assert!(TcpStream::connect((moved.ip.as_str(), net.port)).is_err());
}

#[test]
fn discover_says_when_tailscale_is_not_installed() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let view = net.view(&desk, &[]);
    let missing = view.program().with_file_name("missing");

    let a = net
        .server(&desk, &view)
        .env("AMUX_TAILSCALE", &missing.display().to_string())
        .start();

    a.wait_for_output(&["discover"], "tailscale not installed", |discover| {
        discover.starts_with("tailscale  tailscale is not installed\n")
    });
    thread::sleep(SETTLE);
    assert_eq!(a.log().matches("tailscale is not installed").count(), 1);
}

#[test]
fn discover_reports_a_tailscale_status_it_cannot_read() {
    let net = Tailnet::new();
    let desk = net.machine(1, "desk");
    let view = net.view(&desk, &[]);
    view.set_status(&json!({"BackendState": "Running", "Self": "desk"}));

    let a = net.server(&desk, &view).start();

    let discover = a.wait_for_output(&["discover"], "an unreadable status", |discover| {
        discover.starts_with("tailscale  the output of `tailscale status` does not parse")
    });
    assert!(
        !a.log().contains("listening for peers on the tailnet"),
        "{discover}"
    );
}
