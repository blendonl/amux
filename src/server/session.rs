use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use anyhow::Result;
use tokio::sync::Notify;

use super::pane::Pane;
use crate::protocol::{SessionId, SessionInfo, Size, WindowSummary};

const FALLBACK_WINDOW_NAME: &str = "shell";

pub struct Session {
    id: SessionId,
    name: Mutex<String>,
    attached_clients: AtomicUsize,
    last_input: Mutex<SystemTime>,
    input: Notify,
    window_name: String,
    pane: Pane,
}

impl Session {
    pub fn spawn(
        id: SessionId,
        name: String,
        cwd: &Path,
        size: Size,
        env: &[(String, String)],
    ) -> Result<Self> {
        Ok(Self {
            id,
            name: Mutex::new(name),
            attached_clients: AtomicUsize::new(0),
            last_input: Mutex::new(SystemTime::now()),
            input: Notify::new(),
            window_name: shell_name(),
            pane: Pane::spawn(cwd, size, env)?,
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn name(&self) -> String {
        lock(&self.name).clone()
    }

    pub fn rename(&self, name: String) {
        *lock(&self.name) = name;
    }

    pub fn active_pane(&self) -> &Pane {
        &self.pane
    }

    pub fn last_activity(&self) -> SystemTime {
        (*lock(&self.last_input)).max(self.pane.last_output())
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            name: self.name(),
            windows: vec![WindowSummary {
                index: 0,
                name: self.window_name.clone(),
                panes: 1,
            }],
            attached_clients: self.attached_clients.load(Ordering::Relaxed),
            last_activity: self.last_activity(),
            project: None,
            branch: None,
        }
    }

    pub fn record_input(&self) {
        *lock(&self.last_input) = SystemTime::now();
        self.input.notify_one();
    }

    pub async fn input_recorded(&self) {
        self.input.notified().await;
    }

    pub fn client_attached(&self) {
        self.attached_clients.fetch_add(1, Ordering::Relaxed);
    }

    pub fn client_detached(&self) {
        self.attached_clients.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn kill(&self) {
        self.pane.kill();
    }
}

fn shell_name() -> String {
    env::var_os("SHELL")
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| FALLBACK_WINDOW_NAME.to_owned())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
