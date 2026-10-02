use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::SystemTime;

use anyhow::{anyhow, bail, Result};
use portable_pty::ExitStatus;
use tokio::sync::{watch, Notify};
use tracing::info;

use super::connection::Origin;
use super::graphics::store::ImageStore;
use super::layout::PaneId;
use super::lua_host::{HookEvent, HookSink};
use super::mouse::InputEvent;
use super::pane::{Pane, PaneObserver, PaneSpec};
use super::paste::PasteBuffer;
use super::render::{Frame, Viewer};
use super::window::Window;
use crate::keys::KeyDecoder;
use crate::project::ProjectId;
use crate::protocol::{
    ClientTerminal, SessionCommand, SessionId, SessionInfo, SessionState, Size, Split,
    WindowSummary,
};
use crate::settings::{Keymap, Settings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub project: ProjectId,
    pub branch: String,
    pub checkout: PathBuf,
    pub worktree: PathBuf,
}

impl Binding {
    pub fn is_for(&self, project: &ProjectId, branch: &str) -> bool {
        self.project == *project && self.branch == branch
    }
}

#[derive(Clone)]
pub struct SessionHost {
    pub settings: watch::Receiver<Arc<Settings>>,
    pub keymap: watch::Receiver<Arc<Keymap>>,
    pub hooks: HookSink,
    pub images: Arc<ImageStore>,
    pub paste: Arc<PasteBuffer>,
}

pub struct Session {
    id: SessionId,
    name: Mutex<String>,
    attached_clients: AtomicUsize,
    last_activity: Mutex<SystemTime>,
    activity: Notify,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    binding: Option<Binding>,
    settings: watch::Receiver<Arc<Settings>>,
    keymap: watch::Receiver<Arc<Keymap>>,
    hooks: HookSink,
    images: Arc<ImageStore>,
    paste: Arc<PasteBuffer>,
    terminals: Mutex<Terminals>,
    terminal: watch::Sender<Option<ClientTerminal>>,
    windows: Mutex<Windows>,
}

impl Session {
    pub fn spawn(
        id: SessionId,
        name: String,
        cwd: &Path,
        size: Size,
        env: &[(String, String)],
        binding: Option<Binding>,
        host: SessionHost,
    ) -> Result<Arc<Self>> {
        let SessionHost {
            settings,
            keymap,
            hooks,
            images,
            paste,
        } = host;
        let session = Arc::new(Self {
            id,
            name: Mutex::new(name),
            attached_clients: AtomicUsize::new(0),
            last_activity: Mutex::new(SystemTime::now()),
            activity: Notify::new(),
            cwd: cwd.to_owned(),
            env: env.to_vec(),
            binding,
            settings,
            keymap,
            hooks,
            images,
            paste,
            terminals: Mutex::new(Terminals::default()),
            terminal: watch::channel(None).0,
            windows: Mutex::new(Windows::new(size.clamped())),
        });
        session.open_window()?;
        Ok(session)
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn name(&self) -> String {
        lock(&self.name).clone()
    }

    pub fn rename(&self, name: String) {
        *lock(&self.name) = name;
        self.state().status_changed();
    }

    pub fn binding(&self) -> Option<&Binding> {
        self.binding.as_ref()
    }

    pub fn last_activity(&self) -> SystemTime {
        *lock(&self.last_activity)
    }

    pub fn windows(&self) -> Vec<WindowSummary> {
        self.state().list.iter().map(Window::summary).collect()
    }

    pub fn status(&self) -> SessionState {
        let name = self.name();
        let windows = self.state();
        SessionState {
            name,
            windows: windows.list.iter().map(Window::summary).collect(),
            active: windows
                .active_window()
                .map(Window::index)
                .unwrap_or_default(),
        }
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            name: self.name(),
            windows: self.windows(),
            attached_clients: self.attached_clients.load(Ordering::Relaxed),
            last_activity: self.last_activity(),
            project: self.binding.as_ref().map(|binding| binding.project.clone()),
            branch: self.binding.as_ref().map(|binding| binding.branch.clone()),
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<()> {
        match &self.state().signals {
            Some(signals) => signals.frames.subscribe(),
            None => closed(),
        }
    }

    pub fn watch_windows(&self) -> watch::Receiver<()> {
        match &self.state().signals {
            Some(signals) => signals.windows.subscribe(),
            None => closed(),
        }
    }

    pub fn watch_status(&self) -> watch::Receiver<()> {
        match &self.state().signals {
            Some(signals) => signals.status.subscribe(),
            None => closed(),
        }
    }

    pub async fn activity(&self) {
        self.activity.notified().await;
    }

    pub fn record_input(&self) {
        self.touch();
    }

    pub fn client_attached(&self, origin: Origin) {
        let clients = self.attached_clients.fetch_add(1, Ordering::Relaxed) + 1;
        self.hooks.emit(HookEvent::ClientAttached {
            session: self.name(),
            origin,
            clients,
        });
    }

    pub fn client_detached(&self, origin: Origin) {
        let clients = self
            .attached_clients
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1);
        self.hooks.emit(HookEvent::ClientDetached {
            session: self.name(),
            origin,
            clients,
        });
    }

    pub fn size(&self) -> Size {
        self.state().size
    }

    pub fn track_terminal(&self) -> TrackedTerminal<'_> {
        let id = self.update_terminals(Terminals::join);
        TrackedTerminal { session: self, id }
    }

    pub fn client_terminal(&self) -> Option<ClientTerminal> {
        *self.terminal.borrow()
    }

    pub fn watch_client_terminal(&self) -> watch::Receiver<Option<ClientTerminal>> {
        self.terminal.subscribe()
    }

    fn update_terminals<T>(&self, change: impl FnOnce(&mut Terminals) -> T) -> T {
        let mut terminals = lock(&self.terminals);
        let result = change(&mut terminals);
        let changed = terminals.latest().filter(|&latest| {
            self.terminal
                .send_if_modified(|current| current.replace(latest) != Some(latest))
        });
        drop(terminals);
        if let Some(terminal) = changed {
            for window in &self.state().list {
                window.set_client_terminal(terminal);
            }
            info!(session = %self.name(), ?terminal, "client terminal changed");
        }
        result
    }

    pub fn redraw(&self) {
        self.state().redraw();
    }

    pub fn resize(&self, size: Size) {
        let mut windows = self.state();
        let size = size.clamped();
        windows.size = size;
        for window in &mut windows.list {
            window.resize(size);
        }
        windows.redraw();
    }

    pub fn frame(&self, viewer: Viewer<'_>) -> Option<Frame> {
        let settings = self.settings();
        let windows = self.state();
        windows.signals.as_ref()?;
        Some(
            windows
                .active_window()?
                .compose(windows.size, &settings, viewer),
        )
    }

    pub fn input(
        &self,
        event: InputEvent,
        keys: &mut KeyDecoder,
        timed_out: bool,
    ) -> Option<String> {
        let settings = self.settings();
        let keymap = self.keymap();
        let mut windows = self.state();
        let size = windows.size;
        let copied = match event {
            InputEvent::Bytes(bytes) => {
                let handled = windows
                    .active_window_mut()
                    .map(|window| window.keys(keys, &bytes, timed_out, &keymap))
                    .unwrap_or_default();
                if handled.redraw {
                    windows.redraw();
                }
                handled.copied
            }
            InputEvent::Mouse(event) => {
                let handled = windows
                    .active_window_mut()
                    .map(|window| window.mouse(event, size, &settings.mouse))
                    .unwrap_or_default();
                if handled.redraw {
                    windows.redraw();
                }
                handled.copied
            }
        };
        drop(windows);
        if let Some(text) = &copied {
            self.paste.store(text.clone());
        }
        copied
    }

    pub fn run(self: &Arc<Self>, command: SessionCommand) -> Result<()> {
        match command {
            SessionCommand::NewWindow => self.new_window(),
            SessionCommand::NextWindow => {
                self.state().cycle(Cycle::Next);
                Ok(())
            }
            SessionCommand::PreviousWindow => {
                self.state().cycle(Cycle::Previous);
                Ok(())
            }
            SessionCommand::SelectWindow(index) => self.select(Some(index), None),
            SessionCommand::SplitPane(split) => self.split(split),
            SessionCommand::NextPane => {
                self.state().focus(|window, _| window.focus_next());
                Ok(())
            }
            SessionCommand::SelectPane(direction) => {
                self.state()
                    .focus(|window, size| window.focus_neighbor(direction, size));
                Ok(())
            }
            SessionCommand::KillPane => {
                let pane = self
                    .state()
                    .active_window()
                    .map(Window::active_pane)
                    .ok_or_else(ended)?;
                self.remove_pane(pane);
                Ok(())
            }
            SessionCommand::KillWindow => {
                let active = self.state().active;
                self.remove_window(active)
            }
            SessionCommand::RenameWindow(name) => self.rename_window(None, name),
            SessionCommand::CopyMode { page_up } => {
                let mut windows = self.state();
                let size = windows.size;
                windows
                    .active_window_mut()
                    .ok_or_else(ended)?
                    .copy_mode(size, page_up);
                windows.redraw();
                Ok(())
            }
            SessionCommand::PasteBuffer => {
                let text = self.paste.contents();
                if text.is_empty() {
                    bail!("the paste buffer is empty");
                }
                self.state().active_window().ok_or_else(ended)?.paste(&text);
                Ok(())
            }
        }
    }

    pub fn rename_window(&self, window: Option<usize>, name: String) -> Result<()> {
        if name.trim().is_empty() {
            bail!("a window name can't be empty");
        }
        if name.chars().any(char::is_control) {
            bail!("a window name can't contain control characters");
        }
        let mut windows = self.state();
        let position = windows.position(window)?;
        windows
            .list
            .get_mut(position)
            .ok_or_else(ended)?
            .rename(name);
        windows.structure_changed();
        Ok(())
    }

    pub fn send_keys(
        &self,
        window: Option<usize>,
        pane: Option<usize>,
        keys: Vec<u8>,
    ) -> Result<()> {
        let windows = self.state();
        let target = windows
            .list
            .get(windows.position(window)?)
            .ok_or_else(ended)?;
        let pane = match pane {
            Some(index) => target.pane_at(index)?,
            None => target.active_pane(),
        };
        target.write_input_to(pane, keys);
        Ok(())
    }

    pub fn select(&self, window: Option<usize>, pane: Option<usize>) -> Result<()> {
        let mut windows = self.state();
        let position = windows.position(window)?;
        let selected = windows.list.get_mut(position).ok_or_else(ended)?;
        if let Some(pane) = pane {
            let id = selected.pane_at(pane)?;
            selected.focus(id);
        }
        windows.active = position;
        windows.active_changed();
        Ok(())
    }

    pub fn kill_window(&self, index: usize) -> Result<()> {
        let position = self.state().position_of(index)?;
        self.remove_window(position)
    }

    pub fn kill_pane(&self, window: Option<usize>, pane: usize) -> Result<()> {
        let id = {
            let windows = self.state();
            windows
                .list
                .get(windows.position(window)?)
                .ok_or_else(ended)?
                .pane_at(pane)?
        };
        self.remove_pane(id);
        Ok(())
    }

    pub fn kill(&self) {
        let closed = self.state().close();
        for window in &closed {
            window.kill();
        }
    }

    fn remove_pane(&self, pane: PaneId) {
        let Some(removed) = self.state().remove_pane(pane) else {
            return;
        };
        self.window_closed(removed.closed.as_ref());
    }

    fn remove_window(&self, position: usize) -> Result<()> {
        let removed = self.state().remove_window(position).ok_or_else(ended)?;
        self.window_closed(Some(&removed));
        Ok(())
    }

    fn window_closed(&self, window: Option<&Window>) {
        if let Some(WindowSummary { index, name, .. }) = window.map(Window::summary) {
            self.hooks.emit(HookEvent::WindowClosed {
                session: self.name(),
                window: index,
                name,
            });
        }
    }

    fn new_window(self: &Arc<Self>) -> Result<()> {
        let WindowSummary { index, name, .. } = self.open_window()?;
        self.hooks.emit(HookEvent::WindowCreated {
            session: self.name(),
            window: index,
            name,
        });
        Ok(())
    }

    fn open_window(self: &Arc<Self>) -> Result<WindowSummary> {
        let settings = self.settings();
        let mut windows = self.state();
        if windows.signals.is_none() {
            bail!("the session has ended");
        }
        let index = windows.free_index(settings.window.base_index);
        let id = windows.next_pane_id();
        let pane = self.spawn_pane(id, windows.size)?;
        let position = windows
            .list
            .partition_point(|window| window.index() < index);
        let name = settings.window.name.clone().unwrap_or_else(shell_name);
        let window = Window::new(index, name, id, pane);
        let summary = window.summary();
        windows.list.insert(position, window);
        windows.active = position;
        windows.structure_changed();
        Ok(summary)
    }

    fn split(self: &Arc<Self>, split: Split) -> Result<()> {
        let mut windows = self.state();
        let size = windows.size;
        let id = windows.next_pane_id();
        let window = windows.active_window_mut().ok_or_else(ended)?;
        window.split(id, split, size, |pane_size| self.spawn_pane(id, pane_size))?;
        windows.structure_changed();
        Ok(())
    }

    fn spawn_pane(self: &Arc<Self>, id: PaneId, size: Size) -> Result<Pane> {
        let this: Weak<Self> = Arc::downgrade(self);
        let observer: Weak<dyn PaneObserver> = this;
        Pane::spawn(PaneSpec {
            id,
            cwd: &self.cwd,
            size,
            env: &self.env,
            settings: &self.settings().pane,
            observer,
            store: &self.images,
            terminal: self.client_terminal(),
        })
    }

    fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.settings.borrow())
    }

    fn keymap(&self) -> Arc<Keymap> {
        Arc::clone(&self.keymap.borrow())
    }

    fn touch(&self) {
        *lock(&self.last_activity) = SystemTime::now();
        self.activity.notify_one();
    }

    fn state(&self) -> MutexGuard<'_, Windows> {
        lock(&self.windows)
    }
}

impl PaneObserver for Session {
    fn pane_output(&self, pane: PaneId) {
        self.touch();
        let windows = self.state();
        if windows
            .active_window()
            .is_some_and(|window| window.shows_output_of(pane))
        {
            windows.redraw();
        }
    }

    fn pane_exited(&self, pane: PaneId, exit: Option<ExitStatus>) {
        let Some(removed) = self.state().remove_pane(pane) else {
            return;
        };
        let (status, signal) = match &exit {
            Some(exit) => match exit.signal() {
                Some(signal) => (None, Some(signal.to_owned())),
                None => (Some(exit.exit_code()), None),
            },
            None => (None, None),
        };
        self.hooks.emit(HookEvent::PaneExited {
            session: self.name(),
            window: removed.window,
            pane: pane.0,
            status,
            signal,
        });
        self.window_closed(removed.closed.as_ref());
    }
}

pub struct TrackedTerminal<'a> {
    session: &'a Session,
    id: u64,
}

impl TrackedTerminal<'_> {
    pub fn report(&self, terminal: ClientTerminal) {
        self.session
            .update_terminals(|terminals| terminals.report(self.id, terminal));
    }

    pub fn mark_active(&self) {
        self.session
            .update_terminals(|terminals| terminals.activate(self.id));
    }
}

impl Drop for TrackedTerminal<'_> {
    fn drop(&mut self) {
        self.session
            .update_terminals(|terminals| terminals.leave(self.id));
    }
}

#[derive(Default)]
struct Terminals {
    next_id: u64,
    clock: u64,
    clients: Vec<AttachedTerminal>,
}

struct AttachedTerminal {
    id: u64,
    active: u64,
    terminal: Option<ClientTerminal>,
}

impl Terminals {
    fn join(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.clock += 1;
        self.clients.push(AttachedTerminal {
            id,
            active: self.clock,
            terminal: None,
        });
        id
    }

    fn activate(&mut self, id: u64) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(client) = self.client(id) {
            client.active = clock;
        }
    }

    fn report(&mut self, id: u64, terminal: ClientTerminal) {
        if let Some(client) = self.client(id) {
            client.terminal = Some(terminal);
        }
    }

    fn leave(&mut self, id: u64) {
        self.clients.retain(|client| client.id != id);
    }

    fn client(&mut self, id: u64) -> Option<&mut AttachedTerminal> {
        self.clients.iter_mut().find(|client| client.id == id)
    }

    fn latest(&self) -> Option<ClientTerminal> {
        self.clients
            .iter()
            .filter_map(|client| Some((client.active, client.terminal?)))
            .max_by_key(|(active, _)| *active)
            .map(|(_, terminal)| terminal)
    }
}

struct Windows {
    list: Vec<Window>,
    active: usize,
    size: Size,
    next_pane: u32,
    signals: Option<Signals>,
}

enum Cycle {
    Next,
    Previous,
}

struct RemovedPane {
    window: usize,
    closed: Option<Window>,
    _pane: Option<Pane>,
}

struct Signals {
    frames: watch::Sender<()>,
    windows: watch::Sender<()>,
    status: watch::Sender<()>,
}

impl Windows {
    fn new(size: Size) -> Self {
        Self {
            list: Vec::new(),
            active: 0,
            size,
            next_pane: 0,
            signals: Some(Signals {
                frames: watch::channel(()).0,
                windows: watch::channel(()).0,
                status: watch::channel(()).0,
            }),
        }
    }

    fn active_window(&self) -> Option<&Window> {
        self.list.get(self.active)
    }

    fn active_window_mut(&mut self) -> Option<&mut Window> {
        self.list.get_mut(self.active)
    }

    fn next_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        id
    }

    fn free_index(&self, base: usize) -> usize {
        (base..)
            .find(|index| self.list.iter().all(|window| window.index() != *index))
            .expect("a free window index always exists")
    }

    fn position(&self, window: Option<usize>) -> Result<usize> {
        match window {
            Some(index) => self.position_of(index),
            None => Ok(self.active),
        }
    }

    fn position_of(&self, index: usize) -> Result<usize> {
        self.list
            .iter()
            .position(|window| window.index() == index)
            .ok_or_else(|| anyhow!("can't find window {index}"))
    }

    fn cycle(&mut self, cycle: Cycle) {
        let count = self.list.len();
        if count < 2 {
            return;
        }
        self.active = match cycle {
            Cycle::Next => (self.active + 1) % count,
            Cycle::Previous => (self.active + count - 1) % count,
        };
        self.active_changed();
    }

    fn focus(&mut self, change: impl FnOnce(&mut Window, Size) -> bool) {
        let size = self.size;
        if self
            .active_window_mut()
            .is_some_and(|window| change(window, size))
        {
            self.redraw();
        }
    }

    fn remove_pane(&mut self, pane: PaneId) -> Option<RemovedPane> {
        let size = self.size;
        let position = self.list.iter().position(|window| window.contains(pane))?;
        let window = self.list[position].index();
        let removed = self.list[position].remove(pane, size);
        let closed = if self.list[position].is_empty() {
            self.remove_window(position)
        } else {
            self.structure_changed();
            None
        };
        Some(RemovedPane {
            window,
            closed,
            _pane: removed,
        })
    }

    fn remove_window(&mut self, position: usize) -> Option<Window> {
        if position >= self.list.len() {
            return None;
        }
        let removed = self.list.remove(position);
        if self.list.is_empty() {
            self.signals = None;
            return Some(removed);
        }
        if self.active > position || (self.active == position && position > 0) {
            self.active -= 1;
        }
        self.active = self.active.min(self.list.len() - 1);
        self.structure_changed();
        Some(removed)
    }

    fn close(&mut self) -> Vec<Window> {
        self.signals = None;
        std::mem::take(&mut self.list)
    }

    fn redraw(&self) {
        if let Some(signals) = &self.signals {
            signals.frames.send_replace(());
        }
    }

    fn active_changed(&self) {
        self.redraw();
        self.status_changed();
    }

    fn status_changed(&self) {
        if let Some(signals) = &self.signals {
            signals.status.send_replace(());
        }
    }

    fn structure_changed(&self) {
        if let Some(signals) = &self.signals {
            signals.frames.send_replace(());
            signals.windows.send_replace(());
            signals.status.send_replace(());
        }
    }
}

fn closed() -> watch::Receiver<()> {
    watch::channel(()).1
}

fn ended() -> anyhow::Error {
    anyhow!("the session has ended")
}

fn shell_name() -> String {
    env::var_os("SHELL")
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "shell".to_owned())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CellPixels;
    use crate::settings::{PaneSettings, WindowSettings};

    fn spawn(window: WindowSettings) -> Arc<Session> {
        let settings = Settings {
            window,
            pane: PaneSettings {
                shell: Some(vec!["/bin/sh".into()]),
                ..PaneSettings::default()
            },
            ..Settings::default()
        };
        Session::spawn(
            SessionId(1),
            "work".into(),
            &env::temp_dir(),
            Size { rows: 24, cols: 80 },
            &[],
            None,
            SessionHost {
                settings: watch::channel(Arc::new(settings)).1,
                keymap: watch::channel(Arc::new(Keymap::default())).1,
                hooks: HookSink::default(),
                images: Arc::new(ImageStore::new(1 << 20)),
                paste: Arc::default(),
            },
        )
        .unwrap()
    }

    fn listed(session: &Session) -> Vec<(usize, String)> {
        session
            .windows()
            .into_iter()
            .map(|window| (window.index, window.name))
            .collect()
    }

    #[test]
    fn windows_are_numbered_from_zero_and_named_after_the_shell() {
        let session = spawn(WindowSettings::default());
        session.run(SessionCommand::NewWindow).unwrap();

        assert_eq!(listed(&session), [(0, shell_name()), (1, shell_name())]);
        session.kill();
    }

    #[test]
    fn windows_are_numbered_from_the_base_index_with_the_configured_name() {
        let session = spawn(WindowSettings {
            base_index: 1,
            name: Some("editor".into()),
        });
        session.run(SessionCommand::NewWindow).unwrap();
        session.run(SessionCommand::NewWindow).unwrap();
        session.kill_window(2).unwrap();
        session.run(SessionCommand::NewWindow).unwrap();

        let editor = || "editor".to_owned();
        assert_eq!(
            listed(&session),
            [(1, editor()), (2, editor()), (3, editor())]
        );
        assert_eq!(session.status().active, 2);
        session.kill();
    }

    #[test]
    fn the_latest_active_client_with_a_terminal_sets_the_session_terminal() {
        let session = spawn(WindowSettings::default());
        let kitty = ClientTerminal {
            graphics: true,
            cell_pixels: Some(CellPixels {
                width: 10,
                height: 21,
            }),
        };
        let plain = ClientTerminal {
            graphics: false,
            cell_pixels: Some(CellPixels {
                width: 8,
                height: 16,
            }),
        };
        let mut changes = session.watch_client_terminal();
        assert_eq!(session.client_terminal(), None);

        let first = session.track_terminal();
        assert!(!changes.has_changed().unwrap());
        first.report(ClientTerminal::default());
        assert_eq!(
            *changes.borrow_and_update(),
            Some(ClientTerminal::default())
        );
        first.report(kitty);
        assert_eq!(*changes.borrow_and_update(), Some(kitty));

        let second = session.track_terminal();
        assert_eq!(session.client_terminal(), Some(kitty));
        second.report(plain);
        assert_eq!(*changes.borrow_and_update(), Some(plain));

        first.mark_active();
        assert_eq!(*changes.borrow_and_update(), Some(kitty));
        first.report(kitty);
        first.mark_active();
        assert!(!changes.has_changed().unwrap());

        drop(first);
        assert_eq!(*changes.borrow_and_update(), Some(plain));
        drop(second);
        assert!(!changes.has_changed().unwrap());
        assert_eq!(session.client_terminal(), Some(plain));
        session.kill();
    }
}
