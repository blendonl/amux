mod common;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
use std::thread;

use amux::protocol::{Version, Welcome, PROTOCOL_MAJOR};
use common::{linked, Listing, Resume, TestServer};
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
fn servers_add_and_remove_edit_the_config_and_the_running_server() {
    let b = TestServer::builder().name("b").start();
    let a = TestServer::builder().name("a").start();

    a.run_ok(&["servers", "add", "b", &b.bridge_address()]);

    let config = fs::read_to_string(a.config_path()).unwrap();
    assert!(config.contains("[servers.b]"), "{config}");
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

    a.run_ok(&["servers", "remove", "b"]);

    let config = fs::read_to_string(a.config_path()).unwrap();
    assert!(!config.contains("servers.b"), "{config}");
    a.wait_for_ls("b to be forgotten", |ls| ls.server("b").is_none());
}

#[test]
fn servers_add_without_a_running_server_only_edits_the_config() {
    let a = TestServer::builder().name("a").prepare();

    a.run_ok(&["servers", "add", "b", "ssh://me@b.example:2222"]);

    let config = fs::read_to_string(a.config_path()).unwrap();
    assert!(
        config.contains("[servers.b]\naddress = \"ssh://me@b.example:2222\"\n"),
        "{config}"
    );
    assert!(!a.is_listening());
    let invalid = a.run(&["servers", "add", "c", "ftp://c"]);
    assert!(!invalid.status.success());
}

#[test]
fn a_configured_server_that_was_never_reached_is_listed_as_never_seen() {
    let a = TestServer::builder()
        .name("a")
        .config("[servers.nowhere]\naddress = \"exec:false\"")
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
        format!("name = \"a\"\n[servers.future]\naddress = {address:?}\n"),
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
