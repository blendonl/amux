use std::env;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Instant, SystemTime};

use anyhow::Result;
use tokio::sync::Notify;

use super::pane::Pane;
use crate::protocol::{SessionId, SessionInfo, Size, WindowSummary};

const FALLBACK_WINDOW_NAME: &str = "shell";

pub struct Session {
    id: SessionId,
    name: String,
    created_at: Instant,
    attached_clients: AtomicUsize,
    last_activity: Mutex<SystemTime>,
    input: Notify,
    window_name: String,
    pane: Pane,
}

impl Session {
    pub fn spawn(id: SessionId, name: String, cwd: &Path, size: Size) -> Result<Self> {
        Ok(Self {
            id,
            name,
            created_at: Instant::now(),
            attached_clients: AtomicUsize::new(0),
            last_activity: Mutex::new(SystemTime::now()),
            input: Notify::new(),
            window_name: shell_name(),
            pane: Pane::spawn(cwd, size)?,
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn created_at(&self) -> Instant {
        self.created_at
    }

    pub fn active_pane(&self) -> &Pane {
        &self.pane
    }

    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id,
            name: self.name.clone(),
            windows: vec![WindowSummary {
                index: 0,
                name: self.window_name.clone(),
                panes: 1,
            }],
            attached_clients: self.attached_clients.load(Ordering::Relaxed),
            last_activity: *self
                .last_activity
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            project: None,
            branch: None,
        }
    }

    pub fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = SystemTime::now();
    }

    pub fn record_input(&self) {
        self.touch();
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
}

fn shell_name() -> String {
    env::var_os("SHELL")
        .as_deref()
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| FALLBACK_WINDOW_NAME.to_owned())
}
