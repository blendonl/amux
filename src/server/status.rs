use tokio::sync::watch;
use tokio::time::{Interval, MissedTickBehavior};

use super::connection::Origin;
use super::Server;
use crate::protocol::{ClusterStatus, ServerMessage};

pub struct StatusFeed {
    enabled: bool,
    changes: watch::Receiver<()>,
    ticks: Interval,
    sent: Option<ClusterStatus>,
}

impl StatusFeed {
    pub fn new(server: &Server, origin: Origin) -> Self {
        let mut ticks = tokio::time::interval(server.settings.cluster.status_interval());
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self {
            enabled: origin == Origin::Local,
            changes: server.cluster().watch(),
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
