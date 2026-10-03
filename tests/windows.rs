mod common;

use std::ops::Range;
use std::time::Duration;

use amux::protocol::{ClientMessage, Direction, ServerMessage, SessionCommand, Split};
use common::{
    screen_column, screen_region, window_summary as window, TestClient, TestServer, SIZE, TIMEOUT,
};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

const BORDER_COLUMN: u16 = 40;
const LEFT: Range<u16> = 0..BORDER_COLUMN;
const RIGHT: Range<u16> = BORDER_COLUMN + 1..SIZE.cols;

fn region(screen: &vt100::Screen, cols: Range<u16>) -> String {
    screen_region(screen, cols)
}

fn has_border_column(screen: &vt100::Screen) -> bool {
    screen_column(screen, BORDER_COLUMN) == "│".repeat(usize::from(SIZE.rows))
}

async fn session_with_prompt(server: &TestServer, name: &str) -> TestClient {
    let mut client = server.client().await;
    client.new_session(Some(name)).await;
    client.wait_for_text("$").await;
    client
}

async fn split_left_right(client: &mut TestClient) {
    client
        .command(SessionCommand::SplitPane(Split::LeftRight))
        .await;
    client
        .wait_for_screen("a border column", has_border_column)
        .await;
}

async fn wait_in(client: &mut TestClient, cols: Range<u16>, text: &str) {
    let what = format!("{text:?} in columns {cols:?}");
    client
        .wait_for_screen(&what, |screen| region(screen, cols.clone()).contains(text))
        .await;
}

async fn expect_exited(client: &mut TestClient) {
    assert_eq!(client.next_non_output().await, Some(ServerMessage::Exited));
}

#[tokio::test]
async fn a_split_window_shows_both_panes_with_a_border_column() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client.type_text("echo before-$((6*7))\r").await;
    client.wait_for_text("before-42").await;

    split_left_right(&mut client).await;
    client.type_text("echo right-$((6*7))\r").await;
    wait_in(&mut client, RIGHT, "right-42").await;

    client
        .command(SessionCommand::SelectPane(Direction::Left))
        .await;
    client.type_text("echo left-$((6*7))\r").await;
    wait_in(&mut client, LEFT, "left-42").await;

    let screen = client.screen();
    assert!(has_border_column(screen));
    assert!(region(screen, LEFT).contains("before-42"));
    assert!(!region(screen, LEFT).contains("right-42"));
    assert!(!region(screen, RIGHT).contains("left-42"));
    server.wait_for_windows("s", &[window(0, 2)]).await;
}

#[tokio::test]
async fn pane_sizes_follow_the_layout() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client
        .command(SessionCommand::SplitPane(Split::TopBottom))
        .await;
    client.type_text("stty size\r").await;
    client.wait_for_text("11 80").await;

    client
        .command(SessionCommand::SplitPane(Split::LeftRight))
        .await;
    client.type_text("stty size\r").await;
    client.wait_for_text("11 39").await;
    client.command(SessionCommand::NextPane).await;
    client.type_text("clear; stty size\r").await;
    client.wait_for_text("12 80").await;
}

#[tokio::test]
async fn a_pane_narrowed_through_a_wide_character_prints_at_its_new_edge() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client
        .type_text(
            "printf '\\033[2J\\033[1;40H\\344\\270\\255'; read line; \
             printf '\\033[1;40Hx\\033[3;1H'; echo edge-$((6*7))\r",
        )
        .await;
    client
        .wait_for_screen("a wide character across the split", |screen| {
            screen
                .cell(0, BORDER_COLUMN - 1)
                .is_some_and(|cell| cell.contents() == "中")
        })
        .await;

    split_left_right(&mut client).await;
    client
        .command(SessionCommand::SelectPane(Direction::Left))
        .await;
    client.type_text("\r").await;
    wait_in(&mut client, LEFT, "edge-42").await;

    let edge = client.screen().cell(0, BORDER_COLUMN - 1).unwrap();
    assert_eq!(edge.contents(), "x");
    server.wait_for_windows("s", &[window(0, 2)]).await;
}

#[tokio::test]
async fn windows_are_created_cycled_and_selected_by_number() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client.type_text("echo first-$((6*7))\r").await;
    client.wait_for_text("first-42").await;

    client.command(SessionCommand::NewWindow).await;
    client
        .wait_for_screen("a fresh window", |screen| {
            !screen.contents().contains("first-42") && screen.contents().contains('$')
        })
        .await;
    client.type_text("echo second-$((6*7))\r").await;
    client.wait_for_text("second-42").await;
    server
        .wait_for_windows("s", &[window(0, 1), window(1, 1)])
        .await;

    client.command(SessionCommand::SelectWindow(0)).await;
    client.wait_for_text("first-42").await;
    client.command(SessionCommand::NextWindow).await;
    client.wait_for_text("second-42").await;
    client.command(SessionCommand::NextWindow).await;
    client.wait_for_text("first-42").await;
    client.command(SessionCommand::PreviousWindow).await;
    client.wait_for_text("second-42").await;

    client.command(SessionCommand::KillWindow).await;
    client.wait_for_text("first-42").await;
    server.wait_for_windows("s", &[window(0, 1)]).await;

    client.command(SessionCommand::NewWindow).await;
    server
        .wait_for_windows("s", &[window(0, 1), window(1, 1)])
        .await;
    client.command(SessionCommand::SelectWindow(0)).await;
    client.command(SessionCommand::KillWindow).await;
    server.wait_for_windows("s", &[window(1, 1)]).await;
    client.command(SessionCommand::NewWindow).await;
    server
        .wait_for_windows("s", &[window(0, 1), window(1, 1)])
        .await;
}

#[tokio::test]
async fn an_exited_pane_leaves_the_layout_and_the_last_one_ends_the_session() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client.type_text("echo kept-$((6*7))\r").await;
    client.wait_for_text("kept-42").await;
    split_left_right(&mut client).await;

    client.type_text("exit\r").await;
    client
        .wait_for_screen("the split to close", |screen| {
            !has_border_column(screen) && screen.contents().contains("kept-42")
        })
        .await;
    server.wait_for_windows("s", &[window(0, 1)]).await;

    client.type_text("exit\r").await;
    expect_exited(&mut client).await;
    let deadline = std::time::Instant::now() + TIMEOUT;
    while server.windows_of("s").await.is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the session was never removed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn killing_the_last_pane_or_window_ends_the_session() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "panes").await;
    split_left_right(&mut client).await;
    client.command(SessionCommand::KillPane).await;
    client
        .wait_for_screen("the split to close", |screen| !has_border_column(screen))
        .await;
    client.command(SessionCommand::KillPane).await;
    expect_exited(&mut client).await;

    let mut client = session_with_prompt(&server, "windows").await;
    client.command(SessionCommand::KillWindow).await;
    expect_exited(&mut client).await;
}

#[tokio::test]
async fn targets_pick_the_window_and_pane_to_attach_to() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client.command(SessionCommand::NewWindow).await;
    split_left_right(&mut client).await;
    client.detach().await;

    let mut client = server.client().await;
    client.attach_to("s:1.0").await;
    client
        .wait_for_screen("the split window", has_border_column)
        .await;
    client.type_text("echo in-left-$((6*7))\r").await;
    wait_in(&mut client, LEFT, "in-left-42").await;
    client.detach().await;

    let mut client = server.client().await;
    client.attach_to("s:0").await;
    client
        .wait_for_screen("the unsplit window", |screen| {
            !has_border_column(screen) && screen.contents().contains('$')
        })
        .await;
    client.detach().await;

    for (target, error) in [
        ("s:5", "can't find window 5 in session s"),
        ("s:1.2", "can't find pane 2 in window 1 of session s"),
    ] {
        let mut client = server.client().await;
        client
            .send(ClientMessage::Attach {
                target: target.parse().unwrap(),
                size: SIZE,
            })
            .await;
        assert_eq!(client.expect_error().await, error);
    }
}

#[test]
fn kill_targets_a_pane_or_a_window() {
    let server = TestServer::start();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut client = session_with_prompt(&server, "s").await;
        client.command(SessionCommand::NewWindow).await;
        split_left_right(&mut client).await;
        client.detach().await;
        server
            .wait_for_windows("s", &[window(0, 1), window(1, 2)])
            .await;
    });

    server.run_ok(&["kill", "-t", "s:1.1"]);
    runtime.block_on(server.wait_for_windows("s", &[window(0, 1), window(1, 1)]));
    server.run_ok(&["kill", "-t", "s:0"]);
    runtime.block_on(server.wait_for_windows("s", &[window(1, 1)]));

    for (target, error) in [
        ("s:0", "can't find window 0 in session s"),
        ("s:1.3", "can't find pane 3 in window 1 of session s"),
    ] {
        let output = server.run(&["kill", "-t", target]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success());
        assert!(stderr.contains(error), "{stderr}");
    }
    let output = server.run(&["kill", "-t", "s:1", "--remove-worktree"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("can't name a window or pane"));

    server.run_ok(&["kill", "-t", "s:1.0"]);
    server.wait_for_ls("the session to end", |ls| {
        ls.servers.iter().all(|listed| listed.sessions.is_empty())
    });
}

async fn wait_for_mouse_mode(client: &mut TestClient, mode: MouseProtocolMode) {
    client
        .wait_for_screen(&format!("{mode:?} mouse reports in SGR"), |screen| {
            screen.mouse_protocol_mode() == mode
                && screen.mouse_protocol_encoding() == MouseProtocolEncoding::Sgr
        })
        .await;
}

#[tokio::test]
async fn a_click_focuses_a_pane_and_reaches_only_panes_that_asked_for_the_mouse() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    wait_for_mouse_mode(&mut client, MouseProtocolMode::ButtonMotion).await;
    split_left_right(&mut client).await;
    assert_eq!(
        client.screen().mouse_protocol_mode(),
        MouseProtocolMode::ButtonMotion
    );

    client
        .type_text("printf '\\033[?1000h'; echo mouse-$((6*7))\r")
        .await;
    wait_in(&mut client, RIGHT, "mouse-42").await;
    client.type_text("\x1b[<0;46;3M\x1b[<0;46;3m").await;
    wait_in(&mut client, RIGHT, "^[[M %#^[[M#%#").await;

    client.type_text("\x1b[<0;10;5M\x1b[<0;10;5m").await;
    client.type_text("echo focused-$((6*7))\r").await;
    wait_in(&mut client, LEFT, "focused-42").await;
    assert!(!client.contents().contains("^[[<"), "{}", client.contents());
    assert!(!region(client.screen(), LEFT).contains("^[["));
}

#[tokio::test]
async fn without_scrolling_only_split_windows_and_copy_mode_report_clicks() {
    let server = TestServer::builder()
        .config("amux.opt.mouse.scroll = false")
        .start();
    let mut client = session_with_prompt(&server, "s").await;
    assert_eq!(
        client.screen().mouse_protocol_mode(),
        MouseProtocolMode::None
    );

    client
        .command(SessionCommand::CopyMode { page_up: false })
        .await;
    wait_for_mouse_mode(&mut client, MouseProtocolMode::PressRelease).await;
    client.type_text("q").await;
    client
        .wait_for_screen("no mouse reports", |screen| {
            screen.mouse_protocol_mode() == MouseProtocolMode::None
        })
        .await;

    split_left_right(&mut client).await;
    wait_for_mouse_mode(&mut client, MouseProtocolMode::PressRelease).await;
    client.type_text("\x1b[<0;10;5M\x1b[<0;10;5m").await;
    client.type_text("seq 1 50; echo seq-$((6*7))\r").await;
    wait_in(&mut client, LEFT, "seq-42").await;
    client.type_text("\x1b[<64;10;5M").await;
    client.type_text("echo live-$((6*7))\r").await;
    wait_in(&mut client, LEFT, "live-42").await;
}

#[tokio::test]
async fn a_lone_escape_reaches_the_pane() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server, "s").await;
    client.type_text("\x1b").await;
    client.wait_for_text("^[").await;
    client.type_text("\x1b[").await;
    client.wait_for_text("^[^[[").await;
}
