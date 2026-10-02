use serde::{Deserialize, Serialize};

use super::style::{Color, StyleSpec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub status: StyleSpec,
    pub status_session: StyleSpec,
    pub status_active_window: StyleSpec,
    pub status_offline: StyleSpec,
    pub message: StyleSpec,
    pub prompt: StyleSpec,
    pub prompt_label: StyleSpec,
    pub tree: StyleSpec,
    pub tree_server: StyleSpec,
    pub tree_stale: StyleSpec,
    pub tree_cursor: StyleSpec,
    pub picker: StyleSpec,
    pub picker_label: StyleSpec,
    pub picker_cursor: StyleSpec,
    pub picker_match: StyleSpec,
    pub picker_detail: StyleSpec,
    pub which_key: StyleSpec,
    pub which_key_border: StyleSpec,
    pub which_key_title: StyleSpec,
    pub which_key_key: StyleSpec,
    pub which_key_separator: StyleSpec,
    pub which_key_group: StyleSpec,
    pub overlay_text: StyleSpec,
    pub overlay_border: StyleSpec,
    pub pane_border: StyleSpec,
    pub pane_border_active: StyleSpec,
    pub copy_position: StyleSpec,
    pub copy_selection: StyleSpec,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            status: StyleSpec::colors(Color::BLACK, Color::GREEN),
            status_session: StyleSpec::BOLD,
            status_active_window: StyleSpec::BOLD.merge(StyleSpec::REVERSE),
            status_offline: StyleSpec::colors(Color::WHITE, Color::RED).merge(StyleSpec::BOLD),
            message: StyleSpec::colors(Color::BLACK, Color::YELLOW),
            prompt: StyleSpec::colors(Color::BLACK, Color::YELLOW),
            prompt_label: StyleSpec::BOLD,
            tree: StyleSpec::EMPTY,
            tree_server: StyleSpec::BOLD,
            tree_stale: StyleSpec::DIM,
            tree_cursor: StyleSpec::REVERSE,
            picker: StyleSpec::EMPTY,
            picker_label: StyleSpec::BOLD,
            picker_cursor: StyleSpec::REVERSE,
            picker_match: StyleSpec {
                fg: Some(Color::YELLOW),
                ..StyleSpec::BOLD
            },
            picker_detail: StyleSpec::DIM,
            which_key: StyleSpec::EMPTY,
            which_key_border: StyleSpec::DIM,
            which_key_title: StyleSpec::BOLD,
            which_key_key: StyleSpec {
                fg: Some(Color::CYAN),
                ..StyleSpec::BOLD
            },
            which_key_separator: StyleSpec::DIM,
            which_key_group: StyleSpec {
                fg: Some(Color::MAGENTA),
                ..StyleSpec::EMPTY
            },
            overlay_text: StyleSpec::BOLD,
            overlay_border: StyleSpec::EMPTY,
            pane_border: StyleSpec::EMPTY,
            pane_border_active: StyleSpec {
                fg: Some(Color::GREEN),
                ..StyleSpec::EMPTY
            },
            copy_position: StyleSpec::colors(Color::BLACK, Color::YELLOW),
            copy_selection: StyleSpec::REVERSE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_overrides_only_the_slots_it_names() {
        let theme: Theme =
            toml::from_str("[status]\nbg = \"#8ec07c\"\n\n[tree_cursor]\nunderline = true")
                .unwrap();
        assert_eq!(
            theme,
            Theme {
                status: StyleSpec {
                    bg: Some(Color::Rgb(0x8e, 0xc0, 0x7c)),
                    ..StyleSpec::EMPTY
                },
                tree_cursor: StyleSpec {
                    underline: Some(true),
                    ..StyleSpec::EMPTY
                },
                ..Theme::default()
            }
        );
        assert!(toml::from_str::<Theme>("[pane_bordr]\nfg = \"red\"").is_err());
        assert!(toml::from_str::<Theme>("[status]\nblink = true").is_err());
    }

    #[test]
    fn the_default_theme_round_trips() {
        let written = toml::to_string(&Theme::default()).unwrap();
        assert_eq!(toml::from_str::<Theme>(&written).unwrap(), Theme::default());
    }
}
