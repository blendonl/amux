use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::session::Session;
use super::Server;
use crate::protocol::{ClientMessage, Duplex, ServerMessage, Size};

pub type ClientConnection = Duplex<ClientMessage, ServerMessage>;

pub async fn handle(server: Arc<Server>, mut client: ClientConnection) -> Result<()> {
    let Some(request) = client.incoming.recv().await else {
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
            return send(&client.outgoing, sessions).await;
        }
        ClientMessage::KillServer => {
            server.shut_down();
            return Ok(());
        }
        other => Err(anyhow!("{other:?} is only valid while attached")),
    };

    match target {
        Ok((session, size)) => attach(&session, size, client).await,
        Err(err) => send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await,
    }
}

async fn attach(session: &Session, size: Size, client: ClientConnection) -> Result<()> {
    let Duplex {
        mut incoming,
        outgoing,
    } = client;
    let _client = session.track_client();
    let pane = session.active_pane();
    pane.resize(size)?;

    let attached = ServerMessage::Attached {
        session: session.name().to_owned(),
    };
    send(&outgoing, attached).await?;

    let mut updates = pane.subscribe();
    let mut frames = FrameDiffer::default();
    let mut dirty = true;

    loop {
        tokio::select! {
            permit = outgoing.reserve(), if dirty => {
                let Ok(permit) = permit else {
                    return Ok(());
                };
                dirty = false;
                let frame = frames.next(pane.screen());
                if !frame.is_empty() {
                    permit.send(ServerMessage::Output(frame));
                }
            }
            changed = updates.changed() => {
                if changed.is_err() {
                    return send(&outgoing, ServerMessage::Exited).await;
                }
                dirty = true;
            }
            message = incoming.recv() => match message {
                Some(ClientMessage::Input(bytes)) => pane.write_input(bytes)?,
                Some(ClientMessage::Resize(size)) => {
                    pane.resize(size)?;
                    dirty = true;
                }
                Some(ClientMessage::Detach) => {
                    return send(&outgoing, ServerMessage::Detached).await;
                }
                Some(other) => warn!(?other, "ignoring message from attached client"),
                None => return Ok(()),
            },
        }
    }
}

async fn send(outgoing: &mpsc::Sender<ServerMessage>, message: ServerMessage) -> Result<()> {
    outgoing
        .send(message)
        .await
        .map_err(|_| anyhow!("the client disconnected"))
}

#[derive(Default)]
struct FrameDiffer {
    previous: Option<vt100::Screen>,
}

impl FrameDiffer {
    fn next(&mut self, screen: vt100::Screen) -> Vec<u8> {
        let frame = match &self.previous {
            Some(previous) if previous.size() == screen.size() => screen.state_diff(previous),
            _ => screen.state_formatted(),
        };
        self.previous = Some(screen);
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser_with(text: &[u8]) -> vt100::Parser {
        let mut parser = vt100::Parser::new(4, 20, 0);
        parser.process(text);
        parser
    }

    #[test]
    fn the_first_frame_is_a_full_redraw() {
        let parser = parser_with(b"hello");
        let mut frames = FrameDiffer::default();
        assert_eq!(
            frames.next(parser.screen().clone()),
            parser.screen().state_formatted()
        );
    }

    #[test]
    fn an_unchanged_screen_sends_nothing() {
        let parser = parser_with(b"hello");
        let mut frames = FrameDiffer::default();
        frames.next(parser.screen().clone());
        assert!(frames.next(parser.screen().clone()).is_empty());
    }

    #[test]
    fn changes_at_the_same_size_send_a_diff_that_reproduces_the_screen() {
        let mut parser = parser_with(b"hello");
        let mut frames = FrameDiffer::default();
        let mut client = vt100::Parser::new(4, 20, 0);
        client.process(&frames.next(parser.screen().clone()));

        parser.process(b" world");
        let diff = frames.next(parser.screen().clone());
        assert_ne!(diff, parser.screen().state_formatted());
        client.process(&diff);
        assert_eq!(client.screen().contents(), "hello world");
    }

    #[test]
    fn a_size_change_resets_to_a_full_redraw() {
        let mut parser = parser_with(b"hello");
        let mut frames = FrameDiffer::default();
        frames.next(parser.screen().clone());

        parser.screen_mut().set_size(6, 30);
        assert_eq!(
            frames.next(parser.screen().clone()),
            parser.screen().state_formatted()
        );
    }
}
