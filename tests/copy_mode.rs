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

async fn yank_alpha_and_beta(client: &mut TestClient) {
    client.type_text("printf 'alpha\\nbeta\\n'\r").await;
    client
        .wait_for_screen("the printf output", |screen| {
            screen.contents().contains("\nalpha\nbeta\n$")
        })
        .await;
    enter_copy_mode(client).await;
    client.type_text("k0Vky").await;
    client
        .wait_until("the yanked lines", |client| !client.clipboard().is_empty())
        .await;
    wait_until_live(client).await;
}

async fn raw_cat(client: &mut TestClient, setup: &str) {
    client
        .type_text(&format!(
            "{setup}stty raw -echo; echo raw-$((6*7)); cat -v\r"
        ))
        .await;
    client.wait_for_text("raw-42").await;
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

#[tokio::test]
async fn a_line_selection_yanks_whole_lines_to_the_clipboard_and_the_paste_buffer() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    yank_alpha_and_beta(&mut client).await;
    assert_eq!(client.clipboard(), ["alpha\nbeta"]);

    raw_cat(&mut client, "").await;
    client.command(SessionCommand::PasteBuffer).await;
    client.wait_for_text("alpha^Mbeta").await;
    assert_eq!(client.clipboard(), ["alpha\nbeta"]);
}

#[tokio::test]
async fn the_paste_buffer_is_shared_by_every_session_and_bracketed_when_asked() {
    let server = TestServer::start();
    let mut yanking = session_with_prompt(&server).await;
    yank_alpha_and_beta(&mut yanking).await;

    let mut pasting = server.client().await;
    pasting.new_session(Some("t")).await;
    pasting.wait_for_text("$").await;
    raw_cat(&mut pasting, "printf '\\033[?2004h'; ").await;
    pasting.command(SessionCommand::PasteBuffer).await;
    pasting.wait_for_text("^[[200~alpha^Mbeta^[[201~").await;
    assert!(pasting.clipboard().is_empty());
}

#[tokio::test]
async fn pasting_an_empty_buffer_is_an_error() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    client.command(SessionCommand::PasteBuffer).await;
    assert_eq!(
        client.next_non_output().await,
        Some(ServerMessage::Error("the paste buffer is empty".into()))
    );
    client.type_text("echo still-$((6*7))\r").await;
    client.wait_for_text("still-42").await;
}

#[tokio::test]
async fn escape_clears_the_selection_before_it_leaves_copy_mode() {
    let server = TestServer::start();
    let mut client = session_with_prompt(&server).await;
    seq_100(&mut client).await;
    enter_copy_mode(&mut client).await;
    client.type_text("kvk").await;
    client
        .wait_for_screen("the selection", |screen| {
            screen.cell(22, 0).is_some_and(vt100::Cell::inverse)
        })
        .await;

    client.type_text("\x1b").await;
    client
        .wait_for_screen("the selection to clear", |screen| {
            !screen.cell(22, 0).is_some_and(vt100::Cell::inverse)
        })
        .await;
    assert!(screen_row(client.screen(), 0).ends_with("[100/102]"));

    client.type_text("\x1b").await;
    wait_until_live(&mut client).await;
    assert!(client.clipboard().is_empty());
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

#[test]
fn a_yank_reaches_the_terminal_clipboard_through_osc_52() {
    let server = TestServer::start();
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_status("the session", |line| line.starts_with("[s@"));
    terminal.wait_for_text("$");
    terminal.type_text("echo copy-$((6*7)) done\r");
    terminal.wait_for_text("copy-42 done");

    terminal.type_text("\x02[");
    terminal.wait_for_screen("the copy mode position", |screen| {
        screen_row(screen, 0).ends_with(']')
    });
    terminal.type_text("k0vEy");
    let copied = terminal.wait_for_clipboard("the OSC 52 copy", |texts| !texts.is_empty());
    assert_eq!(copied, ["copy-42"]);
    terminal.wait_for_screen("the live pane", |screen| {
        !screen_row(screen, 0).contains('[')
    });
    exit(terminal);
}

fn exit(mut terminal: TerminalClient) {
    terminal.type_text(DETACH);
    terminal.wait_for_text("[detached (from session s)]");
    assert!(terminal.wait_for_exit().success());
}
