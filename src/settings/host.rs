use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::template;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PaneSettings {
    pub shell: Option<Vec<String>>,
    pub term: String,
    pub scrollback: usize,
    pub env: BTreeMap<String, String>,
    pub strip_env: Vec<String>,
    pub images: bool,
}

impl PaneSettings {
    pub fn strips(&self, key: &str) -> bool {
        self.strip_env
            .iter()
            .any(|prefix| key.starts_with(prefix.as_str()))
    }
}

impl Default for PaneSettings {
    fn default() -> Self {
        Self {
            shell: None,
            term: "screen-256color".into(),
            scrollback: 10_000,
            env: BTreeMap::new(),
            strip_env: vec!["SSH_".into()],
            images: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowSettings {
    pub base_index: usize,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    pub base_index: usize,
    pub clash_format: String,
    pub clash_start: usize,
    pub activity_interval_ms: u64,
}

impl SessionSettings {
    pub fn numbered_names(&self) -> impl Iterator<Item = String> {
        (self.base_index..).map(|index| index.to_string())
    }

    pub fn clash_names<'a>(&'a self, base: &'a str) -> impl Iterator<Item = String> + 'a {
        (self.clash_start..).map(move |suffix| {
            template::fill(
                &self.clash_format,
                &[("base", base), ("n", &suffix.to_string())],
            )
        })
    }

    pub fn activity_interval(&self) -> Duration {
        Duration::from_millis(self.activity_interval_ms)
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            base_index: 0,
            clash_format: "{base}-{n}".into(),
            clash_start: 2,
            activity_interval_ms: 5000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BorderSettings {
    pub horizontal: char,
    pub vertical: char,
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub left_tee: char,
    pub right_tee: char,
    pub top_tee: char,
    pub bottom_tee: char,
    pub cross: char,
}

impl Default for BorderSettings {
    fn default() -> Self {
        Self {
            horizontal: '─',
            vertical: '│',
            top_left: '┌',
            top_right: '┐',
            bottom_left: '└',
            bottom_right: '┘',
            left_tee: '├',
            right_tee: '┤',
            top_tee: '┬',
            bottom_tee: '┴',
            cross: '┼',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MouseSettings {
    pub escape_time_ms: u64,
}

impl MouseSettings {
    pub fn escape_time(&self) -> Duration {
        Duration::from_millis(self.escape_time_ms)
    }
}

impl Default for MouseSettings {
    fn default() -> Self {
        Self { escape_time_ms: 25 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeSettings {
    pub suffix: String,
    pub fetch_timeout_ms: u64,
}

impl WorktreeSettings {
    pub fn fetch_timeout(&self) -> Duration {
        Duration::from_millis(self.fetch_timeout_ms)
    }
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        Self {
            suffix: "-worktrees".into(),
            fetch_timeout_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub default_server: Option<String>,
    pub worktrees_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_tables_override_only_the_fields_they_name() {
        let pane: PaneSettings = toml::from_str(
            "shell = [\"/usr/bin/fish\", \"--private\"]\n\
             term = \"tmux-256color\"\n\
             [env]\nEDITOR = \"vi\"",
        )
        .unwrap();
        assert_eq!(
            pane,
            PaneSettings {
                shell: Some(vec!["/usr/bin/fish".into(), "--private".into()]),
                term: "tmux-256color".into(),
                env: BTreeMap::from([("EDITOR".into(), "vi".into())]),
                ..PaneSettings::default()
            }
        );

        let borders: BorderSettings = toml::from_str("vertical = \"|\"\ncross = \"+\"").unwrap();
        assert_eq!(
            borders,
            BorderSettings {
                vertical: '|',
                cross: '+',
                ..BorderSettings::default()
            }
        );
        assert!(toml::from_str::<BorderSettings>("vertical = \"||\"").is_err());

        for table in [
            "[pane]\nbogus = 1",
            "[window]\nbogus = 1",
            "[session]\nbogus = 1",
            "[borders]\nbogus = \"x\"",
            "[mouse]\nbogus = 1",
            "[worktrees]\nbogus = 1",
        ] {
            assert!(
                toml::from_str::<super::super::Settings>(table).is_err(),
                "{table}"
            );
        }
    }

    #[test]
    fn strip_env_matches_variable_prefixes() {
        let pane = PaneSettings::default();
        assert!(pane.strips("SSH_AUTH_SOCK"));
        assert!(!pane.strips("HOME"));

        let pane = PaneSettings {
            strip_env: vec!["AWS_".into(), "GPG_TTY".into()],
            ..PaneSettings::default()
        };
        assert!(pane.strips("AWS_PROFILE") && pane.strips("GPG_TTY"));
        assert!(!pane.strips("SSH_AUTH_SOCK"));
        assert!(!PaneSettings {
            strip_env: Vec::new(),
            ..PaneSettings::default()
        }
        .strips("SSH_AUTH_SOCK"));
    }

    fn first<T>(names: impl Iterator<Item = T>, count: usize) -> Vec<T> {
        names.take(count).collect()
    }

    #[test]
    fn sessions_are_numbered_from_the_base_index() {
        assert_eq!(
            first(SessionSettings::default().numbered_names(), 3),
            ["0", "1", "2"]
        );
        let session = SessionSettings {
            base_index: 1,
            ..SessionSettings::default()
        };
        assert_eq!(first(session.numbered_names(), 3), ["1", "2", "3"]);
    }

    #[test]
    fn clash_names_fill_the_base_and_the_suffix() {
        let session = SessionSettings::default();
        assert_eq!(
            first(session.clash_names("amux-main"), 2),
            ["amux-main-2", "amux-main-3"]
        );

        let session = SessionSettings {
            clash_format: "{n}.{base}".into(),
            clash_start: 1,
            ..SessionSettings::default()
        };
        assert_eq!(first(session.clash_names("work"), 2), ["1.work", "2.work"]);
        assert_eq!(first(session.clash_names("{n}"), 1), ["1.{n}"]);
    }

    #[test]
    fn durations_are_read_in_milliseconds() {
        assert_eq!(
            SessionSettings::default().activity_interval(),
            Duration::from_secs(5)
        );
        assert_eq!(
            MouseSettings::default().escape_time(),
            Duration::from_millis(25)
        );
        assert_eq!(
            WorktreeSettings::default().fetch_timeout(),
            Duration::from_secs(30)
        );
    }
}
