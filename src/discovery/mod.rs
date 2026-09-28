pub mod directory;
pub mod lan;
pub mod tailscale;

use std::env;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::cluster::Cluster;
use crate::config::{DiscoveryConfig, LanConfig, ServerId};
use crate::protocol::{DiscoveryReport, PublicKey, SourceState, SourceView, Via};

pub const INTERVAL_ENV: &str = "AMUX_DISCOVERY_INTERVAL_MS";
const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);
const STOP_GRACE: Duration = Duration::from_secs(3);

pub struct DiscoveryOptions {
    pub config: DiscoveryConfig,
    pub lan: LanConfig,
    pub socket_name: String,
    pub state_dir: Option<PathBuf>,
}

#[derive(Clone)]
pub struct SourceContext {
    pub cluster: Arc<Cluster>,
    pub options: Arc<DiscoveryOptions>,
    pub stopping: watch::Receiver<bool>,
}

#[derive(Clone)]
pub struct SourceStatus(Arc<Mutex<SourceState>>);

impl SourceStatus {
    fn new(state: SourceState) -> Self {
        Self(Arc::new(Mutex::new(state)))
    }

    pub fn set(&self, state: SourceState) {
        *self.lock() = state;
    }

    pub fn get(&self) -> SourceState {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, SourceState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Discovery {
    context: SourceContext,
    stop: watch::Sender<bool>,
    tailscale: SourceStatus,
    lan: SourceStatus,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Discovery {
    pub fn new(cluster: Arc<Cluster>, options: DiscoveryOptions) -> Self {
        let (stop, stopping) = watch::channel(false);
        let status = |enabled| {
            SourceStatus::new(if enabled {
                SourceState::NotRunning
            } else {
                SourceState::Off
            })
        };
        Self {
            tailscale: status(options.config.tailscale),
            lan: status(options.config.lan),
            context: SourceContext {
                cluster,
                options: Arc::new(options),
                stopping,
            },
            stop,
            tasks: Mutex::default(),
        }
    }

    pub fn context(&self) -> &SourceContext {
        &self.context
    }

    pub fn start(&self) {
        let config = &self.context.options.config;
        let mut tasks = self.tasks();
        if config.tailscale {
            let source = tailscale::run(self.context.clone(), self.tailscale.clone());
            tasks.push(tokio::spawn(source));
        }
        if config.lan {
            let source = lan::run(self.context.clone(), self.lan.clone());
            tasks.push(tokio::spawn(source));
        }
    }

    pub async fn stop(&self) {
        self.stop.send_replace(true);
        let tasks = std::mem::take(&mut *self.tasks());
        for task in tasks {
            if tokio::time::timeout(STOP_GRACE, task).await.is_err() {
                warn!("gave up waiting for a discovery source to stop");
            }
        }
    }

    pub fn report(&self) -> DiscoveryReport {
        DiscoveryReport {
            sources: vec![
                SourceView {
                    via: Via::Tailscale,
                    state: self.tailscale.get(),
                },
                SourceView {
                    via: Via::Lan,
                    state: self.lan.get(),
                },
            ],
            peers: self.context.cluster.discovery_view(),
        }
    }

    fn tasks(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Advertisement {
    pub id: ServerId,
    pub name: String,
    pub key: PublicKey,
    pub cluster: String,
    pub proto: u16,
    pub pairing: Option<String>,
    pub port: u16,
    pub addresses: Vec<IpAddr>,
}

pub trait LanDiscovery: Send + Sync {
    fn advertise(&self, advertisement: &Advertisement) -> Result<()>;
    fn withdraw(&self) -> Result<()>;
    fn browse(&self) -> watch::Receiver<Vec<Advertisement>>;
    fn shutdown(&self);
}

pub fn interval() -> Result<Duration> {
    let Some(value) = env::var_os(INTERVAL_ENV) else {
        return Ok(DEFAULT_INTERVAL);
    };
    value
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|millis| *millis > 0)
        .map(Duration::from_millis)
        .with_context(|| format!("{INTERVAL_ENV} must be a positive number"))
}
