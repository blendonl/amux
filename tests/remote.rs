mod common;

use amux::identity::Incarnation;
use amux::protocol::{
    ClientMessage, Direction, NewSession, ServerMessage, ServerView, SessionCommand, Size, Split,
};
use common::{
    linked, screen_column, screen_region, screen_row, terminal_log, window_summary as window,
    Listing, Resume, TestClient, TestServer, DETACH, KITTY, PLAIN, SIZE,
};
use nix::sys::signal::{kill, Signal};

fn pair() -> [TestServer; 2] {
    let [a, b] = linked([
        TestServer::builder().name("a"),
        TestServer::builder().name("b"),
    ]);
    let (lower, higher) = if a.server_id() < b.server_id() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    lower.wait_for_links("the link dialed by the lower id", |links| {
        links.len() == 1 && links[0].dialed && links[0].state == "up"
    });
    higher.wait_for_links("the link accepted by the higher id", |links| {
        links.len() == 1 && !links[0].dialed && links[0].state == "up"
    });
    [a, b]
}

fn a_dials_b(env: &[(&str, &str)]) -> [TestServer; 2] {
    let with_env = |mut builder: common::TestServerBuilder| {
        for (key, value) in env {
            builder = builder.env(key, value);
        }
        builder
    };
    let b = with_env(TestServer::builder().name("b")).start();
    let a = with_env(TestServer::builder().name("a").peer(&b)).start();
    a.wait_for_links("the link to b", |links| {
        links.len() == 1 && links[0].state == "up"
    });
    [a, b]
}

fn blocking<T>(work: impl FnOnce() -> T) -> T {
    tokio::task::block_in_place(work)
}

async fn session_on(server: &TestServer, name: &str) {
    let mut client = server.client().await;
    client.new_session(Some(name)).await;
    client.wait_for_text("$").await;
    client.detach().await;
}

fn sessions_on(listing: &Listing, server: &str) -> Vec<String> {
    listing
        .sessions(server)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn sessions_in(servers: &[ServerView], server: &str) -> Vec<String> {
    servers
        .iter()
        .filter(|view| view.name == server)
        .flat_map(|view| view.sessions.iter().map(|session| session.name.clone()))
        .collect()
}

async fn cluster_of(client: &mut TestClient) -> Vec<ServerView> {
    client.send(ClientMessage::ListCluster).await;
    match client.next_non_output().await {
        Some(ServerMessage::Cluster(servers)) => servers,
        other => panic!("expected the cluster, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn new_on_a_peer_starts_the_session_there_in_its_home_with_the_client_locale() {
    let [a, b] = pair();
    let mut client = a.client().await;

    let request = NewSession {
        on: Some("b".into()),
        env: vec![
            ("LANG".into(), "C.UTF-8".into()),
            ("EDITOR".into(), "ed".into()),
        ],
        ..client.session_request(Some("s"))
    };
    let attached = client.create(request).await;

    assert_eq!(attached.server, "b");
    assert_eq!(attached.session, "s");
    client
        .type_text("echo \"at=$PWD lang=$LANG editor=[$EDITOR]\"\r")
        .await;
    client
        .wait_for_text(&format!("at={} lang=C.UTF-8 editor=[]", b.home().display()))
        .await;

    blocking(|| {
        b.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]);
        let ls = a.wait_for_ls("s seen from a", |ls| sessions_on(ls, "b") == ["s"]);
        assert!(ls.sessions("a").is_empty(), "{ls:?}");
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_attached_through_a_peer_types_and_reads_the_output() {
    let [a, b] = pair();
    let mut local = b.client().await;
    local.new_session(Some("s")).await;
    local.wait_for_text("$").await;
    blocking(|| a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]));

    let mut remote = a.client().await;
    let attached = remote.attach_to("s@b").await;
    assert_eq!(
        (attached.server.as_str(), attached.session.as_str()),
        ("b", "s")
    );
    remote.type_text("echo $((6*7))\r").await;

    remote.wait_for_text("42").await;
    local.wait_for_text("42").await;
    let unique = a.client().await.attach_to("s").await;
    assert_eq!(unique.server, "b");
}

#[test]
fn a_remote_session_can_be_renamed_and_then_killed_from_a_peer() {
    let [a, b] = pair();
    b.create_session("s");
    a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]);

    a.run_ok(&["rename", "-t", "s@b", "t"]);

    b.wait_for_ls("the rename on b", |ls| sessions_on(ls, "b") == ["t"]);
    a.wait_for_ls("the rename seen from a", |ls| sessions_on(ls, "b") == ["t"]);
    let invalid = a.run(&["rename", "-t", "t@b", "u@v"]);
    assert!(!invalid.status.success());

    a.run_ok(&["kill", "-t", "t@b"]);

    b.wait_for_ls("the kill on b", |ls| ls.sessions("b").is_empty());
    a.wait_for_ls("the kill seen from a", |ls| ls.sessions("b").is_empty());
    let missing = a.run(&["kill", "-t", "t@b"]);
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(!missing.status.success());
    assert!(stderr.contains("can't find session: t@b"), "{stderr}");
}

#[test]
fn an_ambiguous_session_name_fails_and_lists_every_match() {
    let [a, b] = pair();
    a.create_session("same");
    b.create_session("same");
    a.wait_for_ls("same on b", |ls| sessions_on(ls, "b") == ["same"]);

    let output = a.run(&["kill", "-t", "same"]);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("session same is on more than one server, pick one of: same@a, same@b"),
        "{stderr}"
    );
    let ls = Listing::parse(&a.run_ok(&["ls"]));
    assert_eq!(ls.sessions("a"), ["same"]);
    assert_eq!(ls.sessions("b"), ["same"]);
    let unknown = a.run(&["kill", "-t", "same@nowhere"]);
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown server: nowhere"));
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_link_mid_attach_reconnects_with_a_full_redraw() {
    let [a, b] = a_dials_b(&[]);
    session_on(&b, "s").await;
    blocking(|| a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]));
    let mut client = a.client().await;
    client.attach_to("s@b").await;
    client.type_text("echo marker-$((6*7))\r").await;
    client.wait_for_text("marker-42").await;

    blocking(|| a.run_ok(&["debug", "drop-link", "b"]));

    match client.next_non_output().await {
        Some(ServerMessage::Reconnecting { server }) => assert_eq!(server, "b"),
        other => panic!("expected to reconnect, got {other:?}"),
    }
    match client.recv().await {
        Some(ServerMessage::Attached(attached)) => assert_eq!(attached.session, "s"),
        other => panic!("expected to be attached again, got {other:?}"),
    }
    let frame = client.next_output().await;
    let mut fresh = vt100::Parser::new(SIZE.rows, SIZE.cols, 0);
    fresh.process(&frame);
    let contents = fresh.screen().contents();
    assert!(
        contents.contains("marker-42"),
        "first frame after reconnecting:\n{contents}"
    );

    client.type_text("echo again-$((6*7))\r").await;
    client.wait_for_text("again-42").await;
    client.detach().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_client_terminal_follows_the_client_to_a_peer_and_after_a_reconnect() {
    let [a, b] = a_dials_b(&[]);
    session_on(&b, "s").await;
    blocking(|| a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]));
    let mut client = a.client().await;
    client.new_session(Some("here")).await;
    client.send(ClientMessage::Terminal(KITTY)).await;
    blocking(|| a.wait_for_log(&terminal_log("here", KITTY)));

    client
        .send(ClientMessage::Switch("s@b".parse().unwrap()))
        .await;
    match client.next_non_output().await {
        Some(ServerMessage::Attached(attached)) => assert_eq!(attached.session, "s"),
        other => panic!("expected to switch to s@b, got {other:?}"),
    }
    blocking(|| b.wait_for_log(&terminal_log("s", KITTY)));

    let mut direct = b.client().await;
    direct.attach(Some("s")).await;
    direct.send(ClientMessage::Terminal(PLAIN)).await;
    blocking(|| b.wait_for_log(&terminal_log("s", PLAIN)));

    blocking(|| a.run_ok(&["debug", "drop-link", "b"]));
    match client.next_non_output().await {
        Some(ServerMessage::Reconnecting { server }) => assert_eq!(server, "b"),
        other => panic!("expected to reconnect, got {other:?}"),
    }
    match client.next_non_output().await {
        Some(ServerMessage::Attached(attached)) => assert_eq!(attached.session, "s"),
        other => panic!("expected to be attached again, got {other:?}"),
    }
    blocking(|| b.wait_for_log_count(&terminal_log("s", KITTY), 2));
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_server_on_the_host_exits_the_client_attached_through_a_peer() {
    let [a, b] = pair();
    session_on(&b, "s").await;
    blocking(|| a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]));
    let mut client = a.client().await;
    client.attach_to("s@b").await;
    client.wait_for_text("$").await;

    blocking(|| b.run_ok(&["kill-server"]));

    assert_eq!(client.next_non_output().await, Some(ServerMessage::Exited));
    assert_eq!(client.recv().await, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_attached_through_a_peer_opens_sessions_from_its_own_server() {
    let [a, b] = pair();
    session_on(&b, "there").await;
    blocking(|| a.wait_for_ls("there on b", |ls| sessions_on(ls, "b") == ["there"]));
    let mut client = a.client().await;
    client.attach_to("there@b").await;
    client.wait_for_text("$").await;
    let back = || ClientMessage::NewSession(NewSession::new(Some("back".into()), SIZE));

    client.send(back()).await;
    match client.next_non_output().await {
        Some(ServerMessage::Attached(attached)) => assert_eq!(
            (attached.server.as_str(), attached.session.as_str()),
            ("a", "back")
        ),
        other => panic!("expected to open back on a, got {other:?}"),
    }
    client.reset_screen();
    client.type_text("echo \"home=$HOME\"\r").await;
    client
        .wait_for_text(&format!("home={}", a.home().display()))
        .await;

    client
        .send(ClientMessage::Switch("there@b".parse().unwrap()))
        .await;
    assert_eq!(client.expect_attached().await.server, "b");
    client.send(back()).await;
    match client.next_non_output().await {
        Some(ServerMessage::Error(message)) => assert_eq!(message, "duplicate session: back"),
        other => panic!("expected the open to be refused, got {other:?}"),
    }
    client.reset_screen();
    client.type_text("echo \"home=$HOME\"\r").await;
    client
        .wait_for_text(&format!("home={}", b.home().display()))
        .await;
    client.detach().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_attached_client_lists_the_cluster_and_switches_between_servers() {
    let [a, b] = pair();
    session_on(&a, "here").await;
    session_on(&b, "there").await;
    blocking(|| a.wait_for_ls("there on b", |ls| sessions_on(ls, "b") == ["there"]));
    let mut client = a.client().await;
    assert_eq!(client.attach_to("here").await.server, "a");

    let servers = cluster_of(&mut client).await;
    assert_eq!(sessions_in(&servers, "b"), ["there"]);

    client
        .send(ClientMessage::Switch("there@b".parse().unwrap()))
        .await;
    match client.next_non_output().await {
        Some(ServerMessage::Attached(attached)) => {
            assert_eq!(
                (attached.server.as_str(), attached.session.as_str()),
                ("b", "there")
            );
        }
        other => panic!("expected to switch, got {other:?}"),
    }
    client.reset_screen();
    client.type_text("echo \"home=$HOME\"\r").await;
    client
        .wait_for_text(&format!("home={}", b.home().display()))
        .await;

    let servers = cluster_of(&mut client).await;
    assert_eq!(sessions_in(&servers, "a"), ["here"]);
    client
        .send(ClientMessage::Switch("nowhere@b".parse().unwrap()))
        .await;
    match client.next_non_output().await {
        Some(ServerMessage::Error(message)) => {
            assert_eq!(message, "can't find session: nowhere@b");
        }
        other => panic!("expected the switch to be refused, got {other:?}"),
    }

    client
        .send(ClientMessage::Switch("here@a".parse().unwrap()))
        .await;
    match client.next_non_output().await {
        Some(ServerMessage::Attached(attached)) => {
            assert_eq!(
                (attached.server.as_str(), attached.session.as_str()),
                ("a", "here")
            );
        }
        other => panic!("expected to switch back, got {other:?}"),
    }
    client.reset_screen();
    client.type_text("echo \"home=$HOME\"\r").await;
    client
        .wait_for_text(&format!("home={}", a.home().display()))
        .await;
    client.detach().await;
}

#[tokio::test]
async fn reattaching_needs_the_same_incarnation_and_session() {
    let server = TestServer::start();
    let mut first = server.client().await;
    let request = first.session_request(Some("s"));
    let attached = first.create(request).await;
    first.type_text("echo marker-$((6*7))\r").await;
    first.wait_for_text("marker-42").await;
    first.detach().await;

    let mut again = server.client().await;
    again
        .send(ClientMessage::Reattach {
            incarnation: attached.incarnation,
            session_id: attached.id,
            size: SIZE,
        })
        .await;
    assert_eq!(again.expect_attached().await, attached);
    let mut fresh = vt100::Parser::new(SIZE.rows, SIZE.cols, 0);
    fresh.process(&again.next_output().await);
    assert!(fresh.screen().contents().contains("marker-42"));

    let stale = [
        (Incarnation::random().unwrap(), attached.id),
        (
            attached.incarnation,
            amux::protocol::SessionId(attached.id.0 + 1),
        ),
    ];
    for (incarnation, session_id) in stale {
        let mut client = server.client().await;
        client
            .send(ClientMessage::Reattach {
                incarnation,
                session_id,
                size: SIZE,
            })
            .await;
        assert_eq!(client.recv().await, Some(ServerMessage::Exited));
    }
}

#[tokio::test]
async fn panes_take_the_client_locale_instead_of_the_servers() {
    let server = TestServer::builder()
        .env("LC_ALL", "C")
        .env("LANG", "server-lang")
        .env("COLORTERM", "server-colors")
        .start();
    let mut client = server.client().await;
    let request = NewSession {
        env: vec![
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_TIME".into(), "POSIX".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("EDITOR".into(), "ed".into()),
        ],
        ..client.session_request(None)
    };
    client.create(request).await;

    client
        .type_text("echo \"[$LANG|$LC_TIME|$COLORTERM|$LC_ALL|$EDITOR]\"\r")
        .await;

    client.wait_for_text("[C.UTF-8|POSIX|truecolor||]").await;
}

#[tokio::test]
async fn a_client_reporting_a_tiny_terminal_does_not_break_the_session() {
    let server = TestServer::start();
    let mut client = server.client().await;
    let request = NewSession {
        size: Size { rows: 0, cols: 0 },
        ..client.session_request(Some("tiny"))
    };
    client.create(request).await;

    client
        .type_text("printf '\\346\\274\\242\\346\\274\\242 wide\\n'\r")
        .await;
    client
        .send(ClientMessage::Resize(Size { rows: 1, cols: 1 }))
        .await;
    client.type_text("echo tiny-$((6*7))\r").await;
    client.send(ClientMessage::Resize(SIZE)).await;
    client.type_text("clear; stty size\r").await;

    client.wait_for_text("24 80").await;
    assert!(!server.log().contains("panicked"), "{}", server.log());
}

#[tokio::test]
async fn the_client_that_typed_last_sets_the_window_size() {
    let server = TestServer::start();
    let mut large = server.client().await;
    large.new_session(Some("s")).await;
    large.wait_for_text("$").await;
    let mut small = server.client().await;
    small
        .send(ClientMessage::Attach {
            target: "s".parse().unwrap(),
            size: Size { rows: 10, cols: 40 },
        })
        .await;
    small.expect_attached().await;

    small.type_text("stty size\r").await;
    small.wait_for_text("10 40").await;
    large.type_text("clear; stty size\r").await;
    large.wait_for_text("24 80").await;
}

#[test]
fn session_names_with_target_separators_are_refused() {
    let server = TestServer::start();

    for args in [["new", "-s", "a@b"], ["new", "-s", "a:b"]] {
        let output = server.run(&args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(stderr.contains("must not contain"), "{stderr}");
    }
    server.create_session("fine");
    let output = server.run(&["rename", "-t", "fine", "not:fine"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("must not contain ':'"));
}

#[test]
fn the_command_line_creates_and_attaches_to_sessions_on_a_peer() {
    let [a, b] = pair();

    let mut created = a.terminal_with_env(&["new", "-s", "s", "--on", "b"], &[("LANG", "C.UTF-8")]);
    created.type_text("echo \"$LANG $((6*7))\"\r");
    created.wait_for_text("C.UTF-8 42");
    created.type_text(DETACH);
    created.wait_for_text("[detached (from session s@b)]");
    assert!(created.wait_for_exit().success());
    b.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]);

    let mut attached = a.terminal(&["attach", "-t", "s@b"]);
    attached.wait_for_text("C.UTF-8 42");
    attached.type_text(DETACH);
    attached.wait_for_text("[detached (from session s@b)]");
    assert!(attached.wait_for_exit().success());
}

#[test]
fn the_client_shows_a_reconnecting_overlay_until_the_link_is_back() {
    let [a, b] = a_dials_b(&[("AMUX_PING_INTERVAL_MS", "100")]);
    b.create_session("s");
    a.wait_for_ls("s on b", |ls| sessions_on(ls, "b") == ["s"]);
    let mut terminal = a.terminal(&["attach", "-t", "s@b"]);
    terminal.type_text("echo marker-$((6*7))\r");
    terminal.wait_for_text("marker-42");

    kill(b.pid(), Signal::SIGSTOP).unwrap();
    let resume = Resume(b.pid());
    terminal.wait_for_text("reconnecting to b…");
    drop(resume);

    terminal.wait_for("the overlay to go away", |contents| {
        !contents.contains("reconnecting") && contents.contains("marker-42")
    });
    terminal.type_text("echo back-$((6*7))\r");
    terminal.wait_for_text("back-42");
    terminal.type_text(DETACH);
    terminal.wait_for_text("[detached (from session s@b)]");
    assert!(terminal.wait_for_exit().success());
}

fn split_at_column_40(screen: &vt100::Screen) -> bool {
    screen_column(screen, 40) == "│".repeat(usize::from(SIZE.rows))
}

async fn wait_for_cached_windows(
    server: &TestServer,
    host: &str,
    session: &str,
    expected: &[amux::protocol::WindowSummary],
) {
    let deadline = std::time::Instant::now() + common::TIMEOUT;
    loop {
        let servers = cluster_of(&mut server.client().await).await;
        let windows = servers
            .iter()
            .filter(|view| view.name == host)
            .flat_map(|view| &view.sessions)
            .find(|info| info.name == session)
            .map(|info| info.windows.clone());
        if windows.as_deref() == Some(expected) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {session}@{host} to show {expected:?}, got {windows:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn windows_listed(ls: &Listing, server: &str, session: &str, count: &str) -> bool {
    ls.server(server).is_some_and(|listed| {
        listed
            .sessions
            .iter()
            .any(|listed| listed.name == session && listed.details.starts_with(count))
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn window_and_pane_commands_drive_a_session_on_a_peer() {
    let [a, b] = pair();
    let mut client = a.client().await;
    let request = NewSession {
        on: Some("b".into()),
        ..client.session_request(Some("s"))
    };
    assert_eq!(client.create(request).await.server, "b");
    client.wait_for_text("$").await;

    client
        .command(SessionCommand::SplitPane(Split::LeftRight))
        .await;
    client
        .wait_for_screen("the split on b", split_at_column_40)
        .await;
    client.type_text("echo right-$((6*7))\r").await;
    client
        .wait_for_screen("output in the right pane", |screen| {
            screen_region(screen, 41..80).contains("right-42")
        })
        .await;
    client
        .command(SessionCommand::SelectPane(Direction::Left))
        .await;
    client.type_text("echo left-$((6*7))\r").await;
    client
        .wait_for_screen("output in the left pane", |screen| {
            screen_region(screen, 0..40).contains("left-42")
        })
        .await;

    client.command(SessionCommand::NewWindow).await;
    client
        .wait_for_screen("the new window", |screen| {
            !screen.contents().contains("left-42") && screen.contents().contains('$')
        })
        .await;
    b.wait_for_windows("s", &[window(0, 2), window(1, 1)]).await;
    blocking(|| {
        a.wait_for_ls("both windows seen from a", |ls| {
            windows_listed(ls, "b", "s", "2 windows")
        })
    });

    client.command(SessionCommand::NextWindow).await;
    client.wait_for_text("left-42").await;
    client.command(SessionCommand::NextPane).await;
    client.command(SessionCommand::KillPane).await;
    client
        .wait_for_screen("the split to close", |screen| {
            !split_at_column_40(screen) && screen.contents().contains("left-42")
        })
        .await;
    b.wait_for_windows("s", &[window(0, 1), window(1, 1)]).await;
    client.command(SessionCommand::SelectWindow(1)).await;
    client.command(SessionCommand::KillWindow).await;
    b.wait_for_windows("s", &[window(0, 1)]).await;
    client.detach().await;

    blocking(|| a.run_ok(&["kill", "-t", "s@b:0.0"]));
    blocking(|| a.wait_for_ls("s to end on b", |ls| ls.sessions("b").is_empty()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_yank_in_a_session_on_a_peer_reaches_the_local_client() {
    let [a, _b] = pair();
    let mut client = a.client().await;
    let request = NewSession {
        on: Some("b".into()),
        ..client.session_request(Some("s"))
    };
    assert_eq!(client.create(request).await.server, "b");
    client.wait_for_text("$").await;
    client.type_text("echo remote-$((6*7)) done\r").await;
    client.wait_for_text("remote-42 done").await;

    client
        .command(SessionCommand::CopyMode { page_up: false })
        .await;
    client
        .wait_for_screen("copy mode on b", |screen| {
            screen_row(screen, 0).ends_with(']')
        })
        .await;
    client.type_text("k0vEy").await;
    client
        .wait_until("the yank from b", |client| !client.clipboard().is_empty())
        .await;
    assert_eq!(client.clipboard(), ["remote-42"]);
    client
        .wait_for_screen("the live pane on b", |screen| {
            !screen_row(screen, 0).contains('[')
        })
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn window_and_pane_targets_reach_a_session_on_a_peer() {
    let [a, b] = pair();
    let mut local = b.client().await;
    local.new_session(Some("s")).await;
    local.wait_for_text("$").await;
    local.command(SessionCommand::NewWindow).await;
    local
        .command(SessionCommand::SplitPane(Split::TopBottom))
        .await;
    local.detach().await;
    wait_for_cached_windows(&a, "b", "s", &[window(0, 1), window(1, 2)]).await;

    let mut client = a.client().await;
    client.attach_to("s@b:1.0").await;
    client.type_text("echo top-$((6*7))\r").await;
    client
        .wait_for_screen("output in the top pane", |screen| {
            screen_region(screen, 0..80)
                .lines()
                .take(12)
                .any(|line| line.contains("top-42"))
        })
        .await;
    client.detach().await;

    let mut refused = a.client().await;
    refused
        .send(ClientMessage::Attach {
            target: "s@b:4".parse().unwrap(),
            size: SIZE,
        })
        .await;
    assert_eq!(
        refused.expect_error().await,
        "can't find window 4 in session s@b"
    );

    blocking(|| a.run_ok(&["kill", "-t", "s@b:1.1"]));
    b.wait_for_windows("s", &[window(0, 1), window(1, 1)]).await;
    blocking(|| a.run_ok(&["kill", "-t", "s@b:0"]));
    b.wait_for_windows("s", &[window(1, 1)]).await;
}
