use super::draw::{self, Rect, Span, Style};
use crate::client::router::{Level, Page};
use crate::keys::{Key, KeyCode, Mods};
use crate::settings::{Binding, Keymap, Theme, WhichKeySettings, PREFIX_TABLE, ROOT_TABLE};

const MARGIN: &str = " ";
const COLUMN_GAP: usize = 3;
const MAX_DESCRIPTION: usize = 30;
const RULE: &str = "─";
const NO_KEYS: &str = "no keys";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    key: String,
    description: String,
    group: bool,
}

#[derive(Debug, Clone, Copy)]
struct Styles {
    body: Style,
    border: Style,
    title: Style,
    key: Style,
    separator: Style,
    group: Style,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        let base = theme.which_key;
        Self {
            body: base.into(),
            border: base.merge(theme.which_key_border).into(),
            title: base.merge(theme.which_key_title).into(),
            key: base.merge(theme.which_key_key).into(),
            separator: base.merge(theme.which_key_separator).into(),
            group: base.merge(theme.which_key_group).into(),
        }
    }
}

#[derive(Debug)]
pub struct WhichKey {
    title: String,
    entries: Vec<Entry>,
    separator: String,
    styles: Styles,
    page: usize,
    pages: usize,
}

impl WhichKey {
    pub fn new(
        keymap: &Keymap,
        levels: &[Level],
        prefix: Key,
        settings: &WhichKeySettings,
        theme: &Theme,
    ) -> Self {
        Self {
            title: title(levels),
            entries: levels
                .last()
                .map(|level| entries(keymap, &level.table, prefix, settings))
                .unwrap_or_default(),
            separator: format!(" {} ", settings.separator),
            styles: Styles::new(theme),
            page: 0,
            pages: 1,
        }
    }

    pub fn turn(&mut self, page: Page) {
        self.page = match page {
            Page::Next => (self.page + 1) % self.pages,
            Page::Previous => self.page.checked_sub(1).unwrap_or(self.pages - 1),
        };
    }

    pub fn render(&mut self, area: Rect) -> Vec<u8> {
        let rows = usize::from(area.rows);
        let cols = usize::from(area.cols);
        let mut out = Vec::new();
        if rows < 2 || cols == 0 {
            return out;
        }
        let layout = Layout::new(&self.entries, &self.separator, rows - 1, cols);
        self.pages = layout.pages;
        self.page = self.page.min(layout.pages - 1);
        let top = usize::from(area.row) + rows - 1 - layout.rows;
        let col = usize::from(area.col);

        out.extend_from_slice(draw::SAVE_CURSOR);
        let rule = self.rule(cols, layout.pages);
        draw::draw_row(&mut out, top, col, cols, &rule, self.styles.border);
        for row in 0..layout.rows {
            let spans = self.row(&layout, row);
            draw::draw_row(&mut out, top + 1 + row, col, cols, &spans, self.styles.body);
        }
        out.extend_from_slice(draw::RESTORE_CURSOR);
        out
    }

    fn rule(&self, cols: usize, pages: usize) -> Vec<Span> {
        let border = self.styles.border;
        let left = format!("{RULE} ");
        let right = if pages > 1 {
            format!(" {}/{pages} {RULE}", self.page + 1)
        } else {
            RULE.to_owned()
        };
        let used = draw::width(&left) + draw::width(&self.title) + 1 + draw::width(&right);
        vec![
            Span::new(left, border),
            Span::new(self.title.clone(), self.styles.title),
            Span::new(" ", border),
            Span::new(RULE.repeat(cols.saturating_sub(used)), border),
            Span::new(right, border),
        ]
    }

    fn row(&self, layout: &Layout, row: usize) -> Vec<Span> {
        let margin = Span::new(MARGIN, self.styles.body);
        if self.entries.is_empty() {
            return vec![margin, Span::new(NO_KEYS, self.styles.separator)];
        }
        let first = self.page * layout.rows * layout.columns;
        let mut spans = vec![margin];
        for column in 0..layout.columns {
            let Some(entry) = self.entries.get(first + column * layout.rows + row) else {
                break;
            };
            if column > 0 {
                spans.push(Span::new(" ".repeat(COLUMN_GAP), self.styles.body));
            }
            let description_style = if entry.group {
                self.styles.group
            } else {
                self.styles.body
            };
            spans.extend([
                Span::new(padded(&entry.key, layout.key_width), self.styles.key),
                Span::new(self.separator.clone(), self.styles.separator),
                Span::new(
                    padded(&entry.description, layout.description_width),
                    description_style,
                ),
            ]);
        }
        spans
    }
}

#[derive(Debug)]
struct Layout {
    key_width: usize,
    description_width: usize,
    columns: usize,
    rows: usize,
    pages: usize,
}

impl Layout {
    fn new(entries: &[Entry], separator: &str, available_rows: usize, cols: usize) -> Self {
        let usable = cols.saturating_sub(2 * draw::width(MARGIN));
        let key_width = widest(entries.iter().map(|entry| entry.key.as_str()));
        let fixed = key_width + draw::width(separator);
        let description_width = widest(entries.iter().map(|entry| entry.description.as_str()))
            .min(MAX_DESCRIPTION)
            .min(usable.saturating_sub(fixed));
        let cell = fixed + description_width;
        let columns = ((usable + COLUMN_GAP) / (cell + COLUMN_GAP)).max(1);
        let rows = entries.len().div_ceil(columns).clamp(1, available_rows);
        let pages = entries.len().div_ceil(rows * columns).max(1);
        Self {
            key_width,
            description_width,
            columns,
            rows,
            pages,
        }
    }
}

fn widest<'a>(texts: impl Iterator<Item = &'a str>) -> usize {
    texts.map(draw::width).max().unwrap_or(0)
}

fn padded(text: &str, columns: usize) -> String {
    let text = draw::truncate(text, columns);
    let slack = columns.saturating_sub(draw::width(&text));
    format!("{text}{}", " ".repeat(slack))
}

fn title(levels: &[Level]) -> String {
    levels
        .iter()
        .map(|level| {
            level
                .key
                .map_or_else(|| level.table.clone(), |key| key.to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn entries(keymap: &Keymap, table: &str, prefix: Key, settings: &WhichKeySettings) -> Vec<Entry> {
    let Some(bindings) = keymap.table(table) else {
        return Vec::new();
    };
    let mut entries: Vec<(Key, Entry)> = bindings
        .iter()
        .map(|(key, binding)| {
            let group = matches!(
                binding,
                Binding::SwitchTable(name) if name != ROOT_TABLE && keymap.table(name).is_some()
            );
            let description = bindings
                .description(key)
                .map_or_else(|| binding.description(), str::to_owned);
            let description = if group {
                format!("{}{description}", settings.group_marker)
            } else {
                description
            };
            let entry = Entry {
                key: key.to_string(),
                description,
                group,
            };
            (*key, entry)
        })
        .collect();
    if table == PREFIX_TABLE && bindings.get(&prefix).is_none() {
        let entry = Entry {
            key: prefix.to_string(),
            description: Binding::SendPrefix.description(),
            group: false,
        };
        entries.push((prefix, entry));
    }
    entries.sort_by_key(|(key, _)| rank(key));
    entries.into_iter().map(|(_, entry)| entry).collect()
}

fn rank(key: &Key) -> (Mods, u8, char, bool, KeyCode) {
    match key.code {
        KeyCode::Char(character) if character.is_alphanumeric() => (
            key.mods,
            0,
            character.to_lowercase().next().unwrap_or(character),
            character.is_uppercase(),
            key.code,
        ),
        KeyCode::Char(character) => (key.mods, 1, character, false, key.code),
        code => (key.mods, 2, ' ', false, code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::chrome::testing::{screen_text, terminal};
    use crate::settings::{Color, StyleSpec, Table};

    fn key(notation: &str) -> Key {
        notation.parse().unwrap()
    }

    fn level(table: &str, notation: Option<&str>) -> Level {
        Level {
            table: table.into(),
            key: notation.map(key),
        }
    }

    fn prefix_levels() -> Vec<Level> {
        vec![level(PREFIX_TABLE, Some("C-b"))]
    }

    fn popup(keymap: &Keymap, levels: &[Level]) -> WhichKey {
        WhichKey::new(
            keymap,
            levels,
            key("C-b"),
            &WhichKeySettings::default(),
            &Theme::default(),
        )
    }

    fn area(rows: u16, cols: u16) -> Rect {
        Rect {
            row: 0,
            col: 0,
            rows,
            cols,
        }
    }

    fn drawn(popup: &mut WhichKey, rows: u16, cols: u16) -> vt100::Parser {
        let mut parser = terminal(rows + 1, cols);
        parser.process(b"shell$\x1b[1;4H");
        parser.process(&popup.render(area(rows, cols)));
        parser
    }

    fn small_keymap() -> Keymap {
        let mut keymap = Keymap {
            prefix: [
                (key("c"), Binding::NewWindow),
                (key("d"), Binding::Detach),
                (key("r"), Binding::SwitchTable("resize".into())),
                (key("C"), Binding::NewWindow),
                (key("%"), Binding::NextPane),
                (key("Up"), Binding::NextPane),
                (key("M-a"), Binding::KillPane),
            ]
            .into_iter()
            .collect(),
            ..Keymap::default()
        };
        keymap
            .prefix
            .describe(key("C"), "a very long description that is cut".into());
        keymap.custom.insert(
            "resize".into(),
            [(key("h"), Binding::NextPane)].into_iter().collect(),
        );
        keymap
    }

    #[test]
    fn the_keys_fill_columns_under_a_rule_that_names_what_was_typed() {
        let mut popup = popup(&small_keymap(), &prefix_levels());
        let rows = screen_text(&drawn(&mut popup, 10, 100));
        let rule = format!("─ C-b {}─", "─".repeat(93));
        assert_eq!(rows[0], "shell$");
        assert_eq!(
            &rows[5..11],
            [
                rule.as_str(),
                " c   → new window                       %   → next pane",
                " C   → a very long description that …   Up  → next pane",
                " d   → detach                           M-a → kill pane",
                " r   → +resize                          C-b → send prefix",
                "",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn the_cursor_and_the_rows_above_are_left_alone() {
        let mut popup = popup(&small_keymap(), &prefix_levels());
        let mut parser = drawn(&mut popup, 10, 100);
        assert_eq!(parser.screen().cursor_position(), (0, 3));
        parser.process(b"x");
        assert_eq!(screen_text(&parser)[0], "shexl$");
        assert!(!parser.screen().cell(0, 3).unwrap().bold());
    }

    #[test]
    fn keys_groups_and_the_rule_have_their_own_styles() {
        let theme = Theme {
            which_key: StyleSpec::colors(Color::WHITE, Color::BLUE),
            ..Theme::default()
        };
        let mut popup = WhichKey::new(
            &small_keymap(),
            &prefix_levels(),
            key("C-b"),
            &WhichKeySettings::default(),
            &theme,
        );
        let parser = drawn(&mut popup, 10, 100);
        let cell = |row, col| parser.screen().cell(row, col).unwrap().clone();
        assert!(cell(5, 0).dim());
        assert!(cell(5, 2).bold());
        assert!(cell(6, 1).bold());
        assert_eq!(cell(6, 1).fgcolor(), vt100::Color::Idx(6));
        assert!(cell(6, 5).dim());
        assert_eq!(cell(6, 7).fgcolor(), vt100::Color::Idx(7));
        assert_eq!(cell(9, 7).fgcolor(), vt100::Color::Idx(5));
        assert_eq!(cell(9, 7).bgcolor(), vt100::Color::Idx(4));
        assert_eq!(cell(6, 99).bgcolor(), vt100::Color::Idx(4));
        assert_eq!(cell(8, 99).bgcolor(), vt100::Color::Idx(4));
    }

    #[test]
    fn keys_sort_letters_before_symbols_named_keys_and_modifiers() {
        let mut keymap = Keymap {
            prefix: ["M-x", "F2", "Enter", "%", "B", "b", "a", "2", "C-a", "A"]
                .into_iter()
                .map(|notation| (key(notation), Binding::NextPane))
                .collect(),
            ..Keymap::default()
        };
        keymap.prefix.insert(key("C-b"), Binding::Detach);
        let keys: Vec<String> = popup(&keymap, &prefix_levels())
            .entries
            .into_iter()
            .map(|entry| entry.key)
            .collect();
        assert_eq!(
            keys,
            ["2", "a", "A", "b", "B", "%", "Enter", "F2", "M-x", "C-a", "C-b"]
        );
    }

    #[test]
    fn a_bound_prefix_key_replaces_the_send_prefix_entry() {
        let mut keymap = small_keymap();
        keymap.prefix.insert(key("C-b"), Binding::Detach);
        let popup = popup(&keymap, &prefix_levels());
        let prefix: Vec<&Entry> = popup
            .entries
            .iter()
            .filter(|entry| entry.key == "C-b")
            .collect();
        assert_eq!(prefix.len(), 1);
        assert_eq!(prefix[0].description, "detach");

        let resize = WhichKey::new(
            &small_keymap(),
            &[level(PREFIX_TABLE, Some("C-b")), level("resize", Some("r"))],
            key("C-b"),
            &WhichKeySettings::default(),
            &Theme::default(),
        );
        assert_eq!(resize.title, "C-b r");
        assert_eq!(
            resize.entries,
            [Entry {
                key: "h".into(),
                description: "next pane".into(),
                group: false,
            }]
        );
    }

    #[test]
    fn a_table_entered_without_a_key_is_named_and_an_empty_one_says_so() {
        let mut keymap = Keymap::default();
        keymap.custom.insert("empty".into(), Table::default());
        let mut popup = popup(&keymap, &[level("empty", None)]);
        let rows = screen_text(&drawn(&mut popup, 4, 20));
        assert_eq!(&rows[2..4], ["─ empty ────────────", " no keys"]);
    }

    #[test]
    fn a_short_area_splits_the_keys_into_pages_that_wrap_around() {
        let rule = |page: usize| format!("─ C-b {} {page}/4 ─", "─".repeat(18));
        let mut popup = popup(&small_keymap(), &prefix_levels());
        let rows = screen_text(&drawn(&mut popup, 3, 30));
        assert_eq!(
            rows[0..3],
            [
                rule(1),
                " c   → new window".to_owned(),
                " C   → a very long descripti…".to_owned(),
            ]
        );

        popup.turn(Page::Next);
        let rows = screen_text(&drawn(&mut popup, 3, 30));
        assert_eq!(
            rows[0..3],
            [rule(2), " d   → detach".into(), " r   → +resize".into()]
        );
        popup.turn(Page::Previous);
        popup.turn(Page::Previous);
        let rows = screen_text(&drawn(&mut popup, 3, 30));
        assert_eq!(
            rows[0..3],
            [
                rule(4),
                " M-a → kill pane".into(),
                " C-b → send prefix".into()
            ]
        );
        popup.turn(Page::Next);
        let rows = screen_text(&drawn(&mut popup, 3, 30));
        assert_eq!(rows[0], rule(1));
    }

    #[test]
    fn a_resize_that_needs_fewer_pages_keeps_a_valid_page() {
        let mut popup = popup(&small_keymap(), &prefix_levels());
        drawn(&mut popup, 3, 30);
        popup.turn(Page::Previous);
        let rows = screen_text(&drawn(&mut popup, 10, 100));
        assert!(rows[5].ends_with("──"), "{:?}", rows[5]);
        assert!(rows[6].starts_with(" c   → new window "), "{:?}", rows[6]);
    }

    #[test]
    fn a_tiny_area_draws_nothing_or_cuts_the_rows() {
        let mut popup = popup(&small_keymap(), &prefix_levels());
        assert!(popup.render(area(1, 80)).is_empty());
        assert!(popup.render(area(5, 0)).is_empty());
        let rows = screen_text(&drawn(&mut popup, 2, 8));
        assert_eq!(rows[0..3], ["─ C-b  …", " c   →", ""].map(str::to_owned));
    }

    #[test]
    fn the_separator_and_the_group_marker_come_from_the_settings() {
        let settings = WhichKeySettings {
            separator: "::".into(),
            group_marker: "> ".into(),
            ..WhichKeySettings::default()
        };
        let mut popup = WhichKey::new(
            &small_keymap(),
            &prefix_levels(),
            key("C-b"),
            &settings,
            &Theme::default(),
        );
        let rows = screen_text(&drawn(&mut popup, 10, 100));
        assert!(rows[6].starts_with(" c   :: new window"), "{:?}", rows[6]);
        assert!(rows[9].starts_with(" r   :: > resize "), "{:?}", rows[9]);
    }

    #[test]
    fn the_default_prefix_table_fits_an_ordinary_terminal_on_one_page() {
        let mut popup = popup(&Keymap::default(), &prefix_levels());
        let rows = screen_text(&drawn(&mut popup, 23, 80));
        let rule = rows
            .iter()
            .position(|row| row.starts_with("─ C-b ─"))
            .unwrap();
        assert!(rows[rule].ends_with("──"), "{:?}", rows[rule]);
        let body = rows[rule + 1..].join("\n");
        for text in [
            "c     → new window",
            "?     → show prefix keys",
            "Right → pane right",
            "C-b   → send prefix",
            "%     → split left/right",
        ] {
            assert!(body.contains(text), "{text} in\n{body}");
        }
    }
}
