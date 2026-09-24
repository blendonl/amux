use std::io;
use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
const INCOMING_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub attached_clients: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    NewSession {
        name: Option<String>,
        cwd: PathBuf,
        size: Size,
    },
    Attach {
        target: Option<String>,
        size: Size,
    },
    ListSessions,
    KillServer,
    Input(Vec<u8>),
    Resize(Size),
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Attached { session: String },
    Sessions(Vec<SessionInfo>),
    Output(Vec<u8>),
    Detached,
    Exited,
    Error(String),
}

pub async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = postcard::to_stdvec(message)?;
    let len = u32::try_from(payload.len())?;
    if len > MAX_FRAME_LEN {
        bail!("outgoing frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit");
    }
    writer.write_u32(len).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn read_message<R, T>(reader: &mut R) -> Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
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
    Ok(Some(postcard::from_bytes(&payload)?))
}

pub fn incoming<R, T>(mut reader: R) -> mpsc::Receiver<T>
where
    R: AsyncRead + Unpin + Send + 'static,
    T: DeserializeOwned + Send + 'static,
{
    let (sender, receiver) = mpsc::channel(INCOMING_CAPACITY);
    tokio::spawn(async move {
        while let Ok(Some(message)) = read_message(&mut reader).await {
            if sender.send(message).await.is_err() {
                break;
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn messages_survive_a_round_trip() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let sent = vec![
            ClientMessage::NewSession {
                name: Some("work".into()),
                cwd: PathBuf::from("/tmp"),
                size: Size { rows: 24, cols: 80 },
            },
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

    #[tokio::test]
    async fn oversized_frames_are_rejected() {
        let (mut client, mut server) = tokio::io::duplex(64);
        client.write_u32(MAX_FRAME_LEN + 1).await.unwrap();

        let result = read_message::<_, ServerMessage>(&mut server).await;
        assert!(result.is_err());
    }
}
