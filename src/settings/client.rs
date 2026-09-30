use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::callback::{self, CallbackId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StatusSettings {
    pub enabled: bool,
    pub session_format: String,
    pub window_format: String,
    pub latency_format: String,
    pub offline_format: String,
    pub offline_count_format: String,
    pub hidden_marker: String,
    #[serde(with = "callback::slot")]
    pub left: Option<CallbackId>,
    #[serde(with = "callback::slot")]
    pub right: Option<CallbackId>,
    pub interval_ms: u64,
}

impl StatusSettings {
    pub fn rows(&self) -> u16 {
        u16::from(self.enabled)
    }

    pub fn is_scripted(&self) -> bool {
        self.enabled && (self.left.is_some() || self.right.is_some())
    }
}

impl Default for StatusSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            session_format: "[{session}@{server}]".into(),
            window_format: " {index}:{name} ".into(),
            latency_format: " {latency} ".into(),
            offline_format: " {server} offline ".into(),
            offline_count_format: " {count} offline ".into(),
            hidden_marker: "…".into(),
            left: None,
            right: None,
            interval_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TreeSettings {
    pub indent: String,
    pub expanded_marker: String,
    pub collapsed_marker: String,
    pub leaf_marker: String,
    pub detail_gap: String,
}

impl Default for TreeSettings {
    fn default() -> Self {
        Self {
            indent: "  ".into(),
            expanded_marker: "- ".into(),
            collapsed_marker: "+ ".into(),
            leaf_marker: "  ".into(),
            detail_gap: "  ".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SearchSettings {
    pub project_dirs: Vec<PathBuf>,
    pub project_depth: u32,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            project_dirs: vec![PathBuf::from("~/projects"), PathBuf::from("~/Projects")],
            project_depth: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WhichKeySettings {
    pub enabled: bool,
    pub delay_ms: u64,
    pub separator: String,
    pub group_marker: String,
}

impl WhichKeySettings {
    pub fn delay(&self) -> Duration {
        Duration::from_millis(self.delay_ms)
    }
}

impl Default for WhichKeySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: 500,
            separator: "→".into(),
            group_marker: "+".into(),
        }
    }
}
