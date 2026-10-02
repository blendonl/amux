use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use snow::{Builder, HandshakeState, StatelessTransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, DuplexStream};
use tokio::sync::oneshot;
use tracing::debug;

use crate::protocol::{
    self, from_hex, hex, PublicKey, TcpKind, TcpOpen, PUBLIC_KEY_LEN, TCP_MAGIC,
};

pub const NOISE_KEY_FILE: &str = "noise-key";
pub const MAX_MESSAGE_LEN: usize = 65535;
pub const MAX_PLAINTEXT_LEN: usize = MAX_MESSAGE_LEN - TAG_LEN;
const NOISE_KEY_FILE_MODE: u32 = 0o600;
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const TAG_LEN: usize = 16;
const NOISE_LEN_PREFIX: usize = 2;
const MAX_NOISE_FRAME_LEN: usize = NOISE_LEN_PREFIX + MAX_MESSAGE_LEN;
const MAX_OPENING_LEN: u32 = 64;
const PIPE_CAPACITY: usize = 64 * 1024;
const LINGER: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct NoiseKey {
    private: [u8; PUBLIC_KEY_LEN],
    public: PublicKey,
}

impl NoiseKey {
    pub fn generate() -> Result<Self> {
        let keypair = Builder::new(params()?)
            .generate_keypair()
            .context("generating a noise key")?;
        Self::from_private(&keypair.private)
    }

    pub fn load_or_create(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(NOISE_KEY_FILE);
        if let Some(key) = Self::read(&path)? {
            return Ok(key);
        }
        let key = Self::generate()?;
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(NOISE_KEY_FILE_MODE)
            .open(&path);
        match created {
            Ok(mut file) => {
                writeln!(file, "{}", hex(&key.private))
                    .with_context(|| format!("writing {}", path.display()))?;
                Ok(key)
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Self::read(&path)?
                .with_context(|| format!("{} disappeared while reading it", path.display())),
            Err(err) => Err(err).with_context(|| format!("creating {}", path.display())),
        }
    }

    pub fn rotate(state_dir: &Path) -> Result<Self> {
        let path = state_dir.join(NOISE_KEY_FILE);
        let temporary = path.with_extension("tmp");
        let key = Self::generate()?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(NOISE_KEY_FILE_MODE)
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        file.set_permissions(Permissions::from_mode(NOISE_KEY_FILE_MODE))
            .with_context(|| format!("restricting {}", temporary.display()))?;
        writeln!(file, "{}", hex(&key.private))
            .and_then(|()| file.sync_all())
            .with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, &path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(key)
    }

    pub fn public(&self) -> PublicKey {
        self.public
    }

    fn from_private(private: &[u8]) -> Result<Self> {
        let private: [u8; PUBLIC_KEY_LEN] = private
            .try_into()
            .context("a noise private key is 32 bytes")?;
        let mut dh = DefaultResolver
            .resolve_dh(&DHChoice::Curve25519)
            .context("curve25519 is not available")?;
        dh.set(&private);
        let public = dh
            .pubkey()
            .try_into()
            .context("a noise public key is 32 bytes")?;
        Ok(Self {
            private,
            public: PublicKey(public),
        })
    }

    fn read(path: &Path) -> Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        let private = from_hex(text.trim())
            .with_context(|| format!("{} does not hold a noise key", path.display()))?;
        Self::from_private(&private).map(Some)
    }
}

pub struct Secured {
    pub remote: PublicKey,
    pub handshake_hash: Vec<u8>,
    pub stream: DuplexStream,
    pub flushed: oneshot::Receiver<()>,
}

pub async fn initiate<S>(mut stream: S, key: &NoiseKey, kind: TcpKind) -> Result<Secured>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let prologue = protocol::encode(&TcpOpen::new(kind))?;
    protocol::write_frame(&mut stream, &prologue)
        .await
        .context("opening the tcp connection")?;
    let handshake = builder(key, &prologue)?
        .build_initiator()
        .context("starting the noise handshake")?;
    secure(stream, handshake).await
}

pub async fn read_opening<S>(stream: &mut S) -> Result<Option<TcpKind>>
where
    S: AsyncRead + Unpin,
{
    let len = match stream.read_u32().await {
        Ok(len) => len,
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if len > MAX_OPENING_LEN {
        bail!("the connection did not open with an amux tcp opening");
    }
    let mut payload = vec![0; len as usize];
    stream.read_exact(&mut payload).await?;
    let opening: TcpOpen = postcard::from_bytes(&payload).context("decoding the tcp opening")?;
    if opening.magic != TCP_MAGIC {
        bail!("the connection did not open with an amux tcp opening");
    }
    Ok(Some(opening.kind))
}

pub async fn respond<S>(stream: S, key: &NoiseKey, kind: TcpKind) -> Result<Secured>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let prologue = protocol::encode(&TcpOpen::new(kind))?;
    let handshake = builder(key, &prologue)?
        .build_responder()
        .context("starting the noise handshake")?;
    secure(stream, handshake).await
}

fn params() -> Result<snow::params::NoiseParams> {
    PATTERN.parse().context("parsing the noise pattern")
}

fn builder<'a>(key: &'a NoiseKey, prologue: &'a [u8]) -> Result<Builder<'a>> {
    Ok(Builder::new(params()?)
        .local_private_key(&key.private)
        .prologue(prologue))
}

async fn secure<S>(mut stream: S, mut handshake: HandshakeState) -> Result<Secured>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut buffer = vec![0; MAX_MESSAGE_LEN];
    while !handshake.is_handshake_finished() {
        if handshake.is_my_turn() {
            let len = handshake
                .write_message(&[], &mut buffer)
                .context("writing a noise handshake message")?;
            write_noise_frame(&mut stream, &buffer[..len]).await?;
        } else {
            let message = read_noise_frame(&mut stream)
                .await?
                .context("the peer closed the connection during the noise handshake")?;
            handshake
                .read_message(&message, &mut buffer)
                .context("the noise handshake failed")?;
        }
    }
    let remote = handshake
        .get_remote_static()
        .context("the peer sent no static key")?
        .try_into()
        .context("the peer's static key is not 32 bytes")?;
    let handshake_hash = handshake.get_handshake_hash().to_vec();
    let transport = handshake
        .into_stateless_transport_mode()
        .context("finishing the noise handshake")?;
    let (ours, theirs) = tokio::io::duplex(PIPE_CAPACITY);
    let (flush, flushed) = oneshot::channel();
    tokio::spawn(pump(stream, theirs, transport, flush));
    Ok(Secured {
        remote: PublicKey(remote),
        handshake_hash,
        stream: ours,
        flushed,
    })
}

async fn pump<S>(
    stream: S,
    plain: DuplexStream,
    transport: StatelessTransportState,
    flush: oneshot::Sender<()>,
) where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (wire_reader, wire_writer) = tokio::io::split(stream);
    let (plain_reader, plain_writer) = tokio::io::split(plain);
    let outbound = async {
        let sealed = seal(plain_reader, wire_writer, &transport).await;
        let _ = flush.send(());
        sealed
    };
    let inbound = unseal(wire_reader, plain_writer, &transport);
    tokio::pin!(outbound, inbound);
    tokio::select! {
        sealed = &mut outbound => {
            if let Err(err) = sealed {
                debug!("writing to the noise connection failed: {err:#}");
            }
            let _ = tokio::time::timeout(LINGER, inbound).await;
        }
        unsealed = &mut inbound => {
            if let Err(err) = unsealed {
                debug!("reading from the noise connection failed: {err:#}");
            }
            let _ = outbound.await;
        }
    }
}

async fn seal<R, W>(mut plain: R, mut wire: W, transport: &StatelessTransportState) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut chunk = vec![0; MAX_PLAINTEXT_LEN];
    let mut frame = vec![0; MAX_NOISE_FRAME_LEN];
    let mut nonce = 0;
    loop {
        let len = plain.read(&mut chunk).await?;
        if len == 0 {
            break;
        }
        let (prefix, message) = frame.split_at_mut(NOISE_LEN_PREFIX);
        let sealed = transport
            .write_message(nonce, &chunk[..len], message)
            .context("encrypting a noise message")?;
        nonce += 1;
        prefix.copy_from_slice(&noise_len_prefix(sealed)?);
        wire.write_all(&frame[..NOISE_LEN_PREFIX + sealed]).await?;
        wire.flush().await?;
    }
    wire.shutdown().await?;
    Ok(())
}

async fn unseal<R, W>(mut wire: R, mut plain: W, transport: &StatelessTransportState) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let unsealed = deliver(&mut wire, &mut plain, transport).await;
    let _ = plain.shutdown().await;
    unsealed
}

async fn deliver<R, W>(
    wire: &mut R,
    plain: &mut W,
    transport: &StatelessTransportState,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut wire = BufReader::with_capacity(MAX_NOISE_FRAME_LEN, wire);
    let mut received = vec![0; MAX_MESSAGE_LEN];
    let mut chunk = vec![0; MAX_MESSAGE_LEN];
    let mut nonce = 0;
    let mut delivering = true;
    while let Some(message) = read_noise_frame_into(&mut wire, &mut received).await? {
        let len = transport
            .read_message(nonce, message, &mut chunk)
            .context("decrypting a noise message")?;
        nonce += 1;
        if delivering && plain.write_all(&chunk[..len]).await.is_err() {
            delivering = false;
        }
    }
    Ok(())
}

async fn write_noise_frame<W>(writer: &mut W, message: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = noise_len_prefix(message.len())?;
    let mut frame = Vec::with_capacity(message.len() + NOISE_LEN_PREFIX);
    frame.extend_from_slice(&len);
    frame.extend_from_slice(message);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

fn noise_len_prefix(len: usize) -> Result<[u8; NOISE_LEN_PREFIX]> {
    let len = u16::try_from(len)
        .with_context(|| format!("a noise message of {len} bytes is too long"))?;
    Ok(len.to_be_bytes())
}

async fn read_noise_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let Some(len) = read_noise_len(reader).await? else {
        return Ok(None);
    };
    let mut message = vec![0; len];
    reader.read_exact(&mut message).await?;
    Ok(Some(message))
}

async fn read_noise_frame_into<'a, R>(
    reader: &mut R,
    buffer: &'a mut [u8],
) -> Result<Option<&'a [u8]>>
where
    R: AsyncRead + Unpin,
{
    let Some(len) = read_noise_len(reader).await? else {
        return Ok(None);
    };
    let message = &mut buffer[..len];
    reader.read_exact(message).await?;
    Ok(Some(message))
}

async fn read_noise_len<R>(reader: &mut R) -> Result<Option<usize>>
where
    R: AsyncRead + Unpin,
{
    match reader.read_u16().await {
        Ok(len) => Ok(Some(usize::from(len))),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::duplex;

    use super::*;

    const PAYLOAD_LEN: usize = 200_000;

    fn payload(seed: u8) -> Vec<u8> {
        (0..PAYLOAD_LEN)
            .map(|index| (index % 251) as u8 ^ seed)
            .collect()
    }

    fn transports() -> (StatelessTransportState, StatelessTransportState) {
        let (dialer, acceptor) = (NoiseKey::generate().unwrap(), NoiseKey::generate().unwrap());
        let mut initiator = builder(&dialer, b"test")
            .unwrap()
            .build_initiator()
            .unwrap();
        let mut responder = builder(&acceptor, b"test")
            .unwrap()
            .build_responder()
            .unwrap();
        let mut message = vec![0; MAX_MESSAGE_LEN];
        let mut payload = vec![0; MAX_MESSAGE_LEN];
        while !initiator.is_handshake_finished() {
            let (from, to) = if initiator.is_my_turn() {
                (&mut initiator, &mut responder)
            } else {
                (&mut responder, &mut initiator)
            };
            let len = from.write_message(&[], &mut message).unwrap();
            to.read_message(&message[..len], &mut payload).unwrap();
        }
        (
            initiator.into_stateless_transport_mode().unwrap(),
            responder.into_stateless_transport_mode().unwrap(),
        )
    }

    fn frame_lengths(wire: &[u8]) -> Vec<usize> {
        let mut lengths = Vec::new();
        let mut rest = wire;
        while let [high, low, tail @ ..] = rest {
            let len = usize::from(u16::from_be_bytes([*high, *low]));
            lengths.push(len);
            rest = &tail[len..];
        }
        lengths
    }

    async fn secured_pair(dialer: &NoiseKey, acceptor: &NoiseKey) -> (Secured, Secured) {
        let (dial_end, accept_end) = duplex(PIPE_CAPACITY);
        let acceptor = acceptor.clone();
        let accepted = tokio::spawn(async move {
            let mut stream = accept_end;
            let kind = read_opening(&mut stream).await.unwrap().unwrap();
            respond(stream, &acceptor, kind).await.unwrap()
        });
        let dialed = initiate(dial_end, dialer, TcpKind::Link).await.unwrap();
        (dialed, accepted.await.unwrap())
    }

    #[tokio::test]
    async fn frames_carry_up_to_the_largest_noise_message() {
        let (mut writer, mut reader) = duplex(4 * MAX_MESSAGE_LEN);
        let largest = vec![7; MAX_MESSAGE_LEN];

        write_noise_frame(&mut writer, b"hello").await.unwrap();
        write_noise_frame(&mut writer, &largest).await.unwrap();
        assert!(write_noise_frame(&mut writer, &[0; MAX_MESSAGE_LEN + 1])
            .await
            .is_err());
        drop(writer);

        assert_eq!(
            read_noise_frame(&mut reader).await.unwrap().unwrap(),
            b"hello"
        );
        assert_eq!(
            read_noise_frame(&mut reader).await.unwrap().unwrap(),
            largest
        );
        assert_eq!(read_noise_frame(&mut reader).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_large_payload_is_sealed_in_chunks_and_opened_whole() {
        let (initiator, responder) = transports();
        let sent = payload(0);
        let mut wire = Vec::new();

        seal(sent.as_slice(), &mut wire, &initiator).await.unwrap();

        let lengths = frame_lengths(&wire);
        assert_eq!(lengths.len(), PAYLOAD_LEN.div_ceil(MAX_PLAINTEXT_LEN));
        assert!(
            lengths.iter().all(|len| *len <= MAX_MESSAGE_LEN),
            "{lengths:?}"
        );
        assert_eq!(lengths[0], MAX_MESSAGE_LEN);
        let mut received = Vec::new();
        unseal(wire.as_slice(), &mut received, &responder)
            .await
            .unwrap();
        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn frames_written_one_byte_at_a_time_still_open() {
        let (initiator, responder) = transports();
        let pasted = payload(3);
        let typed: [&[u8]; 3] = [b"echo hi\r", &pasted[..1000], b"exit\r"];
        let mut wire = Vec::new();
        seal(
            typed[0].chain(typed[1]).chain(typed[2]),
            &mut wire,
            &initiator,
        )
        .await
        .unwrap();
        assert_eq!(frame_lengths(&wire).len(), typed.len());

        let (mut writer, reader) = duplex(1);
        let trickle = tokio::spawn(async move {
            for byte in wire {
                writer.write_all(&[byte]).await.unwrap();
            }
        });
        let mut received = Vec::new();
        unseal(reader, &mut received, &responder).await.unwrap();
        trickle.await.unwrap();

        assert_eq!(received, typed.concat());
    }

    #[tokio::test]
    async fn a_tampered_message_is_refused() {
        let (initiator, responder) = transports();
        let mut wire = Vec::new();
        seal(&b"ls -la\r"[..], &mut wire, &initiator).await.unwrap();
        let last = wire.len() - 1;
        wire[last] ^= 1;

        let mut received = Vec::new();
        assert!(unseal(wire.as_slice(), &mut received, &responder)
            .await
            .is_err());
        assert!(received.is_empty());
    }

    #[tokio::test]
    async fn the_handshake_proves_both_static_keys() {
        let (dialer, acceptor) = (NoiseKey::generate().unwrap(), NoiseKey::generate().unwrap());

        let (dialed, accepted) = secured_pair(&dialer, &acceptor).await;

        assert_eq!(dialed.remote, acceptor.public());
        assert_eq!(accepted.remote, dialer.public());
        assert_eq!(dialed.handshake_hash, accepted.handshake_hash);
        assert!(!dialed.handshake_hash.is_empty());
    }

    #[tokio::test]
    async fn secured_streams_carry_large_payloads_both_ways() {
        let (dialer, acceptor) = (NoiseKey::generate().unwrap(), NoiseKey::generate().unwrap());
        let (dialed, accepted) = secured_pair(&dialer, &acceptor).await;
        let (mut dial_reader, mut dial_writer) = tokio::io::split(dialed.stream);
        let (mut accept_reader, mut accept_writer) = tokio::io::split(accepted.stream);

        let up = tokio::spawn(async move {
            dial_writer.write_all(&payload(1)).await.unwrap();
            dial_writer.shutdown().await.unwrap();
        });
        let down = tokio::spawn(async move {
            accept_writer.write_all(&payload(2)).await.unwrap();
            accept_writer.shutdown().await.unwrap();
        });
        let mut upstream = Vec::new();
        accept_reader.read_to_end(&mut upstream).await.unwrap();
        let mut downstream = Vec::new();
        dial_reader.read_to_end(&mut downstream).await.unwrap();
        up.await.unwrap();
        down.await.unwrap();

        assert_eq!(upstream, payload(1));
        assert_eq!(downstream, payload(2));
        assert!(dialed.flushed.await.is_ok());
    }

    #[tokio::test]
    async fn the_opening_kind_is_bound_into_the_handshake() {
        let (dialer, acceptor) = (NoiseKey::generate().unwrap(), NoiseKey::generate().unwrap());
        let (dial_end, accept_end) = duplex(PIPE_CAPACITY);
        let accepted = tokio::spawn(async move {
            let mut stream = accept_end;
            assert_eq!(
                read_opening(&mut stream).await.unwrap(),
                Some(TcpKind::Pair)
            );
            respond(stream, &acceptor, TcpKind::Link).await
        });

        let dialed = initiate(dial_end, &dialer, TcpKind::Pair).await;

        assert!(accepted.await.unwrap().is_err());
        assert!(dialed.is_err());
    }

    #[tokio::test]
    async fn a_connection_without_the_tcp_magic_is_refused() {
        let (mut writer, mut reader) = duplex(1024);
        let greeting = protocol::Greeting {
            magic: protocol::MAGIC,
            major: protocol::PROTOCOL_MAJOR,
            minor: protocol::PROTOCOL_MINOR,
            role: protocol::Role::Peer,
        };
        protocol::write_message(&mut writer, &greeting)
            .await
            .unwrap();
        assert!(read_opening(&mut reader).await.is_err());

        let (mut writer, mut reader) = duplex(1024);
        writer.write_u32(u32::MAX).await.unwrap();
        assert!(read_opening(&mut reader).await.is_err());

        let (writer, mut reader) = duplex(1024);
        drop(writer);
        assert_eq!(read_opening(&mut reader).await.unwrap(), None);
    }

    #[test]
    fn a_key_is_created_once_and_rotated_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NOISE_KEY_FILE);

        let first = NoiseKey::load_or_create(dir.path()).unwrap();
        let again = NoiseKey::load_or_create(dir.path()).unwrap();
        assert_eq!(first.public(), again.public());
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, NOISE_KEY_FILE_MODE);

        let rotated = NoiseKey::rotate(dir.path()).unwrap();

        assert_ne!(rotated.public(), first.public());
        let loaded = NoiseKey::load_or_create(dir.path()).unwrap();
        assert_eq!(loaded.public(), rotated.public());
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, NOISE_KEY_FILE_MODE);
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn a_corrupt_key_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(NOISE_KEY_FILE), "not-a-key\n").unwrap();
        assert!(NoiseKey::load_or_create(dir.path()).is_err());
    }
}
