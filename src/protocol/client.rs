use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
