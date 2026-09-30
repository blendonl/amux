use std::path::PathBuf;

use super::draw::Rect;
use crate::client::search::{Candidate, Job};
use crate::protocol::{ClientMessage, ServerView};
use crate::settings::CallbackId;
use crate::target::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    SessionArea,
    StatusRow,
}

pub enum PanelEvent {
    Unchanged,
    Pending,
    Cancel,
    Done(ClientMessage),
    Callback(CallbackId, String),
    Replace(Box<dyn Panel>, Vec<u8>),
    Load(Job),
    Open(PathBuf),
}

pub trait Panel {
    fn handle(&mut self, input: &[u8], attached: &Target) -> PanelEvent;

    fn time_out(&mut self, attached: &Target) -> PanelEvent;

    fn is_partial(&self) -> bool;

    fn render(&mut self, area: Rect) -> Vec<u8>;

    fn placement(&self) -> Option<Placement>;

    fn hides_session(&self) -> bool;

    fn cluster_listed(&mut self, _servers: &[ServerView], _attached: &Target) -> PanelEvent {
        PanelEvent::Unchanged
    }

    fn found(&mut self, _candidates: Result<Vec<Candidate>, String>) -> PanelEvent {
        PanelEvent::Unchanged
    }
}
