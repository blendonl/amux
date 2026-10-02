use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReloadSettings {
    pub watch: bool,
    pub interval_ms: u64,
}

impl ReloadSettings {
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
}

impl Default for ReloadSettings {
    fn default() -> Self {
        Self {
            watch: true,
            interval_ms: 250,
        }
    }
}
