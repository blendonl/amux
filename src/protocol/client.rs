use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::Version;
use crate::config::{Incarnation, ServerConfig, ServerId};
use crate::project::ProjectId;
use crate::target::Target;

pub const MIN_ROWS: u16 = 2;
pub const MIN_COLS: u16 = 2;
const LOCALE_ENV_PREFIX: &str = "LC_";
const LOCALE_ENV: [&str; 2] = ["LANG", "COLORTERM"];

pub fn is_locale_variable(key: &str) -> bool {
    key.starts_with(LOCALE_ENV_PREFIX) || LOCALE_ENV.contains(&key)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub rows: u16,
    pub cols: u16,
}

impl Size {
    pub fn clamped(self) -> Self {
        Self {
            rows: self.rows.max(MIN_ROWS),
            cols: self.cols.max(MIN_COLS),
        }
    }
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
    pub project: Option<ProjectId>,
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
    pub id: ProjectId,
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
pub struct ProjectRef {
    pub id: ProjectId,
    pub name: String,
    pub origin: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSession {
    pub name: Option<String>,
    pub on: Option<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub size: Size,
    pub project: Option<ProjectRef>,
    pub branch: Option<String>,
    pub clone: bool,
}

impl NewSession {
    pub fn new(name: Option<String>, size: Size) -> Self {
        Self {
            name,
            on: None,
            cwd: None,
            env: Vec::new(),
            size,
            project: None,
            branch: None,
            clone: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Split {
    LeftRight,
    TopBottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionCommand {
    NewWindow,
    NextWindow,
    PreviousWindow,
    SelectWindow(usize),
    SplitPane(Split),
    NextPane,
    SelectPane(Direction),
    KillPane,
    KillWindow,
    RenameWindow(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    NewSession(NewSession),
    Attach {
        target: Target,
        size: Size,
    },
    Reattach {
        incarnation: Incarnation,
        session_id: SessionId,
        size: Size,
    },
    KillSession {
        target: Target,
        remove_worktree: bool,
    },
    RenameSession {
        target: Target,
        name: String,
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
    AddProject {
        path: PathBuf,
    },
    Debug(DebugCommand),
    KillServer,
    Input(Vec<u8>),
    Resize(Size),
    Command(SessionCommand),
    Switch(Target),
    Redraw,
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedSession {
    pub server: String,
    pub session: String,
    pub id: SessionId,
    pub incarnation: Incarnation,
}

impl fmt::Display for AttachedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.session, self.server)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionState {
    pub name: String,
    pub windows: Vec<WindowSummary>,
    pub active: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterStatus {
    pub local: String,
    pub host: String,
    pub latency: Option<Duration>,
    pub offline: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Attached(AttachedSession),
    Sessions(Vec<SessionInfo>),
    Cluster(Vec<ServerView>),
    Project(ProjectCheckout),
    Links(Vec<LinkInfo>),
    Done,
    Output(Vec<u8>),
    SessionState(SessionState),
    ClusterStatus(ClusterStatus),
    Reconnecting { server: String },
    Detached,
    Exited,
    Error(String),
}
