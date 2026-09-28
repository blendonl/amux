use std::fmt;

use serde::{Deserialize, Serialize};

use super::{
    ClientMessage, ProjectCheckout, PublicKey, ServerMessage, SessionId, SessionInfo, Version,
};
use crate::config::{Incarnation, ServerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChannelId(pub u64);

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerMessage {
    Hello(Hello),
    Refused(Refusal),
    LinkConfirmed {
        generation: u64,
    },
    Goodbye(Farewell),
    Snapshot(Snapshot),
    Event(Event),
    Ping(u64),
    Pong(u64),
    ChannelOpen {
        id: ChannelId,
        first: ClientMessage,
    },
    ChannelToHost {
        id: ChannelId,
        message: ClientMessage,
    },
    ChannelToClient {
        id: ChannelId,
        message: ServerMessage,
    },
    ChannelCredit {
        id: ChannelId,
        credit: u32,
    },
    ChannelClose {
        id: ChannelId,
        from_opener: bool,
    },
    Trust(TrustUpdate),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub id: ServerId,
    pub incarnation: Incarnation,
    pub name: String,
    pub version: Version,
    pub peers: Vec<PeerAddress>,
    pub public_key: Option<PublicKey>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAddress {
    pub id: ServerId,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    Incompatible,
    NameTaken,
    Duplicate,
    SelfDial,
    Untrusted,
    Forgotten,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Incompatible => "the protocol versions are incompatible",
            Self::NameTaken => "the server name is already taken by another server",
            Self::Duplicate => "the servers are already linked",
            Self::SelfDial => "the address leads back to this server",
            Self::Untrusted => "the server's key is not trusted",
            Self::Forgotten => "the server was forgotten",
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustUpdate {
    pub trusted: Vec<TrustedPeer>,
    pub forgotten: Vec<ForgottenPeer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub id: ServerId,
    pub name: String,
    pub key: PublicKey,
    pub introduced_by: Option<ServerId>,
    pub direct: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgottenPeer {
    pub id: ServerId,
    pub key: Option<PublicKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Farewell {
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub incarnation: Incarnation,
    pub seq: u64,
    pub state: ServerState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerState {
    pub sessions: Vec<SessionInfo>,
    pub projects: Vec<ProjectCheckout>,
    pub peers: Vec<PeerAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub incarnation: Incarnation,
    pub seq: u64,
    pub event: StateEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StateEvent {
    SessionCreated(SessionInfo),
    SessionClosed(SessionId),
    SessionChanged(SessionInfo),
    ProjectsChanged(Vec<ProjectCheckout>),
    PeersChanged(Vec<PeerAddress>),
}

impl ServerState {
    pub fn apply(&mut self, event: StateEvent) {
        match event {
            StateEvent::SessionCreated(session) | StateEvent::SessionChanged(session) => match self
                .sessions
                .iter_mut()
                .find(|known| known.id == session.id)
            {
                Some(known) => *known = session,
                None => self.sessions.push(session),
            },
            StateEvent::SessionClosed(id) => self.sessions.retain(|session| session.id != id),
            StateEvent::ProjectsChanged(projects) => self.projects = projects,
            StateEvent::PeersChanged(peers) => self.peers = peers,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::protocol::WindowSummary;

    fn session(id: u64, name: &str) -> SessionInfo {
        SessionInfo {
            id: SessionId(id),
            name: name.into(),
            windows: vec![WindowSummary {
                index: 0,
                name: "sh".into(),
                panes: 1,
            }],
            attached_clients: 0,
            last_activity: SystemTime::UNIX_EPOCH,
            project: None,
            branch: None,
        }
    }

    #[test]
    fn events_update_the_state_by_session_id() {
        let mut state = ServerState::default();
        state.apply(StateEvent::SessionCreated(session(1, "a")));
        state.apply(StateEvent::SessionCreated(session(2, "b")));
        state.apply(StateEvent::SessionChanged(session(1, "renamed")));
        state.apply(StateEvent::SessionClosed(SessionId(2)));

        assert_eq!(state.sessions, vec![session(1, "renamed")]);
    }

    #[test]
    fn peer_messages_survive_a_round_trip() {
        let hello = PeerMessage::Hello(Hello {
            id: ServerId::random().unwrap(),
            incarnation: Incarnation::random().unwrap(),
            name: "desk".into(),
            version: Version::current(),
            peers: Vec::new(),
            public_key: Some(PublicKey([7; 32])),
        });
        let bytes = postcard::to_stdvec(&hello).unwrap();
        assert_eq!(postcard::from_bytes::<PeerMessage>(&bytes).unwrap(), hello);
    }

    #[test]
    fn new_refusals_and_trust_updates_survive_a_round_trip() {
        let id = ServerId::random().unwrap();
        let messages = [
            PeerMessage::Refused(Refusal::Untrusted),
            PeerMessage::Refused(Refusal::Forgotten),
            PeerMessage::Trust(TrustUpdate {
                trusted: vec![TrustedPeer {
                    id,
                    name: "desk".into(),
                    key: PublicKey([1; 32]),
                    introduced_by: Some(ServerId::random().unwrap()),
                    direct: false,
                }],
                forgotten: vec![ForgottenPeer { id, key: None }],
            }),
        ];
        for message in messages {
            let bytes = postcard::to_stdvec(&message).unwrap();
            assert_eq!(
                postcard::from_bytes::<PeerMessage>(&bytes).unwrap(),
                message
            );
        }
    }

    #[test]
    fn channel_messages_survive_a_round_trip() {
        let id = ChannelId(7);
        let messages = [
            PeerMessage::ChannelOpen {
                id,
                first: ClientMessage::Attach {
                    target: "work@desk".parse().unwrap(),
                    size: crate::protocol::Size { rows: 24, cols: 80 },
                },
            },
            PeerMessage::ChannelToHost {
                id,
                message: ClientMessage::Input(b"ls\r".to_vec()),
            },
            PeerMessage::ChannelToClient {
                id,
                message: ServerMessage::Output(b"hi".to_vec()),
            },
            PeerMessage::ChannelCredit { id, credit: 1 },
            PeerMessage::ChannelClose {
                id,
                from_opener: true,
            },
        ];
        for message in messages {
            let bytes = postcard::to_stdvec(&message).unwrap();
            assert_eq!(
                postcard::from_bytes::<PeerMessage>(&bytes).unwrap(),
                message
            );
        }
    }
}
