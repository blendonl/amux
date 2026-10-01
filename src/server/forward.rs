use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use tokio::sync::mpsc;
use tracing::{debug, info};

use super::connection::{
    open_or_refuse, send, switch_or_refuse, ClientConnection, Origin, Outcome,
};
use super::status::StatusFeed;
use super::Server;
use crate::cluster::{Channel, ChannelEnd};
use crate::identity::ServerId;
use crate::protocol::{
    AttachedSession, ClientMessage, ClientTerminal, NewSession, ServerMessage, Size,
};
use crate::target::Target;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub peer: ServerId,
    pub name: String,
}

pub struct RemoteSession {
    pub host: Host,
    pub target: Target,
}

pub enum Opening {
    Attach(Target),
    Create(Box<NewSession>),
}

impl Opening {
    fn request(self, size: Size) -> ClientMessage {
        match self {
            Self::Attach(target) => ClientMessage::Attach { target, size },
            Self::Create(request) => ClientMessage::NewSession(NewSession { size, ..*request }),
        }
    }
}

pub async fn request(server: &Server, host: &Host, message: ClientMessage) -> Result<()> {
    let mut channel = open(server, host, message).await?;
    match channel.recv().await {
        Some(ServerMessage::Done) => Ok(()),
        Some(ServerMessage::Error(message)) => Err(anyhow!(message)),
        Some(other) => bail!("unexpected reply from {}: {other:?}", host.name),
        None => bail!("lost server {}: {}", host.name, channel.end()),
    }
}

pub async fn run(
    server: &Arc<Server>,
    client: &mut ClientConnection,
    host: &Host,
    opening: Opening,
    size: &mut Size,
    terminal: &mut Option<ClientTerminal>,
) -> Result<Outcome> {
    let channel = match open(server, host, opening.request(*size)).await {
        Ok(channel) => channel,
        Err(err) => {
            send(&client.outgoing, ServerMessage::Error(format!("{err:#}"))).await?;
            return Ok(Outcome::Exited);
        }
    };
    replay(&channel, *terminal).await;
    let mut forward = Forward {
        server,
        host,
        attached: None,
        status: StatusFeed::new(server, Origin::Local),
    };
    let mut channel = Some(channel);
    loop {
        let step = match channel.as_mut() {
            Some(open) => forward.relay(client, open, size, terminal).await?,
            None => forward.reconnect(client, size, terminal).await?,
        };
        match step {
            Step::Done(outcome) => return Ok(outcome),
            Step::Lost => {
                channel = None;
                send(
                    &client.outgoing,
                    ServerMessage::Reconnecting {
                        server: host.name.clone(),
                    },
                )
                .await?;
            }
            Step::Reopened(reopened) => channel = Some(reopened),
        }
    }
}

enum Step {
    Done(Outcome),
    Lost,
    Reopened(Channel),
}

struct Forward<'a> {
    server: &'a Arc<Server>,
    host: &'a Host,
    attached: Option<AttachedSession>,
    status: StatusFeed,
}

impl Forward<'_> {
    async fn relay(
        &mut self,
        client: &mut ClientConnection,
        channel: &mut Channel,
        size: &mut Size,
        terminal: &mut Option<ClientTerminal>,
    ) -> Result<Step> {
        let mut pending: Option<ServerMessage> = None;
        loop {
            tokio::select! {
                permit = client.outgoing.reserve(), if pending.is_some() => {
                    let Ok(permit) = permit else {
                        return Ok(Step::Done(Outcome::Detached));
                    };
                    if let Some(message) = pending.take() {
                        permit.send(message);
                    }
                    channel.delivered();
                }
                message = channel.recv(), if pending.is_none() => match message {
                    Some(ServerMessage::Attached(session)) => {
                        if self.attached.is_some() {
                            debug!(host = %self.host.name, "reattached");
                        }
                        self.attached = Some(session.clone());
                        pending = Some(ServerMessage::Attached(session));
                    }
                    Some(ServerMessage::Detached) => {
                        send(&client.outgoing, ServerMessage::Detached).await?;
                        return Ok(Step::Done(Outcome::Detached));
                    }
                    Some(ServerMessage::Exited) => {
                        send(&client.outgoing, ServerMessage::Exited).await?;
                        return Ok(Step::Done(Outcome::Exited));
                    }
                    Some(ServerMessage::Error(message)) if self.attached.is_none() => {
                        send(&client.outgoing, ServerMessage::Error(message)).await?;
                        return Ok(Step::Done(Outcome::Exited));
                    }
                    Some(message) => pending = Some(message),
                    None => return self.channel_ended(&client.outgoing, channel.end()).await,
                },
                () = self.status.due(), if self.attached.is_some() && pending.is_none() => {
                    self.send_status(&client.outgoing).await?;
                }
                message = client.incoming.recv() => match message {
                    Some(ClientMessage::ListCluster) => {
                        let servers = ServerMessage::Cluster(self.server.cluster_view());
                        send(&client.outgoing, servers).await?;
                    }
                    Some(ClientMessage::Switch(target)) => {
                        if let Some(outcome) =
                            switch_or_refuse(self.server, &client.outgoing, target, Origin::Local).await?
                        {
                            return Ok(Step::Done(outcome));
                        }
                    }
                    Some(ClientMessage::NewSession(request)) => {
                        if let Some(outcome) =
                            open_or_refuse(self.server, &client.outgoing, request, Origin::Local).await?
                        {
                            return Ok(Step::Done(outcome));
                        }
                    }
                    Some(ClientMessage::Resize(new_size)) => {
                        *size = new_size.clamped();
                        let _ = channel.send(ClientMessage::Resize(*size)).await;
                    }
                    Some(ClientMessage::Terminal(reported)) => {
                        *terminal = Some(reported);
                        let _ = channel.send(ClientMessage::Terminal(reported)).await;
                    }
                    Some(message) => {
                        let _ = channel.send(message).await;
                    }
                    None => return Ok(Step::Done(Outcome::Detached)),
                },
            }
        }
    }

    async fn send_status(&mut self, outgoing: &mpsc::Sender<ServerMessage>) -> Result<()> {
        match self.status.update(self.server, &self.host.name) {
            Some(message) => send(outgoing, message).await,
            None => Ok(()),
        }
    }

    async fn channel_ended(
        &self,
        outgoing: &mpsc::Sender<ServerMessage>,
        end: ChannelEnd,
    ) -> Result<Step> {
        info!(host = %self.host.name, "forwarded session interrupted: {end}");
        let message = match end {
            ChannelEnd::HostStopped => ServerMessage::Exited,
            ChannelEnd::LinkDown | ChannelEnd::Overflow if self.attached.is_some() => {
                return Ok(Step::Lost);
            }
            ChannelEnd::ClosedByHost | ChannelEnd::LinkDown | ChannelEnd::Overflow => {
                ServerMessage::Error(format!("lost server {}: {end}", self.host.name))
            }
        };
        send(outgoing, message).await?;
        Ok(Step::Done(Outcome::Exited))
    }

    async fn reconnect(
        &mut self,
        client: &mut ClientConnection,
        size: &mut Size,
        terminal: &mut Option<ClientTerminal>,
    ) -> Result<Step> {
        let cluster = self.server.cluster();
        let Some(attached) = self.attached.clone() else {
            bail!("reconnecting before the session was attached");
        };
        let mut changes = cluster.watch();
        loop {
            changes.borrow_and_update();
            if cluster.is_stopped(self.host.peer) {
                send(&client.outgoing, ServerMessage::Exited).await?;
                return Ok(Step::Done(Outcome::Exited));
            }
            if cluster.is_linked(self.host.peer) {
                let reattach = ClientMessage::Reattach {
                    incarnation: attached.incarnation,
                    session_id: attached.id,
                    size: *size,
                };
                if let Ok(channel) = cluster.open_channel(self.host.peer, reattach).await {
                    info!(host = %self.host.name, "reattaching after the link came back");
                    replay(&channel, *terminal).await;
                    return Ok(Step::Reopened(channel));
                }
            }
            tokio::select! {
                changed = changes.changed() => {
                    if changed.is_err() {
                        send(&client.outgoing, ServerMessage::Exited).await?;
                        return Ok(Step::Done(Outcome::Exited));
                    }
                }
                () = self.status.due() => self.send_status(&client.outgoing).await?,
                message = client.incoming.recv() => match message {
                    Some(ClientMessage::Detach) => {
                        send(&client.outgoing, ServerMessage::Detached).await?;
                        return Ok(Step::Done(Outcome::Detached));
                    }
                    Some(ClientMessage::Resize(new_size)) => *size = new_size.clamped(),
                    Some(ClientMessage::Terminal(reported)) => *terminal = Some(reported),
                    Some(ClientMessage::ListCluster) => {
                        let servers = ServerMessage::Cluster(self.server.cluster_view());
                        send(&client.outgoing, servers).await?;
                    }
                    Some(ClientMessage::Switch(target)) => {
                        if let Some(outcome) =
                            switch_or_refuse(self.server, &client.outgoing, target, Origin::Local).await?
                        {
                            return Ok(Step::Done(outcome));
                        }
                    }
                    Some(ClientMessage::NewSession(request)) => {
                        if let Some(outcome) =
                            open_or_refuse(self.server, &client.outgoing, request, Origin::Local).await?
                        {
                            return Ok(Step::Done(outcome));
                        }
                    }
                    Some(_) => {}
                    None => return Ok(Step::Done(Outcome::Detached)),
                },
            }
        }
    }
}

async fn replay(channel: &Channel, terminal: Option<ClientTerminal>) {
    if let Some(terminal) = terminal {
        let _ = channel.send(ClientMessage::Terminal(terminal)).await;
    }
}

async fn open(server: &Server, host: &Host, message: ClientMessage) -> Result<Channel> {
    server
        .cluster()
        .open_channel(host.peer, message)
        .await
        .map_err(|_| anyhow!("server {} is offline", host.name))
}
