use std::env;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use if_addrs::Interface;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::directory::{DirectoryLan, LAN_DIR_ENV};
use super::mdns::{self, MdnsLan};
use super::{interval, Advertisement, DiscoveryOptions, LanDiscovery, SourceContext, SourceStatus};
use crate::cluster::Candidate;
use crate::protocol::{SourceState, Via, PROTOCOL_MAJOR};

const LAN_SCHEME: &str = "lan://";
const LINK_LOCAL_MASK: u16 = 0xffc0;
const LINK_LOCAL_PREFIX: u16 = 0xfe80;

pub struct Lan {
    listening: watch::Receiver<Option<SocketAddr>>,
    seen: watch::Sender<Vec<Advertisement>>,
}

impl Lan {
    pub fn new(listening: watch::Receiver<Option<SocketAddr>>) -> Self {
        Self {
            listening,
            seen: watch::channel(Vec::new()).0,
        }
    }

    pub async fn listening(&self, patience: Duration) -> Result<SocketAddr> {
        let mut listening = self.listening.clone();
        let bound = tokio::time::timeout(patience, listening.wait_for(Option::is_some))
            .await
            .context("the LAN listener did not start, see the server log")?
            .context("the LAN listener stopped")?;
        bound.context("the LAN listener stopped")
    }

    pub async fn find(
        &self,
        patience: Duration,
        mut wanted: impl FnMut(&Advertisement) -> bool,
    ) -> Option<Advertisement> {
        let mut seen = self.seen.subscribe();
        let found = tokio::time::timeout(
            patience,
            seen.wait_for(|advertisements| advertisements.iter().any(&mut wanted)),
        )
        .await
        .ok()?
        .ok()?;
        found
            .iter()
            .find(|advertisement| wanted(advertisement))
            .cloned()
    }

    pub fn seen(&self) -> Vec<Advertisement> {
        self.seen.borrow().clone()
    }
}

pub async fn run(context: SourceContext, status: SourceStatus, lan: Arc<Lan>) {
    let backend = match backend(&context.options) {
        Ok(backend) => backend,
        Err(err) => {
            warn!("LAN discovery is not running: {err:#}");
            status.set(SourceState::Unavailable(format!("{err:#}")));
            return;
        }
    };
    status.set(SourceState::Running);
    info!("LAN discovery started");
    let stopped = follow(&context, backend.as_ref(), &lan).await;
    backend.shutdown();
    lan.seen.send_replace(Vec::new());
    if !stopped {
        warn!("LAN discovery stopped unexpectedly");
        status.set(SourceState::Unavailable(
            "LAN discovery stopped unexpectedly, see the server log".into(),
        ));
        context.cluster.discovered(Via::Lan, Vec::new());
    }
}

pub fn backend(options: &DiscoveryOptions) -> Result<Arc<dyn LanDiscovery>> {
    match env::var_os(LAN_DIR_ENV).filter(|dir| !dir.is_empty()) {
        Some(dir) => Ok(DirectoryLan::start(
            dir.into(),
            interval(&options.settings)?,
        )?),
        None => {
            let service = mdns::service(&options.lan.mdns_service).context("mDNS failed")?;
            Ok(MdnsLan::start(service).context("mDNS failed")?)
        }
    }
}

pub fn endpoints(advertisement: &Advertisement) -> Vec<SocketAddr> {
    let mut addresses: Vec<IpAddr> = advertisement
        .addresses
        .iter()
        .copied()
        .filter(is_dialable)
        .collect();
    addresses.sort_by_key(IpAddr::is_ipv6);
    addresses
        .into_iter()
        .map(|ip| SocketAddr::new(ip, advertisement.port))
        .collect()
}

pub fn lan_interfaces() -> Vec<Interface> {
    let mut interfaces: Vec<Interface> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|interface| {
            !interface.is_loopback()
                && !mdns::is_skipped_interface(&interface.name)
                && is_dialable(&interface.ip())
        })
        .collect();
    interfaces.sort_by_key(|interface| interface.ip().is_ipv6());
    interfaces
}

async fn follow(context: &SourceContext, backend: &dyn LanDiscovery, lan: &Lan) -> bool {
    let cluster = &context.cluster;
    let mut listening = lan.listening.clone();
    let mut windows = cluster.pairing().watch();
    let mut changes = cluster.watch();
    let mut browsing = backend.browse();
    let mut stopping = context.stopping.clone();
    let mut advertised: Option<Advertisement> = None;
    let mut listed: Option<Vec<Candidate>> = None;
    loop {
        changes.borrow_and_update();
        let port = listening.borrow_and_update().map(|address| address.port());
        let window = windows.borrow_and_update().clone();
        let wanted = port.map(|port| advertisement(context, port, window));
        if wanted != advertised {
            let updated = match &wanted {
                Some(advertisement) => backend.advertise(advertisement),
                None => backend.withdraw(),
            };
            match updated {
                Ok(()) => {
                    match &wanted {
                        Some(advertisement) => debug!(
                            port = advertisement.port,
                            pairing = ?advertisement.pairing,
                            "advertising this server on the LAN"
                        ),
                        None => debug!("stopped advertising this server on the LAN"),
                    }
                    advertised = wanted;
                }
                Err(err) => warn!("updating the LAN advertisement failed: {err:#}"),
            }
        }

        let seen = browsing.borrow_and_update().clone();
        let candidates = candidates(context, &seen);
        lan.seen.send_replace(seen);
        if listed.as_ref() != Some(&candidates) {
            cluster.discovered(Via::Lan, candidates.clone());
            listed = Some(candidates);
        }

        tokio::select! {
            changed = listening.changed() => if changed.is_err() {
                return false;
            },
            changed = windows.changed() => if changed.is_err() {
                return false;
            },
            changed = changes.changed() => if changed.is_err() {
                return false;
            },
            changed = browsing.changed() => if changed.is_err() {
                return false;
            },
            _ = stopping.wait_for(|stop| *stop) => return true,
        }
    }
}

fn advertisement(context: &SourceContext, port: u16, pairing: Option<String>) -> Advertisement {
    let identity = context.cluster.identity();
    Advertisement {
        id: identity.id,
        name: identity.name.clone(),
        key: context.cluster.public_key(),
        cluster: context.options.socket_name.clone(),
        proto: PROTOCOL_MAJOR,
        pairing,
        port,
        addresses: Vec::new(),
    }
}

fn candidates(context: &SourceContext, seen: &[Advertisement]) -> Vec<Candidate> {
    let cluster = &context.cluster;
    let own = cluster.identity().id;
    seen.iter()
        .filter(|advertisement| {
            advertisement.id != own && advertisement.cluster == context.options.socket_name
        })
        .map(|advertisement| Candidate {
            name: advertisement.name.clone(),
            address: format!("{LAN_SCHEME}{}", advertisement.id),
            server: Some(advertisement.id),
            key: Some(advertisement.key),
            pairing: advertisement.pairing.clone(),
            dialable: cluster.trusts(advertisement.id, &advertisement.key),
            endpoints: endpoints(advertisement),
        })
        .collect()
}

fn is_dialable(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !ip.is_unspecified() && !ip.is_multicast(),
        IpAddr::V6(ip) => {
            let link_local = ip.segments()[0] & LINK_LOCAL_MASK == LINK_LOCAL_PREFIX;
            !link_local && !ip.is_unspecified() && !ip.is_multicast()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ServerId;
    use crate::protocol::PublicKey;

    fn advertisement(addresses: &[&str]) -> Advertisement {
        Advertisement {
            id: ServerId::random().unwrap(),
            name: "desk".into(),
            key: PublicKey([5; 32]),
            cluster: "default".into(),
            proto: PROTOCOL_MAJOR,
            pairing: None,
            port: 40123,
            addresses: addresses
                .iter()
                .map(|address| address.parse().unwrap())
                .collect(),
        }
    }

    #[test]
    fn endpoints_put_ipv4_first_and_skip_addresses_that_cannot_be_dialed() {
        let advertisement = advertisement(&[
            "fd00::5",
            "fe80::2",
            "192.168.0.10",
            "0.0.0.0",
            "10.0.0.3",
            "ff02::fb",
        ]);

        let endpoints: Vec<String> = endpoints(&advertisement)
            .iter()
            .map(ToString::to_string)
            .collect();

        assert_eq!(
            endpoints,
            ["192.168.0.10:40123", "10.0.0.3:40123", "[fd00::5]:40123"]
        );
    }
}
