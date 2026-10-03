mod client;
mod greeting;
mod key;
mod peer;

use std::io;

use anyhow::{bail, Result};
use postcard::ser_flavors::Flavor;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub use client::{
    cap_clipboard, is_locale_variable, AnimationControl, AnimationState, AttachedSession,
    CellPixels, ClientMessage, ClientTerminal, ClusterStatus, DebugCommand, Direction,
    DiscoveryReport, DiscoveryStatus, DiscoveryView, FrameSpec, ImageFormat, ImageOp, LinkInfo,
    LinkState, LinkTransport, NewSession, ProjectCheckout, ProjectRef, ServerMessage, ServerStatus,
    ServerView, SessionCommand, SessionId, SessionInfo, SessionState, Size, SourceState,
    SourceView, Split, Via, WindowSummary, MAX_CLIPBOARD_LEN, MIN_COLS, MIN_ROWS,
};
pub use greeting::{
    accept, cli_version, greet, Greeting, IncompatibleServer, Role, TcpKind, TcpOpen, Version,
    Welcome, MAGIC, PROTOCOL_MAJOR, PROTOCOL_MINOR, RELEASE, TCP_MAGIC,
};
pub(crate) use key::{from_hex, hex};
pub use key::{PublicKey, PUBLIC_KEY_LEN};
pub use peer::{
    ChannelId, Event, Farewell, ForgottenPeer, Hello, PeerAddress, PeerMessage, Refusal,
    ServerState, Snapshot, StateEvent, TrustUpdate, TrustedPeer,
};

const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
const FRAME_HEADER_LEN: usize = 4;
const BATCH_LEN: usize = 64 * 1024;
const KEPT_BUFFER_LEN: usize = 1024 * 1024;
pub(crate) const READ_BUFFER_LEN: usize = 64 * 1024;
const INCOMING_CAPACITY: usize = 64;
const OUTGOING_CAPACITY: usize = 1;

pub struct Duplex<In, Out> {
    pub incoming: mpsc::Receiver<In>,
    pub outgoing: mpsc::Sender<Out>,
}

pub fn duplex<In, Out, R, W>(reader: R, writer: W) -> Duplex<In, Out>
where
    In: DeserializeOwned + Send + 'static,
    Out: Serialize + Send + 'static,
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (incoming_sender, incoming) = mpsc::channel(INCOMING_CAPACITY);
    let (outgoing, outgoing_receiver) = mpsc::channel(OUTGOING_CAPACITY);
    tokio::spawn(pump_incoming(reader, incoming_sender));
    tokio::spawn(pump_outgoing(writer, outgoing_receiver));
    Duplex { incoming, outgoing }
}

pub async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut frame = Vec::new();
    encode_frame_into(&mut frame, message)?;
    send_frames(writer, &frame).await
}

pub(crate) fn encode_frame_into<T: Serialize>(frames: &mut Vec<u8>, message: &T) -> Result<()> {
    let start = frames.len();
    frames.extend_from_slice(&[0; FRAME_HEADER_LEN]);
    match append_payload(frames, start, message) {
        Ok(len) => {
            frames[start..start + FRAME_HEADER_LEN].copy_from_slice(&len.to_be_bytes());
            Ok(())
        }
        Err(err) => {
            frames.truncate(start);
            Err(err)
        }
    }
}

fn append_payload<T: Serialize>(frames: &mut Vec<u8>, start: usize, message: &T) -> Result<u32> {
    postcard::serialize_with_flavor(message, AppendTo(frames))?;
    let len = u32::try_from(frames.len() - start - FRAME_HEADER_LEN)?;
    if len > MAX_FRAME_LEN {
        bail!("outgoing frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    Ok(len)
}

struct AppendTo<'a>(&'a mut Vec<u8>);

impl Flavor for AppendTo<'_> {
    type Output = ();

    #[inline(always)]
    fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    #[inline(always)]
    fn try_push(&mut self, byte: u8) -> postcard::Result<()> {
        self.0.push(byte);
        Ok(())
    }

    fn finalize(self) -> postcard::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct FrameBatch {
    frames: Vec<u8>,
}

impl FrameBatch {
    pub fn fill<T: Serialize>(
        &mut self,
        first: T,
        mut next: impl FnMut() -> Option<T>,
    ) -> Result<()> {
        encode_frame_into(&mut self.frames, &first)?;
        while self.frames.len() < BATCH_LEN {
            let Some(message) = next() else {
                break;
            };
            encode_frame_into(&mut self.frames, &message)?;
        }
        Ok(())
    }

    pub async fn write_to<W>(&mut self, writer: &mut W) -> Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let sent = send_frames(writer, &self.frames).await;
        self.frames.clear();
        if self.frames.capacity() > KEPT_BUFFER_LEN {
            self.frames = Vec::new();
        }
        sent
    }
}

async fn send_frames<W>(writer: &mut W, frames: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(frames).await?;
    writer.flush().await?;
    Ok(())
}

pub(crate) fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let payload = postcard::to_stdvec(message)?;
    let len = u32::try_from(payload.len())?;
    if len > MAX_FRAME_LEN {
        bail!("outgoing frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    Ok(payload)
}

pub(crate) async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
    frame.extend_from_slice(&u32::try_from(payload.len())?.to_be_bytes());
    frame.extend_from_slice(payload);
    send_frames(writer, &frame).await
}

pub async fn read_message<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    match read_frame(reader).await? {
        Some(payload) => Ok(Some(postcard::from_bytes(&payload)?)),
        None => Ok(None),
    }
}

pub(crate) async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut payload = Vec::new();
    Ok(read_frame_into(reader, &mut payload)
        .await?
        .then_some(payload))
}

pub(crate) async fn read_frame_into<R>(reader: &mut R, payload: &mut Vec<u8>) -> Result<bool>
where
    R: AsyncRead + Unpin,
{
    let len = match reader.read_u32().await {
        Ok(len) => len,
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    if len > MAX_FRAME_LEN {
        bail!("incoming frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    if payload.capacity() > KEPT_BUFFER_LEN {
        *payload = Vec::new();
    }
    payload.clear();
    payload.resize(len as usize, 0);
    reader.read_exact(payload).await?;
    Ok(true)
}

async fn pump_incoming<R, T>(reader: R, sender: mpsc::Sender<T>)
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut reader = BufReader::with_capacity(READ_BUFFER_LEN, reader);
    let mut payload = Vec::new();
    loop {
        let frame = tokio::select! {
            frame = read_frame_into(&mut reader, &mut payload) => frame,
            () = sender.closed() => return,
        };
        match frame {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                debug!("reading a frame failed: {err:#}");
                return;
            }
        }
        match postcard::from_bytes(&payload) {
            Ok(message) => {
                if sender.send(message).await.is_err() {
                    return;
                }
            }
            Err(err) => warn!(
                len = payload.len(),
                "dropping a frame that failed to decode: {err}"
            ),
        }
    }
}

async fn pump_outgoing<W, T>(mut writer: W, mut receiver: mpsc::Receiver<T>)
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut batch = FrameBatch::default();
    while let Some(message) = receiver.recv().await {
        let filled = batch.fill(message, || receiver.try_recv().ok());
        if let Err(err) = batch.write_to(&mut writer).await {
            debug!("writing a frame failed: {err:#}");
            return;
        }
        if let Err(err) = filled {
            warn!("closing the connection, a message failed to encode: {err:#}");
            return;
        }
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{duplex as byte_pipe, split};

    use super::*;

    #[derive(Default)]
    struct CountingWriter {
        writes: usize,
        flushes: usize,
        written: Vec<u8>,
    }

    impl AsyncWrite for CountingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            let writer = self.get_mut();
            writer.writes += 1;
            writer.written.extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.get_mut().flushes += 1;
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    async fn decode_all<T: DeserializeOwned>(mut frames: &[u8]) -> Vec<T> {
        let mut received = Vec::new();
        while let Some(message) = read_message(&mut frames).await.unwrap() {
            received.push(message);
        }
        received
    }

    async fn pumped(sent: &[ServerMessage]) -> CountingWriter {
        let (sender, receiver) = mpsc::channel(sent.len());
        for message in sent {
            sender.send(message.clone()).await.unwrap();
        }
        drop(sender);
        let mut writer = CountingWriter::default();
        pump_outgoing(&mut writer, receiver).await;
        writer
    }

    #[tokio::test]
    async fn queued_messages_leave_in_one_write_and_decode_in_order() {
        let sent: Vec<ServerMessage> = (0..10)
            .map(|index| ServerMessage::Output(vec![index; 100]))
            .collect();

        let writer = pumped(&sent).await;

        assert_eq!((writer.writes, writer.flushes), (1, 1));
        assert_eq!(decode_all::<ServerMessage>(&writer.written).await, sent);
    }

    #[tokio::test]
    async fn queued_messages_past_the_batch_size_take_another_write() {
        let frame = |index| ServerMessage::Image(ImageOp::Delete { key: index });
        let large = |index| ServerMessage::Clipboard("x".repeat(BATCH_LEN / 2 + index));
        let sent = vec![large(0), frame(1), large(2), frame(3)];

        let writer = pumped(&sent).await;

        assert_eq!((writer.writes, writer.flushes), (2, 2));
        assert_eq!(decode_all::<ServerMessage>(&writer.written).await, sent);
    }

    #[tokio::test]
    async fn a_single_frame_leaves_in_one_write() {
        let mut writer = CountingWriter::default();

        write_message(&mut writer, &ClientMessage::Input(b"ls\r".to_vec()))
            .await
            .unwrap();

        assert_eq!((writer.writes, writer.flushes), (1, 1));
        assert_eq!(
            decode_all::<ClientMessage>(&writer.written).await,
            [ClientMessage::Input(b"ls\r".to_vec())]
        );
    }

    #[tokio::test]
    async fn messages_survive_a_round_trip() {
        let (mut client, mut server) = byte_pipe(1024);
        let sent = vec![
            ClientMessage::NewSession(NewSession {
                on: Some("desk".into()),
                cwd: Some(PathBuf::from("/tmp")),
                env: vec![("LANG".into(), "C.UTF-8".into())],
                ..NewSession::new(Some("work".into()), Size { rows: 24, cols: 80 })
            }),
            ClientMessage::Attach {
                target: "work@desk:1.2".parse().unwrap(),
                size: Size { rows: 24, cols: 80 },
            },
            ClientMessage::Switch("@laptop".parse().unwrap()),
            ClientMessage::Command(SessionCommand::RenameWindow("logs".into())),
            ClientMessage::Input(b"ls\r".to_vec()),
            ClientMessage::Detach,
            ClientMessage::Discover,
            ClientMessage::ForgetServer {
                name: "laptop".into(),
            },
            ClientMessage::OpenPairing {
                new_key: true,
                verbose: false,
            },
            ClientMessage::JoinPairing {
                code: "k7-4821-9930".into(),
                host: Some("192.168.0.10".into()),
                new_key: false,
                verbose: true,
            },
            ClientMessage::ReloadConfig,
        ];

        for message in &sent {
            write_message(&mut client, message).await.unwrap();
        }
        drop(client);

        let mut received = Vec::new();
        while let Some(message) = read_message::<_, ClientMessage>(&mut server).await.unwrap() {
            received.push(message);
        }
        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn chrome_messages_survive_a_round_trip() {
        let (mut server, mut client) = byte_pipe(1024);
        let sent = vec![
            ServerMessage::SessionState(SessionState {
                name: "work".into(),
                windows: vec![WindowSummary {
                    index: 2,
                    name: "vim".into(),
                    panes: 3,
                }],
                active: 2,
            }),
            ServerMessage::ClusterStatus(ClusterStatus {
                local: "laptop".into(),
                host: "desktop".into(),
                latency: Some(std::time::Duration::from_millis(12)),
                offline: vec!["home-server".into()],
            }),
        ];
        for message in &sent {
            write_message(&mut server, message).await.unwrap();
        }
        drop(server);

        let mut received = Vec::new();
        while let Some(message) = read_message::<_, ServerMessage>(&mut client).await.unwrap() {
            received.push(message);
        }
        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn discovery_and_pairing_replies_survive_a_round_trip() {
        let (mut server, mut client) = byte_pipe(1024);
        let id = crate::identity::ServerId::random().unwrap();
        let sent = vec![
            ServerMessage::Discovery(DiscoveryReport {
                sources: vec![
                    SourceView {
                        via: Via::Tailscale,
                        state: SourceState::Unavailable("tailscale is not installed".into()),
                    },
                    SourceView {
                        via: Via::Lan,
                        state: SourceState::Running,
                    },
                ],
                peers: vec![DiscoveryView {
                    via: Via::Lan,
                    name: "laptop".into(),
                    address: format!("lan://{id}"),
                    server: Some(id),
                    status: DiscoveryStatus::Failing,
                    last_error: Some("connection refused".into()),
                }],
            }),
            ServerMessage::PairingOpen {
                code: "k7-4821-9930".into(),
                expires_in_secs: 300,
            },
            ServerMessage::Paired {
                name: "laptop".into(),
                id,
                fingerprint: PublicKey([3; PUBLIC_KEY_LEN]).fingerprint(),
            },
            ServerMessage::PairingClosed {
                reason: "the code expired".into(),
            },
            ServerMessage::PairingStep("connected to 192.168.0.10:40123".into()),
            ServerMessage::PairingAttemptFailed {
                reason: "the code did not match".into(),
                attempts_left: 2,
            },
            ServerMessage::Notice("amux.opt.name changes need a restart".into()),
        ];
        for message in &sent {
            write_message(&mut server, message).await.unwrap();
        }
        drop(server);

        let mut received = Vec::new();
        while let Some(message) = read_message::<_, ServerMessage>(&mut client).await.unwrap() {
            received.push(message);
        }
        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn terminal_reports_survive_a_round_trip() {
        let (mut client, mut server) = byte_pipe(1024);
        let sent = vec![
            ClientMessage::Terminal(ClientTerminal {
                graphics: true,
                cell_pixels: Some(CellPixels {
                    width: 10,
                    height: 21,
                }),
            }),
            ClientMessage::Terminal(ClientTerminal::default()),
        ];
        for message in &sent {
            write_message(&mut client, message).await.unwrap();
        }
        drop(client);

        let mut received = Vec::new();
        while let Some(message) = read_message::<_, ClientMessage>(&mut server).await.unwrap() {
            received.push(message);
        }
        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn image_operations_survive_a_round_trip() {
        let (mut server, mut client) = byte_pipe(1 << 16);
        let transmit = |format, data: Vec<u8>, last| ImageOp::Transmit {
            key: 70_000,
            format,
            width: 640,
            height: 480,
            compressed: format != ImageFormat::Png,
            total: 40_000,
            data,
            last,
        };
        let sent = vec![
            ServerMessage::Image(transmit(ImageFormat::Rgb24, vec![0, 255, 7], false)),
            ServerMessage::Image(transmit(ImageFormat::Rgba32, vec![1; 9000], true)),
            ServerMessage::Image(transmit(ImageFormat::Png, Vec::new(), true)),
            ServerMessage::Image(ImageOp::Place {
                key: 70_000,
                cols: 30,
                rows: 12,
            }),
            ServerMessage::Image(ImageOp::Delete { key: u32::MAX }),
            ServerMessage::Image(ImageOp::Frame {
                key: 70_000,
                spec: FrameSpec {
                    edit: 3,
                    base: 2,
                    x: 10,
                    y: 20,
                    background: 0xff00_00ff,
                    replace: true,
                    gap: -1,
                },
                format: ImageFormat::Rgba32,
                width: 30,
                height: 40,
                compressed: true,
                total: 9000,
                data: vec![5; 9000],
                last: false,
            }),
            ServerMessage::Image(ImageOp::Animate {
                key: 70_000,
                control: AnimationControl {
                    frame: 1,
                    gap: 120,
                    current: 4,
                    state: Some(AnimationState::Loading),
                    loops: 3,
                },
            }),
            ServerMessage::Image(ImageOp::Animate {
                key: 1,
                control: AnimationControl::default(),
            }),
        ];
        for message in &sent {
            write_message(&mut server, message).await.unwrap();
        }
        drop(server);

        let mut received = Vec::new();
        while let Some(message) = read_message::<_, ServerMessage>(&mut client).await.unwrap() {
            received.push(message);
        }
        assert_eq!(received, sent);
    }

    #[test]
    fn image_data_travels_as_raw_bytes() {
        let transmit = ImageOp::Transmit {
            key: 1,
            format: ImageFormat::Png,
            width: 2,
            height: 3,
            compressed: true,
            total: 3,
            data: vec![7, 8, 9],
            last: true,
        };
        assert_eq!(
            postcard::to_stdvec(&transmit).unwrap(),
            [0, 1, 2, 2, 3, 1, 3, 3, 7, 8, 9, 1]
        );
        let frame = ImageOp::Frame {
            key: 1,
            spec: FrameSpec::default(),
            format: ImageFormat::Png,
            width: 2,
            height: 3,
            compressed: true,
            total: 3,
            data: vec![7, 8, 9],
            last: true,
        };
        assert_eq!(
            postcard::to_stdvec(&frame).unwrap(),
            [3, 1, 0, 0, 0, 0, 0, 0, 0, 2, 2, 3, 1, 3, 3, 7, 8, 9, 1]
        );
    }

    #[test]
    fn output_and_input_travel_as_raw_bytes() {
        assert_eq!(
            postcard::to_stdvec(&ClientMessage::Input(b"ls\r".to_vec())).unwrap(),
            [12, 3, b'l', b's', b'\r']
        );
        let output: Vec<u8> = (0..200).collect();
        assert_eq!(
            postcard::to_stdvec(&ServerMessage::Output(output.clone())).unwrap(),
            [[6, 200, 1].as_slice(), &output].concat()
        );
    }

    #[tokio::test]
    async fn clipboard_text_survives_a_round_trip() {
        let (mut server, mut client) = byte_pipe(1024);
        let sent = ServerMessage::Clipboard("naïve 日本\n\tdone".into());
        write_message(&mut server, &sent).await.unwrap();

        assert_eq!(
            read_message::<_, ServerMessage>(&mut client).await.unwrap(),
            Some(sent)
        );
    }

    #[test]
    fn clipboard_text_within_the_cap_is_kept_whole() {
        assert_eq!(cap_clipboard(String::new()), "");
        assert_eq!(cap_clipboard("naïve 日本".into()), "naïve 日本");
        let full = "a".repeat(MAX_CLIPBOARD_LEN);
        assert_eq!(cap_clipboard(full.clone()), full);
    }

    #[test]
    fn clipboard_text_over_the_cap_is_cut_at_the_cap() {
        let text = "a".repeat(MAX_CLIPBOARD_LEN + 10);
        assert_eq!(cap_clipboard(text), "a".repeat(MAX_CLIPBOARD_LEN));
    }

    #[test]
    fn clipboard_text_is_never_cut_inside_a_character() {
        for (short, kept) in [
            (0, MAX_CLIPBOARD_LEN),
            (1, MAX_CLIPBOARD_LEN - 1),
            (2, MAX_CLIPBOARD_LEN - 2),
            (3, MAX_CLIPBOARD_LEN),
        ] {
            let text = "a".repeat(MAX_CLIPBOARD_LEN - short) + &"日".repeat(4);
            let capped = cap_clipboard(text.clone());
            assert_eq!(capped.len(), kept);
            assert!(text.starts_with(&capped));
        }
    }

    #[test]
    fn sizes_are_clamped_to_what_the_terminal_emulator_supports() {
        let tiny = Size { rows: 0, cols: 1 }.clamped();
        assert_eq!(
            tiny,
            Size {
                rows: MIN_ROWS,
                cols: MIN_COLS
            }
        );
        let normal = Size { rows: 24, cols: 80 };
        assert_eq!(normal.clamped(), normal);
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected() {
        let (mut client, mut server) = byte_pipe(64);
        client.write_u32(MAX_FRAME_LEN + 1).await.unwrap();

        let result = read_message::<_, ServerMessage>(&mut server).await;
        assert!(result.is_err());

        let mut frames = Vec::new();
        encode_frame_into(&mut frames, &ClientMessage::Detach).unwrap();
        let queued = frames.clone();
        let oversized = ServerMessage::Image(ImageOp::Transmit {
            key: 1,
            format: ImageFormat::Png,
            width: 1,
            height: 1,
            compressed: true,
            total: MAX_FRAME_LEN,
            data: vec![0; MAX_FRAME_LEN as usize],
            last: true,
        });
        assert!(encode_frame_into(&mut frames, &oversized).is_err());
        assert_eq!(frames, queued);
    }

    fn connected_pair() -> (
        Duplex<ServerMessage, ClientMessage>,
        Duplex<ClientMessage, ServerMessage>,
    ) {
        let (client_end, server_end) = byte_pipe(64);
        let (client_reader, client_writer) = split(client_end);
        let (server_reader, server_writer) = split(server_end);
        (
            duplex(client_reader, client_writer),
            duplex(server_reader, server_writer),
        )
    }

    #[tokio::test]
    async fn a_duplex_carries_messages_both_ways() {
        let (mut client, mut server) = connected_pair();
        let input = ClientMessage::Input(b"echo hi\r".to_vec());
        let output = ServerMessage::Output(vec![b'x'; 4096]);

        client.outgoing.send(input.clone()).await.unwrap();
        server.outgoing.send(output.clone()).await.unwrap();

        assert_eq!(server.incoming.recv().await, Some(input));
        assert_eq!(client.incoming.recv().await, Some(output));
    }

    #[tokio::test]
    async fn a_duplex_delivers_queued_messages_before_closing() {
        let (mut client, server) = connected_pair();
        let Duplex { incoming, outgoing } = server;
        outgoing.send(ServerMessage::Detached).await.unwrap();
        drop(outgoing);
        drop(incoming);

        assert_eq!(client.incoming.recv().await, Some(ServerMessage::Detached));
        assert_eq!(client.incoming.recv().await, None);
    }

    #[tokio::test]
    async fn a_duplex_skips_frames_it_cannot_decode() {
        let (mut raw, server_end) = byte_pipe(1024);
        let (reader, writer) = split(server_end);
        let mut server: Duplex<ClientMessage, ServerMessage> = duplex(reader, writer);

        write_message(&mut raw, &(u32::MAX, "not a client message"))
            .await
            .unwrap();
        write_message(&mut raw, &ClientMessage::Detach)
            .await
            .unwrap();

        assert_eq!(server.incoming.recv().await, Some(ClientMessage::Detach));
    }
}
