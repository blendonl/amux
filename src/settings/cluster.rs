use std::num::NonZeroU16;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClusterSettings {
    pub ping_interval_ms: u64,
    pub missed_pings: u32,
    pub handshake_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub backoff_min_ms: u64,
    pub backoff_max_ms: u64,
    pub max_unverified_backoff_ms: u64,
    pub address_expiry_hours: u64,
    pub status_interval_ms: u64,
    pub ssh: SshSettings,
}

impl ClusterSettings {
    pub fn ping_interval(&self) -> Duration {
        Duration::from_millis(self.ping_interval_ms)
    }

    pub fn silence_limit(&self) -> Duration {
        self.ping_interval() * self.missed_pings
    }

    pub fn handshake_timeout(&self) -> Duration {
        Duration::from_millis(self.handshake_timeout_ms)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.connect_timeout_ms)
    }

    pub fn backoff_min(&self) -> Duration {
        Duration::from_millis(self.backoff_min_ms)
    }

    pub fn backoff_max(&self) -> Duration {
        Duration::from_millis(self.backoff_max_ms)
    }

    pub fn max_unverified_backoff(&self) -> Duration {
        Duration::from_millis(self.max_unverified_backoff_ms)
    }

    pub fn address_expiry(&self) -> Duration {
        Duration::from_secs(self.address_expiry_hours.saturating_mul(60 * 60))
    }

    pub fn status_interval(&self) -> Duration {
        Duration::from_millis(self.status_interval_ms)
    }
}

impl Default for ClusterSettings {
    fn default() -> Self {
        Self {
            ping_interval_ms: 5000,
            missed_pings: 3,
            handshake_timeout_ms: 30_000,
            connect_timeout_ms: 10_000,
            backoff_min_ms: 1000,
            backoff_max_ms: 60_000,
            max_unverified_backoff_ms: 10 * 60 * 1000,
            address_expiry_hours: 7 * 24,
            status_interval_ms: 2000,
            ssh: SshSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SshSettings {
    pub program: String,
    pub options: Vec<String>,
    pub default_amux_path: String,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            program: "ssh".into(),
            options: ["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15"]
                .map(String::from)
                .into(),
            default_amux_path: "amux".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoverySettings {
    pub tailscale: bool,
    pub lan: bool,
    pub tailscale_tags: Vec<String>,
    pub tailscale_port: Option<NonZeroU16>,
    pub tailscale_program: String,
    pub interval_ms: u64,
}

impl DiscoverySettings {
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
}

impl Default for DiscoverySettings {
    fn default() -> Self {
        Self {
            tailscale: true,
            lan: true,
            tailscale_tags: Vec::new(),
            tailscale_port: None,
            tailscale_program: "tailscale".into(),
            interval_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LanSettings {
    pub port: u16,
    pub mdns_service: String,
    pub pairing_window_ms: u64,
}

impl LanSettings {
    pub fn pairing_window(&self) -> Duration {
        Duration::from_millis(self.pairing_window_ms)
    }
}

impl Default for LanSettings {
    fn default() -> Self {
        Self {
            port: 0,
            mdns_service: "_amux._tcp.local.".into(),
            pairing_window_ms: 5 * 60 * 1000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub address: String,
    pub amux_path: Option<String>,
    pub socket: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::super::Settings;
    use super::*;

    #[test]
    fn the_defaults_are_todays_timings() {
        let cluster = ClusterSettings::default();
        assert_eq!(cluster.ping_interval(), Duration::from_secs(5));
        assert_eq!(cluster.silence_limit(), Duration::from_secs(15));
        assert_eq!(cluster.handshake_timeout(), Duration::from_secs(30));
        assert_eq!(cluster.connect_timeout(), Duration::from_secs(10));
        assert_eq!(cluster.backoff_min(), Duration::from_secs(1));
        assert_eq!(cluster.backoff_max(), Duration::from_secs(60));
        assert_eq!(cluster.max_unverified_backoff(), Duration::from_secs(600));
        assert_eq!(
            cluster.address_expiry(),
            Duration::from_secs(7 * 24 * 60 * 60)
        );
        assert_eq!(cluster.status_interval(), Duration::from_secs(2));
        assert_eq!(cluster.ssh.program, "ssh");
        assert_eq!(
            cluster.ssh.options,
            ["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15"]
        );
        assert_eq!(cluster.ssh.default_amux_path, "amux");

        let discovery = DiscoverySettings::default();
        assert_eq!(discovery.interval(), Duration::from_secs(30));
        assert_eq!(discovery.tailscale_program, "tailscale");
        let lan = LanSettings::default();
        assert_eq!(lan.pairing_window(), Duration::from_secs(5 * 60));
        assert_eq!(lan.mdns_service, "_amux._tcp.local.");
    }

    #[test]
    fn cluster_tables_override_only_the_fields_they_name() {
        let settings: Settings = toml::from_str(
            "[cluster]\nbackoff_max_ms = 5000\naddress_expiry_hours = 1\n\n\
             [cluster.ssh]\nprogram = \"autossh\"\n\n\
             [discovery]\ninterval_ms = 1000\ntailscale_port = 7500\n\n\
             [lan]\npairing_window_ms = 60000",
        )
        .unwrap();
        assert_eq!(
            settings.cluster,
            ClusterSettings {
                backoff_max_ms: 5000,
                address_expiry_hours: 1,
                ssh: SshSettings {
                    program: "autossh".into(),
                    ..SshSettings::default()
                },
                ..ClusterSettings::default()
            }
        );
        assert_eq!(settings.cluster.address_expiry(), Duration::from_secs(3600));
        assert_eq!(
            settings.discovery,
            DiscoverySettings {
                interval_ms: 1000,
                tailscale_port: NonZeroU16::new(7500),
                ..DiscoverySettings::default()
            }
        );
        assert_eq!(settings.lan.pairing_window(), Duration::from_secs(60));
        assert_eq!(settings.lan.port, 0);

        for table in [
            "[cluster]\nbogus = 1",
            "[cluster.ssh]\nbogus = 1",
            "[discovery]\nbogus = 1",
            "[discovery]\ntailscale_port = 0",
            "[lan]\nbogus = 1",
        ] {
            assert!(toml::from_str::<Settings>(table).is_err(), "{table}");
        }
    }

    #[test]
    fn the_default_network_settings_round_trip() {
        let written = toml::to_string(&Settings::default()).unwrap();
        let read: Settings = toml::from_str(&written).unwrap();
        assert_eq!(read.cluster, ClusterSettings::default());
        assert_eq!(read.discovery, DiscoverySettings::default());
        assert_eq!(read.lan, LanSettings::default());
        assert!(written.contains("[cluster.ssh]"), "{written}");
        assert!(written.contains("ping_interval_ms = 5000"), "{written}");
        assert!(
            written.contains("mdns_service = \"_amux._tcp.local.\""),
            "{written}"
        );
        assert!(!written.contains("tailscale_port"), "{written}");

        let json = serde_json::to_value(ClusterSettings::default()).unwrap();
        assert_eq!(
            serde_json::from_value::<ClusterSettings>(json).unwrap(),
            ClusterSettings::default()
        );
    }
}
