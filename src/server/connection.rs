use std::sync::Arc;

use anyhow::{anyhow, Result};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::forward::{self, Host, Opening};
use super::session::Session;
use super::{Resolved, Server};
use crate::protocol::{
    AttachedSession, ClientMessage, DebugCommand, Duplex, NewSession, ServerMessage, Size,
};
use crate::target::{validate_session_name, Target};

pub type ClientConnection = Duplex<ClientMessage, ServerMessage>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Local,
    Peer,
}

pub enum Route {
    Local(Arc<Session>),
    Remote { host: Host, opening: Opening },
}

impl From<Resolved> for Route {
    fn from(resolved: Resolved) -> Self {
        match resolved {
            Resolved::Local(session) => Self::Local(session),
            Resolved::Remote(remote) => Self::Remote {
                host: remote.host,
                opening: Opening::Attach(remote.target),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Detached,
    Exited,
    Switch(Target),
}

pub async fn handle(
    server: Arc<Server>,
    mut client: ClientConnection,
    origin: Origin,
) -> Result<()> {
    let Some(request) = client.incoming.recv().await else {
        return Ok(());
    };
    debug!(?request, ?origin, "client request");

    if origin == Origin::Peer && !allowed_over_a_channel(&request) {
        let refusal = ServerMessage::Error("not allowed over a peer link".into());
        return send(&client.outgoing, refusal).await;
    }

    let routed = match request {
        ClientMessage::NewSession(request) => new_session_route(&server, request, origin).await,
        ClientMessage::Attach { target, size } => server
            .resolve(&target, origin)
            .map(|resolved| (Route::from(resolved), size)),
        ClientMessage::Reattach {
            incarnation,
            session_id,
            size,
        } => match server.find_by_id(incarnation, session_id) {
            Some(session) => Ok((Route::Local(session), size)),
            None => return send(&client.outgoing, ServerMessage::Exited).await,
        },
        ClientMessage::KillSession {
            target,
            remove_worktree,
        } => {
            let reply = kill_session(&server, &target, remove_worktree, origin).await;
            return send(&client.outgoing, done_or_error(reply)).await;
        }
        ClientMessage::RenameSession { target, name } => {
            let reply = rename_session(&server, &target, name, origin).await;
            return send(&client.outgoing, done_or_error(reply)).await;
        }
        ClientMessage::ListSessions => {
            let sessions = ServerMessage::Sessions(server.list_sessions());
            return send(&client.outgoing, sessions).await;
        }
        ClientMessage::ListCluster => {
            let servers = ServerMessage::Cluster(server.cluster_view());
            return send(&client.outgoing, servers).await;
        }
        ClientMessage::AddServer { name, server: peer } => {
            let reply = server.cluster().add_server(name, peer);
            return send(&client.outgoing, done_or_error(reply)).await;
        }
        ClientMessage::RemoveServer { name } => {
            let reply = server.cluster().remove_server(&name);
            return send(&client.outgoing, done_or_error(reply)).await;
        }
        ClientMessage::AddProject { path } => {
            let reply = match server.register_project(path).await {
                Ok(project) => ServerMessage::Project(project),
                Err(err) => ServerMessage::Error(format!("{err:#}")),
            };
            return send(&client.outgoing, reply).await;
        }
        ClientMessage::Debug(DebugCommand::Links) => {
            let links = ServerMessage::Links(server.cluster().links());
            return send(&client.outgoing, links).await;
        }
        ClientMessage::Debug(DebugCommand::DropLink(peer)) => {
            let reply = server.cluster().drop_link(&peer);
            return send(&client.outgoing, done_or_error(reply)).await;
        }
        ClientMessage::KillServer => {
            server.shut_down();
            return Ok(());
        }
        other => Err(anyhow!("{other:?} is only valid while attached")),
    };

    match routed {
        Ok((route, size)) => run_routes(&server, &mut client, route, size, origin).await,
        Err(err) => send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await,
    }
}

fn allowed_over_a_channel(request: &ClientMessage) -> bool {
    matches!(
        request,
        ClientMessage::NewSession(_)
            | ClientMessage::Attach { .. }
            | ClientMessage::Reattach { .. }
            | ClientMessage::KillSession { .. }
            | ClientMessage::RenameSession { .. }
            | ClientMessage::ListSessions
            | ClientMessage::ListCluster
    )
}

async fn new_session_route(
    server: &Arc<Server>,
    request: NewSession,
    origin: Origin,
) -> Result<(Route, Size)> {
    let size = request.size;
    match server.host_for_new_session(&request, origin)? {
        None => Ok((Route::Local(server.create_session(&request).await?), size)),
        Some(host) => {
            let forwarded = NewSession {
                on: None,
                cwd: None,
                ..request
            };
            Ok((
                Route::Remote {
                    host,
                    opening: Opening::Create(Box::new(forwarded)),
                },
                size,
            ))
        }
    }
}

async fn kill_session(
    server: &Arc<Server>,
    target: &Target,
    remove_worktree: bool,
    origin: Origin,
) -> Result<()> {
    match server.resolve(target, origin)? {
        Resolved::Local(session) if remove_worktree => {
            server.kill_and_remove_worktree(&session).await
        }
        Resolved::Local(session) => {
            server.kill_session(&session);
            Ok(())
        }
        Resolved::Remote(remote) => {
            let request = ClientMessage::KillSession {
                target: remote.target,
                remove_worktree,
            };
            forward::request(server, &remote.host, request).await
        }
    }
}

async fn rename_session(
    server: &Arc<Server>,
    target: &Target,
    name: String,
    origin: Origin,
) -> Result<()> {
    match server.resolve(target, origin)? {
        Resolved::Local(session) => server.rename_session(&session, name),
        Resolved::Remote(remote) => {
            validate_session_name(&name)?;
            let request = ClientMessage::RenameSession {
                target: remote.target,
                name,
            };
            forward::request(server, &remote.host, request).await
        }
    }
}

async fn run_routes(
    server: &Arc<Server>,
    client: &mut ClientConnection,
    mut route: Route,
    size: Size,
    origin: Origin,
) -> Result<()> {
    let mut size = size.clamped();
    loop {
        let outcome = match route {
            Route::Local(session) => attach(server, &session, &mut size, client, origin).await?,
            Route::Remote { host, opening } => {
                forward::run(server, client, &host, opening, &mut size).await?
            }
        };
        let target = match outcome {
            Outcome::Detached | Outcome::Exited => return Ok(()),
            Outcome::Switch(target) => target,
        };
        route = match server.resolve(&target, origin) {
            Ok(resolved) => Route::from(resolved),
            Err(err) => {
                return send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await
            }
        };
    }
}

pub async fn switch_or_refuse(
    server: &Server,
    outgoing: &mpsc::Sender<ServerMessage>,
    target: Target,
    origin: Origin,
) -> Result<Option<Outcome>> {
    match server.resolve(&target, origin) {
        Ok(_) => Ok(Some(Outcome::Switch(target))),
        Err(err) => {
            send(outgoing, ServerMessage::Error(format!("{err:#}"))).await?;
            Ok(None)
        }
    }
}

fn done_or_error(result: Result<()>) -> ServerMessage {
    match result {
        Ok(()) => ServerMessage::Done,
        Err(err) => ServerMessage::Error(format!("{err:#}")),
    }
}

async fn attach(
    server: &Arc<Server>,
    session: &Arc<Session>,
    size: &mut Size,
    client: &mut ClientConnection,
    origin: Origin,
) -> Result<Outcome> {
    let _client = server.track_client(session);
    let pane = session.active_pane();
    pane.resize(*size)?;

    let attached = ServerMessage::Attached(AttachedSession {
        server: server.identity().name.clone(),
        session: session.name(),
        id: session.id(),
        incarnation: server.incarnation(),
    });
    send(&client.outgoing, attached).await?;

    let mut updates = pane.subscribe();
    let mut frames = FrameDiffer::default();
    let mut dirty = true;

    loop {
        tokio::select! {
            permit = client.outgoing.reserve(), if dirty => {
                let Ok(permit) = permit else {
                    return Ok(Outcome::Detached);
                };
                dirty = false;
                let frame = frames.next(pane.screen());
                if !frame.is_empty() {
                    permit.send(ServerMessage::Output(frame));
                }
            }
            changed = updates.changed() => {
                if changed.is_err() {
                    send(&client.outgoing, ServerMessage::Exited).await?;
                    return Ok(Outcome::Exited);
                }
                dirty = true;
            }
            message = client.incoming.recv() => match message {
                Some(ClientMessage::Input(bytes)) => {
                    if pane.size() != *size {
                        pane.resize(*size)?;
                        dirty = true;
                    }
                    session.record_input();
                    pane.write_input(bytes)?;
                }
                Some(ClientMessage::Resize(new_size)) => {
                    *size = new_size.clamped();
                    pane.resize(*size)?;
                    dirty = true;
                }
                Some(ClientMessage::Redraw) => {
                    frames = FrameDiffer::default();
                    dirty = true;
                }
                Some(ClientMessage::ListCluster) => {
                    send(&client.outgoing, ServerMessage::Cluster(server.cluster_view())).await?;
                }
                Some(ClientMessage::Switch(target)) => {
                    if let Some(outcome) =
                        switch_or_refuse(server, &client.outgoing, target, origin).await?
                    {
                        return Ok(outcome);
                    }
                }
                Some(ClientMessage::Detach) => {
                    send(&client.outgoing, ServerMessage::Detached).await?;
                    return Ok(Outcome::Detached);
                }
                Some(other) => warn!(?other, "ignoring message from attached client"),
                None => return Ok(Outcome::Detached),
            },
        }
    }
}

pub async fn send(outgoing: &mpsc::Sender<ServerMessage>, message: ServerMessage) -> Result<()> {
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
