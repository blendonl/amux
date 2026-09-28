use std::collections::BTreeMap;
use std::env;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use mdns_sd::{
    IfKind, IfPredicate, Receiver, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent,
    ServiceInfo,
};
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::{Advertisement, LanDiscovery};
use crate::config::ServerId;

pub const SERVICE_ENV: &str = "AMUX_MDNS_SERVICE";
pub const DEFAULT_SERVICE: &str = "_amux._tcp.local.";
const LOOPBACK_INTERFACE: &str = "lo";
const SKIPPED_PREFIXES: [&str; 5] = ["tailscale", "utun", "docker", "br-", "veth"];
const NAME: &str = "name";
const KEY: &str = "key";
const CLUSTER: &str = "cluster";
const PROTO: &str = "proto";
const PAIR: &str = "pair";

pub struct MdnsLan {
    daemon: ServiceDaemon,
    service: String,
    advertised: Mutex<Option<String>>,
    found: watch::Receiver<Vec<Advertisement>>,
}

impl MdnsLan {
    pub fn start(service: String) -> Result<Arc<Self>> {
        let daemon = ServiceDaemon::new().context("starting the mDNS daemon")?;
        let skipped = IfPredicate::new(|interface| {
            interface.is_loopback() || is_skipped_interface(&interface.name)
        });
        daemon
            .disable_interface(IfKind::Predicate(skipped))
            .context("choosing the network interfaces for mDNS")?;
        let events = daemon
            .browse(&service)
            .with_context(|| format!("browsing for {service}"))?;
        let (found, browsing) = watch::channel(Vec::new());
        tokio::spawn(follow(events, service.clone(), found));
        info!(%service, "browsing for servers over mDNS");
        Ok(Arc::new(Self {
            daemon,
            service,
            advertised: Mutex::new(None),
            found: browsing,
        }))
    }

    fn advertised(&self) -> MutexGuard<'_, Option<String>> {
        self.advertised
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl LanDiscovery for MdnsLan {
    fn advertise(&self, advertisement: &Advertisement) -> Result<()> {
        let mut properties = vec![
            (NAME, advertisement.name.clone()),
            (KEY, advertisement.key.to_hex()),
            (CLUSTER, advertisement.cluster.clone()),
            (PROTO, advertisement.proto.to_string()),
        ];
        if let Some(window) = &advertisement.pairing {
            properties.push((PAIR, window.clone()));
        }
        let id = advertisement.id.to_string();
        let host = format!("amux-{id}.local.");
        let mut info = ServiceInfo::new(
            &self.service,
            &id,
            &host,
            advertisement.addresses.as_slice(),
            advertisement.port,
            properties.as_slice(),
        )
        .context("describing the mDNS service")?;
        if advertisement.addresses.is_empty() {
            info = info.enable_addr_auto();
        }
        info.set_requires_probe(false);
        let fullname = info.get_fullname().to_owned();
        self.daemon
            .register(info)
            .context("registering the mDNS service")?;
        *self.advertised() = Some(fullname);
        Ok(())
    }

    fn withdraw(&self) -> Result<()> {
        let Some(fullname) = self.advertised().take() else {
            return Ok(());
        };
        self.daemon
            .unregister(&fullname)
            .context("unregistering the mDNS service")?;
        Ok(())
    }

    fn browse(&self) -> watch::Receiver<Vec<Advertisement>> {
        self.found.clone()
    }

    fn shutdown(&self) {
        if let Err(err) = self.withdraw() {
            warn!("withdrawing the mDNS advertisement failed: {err:#}");
        }
        if let Err(err) = self.daemon.shutdown() {
            debug!("stopping the mDNS daemon failed: {err}");
        }
    }
}

pub fn service() -> Result<String> {
    match env::var_os(SERVICE_ENV) {
        None => Ok(DEFAULT_SERVICE.to_owned()),
        Some(service) => service
            .into_string()
            .ok()
            .filter(|service| service.ends_with("._tcp.local."))
            .with_context(|| {
                format!("{SERVICE_ENV} must be an mDNS service like {DEFAULT_SERVICE}")
            }),
    }
}

pub fn is_skipped_interface(name: &str) -> bool {
    name == LOOPBACK_INTERFACE
        || SKIPPED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

async fn follow(
    events: Receiver<ServiceEvent>,
    service: String,
    found: watch::Sender<Vec<Advertisement>>,
) {
    let mut services = BTreeMap::new();
    while let Ok(event) = events.recv_async().await {
        match event {
            ServiceEvent::ServiceResolved(resolved) => match parse(&resolved, &service) {
                Some(advertisement) => {
                    services.insert(resolved.fullname.clone(), advertisement);
                }
                None => {
                    debug!(service = %resolved.fullname, "ignoring an mDNS service that does not parse");
                    services.remove(&resolved.fullname);
                }
            },
            ServiceEvent::ServiceRemoved(_, fullname) => {
                services.remove(&fullname);
            }
            _ => continue,
        }
        let mut advertisements: Vec<Advertisement> = services.values().cloned().collect();
        advertisements.sort_by_key(|advertisement| advertisement.id);
        advertisements.dedup_by_key(|advertisement| advertisement.id);
        found.send_if_modified(|current| {
            let changed = *current != advertisements;
            if changed {
                *current = advertisements;
            }
            changed
        });
    }
    debug!("the mDNS daemon stopped");
}

fn parse(resolved: &ResolvedService, service: &str) -> Option<Advertisement> {
    let instance = resolved.fullname.strip_suffix(service)?.strip_suffix('.')?;
    let id: ServerId = instance.to_ascii_lowercase().parse().ok()?;
    let property = |key| resolved.get_property_val_str(key);
    let mut addresses: Vec<IpAddr> = resolved
        .addresses
        .iter()
        .map(ScopedIp::to_ip_addr)
        .collect();
    addresses.sort();
    Some(Advertisement {
        id,
        name: property(NAME)?.to_owned(),
        key: property(KEY)?.parse().ok()?,
        cluster: property(CLUSTER)?.to_owned(),
        proto: property(PROTO)?.parse().ok()?,
        pairing: property(PAIR)
            .filter(|window| !window.is_empty())
            .map(str::to_owned),
        port: resolved.port,
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_tailscale_docker_and_bridge_interfaces_are_skipped() {
        for name in [
            "lo",
            "tailscale0",
            "utun3",
            "docker0",
            "br-1a2b3c",
            "veth12ab",
        ] {
            assert!(is_skipped_interface(name), "{name}");
        }
        for name in [
            "eth0", "enp3s0", "wlan0", "wlp2s0", "en0", "bridge0", "lowpan0",
        ] {
            assert!(!is_skipped_interface(name), "{name}");
        }
    }
}
