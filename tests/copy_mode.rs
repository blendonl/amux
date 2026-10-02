mod common;

use std::time::Duration;

use amux::protocol::{ServerMessage, SessionCommand, Split};
use common::{
    screen_column, screen_row, window_summary as window, TerminalClient, TestClient, TestServer,
    DETACH, SIZE,
};
use tokio::time::Instant;

const HIDDEN_FOR: Duration = Duration::from_millis(1500);

async fn session_with_prompt(server: &TestServer) -> TestClient {
    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    client.wait_for_text("$").await;
    client
}

async fn wait_for_row(client: &mut TestClient, row: u16, text: &str) {
    let what = format!("{text:?} on row {row}");
    client
        .wait_for_screen(&what, |screen| screen_row(screen, row).trim_end() == text)
        .await;
}

async fn enter_copy_mode(client: &mut TestClient) {
    client
        .command(SessionCommand::CopyMode { page_up: false })
        .await;
    client
        .wait_for_screen("the copy mode position", |screen| {
            screen_row(screen, 0).trim_end().ends_with(']')
        })
        .await;
}

async fn wait_until_live(client: &mut TestClient) {
    client
        .wait_for_screen("the position to go away", |screen| {
            !screen_row(screen, 0).contains('[')
        })
        .await;
}

async fn seq_100(client: &mut TestClient) {
    client.type_text("seq 1 100\r").await;
    wait_for_row(client, SIZE.rows - 2, "100").await;
    wait_for_row(client, SIZE.rows - 1, "$").await;
}

async fn stays_hidden(client: &mut TestClient, text: &str) {
    let deadline = Instant::now() + HIDDEN_FOR;
    while let Ok(message) = tokio::time::timeout_at(deadline, client.recv()).await {
        assert!(
            matches!(message, Some(ServerMessage::Output(_))),
            "{message:?}"
        );
        assert!(!client.contents().contains(text), "{}", client.contents());
    }
}

#[tokio::test]
async fn copy_mode_browses_the_history_and_q_returns_to_the_live_pane() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    seq_100(&mut client).await;

    enter_copy_mode(&mut client).await;
    wait_for_row(&mut client, 0, &format!("78{}[102/102]", " ".repeat(69))).await;
    assert_eq!(client.screen().cursor_position(), (23, 2));

    client.type_text("\x15").await;
    wait_for_row(&mut client, 0, &format!("66{}[90/102]", " ".repeat(70))).await;
    assert_eq!(client.screen().cursor_position(), (23, 2));

    client.type_text("g").await;
    wait_for_row(
        &mut client,
        0,
        &format!("$ seq 1 100{}[1/102]", " ".repeat(62)),
    )
    .await;
    wait_for_row(&mut client, 1, "1").await;
    assert_eq!(client.screen().cursor_position(), (0, 0));

    client.type_text("q").await;
    wait_for_row(&mut client, 0, "78").await;
    client.type_text("echo live-$((6*7))\r").await;
    client.wait_for_text("live-42").await;
}

#[tokio::test]
async fn output_that_arrives_while_browsing_shows_once_copy_mode_ends() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    client.type_text("sleep 1; echo late-$((6*7))\r").await;
    client.wait_for_text("late-$((6*7))").await;

    enter_copy_mode(&mut client).await;
    stays_hidden(&mut client, "late-42").await;

    client.type_text("q").await;
    client.wait_for_text("late-42").await;
    wait_until_live(&mut client).await;
}

#[tokio::test]
async fn a_lone_escape_leaves_copy_mode_after_the_escape_time() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    seq_100(&mut client).await;
    enter_copy_mode(&mut client).await;

    client.type_text("\x1b[A").await;
    client
        .wait_for_screen("the cursor one line up", |screen| {
            screen_row(screen, 0).trim_end().ends_with("[101/102]")
        })
        .await;

    client.type_text("\x1b").await;
    wait_until_live(&mut client).await;
    client.type_text("echo after-$((6*7))\r").await;
    client.wait_for_text("after-42").await;
}

#[tokio::test]
async fn copy_mode_stays_on_its_pane_across_a_window_switch() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    seq_100(&mut client).await;
    enter_copy_mode(&mut client).await;
    client.type_text("g").await;
    wait_for_row(&mut client, 1, "1").await;

    client.command(SessionCommand::NewWindow).await;
    server
        .wait_for_windows("s", &[window(0, 1), window(1, 1)])
        .await;
    client
        .wait_for_screen("the new window", |screen| screen_row(screen, 1).is_empty())
        .await;

    client.command(SessionCommand::SelectWindow(0)).await;
    wait_for_row(&mut client, 1, "1").await;
    assert!(screen_row(client.screen(), 0).ends_with("[1/102]"));

    client.type_text("q").await;
    wait_for_row(&mut client, 0, "78").await;
}

#[tokio::test]
async fn a_pane_that_exits_while_browsing_closes_cleanly() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    client
        .command(SessionCommand::SplitPane(Split::LeftRight))
        .await;
    client
        .wait_for_screen("a border column", |screen| {
            screen_column(screen, 40).starts_with('│')
        })
        .await;
    client.type_text("sleep 2; exit\r").await;
    client.wait_for_text("exit").await;
    client
        .command(SessionCommand::CopyMode { page_up: false })
        .await;
    client
        .wait_for_screen("the copy mode position", |screen| {
            screen_row(screen, 0).ends_with(']')
        })
        .await;

    server.wait_for_windows("s", &[window(0, 1)]).await;
    client
        .wait_for_screen("the border to go away", |screen| {
            !screen_column(screen, 40).starts_with('│')
        })
        .await;
    wait_until_live(&mut client).await;
    client.type_text("echo still-$((6*7))\r").await;
    client.wait_for_text("still-42").await;
}

#[test]
fn ctrl_b_bracket_enters_copy_mode_and_q_leaves_it() {
    let server = TestServer::start();
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_status("the session", |line| line.starts_with("[s@"));
    terminal.type_text("seq 1 50\r");
    terminal.wait_for("the seq output", |contents| contents.contains("\n50\n$"));

    terminal.type_text("\x02[");
    terminal.wait_for_screen("the copy mode position", |screen| {
        screen_row(screen, 0).ends_with("[52/52]")
    });
    terminal.type_text("k");
    terminal.wait_for_screen("the cursor one line up", |screen| {
        screen_row(screen, 0).ends_with("[51/52]")
    });

    terminal.type_text("q");
    terminal.wait_for_screen("the live pane", |screen| {
        !screen_row(screen, 0).contains('[')
    });
    terminal.type_text("echo back-$((6*7))\r");
    terminal.wait_for_text("back-42");
    exit(terminal);
}

fn exit(mut terminal: TerminalClient) {
    terminal.type_text(DETACH);
    terminal.wait_for_text("[detached (from session s)]");
    assert!(terminal.wait_for_exit().success());
}
