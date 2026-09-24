use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::Version;
use crate::config::{Incarnation, ServerConfig, ServerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub windows: Vec<WindowSummary>,
    pub attached_clients: usize,
    pub last_activity: SystemTime,
    pub project: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSummary {
    pub index: usize,
    pub name: String,
    pub panes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCheckout {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerView {
    pub id: Option<ServerId>,
    pub name: String,
    pub address: Option<String>,
    pub version: Option<Version>,
    pub status: ServerStatus,
    pub sessions: Vec<SessionInfo>,
    pub projects: Vec<ProjectCheckout>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerStatus {
    Local,
    Online {
        latency: Option<Duration>,
    },
    Offline {
        last_seen: Option<SystemTime>,
        stopped: bool,
    },
    Incompatible {
        version: Version,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkInfo {
    pub peer: ServerId,
    pub name: String,
    pub dialed: bool,
    pub incarnation: Incarnation,
    pub state: LinkState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkState {
    Up,
    Closing,
}

impl fmt::Display for LinkState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Up => "up",
            Self::Closing => "closing",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DebugCommand {
    Links,
    DropLink(String),
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
    ListCluster,
    AddServer {
        name: String,
        server: ServerConfig,
    },
    RemoveServer {
        name: String,
    },
    Debug(DebugCommand),
    KillServer,
    Input(Vec<u8>),
    Resize(Size),
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Attached { session: String },
    Sessions(Vec<SessionInfo>),
    Cluster(Vec<ServerView>),
    Links(Vec<LinkInfo>),
    Done,
    Output(Vec<u8>),
    Detached,
    Exited,
    Error(String),
}
