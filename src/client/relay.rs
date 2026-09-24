use std::fmt;
use std::mem;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};

use super::chrome::{
    draw_row, render_reconnecting, Prompt, PromptEvent, Rect, StatusLine, Style, WindowTab,
    ESCAPE_TIMEOUT,
};
use super::keys::{Action, Panel, PrefixRouter, DEFAULT_PREFIX};
use super::terminal;
use super::tree::{ClusterTree, TreeEvent};
use crate::protocol::{
    AttachedSession, ClientMessage, ClusterStatus, ServerMessage, ServerView, SessionCommand,
    SessionState, Size,
};
use crate::target::Target;

const NOTICE_TIME: Duration = Duration::from_secs(3);
const DRAIN_LIMIT: usize = 64;
const RENAME_WINDOW: &str = "rename window";
const RENAME_SESSION: &str = "rename session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Detached,
    Exited,
    ServerExited,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Detached => "detached",
            Self::Exited => "exited",
            Self::ServerExited => "server exited",
        })
    }
}

enum Overlay {
    Prompt(Prompt, Rename),
    Loading(Vec<u8>),
    Tree(ClusterTree),
}

#[derive(Debug, Clone, Copy)]
enum Rename {
    Window,
    Session,
}

pub struct Relay {
    local: String,
    attached: AttachedSession,
    size: Size,
    router: PrefixRouter,
    session: Option<SessionState>,
    cluster: Option<ClusterStatus>,
    notice: Option<(String, Instant)>,
    reconnecting: Option<String>,
    overlay: Option<Overlay>,
    escape_at: Option<Instant>,
    last_error: Option<String>,
    output: Vec<u8>,
    messages: Vec<ClientMessage>,
    chrome_dirty: bool,
    end: Option<Result<Outcome, String>>,
}

impl Relay {
    pub fn new(local: String, attached: AttachedSession, size: Size) -> Self {
        Self {
            local,
            attached,
            size,
            router: PrefixRouter::new(DEFAULT_PREFIX),
            session: None,
            cluster: None,
            notice: None,
            reconnecting: None,
            overlay: None,
            escape_at: None,
            last_error: None,
            output: Vec::new(),
            messages: Vec::new(),
            chrome_dirty: true,
            end: None,
        }
    }

    pub fn label(&self) -> String {
        if self.attached.server == self.local {
            self.session_name().to_owned()
        } else {
            format!("{}@{}", self.session_name(), self.attached.server)
        }
    }

    pub fn server_message(&mut self, message: Option<ServerMessage>) {
        let error = self.last_error.take();
        match message {
            Some(ServerMessage::Output(bytes)) => self.host_output(&bytes),
            Some(ServerMessage::Attached(attached)) => self.attached_to(attached),
            Some(ServerMessage::SessionState(state)) => {
                self.session = Some(state);
                self.chrome_dirty = true;
            }
            Some(ServerMessage::ClusterStatus(status)) => {
                self.cluster = Some(status);
                self.chrome_dirty = true;
            }
            Some(ServerMessage::Cluster(servers)) => self.show_tree(&servers),
            Some(ServerMessage::Reconnecting { server }) => {
                self.reconnecting = Some(server);
                self.chrome_dirty = true;
            }
            Some(ServerMessage::Detached) => self.end = Some(Ok(Outcome::Detached)),
            Some(ServerMessage::Exited) => self.end = Some(Ok(Outcome::Exited)),
            Some(ServerMessage::Error(message)) => {
                self.notice = Some((message.clone(), Instant::now() + NOTICE_TIME));
                self.chrome_dirty = true;
                self.last_error = Some(message);
            }
            Some(other) => {
                self.end = Some(Err(format!("unexpected message from server: {other:?}")));
            }
            None => self.end = Some(error.map_or(Ok(Outcome::ServerExited), Err)),
        }
    }

    pub fn input(&mut self, chunk: &[u8]) {
        if self.overlay.is_some() {
            self.overlay_input(chunk);
            return;
        }
        let (actions, rest) = self.router.route(chunk);
        for action in actions {
            match action {
                Action::Forward(bytes) => self.messages.push(ClientMessage::Input(bytes)),
                Action::Detach => self.messages.push(ClientMessage::Detach),
                Action::Command(command) => self.messages.push(ClientMessage::Command(command)),
                Action::Open(panel) => self.open(panel),
            }
        }
        if !rest.is_empty() {
            self.overlay_input(rest);
        }
    }

    pub fn stdin_closed(&mut self) {
        self.messages.push(ClientMessage::Detach);
    }

    pub fn resize(&mut self, size: Size) {
        self.size = size;
        self.messages
            .push(ClientMessage::Resize(terminal::session_area(size)));
        if self.reconnecting.is_some() {
            self.clear_above_status();
        }
        self.chrome_dirty = true;
    }

    pub fn escape_deadline(&self) -> Option<Instant> {
        self.escape_at
    }

    pub fn escape_timeout(&mut self) {
        self.escape_at = None;
        match &mut self.overlay {
            Some(Overlay::Prompt(prompt, rename)) => {
                let (event, rename) = (prompt.time_out(), *rename);
                self.prompt_event(event, rename);
            }
            Some(Overlay::Tree(tree)) => {
                let event = tree.time_out();
                self.tree_event(event);
            }
            Some(Overlay::Loading(_)) | None => {}
        }
    }

    pub fn notice_deadline(&self) -> Option<Instant> {
        self.notice.as_ref().map(|(_, until)| *until)
    }

    pub fn notice_expired(&mut self) {
        self.notice = None;
        self.chrome_dirty = true;
    }

    pub fn take_output(&mut self) -> Vec<u8> {
        if mem::take(&mut self.chrome_dirty) {
            self.draw_chrome();
        }
        mem::take(&mut self.output)
    }

    pub fn take_messages(&mut self) -> Vec<ClientMessage> {
        mem::take(&mut self.messages)
    }

    pub fn has_ended(&self) -> bool {
        self.end.is_some()
    }

    pub fn take_end(&mut self) -> Option<Result<Outcome, String>> {
        self.end.take()
    }

    fn host_output(&mut self, bytes: &[u8]) {
        if matches!(self.overlay, Some(Overlay::Tree(_) | Overlay::Loading(_))) {
            return;
        }
        self.output.extend_from_slice(bytes);
        self.chrome_dirty = true;
    }

    fn attached_to(&mut self, attached: AttachedSession) {
        self.attached = attached;
        self.session = None;
        self.reconnecting = None;
        self.chrome_dirty = true;
    }

    fn open(&mut self, panel: Panel) {
        let overlay = match panel {
            Panel::RenameWindow => Overlay::Prompt(
                Prompt::new(RENAME_WINDOW, &self.active_window_name()),
                Rename::Window,
            ),
            Panel::RenameSession => Overlay::Prompt(
                Prompt::new(RENAME_SESSION, self.session_name()),
                Rename::Session,
            ),
            Panel::ClusterTree => {
                self.messages.push(ClientMessage::ListCluster);
                Overlay::Loading(Vec::new())
            }
        };
        self.overlay = Some(overlay);
        self.chrome_dirty = true;
    }

    fn show_tree(&mut self, servers: &[ServerView]) {
        let Some(Overlay::Loading(pending)) = &mut self.overlay else {
            return;
        };
        let pending = mem::take(pending);
        let attached = self.attached_target();
        self.overlay = Some(Overlay::Tree(ClusterTree::new(servers, Some(&attached))));
        self.chrome_dirty = true;
        self.overlay_input(&pending);
    }

    fn overlay_input(&mut self, input: &[u8]) {
        match &mut self.overlay {
            Some(Overlay::Prompt(prompt, rename)) => {
                let (event, rename) = (prompt.handle(input), *rename);
                self.prompt_event(event, rename);
            }
            Some(Overlay::Tree(tree)) => {
                let event = tree.handle(input);
                self.tree_event(event);
            }
            Some(Overlay::Loading(pending)) => pending.extend_from_slice(input),
            None => {}
        }
        self.arm_escape();
    }

    fn prompt_event(&mut self, event: PromptEvent, rename: Rename) {
        match event {
            PromptEvent::Pending => self.chrome_dirty = true,
            PromptEvent::Cancel => self.close_overlay(),
            PromptEvent::Submit(name) if name.is_empty() => self.close_overlay(),
            PromptEvent::Submit(name) => {
                let request = match rename {
                    Rename::Window => ClientMessage::Command(SessionCommand::RenameWindow(name)),
                    Rename::Session => ClientMessage::RenameSession {
                        target: self.attached_target(),
                        name,
                    },
                };
                self.messages.push(request);
                self.close_overlay();
            }
        }
    }

    fn tree_event(&mut self, event: TreeEvent) {
        match event {
            TreeEvent::Pending => self.chrome_dirty = true,
            TreeEvent::Cancel => self.close_overlay(),
            TreeEvent::Pick(target) => {
                self.messages.push(ClientMessage::Switch(target));
                self.close_overlay();
            }
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = None;
        self.escape_at = None;
        self.messages.push(ClientMessage::Redraw);
        if self.reconnecting.is_some() {
            self.clear_above_status();
        }
        self.chrome_dirty = true;
    }

    fn arm_escape(&mut self) {
        let partial = match &self.overlay {
            Some(Overlay::Prompt(prompt, _)) => prompt.is_partial(),
            Some(Overlay::Tree(tree)) => tree.is_partial(),
            Some(Overlay::Loading(_)) | None => false,
        };
        self.escape_at = partial.then(|| Instant::now() + ESCAPE_TIMEOUT);
    }

    fn draw_chrome(&mut self) {
        let above = self.above_status();
        let status_row = self.size.rows.saturating_sub(terminal::STATUS_ROWS);
        let status = self.status_line();
        match &mut self.overlay {
            Some(Overlay::Tree(tree)) => self.output.extend(tree.render(above)),
            _ => {
                if let Some(server) = &self.reconnecting {
                    let area = Size {
                        rows: above.rows,
                        cols: above.cols,
                    };
                    self.output.extend(render_reconnecting(server, area));
                }
            }
        }
        match &self.overlay {
            Some(Overlay::Prompt(prompt, _)) => self
                .output
                .extend(prompt.render(self.size.cols, status_row)),
            _ => self.output.extend(status.render(self.size.cols)),
        }
    }

    fn clear_above_status(&mut self) {
        let above = self.above_status();
        for row in 0..usize::from(above.rows) {
            draw_row(
                &mut self.output,
                row,
                0,
                usize::from(above.cols),
                &[],
                Style::PLAIN,
            );
        }
    }

    fn above_status(&self) -> Rect {
        Rect {
            row: 0,
            col: 0,
            rows: self.size.rows.saturating_sub(terminal::STATUS_ROWS),
            cols: self.size.cols,
        }
    }

    fn status_line(&self) -> StatusLine {
        let host = &self.attached.server;
        let cluster = self.cluster.as_ref();
        let local = cluster.map_or(&self.local, |status| &status.local);
        StatusLine {
            session: self.session_name().to_owned(),
            server: host.clone(),
            local: host == local,
            windows: self
                .session
                .as_ref()
                .map(|state| WindowTab::list(&state.windows, state.active))
                .unwrap_or_default(),
            latency: cluster
                .filter(|status| status.host == *host)
                .and_then(|status| status.latency),
            offline: cluster
                .map(|status| status.offline.clone())
                .unwrap_or_default(),
            message: self.notice.as_ref().map(|(message, _)| message.clone()),
        }
    }

    fn session_name(&self) -> &str {
        self.session
            .as_ref()
            .map_or(&self.attached.session, |state| &state.name)
    }

    fn active_window_name(&self) -> String {
        self.session
            .as_ref()
            .and_then(|state| {
                state
                    .windows
                    .iter()
                    .find(|window| window.index == state.active)
            })
            .map(|window| window.name.clone())
            .unwrap_or_default()
    }

    fn attached_target(&self) -> Target {
        Target {
            session: Some(self.session_name().to_owned()),
            server: Some(self.attached.server.clone()),
            window: None,
            pane: None,
        }
    }
}

pub async fn run(
    mut incoming: mpsc::Receiver<ServerMessage>,
    outgoing: mpsc::Sender<ClientMessage>,
    relay: &mut Relay,
) -> Result<Outcome> {
    let mut stdin = terminal::stdin_chunks();
    let mut resizes = signal(SignalKind::window_change())?;
    let mut stdin_open = true;

    loop {
        let output = relay.take_output();
        if !output.is_empty() {
            terminal::write_output(&output)?;
        }
        if let Some(end) = relay.take_end() {
            return end.map_err(|message| anyhow!(message));
        }
        for message in relay.take_messages() {
            super::send(&outgoing, message).await?;
        }

        let escape = relay.escape_deadline();
        let notice = relay.notice_deadline();
        tokio::select! {
            message = incoming.recv() => {
                relay.server_message(message);
                for _ in 0..DRAIN_LIMIT {
                    if relay.has_ended() {
                        break;
                    }
                    let Ok(message) = incoming.try_recv() else {
                        break;
                    };
                    relay.server_message(Some(message));
                }
            }
            chunk = stdin.recv(), if stdin_open => match chunk {
                Some(chunk) => relay.input(&chunk),
                None => {
                    stdin_open = false;
                    relay.stdin_closed();
                }
            },
            _ = resizes.recv() => relay.resize(terminal::size()?),
            () = sleep_until(escape.unwrap_or_else(Instant::now)), if escape.is_some() => {
                relay.escape_timeout();
            }
            () = sleep_until(notice.unwrap_or_else(Instant::now)), if notice.is_some() => {
                relay.notice_expired();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::client::chrome::testing::{row_text, screen_text, terminal};
    use crate::config::Incarnation;
    use crate::protocol::{ServerStatus, SessionId, SessionInfo, WindowSummary};

    const SIZE: Size = Size { rows: 10, cols: 40 };
    const BOTTOM: u16 = SIZE.rows - 1;

    fn attached(session: &str, server: &str) -> AttachedSession {
        AttachedSession {
            server: server.into(),
            session: session.into(),
            id: SessionId(1),
            incarnation: Incarnation::random().unwrap(),
        }
    }

    fn relay_on(server: &str) -> (Relay, vt100::Parser) {
        let relay = Relay::new("laptop".into(), attached("work", server), SIZE);
        (relay, terminal(SIZE.rows, SIZE.cols))
    }

    fn draw(relay: &mut Relay, parser: &mut vt100::Parser) {
        parser.process(&relay.take_output());
    }

    fn bottom(relay: &mut Relay, parser: &mut vt100::Parser) -> String {
        draw(relay, parser);
        row_text(parser, BOTTOM)
    }

    fn window(index: usize, name: &str) -> WindowSummary {
        WindowSummary {
            index,
            name: name.into(),
            panes: 1,
        }
    }

    fn state(name: &str, active: usize) -> ServerMessage {
        ServerMessage::SessionState(SessionState {
            name: name.into(),
            windows: vec![window(0, "sh"), window(1, "vim")],
            active,
        })
    }

    fn session_info(name: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId(1),
            name: name.into(),
            windows: vec![window(0, "sh")],
            attached_clients: 0,
            last_activity: SystemTime::UNIX_EPOCH,
            project: None,
            branch: None,
        }
    }

    fn server_view(name: &str, status: ServerStatus, session: &str) -> ServerView {
        ServerView {
            id: None,
            name: name.into(),
            address: None,
            version: None,
            status,
            sessions: vec![session_info(session)],
            projects: Vec::new(),
        }
    }

    fn cluster() -> ServerMessage {
        ServerMessage::Cluster(vec![
            server_view("laptop", ServerStatus::Local, "work"),
            server_view(
                "desktop",
                ServerStatus::Online {
                    latency: Some(Duration::from_millis(12)),
                },
                "notes",
            ),
        ])
    }

    #[test]
    fn the_status_line_names_the_session_and_its_windows() {
        let (mut relay, mut parser) = relay_on("laptop");
        assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop]");

        relay.server_message(Some(state("work", 1)));
        relay.server_message(Some(ServerMessage::Output(b"\x1b[1;1Hhello".to_vec())));
        assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop] 0:sh  1:vim");
        assert_eq!(row_text(&parser, 0), "hello");
        assert_eq!(parser.screen().cursor_position(), (0, 5));
        assert_eq!(relay.label(), "work");
    }

    #[test]
    fn a_remote_session_shows_its_latency_and_the_offline_peers() {
        let size = Size { rows: 4, cols: 70 };
        let mut relay = Relay::new("laptop".into(), attached("work", "desktop"), size);
        let mut parser = terminal(size.rows, size.cols);
        relay.server_message(Some(state("work", 0)));
        relay.server_message(Some(ServerMessage::ClusterStatus(ClusterStatus {
            local: "laptop".into(),
            host: "desktop".into(),
            latency: Some(Duration::from_millis(12)),
            offline: vec!["attic".into()],
        })));

        draw(&mut relay, &mut parser);
        assert_eq!(
            row_text(&parser, size.rows - 1),
            format!("{:<49}attic offline  12 ms", "[work@desktop] 0:sh  1:vim")
        );
        assert_eq!(relay.label(), "work@desktop");
    }

    #[test]
    fn the_latency_of_another_server_is_not_shown() {
        let (mut relay, mut parser) = relay_on("laptop");
        relay.server_message(Some(ServerMessage::ClusterStatus(ClusterStatus {
            local: "laptop".into(),
            host: "desktop".into(),
            latency: Some(Duration::from_millis(12)),
            offline: Vec::new(),
        })));
        assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop]");
    }

    #[test]
    fn typing_reaches_the_session_and_resizes_leave_room_for_the_status_line() {
        let (mut relay, _) = relay_on("laptop");
        relay.input(b"ls\r\x02n\x02d");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Input(b"ls\r".to_vec()),
                ClientMessage::Command(SessionCommand::NextWindow),
                ClientMessage::Detach,
            ]
        );

        relay.resize(Size {
            rows: 30,
            cols: 100,
        });
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Resize(Size {
                rows: 29,
                cols: 100
            })]
        );
    }

    #[test]
    fn renaming_the_window_starts_from_its_name_and_redraws_after() {
        let (mut relay, mut parser) = relay_on("laptop");
        relay.server_message(Some(state("work", 1)));
        relay.input(b"\x02,");
        assert_eq!(bottom(&mut relay, &mut parser), "rename window: vim");
        assert_eq!(parser.screen().cursor_position(), (BOTTOM, 18));

        relay.server_message(Some(ServerMessage::Output(b"\x1b[2;1Hout".to_vec())));
        draw(&mut relay, &mut parser);
        assert_eq!(row_text(&parser, 1), "out");
        assert_eq!(parser.screen().cursor_position(), (BOTTOM, 18));

        relay.input(b"\x7f\x7f\x7fed\x1b[<0;3;4M\x1b[M !!it\r");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Command(SessionCommand::RenameWindow("edit".into())),
                ClientMessage::Redraw,
            ]
        );
        assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop] 0:sh  1:vim");
    }

    #[test]
    fn renaming_the_session_targets_the_attached_session_in_one_chunk() {
        let (mut relay, _) = relay_on("desktop");
        relay.input(b"a\x02$\x15notes\rignored");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Input(b"a".to_vec()),
                ClientMessage::RenameSession {
                    target: "work@desktop".parse().unwrap(),
                    name: "notes".into(),
                },
                ClientMessage::Redraw,
            ]
        );
        relay.input(b"b");
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Input(b"b".to_vec())]
        );
    }

    #[test]
    fn an_empty_name_renames_nothing() {
        let (mut relay, _) = relay_on("laptop");
        relay.input(b"\x02$\x15\r");
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
    }

    #[test]
    fn the_cluster_tree_hides_the_session_and_switches_to_the_pick() {
        let (mut relay, mut parser) = relay_on("laptop");
        relay.input(b"\x02sG");
        assert_eq!(relay.take_messages(), vec![ClientMessage::ListCluster]);

        relay.server_message(Some(ServerMessage::Output(b"hidden".to_vec())));
        relay.server_message(Some(cluster()));
        draw(&mut relay, &mut parser);
        let screen = screen_text(&parser);
        assert!(!screen.concat().contains("hidden"), "{screen:?}");
        assert_eq!(screen[0], "- laptop  (this server)");
        assert_eq!(screen[5], "    + notes  1 window");
        assert_eq!(screen[usize::from(BOTTOM)], "[work@laptop]");
        assert!(parser.screen().cell(5, 6).unwrap().inverse());

        relay.server_message(Some(ServerMessage::Output(b"hidden".to_vec())));
        assert!(!relay
            .take_output()
            .windows(6)
            .any(|bytes| bytes == b"hidden"));

        relay.input(b"\r");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Switch("notes@desktop".parse().unwrap()),
                ClientMessage::Redraw,
            ]
        );
        relay.server_message(Some(ServerMessage::Output(b"shown".to_vec())));
        assert!(relay.take_output().starts_with(b"shown"));
    }

    #[test]
    fn a_lone_escape_closes_a_panel_once_the_timeout_expires() {
        let (mut relay, _) = relay_on("laptop");
        relay.input(b"\x02s");
        relay.server_message(Some(cluster()));
        relay.take_messages();
        assert_eq!(relay.escape_deadline(), None);

        relay.input(b"\x1b");
        assert!(relay.escape_deadline().is_some());
        relay.input(b"[B");
        assert_eq!(relay.escape_deadline(), None);
        relay.input(b"\x1b");
        relay.escape_timeout();
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
        assert_eq!(relay.escape_deadline(), None);

        relay.input(b"x");
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Input(b"x".to_vec())]
        );
    }

    #[test]
    fn mouse_reports_never_reach_the_session_while_a_panel_is_open() {
        let (mut relay, _) = relay_on("laptop");
        relay.input(b"\x02,\x1b[<0;10;5M\x1b[<0;10;5m\x1b[M #$");
        relay.input(b"\x1b[<64;1;1M");
        relay.input(b"\x03");
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
    }

    #[test]
    fn an_error_shows_on_the_status_line_for_a_while_and_keeps_the_session() {
        let (mut relay, mut parser) = relay_on("laptop");
        relay.server_message(Some(ServerMessage::Error("can't find session: x".into())));
        assert_eq!(bottom(&mut relay, &mut parser), "can't find session: x");
        assert!(!relay.has_ended());
        assert!(relay.notice_deadline().is_some());

        relay.server_message(Some(ServerMessage::Output(b"more".to_vec())));
        relay.server_message(None);
        assert_eq!(relay.take_end(), Some(Ok(Outcome::ServerExited)));

        relay.notice_expired();
        assert_eq!(relay.notice_deadline(), None);
        assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop]");
    }

    #[test]
    fn an_error_right_before_the_connection_closes_ends_the_relay_with_it() {
        let (mut relay, _) = relay_on("desktop");
        relay.server_message(Some(ServerMessage::Error(
            "server desktop is offline".into(),
        )));
        relay.server_message(None);
        assert_eq!(
            relay.take_end(),
            Some(Err("server desktop is offline".to_owned()))
        );
    }

    #[test]
    fn detaching_and_exiting_end_the_relay() {
        for (message, outcome) in [
            (ServerMessage::Detached, Outcome::Detached),
            (ServerMessage::Exited, Outcome::Exited),
        ] {
            let (mut relay, _) = relay_on("laptop");
            relay.server_message(Some(message));
            assert!(relay.has_ended());
            assert_eq!(relay.take_end(), Some(Ok(outcome)));
        }
    }

    #[test]
    fn the_reconnecting_overlay_stays_until_the_session_is_attached_again() {
        let (mut relay, mut parser) = relay_on("desktop");
        relay.server_message(Some(state("work", 0)));
        relay.server_message(Some(ServerMessage::Reconnecting {
            server: "desktop".into(),
        }));
        draw(&mut relay, &mut parser);
        assert!(parser
            .screen()
            .contents()
            .contains("reconnecting to desktop…"));

        relay.resize(SIZE);
        draw(&mut relay, &mut parser);
        assert!(parser
            .screen()
            .contents()
            .contains("reconnecting to desktop…"));

        relay.input(b"\x02s");
        relay.server_message(Some(cluster()));
        relay.input(b"q");
        draw(&mut relay, &mut parser);
        let contents = parser.screen().contents();
        assert!(contents.contains("reconnecting to desktop…"), "{contents}");
        assert!(!contents.contains("this server"), "{contents}");

        relay.server_message(Some(ServerMessage::Attached(attached("work", "desktop"))));
        let repaint: Vec<u8> = (1..SIZE.rows)
            .flat_map(|row| format!("\x1b[{row};1H\x1b[2K").into_bytes())
            .collect();
        relay.server_message(Some(ServerMessage::Output(repaint)));
        draw(&mut relay, &mut parser);
        assert!(!parser.screen().contents().contains("reconnecting"));
    }

    #[test]
    fn the_label_follows_renames() {
        let (mut relay, _) = relay_on("desktop");
        relay.server_message(Some(state("notes", 0)));
        assert_eq!(relay.label(), "notes@desktop");
        relay.input(b"\x02$\r");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::RenameSession {
                    target: "notes@desktop".parse().unwrap(),
                    name: "notes".into(),
                },
                ClientMessage::Redraw,
            ]
        );
    }
}
