use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::callback::CallbackId;
use crate::keys::{Decoded, Key, KeyCode};
use crate::protocol::{Direction, Split};

pub const ROOT_TABLE: &str = "root";
pub const PREFIX_TABLE: &str = "prefix";
pub const PROMPT_TABLE: &str = "prompt";
pub const TREE_TABLE: &str = "tree";
pub const PICKER_TABLE: &str = "picker";
pub const SEARCH_TABLE: &str = "search";
pub const COPY_TABLE: &str = "copy";
const PANEL_TABLES: [&str; 4] = [PROMPT_TABLE, TREE_TABLE, PICKER_TABLE, COPY_TABLE];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Binding {
    Detach,
    SendPrefix,
    NewWindow,
    NextWindow,
    PreviousWindow,
    SelectWindow(usize),
    SplitPane(#[serde(with = "SplitName")] Split),
    NextPane,
    SelectPane(#[serde(with = "DirectionName")] Direction),
    KillPane,
    KillWindow,
    RenameWindow,
    RenameSession,
    ClusterTree,
    SearchProjects,
    SearchWorktrees,
    SwitchTable(String),
    ReloadConfig,
    WhichKey(String),
    CopyMode,
    CopyModePageUp,
    PasteBuffer,
    #[serde(skip)]
    Callback(CallbackId),
}

impl Binding {
    pub fn description(&self) -> String {
        match self {
            Self::Detach => "detach".into(),
            Self::SendPrefix => "send prefix".into(),
            Self::NewWindow => "new window".into(),
            Self::NextWindow => "next window".into(),
            Self::PreviousWindow => "previous window".into(),
            Self::SelectWindow(index) => format!("window {index}"),
            Self::SplitPane(Split::LeftRight) => "split left/right".into(),
            Self::SplitPane(Split::TopBottom) => "split top/bottom".into(),
            Self::NextPane => "next pane".into(),
            Self::SelectPane(direction) => format!("pane {}", direction_name(*direction)),
            Self::KillPane => "kill pane".into(),
            Self::KillWindow => "kill window".into(),
            Self::RenameWindow => "rename window".into(),
            Self::RenameSession => "rename session".into(),
            Self::ClusterTree => "cluster tree".into(),
            Self::SearchProjects => "search projects".into(),
            Self::SearchWorktrees => "search worktrees".into(),
            Self::SwitchTable(table) => table.clone(),
            Self::ReloadConfig => "reload config".into(),
            Self::WhichKey(table) => format!("show {table} keys"),
            Self::CopyMode => "copy mode".into(),
            Self::CopyModePageUp => "copy mode, page up".into(),
            Self::PasteBuffer => "paste buffer".into(),
            Self::Callback(_) => "lua function".into(),
        }
    }
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Split", rename_all = "kebab-case")]
enum SplitName {
    LeftRight,
    TopBottom,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Direction", rename_all = "kebab-case")]
enum DirectionName {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptAction {
    Submit,
    Cancel,
    DeleteBackward,
    DeleteForward,
    DeleteLine,
    CursorLeft,
    CursorRight,
    CursorStart,
    CursorEnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TreeAction {
    Down,
    Up,
    Top,
    Bottom,
    Collapse,
    Expand,
    Pick,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerAction {
    Down,
    Up,
    Pick,
    Cancel,
    DeleteBackward,
    DeleteWord,
    DeleteLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyAction {
    CursorLeft,
    CursorDown,
    CursorUp,
    CursorRight,
    NextWord,
    PreviousWord,
    NextWordEnd,
    NextSpace,
    PreviousSpace,
    NextSpaceEnd,
    StartOfLine,
    BackToIndentation,
    EndOfLine,
    HistoryTop,
    HistoryBottom,
    TopLine,
    MiddleLine,
    BottomLine,
    ScrollUp,
    ScrollDown,
    HalfpageUp,
    HalfpageDown,
    PageUp,
    PageDown,
    BeginSelection,
    SelectLine,
    OtherEnd,
    ClearSelection,
    CopySelectionAndCancel,
    ClearSelectionOrCancel,
    Cancel,
    RefreshFromPane,
    SearchForward,
    SearchBackward,
    SearchAgain,
    SearchReverse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table<A> {
    bindings: BTreeMap<Key, A>,
    descriptions: BTreeMap<Key, String>,
}

impl<A> Default for Table<A> {
    fn default() -> Self {
        Self {
            bindings: BTreeMap::new(),
            descriptions: BTreeMap::new(),
        }
    }
}

impl<A> Table<A> {
    pub fn get(&self, key: &Key) -> Option<&A> {
        self.bindings.get(key)
    }

    pub fn insert(&mut self, key: Key, action: A) -> Option<A> {
        self.descriptions.remove(&key);
        self.bindings.insert(key, action)
    }

    pub fn describe(&mut self, key: Key, description: String) {
        if self.bindings.contains_key(&key) {
            self.descriptions.insert(key, description);
        }
    }

    pub fn description(&self, key: &Key) -> Option<&str> {
        self.descriptions.get(key).map(String::as_str)
    }

    pub fn remove(&mut self, key: &Key) -> Option<A> {
        self.descriptions.remove(key);
        self.bindings.remove(key)
    }

    pub fn clear(&mut self) {
        self.bindings.clear();
        self.descriptions.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &A)> {
        self.bindings.iter()
    }
}

impl<A: PartialEq> Table<A> {
    pub fn key_for(&self, action: &A) -> Option<&Key> {
        self.bindings
            .iter()
            .find(|(_, bound)| *bound == action)
            .map(|(key, _)| key)
    }
}

impl<A: Clone> Table<A> {
    pub fn resolve(&self, decoded: &Decoded) -> Vec<(Key, Option<A>)> {
        let Some(key) = decoded.key else {
            return Vec::new();
        };
        if let Some(action) = self.get(&key) {
            return vec![(key, Some(action.clone()))];
        }
        if let Some((escape, rest)) = decoded.split_escape() {
            return [escape, rest]
                .iter()
                .flat_map(|part| self.resolve(part))
                .collect();
        }
        let unmodified = match key.code {
            KeyCode::Char(_) => None,
            code => self.get(&Key::from(code)).cloned(),
        };
        vec![(key, unmodified)]
    }
}

impl<A> FromIterator<(Key, A)> for Table<A> {
    fn from_iter<I: IntoIterator<Item = (Key, A)>>(bindings: I) -> Self {
        Self {
            bindings: bindings.into_iter().collect(),
            descriptions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub root: Table<Binding>,
    pub prefix: Table<Binding>,
    pub prompt: Table<PromptAction>,
    pub tree: Table<TreeAction>,
    pub picker: Table<PickerAction>,
    pub copy: Table<CopyAction>,
    pub custom: BTreeMap<String, Table<Binding>>,
}

impl Keymap {
    pub fn table(&self, name: &str) -> Option<&Table<Binding>> {
        match name {
            ROOT_TABLE => Some(&self.root),
            PREFIX_TABLE => Some(&self.prefix),
            _ => self.custom.get(name),
        }
    }

    pub fn table_mut(&mut self, name: &str) -> Option<&mut Table<Binding>> {
        match name {
            ROOT_TABLE => Some(&mut self.root),
            PREFIX_TABLE => Some(&mut self.prefix),
            _ => self.custom.get_mut(name),
        }
    }

    pub fn table_entry(&mut self, name: &str) -> &mut Table<Binding> {
        match name {
            ROOT_TABLE => &mut self.root,
            PREFIX_TABLE => &mut self.prefix,
            _ => self.custom.entry(name.to_owned()).or_default(),
        }
    }

    pub fn submap<'a>(&'a self, table: &'a str, keys: &[Key]) -> Option<&'a str> {
        keys.iter()
            .try_fold(table, |name, key| submap_name(self.table(name)?.get(key)?))
    }

    pub fn open_submap(&mut self, table: &str, keys: &[Key]) -> Result<String, String> {
        let mut name = table.to_owned();
        for key in keys {
            let bindings = self.table_entry(&name);
            name = match bindings.get(key) {
                None => {
                    let created = format!("{name} {key}");
                    bindings.insert(*key, Binding::SwitchTable(created.clone()));
                    created
                }
                Some(binding) => submap_name(binding).map(str::to_owned).ok_or_else(|| {
                    format!(
                        "{key} in the {name} table is bound to {}, not to a submap",
                        binding.description()
                    )
                })?,
            };
        }
        self.table_entry(&name);
        Ok(name)
    }

    pub fn hint_for(&self, binding: &Binding, prefix: &Key) -> Option<String> {
        self.root.key_for(binding).map(Key::label).or_else(|| {
            self.prefix
                .key_for(binding)
                .map(|key| format!("{} {}", prefix.label(), key.label()))
        })
    }
}

fn submap_name(binding: &Binding) -> Option<&str> {
    match binding {
        Binding::SwitchTable(name) if !is_panel_table(name) && name != ROOT_TABLE => Some(name),
        _ => None,
    }
}

pub fn is_panel_table(name: &str) -> bool {
    PANEL_TABLES.contains(&name)
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            root: Table::default(),
            prefix: prefix_table(),
            prompt: prompt_table(),
            tree: tree_table(),
            picker: picker_table(),
            copy: copy_table(),
            custom: BTreeMap::from([(SEARCH_TABLE.to_owned(), search_table())]),
        }
    }
}

fn search_table() -> Table<Binding> {
    [
        (Key::char('p'), Binding::SearchProjects),
        (Key::char('w'), Binding::SearchWorktrees),
        (Key::char('s'), Binding::ClusterTree),
    ]
    .into_iter()
    .collect()
}

fn prefix_table() -> Table<Binding> {
    let windows = ('0'..='9')
        .zip(0..)
        .map(|(digit, index)| (Key::char(digit), Binding::SelectWindow(index)));
    [
        (Key::char('d'), Binding::Detach),
        (Key::char(','), Binding::RenameWindow),
        (Key::char('$'), Binding::RenameSession),
        (
            Key::char('s'),
            Binding::SwitchTable(SEARCH_TABLE.to_owned()),
        ),
        (Key::char('c'), Binding::NewWindow),
        (Key::char('n'), Binding::NextWindow),
        (Key::char('p'), Binding::PreviousWindow),
        (Key::char('%'), Binding::SplitPane(Split::LeftRight)),
        (Key::char('"'), Binding::SplitPane(Split::TopBottom)),
        (Key::char('o'), Binding::NextPane),
        (Key::char('x'), Binding::KillPane),
        (Key::char('&'), Binding::KillWindow),
        (Key::char('r'), Binding::ReloadConfig),
        (Key::char('?'), Binding::WhichKey(PREFIX_TABLE.to_owned())),
        (Key::char('['), Binding::CopyMode),
        (Key::char(']'), Binding::PasteBuffer),
        (Key::from(KeyCode::Up), Binding::SelectPane(Direction::Up)),
        (
            Key::from(KeyCode::Down),
            Binding::SelectPane(Direction::Down),
        ),
        (
            Key::from(KeyCode::Left),
            Binding::SelectPane(Direction::Left),
        ),
        (
            Key::from(KeyCode::Right),
            Binding::SelectPane(Direction::Right),
        ),
    ]
    .into_iter()
    .chain(windows)
    .collect()
}

fn prompt_table() -> Table<PromptAction> {
    [
        (Key::from(KeyCode::Enter), PromptAction::Submit),
        (Key::from(KeyCode::Escape), PromptAction::Cancel),
        (Key::ctrl('c'), PromptAction::Cancel),
        (Key::from(KeyCode::Backspace), PromptAction::DeleteBackward),
        (Key::from(KeyCode::Delete), PromptAction::DeleteForward),
        (Key::ctrl('u'), PromptAction::DeleteLine),
        (Key::from(KeyCode::Left), PromptAction::CursorLeft),
        (Key::from(KeyCode::Right), PromptAction::CursorRight),
        (Key::from(KeyCode::Home), PromptAction::CursorStart),
        (Key::ctrl('a'), PromptAction::CursorStart),
        (Key::from(KeyCode::End), PromptAction::CursorEnd),
        (Key::ctrl('e'), PromptAction::CursorEnd),
    ]
    .into_iter()
    .collect()
}

fn picker_table() -> Table<PickerAction> {
    [
        (Key::from(KeyCode::Down), PickerAction::Down),
        (Key::ctrl('n'), PickerAction::Down),
        (Key::from(KeyCode::Up), PickerAction::Up),
        (Key::ctrl('p'), PickerAction::Up),
        (Key::from(KeyCode::Enter), PickerAction::Pick),
        (Key::from(KeyCode::Escape), PickerAction::Cancel),
        (Key::ctrl('c'), PickerAction::Cancel),
        (Key::from(KeyCode::Backspace), PickerAction::DeleteBackward),
        (Key::ctrl('w'), PickerAction::DeleteWord),
        (Key::ctrl('u'), PickerAction::DeleteLine),
    ]
    .into_iter()
    .collect()
}

fn copy_table() -> Table<CopyAction> {
    [
        (Key::char('h'), CopyAction::CursorLeft),
        (Key::from(KeyCode::Left), CopyAction::CursorLeft),
        (Key::char('j'), CopyAction::CursorDown),
        (Key::from(KeyCode::Down), CopyAction::CursorDown),
        (Key::char('k'), CopyAction::CursorUp),
        (Key::from(KeyCode::Up), CopyAction::CursorUp),
        (Key::char('l'), CopyAction::CursorRight),
        (Key::from(KeyCode::Right), CopyAction::CursorRight),
        (Key::char('w'), CopyAction::NextWord),
        (Key::char('b'), CopyAction::PreviousWord),
        (Key::char('e'), CopyAction::NextWordEnd),
        (Key::char('W'), CopyAction::NextSpace),
        (Key::char('B'), CopyAction::PreviousSpace),
        (Key::char('E'), CopyAction::NextSpaceEnd),
        (Key::char('0'), CopyAction::StartOfLine),
        (Key::from(KeyCode::Home), CopyAction::StartOfLine),
        (Key::char('^'), CopyAction::BackToIndentation),
        (Key::char('$'), CopyAction::EndOfLine),
        (Key::from(KeyCode::End), CopyAction::EndOfLine),
        (Key::char('g'), CopyAction::HistoryTop),
        (Key::char('G'), CopyAction::HistoryBottom),
        (Key::char('H'), CopyAction::TopLine),
        (Key::char('M'), CopyAction::MiddleLine),
        (Key::char('L'), CopyAction::BottomLine),
        (Key::ctrl('y'), CopyAction::ScrollUp),
        (Key::ctrl('e'), CopyAction::ScrollDown),
        (Key::ctrl('u'), CopyAction::HalfpageUp),
        (Key::ctrl('d'), CopyAction::HalfpageDown),
        (Key::from(KeyCode::PageUp), CopyAction::PageUp),
        (Key::from(KeyCode::PageDown), CopyAction::PageDown),
        (Key::ctrl('f'), CopyAction::PageDown),
        (Key::char('v'), CopyAction::BeginSelection),
        (Key::char(' '), CopyAction::BeginSelection),
        (Key::char('V'), CopyAction::SelectLine),
        (Key::char('o'), CopyAction::OtherEnd),
        (Key::char('y'), CopyAction::CopySelectionAndCancel),
        (
            Key::from(KeyCode::Enter),
            CopyAction::CopySelectionAndCancel,
        ),
        (Key::char('q'), CopyAction::Cancel),
        (Key::ctrl('c'), CopyAction::Cancel),
        (
            Key::from(KeyCode::Escape),
            CopyAction::ClearSelectionOrCancel,
        ),
        (Key::char('r'), CopyAction::RefreshFromPane),
        (Key::char('/'), CopyAction::SearchForward),
        (Key::char('?'), CopyAction::SearchBackward),
        (Key::char('n'), CopyAction::SearchAgain),
        (Key::char('N'), CopyAction::SearchReverse),
    ]
    .into_iter()
    .collect()
}

fn tree_table() -> Table<TreeAction> {
    [
        (Key::char('j'), TreeAction::Down),
        (Key::from(KeyCode::Down), TreeAction::Down),
        (Key::char('k'), TreeAction::Up),
        (Key::from(KeyCode::Up), TreeAction::Up),
        (Key::char('g'), TreeAction::Top),
        (Key::from(KeyCode::Home), TreeAction::Top),
        (Key::char('G'), TreeAction::Bottom),
        (Key::from(KeyCode::End), TreeAction::Bottom),
        (Key::char('h'), TreeAction::Collapse),
        (Key::from(KeyCode::Left), TreeAction::Collapse),
        (Key::char('l'), TreeAction::Expand),
        (Key::from(KeyCode::Right), TreeAction::Expand),
        (Key::from(KeyCode::Enter), TreeAction::Pick),
        (Key::from(KeyCode::Escape), TreeAction::Cancel),
        (Key::char('q'), TreeAction::Cancel),
        (Key::ctrl('c'), TreeAction::Cancel),
    ]
    .into_iter()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyDecoder;

    fn key(notation: &str) -> Key {
        notation.parse().unwrap()
    }

    fn resolved(table: &Table<PromptAction>, input: &[u8]) -> Vec<(Key, Option<PromptAction>)> {
        KeyDecoder::default()
            .feed(input)
            .iter()
            .flat_map(|decoded| table.resolve(decoded))
            .collect()
    }

    #[test]
    fn the_default_prefix_table_holds_every_built_in_binding() {
        let prefix = Keymap::default().prefix;
        for (notation, binding) in [
            ("d", Binding::Detach),
            (",", Binding::RenameWindow),
            ("$", Binding::RenameSession),
            ("s", Binding::SwitchTable(SEARCH_TABLE.into())),
            ("c", Binding::NewWindow),
            ("n", Binding::NextWindow),
            ("p", Binding::PreviousWindow),
            ("0", Binding::SelectWindow(0)),
            ("9", Binding::SelectWindow(9)),
            ("%", Binding::SplitPane(Split::LeftRight)),
            ("\"", Binding::SplitPane(Split::TopBottom)),
            ("o", Binding::NextPane),
            ("x", Binding::KillPane),
            ("&", Binding::KillWindow),
            ("r", Binding::ReloadConfig),
            ("?", Binding::WhichKey(PREFIX_TABLE.into())),
            ("[", Binding::CopyMode),
            ("]", Binding::PasteBuffer),
            ("Up", Binding::SelectPane(Direction::Up)),
            ("Left", Binding::SelectPane(Direction::Left)),
        ] {
            assert_eq!(prefix.get(&key(notation)), Some(&binding), "{notation}");
        }
        assert_eq!(prefix.iter().count(), 30);
        assert_eq!(prefix.get(&key("C-b")), None);
        assert_eq!(Keymap::default().root, Table::default());
    }

    #[test]
    fn the_default_search_submap_finds_projects_worktrees_and_sessions() {
        let keymap = Keymap::default();
        assert_eq!(keymap.submap(PREFIX_TABLE, &[key("s")]), Some(SEARCH_TABLE));
        let search = &keymap.custom[SEARCH_TABLE];
        assert_eq!(search.get(&key("p")), Some(&Binding::SearchProjects));
        assert_eq!(search.get(&key("w")), Some(&Binding::SearchWorktrees));
        assert_eq!(search.get(&key("s")), Some(&Binding::ClusterTree));
        assert_eq!(search.iter().count(), 3);
        assert_eq!(keymap.custom.len(), 1);
    }

    #[test]
    fn a_sequence_walks_submaps_and_creates_the_missing_ones() {
        let mut keymap = Keymap::default();
        keymap
            .prefix
            .insert(key("g"), Binding::SwitchTable("git".into()));

        assert_eq!(keymap.open_submap(PREFIX_TABLE, &[]).unwrap(), "prefix");
        assert_eq!(
            keymap.open_submap(PREFIX_TABLE, &[key("g")]).unwrap(),
            "git"
        );
        assert_eq!(
            keymap
                .open_submap(PREFIX_TABLE, &[key("g"), key("l")])
                .unwrap(),
            "git l"
        );
        assert_eq!(
            keymap.custom["git"].get(&key("l")),
            Some(&Binding::SwitchTable("git l".into()))
        );
        assert_eq!(keymap.custom["git l"], Table::default());
        assert_eq!(
            keymap.open_submap(ROOT_TABLE, &[key("M-s")]).unwrap(),
            "root M-s"
        );

        assert_eq!(
            keymap.submap(PREFIX_TABLE, &[key("g"), key("l")]),
            Some("git l")
        );
        assert_eq!(keymap.submap(PREFIX_TABLE, &[key("z")]), None);
        assert_eq!(keymap.submap("nowhere", &[key("g")]), None);
        assert_eq!(keymap.submap("nowhere", &[]), Some("nowhere"));
    }

    #[test]
    fn a_sequence_never_walks_through_a_key_bound_to_something_else() {
        let mut keymap = Keymap::default();
        keymap
            .prefix
            .insert(key("t"), Binding::SwitchTable(TREE_TABLE.into()));
        keymap
            .prefix
            .insert(key("R"), Binding::SwitchTable(ROOT_TABLE.into()));

        assert_eq!(
            keymap.open_submap(PREFIX_TABLE, &[key("d"), key("x")]),
            Err("d in the prefix table is bound to detach, not to a submap".into())
        );
        assert!(keymap.open_submap(PREFIX_TABLE, &[key("t")]).is_err());
        assert!(keymap.open_submap(PREFIX_TABLE, &[key("R")]).is_err());
        assert_eq!(keymap.submap(PREFIX_TABLE, &[key("d")]), None);
        assert_eq!(keymap.submap(PREFIX_TABLE, &[key("t")]), None);
        assert_eq!(keymap.prefix.get(&key("d")), Some(&Binding::Detach));
    }

    #[test]
    fn the_hint_names_the_prefix_and_the_key() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap.hint_for(&Binding::Detach, &key("C-b")).as_deref(),
            Some("Ctrl-b d")
        );
        assert_eq!(
            keymap.hint_for(&Binding::Detach, &key("M-a")).as_deref(),
            Some("Alt-a d")
        );
        assert_eq!(keymap.hint_for(&Binding::SendPrefix, &key("C-b")), None);

        let mut keymap = Keymap::default();
        keymap.root.insert(key("M-d"), Binding::Detach);
        assert_eq!(
            keymap.hint_for(&Binding::Detach, &key("C-b")).as_deref(),
            Some("Alt-d")
        );
    }

    #[test]
    fn a_description_lives_until_its_key_is_bound_again_or_removed() {
        let mut table = Keymap::default().prefix;
        assert_eq!(table.description(&key("c")), None);
        table.describe(key("c"), "open a shell".into());
        assert_eq!(table.description(&key("c")), Some("open a shell"));
        table.describe(key("z"), "unbound".into());
        assert_eq!(table.description(&key("z")), None);

        table.insert(key("c"), Binding::NewWindow);
        assert_eq!(table.description(&key("c")), None);
        table.describe(key("c"), "open a shell".into());
        table.remove(&key("c"));
        table.insert(key("c"), Binding::NewWindow);
        assert_eq!(table.description(&key("c")), None);

        table.describe(key("d"), "leave".into());
        table.clear();
        table.insert(key("d"), Binding::Detach);
        assert_eq!(table.description(&key("d")), None);
    }

    #[test]
    fn every_binding_has_a_short_description() {
        for (binding, description) in [
            (Binding::SelectWindow(3), "window 3"),
            (Binding::SplitPane(Split::LeftRight), "split left/right"),
            (Binding::SplitPane(Split::TopBottom), "split top/bottom"),
            (Binding::SelectPane(Direction::Up), "pane up"),
            (Binding::SwitchTable("resize".into()), "resize"),
            (Binding::WhichKey(PREFIX_TABLE.into()), "show prefix keys"),
            (Binding::CopyMode, "copy mode"),
            (Binding::CopyModePageUp, "copy mode, page up"),
            (Binding::PasteBuffer, "paste buffer"),
            (Binding::Callback(CallbackId(4)), "lua function"),
        ] {
            assert_eq!(binding.description(), description);
        }
    }

    #[test]
    fn panel_lookup_falls_back_to_the_unmodified_key() {
        let prompt = Keymap::default().prompt;
        assert_eq!(
            resolved(&prompt, b"\x1b[1;5D\x1b[3;2~"),
            vec![
                (key("C-Left"), Some(PromptAction::CursorLeft)),
                (key("S-Delete"), Some(PromptAction::DeleteForward)),
            ]
        );
        assert_eq!(
            resolved(&prompt, b"\x01\x1a%"),
            vec![
                (key("C-a"), Some(PromptAction::CursorStart)),
                (key("C-z"), None),
                (key("%"), None),
            ]
        );
    }

    #[test]
    fn an_unbound_meta_key_is_escape_then_the_key() {
        let prompt = Keymap::default().prompt;
        assert_eq!(
            resolved(&prompt, b"\x1bx\x1b\x1b[D"),
            vec![
                (key("Escape"), Some(PromptAction::Cancel)),
                (key("x"), None),
                (key("Escape"), Some(PromptAction::Cancel)),
                (key("Left"), Some(PromptAction::CursorLeft)),
            ]
        );

        let mut bound = prompt.clone();
        bound.insert(key("M-x"), PromptAction::DeleteLine);
        assert_eq!(
            resolved(&bound, b"\x1bx"),
            vec![(key("M-x"), Some(PromptAction::DeleteLine))]
        );
    }

    #[test]
    fn the_default_copy_table_uses_vi_keys() {
        let keymap = Keymap::default();
        for (notation, action) in [
            ("k", CopyAction::CursorUp),
            ("Down", CopyAction::CursorDown),
            ("W", CopyAction::NextSpace),
            ("0", CopyAction::StartOfLine),
            ("^", CopyAction::BackToIndentation),
            ("End", CopyAction::EndOfLine),
            ("g", CopyAction::HistoryTop),
            ("G", CopyAction::HistoryBottom),
            ("M", CopyAction::MiddleLine),
            ("C-u", CopyAction::HalfpageUp),
            ("C-f", CopyAction::PageDown),
            ("PageUp", CopyAction::PageUp),
            ("v", CopyAction::BeginSelection),
            ("Space", CopyAction::BeginSelection),
            ("V", CopyAction::SelectLine),
            ("o", CopyAction::OtherEnd),
            ("y", CopyAction::CopySelectionAndCancel),
            ("Enter", CopyAction::CopySelectionAndCancel),
            ("q", CopyAction::Cancel),
            ("C-c", CopyAction::Cancel),
            ("Escape", CopyAction::ClearSelectionOrCancel),
            ("r", CopyAction::RefreshFromPane),
            ("/", CopyAction::SearchForward),
            ("?", CopyAction::SearchBackward),
            ("n", CopyAction::SearchAgain),
            ("N", CopyAction::SearchReverse),
        ] {
            assert_eq!(keymap.copy.get(&key(notation)), Some(&action), "{notation}");
        }
        assert_eq!(keymap.copy.iter().count(), 45);
        assert_eq!(keymap.copy.key_for(&CopyAction::ClearSelection), None);
        assert_eq!(keymap.copy.get(&key("C-b")), None);
        assert!(is_panel_table(COPY_TABLE));
        assert_eq!(keymap.table(COPY_TABLE), None);
        assert_eq!(keymap.prefix.key_for(&Binding::CopyModePageUp), None);
    }

    #[test]
    fn unknown_input_resolves_to_nothing() {
        assert_eq!(resolved(&Keymap::default().prompt, b"\x1b[<0;3;4M"), vec![]);
    }
}
