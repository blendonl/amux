use std::path::PathBuf;

use super::PanelEvent;
use crate::client::search::Job;
use crate::protocol::ClientMessage;
use crate::settings::CallbackId;

pub fn terminal(rows: u16, cols: u16) -> vt100::Parser {
    vt100::Parser::new(rows, cols, 0)
}

pub fn row_text(parser: &vt100::Parser, row: u16) -> String {
    screen_text(parser).swap_remove(usize::from(row))
}

pub fn screen_text(parser: &vt100::Parser) -> Vec<String> {
    let screen = parser.screen();
    screen
        .rows(0, screen.size().1)
        .map(|row| row.trim_end().to_owned())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Unchanged,
    Pending,
    Cancel,
    Done(ClientMessage),
    Callback(CallbackId, String),
    Replace(Vec<u8>),
    Load(Job),
    Open(PathBuf),
}

impl From<PanelEvent> for Event {
    fn from(event: PanelEvent) -> Self {
        match event {
            PanelEvent::Unchanged => Self::Unchanged,
            PanelEvent::Pending => Self::Pending,
            PanelEvent::Cancel => Self::Cancel,
            PanelEvent::Done(message) => Self::Done(message),
            PanelEvent::Callback(id, text) => Self::Callback(id, text),
            PanelEvent::Replace(_, input) => Self::Replace(input),
            PanelEvent::Load(job) => Self::Load(job),
            PanelEvent::Open(path) => Self::Open(path),
        }
    }
}
