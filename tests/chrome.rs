mod common;

use std::fs;
use std::future::Future;

use amux::protocol::{ClientMessage, ServerMessage, SessionCommand, SessionState, WindowSummary};
use common::git::{self, path_str};
use common::{
    settled_pair, window_summary, Listing, TerminalClient, TestClient, TestServer, DETACH, SIZE,
};

fn pair() -> [TestServer; 2] {
    settled_pair(
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    )
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building a runtime")
        .block_on(future)
}

fn sessions_on(listing: &Listing, server: &str) -> Vec<String> {
    listing
        .sessions(server)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn detach(terminal: &mut TerminalClient, label: &str) {
    terminal.type_text(DETACH);
    terminal.wait_for_text(&format!("[detached (from session {label})]"));
    assert!(terminal.wait_for_exit().success());
}

#[test]
fn the_tree_switches_one_client_between_local_and_remote_sessions() {
    let [a, b] = pair();
    a.create_session("here");
    b.create_session("there");
    a.wait_for_ls("there on b", |ls| sessions_on(ls, "b") == ["there"]);

    let mut terminal = a.terminal(&["attach", "-t", "here"]);
    terminal.wait_for_status("the local session", |line| {
        line.starts_with("[here@a] 0:sh")
    });

    terminal.type_text("\x02ss");
    terminal.wait_for_text("(this server)");
    terminal.wait_for_text("+ there  1 window");
    terminal.type_text("G\r");
    terminal.wait_for_status("the remote session", |line| {
        line.starts_with("[there@b] 0:sh")
    });
    terminal.type_text("echo \"home=$HOME\"\r");
    terminal.wait_for_text(&format!("home={}", b.home().display()));

    terminal.type_text("\x02ss");
    terminal.wait_for_text("(this server)");
    terminal.type_text("kkk\r");
    terminal.wait_for_status("the local session again", |line| {
        line.starts_with("[here@a] 0:sh")
    });
    terminal.type_text("echo \"home=$HOME\"\r");
    terminal.wait_for_text(&format!("home={}", a.home().display()));
    detach(&mut terminal, "here");
}

#[test]
fn prompts_rename_a_remote_session_and_its_window() {
    let [a, b] = pair();
    b.create_session("s");
    a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]);
    let mut terminal = a.terminal(&["attach", "-t", "s@b"]);
    terminal.wait_for_status("the remote session", |line| line.starts_with("[s@b] 0:sh"));

    terminal.type_text("\x02$");
    terminal.wait_for_status("the session prompt", |line| line == "rename session: s");
    terminal.type_text("\x15notes\r");
    terminal.wait_for_status("the renamed session", |line| {
        line.starts_with("[notes@b] 0:sh")
    });
    b.wait_for_ls("the rename on b", |ls| sessions_on(ls, "b") == ["notes"]);
    a.wait_for_ls("the rename seen from a", |ls| {
        sessions_on(ls, "b") == ["notes"]
    });

    terminal.type_text("\x02,");
    terminal.wait_for_status("the window prompt", |line| line == "rename window: sh");
    terminal.type_text("\x15editor\r");
    terminal.wait_for_status("the renamed window", |line| {
        line.starts_with("[notes@b] 0:editor")
    });
    let editor = WindowSummary {
        name: "editor".into(),
        ..window_summary(0, 1)
    };
    block_on(b.wait_for_windows("notes", &[editor]));

    terminal.type_text("\x02$");
    terminal.wait_for_status("the session prompt", |line| line == "rename session: notes");
    terminal.type_text("\x15bad:name\r");
    terminal.wait_for_status("the refused rename", |line| {
        line.contains("must not contain ':'")
    });

    terminal.type_text("\x02,");
    terminal.wait_for_status("the window prompt", |line| line == "rename window: editor");
    terminal.type_text("\x1b[<0;5;5M\x1b[<0;5;5m");
    terminal.type_text("\x1b");
    terminal.wait_for_status("the prompt to close", |line| {
        line.starts_with("[notes@b] 0:editor")
    });
    terminal.type_text("echo done-$((6*7))\r");
    terminal.wait_for_text("done-42");
    detach(&mut terminal, "notes@b");
}

#[test]
fn the_status_line_shows_the_host_its_latency_and_offline_peers() {
    let fast_pings =
        |builder: common::TestServerBuilder| builder.env("AMUX_PING_INTERVAL_MS", "100");
    let [a, b] = settled_pair(
        fast_pings(TestServer::builder().name("a")),
        fast_pings(TestServer::builder().name("b")),
    );
    a.create_session("here");
    b.create_session("there");
    a.wait_for_ls("there on b", |ls| sessions_on(ls, "b") == ["there"]);

    let mut remote = a.terminal(&["attach", "-t", "there@b"]);
    let line = remote.wait_for_status("the remote session and its latency", |line| {
        line.starts_with("[there@b] 0:sh") && line.ends_with(" ms")
    });
    assert!(!line.contains("offline"), "{line}");
    detach(&mut remote, "there@b");

    let mut local = a.terminal(&["attach", "-t", "here"]);
    let line = local.wait_for_status("the local session", |line| {
        line.starts_with("[here@a] 0:sh")
    });
    assert!(!line.ends_with(" ms"), "{line}");

    b.run_ok(&["kill-server"]);
    local.wait_for_status("b to be offline", |line| {
        line.starts_with("[here@a] 0:sh") && line.ends_with("b offline")
    });
    detach(&mut local, "here");
}

#[test]
fn a_failed_switch_keeps_the_client_attached_and_shows_the_error() {
    let server = TestServer::builder().name("solo").start();
    server.create_session("gone");
    server.create_session("here");
    let mut terminal = server.terminal(&["attach", "-t", "here"]);
    terminal.type_text("echo marker-$((6*7))\r");
    terminal.wait_for_text("marker-42");

    terminal.type_text("\x02ss");
    terminal.wait_for_text("+ gone  1 window");
    server.run_ok(&["kill", "-t", "gone"]);
    terminal.type_text("k\r");

    terminal.wait_for_status("the error", |line| line == "can't find session: gone@solo");
    terminal.wait_for("the session to be drawn again", |contents| {
        contents.contains("marker-42") && !contents.contains("(this server)")
    });
    terminal.type_text("echo still-$((6*7))\r");
    terminal.wait_for_text("still-42");
    terminal.wait_for_status("the error to go away", |line| {
        line.starts_with("[here@solo] 0:sh")
    });
    detach(&mut terminal, "here");
}

#[test]
fn the_search_keys_open_a_project_from_the_projects_dir_and_then_its_worktrees() {
    let server = TestServer::builder().name("solo").start();
    let notes = server.home().join("projects/notes");
    fs::create_dir_all(&notes).unwrap();
    git::git(&notes, &["init", "--quiet"]);
    git::commit(&notes, "initial");
    let notes = fs::canonicalize(&notes).unwrap();
    let feature = notes.with_file_name("notes-worktrees").join("feature-x");
    git::git(
        &notes,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "feature-x",
            path_str(&feature),
        ],
    );
    fs::create_dir_all(server.home().join("Projects/other/.git")).unwrap();
    server.create_session("here");
    let mut terminal = server.terminal(&["attach", "-t", "here"]);
    terminal.wait_for_status("the session", |line| line.starts_with("[here@solo]"));

    terminal.type_text("\x02sp");
    terminal.wait_for_text("> notes  ~/projects/notes");
    terminal.wait_for_text("  other  ~/Projects/other");
    terminal.type_text("nts\r");
    terminal.wait_for_status("the project session", |line| {
        line.starts_with("[notes/main@solo] 0:")
    });
    terminal.type_text("echo \"at=$PWD\"\r");
    terminal.wait_for_text(&format!("at={}", notes.display()));

    terminal.type_text("\x02sw");
    terminal.wait_for_text("notes worktree>");
    terminal.wait_for_text("feature-x  ~/projects/notes-worktrees/feature-x");
    terminal.type_text("feat\r");
    terminal.wait_for_status("the worktree session", |line| {
        line.starts_with("[notes/feature-x@solo] 0:")
    });
    terminal.type_text("echo \"at=$PWD\"\r");
    terminal.wait_for_text(&format!("at={}", feature.display()));

    terminal.type_text("\x02sp");
    terminal.wait_for_text("> notes  ~/projects/notes");
    terminal.type_text("notes\r");
    terminal.wait_for_status("the project session again", |line| {
        line.starts_with("[notes/main@solo] 0:")
    });
    assert_eq!(
        Listing::parse(&server.run_ok(&["ls"])).sessions("solo"),
        ["here", "notes/feature-x", "notes/main"]
    );
    detach(&mut terminal, "notes/main");
}

fn named_window(index: usize, name: &str) -> WindowSummary {
    WindowSummary {
        name: name.into(),
        ..window_summary(index, 1)
    }
}

async fn wait_for_state(client: &mut TestClient, what: &str, expected: SessionState) {
    client
        .wait_until(what, |client| client.session_state() == Some(&expected))
        .await;
}

async fn expect_error(client: &mut TestClient, expected: &str) {
    match client.next_non_output().await {
        Some(ServerMessage::Error(message)) => assert_eq!(message, expected),
        other => panic!("expected the error {expected:?}, got {other:?}"),
    }
}

#[tokio::test]
async fn an_attached_client_follows_the_session_state_and_survives_failed_commands() {
    let server = TestServer::builder().name("solo").start();
    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    let state = |name: &str, windows: &[(usize, &str)], active| SessionState {
        name: name.into(),
        windows: windows
            .iter()
            .map(|&(index, window)| named_window(index, window))
            .collect(),
        active,
    };
    wait_for_state(
        &mut client,
        "the state on attach",
        state("s", &[(0, "sh")], 0),
    )
    .await;
    client
        .wait_until("the cluster status on attach", |client| {
            client.cluster_statuses().last().is_some_and(|status| {
                (status.local.as_str(), status.host.as_str(), status.latency)
                    == ("solo", "solo", None)
            })
        })
        .await;

    client.command(SessionCommand::NewWindow).await;
    let two = [(0, "sh"), (1, "sh")];
    wait_for_state(&mut client, "the new window", state("s", &two, 1)).await;
    client
        .command(SessionCommand::RenameWindow("logs".into()))
        .await;
    let renamed = [(0, "sh"), (1, "logs")];
    wait_for_state(&mut client, "the renamed window", state("s", &renamed, 1)).await;
    client.command(SessionCommand::SelectWindow(0)).await;
    wait_for_state(&mut client, "window 0", state("s", &renamed, 0)).await;
    client
        .send(ClientMessage::RenameSession {
            target: "s".parse().unwrap(),
            name: "t".into(),
        })
        .await;
    wait_for_state(&mut client, "the renamed session", state("t", &renamed, 0)).await;

    client.command(SessionCommand::SelectWindow(7)).await;
    expect_error(&mut client, "can't find window 7").await;
    client
        .command(SessionCommand::RenameWindow(" ".into()))
        .await;
    expect_error(&mut client, "a window name can't be empty").await;
    client
        .send(ClientMessage::RenameSession {
            target: "t".parse().unwrap(),
            name: "a:b".into(),
        })
        .await;
    let refused = client.next_non_output().await;
    assert!(
        matches!(&refused, Some(ServerMessage::Error(message)) if message.contains("must not contain ':'")),
        "{refused:?}"
    );

    client.type_text("echo still-$((6*7))\r").await;
    client.wait_for_text("still-42").await;
    client.send(ClientMessage::Redraw).await;
    let mut fresh = vt100::Parser::new(SIZE.rows, SIZE.cols, 0);
    while !fresh.screen().contents().contains("still-42") {
        fresh.process(&client.next_output().await);
    }
    client.detach().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cluster_status_of_a_remote_session_comes_from_the_clients_own_server() {
    let [a, b] = tokio::task::block_in_place(|| {
        settled_pair(
            TestServer::builder()
                .name("a")
                .env("AMUX_PING_INTERVAL_MS", "100"),
            TestServer::builder()
                .name("b")
                .env("AMUX_PING_INTERVAL_MS", "100"),
        )
    });
    let mut client = a.client().await;
    let request = amux::protocol::NewSession {
        on: Some("b".into()),
        ..client.session_request(Some("s"))
    };
    client.create(request).await;
    client.wait_for_text("$").await;
    client
        .wait_until("the latency to b", |client| {
            client
                .cluster_statuses()
                .last()
                .is_some_and(|status| status.latency.is_some())
        })
        .await;

    for status in client.cluster_statuses() {
        assert_eq!(
            (status.local.as_str(), status.host.as_str()),
            ("a", "b"),
            "{:?}",
            client.cluster_statuses()
        );
        assert!(status.offline.is_empty(), "{status:?}");
    }
    let state = client.session_state().expect("the host's session state");
    assert_eq!(state.name, "s");
    tokio::task::block_in_place(|| b.run_ok(&["rename", "-t", "s", "t"]));
    client
        .wait_until("the rename from the host", |client| {
            client
                .session_state()
                .is_some_and(|state| state.name == "t")
        })
        .await;
    client.detach().await;
}
