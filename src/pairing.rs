use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex};
use tokio::time::Instant;
use tracing::{debug, info};

use crate::cluster::{noise, Cluster, NoiseKey, Secured, Voucher, Witnessed};
use crate::discovery::{lan, Discovery};
use crate::identity::ServerId;
use crate::protocol::{
    self, ClientMessage, Duplex, PublicKey, ServerMessage, SourceState, TcpKind,
};

pub type PairingClient = Duplex<ClientMessage, ServerMessage>;

pub const ATTEMPTS: u8 = 3;
const WINDOW_ID_LEN: usize = 2;
const SECRET_GROUP_LEN: usize = 4;
const WINDOW_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const DIGITS: &[u8] = b"0123456789";
const CODE_EXAMPLE: &str = "k7-4821-9930";
const LISTEN_PATIENCE: Duration = Duration::from_secs(5);
const LOOKUP_PATIENCE: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const JOINER_LABEL: &[u8] = b"amux pair joiner";
const HOST_LABEL: &[u8] = b"amux pair host";
const WRONG_CODE: &str = "wrong code";

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    window: String,
    secret: String,
}

impl Code {
    pub fn generate() -> Result<Self> {
        let pick = |alphabet: &[u8], len: usize| -> Result<String> {
            (0..len)
                .map(|_| random_below(alphabet.len()).map(|index| char::from(alphabet[index])))
                .collect()
        };
        Ok(Self {
            window: pick(WINDOW_ALPHABET, WINDOW_ID_LEN)?,
            secret: pick(DIGITS, 2 * SECRET_GROUP_LEN)?,
        })
    }

    pub fn window(&self) -> &str {
        &self.window
    }
}

impl FromStr for Code {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let normalized = text.trim().to_ascii_lowercase();
        let parts: Vec<&str> = normalized.split('-').collect();
        let digits = |group: &str| {
            group.len() == SECRET_GROUP_LEN && group.bytes().all(|byte| byte.is_ascii_digit())
        };
        match parts[..] {
            [window, first, second]
                if window.len() == WINDOW_ID_LEN
                    && window.bytes().all(|byte| WINDOW_ALPHABET.contains(&byte))
                    && digits(first)
                    && digits(second) =>
            {
                Ok(Self {
                    window: window.to_owned(),
                    secret: format!("{first}{second}"),
                })
            }
            _ => bail!("{text:?} is not a pairing code like {CODE_EXAMPLE}"),
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (first, second) = self.secret.split_at(SECRET_GROUP_LEN);
        write!(f, "{}-{first}-{second}", self.window)
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Code({}-****-****)", self.window)
    }
}

pub struct Pairing {
    window: Mutex<Option<Window>>,
    open: watch::Sender<Option<String>>,
    connection: AsyncMutex<()>,
}

struct Window {
    code: Code,
    expires: Instant,
    attempts: u8,
    host: mpsc::UnboundedSender<HostEvent>,
}

#[derive(Debug)]
enum HostEvent {
    Paired(Peer),
    Failed { reason: String, attempts_left: u8 },
    Closed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Peer {
    id: ServerId,
    name: String,
    key: PublicKey,
}

#[derive(Debug)]
struct Attempt {
    code: Code,
    attempts_left: u8,
}

impl Default for Pairing {
    fn default() -> Self {
        Self {
            window: Mutex::new(None),
            open: watch::channel(None).0,
            connection: AsyncMutex::new(()),
        }
    }
}

impl Pairing {
    pub fn watch(&self) -> watch::Receiver<Option<String>> {
        self.open.subscribe()
    }

    pub fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    fn open(
        &self,
        code: &Code,
        lifetime: Duration,
        host: mpsc::UnboundedSender<HostEvent>,
    ) -> Result<()> {
        let mut window = self.window();
        if window.as_ref().is_some_and(|window| !window.is_expired()) {
            bail!("another `amux pair` is already waiting on this server");
        }
        *window = Some(Window {
            code: code.clone(),
            expires: Instant::now() + lifetime,
            attempts: ATTEMPTS,
            host,
        });
        self.open.send_replace(Some(code.window.clone()));
        Ok(())
    }

    fn close(&self, code: &Code) -> Option<Window> {
        let mut window = self.window();
        if window.as_ref().is_some_and(|window| window.code == *code) {
            self.open.send_replace(None);
            return window.take();
        }
        None
    }

    fn attempt(&self) -> Option<Attempt> {
        let mut window = self.window();
        let open = window
            .as_mut()
            .filter(|window| !window.is_expired() && window.attempts > 0)?;
        open.attempts -= 1;
        Some(Attempt {
            code: open.code.clone(),
            attempts_left: open.attempts,
        })
    }

    fn failed(&self, attempt: &Attempt, reason: String) {
        if attempt.attempts_left == 0 {
            if let Some(window) = self.close(&attempt.code) {
                let closed = format!("{reason}, and that was the last of {ATTEMPTS} attempts");
                let _ = window.host.send(HostEvent::Closed(closed));
            }
            return;
        }
        let window = self.window();
        if let Some(window) = window.as_ref().filter(|window| window.code == attempt.code) {
            let _ = window.host.send(HostEvent::Failed {
                reason,
                attempts_left: attempt.attempts_left,
            });
        }
    }

    fn window(&self) -> MutexGuard<'_, Option<Window>> {
        self.window.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Window {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires
    }
}

struct OpenWindow<'a> {
    pairing: &'a Pairing,
    code: &'a Code,
}

impl Drop for OpenWindow<'_> {
    fn drop(&mut self) {
        if self.pairing.close(self.code).is_some() {
            info!(window = self.code.window(), "closed the pairing window");
        }
    }
}

pub async fn open(discovery: &Discovery, new_key: bool, client: &mut PairingClient) -> Result<()> {
    let context = discovery.context();
    if !context.options.settings.lan {
        bail!("pairing needs LAN discovery, which is off in the config");
    }
    let cluster = &context.cluster;
    if new_key {
        cluster.rotate_key()?;
    }
    let code = Code::generate()?;
    let (events, mut happened) = mpsc::unbounded_channel();
    let lifetime = context.options.lan.pairing_window();
    let pairing = cluster.pairing();
    pairing.open(&code, lifetime, events)?;
    let _window = OpenWindow {
        pairing,
        code: &code,
    };
    let address = discovery.lan().listening(LISTEN_PATIENCE).await?;
    info!(
        window = code.window(),
        port = address.port(),
        "opened a pairing window"
    );
    let opened = ServerMessage::PairingOpen {
        code: code.to_string(),
        expires_in_secs: lifetime.as_secs(),
    };
    reply(client, opened).await?;

    let expired = tokio::time::sleep(lifetime);
    tokio::pin!(expired);
    loop {
        tokio::select! {
            event = happened.recv() => match event {
                Some(HostEvent::Paired(peer)) => {
                    let paired = ServerMessage::Paired {
                        name: peer.name,
                        id: peer.id,
                        fingerprint: peer.key.fingerprint(),
                    };
                    return reply(client, paired).await;
                }
                Some(HostEvent::Failed { reason, attempts_left }) => {
                    reply(client, ServerMessage::PairingAttemptFailed { reason, attempts_left }).await?;
                }
                Some(HostEvent::Closed(reason)) => {
                    return reply(client, ServerMessage::PairingClosed { reason }).await;
                }
                None => {
                    let reason = "the pairing window closed".to_owned();
                    return reply(client, ServerMessage::PairingClosed { reason }).await;
                }
            },
            message = client.incoming.recv() => match message {
                Some(message) => debug!(?message, "ignoring a client message while pairing"),
                None => {
                    info!(window = code.window(), "`amux pair` went away");
                    return Ok(());
                }
            },
            () = &mut expired => {
                info!(window = code.window(), "the pairing code expired");
                let reason = "the pairing code expired".to_owned();
                return reply(client, ServerMessage::PairingClosed { reason }).await;
            }
        }
    }
}

pub async fn host<S>(
    cluster: &Arc<Cluster>,
    stream: S,
    remote: SocketAddr,
) -> Result<Option<Secured>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let pairing = cluster.pairing();
    if !pairing.is_open() {
        info!(%remote, "closing a pairing connection, no pairing window is open");
        return Ok(None);
    }
    let Ok(_connection) = pairing.connection.try_lock() else {
        info!(%remote, "closing a pairing connection, another one is running");
        return Ok(None);
    };
    let key = cluster.noise_key();
    let mut secured = noise::respond(stream, &key, TcpKind::Pair).await?;
    let theirs = match receive(&mut secured.stream).await? {
        PairMessage::Spake(theirs) => theirs,
        other => bail!("expected a SPAKE2 message, got {}", other.kind()),
    };
    let Some(attempt) = pairing.attempt() else {
        let _ = send(&mut secured.stream, &PairMessage::Closed).await;
        info!(%remote, "closing a pairing connection, the pairing window closed");
        return Ok(None);
    };
    info!(
        %remote,
        key = %secured.remote,
        attempts_left = attempt.attempts_left,
        "a machine is trying the pairing code"
    );
    let peer = match confirm_joiner(cluster, &key, &mut secured, &attempt.code, &theirs).await {
        Ok(Some(peer)) => peer,
        Ok(None) => {
            info!(%remote, "a machine tried a wrong pairing code");
            pairing.failed(&attempt, "a wrong code was tried".into());
            return Ok(None);
        }
        Err(err) => {
            pairing.failed(&attempt, format!("a pairing attempt failed: {err:#}"));
            return Err(err);
        }
    };
    let Some(window) = pairing.close(&attempt.code) else {
        let refusal = "the pairing window closed".to_owned();
        let _ = send(&mut secured.stream, &PairMessage::Refused(refusal)).await;
        bail!("the pairing window closed during the attempt");
    };
    if let Err(err) = trust(cluster, &peer) {
        let refusal = PairMessage::Refused(format!("the other machine refused: {err:#}"));
        let _ = send(&mut secured.stream, &refusal).await;
        let _ = window.host.send(HostEvent::Closed(format!("{err:#}")));
        return Err(err);
    }
    send(&mut secured.stream, &PairMessage::Accepted).await?;
    info!(peer = %peer.name, id = %peer.id, key = %peer.key, "paired");
    let _ = window.host.send(HostEvent::Paired(peer));
    Ok(Some(secured))
}

pub async fn join(
    discovery: &Discovery,
    code: String,
    host: Option<String>,
    new_key: bool,
    client: &mut PairingClient,
) -> Result<()> {
    let code: Code = code.parse()?;
    let cluster = Arc::clone(&discovery.context().cluster);
    if new_key {
        cluster.rotate_key()?;
    }
    let endpoints = match &host {
        Some(host) => vec![host_endpoint(discovery, host)?],
        None => advertised_endpoints(discovery, &code).await?,
    };
    let key = cluster.noise_key();
    let (secured, peer) = tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        pair_with(&cluster, &key, &code, &endpoints),
    )
    .await
    .context("pairing timed out")??;
    info!(peer = %peer.name, id = %peer.id, key = %peer.key, "paired");
    let paired = ServerMessage::Paired {
        name: peer.name.clone(),
        id: peer.id,
        fingerprint: peer.key.fingerprint(),
    };
    tokio::spawn(async move {
        if let Err(err) = cluster.dial_secured(secured, Voucher::Pairing).await {
            info!(peer = %peer.name, "linking over the pairing connection failed: {err:#}");
        }
    });
    reply(client, paired).await
}

async fn pair_with(
    cluster: &Cluster,
    key: &NoiseKey,
    code: &Code,
    endpoints: &[String],
) -> Result<(Secured, Peer)> {
    let stream = connect(endpoints).await?;
    let mut secured = noise::initiate(stream, key, TcpKind::Pair).await.context(
        "the other machine refused the pairing connection, is `amux pair` still waiting there",
    )?;
    if cluster.forgot(&secured.remote) {
        bail!("the other machine's key was forgotten here, run `amux pair --new-key` there");
    }
    let peer = confirm_host(cluster, key, &mut secured, code).await?;
    trust(cluster, &peer)?;
    Ok((secured, peer))
}

async fn confirm_host(
    cluster: &Cluster,
    key: &NoiseKey,
    secured: &mut Secured,
    code: &Code,
) -> Result<Peer> {
    let exchange = Exchange::start(
        Side::Joiner,
        code,
        &key.public(),
        &secured.remote,
        &secured.handshake_hash,
    );
    let stream = &mut secured.stream;
    send(stream, &PairMessage::Spake(exchange.message().to_vec())).await?;
    let theirs = match receive(stream).await? {
        PairMessage::Spake(theirs) => theirs,
        PairMessage::Closed => bail!("the pairing window on the other machine is closed"),
        other => bail!("expected a SPAKE2 message, got {}", other.kind()),
    };
    let confirmed = exchange.finish(&theirs)?;
    send(stream, &PairMessage::Confirm(confirmed.mac(Side::Joiner))).await?;
    let (mac, id, name) = match receive(stream).await? {
        PairMessage::WrongCode => bail!(WRONG_CODE),
        PairMessage::Confirmed { mac, id, name } => (mac, id, name),
        other => bail!("expected the key confirmation, got {}", other.kind()),
    };
    if !confirmed.verify(Side::Host, &mac) {
        bail!("the other machine could not prove that it knows the code");
    }
    let identity = cluster.identity();
    let introduction = PairMessage::Identity {
        id: identity.id,
        name: identity.name.clone(),
    };
    send(stream, &introduction).await?;
    match receive(stream).await? {
        PairMessage::Accepted => Ok(Peer {
            id,
            name,
            key: secured.remote,
        }),
        PairMessage::Refused(reason) => bail!(reason),
        other => bail!("expected the pairing verdict, got {}", other.kind()),
    }
}

async fn confirm_joiner(
    cluster: &Cluster,
    key: &NoiseKey,
    secured: &mut Secured,
    code: &Code,
    theirs: &[u8],
) -> Result<Option<Peer>> {
    let exchange = Exchange::start(
        Side::Host,
        code,
        &key.public(),
        &secured.remote,
        &secured.handshake_hash,
    );
    let stream = &mut secured.stream;
    send(stream, &PairMessage::Spake(exchange.message().to_vec())).await?;
    let confirmed = exchange.finish(theirs);
    let mac = match receive(stream).await? {
        PairMessage::Confirm(mac) => mac,
        other => bail!("expected the key confirmation, got {}", other.kind()),
    };
    let confirmed = confirmed?;
    if !confirmed.verify(Side::Joiner, &mac) {
        let _ = send(stream, &PairMessage::WrongCode).await;
        return Ok(None);
    }
    let identity = cluster.identity();
    let confirmation = PairMessage::Confirmed {
        mac: confirmed.mac(Side::Host),
        id: identity.id,
        name: identity.name.clone(),
    };
    send(stream, &confirmation).await?;
    match receive(stream).await? {
        PairMessage::Identity { id, name } => Ok(Some(Peer {
            id,
            name,
            key: secured.remote,
        })),
        other => bail!(
            "expected the other machine's identity, got {}",
            other.kind()
        ),
    }
}

fn trust(cluster: &Cluster, peer: &Peer) -> Result<()> {
    if peer.id == cluster.identity().id {
        bail!("the other machine is this server");
    }
    if cluster.forgot(&peer.key) {
        bail!(
            "{}'s key was forgotten, run `amux pair --new-key` on {}",
            peer.name,
            peer.name
        );
    }
    if let Witnessed::HeldBy(holder) = cluster.witness(peer.id, &peer.name, peer.key) {
        bail!("{}'s key already belongs to the server {holder}", peer.name);
    }
    cluster.unforget(peer.id);
    Ok(())
}

async fn advertised_endpoints(discovery: &Discovery, code: &Code) -> Result<Vec<String>> {
    let context = discovery.context();
    if !context.options.settings.lan {
        bail!(
            "LAN discovery is off in the config, so pass --host with the other machine's address"
        );
    }
    if let SourceState::Unavailable(reason) = discovery.lan_source() {
        bail!("LAN discovery is not running ({reason}), so pass --host with the other machine's address");
    }
    let own = context.cluster.identity().id;
    let found = discovery
        .lan()
        .find(LOOKUP_PATIENCE, |advertisement| {
            advertisement.pairing.as_deref() == Some(code.window())
                && advertisement.id != own
                && advertisement.cluster == context.options.socket_name
        })
        .await
        .with_context(|| {
            format!(
                "no server on the LAN is waiting with a code that starts with {}, check the code \
                 or pass --host with the other machine's address",
                code.window()
            )
        })?;
    let endpoints = lan::endpoints(&found);
    if endpoints.is_empty() {
        bail!("{} advertises no address this machine can dial", found.name);
    }
    Ok(endpoints.iter().map(ToString::to_string).collect())
}

fn host_endpoint(discovery: &Discovery, host: &str) -> Result<String> {
    if let Ok(address) = host.parse::<SocketAddr>() {
        return Ok(address.to_string());
    }
    let ip: IpAddr = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .with_context(|| format!("--host {host:?} is not an ip[:port]"))?;
    let port = discovery
        .lan()
        .seen()
        .iter()
        .find(|advertisement| advertisement.addresses.contains(&ip))
        .map(|advertisement| advertisement.port)
        .or(Some(discovery.context().options.lan.port).filter(|port| *port != 0))
        .with_context(|| {
            format!("--host {host} names no port, pass {host}:<port> with the port `amux pair` printed there")
        })?;
    Ok(SocketAddr::new(ip, port).to_string())
}

async fn connect(endpoints: &[String]) -> Result<TcpStream> {
    let mut failure = anyhow!("there is no address to connect to");
    for endpoint in endpoints {
        debug!(%endpoint, "connecting to pair");
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(endpoint.as_str())).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(err)) => failure = anyhow!("connecting to {endpoint}: {err}"),
            Err(_) => failure = anyhow!("connecting to {endpoint} timed out"),
        }
    }
    Err(failure)
}

async fn reply(client: &PairingClient, message: ServerMessage) -> Result<()> {
    client
        .outgoing
        .send(message)
        .await
        .map_err(|_| anyhow!("`amux pair` went away"))
}

#[derive(Debug, Serialize, Deserialize)]
enum PairMessage {
    Spake(Vec<u8>),
    Confirm(Vec<u8>),
    WrongCode,
    Confirmed {
        mac: Vec<u8>,
        id: ServerId,
        name: String,
    },
    Identity {
        id: ServerId,
        name: String,
    },
    Accepted,
    Refused(String),
    Closed,
}

impl PairMessage {
    fn kind(&self) -> &'static str {
        match self {
            Self::Spake(_) => "a SPAKE2 message",
            Self::Confirm(_) | Self::Confirmed { .. } => "a key confirmation",
            Self::WrongCode => "a wrong code verdict",
            Self::Identity { .. } => "an identity",
            Self::Accepted | Self::Refused(_) => "a pairing verdict",
            Self::Closed => "a closed window notice",
        }
    }
}

async fn send<S: AsyncWrite + Unpin>(stream: &mut S, message: &PairMessage) -> Result<()> {
    protocol::write_message(stream, message)
        .await
        .context("writing to the pairing connection")
}

async fn receive<S: AsyncRead + Unpin>(stream: &mut S) -> Result<PairMessage> {
    protocol::read_message(stream)
        .await
        .context("reading from the pairing connection")?
        .context("the other machine closed the pairing connection")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Joiner,
    Host,
}

impl Side {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Joiner => JOINER_LABEL,
            Self::Host => HOST_LABEL,
        }
    }
}

struct Exchange {
    side: Side,
    spake: Spake2<Ed25519Group>,
    message: Vec<u8>,
    handshake_hash: Vec<u8>,
}

struct Confirmed {
    key: Vec<u8>,
    transcript: Vec<u8>,
}

impl Exchange {
    fn start(
        side: Side,
        code: &Code,
        ours: &PublicKey,
        theirs: &PublicKey,
        handshake_hash: &[u8],
    ) -> Self {
        let (joiner, host) = match side {
            Side::Joiner => (ours, theirs),
            Side::Host => (theirs, ours),
        };
        let password = Password::new(code.secret.as_bytes());
        let joiner = Identity::new(&identity(Side::Joiner, joiner, handshake_hash));
        let host = Identity::new(&identity(Side::Host, host, handshake_hash));
        let (spake, message) = match side {
            Side::Joiner => Spake2::<Ed25519Group>::start_a(&password, &joiner, &host),
            Side::Host => Spake2::<Ed25519Group>::start_b(&password, &joiner, &host),
        };
        Self {
            side,
            spake,
            message,
            handshake_hash: handshake_hash.to_vec(),
        }
    }

    fn message(&self) -> &[u8] {
        &self.message
    }

    fn finish(self, theirs: &[u8]) -> Result<Confirmed> {
        let key = self
            .spake
            .finish(theirs)
            .map_err(|err| anyhow!("the other machine sent a bad SPAKE2 message: {err}"))?;
        let (joiner, host) = match self.side {
            Side::Joiner => (self.message.as_slice(), theirs),
            Side::Host => (theirs, self.message.as_slice()),
        };
        Ok(Confirmed {
            key,
            transcript: [self.handshake_hash.as_slice(), joiner, host].concat(),
        })
    }
}

impl Confirmed {
    fn mac(&self, side: Side) -> Vec<u8> {
        self.hmac(side).finalize().into_bytes().to_vec()
    }

    fn verify(&self, side: Side, tag: &[u8]) -> bool {
        self.hmac(side).verify_slice(tag).is_ok()
    }

    fn hmac(&self, side: Side) -> HmacSha256 {
        let mut mac =
            HmacSha256::new_from_slice(&self.key).expect("HMAC takes a key of any length");
        mac.update(side.label());
        mac.update(&self.transcript);
        mac
    }
}

fn identity(side: Side, key: &PublicKey, handshake_hash: &[u8]) -> Vec<u8> {
    [side.label(), key.0.as_slice(), handshake_hash].concat()
}

fn random_below(bound: usize) -> Result<usize> {
    let bound = u8::try_from(bound).context("an alphabet has at most 255 letters")?;
    let limit = u8::MAX - u8::MAX % bound;
    loop {
        let mut byte = [0; 1];
        getrandom::fill(&mut byte).map_err(|err| anyhow!("reading random bytes: {err}"))?;
        if byte[0] < limit {
            return Ok(usize::from(byte[0] % bound));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    const CODE: &str = "k7-4821-9930";
    const HASH: [u8; 32] = [7; 32];
    const LIFETIME: Duration = Duration::from_secs(5 * 60);

    fn parsed(text: &str) -> Code {
        text.parse().unwrap()
    }

    fn key(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    struct View<'a> {
        code: &'a str,
        ours: PublicKey,
        theirs: PublicKey,
        handshake_hash: &'a [u8],
    }

    fn view<'a>(
        code: &'a str,
        ours: PublicKey,
        theirs: PublicKey,
        handshake_hash: &'a [u8],
    ) -> View<'a> {
        View {
            code,
            ours,
            theirs,
            handshake_hash,
        }
    }

    fn exchanged(joiner: View, host: View) -> (Confirmed, Confirmed) {
        let start = |side, view: View| {
            Exchange::start(
                side,
                &parsed(view.code),
                &view.ours,
                &view.theirs,
                view.handshake_hash,
            )
        };
        let joining = start(Side::Joiner, joiner);
        let hosting = start(Side::Host, host);
        let to_host = joining.message().to_vec();
        let to_joiner = hosting.message().to_vec();
        (
            joining.finish(&to_joiner).unwrap(),
            hosting.finish(&to_host).unwrap(),
        )
    }

    fn confirms(joiner: View, host: View) -> bool {
        let (joined, hosted) = exchanged(joiner, host);
        let host_accepts = hosted.verify(Side::Joiner, &joined.mac(Side::Joiner));
        let joiner_accepts = joined.verify(Side::Host, &hosted.mac(Side::Host));
        assert_eq!(host_accepts, joiner_accepts);
        host_accepts
    }

    #[test]
    fn generated_codes_name_a_window_and_eight_digits_and_parse_back() {
        let mut windows = BTreeSet::new();
        for _ in 0..200 {
            let code = Code::generate().unwrap();
            let text = code.to_string();
            assert_eq!(text.len(), CODE.len(), "{text}");
            assert_eq!(text.parse::<Code>().unwrap(), code);
            assert_eq!(&text[..WINDOW_ID_LEN], code.window());
            windows.insert(code.window().to_owned());
        }
        assert!(windows.len() > 100, "{windows:?}");
    }

    #[test]
    fn codes_parse_in_the_printed_shape_only() {
        assert_eq!(parsed(" K7-4821-9930\n"), parsed(CODE));
        assert_eq!(parsed(CODE).window(), "k7");
        assert_eq!(parsed(CODE).to_string(), CODE);
        for text in [
            "",
            "k7",
            "k7-4821",
            "k7-48219930",
            "k7-4821-993",
            "k7-4821-99301",
            "k-4821-9930",
            "k77-4821-9930",
            "k_-4821-9930",
            "k7-48a1-9930",
            "k7-4821-9930-1",
            "k7 4821 9930",
        ] {
            assert!(text.parse::<Code>().is_err(), "{text:?} parsed");
        }
    }

    #[test]
    fn debug_output_hides_the_secret() {
        let debug = format!("{:?}", parsed(CODE));
        assert!(debug.contains("k7"), "{debug}");
        assert!(
            !debug.contains("4821") && !debug.contains("9930"),
            "{debug}"
        );
    }

    #[test]
    fn both_sides_confirm_with_the_same_code_keys_and_handshake() {
        let (joiner, host) = (key(1), key(2));
        assert!(confirms(
            view(CODE, joiner, host, &HASH),
            view(CODE, host, joiner, &HASH)
        ));
    }

    #[test]
    fn a_wrong_code_fails_the_confirmation() {
        let (joiner, host) = (key(1), key(2));
        assert!(!confirms(
            view("k7-4821-9931", joiner, host, &HASH),
            view(CODE, host, joiner, &HASH)
        ));
    }

    #[test]
    fn a_mismatched_handshake_hash_fails_the_confirmation() {
        let (joiner, host) = (key(1), key(2));
        assert!(!confirms(
            view(CODE, joiner, host, &HASH),
            view(CODE, host, joiner, &[8; 32])
        ));
    }

    #[test]
    fn a_relay_that_swaps_in_its_own_static_key_fails_the_confirmation() {
        let (joiner, host, relay) = (key(1), key(2), key(3));
        assert!(!confirms(
            view(CODE, joiner, relay, &HASH),
            view(CODE, host, joiner, &HASH)
        ));
        assert!(!confirms(
            view(CODE, joiner, host, &HASH),
            view(CODE, host, relay, &HASH)
        ));
    }

    #[test]
    fn a_confirmation_is_not_accepted_back_from_the_other_side() {
        let (joiner, host) = (key(1), key(2));
        let (joined, hosted) = exchanged(
            view(CODE, joiner, host, &HASH),
            view(CODE, host, joiner, &HASH),
        );
        assert!(!hosted.verify(Side::Joiner, &hosted.mac(Side::Host)));
        assert!(!joined.verify(Side::Host, &joined.mac(Side::Joiner)));
    }

    #[test]
    fn a_bad_spake2_message_is_an_error() {
        let exchange = Exchange::start(Side::Host, &parsed(CODE), &key(2), &key(1), &HASH);
        assert!(exchange.finish(b"short").is_err());
    }

    #[test]
    fn each_attempt_counts_and_the_third_closes_the_window() {
        let pairing = Pairing::default();
        let code = parsed(CODE);
        let (events, mut happened) = mpsc::unbounded_channel();
        let mut windows = pairing.watch();

        pairing.open(&code, LIFETIME, events).unwrap();

        assert_eq!(*windows.borrow_and_update(), Some("k7".to_owned()));
        let (other, _) = mpsc::unbounded_channel();
        assert!(pairing
            .open(&parsed("ab-0000-0000"), LIFETIME, other)
            .is_err());
        for left in [2, 1] {
            let attempt = pairing.attempt().unwrap();
            assert_eq!(attempt.attempts_left, left);
            pairing.failed(&attempt, "a wrong code was tried".into());
            match happened.try_recv().unwrap() {
                HostEvent::Failed { attempts_left, .. } => assert_eq!(attempts_left, left),
                other => panic!("expected a failed attempt, got {other:?}"),
            }
            assert!(pairing.is_open());
        }
        let last = pairing.attempt().unwrap();
        assert_eq!(last.attempts_left, 0);
        assert!(pairing.attempt().is_none());
        pairing.failed(&last, "a wrong code was tried".into());
        match happened.try_recv().unwrap() {
            HostEvent::Closed(reason) => assert!(reason.contains("last of 3"), "{reason}"),
            other => panic!("expected the window to close, got {other:?}"),
        }
        assert!(!pairing.is_open());
        assert!(windows.has_changed().unwrap());
        assert_eq!(*windows.borrow_and_update(), None);
    }

    #[test]
    fn a_window_lasts_as_long_as_the_lifetime_it_was_opened_with() {
        let pairing = Pairing::default();
        let (events, _happened) = mpsc::unbounded_channel();
        pairing.open(&parsed(CODE), Duration::ZERO, events).unwrap();
        assert!(pairing.attempt().is_none());

        let (events, _happened) = mpsc::unbounded_channel();
        pairing
            .open(&parsed("ab-0000-0000"), LIFETIME, events)
            .unwrap();
        assert!(pairing.attempt().is_some());
    }

    #[test]
    fn a_window_closes_only_for_its_own_code() {
        let pairing = Pairing::default();
        let code = parsed(CODE);
        let (events, _happened) = mpsc::unbounded_channel();
        pairing.open(&code, LIFETIME, events).unwrap();

        assert!(pairing.close(&parsed("k7-0000-0000")).is_none());
        assert!(pairing.is_open());
        assert!(pairing.close(&code).is_some());
        assert!(!pairing.is_open());
        assert!(pairing.attempt().is_none());
    }
}
