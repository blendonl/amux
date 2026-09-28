mod common;

use common::{TerminalClient, TestServer, DETACH};

fn new_session(server: &TestServer) -> TerminalClient {
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_status("the session", |line| line.starts_with("[s@"));
    terminal
}

fn wait_until_gone(terminal: &mut TerminalClient, text: &str) {
    terminal.wait_for(&format!("{text:?} to go away"), |contents| {
        !contents.contains(text)
    });
}

fn detach(terminal: &mut TerminalClient) {
    terminal.type_text(DETACH);
    terminal.wait_for_text("[detached (from session s)]");
    assert!(terminal.wait_for_exit().success());
}

#[test]
fn pausing_after_the_prefix_shows_the_keys_and_the_next_key_runs_and_closes_them() {
    let server = TestServer::start();
    let mut terminal = new_session(&server);

    terminal.type_text("\x02");
    terminal.wait_for_text("─ C-b ─");
    terminal.wait_for_text("c     → new window");
    terminal.wait_for_text("C-b   → send prefix");
    terminal.wait_for_status("the status bar under the keys", |line| {
        line.starts_with("[s@") && line.contains("0:sh")
    });

    terminal.type_text("c");
    terminal.wait_for_status("the new window", |line| line.contains("1:sh"));
    wait_until_gone(&mut terminal, "→ new window");
    terminal.type_text("echo after-$((6*7))\r");
    terminal.wait_for_text("after-42");
    detach(&mut terminal);
}

#[test]
fn lua_descriptions_name_the_keys_and_a_group_opens_in_place() {
    let server = TestServer::builder()
        .config(
            "amux.opt.which_key.delay_ms = 100\n\
             amux.keymap.set('prefix', 'g', function() amux.notify('from g') end, \
             { desc = 'say hello' })\n\
             amux.keymap.set('prefix', 'R', amux.action.switch_table('resize'), \
             { desc = 'resize' })\n\
             amux.keymap.set('resize', 'h', amux.action.select_pane('left'), \
             { desc = 'go left' })",
        )
        .start();
    let mut terminal = new_session(&server);

    terminal.type_text("\x02");
    terminal.wait_for_text("g     → say hello");
    terminal.wait_for_text("R     → +resize");

    terminal.type_text("R");
    terminal.wait_for_text("─ C-b R ─");
    terminal.wait_for_text("h → go left");
    wait_until_gone(&mut terminal, "→ say hello");

    terminal.type_text("\x7f");
    terminal.wait_for_text("g     → say hello");
    wait_until_gone(&mut terminal, "→ go left");

    terminal.type_text("\x1b");
    wait_until_gone(&mut terminal, "→ say hello");
    terminal.wait_for_status("the status bar", |line| line.starts_with("[s@"));

    terminal.type_text("\x02g");
    terminal.wait_for_status("the notice", |line| line == "from g");
    detach(&mut terminal);
}

#[test]
fn ctrl_b_question_mark_shows_the_keys_when_the_popup_is_turned_off() {
    let server = TestServer::builder()
        .config("amux.opt.which_key.enabled = false")
        .start();
    let mut terminal = new_session(&server);

    terminal.type_text("\x02?");
    terminal.wait_for_text("─ C-b ─");
    terminal.wait_for_text("?     → show prefix keys");

    terminal.type_text("q");
    wait_until_gone(&mut terminal, "→ show prefix keys");
    terminal.type_text("echo still-$((6*7))\r");
    terminal.wait_for_text("still-42");
    assert!(!terminal.contents().contains("qecho"));
    detach(&mut terminal);
}
