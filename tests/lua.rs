mod common;

use std::fs::{self, File};
use std::io::Write;
use std::ops::Range;

use amux::protocol::SessionCommand;
use common::{screen_region, window_summary, Listing, TerminalClient, TestServer, SIZE};

const STATUS_ROW: u16 = SIZE.rows - 1;
const LEFT: Range<u16> = 0..40;
const RIGHT: Range<u16> = 41..SIZE.cols;

fn server_with(config: &str) -> TestServer {
    TestServer::builder().config(config).start()
}

fn new_session(server: &TestServer) -> TerminalClient {
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_text("$");
    terminal
}

fn wait_in(terminal: &mut TerminalClient, cols: Range<u16>, text: &str) {
    let what = format!("{text:?} in columns {cols:?}");
    terminal.wait_for_screen(&what, |screen| {
        screen_region(screen, cols.clone()).contains(text)
    });
}

#[test]
fn a_ctrl_a_prefix_opens_the_tree_and_detaches_while_ctrl_b_reaches_the_shell() {
    let server = server_with("amux.opt.prefix = \"C-a\"");
    let mut terminal = new_session(&server);
    assert!(!terminal.terminal_modes_unchanged());

    terminal.type_text("\x02");
    terminal.wait_for_text("^B");

    terminal.type_text("\x01ss");
    terminal.wait_for_text("(this server)");
    terminal.type_text("q");
    terminal.wait_for("the tree to close", |contents| {
        !contents.contains("(this server)")
    });

    terminal.type_text("\x01d");
    terminal.wait_for_text("[detached (from session s)]");
    assert!(terminal.wait_for_exit().success());
    assert!(terminal.terminal_modes_unchanged());
}

#[test]
fn a_root_binding_moves_the_focus_without_the_prefix() {
    let server =
        server_with("amux.keymap.set(\"root\", \"M-h\", amux.action.select_pane(\"left\"))");
    let mut terminal = new_session(&server);

    terminal.type_text("\x02%");
    terminal.type_text("echo right-$((6*7))\r");
    wait_in(&mut terminal, RIGHT, "right-42");

    terminal.type_text("\x1bh");
    terminal.type_text("echo left-$((6*7))\r");
    wait_in(&mut terminal, LEFT, "left-42");
    assert!(
        !terminal.contents().contains("^["),
        "{}",
        terminal.contents()
    );
}

#[test]
fn a_lua_binding_types_into_the_pane_and_shows_a_notice() {
    let server = server_with(
        "amux.keymap.set(\"prefix\", \"g\", function(ctx)\n\
           amux.send_keys(\"echo from-lua-$((6*7))\\r\")\n\
           amux.notify(\"sent to \" .. ctx.session)\n\
         end)",
    );
    let mut terminal = new_session(&server);

    terminal.type_text("\x02g");
    terminal.wait_for_status("the notice", |line| line == "sent to s");
    terminal.wait_for_text("from-lua-42");
}

#[test]
fn a_theme_colour_paints_the_status_line() {
    let server = server_with("amux.opt.theme.status = { fg = \"black\", bg = \"blue\" }");
    let mut terminal = new_session(&server);

    let blue = vt100::Color::Idx(4);
    terminal.wait_for_screen("a blue status line", |screen| {
        screen
            .cell(STATUS_ROW, SIZE.cols - 1)
            .is_some_and(|cell| cell.bgcolor() == blue)
    });
    for col in 0..SIZE.cols {
        let cell = terminal.screen().cell(STATUS_ROW, col).unwrap();
        assert_eq!(
            (cell.fgcolor(), cell.bgcolor()),
            (vt100::Color::Idx(0), blue),
            "column {col}"
        );
    }
}

#[test]
fn a_status_function_shows_its_text_on_the_right() {
    let server =
        server_with("amux.opt.status.right = function(ctx) return ' on ' .. ctx.server .. ' ' end");
    let mut terminal = new_session(&server);

    let tag = format!("[s@{}] 0:sh", server.name());
    let right = format!(" on {}", server.name());
    terminal.wait_for_status("the scripted right side", |line| {
        line.starts_with(&tag) && line.ends_with(&right)
    });
}

#[test]
fn without_the_status_bar_the_session_fills_the_terminal() {
    let server = server_with("amux.opt.status.enabled = false");
    let mut terminal = new_session(&server);

    terminal.type_text("stty size\r");
    terminal.wait_for_text(&format!("{} {}", SIZE.rows, SIZE.cols));
    assert!(
        !terminal.contents().contains("[s@"),
        "{}",
        terminal.contents()
    );
}

#[tokio::test]
async fn pane_and_window_settings_shape_new_panes_and_windows() {
    let server = server_with(
        "amux.opt.pane.term = \"tmux-256color\"\n\
         amux.opt.pane.env = { AMUX_GREETING = \"hello\" }\n\
         amux.opt.window.base_index = 1",
    );
    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    client.wait_for_text("$").await;

    client.type_text("echo \"[$TERM|$AMUX_GREETING]\"\r").await;
    client.wait_for_text("[tmux-256color|hello]").await;
    server.wait_for_windows("s", &[window_summary(1, 1)]).await;

    client.command(SessionCommand::NewWindow).await;
    server
        .wait_for_windows("s", &[window_summary(1, 1), window_summary(2, 1)])
        .await;
}

#[test]
fn a_config_error_stops_attach_and_new_before_raw_mode() {
    let server = TestServer::start();
    server.create_session("s");
    let init = server.config_path();
    let line = fs::read_to_string(&init).unwrap().lines().count() + 1;
    let mut file = File::options().append(true).open(&init).unwrap();
    writeln!(file, "amux.opt.bogus = 1").unwrap();
    let expected = format!("init.lua:{line}: unknown option amux.opt.bogus");

    for args in [&[][..], &["new", "-s", "t"], &["attach", "-t", "s"]] {
        let mut terminal = server.terminal(args);
        terminal.wait_for_text(&expected);
        assert!(!terminal.wait_for_exit().success(), "{args:?}");
        assert!(!terminal.screen().alternate_screen(), "{args:?}");
        assert!(terminal.terminal_modes_unchanged(), "{args:?}");
    }

    let listing = Listing::parse(&server.run_ok(&["ls"]));
    assert_eq!(listing.sessions(server.name()), ["s"]);
}
