mod common;

use std::thread;
use std::time::{Duration, Instant};

use amux::cluster::{NoiseKey, TrustStore, TRUST_FILE};
use amux::config::ServerId;
use amux::protocol::{PublicKey, TrustedPeer};
use common::{linked, FakeLan, FakeNetwork, TestServer, TIMEOUT};

const POLL: Duration = Duration::from_millis(50);
const UNTRUSTED: &str = "refused a link: the server's key is not trusted";
const FORGOTTEN: &str = "the server was forgotten";

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

fn has_forgotten(trust: &TrustStore, server: &TestServer) -> bool {
    trust.is_forgotten(id_of(server), None)
}

fn tcp_address(server: &TestServer) -> String {
    format!("tcp://127.0.0.1:{}", server.wait_for_lan_port())
}

#[test]
fn servers_linked_over_exec_exchange_keys_and_relink_over_tcp() {
    let (lan_of_a, lan_of_b) = (FakeLan::new(), FakeLan::new());
    let a = TestServer::builder().name("a").lan(&lan_of_a).start();
    let b = TestServer::builder().name("b").lan(&lan_of_b).start();
    assert_eq!(b.lan_port(), None);

    a.run_ok(&["servers", "add", "b", &b.bridge_address()]);

    wait_for_trust(&a, "b's key", |trust| trusts_directly(trust, &b));
    wait_for_trust(&b, "a's key", |trust| trusts_directly(trust, &a));
    let address = tcp_address(&b);
    a.run_ok(&["servers", "remove", "b"]);
    a.wait_for_links("the exec link to close", |links| links.is_empty());

    a.run_ok(&["servers", "add", "b", &address]);

    let links = a.wait_for_links("a noise link to b", |links| {
        links.len() == 1 && links[0].state == "up" && links[0].transport == "noise"
    });
    assert_eq!(links[0].name, "b");
    assert!(links[0].dialed);
    let links = b.wait_for_links("the noise link from a", |links| {
        links.len() == 1 && links[0].transport == "noise"
    });
    assert_eq!(links[0].name, "a");
    assert!(!links[0].dialed);
    a.wait_for_ls("b over tcp", |ls| ls.is_online("b"));
}

#[test]
fn the_lan_listener_is_bound_only_while_a_key_is_trusted() {
    let lan = FakeLan::new();
    let [a, b] = linked([
        TestServer::builder().name("a").lan(&lan),
        TestServer::builder().name("b"),
    ]);
    a.wait_for_lan_port();
    assert_eq!(b.lan_port(), None);

    a.run_ok(&["servers", "forget", "b"]);

    a.wait_for_log("closed the LAN listener");
    assert_eq!(a.lan_port(), None);
    let c = TestServer::builder().name("c").start();
    a.run_ok(&["servers", "add", "c", &c.bridge_address()]);
    a.wait_for_lan_port();
    a.wait_for_log_count("listening for peers on the LAN", 2);
}

#[test]
fn a_server_with_an_unknown_key_is_refused_as_untrusted() {
    let lan = FakeLan::new();
    let [a, b] = linked([
        TestServer::builder().name("a").lan(&lan),
        TestServer::builder().name("b"),
    ]);
    wait_for_trust(&a, "b's key", |trust| trusts_directly(trust, &b));
    let c = TestServer::builder().name("c").start();

    c.run_ok(&["servers", "add", "a", &tcp_address(&a)]);

    a.wait_for_log(UNTRUSTED);
    c.wait_for_log(UNTRUSTED);
    let names: Vec<String> = a.links().into_iter().map(|link| link.name).collect();
    assert_eq!(names, ["b"]);
    assert!(c.links().is_empty());
    assert!(entry(&trust_of(&a), &c).is_none());
}

#[test]
fn keys_gossiped_through_a_shared_peer_let_two_servers_link_over_tcp() {
    let (lan_of_a, lan_of_c) = (FakeLan::new(), FakeLan::new());
    let b = TestServer::builder().name("b").start();
    let a = TestServer::builder().name("a").lan(&lan_of_a).start();
    let c = TestServer::builder().name("c").lan(&lan_of_c).start();

    a.run_ok(&["servers", "add", "b", &b.bridge_address()]);
    c.run_ok(&["servers", "add", "b", &b.bridge_address()]);

    wait_for_trust(&a, "c's key, introduced by b", |trust| {
        trusts_directly(trust, &b) && trusts_through(trust, &c, &b)
    });
    wait_for_trust(&c, "a's key, introduced by b", |trust| {
        trusts_directly(trust, &b) && trusts_through(trust, &a, &b)
    });
    assert!(a.links().iter().all(|link| link.name != "c"));

    a.run_ok(&["servers", "add", "c", &tcp_address(&c)]);

    a.wait_for_links("a noise link to c", |links| {
        links
            .iter()
            .any(|link| link.name == "c" && link.state == "up" && link.transport == "noise")
    });
    c.wait_for_links("the noise link from a", |links| {
        links
            .iter()
            .any(|link| link.name == "a" && link.transport == "noise")
    });
    let trust = trust_of(&a);
    assert!(trusts_through(&trust, &c, &b), "{trust:#?}");
}

#[test]
fn forgetting_a_server_spreads_and_drops_the_keys_it_introduced() {
    let networks = [(); 4].map(|()| FakeNetwork::new());
    let server =
        |name: &str, network: &FakeNetwork| TestServer::builder().name(name).ssh(network).start();
    let a = server("a", &networks[0]);
    let b = server("b", &networks[1]);
    let c = server("c", &networks[2]);
    let d = server("d", &networks[3]);
    networks[0].add_host("b", &b);
    networks[0].add_host("c", &c);
    networks[1].add_host("c", &c);
    networks[3].add_host("b", &b);

    a.run_ok(&["servers", "add", "b", "ssh://b"]);
    a.run_ok(&["servers", "add", "c", "ssh://c"]);
    d.run_ok(&["servers", "add", "b", "ssh://b"]);

    wait_for_trust(&a, "b and c directly, d through b", |trust| {
        trusts_directly(trust, &b) && trusts_directly(trust, &c) && trusts_through(trust, &d, &b)
    });
    wait_for_trust(&c, "a and b directly, d through b", |trust| {
        trusts_directly(trust, &a) && trusts_directly(trust, &b) && trusts_through(trust, &d, &b)
    });
    b.wait_for_links("b's link to c", |links| {
        links.iter().any(|link| link.name == "c" && link.dialed)
    });

    a.run_ok(&["servers", "forget", "b"]);

    c.wait_for_log(&format!("refused a link: {FORGOTTEN}"));
    b.wait_for_log(&format!("the peer refused the link: {FORGOTTEN}"));
    let trust = wait_for_trust(&a, "b forgotten and d dropped", |trust| {
        has_forgotten(trust, &b) && entry(trust, &d).is_none()
    });
    assert!(entry(&trust, &b).is_none(), "{trust:#?}");
    assert!(trusts_directly(&trust, &c), "{trust:#?}");
    let trust = wait_for_trust(&c, "b's tombstone and d dropped", |trust| {
        has_forgotten(trust, &b) && entry(trust, &b).is_none() && entry(trust, &d).is_none()
    });
    assert!(trusts_directly(&trust, &a), "{trust:#?}");
    assert!(a.links().iter().all(|link| link.name != "b"));
    assert!(c.links().iter().all(|link| link.name != "b"));
}
