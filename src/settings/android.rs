use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::keys::{Key, KeyCode, Mods};

pub const BASE_LAYER: &str = "base";
pub const KEY_NAMES: [&str; 33] = [
    "Escape",
    "Tab",
    "Enter",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Up",
    "Down",
    "Left",
    "Right",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "Space",
    "Ctrl",
    "Alt",
    "Shift",
    "Prefix",
    "Paste",
    "Hide",
];

const OPTION: &str = "amux.opt.android.keyboard";
const LAYER_PREFIX: &str = "layer:";
const SPACE: &str = "Space";
const ACTIONS: [&str; 6] = ["Ctrl", "Alt", "Shift", "Prefix", "Paste", "Hide"];
const DEFAULT_PERCENT: f64 = 25.0;
const MIN_PERCENT: f64 = 5.0;
const MAX_PERCENT: f64 = 45.0;
const KEY_EXPECTED: &str =
    "a key name such as \"Escape\", a character, \"layer:<name>\", \"\", false or a key table";
const ROW_EXPECTED: &str = "a list of keys, or false for the base layer's row";

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AndroidSettings {
    pub keyboard: KeyboardSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardSettings {
    pub width: KeyboardWidth,
    pub layers: BTreeMap<String, KeyboardLayer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardWidth {
    pub left: f64,
    pub right: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyboardLayer {
    pub left: Vec<KeyRow>,
    pub right: Vec<KeyRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeyRow {
    Base,
    Keys(Vec<KeySlot>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeySlot {
    Base,
    Named(String),
    Table(KeyTable),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyTable {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shift: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeats: Option<bool>,
}

impl KeyboardSettings {
    pub fn validate(&self) -> Result<(), String> {
        for (side, percent) in [("left", self.width.left), ("right", self.width.right)] {
            if !(MIN_PERCENT..=MAX_PERCENT).contains(&percent) {
                return Err(format!(
                    "{OPTION}.width.{side} must be between {MIN_PERCENT} and {MAX_PERCENT} \
                     percent of the screen, not {percent}"
                ));
            }
        }
        let base = self
            .layers
            .get(BASE_LAYER)
            .ok_or_else(|| format!("{OPTION}.layers needs a {BASE_LAYER} layer"))?;
        for (name, layer) in &self.layers {
            let base = (name != BASE_LAYER).then_some(base);
            for (side, rows) in layer.sides() {
                let base_rows = base.map(|base| base.side(side));
                self.check_rows(&format!("{OPTION}.layers.{name}.{side}"), rows, base_rows)?;
            }
        }
        Ok(())
    }

    pub fn resolved(&self) -> Self {
        let Some(base) = self.layers.get(BASE_LAYER) else {
            return self.clone();
        };
        let layers = self
            .layers
            .iter()
            .map(|(name, layer)| {
                let resolve = |side: &str| resolve_rows(layer.side(side), base.side(side));
                let layer = KeyboardLayer {
                    left: resolve("left"),
                    right: resolve("right"),
                };
                (name.clone(), layer)
            })
            .collect();
        Self {
            width: self.width,
            layers,
        }
    }

    fn check_rows(
        &self,
        path: &str,
        rows: &[KeyRow],
        base: Option<&[KeyRow]>,
    ) -> Result<(), String> {
        if rows.is_empty() {
            return Err(format!("{path} needs at least one row"));
        }
        for (index, row) in rows.iter().enumerate() {
            let row_path = format!("{path}[{}]", index + 1);
            match row {
                KeyRow::Base => match base {
                    None => return Err(falls_through_in_base(&row_path)),
                    Some(base) if base.get(index).is_none() => {
                        return Err(format!(
                            "{row_path} is false, but the {BASE_LAYER} layer has no row there"
                        ));
                    }
                    Some(_) => {}
                },
                KeyRow::Keys(keys) if keys.is_empty() => {
                    return Err(format!("{row_path} needs at least one key"));
                }
                KeyRow::Keys(keys) => {
                    for (column, key) in keys.iter().enumerate() {
                        self.check_key(&format!("{row_path}[{}]", column + 1), key, base)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn check_key(&self, path: &str, key: &KeySlot, base: Option<&[KeyRow]>) -> Result<(), String> {
        if key == &KeySlot::Base && base.is_none() {
            return Err(falls_through_in_base(path));
        }
        match key.layer() {
            Some(layer) if !self.layers.contains_key(layer) => {
                Err(format!("{path} uses the unknown layer {layer:?}"))
            }
            _ => Ok(()),
        }
    }
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            width: KeyboardWidth::default(),
            layers: default_layers(),
        }
    }
}

impl Default for KeyboardWidth {
    fn default() -> Self {
        Self {
            left: DEFAULT_PERCENT,
            right: DEFAULT_PERCENT,
        }
    }
}

impl KeyboardLayer {
    fn sides(&self) -> [(&'static str, &[KeyRow]); 2] {
        [("left", &self.left), ("right", &self.right)]
    }

    fn side(&self, side: &str) -> &[KeyRow] {
        if side == "left" {
            &self.left
        } else {
            &self.right
        }
    }
}

impl KeySlot {
    fn layer(&self) -> Option<&str> {
        let name = match self {
            Self::Named(name) => name,
            Self::Table(KeyTable {
                key: Some(name), ..
            }) => name,
            _ => return None,
        };
        name.strip_prefix(LAYER_PREFIX)
    }
}

impl KeyTable {
    fn checked(mut self) -> Result<Self, String> {
        let actions = [&self.key, &self.text, &self.send];
        if actions.iter().filter(|action| action.is_some()).count() != 1 {
            return Err("a key table needs exactly one of key, text and send".into());
        }
        self.key = self.key.as_deref().map(canonical_name).transpose()?;
        if self.text.as_deref() == Some("") {
            return Err("text must not be empty".into());
        }
        if self.shift.is_some() && !self.types_text() {
            return Err("shift only applies to keys that type text".into());
        }
        if self
            .width
            .is_some_and(|width| width.is_nan() || width <= 0.0)
        {
            return Err("width must be more than 0".into());
        }
        Ok(self)
    }

    fn types_text(&self) -> bool {
        self.text.is_some()
            || self
                .key
                .as_deref()
                .is_some_and(|name| name.chars().count() == 1 || name == SPACE)
    }
}

fn falls_through_in_base(path: &str) -> String {
    format!(
        "{path} is false, which falls through to the {BASE_LAYER} layer, \
         so the {BASE_LAYER} layer can't use it"
    )
}

fn canonical_name(name: &str) -> Result<String, String> {
    let layer = name
        .strip_prefix(LAYER_PREFIX)
        .is_some_and(|layer| !layer.is_empty());
    if name.is_empty() || name.chars().count() == 1 || layer {
        return Ok(name.to_owned());
    }
    if let Some(action) = ACTIONS
        .iter()
        .find(|action| action.eq_ignore_ascii_case(name))
    {
        return Ok((*action).to_owned());
    }
    match name.parse::<Key>() {
        Ok(key) if key.mods == Mods::NONE && sendable(key.code) => Ok(key.to_string()),
        _ => Err(format!(
            "unknown key {name:?}, expected a character, \"\", \"layer:<name>\" or one of {}",
            KEY_NAMES.join(", ")
        )),
    }
}

fn sendable(code: KeyCode) -> bool {
    match code {
        KeyCode::Char(character) => character == ' ',
        KeyCode::BackTab => false,
        _ => true,
    }
}

fn resolve_rows(rows: &[KeyRow], base: &[KeyRow]) -> Vec<KeyRow> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let base_row = base.get(index);
            let keys = match row {
                KeyRow::Base => match base_row {
                    Some(KeyRow::Keys(keys)) => keys.clone(),
                    _ => Vec::new(),
                },
                KeyRow::Keys(keys) => keys
                    .iter()
                    .enumerate()
                    .map(|(column, key)| match key {
                        KeySlot::Base => match base_row {
                            Some(KeyRow::Keys(base_keys)) => base_keys
                                .get(column)
                                .cloned()
                                .unwrap_or_else(|| KeySlot::Named(String::new())),
                            _ => KeySlot::Named(String::new()),
                        },
                        key => key.clone(),
                    })
                    .collect(),
            };
            KeyRow::Keys(keys)
        })
        .collect()
}

impl Serialize for KeyRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Base => serializer.serialize_bool(false),
            Self::Keys(keys) => keys.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for KeyRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(KeyRowVisitor)
    }
}

struct KeyRowVisitor;

impl<'de> Visitor<'de> for KeyRowVisitor {
    type Value = KeyRow;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(ROW_EXPECTED)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<KeyRow, E> {
        if value {
            return Err(E::invalid_value(Unexpected::Bool(true), &self));
        }
        Ok(KeyRow::Base)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<KeyRow, A::Error> {
        let mut keys = Vec::new();
        while let Some(key) = seq.next_element()? {
            keys.push(key);
        }
        Ok(KeyRow::Keys(keys))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KeyRow, A::Error> {
        if map.next_key::<IgnoredAny>()?.is_some() {
            return Err(de::Error::invalid_type(Unexpected::Map, &self));
        }
        Ok(KeyRow::Keys(Vec::new()))
    }
}

impl Serialize for KeySlot {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Base => serializer.serialize_bool(false),
            Self::Named(name) => serializer.serialize_str(name),
            Self::Table(table) => table.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for KeySlot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(KeySlotVisitor)
    }
}

struct KeySlotVisitor;

impl<'de> Visitor<'de> for KeySlotVisitor {
    type Value = KeySlot;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(KEY_EXPECTED)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<KeySlot, E> {
        if value {
            return Err(E::invalid_value(Unexpected::Bool(true), &self));
        }
        Ok(KeySlot::Base)
    }

    fn visit_str<E: de::Error>(self, name: &str) -> Result<KeySlot, E> {
        canonical_name(name).map(KeySlot::Named).map_err(E::custom)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<KeySlot, A::Error> {
        KeyTable::deserialize(de::value::MapAccessDeserializer::new(map))?
            .checked()
            .map(KeySlot::Table)
            .map_err(de::Error::custom)
    }
}

fn keys(names: &[&str]) -> Vec<KeySlot> {
    names
        .iter()
        .map(|name| KeySlot::Named((*name).to_owned()))
        .collect()
}

fn row(names: &[&str]) -> KeyRow {
    KeyRow::Keys(keys(names))
}

fn row_after_base(names: &[&str]) -> KeyRow {
    KeyRow::Keys([KeySlot::Base].into_iter().chain(keys(names)).collect())
}

fn row_before_base(names: &[&str]) -> KeyRow {
    KeyRow::Keys(keys(names).into_iter().chain([KeySlot::Base]).collect())
}

fn wide_space() -> KeySlot {
    KeySlot::Table(KeyTable {
        key: Some(SPACE.to_owned()),
        width: Some(2.0),
        ..KeyTable::default()
    })
}

fn default_layers() -> BTreeMap<String, KeyboardLayer> {
    let base = KeyboardLayer {
        left: vec![
            row(&["Escape", "1", "2", "3", "4", "5"]),
            row(&["Tab", "q", "w", "e", "r", "t"]),
            row(&["Ctrl", "a", "s", "d", "f", "g"]),
            row(&["Shift", "z", "x", "c", "v", "b"]),
            KeyRow::Keys(
                keys(&["Prefix", "Alt", "layer:nav", "layer:sym"])
                    .into_iter()
                    .chain([wide_space()])
                    .collect(),
            ),
        ],
        right: vec![
            row(&["6", "7", "8", "9", "0", "Backspace"]),
            row(&["y", "u", "i", "o", "p", "'"]),
            row(&["h", "j", "k", "l", ";", "Enter"]),
            row(&["n", "m", ",", ".", "/", "Shift"]),
            KeyRow::Keys(
                [wide_space()]
                    .into_iter()
                    .chain(keys(&["layer:sym", "layer:nav", "Ctrl", "Alt"]))
                    .collect(),
            ),
        ],
    };
    let sym = KeyboardLayer {
        left: vec![
            row_after_base(&["!", "@", "#", "$", "%"]),
            row_after_base(&["`", "~", "\\", "|", "&"]),
            row_after_base(&["-", "_", "=", "+", "*"]),
            row_after_base(&["<", ">", "(", ")", "\""]),
            KeyRow::Base,
        ],
        right: vec![
            row_before_base(&["^", "&", "*", "(", ")"]),
            row_before_base(&["[", "]", "{", "}", "|"]),
            row_before_base(&["-", "=", "_", "+", ":"]),
            row_before_base(&["<", ">", "~", "`", "?"]),
            KeyRow::Base,
        ],
    };
    let nav = KeyboardLayer {
        left: vec![
            row_after_base(&["F1", "F2", "F3", "F4", "F5"]),
            row_after_base(&["F6", "F7", "F8", "F9", "F10"]),
            row_after_base(&["F11", "F12", "Insert", "Delete", "Paste"]),
            row_after_base(&["", "", "", "", "Hide"]),
            KeyRow::Base,
        ],
        right: vec![
            row_before_base(&["", "", "", "", ""]),
            row_before_base(&["Home", "PageDown", "PageUp", "End", ""]),
            row_before_base(&["Left", "Down", "Up", "Right", "Delete"]),
            row_before_base(&["Paste", "Hide", "", "", ""]),
            KeyRow::Base,
        ],
    };
    BTreeMap::from([
        (BASE_LAYER.to_owned(), base),
        ("sym".to_owned(), sym),
        ("nav".to_owned(), nav),
    ])
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::lua::{self, ConfigPaths, Process, INIT_FILE};
    use crate::settings::Settings;

    fn load(source: &str) -> Result<KeyboardSettings, String> {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join(INIT_FILE);
        fs::write(&init, source).unwrap();
        let location = format!("{}: ", init.display());
        let paths = ConfigPaths::new(dir.path().to_owned(), Some(init));
        lua::load(&paths, Process::Client)
            .map(|loaded| loaded.settings.android.keyboard)
            .map_err(|error| format!("{error:#}").replace(&location, ""))
    }

    fn named(name: &str) -> KeySlot {
        KeySlot::Named(name.into())
    }

    fn keys_of(row: &KeyRow) -> &[KeySlot] {
        match row {
            KeyRow::Keys(keys) => keys,
            KeyRow::Base => panic!("the row falls through"),
        }
    }

    #[test]
    fn the_defaults_are_a_valid_split_qwerty_with_sym_and_nav_layers() {
        let keyboard = KeyboardSettings::default();
        assert_eq!(keyboard.validate(), Ok(()));
        assert_eq!(
            keyboard.width,
            KeyboardWidth {
                left: 25.0,
                right: 25.0
            }
        );
        assert_eq!(
            keyboard.layers.keys().collect::<Vec<_>>(),
            ["base", "nav", "sym"]
        );
        let base = &keyboard.layers["base"];
        assert_eq!(keys_of(&base.left[1])[1], named("q"));
        assert_eq!(keys_of(&base.right[0])[5], named("Backspace"));
        assert_eq!(keyboard.layers["sym"].left[4], KeyRow::Base);
    }

    #[test]
    fn a_config_sets_widths_and_one_half_of_a_layer() {
        let keyboard = load(
            "amux.opt.android.keyboard.width.left = 30\n\
             amux.opt.android.keyboard.layers.nav.right = {\n\
               { { send = '\\2c', label = 'new' }, false },\n\
               false,\n\
             }\n",
        )
        .unwrap();
        assert_eq!(
            keyboard.width,
            KeyboardWidth {
                left: 30.0,
                right: 25.0
            }
        );
        let nav = &keyboard.layers["nav"];
        assert_eq!(
            nav.right,
            [
                KeyRow::Keys(vec![
                    KeySlot::Table(KeyTable {
                        send: Some("\u{2}c".into()),
                        label: Some("new".into()),
                        ..KeyTable::default()
                    }),
                    KeySlot::Base,
                ]),
                KeyRow::Base,
            ]
        );
        assert_eq!(nav.left, KeyboardSettings::default().layers["nav"].left);
    }

    #[test]
    fn a_config_adds_and_removes_layers() {
        let keyboard = load(
            "local keyboard = amux.opt.android.keyboard\n\
             keyboard.layers.sym = nil\n\
             keyboard.layers.base.left[5][4] = 'layer:git'\n\
             keyboard.layers.base.right[5][2] = 'layer:git'\n\
             keyboard.layers.git = {\n\
               left = { false, { { text = 'git status\\r', label = 'st' } } },\n\
               right = { { 'x' } },\n\
             }\n",
        )
        .unwrap();
        assert_eq!(
            keyboard.layers.keys().collect::<Vec<_>>(),
            ["base", "git", "nav"]
        );
        let resolved = keyboard.resolved();
        let git = &resolved.layers["git"];
        assert_eq!(git.left[0], resolved.layers["base"].left[0]);
        assert_eq!(
            keys_of(&git.left[1]),
            [KeySlot::Table(KeyTable {
                text: Some("git status\r".into()),
                label: Some("st".into()),
                ..KeyTable::default()
            })]
        );
    }

    #[test]
    fn unknown_keys_are_errors_that_count_from_one() {
        let error = load("amux.opt.android.keyboard.layers.nav.right[2][3] = 'Escap'").unwrap_err();
        assert!(
            error.starts_with(
                "amux.opt.android.keyboard.layers.nav.right[2][3]: unknown key \"Escap\", \
                 expected a character, \"\", \"layer:<name>\" or one of Escape, Tab,"
            ),
            "{error}"
        );
    }

    #[test]
    fn keys_are_named_as_amux_keymap_names_them() {
        for name in KEY_NAMES {
            assert_eq!(canonical_name(name).as_deref(), Ok(name));
        }
        let spellings = [
            ("escape", "Escape"),
            ("Esc", "Escape"),
            ("BSpace", "Backspace"),
            ("DC", "Delete"),
            ("IC", "Insert"),
            ("PgUp", "PageUp"),
            ("NPage", "PageDown"),
            ("space", "Space"),
            ("f5", "F5"),
            ("ctrl", "Ctrl"),
            ("HIDE", "Hide"),
            ("layer:nav", "layer:nav"),
            ("q", "q"),
            ("", ""),
        ];
        for (written, canonical) in spellings {
            assert_eq!(
                canonical_name(written).as_deref(),
                Ok(canonical),
                "{written}"
            );
        }
        for rejected in [
            "C-c", "M-Left", "S-Tab", "BackTab", "F13", "layer:", "Bogus",
        ] {
            assert!(canonical_name(rejected).is_err(), "{rejected}");
        }

        let keyboard = load(
            "amux.opt.android.keyboard.layers.nav.right[1][1] = 'pgup'\n\
             amux.opt.android.keyboard.layers.nav.right[1][2] = { key = 'bspace', repeats = false }",
        )
        .unwrap();
        let resolved = keyboard.resolved();
        let row = keys_of(&resolved.layers["nav"].right[0]);
        assert_eq!(row[0], named("PageUp"));
        assert_eq!(
            row[1],
            KeySlot::Table(KeyTable {
                key: Some("Backspace".into()),
                repeats: Some(false),
                ..KeyTable::default()
            })
        );
    }

    #[test]
    fn key_tables_are_checked() {
        let failure = |key: &str| {
            load(&format!(
                "amux.opt.android.keyboard.layers.base.left[1][1] = {key}"
            ))
            .unwrap_err()
        };
        let at = "amux.opt.android.keyboard.layers.base.left[1][1]";
        assert_eq!(
            failure("{ key = 'a', text = 'b' }"),
            format!("{at}: a key table needs exactly one of key, text and send")
        );
        assert_eq!(
            failure("{ key = 'esc', shift = 'x' }"),
            format!("{at}: shift only applies to keys that type text")
        );
        assert_eq!(
            failure("{ key = 'a', width = 0 }"),
            format!("{at}: width must be more than 0")
        );
        assert_eq!(
            failure("{ text = '' }"),
            format!("{at}: text must not be empty")
        );
        let unknown = failure("{ key = 'a', colour = 'red' }");
        assert!(unknown.contains("unknown field `colour`"), "{unknown}");
        let truth = failure("true");
        assert!(
            truth.contains("expected a key name such as \"Escape\""),
            "{truth}"
        );
        let number = failure("7");
        assert!(number.contains("integer `7`"), "{number}");
    }

    #[test]
    fn rows_are_lists_of_keys_or_false() {
        let error =
            load("amux.opt.android.keyboard.layers.base.left[1] = { a = 'b' }").unwrap_err();
        assert!(
            error.starts_with("amux.opt.android.keyboard.layers.base.left[1]: invalid type: map"),
            "{error}"
        );
    }

    #[test]
    fn layouts_that_cannot_work_are_rejected() {
        let rejected = |change: fn(&mut KeyboardSettings)| {
            let mut keyboard = KeyboardSettings::default();
            change(&mut keyboard);
            keyboard.validate().unwrap_err()
        };
        let layers = "amux.opt.android.keyboard.layers";
        assert_eq!(
            rejected(|keyboard| keyboard.width.left = 60.0),
            "amux.opt.android.keyboard.width.left must be between 5 and 45 percent of the \
             screen, not 60"
        );
        assert_eq!(
            rejected(|keyboard| keyboard.width.right = 2.5),
            "amux.opt.android.keyboard.width.right must be between 5 and 45 percent of the \
             screen, not 2.5"
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.remove(BASE_LAYER);
            }),
            format!("{layers} needs a base layer")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.get_mut(BASE_LAYER).unwrap().left[0] = KeyRow::Base;
            }),
            format!(
                "{layers}.base.left[1] is false, which falls through to the base layer, so the \
                 base layer can't use it"
            )
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.get_mut(BASE_LAYER).unwrap().right[2] =
                    KeyRow::Keys(vec![named("a"), KeySlot::Base]);
            }),
            format!(
                "{layers}.base.right[3][2] is false, which falls through to the base layer, so \
                 the base layer can't use it"
            )
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard
                    .layers
                    .get_mut("sym")
                    .unwrap()
                    .left
                    .push(KeyRow::Base);
            }),
            format!("{layers}.sym.left[6] is false, but the base layer has no row there")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.remove("sym");
            }),
            format!("{layers}.base.left[5][4] uses the unknown layer \"sym\"")
        );
        assert_eq!(
            rejected(|keyboard| keyboard.layers.get_mut("nav").unwrap().right.clear()),
            format!("{layers}.nav.right needs at least one row")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.get_mut("nav").unwrap().right[0] = KeyRow::Keys(Vec::new());
            }),
            format!("{layers}.nav.right[1] needs at least one key")
        );
    }

    #[test]
    fn a_broken_keyboard_stops_the_config_like_any_other_option() {
        let error = load("amux.opt.android.keyboard.width.right = 50").unwrap_err();
        assert_eq!(
            error,
            "amux.opt.android.keyboard.width.right must be between 5 and 45 percent of the \
             screen, not 50"
        );
        assert_eq!(Settings::default().validate(), Ok(()));
    }

    fn fixture_is_current(name: &str, contents: &str) {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(ANDROID_FIXTURES);
        let path = dir.join(name);
        if std::env::var_os(UPDATE_FIXTURES).is_some() {
            fs::create_dir_all(&dir).unwrap();
            fs::write(&path, contents).unwrap();
        }
        assert_eq!(
            fs::read_to_string(&path).unwrap_or_default(),
            contents,
            "{} is out of date, run the tests again with {UPDATE_FIXTURES}=1",
            path.display()
        );
    }

    const ANDROID_FIXTURES: &str = "android/app/src/test/resources/keyboard";
    const UPDATE_FIXTURES: &str = "AMUX_UPDATE_FIXTURES";

    #[test]
    fn the_android_tests_read_the_current_defaults_and_key_names() {
        let defaults = serde_json::to_string_pretty(&KeyboardSettings::default().resolved());
        fixture_is_current("default.json", &format!("{}\n", defaults.unwrap()));
        fixture_is_current("key-names.txt", &format!("{}\n", KEY_NAMES.join("\n")));
    }

    #[test]
    fn resolving_replaces_false_with_the_base_layers_keys() {
        let resolved = KeyboardSettings::default().resolved();
        let base = &resolved.layers[BASE_LAYER];
        let sym = &resolved.layers["sym"];
        assert_eq!(keys_of(&sym.left[0])[0], named("Escape"));
        assert_eq!(keys_of(&sym.left[0])[1], named("!"));
        assert_eq!(keys_of(&sym.right[1])[5], named("'"));
        assert_eq!(sym.left[4], base.left[4]);
        let serialized = serde_json::to_string(&resolved).unwrap();
        assert!(!serialized.contains("false"), "{serialized}");

        let mut longer = KeyboardSettings::default();
        longer.layers.get_mut("sym").unwrap().left[0] =
            KeyRow::Keys(keys(&["a"; 6]).into_iter().chain([KeySlot::Base]).collect());
        let resolved = longer.resolved();
        assert_eq!(keys_of(&resolved.layers["sym"].left[0])[6], named(""));
    }
}
