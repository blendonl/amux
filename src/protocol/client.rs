use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::{PublicKey, Version};
use crate::identity::{Incarnation, ServerId};
use crate::project::ProjectId;
use crate::settings::ServerConfig;
use crate::target::Target;

pub const MIN_ROWS: u16 = 2;
pub const MIN_COLS: u16 = 2;
pub const MAX_CLIPBOARD_LEN: usize = 1024 * 1024;
const LOCALE_ENV_PREFIX: &str = "LC_";
const LOCALE_ENV: [&str; 2] = ["LANG", "COLORTERM"];

pub fn is_locale_variable(key: &str) -> bool {
    key.starts_with(LOCALE_ENV_PREFIX) || LOCALE_ENV.contains(&key)
}

pub fn cap_clipboard(mut text: String) -> String {
    let mut end = text.len().min(MAX_CLIPBOARD_LEN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellPixels {
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientTerminal {
    pub graphics: bool,
    pub cell_pixels: Option<CellPixels>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageFormat {
    Rgb24,
    Rgba32,
    Png,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSpec {
    pub edit: u32,
    pub base: u32,
    pub x: u32,
    pub y: u32,
    pub background: u32,
    pub replace: bool,
    pub gap: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnimationState {
    Stopped,
    Loading,
    Running,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnimationControl {
    pub frame: u32,
    pub gap: i32,
    pub current: u32,
    pub state: Option<AnimationState>,
    pub loops: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageOp {
    Transmit {
        key: u32,
        format: ImageFormat,
        width: u32,
        height: u32,
        compressed: bool,
        total: u32,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
        last: bool,
    },
    Place {
        key: u32,
        cols: u16,
        rows: u16,
    },
    Delete {
        key: u32,
    },
    Frame {
        key: u32,
        spec: FrameSpec,
        format: ImageFormat,
        width: u32,
        height: u32,
        compressed: bool,
        total: u32,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
        last: bool,
    },
    Animate {
        key: u32,
        control: AnimationControl,
    },
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
    pub transport: LinkTransport,
    pub key: Option<PublicKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTransport {
    Ssh,
    Noise,
}

impl fmt::Display for LinkTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ssh => "ssh",
            Self::Noise => "noise",
        })
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Via {
    Tailscale,
    Lan,
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Tailscale => "tailscale",
            Self::Lan => "lan",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub sources: Vec<SourceView>,
    pub peers: Vec<DiscoveryView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceView {
    pub via: Via,
    pub state: SourceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceState {
    Off,
    NotRunning,
    Running,
    Unavailable(String),
    RunningWith { machines: usize },
}

impl fmt::Display for SourceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off (disabled in the config)"),
            Self::NotRunning => f.write_str("not running"),
            Self::Running => f.write_str("running"),
            Self::Unavailable(reason) => f.write_str(reason),
            Self::RunningWith { machines: 1 } => f.write_str("running with 1 machine"),
            Self::RunningWith { machines } => write!(f, "running with {machines} machines"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryView {
    pub via: Via,
    pub name: String,
    pub address: String,
    pub server: Option<ServerId>,
    pub status: DiscoveryStatus,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscoveryStatus {
    Linked,
    Trying,
    Failing,
    Absent,
    NotPaired,
    PairingOpen,
}

impl fmt::Display for DiscoveryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Linked => "linked",
            Self::Trying => "trying",
            Self::Failing => "failing",
            Self::Absent => "absent",
            Self::NotPaired => "not paired",
            Self::PairingOpen => "pairing open",
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
    CopyMode { page_up: bool },
    PasteBuffer,
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
    Discover,
    ForgetServer {
        name: String,
    },
    OpenPairing {
        new_key: bool,
        verbose: bool,
    },
    JoinPairing {
        code: String,
        host: Option<String>,
        new_key: bool,
        verbose: bool,
    },
    ReloadConfig,
    Terminal(ClientTerminal),
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
    Reconnecting {
        server: String,
    },
    Detached,
    Exited,
    Error(String),
    Discovery(DiscoveryReport),
    PairingOpen {
        code: String,
        expires_in_secs: u64,
    },
    Paired {
        name: String,
        id: ServerId,
        fingerprint: String,
    },
    PairingClosed {
        reason: String,
    },
    PairingAttemptFailed {
        reason: String,
        attempts_left: u8,
    },
    PairingStep(String),
    Notice(String),
    Image(ImageOp),
    Clipboard(String),
}
