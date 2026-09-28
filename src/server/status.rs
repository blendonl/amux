use std::sync::Arc;

use tokio::sync::watch;
use tokio::time::{Interval, MissedTickBehavior};

use super::connection::Origin;
use super::Server;
use crate::protocol::{ClusterStatus, ServerMessage};
use crate::settings::Settings;

pub struct StatusFeed {
    enabled: bool,
    changes: watch::Receiver<()>,
    settings: watch::Receiver<Arc<Settings>>,
    ticks: Interval,
    sent: Option<ClusterStatus>,
}

impl StatusFeed {
    pub fn new(server: &Server, origin: Origin) -> Self {
        let mut settings = server.watch_settings();
        let ticks = ticker(&settings.borrow_and_update());
        Self {
            enabled: origin == Origin::Local,
            changes: server.cluster().watch(),
            settings,
            ticks,
            sent: None,
        }
    }

    pub async fn due(&mut self) {
        if !self.enabled {
            return std::future::pending().await;
        }
        tokio::select! {
            Ok(()) = self.changes.changed() => {}
            Ok(()) = self.settings.changed() => {
                self.ticks = ticker(&self.settings.borrow_and_update());
            }
            _ = self.ticks.tick() => {}
        }
    }

    pub fn update(&mut self, server: &Server, host: &str) -> Option<ServerMessage> {
        let status = server.cluster_status(host);
        if self.sent.as_ref() == Some(&status) {
            return None;
        }
        self.sent = Some(status.clone());
        Some(ServerMessage::ClusterStatus(status))
    }
}

fn ticker(settings: &Settings) -> Interval {
    let mut ticks = tokio::time::interval(settings.cluster.status_interval());
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticks
}
