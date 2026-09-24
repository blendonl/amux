mod client;
mod greeting;
mod peer;

use std::io;

use anyhow::{bail, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub use client::{
    is_locale_variable, AttachedSession, ClientMessage, DebugCommand, LinkInfo, LinkState,
    NewSession, ProjectCheckout, ProjectRef, ServerMessage, ServerStatus, ServerView, SessionId,
    SessionInfo, Size, WindowSummary, MIN_COLS, MIN_ROWS,
};
pub use greeting::{
    accept, greet, Greeting, IncompatibleServer, Role, Version, Welcome, MAGIC, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, RELEASE,
};
pub use peer::{
    ChannelId, Event, Farewell, Hello, PeerAddress, PeerMessage, Refusal, ServerState, Snapshot,
    StateEvent,
};

const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
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
    write_frame(writer, &encode(message)?).await
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
    writer.write_u32(u32::try_from(payload.len())?).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
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
    let len = match reader.read_u32().await {
        Ok(len) => len,
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    if len > MAX_FRAME_LEN {
        bail!("incoming frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    let mut payload = vec![0; len as usize];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn pump_incoming<R, T>(mut reader: R, sender: mpsc::Sender<T>)
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    loop {
        let frame = tokio::select! {
            frame = read_frame(&mut reader) => frame,
            () = sender.closed() => return,
        };
        let payload = match frame {
            Ok(Some(payload)) => payload,
            Ok(None) => return,
            Err(err) => {
                debug!("reading a frame failed: {err:#}");
                return;
            }
        };
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
    while let Some(message) = receiver.recv().await {
        let payload = match encode(&message) {
            Ok(payload) => payload,
            Err(err) => {
                warn!("closing the connection, a message failed to encode: {err:#}");
                return;
            }
        };
        if let Err(err) = write_frame(&mut writer, &payload).await {
            debug!("writing a frame failed: {err:#}");
            return;
        }
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::io::{duplex as byte_pipe, split};

    use super::*;

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
            ClientMessage::Input(b"ls\r".to_vec()),
            ClientMessage::Detach,
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
