use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatusSettings {
    pub enabled: bool,
}

impl StatusSettings {
    pub fn rows(&self) -> u16 {
        u16::from(self.enabled)
    }
}

impl Default for StatusSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}
