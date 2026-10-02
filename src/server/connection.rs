use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::{debug, warn};

use super::forward::{self, Host, Opening};
use super::mouse::MouseDecoder;
use super::render::GridDiffer;
use super::session::Session;
use super::status::StatusFeed;
use super::upload::{Turn, Uploader};
use super::{target_index, Resolved, Server};
use crate::pairing;
use crate::protocol::{
    AttachedSession, ClientMessage, ClientTerminal, DebugCommand, Duplex, NewSession,
    ServerMessage, Size,
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

fn route_to(server: &Server, target: &Target, origin: Origin) -> Result<Route> {
    match server.resolve(target, origin)? {
        Resolved::Local(session) => {
            if target.window.is_some() || target.pane.is_some() {
                session.select(
                    target.window.map(target_index),
                    target.pane.map(target_index),
                )?;
            }
            Ok(Route::Local(session))
        }
        Resolved::Remote(remote) => Ok(Route::Remote {
            host: remote.host,
            opening: Opening::Attach(remote.target),
        }),
    }
}

pub enum Outcome {
    Detached,
    Exited,
    Switch(Target),
    Open(Route),
}

pub async fn handle(
    server: Arc<Server>,
    mut client: ClientConnection,
    origin: Origin,
) -> Result<()> {
    let request = loop {
        match client.incoming.recv().await {
            Some(ClientMessage::Terminal(_)) => {}
            Some(request) => break request,
            None => return Ok(()),
        }
    };
    debug!(?request, ?origin, "client request");

    if origin == Origin::Peer && !allowed_over_a_channel(&request) {
        let refusal = ServerMessage::Error("not allowed over a peer link".into());
        return send(&client.outgoing, refusal).await;
    }

    let routed = match request {
        ClientMessage::NewSession(request) => new_session_route(&server, request, origin).await,
        ClientMessage::Attach { target, size } => {
            route_to(&server, &target, origin).map(|route| (route, size))
        }
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
        ClientMessage::ForgetServer { name } => {
            let reply = match server.forget_server(&name).await {
                Ok(None) => ServerMessage::Done,
                Ok(Some(notice)) => ServerMessage::Notice(notice),
                Err(err) => ServerMessage::Error(format!("{err:#}")),
            };
            return send(&client.outgoing, reply).await;
        }
        ClientMessage::Discover => {
            let report = ServerMessage::Discovery(server.discovery().report());
            return send(&client.outgoing, report).await;
        }
        ClientMessage::OpenPairing { new_key, verbose } => {
            let paired = pairing::open(server.discovery(), new_key, verbose, &mut client).await;
            return match paired {
                Ok(()) => Ok(()),
                Err(err) => send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await,
            };
        }
        ClientMessage::JoinPairing {
            code,
            host,
            new_key,
            verbose,
        } => {
            let joining = pairing::Joining {
                code,
                host,
                new_key,
                verbose,
            };
            let paired =
                pairing::join(server.discovery(), server.config(), joining, &mut client).await;
            return match paired {
                Ok(()) => Ok(()),
                Err(err) => send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await,
            };
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
        ClientMessage::ReloadConfig => {
            let reply = match server.reload_config().await {
                Ok(None) => ServerMessage::Done,
                Ok(Some(notice)) => ServerMessage::Notice(notice),
                Err(err) => ServerMessage::Error(format!("{err:#}")),
            };
            return send(&client.outgoing, reply).await;
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
    let window = target.window.map(target_index);
    let pane = target.pane.map(target_index);
    match server.resolve(target, origin)? {
        Resolved::Local(_) if remove_worktree && (window.is_some() || pane.is_some()) => {
            bail!("--remove-worktree kills a whole session, so the target can't name a window or pane")
        }
        Resolved::Local(session) if remove_worktree => {
            server.kill_and_remove_worktree(&session).await
        }
        Resolved::Local(session) => match (window, pane) {
            (None, None) => {
                server.kill_session(&session);
                Ok(())
            }
            (Some(window), None) => session.kill_window(window),
            (window, Some(pane)) => session.kill_pane(window, pane),
        },
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
    let mut terminal = None;
    loop {
        let outcome = match route {
            Route::Local(session) => {
                attach(server, &session, &mut size, &mut terminal, client, origin).await?
            }
            Route::Remote { host, opening } => {
                forward::run(server, client, &host, opening, &mut size, &mut terminal).await?
            }
        };
        let target = match outcome {
            Outcome::Detached | Outcome::Exited => return Ok(()),
            Outcome::Open(opened) => {
                route = opened;
                continue;
            }
            Outcome::Switch(target) => target,
        };
        route = match route_to(server, &target, origin) {
            Ok(route) => route,
            Err(err) => {
                return send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await
            }
        };
    }
}

pub async fn open_or_refuse(
    server: &Arc<Server>,
    outgoing: &mpsc::Sender<ServerMessage>,
    request: NewSession,
    origin: Origin,
) -> Result<Option<Outcome>> {
    match new_session_route(server, request, origin).await {
        Ok((route, _)) => Ok(Some(Outcome::Open(route))),
        Err(err) => {
            send(outgoing, ServerMessage::Error(format!("{err:#}"))).await?;
            Ok(None)
        }
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
    terminal: &mut Option<ClientTerminal>,
    client: &mut ClientConnection,
    origin: Origin,
) -> Result<Outcome> {
    let _client = server.track_client(session, origin);
    let tracked = session.track_terminal();
    if let Some(reported) = *terminal {
        tracked.report(reported);
    }
    session.resize(*size);

    let attached = ServerMessage::Attached(AttachedSession {
        server: server.identity().name.clone(),
        session: session.name(),
        id: session.id(),
        incarnation: server.incarnation(),
    });
    send(&client.outgoing, attached).await?;

    let mut updates = session.subscribe();
    let mut status = session.watch_status();
    send(
        &client.outgoing,
        ServerMessage::SessionState(session.status()),
    )
    .await?;
    let mut cluster = StatusFeed::new(server, origin);
    let mut differ = GridDiffer::new(*size);
    let mut uploader = Uploader::new(
        Arc::clone(server.images()),
        origin,
        server.settings().images.client_memory_bytes(),
    );
    uploader.set_graphics(terminal.is_some_and(|reported| reported.graphics));
    let mut session_terminal = session.watch_client_terminal();
    uploader.set_cell_pixels(
        session
            .client_terminal()
            .and_then(|latest| latest.cell_pixels),
    );
    let mut derivations = JoinSet::new();
    let mut mouse = MouseDecoder::new();
    let escape = tokio::time::sleep(Duration::ZERO);
    tokio::pin!(escape);
    let mut dirty = true;

    loop {
        if let Some(job) = uploader.take_job() {
            derivations.spawn_blocking(move || job.run());
        }
        tokio::select! {
            permit = client.outgoing.reserve(), if dirty || uploader.has_work() => {
                let Ok(permit) = permit else {
                    return Ok(Outcome::Detached);
                };
                match uploader.choose(dirty) {
                    Some(Turn::Frame) => {
                        dirty = false;
                        uploader.set_budget(server.settings().images.client_memory_bytes());
                        let Some(frame) = session.frame(uploader.viewer()) else {
                            continue;
                        };
                        uploader.frame(&frame.images);
                        let output = differ.diff(&frame);
                        if !output.is_empty() {
                            permit.send(ServerMessage::Output(output));
                        }
                    }
                    Some(Turn::Upload) => {
                        if let Some(op) = uploader.next() {
                            permit.send(ServerMessage::Image(op));
                        }
                    }
                    None => {}
                }
            }
            changed = updates.changed() => {
                if changed.is_err() {
                    send(&client.outgoing, ServerMessage::Exited).await?;
                    return Ok(Outcome::Exited);
                }
                dirty = true;
            }
            Some(finished) = derivations.join_next(), if !derivations.is_empty() => {
                match finished {
                    Ok(finished) => uploader.finish(finished),
                    Err(err) => {
                        warn!("deriving an image failed: {err}");
                        uploader.abandon();
                    }
                }
            }
            Ok(()) = session_terminal.changed() => {
                let latest = *session_terminal.borrow_and_update();
                uploader.set_cell_pixels(latest.and_then(|latest| latest.cell_pixels));
                dirty = true;
            }
            Ok(()) = status.changed() => {
                send(&client.outgoing, ServerMessage::SessionState(session.status())).await?;
            }
            () = cluster.due() => {
                if let Some(message) = cluster.update(server, &server.identity().name) {
                    send(&client.outgoing, message).await?;
                }
            }
            () = &mut escape, if mouse.has_pending() => {
                if let Some(pending) = mouse.flush() {
                    session.input(pending);
                }
            }
            message = client.incoming.recv() => match message {
                Some(ClientMessage::Input(bytes)) => {
                    resize_to_latest(session, *size);
                    tracked.mark_active();
                    session.record_input();
                    for event in mouse.decode(&bytes) {
                        session.input(event);
                    }
                    if mouse.has_pending() {
                        let escape_time = server.settings().mouse.escape_time();
                        escape.as_mut().reset(Instant::now() + escape_time);
                    }
                }
                Some(ClientMessage::Resize(new_size)) => {
                    *size = new_size.clamped();
                    session.resize(*size);
                    tracked.mark_active();
                    differ.set_client_size(*size);
                    dirty = true;
                }
                Some(ClientMessage::Terminal(reported)) => {
                    *terminal = Some(reported);
                    tracked.report(reported);
                    uploader.set_graphics(reported.graphics);
                    dirty = true;
                }
                Some(ClientMessage::Command(command)) => {
                    resize_to_latest(session, *size);
                    tracked.mark_active();
                    session.record_input();
                    if let Err(err) = session.run(command.clone()) {
                        debug!(?command, "command failed: {err:#}");
                        send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await?;
                    }
                }
                Some(ClientMessage::RenameSession { target, name }) => {
                    if let Err(err) = rename_session(server, &target, name, origin).await {
                        send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await?;
                    }
                }
                Some(ClientMessage::Redraw) => {
                    differ.reset();
                    uploader.restart();
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
                Some(ClientMessage::NewSession(request)) => {
                    if let Some(outcome) =
                        open_or_refuse(server, &client.outgoing, request, origin).await?
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

fn resize_to_latest(session: &Session, size: Size) {
    if session.size() != size {
        session.resize(size);
    }
}

pub async fn send(outgoing: &mpsc::Sender<ServerMessage>, message: ServerMessage) -> Result<()> {
    outgoing
        .send(message)
        .await
        .map_err(|_| anyhow!("the client disconnected"))
}
