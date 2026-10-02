mod common;

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use amux::protocol::{SessionCommand, WindowSummary};
use common::{lua, screen_row, window_summary, Listing, TestServer, TIMEOUT};
use nix::sys::signal::{kill, Signal};

const RELOADED: &str = "config reloaded";
const NOT_RELOADED: &str = "the config did not reload";
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, Copy)]
enum Trigger {
    Command,
    Hangup,
}

fn reload(server: &TestServer, trigger: Trigger) -> Result<String, String> {
    match trigger {
        Trigger::Command => {
            let output = server.run(&["config", "reload"]);
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if output.status.success() {
                Ok(stdout)
            } else {
                Err(stderr)
            }
        }
        Trigger::Hangup => {
            let before = reload_lines(&server.log()).len();
            kill(server.pid(), Signal::SIGHUP).expect("sending SIGHUP");
            wait_for_reload(server, before)
        }
    }
}

fn wait_for_reload(server: &TestServer, before: usize) -> Result<String, String> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let log = server.log();
        if let Some(line) = reload_lines(&log).get(before) {
            return if line.contains(RELOADED) {
                Ok((*line).to_owned())
            } else {
                Err((*line).to_owned())
            };
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the reload in the server log:\n{log}"
        );
        thread::sleep(POLL);
    }
}

fn reload_lines(log: &str) -> Vec<&str> {
    log.lines()
        .filter(|line| line.contains(RELOADED) || line.contains(NOT_RELOADED))
        .collect()
}

fn append(server: &TestServer, source: &str) {
    let mut init = File::options()
        .append(true)
        .open(server.config_path())
        .expect("opening init.lua");
    writeln!(init, "\n{source}").expect("appending to init.lua");
}

fn named_window(index: usize, name: &str) -> WindowSummary {
    WindowSummary {
        index,
        name: name.into(),
        panes: 1,
    }
}

fn wait_for_file(path: &Path, expected: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let written = fs::read_to_string(path).unwrap_or_default();
        if written == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected:?} in {}, found {written:?}",
            path.display()
        );
        thread::sleep(POLL);
    }
}

async fn a_new_pane_term_reaches_new_windows_only(trigger: Trigger) {
    let server = TestServer::start();
    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    client.wait_for_text("$").await;

    append(&server, "amux.opt.pane.term = \"tmux-256color\"");
    let reloaded = reload(&server, trigger).unwrap();
    assert!(reloaded.contains(RELOADED), "{reloaded}");

    client.command(SessionCommand::NewWindow).await;
    server
        .wait_for_windows("s", &[window_summary(0, 1), window_summary(1, 1)])
        .await;
    client.type_text("echo \"[$TERM]\"\r").await;
    client.wait_for_text("[tmux-256color]").await;

    client.command(SessionCommand::SelectWindow(0)).await;
    client.type_text("echo \"<$TERM>\"\r").await;
    client.wait_for_text("<screen-256color>").await;
}

#[tokio::test]
async fn config_reload_gives_new_windows_the_new_pane_term() {
    a_new_pane_term_reaches_new_windows_only(Trigger::Command).await;
}

#[tokio::test]
async fn sighup_gives_new_windows_the_new_pane_term() {
    a_new_pane_term_reaches_new_windows_only(Trigger::Hangup).await;
}

fn a_changed_hook_replaces_the_old_one(trigger: Trigger) {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("created.txt");
    let server = TestServer::builder()
        .config(&format!(
            "amux.on(\"session_created\", function(ev)\n\
               local file = assert(io.open({}, \"a\"))\n\
               file:write(\"first \", ev.session, \"\\n\")\n\
               file:close()\n\
             end)",
            lua(&journal.display().to_string())
        ))
        .start();
    server.create_session("a");
    wait_for_file(&journal, "first a\n");

    let init = fs::read_to_string(server.config_path()).unwrap();
    fs::write(
        server.config_path(),
        init.replace("\"first \"", "\"second \""),
    )
    .unwrap();
    reload(&server, trigger).unwrap();

    server.create_session("b");
    wait_for_file(&journal, "first a\nsecond b\n");
}

#[test]
fn config_reload_replaces_the_hooks() {
    a_changed_hook_replaces_the_old_one(Trigger::Command);
}

#[test]
fn sighup_replaces_the_hooks() {
    a_changed_hook_replaces_the_old_one(Trigger::Hangup);
}

fn servers_follow_the_reloaded_config(trigger: Trigger) {
    let b = TestServer::start();
    let a = TestServer::start();
    let init = fs::read_to_string(a.config_path()).unwrap();

    a.add_peer(&b);
    reload(&a, trigger).unwrap();
    a.wait_for_ls("the added server", |ls| ls.is_online(b.name()));

    fs::write(a.config_path(), init).unwrap();
    reload(&a, trigger).unwrap();
    a.wait_for_ls("the removed server to leave", |ls| {
        ls.server(b.name()).is_none()
    });
}

#[test]
fn config_reload_adds_and_removes_servers() {
    servers_follow_the_reloaded_config(Trigger::Command);
}

#[test]
fn sighup_adds_and_removes_servers() {
    servers_follow_the_reloaded_config(Trigger::Hangup);
}

async fn a_syntax_error_keeps_the_running_config(trigger: Trigger) {
    let server = TestServer::builder()
        .config("amux.opt.window.name = \"kept\"")
        .start();
    append(&server, "amux.opt.window.name = \"changed\"");
    let line = fs::read_to_string(server.config_path())
        .unwrap()
        .lines()
        .count()
        + 1;
    append(&server, "local = 1");
    let line = line + 1;

    let error = reload(&server, trigger).unwrap_err();
    assert!(error.contains(&format!("init.lua:{line}:")), "{error}");

    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    server
        .wait_for_windows("s", &[named_window(0, "kept")])
        .await;
}

#[tokio::test]
async fn config_reload_reports_a_syntax_error_and_keeps_the_running_config() {
    a_syntax_error_keeps_the_running_config(Trigger::Command).await;
}

#[tokio::test]
async fn sighup_reports_a_syntax_error_and_keeps_the_running_config() {
    a_syntax_error_keeps_the_running_config(Trigger::Hangup).await;
}

async fn a_new_name_needs_a_restart(trigger: Trigger) {
    let server = TestServer::start();
    append(
        &server,
        "amux.opt.name = \"renamed\"\namux.opt.window.name = \"applied\"",
    );

    let reloaded = reload(&server, trigger).unwrap();
    assert!(reloaded.contains(RELOADED), "{reloaded}");
    assert!(
        reloaded.contains("restart required for amux.opt.name"),
        "{reloaded}"
    );

    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    server
        .wait_for_windows("s", &[named_window(0, "applied")])
        .await;
    let listing = Listing::parse(&server.run_ok(&["ls"]));
    assert_eq!(listing.sessions(server.name()), ["s"]);
    assert!(listing.server("renamed").is_none());
}

#[tokio::test]
async fn config_reload_refuses_a_new_name() {
    a_new_name_needs_a_restart(Trigger::Command).await;
}

#[tokio::test]
async fn sighup_refuses_a_new_name() {
    a_new_name_needs_a_restart(Trigger::Hangup).await;
}

#[test]
fn config_reload_without_a_server_says_so() {
    let server = TestServer::builder().prepare();
    let output = server.run(&["config", "reload"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("no server is running on ") && stdout.contains("nothing to reload"),
        "{stdout}"
    );
    assert!(!server.is_listening());
}

#[test]
fn config_reload_prints_what_it_did() {
    let server = TestServer::start();
    assert_eq!(
        server.run_ok(&["config", "reload"]),
        format!("{RELOADED}\n")
    );
    append(&server, "amux.opt.lan.port = 7448");
    assert_eq!(
        server.run_ok(&["config", "reload"]),
        format!("{RELOADED}; restart required for amux.opt.lan\n")
    );
}

#[tokio::test]
async fn a_reload_rebinds_copy_keys_of_a_pane_already_in_copy_mode() {
    let server = TestServer::start();
    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    client.wait_for_text("$").await;
    client
        .command(SessionCommand::CopyMode { page_up: false })
        .await;
    client.type_text("j").await;
    client
        .wait_for_screen("the cursor one line down", |screen| {
            screen_row(screen, 0).ends_with("[2/24]")
        })
        .await;

    append(&server, "amux.keymap.set(\"copy\", \"j\", \"cancel\")");
    let reloaded = reload(&server, Trigger::Command).unwrap();
    assert!(reloaded.contains(RELOADED), "{reloaded}");
    client.type_text("j").await;
    client
        .wait_for_screen("the live pane", |screen| screen_row(screen, 0) == "$")
        .await;
}

#[test]
fn ctrl_b_r_reloads_the_attached_client_and_keeps_it_when_the_config_breaks() {
    let server = TestServer::start();
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_text("$");
    terminal.wait_for_status("the built-in status", |line| {
        line.starts_with(&format!("[s@{}] 0:sh", server.name()))
    });

    append(
        &server,
        "amux.opt.notice_ms = 500\n\
         amux.opt.status.session_format = \"<{session}>\"\n\
         amux.keymap.set(\"prefix\", \"g\", function() amux.notify(\"from the new binding\") end)",
    );
    terminal.type_text("\x02r");
    terminal.wait_for_status("the reload notice", |line| line == RELOADED);
    terminal.wait_for_status("the new session format", |line| {
        line.starts_with("<s> 0:sh")
    });
    terminal.type_text("\x02g");
    terminal.wait_for_status("the new binding", |line| line == "from the new binding");
    terminal.wait_for_status("the notice to expire", |line| line.starts_with("<s> "));

    append(&server, "local = 1");
    let line = fs::read_to_string(server.config_path())
        .unwrap()
        .lines()
        .count();
    terminal.type_text("\x02r");
    let error = terminal.wait_for_status("the reload error", |line| line.contains("init.lua:"));
    assert!(
        error.contains(&format!("init.lua:{line}: ")) && !error.contains("the server"),
        "{error}"
    );
    terminal.wait_for_status("the error to expire", |line| line.starts_with("<s> "));
    terminal.type_text("\x02g");
    terminal.wait_for_status("the kept binding", |line| line == "from the new binding");
}

#[tokio::test]
async fn saving_the_config_reloads_the_server_on_its_own() {
    let server = TestServer::builder().watch_config().start();
    let mut client = server.client().await;
    client.new_session(Some("s")).await;
    client.wait_for_text("$").await;

    let before = reload_lines(&server.log()).len();
    append(&server, "amux.opt.pane.term = \"tmux-256color\"");
    let reloaded = wait_for_reload(&server, before).unwrap();
    assert!(reloaded.contains(RELOADED), "{reloaded}");

    client.command(SessionCommand::NewWindow).await;
    server
        .wait_for_windows("s", &[window_summary(0, 1), window_summary(1, 1)])
        .await;
    client.type_text("echo \"[$TERM]\"\r").await;
    client.wait_for_text("[tmux-256color]").await;
}

#[tokio::test]
async fn a_broken_module_keeps_the_running_config_until_it_is_saved_again() {
    let mut server = TestServer::builder()
        .config("amux.opt.window.name = require(\"names\").window")
        .watch_config()
        .prepare();
    let modules = server.config_path().with_file_name("lua");
    fs::create_dir_all(&modules).unwrap();
    let names = modules.join("names.lua");
    fs::write(&names, "return { window = \"first\" }").unwrap();
    server.start_process();

    let before = reload_lines(&server.log()).len();
    fs::write(&names, "return { window = }").unwrap();
    let error = wait_for_reload(&server, before).unwrap_err();
    assert!(error.contains(NOT_RELOADED), "{error}");
    server.client().await.new_session(Some("kept")).await;
    server
        .wait_for_windows("kept", &[named_window(0, "first")])
        .await;

    fs::write(&names, "return { window = \"second\" }").unwrap();
    wait_for_reload(&server, before + 1).unwrap();
    server.client().await.new_session(Some("fixed")).await;
    server
        .wait_for_windows("fixed", &[named_window(0, "second")])
        .await;
}

#[test]
fn a_saved_config_is_left_alone_without_watch() {
    let server = TestServer::start();
    let before = reload_lines(&server.log()).len();
    append(&server, "amux.opt.pane.term = \"tmux-256color\"");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(
        reload_lines(&server.log()).len(),
        before,
        "{}",
        server.log()
    );
}

#[test]
fn saving_the_config_reloads_the_attached_client() {
    let server = TestServer::builder().watch_config().start();
    let mut terminal = server.terminal(&["new", "-s", "s"]);
    terminal.wait_for_text("$");
    terminal.wait_for_status("the built-in status", |line| {
        line.starts_with(&format!("[s@{}] 0:sh", server.name()))
    });

    append(
        &server,
        "amux.opt.notice_ms = 500\namux.opt.status.session_format = \"<{session}>\"",
    );
    terminal.wait_for_status("the reload notice", |line| line == RELOADED);
    terminal.wait_for_status("the new session format", |line| {
        line.starts_with("<s> 0:sh")
    });

    append(&server, "local = 1");
    let line = fs::read_to_string(server.config_path())
        .unwrap()
        .lines()
        .count();
    let error = terminal.wait_for_status("the reload error", |line| line.contains("init.lua:"));
    assert!(error.contains(&format!("init.lua:{line}: ")), "{error}");
    terminal.wait_for_status("the error to expire", |line| line.starts_with("<s> "));
}
