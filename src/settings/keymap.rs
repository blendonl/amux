use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::callback::CallbackId;
use crate::keys::{Decoded, Key, KeyCode};
use crate::protocol::{Direction, Split};

pub const ROOT_TABLE: &str = "root";
pub const PREFIX_TABLE: &str = "prefix";
pub const PROMPT_TABLE: &str = "prompt";
pub const TREE_TABLE: &str = "tree";

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
    SwitchTable(String),
    #[serde(skip)]
    Callback(CallbackId),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table<A>(BTreeMap<Key, A>);

impl<A> Default for Table<A> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<A> Table<A> {
    pub fn get(&self, key: &Key) -> Option<&A> {
        self.0.get(key)
    }

    pub fn insert(&mut self, key: Key, action: A) -> Option<A> {
        self.0.insert(key, action)
    }

    pub fn remove(&mut self, key: &Key) -> Option<A> {
        self.0.remove(key)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &A)> {
        self.0.iter()
    }
}

impl<A: PartialEq> Table<A> {
    pub fn key_for(&self, action: &A) -> Option<&Key> {
        self.0
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
        Self(bindings.into_iter().collect())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    pub root: Table<Binding>,
    pub prefix: Table<Binding>,
    pub prompt: Table<PromptAction>,
    pub tree: Table<TreeAction>,
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

    pub fn hint_for(&self, binding: &Binding, prefix: &Key) -> Option<String> {
        self.root.key_for(binding).map(Key::label).or_else(|| {
            self.prefix
                .key_for(binding)
                .map(|key| format!("{} {}", prefix.label(), key.label()))
        })
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            root: Table::default(),
            prefix: prefix_table(),
            prompt: prompt_table(),
            tree: tree_table(),
            custom: BTreeMap::new(),
        }
    }
}

fn prefix_table() -> Table<Binding> {
    let windows = ('0'..='9')
        .zip(0..)
        .map(|(digit, index)| (Key::char(digit), Binding::SelectWindow(index)));
    [
        (Key::char('d'), Binding::Detach),
        (Key::char(','), Binding::RenameWindow),
        (Key::char('$'), Binding::RenameSession),
        (Key::char('s'), Binding::ClusterTree),
        (Key::char('c'), Binding::NewWindow),
        (Key::char('n'), Binding::NextWindow),
        (Key::char('p'), Binding::PreviousWindow),
        (Key::char('%'), Binding::SplitPane(Split::LeftRight)),
        (Key::char('"'), Binding::SplitPane(Split::TopBottom)),
        (Key::char('o'), Binding::NextPane),
        (Key::char('x'), Binding::KillPane),
        (Key::char('&'), Binding::KillWindow),
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
            ("s", Binding::ClusterTree),
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
            ("Up", Binding::SelectPane(Direction::Up)),
            ("Left", Binding::SelectPane(Direction::Left)),
        ] {
            assert_eq!(prefix.get(&key(notation)), Some(&binding), "{notation}");
        }
        assert_eq!(prefix.iter().count(), 26);
        assert_eq!(prefix.get(&key("C-b")), None);
        assert_eq!(Keymap::default().root, Table::default());
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
    fn unknown_input_resolves_to_nothing() {
        assert_eq!(resolved(&Keymap::default().prompt, b"\x1b[<0;3;4M"), vec![]);
    }
}
