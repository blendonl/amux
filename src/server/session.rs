use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::SystemTime;

use anyhow::{anyhow, bail, Result};
use tokio::sync::{watch, Notify};

use super::layout::PaneId;
use super::mouse::InputEvent;
use super::pane::{Pane, PaneObserver, PaneSpec};
use super::render::Frame;
use super::window::Window;
use crate::project::ProjectId;
use crate::protocol::{
    SessionCommand, SessionId, SessionInfo, SessionState, Size, Split, WindowSummary,
};
use crate::settings::Settings;

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

pub struct Session {
    id: SessionId,
    name: Mutex<String>,
    attached_clients: AtomicUsize,
    last_activity: Mutex<SystemTime>,
    activity: Notify,
    window_name: String,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    binding: Option<Binding>,
    settings: Arc<Settings>,
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
        settings: Arc<Settings>,
    ) -> Result<Arc<Self>> {
        let session = Arc::new(Self {
            id,
            name: Mutex::new(name),
            attached_clients: AtomicUsize::new(0),
            last_activity: Mutex::new(SystemTime::now()),
            activity: Notify::new(),
            window_name: settings.window.name.clone().unwrap_or_else(shell_name),
            cwd: cwd.to_owned(),
            env: env.to_vec(),
            binding,
            settings,
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

    pub fn client_attached(&self) {
        self.attached_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn client_detached(&self) {
        self.attached_clients.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn size(&self) -> Size {
        self.state().size
    }

    pub fn resize(&self, size: Size) {
        let mut windows = self.state();
        windows.size = size.clamped();
        for window in &windows.list {
            window.resize(windows.size);
        }
        windows.redraw();
    }

    pub fn frame(&self) -> Option<Frame> {
        let windows = self.state();
        windows.signals.as_ref()?;
        Some(
            windows
                .active_window()?
                .compose(windows.size, &self.settings),
        )
    }

    pub fn input(&self, event: InputEvent) {
        let mut windows = self.state();
        let size = windows.size;
        match event {
            InputEvent::Bytes(bytes) => {
                if let Some(window) = windows.active_window() {
                    window.write_input(bytes);
                }
            }
            InputEvent::Mouse(event) => {
                let focused = windows
                    .active_window_mut()
                    .is_some_and(|window| window.mouse(event, size));
                if focused {
                    windows.redraw();
                }
            }
        }
    }

    pub fn run(self: &Arc<Self>, command: SessionCommand) -> Result<()> {
        match command {
            SessionCommand::NewWindow => self.open_window(),
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
            SessionCommand::RenameWindow(name) => self.rename_window(name),
        }
    }

    fn rename_window(&self, name: String) -> Result<()> {
        if name.trim().is_empty() {
            bail!("a window name can't be empty");
        }
        if name.chars().any(char::is_control) {
            bail!("a window name can't contain control characters");
        }
        let mut windows = self.state();
        windows.active_window_mut().ok_or_else(ended)?.rename(name);
        windows.structure_changed();
        Ok(())
    }

    pub fn select(&self, window: Option<usize>, pane: Option<usize>) -> Result<()> {
        let mut windows = self.state();
        let position = match window {
            Some(index) => windows.position_of(index)?,
            None => windows.active,
        };
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
            let position = match window {
                Some(index) => windows.position_of(index)?,
                None => windows.active,
            };
            windows
                .list
                .get(position)
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
        let _removed = self.state().remove_pane(pane);
    }

    fn remove_window(&self, position: usize) -> Result<()> {
        let _removed = self.state().remove_window(position).ok_or_else(ended)?;
        Ok(())
    }

    fn open_window(self: &Arc<Self>) -> Result<()> {
        let mut windows = self.state();
        if windows.signals.is_none() {
            bail!("the session has ended");
        }
        let index = windows.free_index(self.settings.window.base_index);
        let id = windows.next_pane_id();
        let pane = self.spawn_pane(id, windows.size)?;
        let position = windows
            .list
            .partition_point(|window| window.index() < index);
        windows.list.insert(
            position,
            Window::new(index, self.window_name.clone(), id, pane),
        );
        windows.active = position;
        windows.structure_changed();
        Ok(())
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
            settings: &self.settings.pane,
            observer,
        })
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
            .is_some_and(|window| window.contains(pane))
        {
            windows.redraw();
        }
    }

    fn pane_exited(&self, pane: PaneId) {
        self.remove_pane(pane);
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

    fn remove_pane(&mut self, pane: PaneId) -> Option<Pane> {
        let size = self.size;
        let position = self.list.iter().position(|window| window.contains(pane))?;
        let removed = self.list[position].remove(pane, size);
        if self.list[position].is_empty() {
            self.remove_window(position);
        } else {
            self.structure_changed();
        }
        removed
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
            Arc::new(settings),
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
}
