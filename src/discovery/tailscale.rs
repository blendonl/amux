use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::ffi::OsString;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU16;
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::net::{TcpSocket, TcpStream};
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::{interval, SourceContext, SourceStatus};
use crate::cluster::{Candidate, Cluster, Listener};
use crate::protocol::{SourceState, Via};

pub const PROGRAM_ENV: &str = "AMUX_TAILSCALE";
const DEFAULT_PROGRAM: &str = "tailscale";
const DEFAULT_SOCKET: &str = "default";
const DEFAULT_PORT: u16 = 7447;
const FIRST_DERIVED_PORT: u16 = 7448;
const DERIVED_PORTS: u32 = 500;
const FNV_OFFSET: u32 = 0x811c_9dc5;
const FNV_PRIME: u32 = 0x0100_0193;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const WHOIS_LIFETIME: Duration = Duration::from_secs(5);
const RUNNING: &str = "Running";
const SERVER_SYSTEMS: [&str; 4] = ["linux", "macOS", "freebsd", "openbsd"];
const NOT_INSTALLED: &str = "tailscale is not installed";

pub async fn run(context: SourceContext, status: SourceStatus) {
    let every = match interval() {
        Ok(every) => every,
        Err(err) => {
            warn!("not looking for servers on the tailnet: {err:#}");
            status.set(SourceState::Unavailable(format!("{err:#}")));
            return;
        }
    };
    let config = &context.options.config;
    let tailnet = Arc::new(Tailnet::new(program(), config.tailscale_tags.clone()));
    context.cluster.use_tailnet(Arc::clone(&tailnet));
    let port = config
        .tailscale_port
        .map_or_else(|| port(&context.options.socket_name), NonZeroU16::get);
    let mut listener = TailnetListener::new(port);
    let mut stopping = context.stopping.clone();
    let mut reported = None;
    loop {
        let (state, own, candidates) = match tailnet.poll(port).await {
            Seen::NotInstalled => {
                info!("{NOT_INSTALLED}, not looking for servers on the tailnet");
                status.set(SourceState::Unavailable(NOT_INSTALLED.into()));
                return;
            }
            Seen::Up { own, candidates } => {
                report(&mut reported, "tailscale is running".into());
                let machines = candidates.len();
                (SourceState::RunningWith { machines }, Some(own), candidates)
            }
            Seen::Down(reason) => {
                report(&mut reported, reason);
                (SourceState::NotRunning, None, Vec::new())
            }
            Seen::Unreadable(reason) => {
                report(&mut reported, reason.clone());
                (SourceState::Unavailable(reason), None, Vec::new())
            }
        };
        status.set(state);
        let address = own.as_ref().and_then(|own| own.address);
        tailnet.update(own);
        listener.follow(&context.cluster, address).await;
        context.cluster.discovered(Via::Tailscale, candidates);

        tokio::select! {
            () = tokio::time::sleep(every) => {}
            _ = stopping.wait_for(|stop| *stop) => break,
        }
    }
}

fn port(socket_name: &str) -> u16 {
    if socket_name == DEFAULT_SOCKET {
        return DEFAULT_PORT;
    }
    let hash = socket_name.bytes().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(FNV_PRIME)
    });
    FIRST_DERIVED_PORT + (hash % DERIVED_PORTS) as u16
}

fn is_tailnet_range(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => {
            let [first, second, ..] = ip.octets();
            first == 100 && (64..128).contains(&second)
        }
        IpAddr::V6(ip) => matches!(ip.segments(), [0xfd7a, 0x115c, 0xa1e0, ..]),
    }
}

pub struct Tailnet {
    program: OsString,
    tags: Vec<String>,
    own: Mutex<Option<Own>>,
    lookups: Mutex<HashMap<IpAddr, Lookup>>,
}

#[derive(Debug, Clone)]
struct Own {
    node: String,
    user: i64,
    tagged: bool,
    address: Option<Ipv4Addr>,
    known: BTreeSet<IpAddr>,
}

struct Lookup {
    at: Instant,
    node: Option<WhoisNode>,
}

enum Seen {
    Up {
        own: Own,
        candidates: Vec<Candidate>,
    },
    Down(String),
    Unreadable(String),
    NotInstalled,
}

impl Tailnet {
    fn new(program: OsString, tags: Vec<String>) -> Self {
        Self {
            program,
            tags,
            own: Mutex::new(None),
            lookups: Mutex::default(),
        }
    }

    pub async fn vouches_for(&self, remote: IpAddr) -> bool {
        let remote = remote.to_canonical();
        let Some(own) = self.own().clone() else {
            return false;
        };
        if !own.is_tailnet(remote) {
            return false;
        }
        let Some(node) = self.whois(remote).await else {
            info!(%remote, "not vouching for a peer that tailscale whois does not know");
            return false;
        };
        if node.stable_id == own.node {
            info!(%remote, node = %node.name, "not vouching for this machine's own tailnet node");
            return false;
        }
        if !own.accepts(node.user, node.tags(), &self.tags) {
            info!(
                %remote,
                node = %node.name,
                "not vouching for a tailnet node that has neither this machine's owner nor an allowed tag"
            );
            return false;
        }
        debug!(%remote, node = %node.name, "the tailnet vouches for the peer");
        true
    }

    pub async fn connect(&self, endpoint: &str) -> io::Result<TcpStream> {
        let bound = endpoint
            .parse::<SocketAddr>()
            .ok()
            .and_then(|remote| Some((remote, self.source_for(remote.ip())?)));
        let Some((remote, source)) = bound else {
            return TcpStream::connect(endpoint).await;
        };
        let socket = TcpSocket::new_v4()?;
        socket.bind(SocketAddr::from((source, 0)))?;
        socket.connect(remote).await
    }

    fn source_for(&self, remote: IpAddr) -> Option<Ipv4Addr> {
        let remote = remote.to_canonical();
        let own = self.own();
        let own = own.as_ref()?;
        if remote.is_ipv4() && own.is_tailnet(remote) {
            own.address
        } else {
            None
        }
    }

    fn read(&self, status: &Status, port: u16) -> Seen {
        if status.backend_state != RUNNING {
            return Seen::Down(format!(
                "tailscale is not running, its state is {}",
                status.backend_state
            ));
        }
        let Some(own) = status.own.as_ref() else {
            return Seen::Unreadable("tailscale status names no node for this machine".into());
        };
        let peers = || status.peer.iter().flat_map(BTreeMap::values);
        let own = Own {
            node: own.id.clone(),
            user: own.user_id,
            tagged: !own.tags().is_empty(),
            address: own.ipv4(),
            known: peers()
                .chain([own])
                .flat_map(|node| node.tailscale_ips.iter().flatten())
                .map(|ip| ip.to_canonical())
                .collect(),
        };
        let candidates = peers()
            .filter(|peer| {
                peer.online
                    && peer.id != own.node
                    && SERVER_SYSTEMS.contains(&peer.os.as_str())
                    && own.accepts(peer.user_id, peer.tags(), &self.tags)
            })
            .filter_map(|peer| {
                let ip = peer.ipv4()?;
                Some(Candidate::new(
                    peer.host_name.clone(),
                    format!("tcp://{ip}:{port}"),
                ))
            })
            .collect();
        Seen::Up { own, candidates }
    }

    fn update(&self, own: Option<Own>) {
        *self.own() = own;
    }

    async fn poll(&self, port: u16) -> Seen {
        let output = match self.run(&["status", "--json"]).await {
            Ok(output) => output,
            Err(err) if is_not_installed(&err) => return Seen::NotInstalled,
            Err(err) => return Seen::Down(format!("{err:#}")),
        };
        match serde_json::from_slice::<Status>(&output) {
            Ok(status) => self.read(&status, port),
            Err(err) => Seen::Unreadable(format!(
                "the output of `tailscale status` does not parse: {err}"
            )),
        }
    }

    async fn whois(&self, ip: IpAddr) -> Option<WhoisNode> {
        let cached = self
            .lookups()
            .get(&ip)
            .filter(|lookup| lookup.at.elapsed() < WHOIS_LIFETIME)
            .map(|lookup| lookup.node.clone());
        if let Some(node) = cached {
            return node;
        }
        let looked_up = self
            .run(&["whois", "--json", &ip.to_string()])
            .await
            .and_then(|output| {
                serde_json::from_slice::<Whois>(&output)
                    .context("parsing the output of `tailscale whois`")
            });
        let node = match looked_up {
            Ok(whois) => Some(whois.node),
            Err(err) => {
                info!(%ip, "tailscale whois failed: {err:#}");
                None
            }
        };
        let mut lookups = self.lookups();
        lookups.retain(|_, lookup| lookup.at.elapsed() < WHOIS_LIFETIME);
        lookups.insert(
            ip,
            Lookup {
                at: Instant::now(),
                node: node.clone(),
            },
        );
        node
    }

    async fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        let command = format!("tailscale {}", args.join(" "));
        let running = Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(COMMAND_TIMEOUT, running)
            .await
            .with_context(|| {
                format!(
                    "`{command}` did not finish within {} s",
                    COMMAND_TIMEOUT.as_secs()
                )
            })?
            .with_context(|| format!("running `{command}`"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = stderr
                .lines()
                .map(str::trim)
                .rev()
                .find(|line| !line.is_empty())
                .unwrap_or("it printed no error");
            bail!("`{command}` failed ({}): {reason}", output.status);
        }
        Ok(output.stdout)
    }

    fn own(&self) -> MutexGuard<'_, Option<Own>> {
        self.own.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lookups(&self) -> MutexGuard<'_, HashMap<IpAddr, Lookup>> {
        self.lookups.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Own {
    fn accepts(&self, user: i64, tags: &[String], allowed: &[String]) -> bool {
        (!self.tagged && user == self.user) || tags.iter().any(|tag| allowed.contains(tag))
    }

    fn is_tailnet(&self, ip: IpAddr) -> bool {
        is_tailnet_range(ip) || self.known.contains(&ip)
    }
}

struct TailnetListener {
    port: u16,
    bound: Option<(Ipv4Addr, Listener)>,
    failed: Option<Ipv4Addr>,
}

impl TailnetListener {
    fn new(port: u16) -> Self {
        Self {
            port,
            bound: None,
            failed: None,
        }
    }

    async fn follow(&mut self, cluster: &Arc<Cluster>, wanted: Option<Ipv4Addr>) {
        if self.bound.as_ref().map(|(ip, _)| *ip) == wanted {
            return;
        }
        if let Some((_, closed)) = self.bound.take() {
            info!(address = %closed.local_addr(), "closed the tailnet listener");
        }
        let Some(ip) = wanted else {
            self.failed = None;
            return;
        };
        let address = SocketAddr::from((ip, self.port));
        match Listener::bind(cluster, address).await {
            Ok(listener) => {
                info!(%address, "listening for peers on the tailnet");
                self.failed = None;
                self.bound = Some((ip, listener));
            }
            Err(err) if self.failed == Some(ip) => {
                debug!("still not listening for peers on the tailnet: {err:#}");
            }
            Err(err) => {
                warn!("not listening for peers on the tailnet: {err:#}");
                self.failed = Some(ip);
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    backend_state: String,
    #[serde(rename = "Self")]
    own: Option<PeerStatus>,
    peer: Option<BTreeMap<String, PeerStatus>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct PeerStatus {
    #[serde(rename = "ID")]
    id: String,
    #[serde(default)]
    host_name: String,
    #[serde(rename = "OS", default)]
    os: String,
    #[serde(rename = "UserID")]
    user_id: i64,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<IpAddr>>,
    #[serde(default)]
    online: bool,
    tags: Option<Vec<String>>,
}

impl PeerStatus {
    fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }

    fn ipv4(&self) -> Option<Ipv4Addr> {
        self.tailscale_ips
            .iter()
            .flatten()
            .find_map(|ip| match ip.to_canonical() {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            })
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Whois {
    node: WhoisNode,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisNode {
    #[serde(rename = "StableID")]
    stable_id: String,
    #[serde(default)]
    name: String,
    user: i64,
    tags: Option<Vec<String>>,
}

impl WhoisNode {
    fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }
}

fn program() -> OsString {
    env::var_os(PROGRAM_ENV)
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| DEFAULT_PROGRAM.into())
}

fn is_not_installed(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>()
        .is_some_and(|err| err.kind() == io::ErrorKind::NotFound)
}

fn report(reported: &mut Option<String>, now: String) {
    if reported.as_ref() != Some(&now) {
        info!("{now}");
        *reported = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const ME: i64 = 5998409928532361;
    const SOMEONE_ELSE: i64 = 7001;
    const TAGGED_DEVICES: i64 = 9001;

    fn peer(name: &str, ip: &str, os: &str, user: i64, online: bool) -> serde_json::Value {
        json!({
            "ID": format!("n{name}CNTRL"),
            "HostName": name,
            "OS": os,
            "UserID": user,
            "TailscaleIPs": [ip, "fd7a:115c:a1e0::1"],
            "Online": online,
            "Tags": null,
        })
    }

    fn status(own: serde_json::Value, peers: &[serde_json::Value]) -> Status {
        let peers: serde_json::Map<String, serde_json::Value> = peers
            .iter()
            .enumerate()
            .map(|(index, peer)| (format!("nodekey:{index}"), peer.clone()))
            .collect();
        serde_json::from_value(json!({
            "BackendState": "Running",
            "Self": own,
            "Peer": peers,
        }))
        .unwrap()
    }

    fn tailnet(tags: &[&str]) -> Tailnet {
        Tailnet::new(
            DEFAULT_PROGRAM.into(),
            tags.iter().map(|tag| tag.to_string()).collect(),
        )
    }

    fn candidates(seen: Seen) -> Vec<(String, String)> {
        match seen {
            Seen::Up { candidates, .. } => candidates
                .into_iter()
                .map(|candidate| (candidate.name, candidate.address))
                .collect(),
            _ => panic!("tailscale is not up"),
        }
    }

    #[test]
    fn the_default_socket_gets_7447_and_other_sockets_a_stable_port_nearby() {
        assert_eq!(port("default"), 7447);
        assert_eq!(port("work"), port("work"));
        assert_ne!(port("work"), port("play"));
        for name in ["work", "play", "amux.sock", "", "a-much-longer-socket-name"] {
            let derived = port(name);
            assert!((7448..7948).contains(&derived), "{name}: {derived}");
        }
    }

    #[test]
    fn only_the_carrier_grade_nat_range_and_the_tailscale_prefix_are_tailnet() {
        for ip in [
            "100.64.0.1",
            "100.100.9.29",
            "100.127.255.254",
            "fd7a:115c:a1e0::7a34:91d",
            "::ffff:100.64.0.1",
        ] {
            assert!(is_tailnet_range(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "100.63.255.255",
            "100.128.0.1",
            "192.168.0.24",
            "127.0.0.1",
            "fd7a:115c:a1e1::1",
            "::1",
        ] {
            assert!(!is_tailnet_range(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn online_servers_of_this_user_are_candidates() {
        let status = status(
            peer("notpc", "100.100.9.29", "linux", ME, true),
            &[
                peer("desk", "100.64.0.2", "linux", ME, true),
                peer("mac", "100.64.0.3", "macOS", ME, true),
                peer("nas", "100.64.0.4", "freebsd", ME, true),
                peer("phone", "100.64.0.5", "android", ME, true),
                peer("windows", "100.64.0.6", "windows", ME, true),
                peer("asleep", "100.64.0.7", "linux", ME, false),
                peer("friend", "100.64.0.8", "linux", SOMEONE_ELSE, true),
            ],
        );

        let mut found = candidates(tailnet(&[]).read(&status, 7447));
        found.sort();

        assert_eq!(
            found,
            [
                ("desk".to_owned(), "tcp://100.64.0.2:7447".to_owned()),
                ("mac".to_owned(), "tcp://100.64.0.3:7447".to_owned()),
                ("nas".to_owned(), "tcp://100.64.0.4:7447".to_owned()),
            ]
        );
    }

    #[test]
    fn this_machine_and_every_tailnet_address_are_remembered() {
        let status = status(
            peer("notpc", "100.100.9.29", "linux", ME, true),
            &[peer("phone", "100.64.0.5", "android", ME, false)],
        );

        let Seen::Up { own, .. } = tailnet(&[]).read(&status, 7447) else {
            panic!("tailscale is down");
        };

        assert_eq!(own.node, "nnotpcCNTRL");
        assert_eq!(own.user, ME);
        assert!(!own.tagged);
        assert_eq!(own.address, Some(Ipv4Addr::new(100, 100, 9, 29)));
        assert!(own.is_tailnet("100.64.0.5".parse().unwrap()));
        assert!(!own.is_tailnet("192.168.0.10".parse().unwrap()));
    }

    #[test]
    fn a_tailnet_that_is_not_running_has_no_candidates() {
        let status: Status = serde_json::from_value(json!({
            "BackendState": "NeedsLogin",
            "Self": null,
            "Peer": null,
        }))
        .unwrap();

        let Seen::Down(reason) = tailnet(&[]).read(&status, 7447) else {
            panic!("a logged out tailnet counted as up");
        };

        assert_eq!(reason, "tailscale is not running, its state is NeedsLogin");
    }

    #[test]
    fn tagged_machines_are_accepted_by_their_tags_only() {
        let own = |tagged| Own {
            node: "nme".into(),
            user: if tagged { TAGGED_DEVICES } else { ME },
            tagged,
            address: None,
            known: BTreeSet::new(),
        };
        let allowed = ["tag:amux".to_owned()];
        let amux = ["tag:amux".to_owned()];
        let ci = ["tag:ci".to_owned()];

        assert!(own(false).accepts(ME, &[], &[]));
        assert!(!own(false).accepts(SOMEONE_ELSE, &[], &[]));
        assert!(own(false).accepts(TAGGED_DEVICES, &amux, &allowed));
        assert!(!own(false).accepts(TAGGED_DEVICES, &ci, &allowed));
        assert!(own(true).accepts(TAGGED_DEVICES, &amux, &allowed));
        assert!(!own(true).accepts(TAGGED_DEVICES, &ci, &allowed));
        assert!(!own(true).accepts(TAGGED_DEVICES, &amux, &[]));
    }

    #[test]
    fn whois_output_names_the_node_and_its_owner() {
        let whois: Whois = serde_json::from_value(json!({
            "Node": {
                "ID": 8773643483275414_i64,
                "StableID": "nT5pgjjbWB21CNTRL",
                "Name": "notpc-1.kangaroo-wall.ts.net.",
                "User": ME,
                "Tags": null,
                "Addresses": ["100.100.9.29/32", "fd7a:115c:a1e0::7a34:91d/128"],
            },
            "UserProfile": {"ID": ME, "LoginName": "someone@example.com"},
            "CapMap": {},
        }))
        .unwrap();

        assert_eq!(whois.node.stable_id, "nT5pgjjbWB21CNTRL");
        assert_eq!(whois.node.name, "notpc-1.kangaroo-wall.ts.net.");
        assert_eq!(whois.node.user, ME);
        assert!(whois.node.tags().is_empty());
    }
}
