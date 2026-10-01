mod android;
pub mod callback;
mod client;
mod cluster;
mod host;
mod keymap;
mod reload;
mod style;
pub mod template;
mod theme;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use android::{AndroidSettings, KeyTable, KeyboardLayer, KeyboardSettings};
pub use callback::{CallbackId, CALLBACK_SLOT};
pub use client::{
    ClientImages, ImagesSettings, SearchSettings, StatusSettings, TreeSettings, WhichKeySettings,
};
pub use cluster::{ClusterSettings, DiscoverySettings, LanSettings, ServerConfig, SshSettings};
pub use host::{
    BorderSettings, MouseSettings, PaneSettings, ProjectConfig, SessionSettings, WindowSettings,
    WorktreeSettings,
};
pub use keymap::{
    Binding, Keymap, PickerAction, PromptAction, Table, TreeAction, PICKER_TABLE, PREFIX_TABLE,
    PROMPT_TABLE, ROOT_TABLE, SEARCH_TABLE, TREE_TABLE,
};
pub use reload::ReloadSettings;
pub use style::{Color, StyleSpec};
pub use theme::Theme;

use crate::keys::Key;
use crate::paths;

const DEFAULT_PROJECTS_DIR: &str = "~/projects";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub name: Option<String>,
    pub projects_dir: PathBuf,
    pub servers: BTreeMap<String, ServerConfig>,
    pub projects: BTreeMap<String, ProjectConfig>,
    pub prefix: Key,
    pub escape_time_ms: u64,
    pub notice_ms: u64,
    pub status: StatusSettings,
    pub theme: Theme,
    pub tree: TreeSettings,
    pub which_key: WhichKeySettings,
    pub search: SearchSettings,
    pub images: ImagesSettings,
    pub pane: PaneSettings,
    pub window: WindowSettings,
    pub session: SessionSettings,
    pub borders: BorderSettings,
    pub mouse: MouseSettings,
    pub cluster: ClusterSettings,
    pub discovery: DiscoverySettings,
    pub lan: LanSettings,
    pub worktrees: WorktreeSettings,
    pub reload: ReloadSettings,
    pub android: AndroidSettings,
}

impl Settings {
    pub fn escape_time(&self) -> Duration {
        Duration::from_millis(self.escape_time_ms)
    }

    pub fn notice_time(&self) -> Duration {
        Duration::from_millis(self.notice_ms)
    }

    pub fn expand_home(mut self, home: &Path) -> Self {
        self.projects_dir = paths::expand_home(self.projects_dir, home);
        self.search.project_dirs = self
            .search
            .project_dirs
            .into_iter()
            .map(|dir| paths::expand_home(dir, home))
            .collect();
        for project in self.projects.values_mut() {
            project.worktrees_dir = project
                .worktrees_dir
                .take()
                .map(|dir| paths::expand_home(dir, home));
        }
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self
            .name
            .as_deref()
            .is_some_and(|name| name.trim().is_empty())
        {
            return Err("amux.opt.name must not be blank".into());
        }
        if self.projects_dir.as_os_str().is_empty() {
            return Err("amux.opt.projects_dir must not be empty".into());
        }
        let cluster = &self.cluster;
        let positive = [
            ("cluster.ping_interval_ms", cluster.ping_interval_ms),
            ("cluster.missed_pings", u64::from(cluster.missed_pings)),
            ("cluster.handshake_timeout_ms", cluster.handshake_timeout_ms),
            ("cluster.connect_timeout_ms", cluster.connect_timeout_ms),
            ("cluster.backoff_min_ms", cluster.backoff_min_ms),
            (
                "cluster.max_unverified_backoff_ms",
                cluster.max_unverified_backoff_ms,
            ),
            ("cluster.status_interval_ms", cluster.status_interval_ms),
            ("discovery.interval_ms", self.discovery.interval_ms),
            ("lan.pairing_window_ms", self.lan.pairing_window_ms),
            (
                "worktrees.fetch_timeout_ms",
                self.worktrees.fetch_timeout_ms,
            ),
            ("search.project_depth", u64::from(self.search.project_depth)),
            ("reload.interval_ms", self.reload.interval_ms),
        ];
        if let Some((option, _)) = positive.iter().find(|(_, value)| *value == 0) {
            return Err(format!("amux.opt.{option} must be a positive number"));
        }
        if cluster.backoff_min_ms > cluster.backoff_max_ms {
            return Err(format!(
                "amux.opt.cluster.backoff_min_ms ({}) must not be more than \
                 amux.opt.cluster.backoff_max_ms ({})",
                cluster.backoff_min_ms, cluster.backoff_max_ms
            ));
        }
        if self.pane.shell.as_ref().is_some_and(Vec::is_empty) {
            return Err("amux.opt.pane.shell must name a program".into());
        }
        if self.worktrees.suffix.is_empty() {
            return Err("amux.opt.worktrees.suffix must not be empty".into());
        }
        self.android.keyboard.validate()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            name: None,
            projects_dir: PathBuf::from(DEFAULT_PROJECTS_DIR),
            servers: BTreeMap::new(),
            projects: BTreeMap::new(),
            prefix: Key::ctrl('b'),
            escape_time_ms: 50,
            notice_ms: 3000,
            status: StatusSettings::default(),
            theme: Theme::default(),
            tree: TreeSettings::default(),
            which_key: WhichKeySettings::default(),
            search: SearchSettings::default(),
            images: ImagesSettings::default(),
            pane: PaneSettings::default(),
            window: WindowSettings::default(),
            session: SessionSettings::default(),
            borders: BorderSettings::default(),
            mouse: MouseSettings::default(),
            cluster: ClusterSettings::default(),
            discovery: DiscoverySettings::default(),
            lan: LanSettings::default(),
            worktrees: WorktreeSettings::default(),
            reload: ReloadSettings::default(),
            android: AndroidSettings::default(),
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

    #[test]
    fn chrome_settings_are_read_from_their_own_tables() {
        let settings: Settings = toml::from_str(
            "[status]\nwindow_format = \"{index}|{name}\"\n\n\
             [theme.tree_cursor]\nfg = \"bright-yellow\"\n\n\
             [tree]\nexpanded_marker = \"v \"",
        )
        .unwrap();
        assert_eq!(settings.status.window_format, "{index}|{name}");
        assert_eq!(settings.status.session_format, "[{session}@{server}]");
        assert_eq!(settings.theme.tree_cursor.fg, Some(Color::Indexed(11)));
        assert_eq!(settings.theme.status, Theme::default().status);
        assert_eq!(settings.tree.expanded_marker, "v ");
        assert_eq!(settings.tree.indent, "  ");

        assert!(toml::from_str::<Settings>("[tree]\nbogus = 1").is_err());
        assert!(toml::from_str::<Settings>("[theme]\nbogus = {}").is_err());
        assert!(toml::from_str::<Settings>("[theme.status]\nbogus = 1").is_err());
    }

    #[test]
    fn the_client_images_mode_is_auto_on_or_off() {
        assert_eq!(Settings::default().images.client, ClientImages::Auto);
        for (written, mode) in [
            ("auto", ClientImages::Auto),
            ("on", ClientImages::On),
            ("off", ClientImages::Off),
        ] {
            let settings: Settings =
                toml::from_str(&format!("[images]\nclient = \"{written}\"")).unwrap();
            assert_eq!(settings.images.client, mode);
        }
        assert!(toml::from_str::<Settings>("[images]\nclient = \"yes\"").is_err());
        assert!(toml::from_str::<Settings>("[images]\nbogus = 1").is_err());
    }

    #[test]
    fn host_settings_are_read_from_their_own_tables() {
        let settings: Settings = toml::from_str(
            "[pane]\nterm = \"tmux-256color\"\n\n\
             [window]\nbase_index = 1\n\n\
             [session]\nclash_format = \"{base}.{n}\"\n\n\
             [borders]\nvertical = \"|\"\n\n\
             [mouse]\nescape_time_ms = 40\n\n\
             [worktrees]\nsuffix = \".trees\"\n\n\
             [theme.pane_border_active]\nfg = \"bright-blue\"",
        )
        .unwrap();
        assert_eq!(settings.pane.term, "tmux-256color");
        assert_eq!(settings.pane.scrollback, 10_000);
        assert_eq!(settings.window.base_index, 1);
        assert_eq!(settings.session.clash_format, "{base}.{n}");
        assert_eq!(settings.session.clash_start, 2);
        assert_eq!(settings.borders.vertical, '|');
        assert_eq!(settings.borders.horizontal, '─');
        assert_eq!(settings.mouse.escape_time_ms, 40);
        assert_eq!(settings.worktrees.suffix, ".trees");
        assert_eq!(settings.worktrees.fetch_timeout_ms, 30_000);
        assert_eq!(
            settings.theme.pane_border_active.fg,
            Some(Color::Indexed(12))
        );
        assert_eq!(settings.theme.pane_border, StyleSpec::EMPTY);
    }

    #[test]
    fn the_defaults_are_valid() {
        assert_eq!(Settings::default().validate(), Ok(()));
        assert_eq!(Settings::default().name, None);
        assert_eq!(
            Settings::default().projects_dir,
            PathBuf::from("~/projects")
        );
        assert_eq!(
            Settings::default().search,
            SearchSettings {
                project_dirs: vec!["~/projects".into(), "~/Projects".into()],
                project_depth: 1,
            }
        );
    }

    #[test]
    fn values_that_would_break_the_server_are_rejected() {
        let rejected = |change: fn(&mut Settings)| {
            let mut settings = Settings::default();
            change(&mut settings);
            settings.validate().unwrap_err()
        };
        assert_eq!(
            rejected(|settings| settings.name = Some(" ".into())),
            "amux.opt.name must not be blank"
        );
        assert_eq!(
            rejected(|settings| settings.projects_dir = PathBuf::new()),
            "amux.opt.projects_dir must not be empty"
        );
        assert_eq!(
            rejected(|settings| settings.cluster.status_interval_ms = 0),
            "amux.opt.cluster.status_interval_ms must be a positive number"
        );
        assert_eq!(
            rejected(|settings| settings.cluster.missed_pings = 0),
            "amux.opt.cluster.missed_pings must be a positive number"
        );
        assert_eq!(
            rejected(|settings| settings.discovery.interval_ms = 0),
            "amux.opt.discovery.interval_ms must be a positive number"
        );
        assert_eq!(
            rejected(|settings| settings.cluster.backoff_min_ms = 120_000),
            "amux.opt.cluster.backoff_min_ms (120000) must not be more than \
             amux.opt.cluster.backoff_max_ms (60000)"
        );
        assert_eq!(
            rejected(|settings| settings.pane.shell = Some(Vec::new())),
            "amux.opt.pane.shell must name a program"
        );
        assert_eq!(
            rejected(|settings| settings.worktrees.suffix.clear()),
            "amux.opt.worktrees.suffix must not be empty"
        );
        assert_eq!(
            rejected(|settings| settings.search.project_depth = 0),
            "amux.opt.search.project_depth must be a positive number"
        );

        let named = Settings {
            name: Some("desk".into()),
            ..Settings::default()
        };
        assert_eq!(named.validate(), Ok(()));
    }

    #[test]
    fn a_leading_tilde_in_project_paths_means_the_home_directory() {
        let settings = Settings {
            projects: BTreeMap::from([
                (
                    "amux".into(),
                    ProjectConfig {
                        default_server: Some("desk".into()),
                        worktrees_dir: Some("~/trees/amux".into()),
                    },
                ),
                ("notes".into(), ProjectConfig::default()),
                (
                    "srv".into(),
                    ProjectConfig {
                        default_server: None,
                        worktrees_dir: Some("/srv/~".into()),
                    },
                ),
            ]),
            search: SearchSettings {
                project_dirs: vec!["~/projects".into(), "~".into(), "/src".into()],
                project_depth: 1,
            },
            ..Settings::default()
        }
        .expand_home(Path::new("/home/tester"));

        assert_eq!(
            settings.search.project_dirs,
            [
                PathBuf::from("/home/tester/projects"),
                PathBuf::from("/home/tester"),
                PathBuf::from("/src"),
            ]
        );

        assert_eq!(
            settings.projects_dir,
            PathBuf::from("/home/tester/projects")
        );
        assert_eq!(
            settings.projects["amux"],
            ProjectConfig {
                default_server: Some("desk".into()),
                worktrees_dir: Some("/home/tester/trees/amux".into()),
            }
        );
        assert_eq!(settings.projects["notes"], ProjectConfig::default());
        assert_eq!(
            settings.projects["srv"].worktrees_dir,
            Some(PathBuf::from("/srv/~"))
        );
    }

    #[test]
    fn the_default_settings_round_trip() {
        let written = toml::to_string(&Settings::default()).unwrap();
        assert_eq!(
            toml::from_str::<Settings>(&written).unwrap(),
            Settings::default()
        );
        assert!(written.contains("term = \"screen-256color\""), "{written}");
        assert!(written.contains("strip_env = [\"SSH_\"]"), "{written}");
        assert!(written.contains("cross = \"┼\""), "{written}");
        assert!(!written.contains("shell"), "{written}");
    }
}
