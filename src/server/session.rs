use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::Result;

use super::pane::Pane;
use crate::protocol::{SessionInfo, Size};

pub struct Session {
    name: String,
    created_at: Instant,
    attached_clients: AtomicUsize,
    pane: Pane,
}

impl Session {
    pub fn spawn(name: String, cwd: &Path, size: Size) -> Result<Self> {
        Ok(Self {
            name,
            created_at: Instant::now(),
            attached_clients: AtomicUsize::new(0),
            pane: Pane::spawn(cwd, size)?,
        })
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
            name: self.name.clone(),
            attached_clients: self.attached_clients.load(Ordering::Relaxed),
        }
    }

    pub fn track_client(&self) -> AttachedClient<'_> {
        self.attached_clients.fetch_add(1, Ordering::Relaxed);
        AttachedClient {
            counter: &self.attached_clients,
        }
    }
}

pub struct AttachedClient<'a> {
    counter: &'a AtomicUsize,
}

impl Drop for AttachedClient<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}
