mod common;

use std::fs;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{Listing, TestServer, DETACH, DISCOVERY_OFF, TIMEOUT};

const FAKE_OLD_SERVER_SOCKET: &str = "AMUX_TEST_FAKE_OLD_SERVER_SOCKET";

fn listed_sessions(server: &TestServer, command: &str) -> Vec<String> {
    let listing = Listing::parse(&server.run_ok(&[command]));
    assert_eq!(listing.servers.len(), 1, "{listing:?}");
    let local = &listing.servers[0];
    assert_eq!(local.name, server.name());
    assert_eq!(local.status, "(this server)");
    local
        .sessions
        .iter()
        .map(|session| session.name.clone())
        .collect()
}

#[test]
fn bare_amux_starts_the_server_creates_a_session_and_later_reattaches() {
    let server = TestServer::builder().prepare();

    let mut first = server.terminal(&[]);
    first.type_text("echo $((6*7))\r");
    first.wait_for_text("42");
    first.type_text(DETACH);
    first.wait_for_text("[detached (from session 0)]");
    assert!(first.wait_for_exit().success());
    assert_eq!(listed_sessions(&server, "ls"), ["0"]);

    let mut second = server.terminal(&[]);
    second.wait_for_text("42");
    second.type_text(DETACH);
    second.wait_for_text("[detached (from session 0)]");
    assert!(second.wait_for_exit().success());
    assert_eq!(listed_sessions(&server, "ls"), ["0"]);
}

#[test]
fn the_readme_commands_work() {
    let server = TestServer::builder().prepare();

    let mut created = server.terminal(&["new", "-s", "work"]);
    created.type_text("echo $((6*7))\r");
    created.wait_for_text("42");
    created.type_text(DETACH);
    created.wait_for_text("[detached (from session work)]");
    assert!(created.wait_for_exit().success());

    for alias in ["attach", "a", "attach-session"] {
        let mut attached = server.terminal(&[alias, "-t", "work"]);
        attached.wait_for_text("42");
        attached.type_text(DETACH);
        attached.wait_for_text("[detached (from session work)]");
        assert!(attached.wait_for_exit().success());
    }

    assert_eq!(listed_sessions(&server, "ls"), ["work"]);
    assert_eq!(listed_sessions(&server, "list-sessions"), ["work"]);

    server.run_ok(&["kill-server"]);
    assert!(!server.is_listening());
    assert!(!server.socket().exists());
}

#[test]
fn a_server_started_by_the_client_uses_the_client_config() {
    let server = TestServer::builder().prepare();
    let custom = server.root().join("custom.lua");
    fs::write(
        &custom,
        format!("amux.opt.name = \"custom\"\n{DISCOVERY_OFF}"),
    )
    .unwrap();

    let mut client = server.terminal(&["--config", custom.to_str().unwrap(), "new"]);
    server.wait_for_log("name=custom");
    client.type_text(DETACH);
    client.wait_for_text("[detached (from session 0)]");
    assert!(client.wait_for_exit().success());
}

#[test]
fn config_check_accepts_the_printed_defaults_and_names_a_bad_line() {
    let server = TestServer::builder().prepare();
    let init = server.config_path().display().to_string();
    assert_eq!(server.run_ok(&["config", "path"]), format!("{init}\n"));
    assert_eq!(
        server.run_ok(&["config", "check"]),
        format!("init     {init}\nservers  none\n")
    );

    let defaults = server.root().join("defaults.lua");
    fs::write(&defaults, server.run_ok(&["config", "defaults"])).unwrap();
    let defaults = defaults.to_str().unwrap();
    let checked = server
        .command()
        .env("AMUX_CONFIG", defaults)
        .args(["config", "check"])
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    assert_eq!(
        String::from_utf8_lossy(&checked.stdout),
        format!("init     {defaults}\nservers  none\n")
    );

    fs::write(server.config_path(), "amux.opt.bogus = 1\n").unwrap();
    let failed = server.run(&["config", "check"]);
    assert!(!failed.status.success());
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        stderr.contains(&format!("{init}:1: unknown option amux.opt.bogus")),
        "{stderr}"
    );
    assert!(!server.is_listening());
}

#[test]
fn attaching_without_a_server_says_so() {
    let server = TestServer::builder().prepare();

    let output = server.run(&["attach"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("no server running"), "{stderr}");
}

#[test]
fn an_old_server_gets_the_kill_server_hint_and_kill_server_stops_it() {
    let server = TestServer::builder().prepare();
    let mut old_server = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "fake_server_from_before_the_greeting",
            "--nocapture",
        ])
        .env(FAKE_OLD_SERVER_SOCKET, server.socket())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !server.is_listening() {
        assert!(Instant::now() < deadline, "the fake server did not start");
        thread::sleep(Duration::from_millis(10));
    }

    let output = server.run(&["ls"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("incompatible"), "{stderr}");
    assert!(stderr.contains("run `amux kill-server`"), "{stderr}");

    let output = server.run(&["kill-server"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("pid {}", old_server.id())),
        "{stderr}"
    );
    assert!(!server.is_listening());

    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = old_server.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if Instant::now() >= deadline {
            let _ = old_server.kill();
            let _ = old_server.wait();
            panic!("kill-server did not stop the old server");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fake_server_from_before_the_greeting() {
    let Some(socket) = std::env::var_os(FAKE_OLD_SERVER_SOCKET) else {
        return;
    };
    thread::spawn(|| {
        thread::sleep(TIMEOUT * 3);
        std::process::exit(0);
    });
    let listener = UnixListener::bind(socket).unwrap();
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut len = [0; 4];
        if stream.read_exact(&mut len).is_ok() {
            let mut request = vec![0; u32::from_be_bytes(len) as usize];
            let _ = stream.read_exact(&mut request);
        }
    }
}
