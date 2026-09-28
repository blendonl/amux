use std::time::Duration;

pub use super::chrome::StatusSpan;
use crate::protocol::WindowSummary;
use crate::settings::{Binding, CallbackId, Keymap};
use crate::target::Target;

pub trait Scripting {
    fn call(
        &mut self,
        id: CallbackId,
        input: Option<&str>,
        context: &ClientContext,
    ) -> Result<Vec<Effect>, String>;

    fn status(
        &mut self,
        id: CallbackId,
        context: &StatusContext,
    ) -> Result<Option<Vec<StatusSpan>>, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Notify(String),
    Run(Binding),
    SendKeys(Vec<u8>),
    Prompt {
        label: String,
        initial: String,
        submit: CallbackId,
    },
    Switch(Target),
    Keymap(Keymap),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClientContext {
    pub session: String,
    pub server: String,
    pub local: String,
    pub windows: Vec<WindowSummary>,
    pub active: Option<usize>,
    pub latency: Option<Duration>,
    pub offline: Vec<String>,
}

impl ClientContext {
    pub fn active_window(&self) -> Option<&WindowSummary> {
        self.windows
            .iter()
            .find(|window| Some(window.index) == self.active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusContext {
    pub client: ClientContext,
    pub width: u16,
    pub tick: Option<u64>,
}
