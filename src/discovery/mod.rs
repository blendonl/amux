pub mod directory;
pub mod lan;
pub mod mdns;
pub mod tailscale;

use std::env;
use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::warn;

use crate::cluster::Cluster;
use crate::config::ServerId;
use crate::protocol::{DiscoveryReport, PublicKey, SourceState, SourceView, Via};
use crate::settings::{DiscoverySettings, LanSettings};
use lan::Lan;

pub const INTERVAL_ENV: &str = "AMUX_DISCOVERY_INTERVAL_MS";
const STOP_GRACE: Duration = Duration::from_secs(3);

pub struct DiscoveryOptions {
    pub settings: DiscoverySettings,
    pub lan: LanSettings,
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
    lan_state: Arc<Lan>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Discovery {
    pub fn new(
        cluster: Arc<Cluster>,
        options: DiscoveryOptions,
        lan_listener: watch::Receiver<Option<SocketAddr>>,
    ) -> Self {
        let (stop, stopping) = watch::channel(false);
        let status = |enabled| {
            SourceStatus::new(if enabled {
                SourceState::NotRunning
            } else {
                SourceState::Off
            })
        };
        Self {
            tailscale: status(options.settings.tailscale),
            lan: status(options.settings.lan),
            lan_state: Arc::new(Lan::new(lan_listener)),
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

    pub fn lan(&self) -> &Lan {
        &self.lan_state
    }

    pub fn lan_source(&self) -> SourceState {
        self.lan.get()
    }

    pub fn start(&self) {
        let settings = &self.context.options.settings;
        let mut tasks = self.tasks();
        if settings.tailscale {
            let source = tailscale::run(self.context.clone(), self.tailscale.clone());
            tasks.push(tokio::spawn(source));
        }
        if settings.lan {
            let source = lan::run(
                self.context.clone(),
                self.lan.clone(),
                Arc::clone(&self.lan_state),
            );
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

pub fn interval(settings: &DiscoverySettings) -> Result<Duration> {
    interval_from(env::var_os(INTERVAL_ENV), settings.interval_ms)
}

fn interval_from(overridden: Option<OsString>, configured: u64) -> Result<Duration> {
    let millis = match overridden {
        Some(value) => value
            .to_str()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|millis| *millis > 0)
            .with_context(|| format!("{INTERVAL_ENV} must be a positive number"))?,
        None if configured == 0 => bail!("discovery.interval_ms must be a positive number"),
        None => configured,
    };
    Ok(Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interval_comes_from_the_settings_unless_the_variable_is_set() {
        let settings = DiscoverySettings {
            interval_ms: 1000,
            ..DiscoverySettings::default()
        };
        assert_eq!(
            interval_from(None, settings.interval_ms).unwrap(),
            Duration::from_secs(1)
        );
        assert_eq!(
            interval_from(Some("250".into()), settings.interval_ms).unwrap(),
            Duration::from_millis(250)
        );
        assert_eq!(
            interval_from(None, DiscoverySettings::default().interval_ms).unwrap(),
            Duration::from_secs(30)
        );

        for value in ["0", "soon", ""] {
            let error = interval_from(Some(value.into()), 1000).unwrap_err();
            assert!(
                error.to_string().contains(INTERVAL_ENV),
                "{value:?}: {error}"
            );
        }
        let error = interval_from(None, 0).unwrap_err();
        assert!(
            error.to_string().contains("discovery.interval_ms"),
            "{error}"
        );
        assert!(interval_from(Some("10".into()), 0).is_ok());
    }
}
