use serde::{Deserialize, Serialize};

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
}

impl StatusSettings {
    pub fn rows(&self) -> u16 {
        u16::from(self.enabled)
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
