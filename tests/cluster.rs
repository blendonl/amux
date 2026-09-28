mod common;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::thread;

use amux::protocol::{Version, Welcome, PROTOCOL_MAJOR};
use common::{linked, FakeNetwork, Listing, Resume, TestServer, DISCOVERY_OFF};
use nix::sys::signal::{kill, Signal};

fn is_stopped(ls: &Listing, server: &str) -> bool {
    ls.status(server)
        .is_some_and(|status| status.starts_with("offline (stopped), last seen"))
}

#[test]
fn a_server_sees_existing_and_new_sessions_on_its_peer() {
    let b = TestServer::builder().name("b").start();
    b.create_session("early");
    let a = TestServer::builder().name("a").peer(&b).start();

    a.wait_for_ls("the session b had before the link", |ls| {
        ls.is_online("b") && ls.sessions("b") == ["early"]
    });

    b.create_session("later");
    let ls = a.wait_for_ls("the session b created after the link", |ls| {
        ls.sessions("b") == ["early", "later"]
    });
    assert_eq!(ls.servers[0].name, "a");
    assert_eq!(ls.servers[0].status, "(this server)");
    assert_eq!(ls.server("b").unwrap().sessions[0].details, "1 window");
}

#[test]
fn kill_server_marks_the_peer_stopped_and_peers_do_not_restart_it() {
    let [a, mut b] = linked([
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    ]);
    b.create_session("work");
    a.wait_for_ls("b's session", |ls| {
        ls.is_online("b") && ls.sessions("b") == ["work"]
    });

    b.run_ok(&["kill-server"]);

    let ls = a.wait_for_ls("b to be stopped", |ls| is_stopped(ls, "b"));
    let work = &ls.server("b").unwrap().sessions[0];
    assert_eq!(work.name, "work");
    assert!(work.details.ends_with("stale"), "{ls:?}");
    a.wait_for_log("no server running");
    assert!(!b.is_listening());

    b.stop_process();
    b.start_process();
    a.wait_for_ls("b to come back", |ls| {
        ls.is_online("b") && ls.sessions("b").is_empty()
    });
}

#[test]
fn mutual_dialing_leaves_exactly_one_link_dialed_by_the_lower_id() {
    let [a, b] = linked([
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    ]);
    let (lower, higher) = if a.server_id() < b.server_id() {
        (&a, &b)
    } else {
        (&b, &a)
    };

    let links = lower.wait_for_links("one link dialed by the lower id", |links| {
        links.len() == 1 && links[0].dialed && links[0].state == "up"
    });
    assert_eq!(links[0].peer, higher.server_id());
    assert_eq!(links[0].name, higher.name());

    let links = higher.wait_for_links("one link accepted by the higher id", |links| {
        links.len() == 1 && !links[0].dialed
    });
    assert_eq!(links[0].peer, lower.server_id());
    assert_eq!(lower.links().len(), 1);
}

#[test]
fn a_second_server_with_a_taken_name_is_refused() {
    let hub = TestServer::builder().name("hub").start();
    let first = TestServer::builder().name("laptop").peer(&hub).start();
    hub.wait_for_ls("the first laptop", |ls| ls.is_online("laptop"));

    let second = TestServer::builder().name("laptop").peer(&hub).start();

    second.wait_for_log("the peer refused the link: the server name is already taken");
    hub.wait_for_log("refused a link: the server name is already taken");
    let links = hub.links();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].peer, first.server_id());
    assert!(second.links().is_empty());
}

#[test]
fn a_peer_that_stops_answering_goes_offline_and_comes_back() {
    let fast_pings = |name| {
        TestServer::builder()
            .name(name)
            .env("AMUX_PING_INTERVAL_MS", "100")
    };
    let [a, b] = linked([fast_pings("a"), fast_pings("b")]);
    a.wait_for_ls("b online", |ls| ls.is_online("b"));

    kill(b.pid(), Signal::SIGSTOP).unwrap();
    let resume = Resume(b.pid());
    let offline = a.wait_for_ls("b to go silent", |ls| {
        ls.status("b")
            .is_some_and(|status| status.starts_with("offline, last seen"))
    });
    drop(resume);

    assert!(!is_stopped(&offline, "b"));
    a.wait_for_ls("b to come back", |ls| ls.is_online("b"));
}

#[test]
fn servers_learn_addresses_from_their_peers() {
    let c = TestServer::builder().name("c").start();
    let b = TestServer::builder().name("b").peer(&c).start();
    b.wait_for_ls("c", |ls| ls.is_online("c"));

    let a = TestServer::builder().name("a").peer(&b).start();

    let ls = a.wait_for_ls("c, learned through b", |ls| {
        ls.is_online("b") && ls.is_online("c")
    });
    assert_eq!(ls.servers.len(), 3);
    a.wait_for_log("learned a peer address");
}

#[test]
fn servers_add_and_remove_edit_servers_lua_and_the_running_server() {
    let b = TestServer::builder().name("b").start();
    let a = TestServer::builder().name("a").start();
    let init = fs::read_to_string(a.config_path()).unwrap();

    a.run_ok(&["servers", "add", "b", &b.bridge_address()]);

    let servers = fs::read_to_string(a.servers_path()).unwrap();
    assert_eq!(
        servers,
        format!(
            "return {{\n  b = {{\n    address = {},\n  }},\n}}\n",
            common::lua(&b.bridge_address())
        )
    );
    a.wait_for_ls("b after servers add", |ls| ls.is_online("b"));
    let servers = a.wait_for_output(&["servers"], "b's latency", |servers| {
        servers
            .lines()
            .any(|line| line.starts_with("b ") && line.contains("online, "))
    });
    let version = Version::current().to_string();
    assert!(
        servers.lines().all(|line| line.contains(&version)),
        "{servers}"
    );
    assert!(servers.contains(&b.bridge_address()), "{servers}");

    let duplicate = a.run(&["servers", "add", "b", "ssh://elsewhere"]);
    assert!(!duplicate.status.success());
    let stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(stderr.contains("b is already in "), "{stderr}");

    a.run_ok(&["servers", "remove", "b"]);

    assert_eq!(fs::read_to_string(a.servers_path()).unwrap(), "return {}\n");
    assert_eq!(fs::read_to_string(a.config_path()).unwrap(), init);
    a.wait_for_ls("b to be forgotten", |ls| ls.server("b").is_none());
}

#[test]
fn a_server_set_in_init_lua_is_not_added_twice_or_removed_by_the_cli() {
    let b = TestServer::builder().name("b").start();
    let a = TestServer::builder().name("a").peer(&b).start();
    a.wait_for_ls("b from init.lua", |ls| ls.is_online("b"));

    let duplicate = a.run(&["servers", "add", "b", "ssh://elsewhere"]);
    assert!(!duplicate.status.success());
    let stderr = String::from_utf8_lossy(&duplicate.stderr);
    assert!(
        stderr.contains(&format!("b is already in {}", a.config_path().display())),
        "{stderr}"
    );

    let removed = a.run(&["servers", "remove", "b"]);
    assert!(!removed.status.success());
    let stderr = String::from_utf8_lossy(&removed.stderr);
    assert!(
        stderr.contains("b is set in ") && stderr.contains("remove it from "),
        "{stderr}"
    );
    assert!(!a.servers_path().exists());
    a.wait_for_ls("b to stay", |ls| ls.is_online("b"));
}

#[test]
fn servers_add_without_a_running_server_only_edits_the_config() {
    let a = TestServer::builder().name("a").prepare();

    a.run_ok(&["servers", "add", "b", "ssh://me@b.example:2222"]);

    assert_eq!(
        fs::read_to_string(a.servers_path()).unwrap(),
        "return {\n  b = {\n    address = \"ssh://me@b.example:2222\",\n  },\n}\n"
    );
    assert!(!a.is_listening());
    let invalid = a.run(&["servers", "add", "c", "ftp://c"]);
    assert!(!invalid.status.success());
}

#[test]
fn the_server_merges_servers_lua_before_init_lua_runs() {
    let a = TestServer::builder()
        .name("a")
        .config(
            "assert(amux.opt.servers.nowhere.address == 'exec:false')\n\
             amux.opt.servers.elsewhere = { address = 'exec:true' }",
        )
        .prepare();
    fs::create_dir_all(a.servers_path().parent().unwrap()).unwrap();
    fs::write(
        a.servers_path(),
        "return { nowhere = { address = \"exec:false\" } }\n",
    )
    .unwrap();
    let mut a = a;
    a.start_process();

    let servers = a.run_ok(&["servers"]);
    assert!(lists_server(&servers, "nowhere"), "{servers}");
    assert!(lists_server(&servers, "elsewhere"), "{servers}");
}

#[test]
fn a_configured_server_that_was_never_reached_is_listed_as_never_seen() {
    let a = TestServer::builder()
        .name("a")
        .config("amux.opt.servers.nowhere = { address = \"exec:false\" }")
        .start();

    let servers = a.run_ok(&["servers"]);
    let nowhere = servers
        .lines()
        .find(|line| line.starts_with("nowhere "))
        .unwrap_or_else(|| panic!("{servers}"));
    assert!(nowhere.contains("offline, never seen"), "{servers}");
    assert!(nowhere.contains("unknown version"), "{servers}");
    assert!(nowhere.ends_with("exec:false"), "{servers}");
}

#[test]
fn a_peer_with_another_protocol_major_is_listed_as_incompatible() {
    let a = TestServer::builder().name("a").prepare();
    let fake_socket = a.root().join("future.sock");
    let listener = UnixListener::bind(&fake_socket).unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut len = [0; 4];
            if stream.read_exact(&mut len).is_err() {
                continue;
            }
            let mut greeting = vec![0; u32::from_be_bytes(len) as usize];
            let _ = stream.read_exact(&mut greeting);
            let welcome = postcard::to_stdvec(&Welcome {
                server_name: "future".into(),
                version: Version {
                    release: "9.9.9".into(),
                    major: PROTOCOL_MAJOR + 1,
                    minor: 0,
                },
            })
            .unwrap();
            let _ = stream.write_all(&(welcome.len() as u32).to_be_bytes());
            let _ = stream.write_all(&welcome);
        }
    });
    let address = format!(
        "exec:{}",
        shell_words::join([
            common::AMUX,
            "-S",
            fake_socket.to_str().unwrap(),
            "bridge",
            "--no-start"
        ])
    );
    fs::write(
        a.config_path(),
        format!(
            "amux.opt.name = \"a\"\n{DISCOVERY_OFF}amux.opt.servers.future = {{ address = {} }}\n",
            common::lua(&address)
        ),
    )
    .unwrap();
    let mut a = a;
    a.start_process();

    let servers = a.wait_for_output(&["servers"], "the incompatible peer", |servers| {
        servers.contains("incompatible, runs amux 9.9.9 (protocol")
    });
    assert!(
        servers.lines().any(|line| line.starts_with("future ")),
        "{servers}"
    );
}

#[test]
fn a_restarted_server_remembers_offline_peers_and_does_not_start_them() {
    let mut b = TestServer::builder().name("b").start();
    b.create_session("work");
    let mut a = TestServer::builder().name("a").peer(&b).start();
    a.wait_for_ls("b's session", |ls| ls.sessions("b") == ["work"]);
    b.run_ok(&["kill-server"]);
    b.stop_process();
    a.wait_for_ls("b to be stopped", |ls| is_stopped(ls, "b"));
    let failed_dials = a.log().matches("no server running").count();

    a.restart();

    let ls = Listing::parse(&a.run_ok(&["ls"]));
    assert!(is_stopped(&ls, "b"), "{ls:?}");
    assert_eq!(ls.sessions("b"), ["work"]);
    a.wait_for_log_count("no server running", failed_dials + 1);
    assert!(!b.is_listening());
}

#[test]
fn debug_drop_link_drops_the_link_and_it_comes_back() {
    let b = TestServer::builder().name("b").start();
    let a = TestServer::builder().name("a").peer(&b).start();
    let first = a.wait_for_links("the link to b", |links| links.len() == 1);
    let links_up = a.log().matches("link up").count();

    a.run_ok(&["debug", "drop-link", "b"]);

    a.wait_for_log("dropping the link on request");
    a.wait_for_log_count("link up", links_up + 1);
    let second = a.wait_for_links("the link to b again", |links| links.len() == 1);
    assert_eq!(first[0].peer, second[0].peer);
    let missing = a.run(&["debug", "drop-link", "nobody"]);
    assert!(!missing.status.success());
}

const FORGOTTEN: &str = "the server was forgotten";

fn lists_server(servers: &str, name: &str) -> bool {
    servers
        .lines()
        .any(|line| line.starts_with(&format!("{name} ")))
}

#[test]
fn an_ssh_peer_links_when_amux_is_only_in_its_cargo_bin() {
    let network = FakeNetwork::new();
    let b = TestServer::builder().name("b").start();
    network.add_host("b", &b);
    let a = TestServer::builder()
        .name("a")
        .ssh(&network)
        .config("amux.opt.servers.b = { address = \"ssh://me@b\" }")
        .start();

    a.wait_for_ls("b over ssh", |ls| ls.is_online("b"));

    let links = a.wait_for_links("the link to b", |links| {
        links.len() == 1 && links[0].state == "up"
    });
    assert_eq!(links[0].name, "b");
    assert_eq!(links[0].transport, "ssh");
    let servers = a.run_ok(&["servers"]);
    assert!(
        servers
            .lines()
            .any(|line| line.starts_with("b ") && line.ends_with("ssh://me@b")),
        "{servers}"
    );
}

#[test]
fn servers_forget_drops_a_gossiped_peer_and_keeps_refusing_it() {
    let c = TestServer::builder().name("c").start();
    let b = TestServer::builder().name("b").peer(&c).start();
    let a = TestServer::builder().name("a").peer(&b).start();
    a.wait_for_ls("c, learned through b", |ls| ls.is_online("c"));
    c.run_ok(&["servers", "add", "a", &a.bridge_address()]);

    a.run_ok(&["servers", "forget", &c.server_id()]);

    a.wait_for_log(&format!("refused a link: {FORGOTTEN}"));
    c.wait_for_log(&format!("the peer refused the link: {FORGOTTEN}"));
    let ls = a.wait_for_ls("c to be gone", |ls| ls.server("c").is_none());
    assert!(ls.is_online("b"), "{ls:?}");
    assert!(!lists_server(&a.run_ok(&["servers"]), "c"));
    assert!(a.links().iter().all(|link| link.name != "c"));
    let trust = fs::read_to_string(a.state_dir().join("trust.toml")).unwrap();
    assert!(trust.contains(&c.server_id()), "{trust}");
}

#[test]
fn servers_forget_removes_a_configured_peer_from_servers_lua() {
    let [a, b] = linked([
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    ]);
    a.wait_for_ls("b", |ls| ls.is_online("b"));

    let forgot = a.run(&["servers", "forget", "b"]);
    assert!(forgot.status.success());
    assert!(forgot.stderr.is_empty(), "{forgot:?}");

    assert_eq!(fs::read_to_string(a.servers_path()).unwrap(), "return {}\n");
    b.wait_for_log(&format!("the peer refused the link: {FORGOTTEN}"));
    a.wait_for_ls("b to be gone", |ls| ls.server("b").is_none());
    assert!(!lists_server(&a.run_ok(&["servers"]), "b"));

    let again = a.run(&["servers", "forget", "b"]);
    assert!(!again.status.success());
    let stderr = String::from_utf8_lossy(&again.stderr);
    assert!(stderr.contains("no server named b is known"), "{stderr}");
}

#[test]
fn servers_forget_succeeds_but_says_when_init_lua_still_sets_the_server() {
    let b = TestServer::builder().name("b").start();
    let a = TestServer::builder().name("a").peer(&b).start();
    a.wait_for_ls("b", |ls| ls.is_online("b"));
    let init = fs::read_to_string(a.config_path()).unwrap();

    let forgot = a.run(&["servers", "forget", "b"]);
    assert!(forgot.status.success(), "{forgot:?}");
    let stderr = String::from_utf8_lossy(&forgot.stderr);
    assert!(
        stderr.contains(&format!(
            "forgot b, but b is still set in {}; remove it there too",
            a.config_path().display()
        )),
        "{stderr}"
    );

    a.wait_for_ls("b to be gone", |ls| ls.server("b").is_none());
    assert_eq!(fs::read_to_string(a.config_path()).unwrap(), init);
    assert!(!a.servers_path().exists());
    let trust = fs::read_to_string(a.state_dir().join("trust.toml")).unwrap();
    assert!(trust.contains(&b.server_id()), "{trust}");
}
