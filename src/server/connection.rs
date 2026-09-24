use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tracing::{debug, warn};

use super::session::Session;
use super::Server;
use crate::protocol::{self, ClientMessage, ServerMessage, Size};

pub async fn handle(server: Arc<Server>, stream: UnixStream) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let Some(request) = protocol::read_message(&mut reader).await? else {
        return Ok(());
    };
    debug!(?request, "client request");

    let target = match request {
        ClientMessage::NewSession { name, cwd, size } => server
            .create_session(name, &cwd, size)
            .map(|session| (session, size)),
        ClientMessage::Attach { target, size } => server
            .find_session(target.as_deref())
            .map(|session| (session, size)),
        ClientMessage::ListSessions => {
            let sessions = ServerMessage::Sessions(server.list_sessions());
            return protocol::write_message(&mut writer, &sessions).await;
        }
        ClientMessage::KillServer => {
            server.shut_down();
            return Ok(());
        }
        other => Err(anyhow!("{other:?} is only valid while attached")),
    };

    match target {
        Ok((session, size)) => attach(&session, size, reader, writer).await,
        Err(err) => {
            let reply = ServerMessage::Error(format!("{err:#}"));
            protocol::write_message(&mut writer, &reply).await
        }
    }
}

async fn attach(
    session: &Session,
    size: Size,
    reader: OwnedReadHalf,
    mut writer: OwnedWriteHalf,
) -> Result<()> {
    let _client = session.track_client();
    let pane = session.active_pane();
    pane.resize(size)?;

    let attached = ServerMessage::Attached {
        session: session.name().to_owned(),
    };
    protocol::write_message(&mut writer, &attached).await?;

    let mut incoming = protocol::incoming::<_, ClientMessage>(reader);
    let mut updates = pane.subscribe();
    let mut frames = FrameDiffer::default();
    send_frame(&mut writer, frames.next(pane.screen())).await?;

    loop {
        tokio::select! {
            changed = updates.changed() => {
                if changed.is_err() {
                    return protocol::write_message(&mut writer, &ServerMessage::Exited).await;
                }
                send_frame(&mut writer, frames.next(pane.screen())).await?;
            }
            message = incoming.recv() => match message {
                Some(ClientMessage::Input(bytes)) => pane.write_input(bytes)?,
                Some(ClientMessage::Resize(size)) => {
                    pane.resize(size)?;
                    frames.reset();
                    send_frame(&mut writer, frames.next(pane.screen())).await?;
                }
                Some(ClientMessage::Detach) => {
                    return protocol::write_message(&mut writer, &ServerMessage::Detached).await;
                }
                Some(other) => warn!(?other, "ignoring message from attached client"),
                None => return Ok(()),
            },
        }
    }
}

async fn send_frame(writer: &mut OwnedWriteHalf, frame: Vec<u8>) -> Result<()> {
    if frame.is_empty() {
        return Ok(());
    }
    protocol::write_message(writer, &ServerMessage::Output(frame)).await
}

#[derive(Default)]
struct FrameDiffer {
    previous: Option<vt100::Screen>,
}

impl FrameDiffer {
    fn next(&mut self, screen: vt100::Screen) -> Vec<u8> {
        let frame = match &self.previous {
            Some(previous) => screen.state_diff(previous),
            None => screen.state_formatted(),
        };
        self.previous = Some(screen);
        frame
    }

    fn reset(&mut self) {
        self.previous = None;
    }
}
