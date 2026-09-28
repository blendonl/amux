use std::fmt;
use std::mem;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};

use super::chrome::{
    detach_hint, draw_row, render_reconnecting, Panel, PanelEvent, Placement, Prompt,
    PromptPurpose, Rect, StatusLine, StatusSides, Style, WindowTab,
};
use super::router::{self, Action, KeyRouter};
use super::scripting::{ClientContext, Effect, Scripting, StatusContext};
use super::terminal;
use super::tree::Loading;
use crate::protocol::{
    AttachedSession, ClientMessage, ClusterStatus, ServerMessage, ServerView, SessionState, Size,
};
use crate::settings::{CallbackId, Keymap, Settings};
use crate::target::Target;

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

pub struct Relay {
    local: String,
    attached: AttachedSession,
    size: Size,
    settings: Arc<Settings>,
    keymap: Arc<Keymap>,
    router: KeyRouter,
    scripting: Option<Box<dyn Scripting>>,
    status_memo: Option<(StatusContext, StatusSides)>,
    status_error: Option<String>,
    detach_hint: Option<String>,
    session: Option<SessionState>,
    cluster: Option<ClusterStatus>,
    notice: Option<(String, Instant)>,
    reconnecting: Option<String>,
    panel: Option<Box<dyn Panel>>,
    escape_at: Option<Instant>,
    last_error: Option<String>,
    output: Vec<u8>,
    messages: Vec<ClientMessage>,
    chrome_dirty: bool,
    end: Option<Result<Outcome, String>>,
}

impl Relay {
    pub fn new(
        local: String,
        attached: AttachedSession,
        size: Size,
        settings: Arc<Settings>,
        keymap: Arc<Keymap>,
        scripting: Option<Box<dyn Scripting>>,
    ) -> Self {
        let router = KeyRouter::new(settings.prefix, Arc::clone(&keymap), settings.escape_time());
        let detach_hint = detach_hint(&keymap, &settings.prefix);
        Self {
            local,
            attached,
            size,
            settings,
            keymap,
            router,
            scripting,
            status_memo: None,
            status_error: None,
            detach_hint,
            session: None,
            cluster: None,
            notice: None,
            reconnecting: None,
            panel: None,
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
            Some(ServerMessage::Cluster(servers)) => self.cluster_listed(&servers),
            Some(ServerMessage::Reconnecting { server }) => {
                self.reconnecting = Some(server);
                self.chrome_dirty = true;
            }
            Some(ServerMessage::Detached) => self.end = Some(Ok(Outcome::Detached)),
            Some(ServerMessage::Exited) => self.end = Some(Ok(Outcome::Exited)),
            Some(ServerMessage::Error(message)) => {
                self.notify(message.clone());
                self.last_error = Some(message);
            }
            Some(other) => {
                self.end = Some(Err(format!("unexpected message from server: {other:?}")));
            }
            None => self.end = Some(error.map_or(Ok(Outcome::ServerExited), Err)),
        }
    }

    pub fn input(&mut self, chunk: &[u8]) {
        let mut input = chunk.to_vec();
        while !input.is_empty() {
            if self.panel.is_some() {
                self.panel_input(&input);
                return;
            }
            let (actions, rest) = self.router.route(&input);
            self.act(actions);
            input = rest;
        }
    }

    pub fn stdin_closed(&mut self) {
        let held = self.router.flush();
        if !held.is_empty() {
            self.messages.push(ClientMessage::Input(held));
        }
        self.messages.push(ClientMessage::Detach);
    }

    pub fn resize(&mut self, size: Size) {
        self.size = size;
        self.messages
            .push(ClientMessage::Resize(terminal::session_area(
                size,
                &self.settings.status,
            )));
        if self.reconnecting.is_some() {
            self.clear_above_status();
        }
        self.chrome_dirty = true;
    }

    pub fn escape_deadline(&self) -> Option<Instant> {
        [self.router.escape_deadline(), self.escape_at]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn escape_timeout(&mut self) {
        if self.router.escape_deadline().is_some() {
            let (actions, rest) = self.router.time_out();
            self.act(actions);
            self.input(&rest);
            return;
        }
        self.escape_at = None;
        self.drive_panel(|panel, attached| panel.time_out(attached));
    }

    pub fn notice_deadline(&self) -> Option<Instant> {
        self.notice.as_ref().map(|(_, until)| *until)
    }

    pub fn notice_expired(&mut self) {
        self.notice = None;
        self.chrome_dirty = true;
    }

    pub fn status_deadline(&self) -> Option<Instant> {
        let interval = self.settings.status.interval_ms;
        if interval == 0 || !self.runs_status_scripts() {
            return None;
        }
        let wait = interval - wall_clock_ms() % interval;
        Some(Instant::now() + Duration::from_millis(wait))
    }

    pub fn status_due(&mut self) {
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

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Forward(bytes) => self.messages.push(ClientMessage::Input(bytes)),
                Action::Detach => self.messages.push(ClientMessage::Detach),
                Action::Command(command) => self.messages.push(ClientMessage::Command(command)),
                Action::Open(panel) => self.open(panel),
                Action::Callback(id) => self.run_callback(id, None),
            }
        }
    }

    fn run_callback(&mut self, id: CallbackId, input: Option<&str>) {
        let context = self.client_context();
        let Some(scripting) = self.scripting.as_deref_mut() else {
            return;
        };
        match scripting.call(id, input, &context) {
            Ok(effects) => self.apply(effects),
            Err(error) => self.notify(error),
        }
    }

    fn apply(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Notify(message) => self.notify(message),
                Effect::Run(binding) => {
                    let actions = self.router.run_binding(binding);
                    self.act(actions);
                }
                Effect::SendKeys(bytes) => self.messages.push(ClientMessage::Input(bytes)),
                Effect::Prompt {
                    label,
                    initial,
                    submit,
                } => self.open_prompt(PromptPurpose::Callback(submit), label, &initial),
                Effect::Switch(target) => self.messages.push(ClientMessage::Switch(target)),
                Effect::Keymap(keymap) => self.set_keymap(keymap),
            }
        }
    }

    fn set_keymap(&mut self, keymap: Keymap) {
        self.keymap = Arc::new(keymap);
        self.detach_hint = detach_hint(&self.keymap, &self.settings.prefix);
        let held = self.router.set_keymap(Arc::clone(&self.keymap));
        if !held.is_empty() {
            self.messages.push(ClientMessage::Input(held));
        }
    }

    fn notify(&mut self, message: String) {
        self.notice = Some((message, Instant::now() + self.settings.notice_time()));
        self.chrome_dirty = true;
    }

    fn host_output(&mut self, bytes: &[u8]) {
        if self
            .panel
            .as_ref()
            .is_some_and(|panel| panel.hides_session())
        {
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

    fn open(&mut self, request: router::Panel) {
        match request {
            router::Panel::RenameWindow => {
                let name = self.active_window_name();
                self.open_prompt(PromptPurpose::RenameWindow, RENAME_WINDOW, &name);
            }
            router::Panel::RenameSession => {
                let name = self.session_name().to_owned();
                self.open_prompt(PromptPurpose::RenameSession, RENAME_SESSION, &name);
            }
            router::Panel::ClusterTree => {
                self.messages.push(ClientMessage::ListCluster);
                self.show(Box::new(Loading::new(
                    self.keymap.tree.clone(),
                    Arc::clone(&self.settings),
                )));
            }
        }
    }

    fn open_prompt(&mut self, purpose: PromptPurpose, label: impl Into<String>, initial: &str) {
        let prompt = Prompt::new(
            purpose,
            label,
            initial,
            self.keymap.prompt.clone(),
            &self.settings.theme,
        );
        self.show(Box::new(prompt));
    }

    fn show(&mut self, panel: Box<dyn Panel>) {
        self.panel = Some(panel);
        self.chrome_dirty = true;
    }

    fn cluster_listed(&mut self, servers: &[ServerView]) {
        self.drive_panel(|panel, attached| panel.cluster_listed(servers, attached));
    }

    fn panel_input(&mut self, input: &[u8]) {
        self.drive_panel(|panel, attached| panel.handle(input, attached));
        self.arm_escape();
    }

    fn drive_panel(&mut self, drive: impl FnOnce(&mut dyn Panel, &Target) -> PanelEvent) {
        let attached = self.attached_target();
        if let Some(panel) = self.panel.as_deref_mut() {
            let event = drive(panel, &attached);
            self.panel_event(event);
        }
    }

    fn panel_event(&mut self, event: PanelEvent) {
        match event {
            PanelEvent::Unchanged => {}
            PanelEvent::Pending => self.chrome_dirty = true,
            PanelEvent::Cancel => self.close_panel(),
            PanelEvent::Done(message) => {
                self.messages.push(message);
                self.close_panel();
            }
            PanelEvent::Callback(id, text) => {
                self.close_panel();
                self.run_callback(id, Some(&text));
            }
            PanelEvent::Replace(panel, input) => {
                self.show(panel);
                self.panel_input(&input);
            }
        }
    }

    fn close_panel(&mut self) {
        self.panel = None;
        self.escape_at = None;
        self.messages.push(ClientMessage::Redraw);
        if self.reconnecting.is_some() {
            self.clear_above_status();
        }
        self.chrome_dirty = true;
    }

    fn arm_escape(&mut self) {
        let partial = self.panel.as_ref().is_some_and(|panel| panel.is_partial());
        self.escape_at = partial.then(|| Instant::now() + self.settings.escape_time());
    }

    fn draw_chrome(&mut self) {
        let above = self.above_status();
        let status_row = self.status_row();
        if let Some(panel) = self.placed(Placement::SessionArea) {
            let drawn = panel.render(above);
            self.output.extend(drawn);
        } else if let Some(server) = &self.reconnecting {
            let area = Size {
                rows: above.rows,
                cols: above.cols,
            };
            self.output.extend(render_reconnecting(
                server,
                self.detach_hint.as_deref(),
                area,
                &self.settings.theme,
            ));
        }
        match self.placed(Placement::StatusRow) {
            Some(panel) => {
                let drawn = panel.render(status_row);
                self.output.extend(drawn);
            }
            None => self.draw_status(),
        }
    }

    fn draw_status(&mut self) {
        let sides = self.scripted_sides();
        let status = self.status_line();
        self.output.extend(status.render_scripted(
            self.size.cols,
            &self.settings.status,
            &self.settings.theme,
            &sides,
        ));
    }

    fn runs_status_scripts(&self) -> bool {
        self.scripting.is_some() && self.settings.status.is_scripted()
    }

    fn scripted_sides(&mut self) -> StatusSides {
        if self.notice.is_some() || !self.runs_status_scripts() {
            return StatusSides::default();
        }
        let interval = self.settings.status.interval_ms;
        let context = StatusContext {
            client: self.client_context(),
            width: self.size.cols,
            tick: (interval > 0).then(|| wall_clock_ms() / interval),
        };
        if let Some((memo, sides)) = &self.status_memo {
            if *memo == context {
                return sides.clone();
            }
        }
        let Some(scripting) = self.scripting.as_deref_mut() else {
            return StatusSides::default();
        };
        let status = &self.settings.status;
        let mut errors = Vec::new();
        let mut side = |name: &str, id: Option<CallbackId>| {
            let id = id?;
            scripting.status(id, &context).unwrap_or_else(|error| {
                errors.push(format!("status.{name}: {error}"));
                None
            })
        };
        let sides = StatusSides {
            left: side("left", status.left),
            right: side("right", status.right),
        };
        self.status_memo = Some((context, sides.clone()));
        let error = errors.into_iter().next();
        if error != self.status_error {
            if let Some(message) = error.clone() {
                self.notify(message);
            }
            self.status_error = error;
        }
        sides
    }

    fn placed(&mut self, placement: Placement) -> Option<&mut Box<dyn Panel>> {
        self.panel
            .as_mut()
            .filter(|panel| panel.placement() == Some(placement))
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
            rows: self.size.rows.saturating_sub(self.settings.status.rows()),
            cols: self.size.cols,
        }
    }

    fn status_row(&self) -> Rect {
        let above = self.above_status();
        Rect {
            row: above.rows,
            col: 0,
            rows: self.settings.status.rows(),
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

    fn client_context(&self) -> ClientContext {
        let cluster = self.cluster.as_ref();
        let host = &self.attached.server;
        ClientContext {
            session: self.session_name().to_owned(),
            server: host.clone(),
            local: cluster.map_or(&self.local, |status| &status.local).clone(),
            windows: self
                .session
                .as_ref()
                .map(|state| state.windows.clone())
                .unwrap_or_default(),
            active: self.session.as_ref().map(|state| state.active),
            latency: cluster
                .filter(|status| status.host == *host)
                .and_then(|status| status.latency),
            offline: cluster
                .map(|status| status.offline.clone())
                .unwrap_or_default(),
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

fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
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
        let status = relay.status_deadline();
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
            () = sleep_until(status.unwrap_or_else(Instant::now)), if status.is_some() => {
                relay.status_due();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::client::chrome::testing::{row_text, screen_text, terminal};
    use crate::identity::Incarnation;
    use crate::protocol::{
        Direction, ServerStatus, SessionCommand, SessionId, SessionInfo, WindowSummary,
    };
    use crate::settings::{Binding, Color, StyleSpec};

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
        let relay = Relay::new(
            "laptop".into(),
            attached("work", server),
            SIZE,
            Arc::new(Settings::default()),
            Arc::new(Keymap::default()),
            None,
        );
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
        let mut relay = Relay::new(
            "laptop".into(),
            attached("work", "desktop"),
            size,
            Arc::new(Settings::default()),
            Arc::new(Keymap::default()),
            None,
        );
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

    fn relay_with(settings: Settings, keymap: Keymap) -> Relay {
        Relay::new(
            "laptop".into(),
            attached("work", "laptop"),
            SIZE,
            Arc::new(settings),
            Arc::new(keymap),
            None,
        )
    }

    fn ctrl_a() -> Settings {
        Settings {
            prefix: "C-a".parse().unwrap(),
            ..Settings::default()
        }
    }

    fn with_root(notation: &str, binding: Binding) -> Keymap {
        let mut keymap = Keymap::default();
        keymap.root.insert(notation.parse().unwrap(), binding);
        keymap
    }

    #[test]
    fn the_prefix_setting_replaces_ctrl_b() {
        let mut relay = relay_with(ctrl_a(), Keymap::default());
        relay.input(b"\x02n\x01n\x01\x01\x01d");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Input(b"\x02n".to_vec()),
                ClientMessage::Command(SessionCommand::NextWindow),
                ClientMessage::Input(b"\x01".to_vec()),
                ClientMessage::Detach,
            ]
        );
    }

    #[test]
    fn a_root_binding_opens_a_panel_that_takes_the_rest_of_the_chunk() {
        let mut relay = relay_with(
            Settings::default(),
            with_root("M-r", Binding::RenameSession),
        );
        relay.input(b"a\x1br\x15notes\rignored");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Input(b"a".to_vec()),
                ClientMessage::RenameSession {
                    target: "work@laptop".parse().unwrap(),
                    name: "notes".into(),
                },
                ClientMessage::Redraw,
            ]
        );
    }

    #[test]
    fn a_held_escape_reaches_the_session_when_the_timeout_expires() {
        let mut relay = relay_with(
            Settings::default(),
            with_root("M-h", Binding::SelectPane(Direction::Left)),
        );
        relay.input(b"ls\x1b");
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Input(b"ls".to_vec())]
        );
        assert!(relay.escape_deadline().is_some());
        relay.escape_timeout();
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Input(b"\x1b".to_vec())]
        );
        assert_eq!(relay.escape_deadline(), None);

        relay.input(b"\x1b");
        relay.input(b"h");
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Command(SessionCommand::SelectPane(
                Direction::Left
            ))]
        );
    }

    #[test]
    fn closing_stdin_sends_held_bytes_before_detaching() {
        let mut relay = relay_with(
            Settings::default(),
            with_root("M-h", Binding::SelectPane(Direction::Left)),
        );
        relay.input(b"\x1b");
        relay.stdin_closed();
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Input(b"\x1b".to_vec()),
                ClientMessage::Detach
            ]
        );
    }

    #[test]
    fn the_reconnecting_hint_names_the_configured_prefix() {
        let mut relay = relay_with(ctrl_a(), Keymap::default());
        let mut parser = terminal(SIZE.rows, SIZE.cols);
        relay.server_message(Some(ServerMessage::Reconnecting {
            server: "laptop".into(),
        }));
        draw(&mut relay, &mut parser);
        assert!(parser.screen().contents().contains("Ctrl-a d detaches"));
    }

    #[test]
    fn the_theme_and_chrome_settings_reach_every_panel() {
        let mut settings = Settings::default();
        settings.status.window_format = "[{index}]{name} ".into();
        settings.theme.status.bg = Some(Color::Rgb(0x8e, 0xc0, 0x7c));
        settings.theme.prompt_label = StyleSpec {
            underline: Some(true),
            ..StyleSpec::EMPTY
        };
        settings.theme.tree_cursor = StyleSpec {
            fg: Some(Color::Indexed(11)),
            ..StyleSpec::EMPTY
        };
        settings.theme.overlay_text = StyleSpec {
            italic: Some(true),
            ..StyleSpec::EMPTY
        };
        settings.tree.expanded_marker = "v ".into();
        settings.tree.detail_gap = " | ".into();
        let mut relay = relay_with(settings, Keymap::default());
        let mut parser = terminal(SIZE.rows, SIZE.cols);

        relay.server_message(Some(state("work", 1)));
        assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop][0]sh [1]vim");
        assert_eq!(
            parser.screen().cell(BOTTOM, 39).unwrap().bgcolor(),
            vt100::Color::Rgb(0x8e, 0xc0, 0x7c)
        );

        relay.input(b"\x02,");
        assert_eq!(bottom(&mut relay, &mut parser), "rename window: vim");
        assert!(parser.screen().cell(BOTTOM, 0).unwrap().underline());
        assert!(!parser.screen().cell(BOTTOM, 15).unwrap().underline());
        relay.input(b"\x03");

        relay.input(b"\x02s");
        relay.server_message(Some(cluster()));
        draw(&mut relay, &mut parser);
        let screen = screen_text(&parser);
        assert_eq!(screen[0], "v laptop | (this server)");
        assert_eq!(screen[2], "    + work | 1 window");
        let cursor = parser.screen().cell(2, 6).unwrap();
        assert_eq!(cursor.fgcolor(), vt100::Color::Idx(11));
        assert!(!cursor.inverse());
        relay.input(b"q");

        relay.server_message(Some(ServerMessage::Reconnecting {
            server: "laptop".into(),
        }));
        draw(&mut relay, &mut parser);
        let title = parser.screen().cell(3, 8).unwrap();
        assert_eq!(title.contents(), "r");
        assert!(title.italic() && !title.bold());
    }

    mod scripted {
        use std::fs;
        use std::time::Instant as Clock;

        use super::*;
        use crate::lua::{self, ConfigPaths, LuaScripting, Process, INIT_FILE};

        fn scripted(source: &str) -> (Relay, vt100::Parser) {
            let dir = tempfile::tempdir().unwrap();
            let init = dir.path().join(INIT_FILE);
            fs::write(&init, source).unwrap();
            let paths = ConfigPaths::new(dir.path().to_owned(), Some(init));
            let loaded = lua::load(&paths, Process::Client).unwrap();
            let relay = Relay::new(
                "laptop".into(),
                attached("work", "laptop"),
                SIZE,
                Arc::new(loaded.settings.clone()),
                Arc::new(loaded.keymap.clone()),
                Some(Box::new(LuaScripting::new(loaded))),
            );
            (relay, terminal(SIZE.rows, SIZE.cols))
        }

        fn notice(relay: &Relay) -> String {
            relay
                .notice
                .as_ref()
                .map(|(message, _)| message.clone())
                .unwrap_or_default()
        }

        fn output_batch(relay: &mut Relay, parser: &mut vt100::Parser) {
            relay.server_message(Some(ServerMessage::Output(b"x".to_vec())));
            draw(relay, parser);
        }

        #[test]
        fn a_binding_callback_runs_actions_and_sends_keys_with_the_client_context() {
            let (mut relay, _) = scripted(
                "amux.keymap.set('prefix', 'g', function(ctx)\n\
                   assert(ctx.session == 'work' and ctx.server == 'laptop')\n\
                   assert(ctx.local_server == 'laptop' and ctx.panes == 1)\n\
                   assert(ctx.window_index == 1 and ctx.window_name == 'vim')\n\
                   assert(#ctx.windows == 2 and ctx.windows[2].active)\n\
                   assert(amux.state() == ctx)\n\
                   assert(not pcall(function() ctx.session = 'x' end))\n\
                   assert(not pcall(function() ctx.windows[1].name = 'x' end))\n\
                   amux.run(amux.action.new_window())\n\
                   amux.send_keys('git status\\r')\n\
                 end)",
            );
            relay.server_message(Some(state("work", 1)));
            relay.input(b"a\x02gb");
            assert_eq!(notice(&relay), "");
            assert_eq!(
                relay.take_messages(),
                vec![
                    ClientMessage::Input(b"a".to_vec()),
                    ClientMessage::Command(SessionCommand::NewWindow),
                    ClientMessage::Input(b"git status\r".to_vec()),
                    ClientMessage::Input(b"b".to_vec()),
                ]
            );
        }

        #[test]
        fn a_notice_from_lua_shows_on_the_status_line() {
            let (mut relay, mut parser) = scripted(
                "amux.keymap.set('prefix', 'm', function(ctx)\n\
                   amux.notify('hello ' .. ctx.session)\n\
                 end)",
            );
            relay.input(b"\x02m");
            assert_eq!(bottom(&mut relay, &mut parser), "hello work");
            assert!(relay.notice_deadline().is_some());
            assert_eq!(relay.take_messages(), Vec::new());

            relay.notice_expired();
            assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop]");
        }

        #[test]
        fn a_lua_prompt_submits_its_text_to_on_submit() {
            let (mut relay, mut parser) = scripted(
                "amux.keymap.set('prefix', 'f', function(ctx)\n\
                   amux.prompt {\n\
                     label = 'find',\n\
                     initial = ctx.window_name,\n\
                     on_submit = function(text, ctx)\n\
                       amux.send_keys('find ' .. text .. ' in ' .. ctx.session .. '\\r')\n\
                       amux.switch(text .. '@laptop')\n\
                     end,\n\
                   }\n\
                 end)",
            );
            relay.server_message(Some(state("work", 1)));
            relay.input(b"\x02f");
            assert_eq!(bottom(&mut relay, &mut parser), "find: vim");

            relay.input(b"ed\rignored");
            assert_eq!(
                relay.take_messages(),
                vec![
                    ClientMessage::Redraw,
                    ClientMessage::Input(b"find vimed in work\r".to_vec()),
                    ClientMessage::Switch("vimed@laptop".parse().unwrap()),
                ]
            );
            relay.input(b"x");
            assert_eq!(
                relay.take_messages(),
                vec![ClientMessage::Input(b"x".to_vec())]
            );
        }

        #[test]
        fn a_runtime_keymap_change_applies_to_the_next_key() {
            let (mut relay, mut parser) = scripted(
                "amux.keymap.set('prefix', 'k', function()\n\
                   amux.keymap.set('root', 'M-x', amux.action.next_window())\n\
                   amux.keymap.set('prefix', 'j', function() amux.notify('jumped') end)\n\
                   amux.keymap.del('prefix', 'n')\n\
                 end)",
            );
            relay.input(b"\x1bx");
            assert_eq!(
                relay.take_messages(),
                vec![ClientMessage::Input(b"\x1bx".to_vec())]
            );

            relay.input(b"\x02k\x1bx");
            assert_eq!(
                relay.take_messages(),
                vec![ClientMessage::Command(SessionCommand::NextWindow)]
            );
            relay.input(b"\x02n\x02j");
            assert_eq!(relay.take_messages(), Vec::new());
            assert_eq!(bottom(&mut relay, &mut parser), "jumped");
        }

        #[test]
        fn a_lua_error_shows_a_notice_and_changes_nothing() {
            let (mut relay, _) = scripted(
                "amux.keymap.set('prefix', 'e', function()\n\
                   amux.keymap.set('root', 'M-z', 'detach')\n\
                   amux.run(amux.action.new_window())\n\
                   error('boom')\n\
                 end)",
            );
            relay.input(b"\x02e");
            assert!(
                notice(&relay).ends_with("init.lua:4: boom"),
                "{}",
                notice(&relay)
            );
            assert!(!relay.has_ended());
            assert_eq!(relay.take_messages(), Vec::new());

            relay.input(b"\x1bz");
            assert_eq!(
                relay.take_messages(),
                vec![ClientMessage::Input(b"\x1bz".to_vec())]
            );
        }

        #[test]
        fn a_runaway_binding_is_cut_off_by_its_budget() {
            let (mut relay, _) = scripted(
                "amux.keymap.set('prefix', 'l', function()\n\
                   amux.send_keys('never')\n\
                   while true do end\n\
                 end)",
            );
            let started = Clock::now();
            relay.input(b"\x02lx");
            let elapsed = started.elapsed();
            assert!(elapsed >= Duration::from_millis(900), "{elapsed:?}");
            assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
            assert!(
                notice(&relay).ends_with("init.lua:3: a key binding ran past its 1000 ms budget"),
                "{}",
                notice(&relay)
            );
            assert_eq!(
                relay.take_messages(),
                vec![ClientMessage::Input(b"x".to_vec())]
            );
        }

        #[test]
        fn status_functions_replace_the_session_tag_and_the_right_side() {
            let (mut relay, mut parser) = scripted(
                "amux.opt.status.left = function(ctx)\n\
                   return { { text = '<' .. ctx.session .. '>', style = { fg = 'blue', bold = true } } }\n\
                 end\n\
                 amux.opt.status.right = function(ctx) return ' ' .. ctx.width .. ' cols ' end",
            );
            relay.server_message(Some(state("work", 1)));
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<32}40 cols", "<work> 0:sh  1:vim")
            );
            let tag = parser.screen().cell(BOTTOM, 0).unwrap();
            assert_eq!(tag.fgcolor(), vt100::Color::Idx(4));
            assert_eq!(tag.bgcolor(), vt100::Color::Idx(2));
            assert!(tag.bold());
            assert_eq!(relay.status_deadline(), None);
        }

        #[test]
        fn a_status_function_can_fall_back_to_the_built_in_segment() {
            let (mut relay, mut parser) = scripted(
                "amux.opt.status.left = function(ctx) return nil end\n\
                 amux.opt.status.right = function(ctx) return { { text = ' a very long right side' } } end",
            );
            relay.server_message(Some(state("work", 1)));
            assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop] 0:sh  1:vim");
        }

        #[test]
        fn a_failing_status_function_shows_its_error_once_and_falls_back() {
            let (mut relay, mut parser) = scripted(
                "amux.opt.status.right = function(ctx) while true do end end\n\
                 amux.opt.status.left = function(ctx) return 5 end",
            );
            relay.server_message(Some(state("work", 1)));
            draw(&mut relay, &mut parser);
            assert_eq!(
                notice(&relay),
                "status.left: a status function returns a string or a list of spans, not a integer"
            );
            relay.notice_expired();
            assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop] 0:sh  1:vim");

            relay.server_message(Some(state("work", 0)));
            draw(&mut relay, &mut parser);
            assert_eq!(relay.notice_deadline(), None);
        }

        #[test]
        fn a_runaway_status_function_is_cut_off_by_its_budget() {
            let (mut relay, mut parser) =
                scripted("amux.opt.status.right = function(ctx) while true do end end");
            let started = Clock::now();
            draw(&mut relay, &mut parser);
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(
                notice(&relay).ends_with("a status function ran past its 50 ms budget"),
                "{}",
                notice(&relay)
            );
            relay.notice_expired();
            assert_eq!(bottom(&mut relay, &mut parser), "[work@laptop]");
        }

        #[test]
        fn status_functions_run_only_when_their_context_changes() {
            let (mut relay, mut parser) = scripted(
                "local calls = 0\n\
                 amux.opt.status.right = function(ctx)\n\
                   calls = calls + 1\n\
                   return ' #' .. calls .. ' '\n\
                 end",
            );
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<37}#1", "[work@laptop]")
            );
            for _ in 0..50 {
                output_batch(&mut relay, &mut parser);
            }
            assert_eq!(
                row_text(&parser, BOTTOM),
                format!("{:<37}#1", "[work@laptop]")
            );

            relay.server_message(Some(state("work", 1)));
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<37}#2", "[work@laptop] 0:sh  1:vim")
            );
            relay.server_message(Some(ServerMessage::ClusterStatus(ClusterStatus {
                local: "laptop".into(),
                host: "laptop".into(),
                latency: None,
                offline: vec!["attic".into()],
            })));
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<37}#3", "[work@laptop] 0:sh  1:vim")
            );

            relay.input(b"\x02,");
            output_batch(&mut relay, &mut parser);
            relay.input(b"\x03");
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<37}#3", "[work@laptop] 0:sh  1:vim")
            );
        }

        #[test]
        fn an_interval_redraws_the_status_on_each_tick() {
            let (mut relay, mut parser) = scripted(
                "local calls = 0\n\
                 amux.opt.status.interval_ms = 30\n\
                 amux.opt.status.right = function(ctx)\n\
                   calls = calls + 1\n\
                   return ' #' .. calls .. ' '\n\
                 end",
            );
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<37}#1", "[work@laptop]")
            );
            assert!(relay.take_output().is_empty());

            let deadline = relay.status_deadline().unwrap();
            let wait = deadline - Instant::now();
            assert!(wait <= Duration::from_millis(30), "{wait:?}");
            std::thread::sleep(wait + Duration::from_millis(5));
            relay.status_due();
            assert_eq!(
                bottom(&mut relay, &mut parser),
                format!("{:<37}#2", "[work@laptop]")
            );

            let (relay, _) = scripted("amux.opt.status.interval_ms = 30");
            assert_eq!(relay.status_deadline(), None);
            let mut settings = Settings::default();
            settings.status.interval_ms = 30;
            settings.status.right = Some(CallbackId(0));
            assert_eq!(
                relay_with(settings, Keymap::default()).status_deadline(),
                None
            );
        }
    }
}
