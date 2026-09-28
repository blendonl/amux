use std::fmt;
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Result};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};

use super::chrome::{
    detach_hint, draw_row, render_reconnecting, Panel, PanelEvent, Placement, Prompt,
    PromptPurpose, Rect, StatusLine, StatusSides, Style, WhichKey, WindowTab,
};
use super::router::{self, Action, KeyRouter, Level};
use super::scripting::{ClientContext, Effect, Scripting, StatusContext};
use super::terminal;
use super::tree::Loading;
use super::{ClientConfig, Endpoint};
use crate::protocol::{
    AttachedSession, ClientMessage, ClusterStatus, ServerMessage, ServerView, SessionState, Size,
};
use crate::settings::{CallbackId, Keymap, Settings};
use crate::target::Target;

pub const RELOADED: &str = "config reloaded";
const DRAIN_LIMIT: usize = 64;
const RELOAD_TIMEOUT: Duration = Duration::from_secs(10);
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
    prompt_callback: Option<CallbackId>,
    escape_at: Option<Instant>,
    which_key: Option<WhichKey>,
    which_key_levels: Vec<Level>,
    which_key_at: Option<Instant>,
    last_error: Option<String>,
    reload_requested: bool,
    reloading: Option<Result<(), String>>,
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
            prompt_callback: None,
            escape_at: None,
            which_key: None,
            which_key_levels: Vec::new(),
            which_key_at: None,
            last_error: None,
            reload_requested: false,
            reloading: None,
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

    pub fn which_key_deadline(&self) -> Option<Instant> {
        self.which_key_at
    }

    pub fn which_key_due(&mut self) {
        if self.which_key_at.take().is_some() {
            self.show_keys();
        }
    }

    pub fn notice_deadline(&self) -> Option<Instant> {
        self.notice.as_ref().map(|(_, until)| *until)
    }

    pub fn notice_expired(&mut self) {
        self.notice = None;
        if !self.settings.status.enabled {
            self.messages.push(ClientMessage::Redraw);
        }
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

    pub fn take_reload(&mut self) -> bool {
        mem::take(&mut self.reload_requested)
    }

    pub fn client_reloaded(&mut self, config: Result<ClientConfig, String>) {
        self.reloading = Some(config.map(|config| self.reconfigure(config)));
    }

    pub fn server_reloaded(&mut self, server: Result<Option<String>, String>) {
        let client = self.reloading.take().unwrap_or(Ok(()));
        self.notify(reload_notice(client, server));
    }

    fn reconfigure(&mut self, config: ClientConfig) {
        if self.panel.is_some() {
            self.close_panel();
        }
        self.hide_keys();
        let ClientConfig {
            settings,
            keymap,
            scripting,
        } = config;
        let held = self.router.flush();
        if !held.is_empty() {
            self.messages.push(ClientMessage::Input(held));
        }
        let rows = self.settings.status.rows();
        self.router = KeyRouter::new(settings.prefix, Arc::clone(&keymap), settings.escape_time());
        self.detach_hint = detach_hint(&keymap, &settings.prefix);
        self.settings = settings;
        self.keymap = keymap;
        self.scripting = Some(scripting);
        self.status_memo = None;
        self.status_error = None;
        if self.settings.status.rows() != rows {
            self.resize(self.size);
            self.messages.push(ClientMessage::Redraw);
        }
        self.chrome_dirty = true;
    }

    fn act(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Forward(bytes) => self.messages.push(ClientMessage::Input(bytes)),
                Action::Detach => self.messages.push(ClientMessage::Detach),
                Action::Command(command) => self.messages.push(ClientMessage::Command(command)),
                Action::Open(panel) => self.open(panel),
                Action::Callback(id) => self.run_callback(id, None),
                Action::ReloadConfig => self.reload_requested = self.reloading.is_none(),
                Action::ShowKeys => self.show_keys(),
                Action::TurnPage(page) => {
                    if let Some(which_key) = self.which_key.as_mut() {
                        which_key.turn(page);
                        self.chrome_dirty = true;
                    }
                }
            }
        }
        self.follow_keys();
    }

    fn follow_keys(&mut self) {
        let levels = self.router.pending();
        if levels.is_empty() {
            self.hide_keys();
            return;
        }
        if levels == self.which_key_levels.as_slice() {
            return;
        }
        self.which_key_levels = levels.to_vec();
        if self.which_key.is_some() {
            self.show_keys();
            return;
        }
        let which_key = &self.settings.which_key;
        self.which_key_at = which_key
            .enabled
            .then(|| Instant::now() + which_key.delay());
    }

    fn show_keys(&mut self) {
        self.which_key_at = None;
        self.which_key_levels = self.router.pending().to_vec();
        if self.which_key_levels.is_empty() {
            self.hide_keys();
            return;
        }
        if self.which_key.is_some() {
            self.messages.push(ClientMessage::Redraw);
        }
        self.which_key = Some(WhichKey::new(
            &self.keymap,
            &self.which_key_levels,
            self.settings.prefix,
            &self.settings.which_key,
            &self.settings.theme,
        ));
        self.router.set_showing_keys(true);
        self.chrome_dirty = true;
    }

    fn hide_keys(&mut self) {
        self.which_key_levels.clear();
        self.which_key_at = None;
        if self.which_key.take().is_none() {
            return;
        }
        self.router.set_showing_keys(false);
        self.messages.push(ClientMessage::Redraw);
        if self.reconnecting.is_some() {
            self.clear_above_status();
        }
        self.chrome_dirty = true;
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
        if self.which_key.is_some() {
            self.show_keys();
        } else {
            self.follow_keys();
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
        let callback = match purpose {
            PromptPurpose::Callback(id) => Some(id),
            PromptPurpose::RenameWindow | PromptPurpose::RenameSession => None,
        };
        let prompt = Prompt::new(
            purpose,
            label,
            initial,
            self.keymap.prompt.clone(),
            &self.settings.theme,
        );
        self.show(Box::new(prompt));
        self.prompt_callback = callback;
    }

    fn show(&mut self, panel: Box<dyn Panel>) {
        self.release_prompt_callback();
        self.panel = Some(panel);
        self.chrome_dirty = true;
    }

    fn release_prompt_callback(&mut self) {
        if let Some(id) = self.prompt_callback.take() {
            self.release_callback(id);
        }
    }

    fn release_callback(&mut self, id: CallbackId) {
        if let Some(scripting) = self.scripting.as_deref_mut() {
            scripting.release(id);
        }
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
                self.prompt_callback = None;
                self.close_panel();
                self.run_callback(id, Some(&text));
                self.release_callback(id);
            }
            PanelEvent::Replace(panel, input) => {
                self.show(panel);
                self.panel_input(&input);
            }
        }
    }

    fn close_panel(&mut self) {
        self.release_prompt_callback();
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
        let shows_status = self.settings.status.enabled || self.notice.is_some();
        if let Some(panel) = self.placed(Placement::SessionArea) {
            let drawn = panel.render(above);
            self.output.extend(drawn);
        } else {
            if let Some(server) = &self.reconnecting {
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
            let area = self.which_key_area();
            if let Some(which_key) = self.which_key.as_mut() {
                let drawn = which_key.render(area);
                self.output.extend(drawn);
            }
        }
        match self.placed(Placement::StatusRow) {
            Some(panel) => {
                let drawn = panel.render(status_row);
                self.output.extend(drawn);
            }
            None if shows_status => self.draw_status(),
            None => {}
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

    fn which_key_area(&self) -> Rect {
        let mut area = self.above_status();
        let borrows_last_row = self.notice.is_some()
            || self
                .panel
                .as_ref()
                .is_some_and(|panel| panel.placement() == Some(Placement::StatusRow));
        if !self.settings.status.enabled && borrows_last_row {
            area.rows = area.rows.saturating_sub(1);
        }
        area
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
        Rect {
            row: self.size.rows.saturating_sub(1),
            col: 0,
            rows: 1,
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

fn reload_notice(client: Result<(), String>, server: Result<Option<String>, String>) -> String {
    match (client, server) {
        (Ok(()), Ok(None)) => RELOADED.to_owned(),
        (Ok(()), Ok(Some(notice))) => format!("{RELOADED}; {notice}"),
        (Ok(()), Err(error)) => format!("{RELOADED}, but not on the server: {error}"),
        (Err(error), Ok(_)) => format!("{error} (the server reloaded)"),
        (Err(client), Err(server)) if client == server => client,
        (Err(client), Err(server)) => format!("{client}; the server: {server}"),
    }
}

async fn reload_server(socket: PathBuf) -> Result<Option<String>, String> {
    match tokio::time::timeout(RELOAD_TIMEOUT, super::reload_server(&socket)).await {
        Ok(reply) => reply.map_err(|error| format!("{error:#}")),
        Err(_) => Err("the server did not answer".to_owned()),
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
    endpoint: &Endpoint,
) -> Result<Outcome> {
    let mut stdin = terminal::stdin_chunks();
    let mut resizes = signal(SignalKind::window_change())?;
    let mut stdin_open = true;
    let (reloaded, mut reloads) = mpsc::channel(1);

    loop {
        let output = relay.take_output();
        if !output.is_empty() {
            terminal::write_output(&output)?;
        }
        if let Some(end) = relay.take_end() {
            return end.map_err(|message| anyhow!(message));
        }
        if relay.take_reload() {
            relay.client_reloaded(
                ClientConfig::load(endpoint).map_err(|error| format!("{error:#}")),
            );
            let (socket, reloaded) = (endpoint.socket.clone(), reloaded.clone());
            tokio::spawn(async move {
                let _ = reloaded.send(reload_server(socket).await).await;
            });
            continue;
        }
        for message in relay.take_messages() {
            super::send(&outgoing, message).await?;
        }

        let escape = relay.escape_deadline();
        let which_key = relay.which_key_deadline();
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
            Some(reply) = reloads.recv() => relay.server_reloaded(reply),
            () = sleep_until(escape.unwrap_or_else(Instant::now)), if escape.is_some() => {
                relay.escape_timeout();
            }
            () = sleep_until(which_key.unwrap_or_else(Instant::now)), if which_key.is_some() => {
                relay.which_key_due();
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
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::client::chrome::testing::{row_text, screen_text, terminal};
    use crate::client::scripting::StatusSpan;
    use crate::identity::Incarnation;
    use crate::protocol::{
        Direction, ServerStatus, SessionCommand, SessionId, SessionInfo, WindowSummary,
    };
    use crate::settings::{Binding, Color, StyleSpec, PREFIX_TABLE};

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

    #[derive(Debug, PartialEq, Eq)]
    enum Scripted {
        Called(usize),
        Released(usize),
    }

    struct Prompting {
        log: Rc<RefCell<Vec<Scripted>>>,
        next: usize,
    }

    impl Prompting {
        fn prompt(&mut self) -> Effect {
            self.next += 1;
            Effect::Prompt {
                label: "find".into(),
                initial: String::new(),
                submit: CallbackId(self.next),
            }
        }
    }

    impl Scripting for Prompting {
        fn call(
            &mut self,
            id: CallbackId,
            input: Option<&str>,
            _: &ClientContext,
        ) -> Result<Vec<Effect>, String> {
            self.log.borrow_mut().push(Scripted::Called(id.0));
            Ok(match input {
                None | Some("again") => vec![self.prompt()],
                Some("twice") => vec![self.prompt(), self.prompt()],
                Some(_) => Vec::new(),
            })
        }

        fn status(
            &mut self,
            _: CallbackId,
            _: &StatusContext,
        ) -> Result<Option<Vec<StatusSpan>>, String> {
            Ok(None)
        }

        fn release(&mut self, id: CallbackId) {
            self.log.borrow_mut().push(Scripted::Released(id.0));
        }
    }

    #[test]
    fn a_prompt_callback_is_released_once_its_prompt_closes() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let scripting = Prompting {
            log: Rc::clone(&log),
            next: 1,
        };
        let mut keymap = Keymap::default();
        keymap
            .prefix
            .insert("g".parse().unwrap(), Binding::Callback(CallbackId(1)));
        let mut relay = Relay::new(
            "laptop".into(),
            attached("work", "laptop"),
            SIZE,
            Arc::new(Settings::default()),
            Arc::new(keymap),
            Some(Box::new(scripting)),
        );
        let take = || mem::take(&mut *log.borrow_mut());

        relay.input(b"\x02g\x03");
        assert_eq!(take(), [Scripted::Called(1), Scripted::Released(2)]);

        relay.input(b"\x02gagain\r");
        assert_eq!(
            take(),
            [
                Scripted::Called(1),
                Scripted::Called(3),
                Scripted::Released(3)
            ]
        );
        relay.input(b"done\r");
        assert_eq!(take(), [Scripted::Called(4), Scripted::Released(4)]);

        relay.input(b"\x02gtwice\r");
        assert_eq!(
            take(),
            [
                Scripted::Called(1),
                Scripted::Called(5),
                Scripted::Released(6),
                Scripted::Released(5)
            ]
        );
        relay.input(b"\x03");
        assert_eq!(take(), [Scripted::Released(7)]);
    }

    #[test]
    fn without_the_status_bar_notices_and_prompts_borrow_the_last_row() {
        let mut settings = Settings::default();
        settings.status.enabled = false;
        let mut relay = relay_with(settings, Keymap::default());
        let mut parser = terminal(SIZE.rows, SIZE.cols);
        relay.server_message(Some(state("work", 1)));
        let last_row = format!("\x1b[{};1Hbottom\x1b[1;1H", SIZE.rows).into_bytes();
        relay.server_message(Some(ServerMessage::Output(last_row.clone())));
        assert_eq!(relay.take_output(), last_row);
        parser.process(&last_row);
        assert_eq!(row_text(&parser, BOTTOM), "bottom");

        relay.resize(SIZE);
        assert_eq!(relay.take_messages(), vec![ClientMessage::Resize(SIZE)]);
        assert!(relay.take_output().is_empty());

        relay.server_message(Some(ServerMessage::Error("boom".into())));
        assert_eq!(bottom(&mut relay, &mut parser), "boom");
        assert_eq!(parser.screen().cursor_position(), (0, 0));
        relay.notice_expired();
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
        assert!(relay.take_output().is_empty());

        relay.input(b"\x02,");
        assert_eq!(bottom(&mut relay, &mut parser), "rename window: vim");
        relay.input(b"\x03");
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
        assert!(relay.take_output().is_empty());
    }

    #[test]
    fn the_reload_notice_names_what_did_not_reload() {
        assert_eq!(reload_notice(Ok(()), Ok(None)), "config reloaded");
        assert_eq!(
            reload_notice(
                Ok(()),
                Ok(Some("restart required for amux.opt.name".into()))
            ),
            "config reloaded; restart required for amux.opt.name"
        );
        assert_eq!(
            reload_notice(Ok(()), Err("the server did not answer".into())),
            "config reloaded, but not on the server: the server did not answer"
        );
        assert_eq!(
            reload_notice(Err("init.lua:1: boom".into()), Ok(None)),
            "init.lua:1: boom (the server reloaded)"
        );
        assert_eq!(
            reload_notice(
                Err("init.lua:1: boom".into()),
                Err("init.lua:1: boom".into())
            ),
            "init.lua:1: boom"
        );
        assert_eq!(
            reload_notice(
                Err("init.lua:1: boom".into()),
                Err("init.lua:2: bang".into())
            ),
            "init.lua:1: boom; the server: init.lua:2: bang"
        );
    }

    fn which_key_keymap() -> Keymap {
        let mut keymap = Keymap {
            prefix: [
                ("c", Binding::NewWindow),
                ("r", Binding::SwitchTable("resize".into())),
                ("?", Binding::WhichKey(PREFIX_TABLE.into())),
            ]
            .into_iter()
            .map(|(notation, binding)| (notation.parse().unwrap(), binding))
            .collect(),
            ..Keymap::default()
        };
        keymap.custom.insert(
            "resize".into(),
            [("h".parse().unwrap(), Binding::SelectPane(Direction::Left))]
                .into_iter()
                .collect(),
        );
        keymap
    }

    fn which_key_relay(settings: Settings) -> (Relay, vt100::Parser) {
        (
            relay_with(settings, which_key_keymap()),
            terminal(SIZE.rows, SIZE.cols),
        )
    }

    fn rows(relay: &mut Relay, parser: &mut vt100::Parser) -> Vec<String> {
        draw(relay, parser);
        screen_text(parser)
    }

    fn prefix_rule() -> String {
        format!("─ C-b {}", "─".repeat(34))
    }

    #[test]
    fn the_keys_show_after_the_delay_and_the_next_key_runs_and_closes_them() {
        let (mut relay, mut parser) = which_key_relay(Settings::default());
        let before = Instant::now();
        relay.input(b"\x02");
        let deadline = relay.which_key_deadline().unwrap();
        assert!(deadline >= before + Duration::from_millis(500));
        assert!(!rows(&mut relay, &mut parser)
            .iter()
            .any(|row| row.starts_with('─')));

        relay.which_key_due();
        assert_eq!(relay.which_key_deadline(), None);
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[4], prefix_rule());
        assert_eq!(
            screen[5..9],
            [
                " c   → new window",
                " r   → +resize",
                " ?   → show prefix keys",
                " C-b → send prefix",
            ]
        );
        assert_eq!(screen[9], "[work@laptop]");
        assert_eq!(relay.take_messages(), vec![]);

        relay.input(b"c");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Command(SessionCommand::NewWindow),
                ClientMessage::Redraw,
            ]
        );
        assert!(relay.which_key.is_none());
        relay.which_key_due();
        assert!(relay.which_key.is_none());
        assert_eq!(relay.take_messages(), vec![]);
    }

    #[test]
    fn a_key_before_the_delay_never_shows_the_keys() {
        let (mut relay, _) = which_key_relay(Settings::default());
        relay.input(b"\x02");
        assert!(relay.which_key_deadline().is_some());
        relay.input(b"c");
        assert_eq!(relay.which_key_deadline(), None);
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Command(SessionCommand::NewWindow)]
        );
    }

    #[test]
    fn turned_off_the_keys_show_only_when_asked_for() {
        let mut settings = Settings::default();
        settings.which_key.enabled = false;
        let (mut relay, mut parser) = which_key_relay(settings);
        relay.input(b"\x02");
        assert_eq!(relay.which_key_deadline(), None);
        relay.input(b"?");
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[4], prefix_rule());
        assert_eq!(screen[5], " c   → new window");

        relay.input(b"\x02");
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Input(vec![0x02]), ClientMessage::Redraw]
        );
        assert!(relay.which_key.is_none());
    }

    #[test]
    fn a_group_opens_in_place_and_backspace_goes_back() {
        let (mut relay, mut parser) = which_key_relay(Settings::default());
        relay.input(b"\x02?");
        rows(&mut relay, &mut parser);

        relay.input(b"r");
        assert_eq!(relay.which_key_deadline(), None);
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[7], format!("─ C-b r {}", "─".repeat(32)));
        assert_eq!(screen[8], " h → pane left");

        relay.input(b"\x7f");
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
        assert_eq!(rows(&mut relay, &mut parser)[4], prefix_rule());

        relay.input(b"rh");
        assert_eq!(
            relay.take_messages(),
            vec![
                ClientMessage::Command(SessionCommand::SelectPane(Direction::Left)),
                ClientMessage::Redraw,
            ]
        );
    }

    #[test]
    fn page_keys_turn_the_pages_of_a_long_table() {
        let (mut relay, mut parser) = relay_on("laptop");
        let rule = |page: usize| format!("─ C-b {} {page}/4 ─", "─".repeat(28));
        relay.input(b"\x02?");
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[0], rule(1));
        assert_eq!(screen[1], " 0     → window 0");

        relay.input(b"\x1b[6~");
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[0], rule(2));
        assert_eq!(screen[1], " 8     → window 8");

        relay.input(b"\x1b[5~\x1b[5~");
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[0], rule(4));
        assert_eq!(screen[1], " Up    → pane up");
        assert_eq!(relay.take_messages(), vec![]);

        relay.input(b"d");
        assert_eq!(
            relay.take_messages(),
            vec![ClientMessage::Detach, ClientMessage::Redraw]
        );
    }

    #[test]
    fn output_under_the_keys_is_covered_again() {
        let (mut relay, mut parser) = which_key_relay(Settings::default());
        relay.input(b"\x02?");
        rows(&mut relay, &mut parser);
        relay.server_message(Some(ServerMessage::Output(
            b"\x1b[6;1Hoverwritten".to_vec(),
        )));
        assert_eq!(rows(&mut relay, &mut parser)[5], " c   → new window");
    }

    #[test]
    fn escape_closes_the_keys_once_the_escape_time_passes() {
        let (mut relay, _) = which_key_relay(Settings::default());
        relay.input(b"\x02?");
        relay.input(b"\x1b");
        assert!(relay.escape_deadline().is_some());
        assert!(relay.which_key.is_some());
        relay.escape_timeout();
        assert!(relay.which_key.is_none());
        assert_eq!(relay.escape_deadline(), None);
        assert_eq!(relay.take_messages(), vec![ClientMessage::Redraw]);
    }

    #[test]
    fn without_the_status_bar_the_keys_stay_above_a_notice() {
        let mut settings = Settings::default();
        settings.status.enabled = false;
        let (mut relay, mut parser) = which_key_relay(settings);
        relay.input(b"\x02?");
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[5], prefix_rule());
        assert_eq!(screen[9], " C-b → send prefix");

        relay.server_message(Some(ServerMessage::Error("no room".into())));
        let screen = rows(&mut relay, &mut parser);
        assert_eq!(screen[4], prefix_rule());
        assert_eq!(screen[8], " C-b → send prefix");
        assert_eq!(screen[9], "no room");
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

        fn config(source: &str) -> ClientConfig {
            let dir = tempfile::tempdir().unwrap();
            let init = dir.path().join(INIT_FILE);
            fs::write(&init, source).unwrap();
            let paths = ConfigPaths::new(dir.path().to_owned(), Some(init));
            let loaded = lua::load(&paths, Process::Client).unwrap();
            ClientConfig {
                settings: Arc::new(loaded.settings.clone()),
                keymap: Arc::new(loaded.keymap.clone()),
                scripting: Box::new(LuaScripting::new(loaded)),
            }
        }

        fn scripted(source: &str) -> (Relay, vt100::Parser) {
            let ClientConfig {
                settings,
                keymap,
                scripting,
            } = config(source);
            let relay = Relay::new(
                "laptop".into(),
                attached("work", "laptop"),
                SIZE,
                settings,
                keymap,
                Some(scripting),
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
        fn lua_descriptions_and_delay_reach_the_keys_and_a_reload_closes_them() {
            let (mut relay, mut parser) = scripted(
                "amux.opt.which_key.delay_ms = 20\n\
                 amux.keymap.clear('prefix')\n\
                 amux.keymap.set('prefix', 'g', function() end, { desc = 'show the log' })",
            );
            let before = Instant::now();
            relay.input(b"\x02");
            let deadline = relay.which_key_deadline().unwrap();
            assert!(deadline >= before + Duration::from_millis(20));
            assert!(deadline <= Instant::now() + Duration::from_millis(20));
            relay.which_key_due();
            draw(&mut relay, &mut parser);
            let screen = screen_text(&parser);
            assert_eq!(screen[6], format!("─ C-b {}", "─".repeat(34)));
            assert_eq!(screen[7..9], [" g   → show the log", " C-b → send prefix"]);

            relay.client_reloaded(Ok(config("")));
            assert!(relay.which_key.is_none());
            assert_eq!(relay.router.pending(), []);
            assert!(relay.take_messages().contains(&ClientMessage::Redraw));
            relay.input(b"\x02d");
            assert_eq!(relay.take_messages(), vec![ClientMessage::Detach]);
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
        fn a_reload_swaps_the_prefix_the_bindings_and_the_status() {
            let (mut relay, mut parser) = scripted(
                "amux.keymap.set('prefix', 'g', function() amux.notify('old binding') end)",
            );
            relay.server_message(Some(state("work", 1)));
            relay.input(b"\x02r");
            assert!(relay.take_reload());
            assert!(!relay.take_reload());

            relay.client_reloaded(Ok(config(
                "amux.opt.prefix = 'C-a'\n\
                 amux.opt.status.session_format = '<{session}>'\n\
                 amux.keymap.set('prefix', 'g', function() amux.notify('new binding') end)",
            )));
            relay.input(b"\x01r");
            assert!(!relay.take_reload());
            relay.server_reloaded(Ok(None));
            assert_eq!(bottom(&mut relay, &mut parser), RELOADED);
            assert_eq!(relay.take_messages(), Vec::new());

            relay.notice_expired();
            assert_eq!(bottom(&mut relay, &mut parser), "<work> 0:sh  1:vim");
            relay.input(b"\x02x\x01g");
            assert_eq!(
                relay.take_messages(),
                vec![ClientMessage::Input(b"\x02x".to_vec())]
            );
            assert_eq!(bottom(&mut relay, &mut parser), "new binding");
            relay.input(b"\x01r");
            assert!(relay.take_reload());
        }

        #[test]
        fn a_broken_reload_keeps_the_running_config_and_shows_the_error_once() {
            let (mut relay, mut parser) =
                scripted("amux.keymap.set('prefix', 'g', function() amux.notify('kept') end)");
            relay.input(b"\x02r");
            assert!(relay.take_reload());
            relay.client_reloaded(Err("init.lua:2: boom".into()));
            relay.server_reloaded(Err("init.lua:2: boom".into()));
            assert_eq!(bottom(&mut relay, &mut parser), "init.lua:2: boom");

            relay.input(b"\x02g");
            assert_eq!(bottom(&mut relay, &mut parser), "kept");
            assert_eq!(relay.take_messages(), Vec::new());
        }

        #[test]
        fn a_reload_that_hides_the_status_bar_resizes_the_session() {
            let (mut relay, _) = scripted("");
            relay.input(b"\x02,");
            relay.take_messages();
            relay.client_reloaded(Ok(config("amux.opt.status.enabled = false")));
            assert_eq!(
                relay.take_messages(),
                vec![
                    ClientMessage::Redraw,
                    ClientMessage::Resize(SIZE),
                    ClientMessage::Redraw,
                ]
            );
            assert!(relay.panel.is_none());
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
