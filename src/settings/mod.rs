mod client;
mod keymap;

use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use client::StatusSettings;
pub use keymap::{Binding, Keymap, PromptAction, Table, TreeAction, PREFIX_TABLE, ROOT_TABLE};

use crate::keys::Key;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub prefix: Key,
    pub escape_time_ms: u64,
    pub notice_ms: u64,
    pub status: StatusSettings,
}

impl Settings {
    pub fn escape_time(&self) -> Duration {
        Duration::from_millis(self.escape_time_ms)
    }

    pub fn notice_time(&self) -> Duration {
        Duration::from_millis(self.notice_ms)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            prefix: Key::ctrl('b'),
            escape_time_ms: 50,
            notice_ms: 3000,
            status: StatusSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_keep_their_defaults() {
        let settings: Settings = toml::from_str("notice_ms = 1000").unwrap();
        assert_eq!(
            settings,
            Settings {
                notice_ms: 1000,
                ..Settings::default()
            }
        );
        assert_eq!(settings.escape_time(), Duration::from_millis(50));
        assert_eq!(settings.status.rows(), 1);
    }

    #[test]
    fn the_prefix_is_written_in_key_notation() {
        assert_eq!(Settings::default().prefix, Key::ctrl('b'));
        let settings: Settings = toml::from_str("prefix = \"M-a\"").unwrap();
        assert_eq!(settings.prefix, "M-a".parse().unwrap());
        assert!(toml::from_str::<Settings>("prefix = \"C-\"").is_err());
    }

    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        assert!(toml::from_str::<Settings>("bogus = 1").is_err());
        assert!(toml::from_str::<Settings>("[status]\nbogus = 1").is_err());
    }
}
