use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::keys::{parse_sequence, Key, KeyCode};

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
const PREFIX: &str = "Prefix";
const MODIFIERS: [&str; 3] = ["Ctrl", "Alt", "Shift"];
const ACTIONS: [&str; 6] = ["Ctrl", "Alt", "Shift", "Prefix", "Paste", "Hide"];
const US_KEYS: &str = "`1234567890-=[]\\;',./";
const US_SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";
const DEFAULT_PERCENT: f64 = 21.0;
const MIN_PERCENT: f64 = 5.0;
const MAX_PERCENT: f64 = 45.0;
const DEFAULT_HOLD_MS: u64 = 300;
const DEFAULT_TAPS_MS: u64 = 250;
const MIN_MS: u64 = 50;
const MAX_MS: u64 = 2000;
const KEY_EXPECTED: &str =
    "a key name such as \"Escape\", a character, \"layer:<name>\", \"\", false or a key table";
const HOLD_EXPECTED: &str = "a key, or a list of keys to pick from";
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
    pub hold_ms: u64,
    pub taps_ms: u64,
    pub layers: BTreeMap<String, KeyboardLayer>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardWidth {
    pub left: f64,
    pub right: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyboardLayer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Vec<KeyRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<Vec<KeyRow>>,
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
    pub prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shift: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeats: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hold: Option<Hold>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taps: Option<Vec<KeySlot>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Hold {
    Key(Box<KeySlot>),
    Choices(Vec<KeySlot>),
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
        for (name, ms) in [("hold_ms", self.hold_ms), ("taps_ms", self.taps_ms)] {
            if !(MIN_MS..=MAX_MS).contains(&ms) {
                return Err(format!(
                    "{OPTION}.{name} must be between {MIN_MS} and {MAX_MS} milliseconds, not {ms}"
                ));
            }
        }
        let base = self
            .layers
            .get(BASE_LAYER)
            .ok_or_else(|| format!("{OPTION}.layers needs a {BASE_LAYER} layer"))?;
        if base.left.is_none() || base.right.is_none() {
            return Err(format!(
                "{OPTION}.layers.{BASE_LAYER} needs both left and right"
            ));
        }
        for (name, layer) in &self.layers {
            if layer.left.is_none() && layer.right.is_none() {
                return Err(format!("{OPTION}.layers.{name} needs left, right or both"));
            }
            let base = (name != BASE_LAYER).then_some(base);
            for (side, rows) in layer.sides() {
                let base_rows = base.and_then(|base| base.side(side));
                self.check_rows(&format!("{OPTION}.layers.{name}.{side}"), rows, base_rows)?;
            }
        }
        Ok(())
    }

    pub fn resolved(&self, prefix: Key) -> Self {
        let prefix = encoded(&[prefix]);
        let base = self.layers.get(BASE_LAYER);
        let layers = self
            .layers
            .iter()
            .map(|(name, layer)| {
                let resolve = |side: &str| {
                    layer.side(side).map(|rows| {
                        let base_rows = base.and_then(|base| base.side(side)).unwrap_or_default();
                        resolve_rows(rows, base_rows, &prefix)
                    })
                };
                let layer = KeyboardLayer {
                    left: resolve("left"),
                    right: resolve("right"),
                };
                (name.clone(), layer)
            })
            .collect();
        Self {
            width: self.width,
            hold_ms: self.hold_ms,
            taps_ms: self.taps_ms,
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
        match key.layers().find(|layer| !self.layers.contains_key(*layer)) {
            Some(layer) => Err(format!("{path} uses the unknown layer {layer:?}")),
            None => Ok(()),
        }
    }
}

impl Default for KeyboardSettings {
    fn default() -> Self {
        Self {
            width: KeyboardWidth::default(),
            hold_ms: DEFAULT_HOLD_MS,
            taps_ms: DEFAULT_TAPS_MS,
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
    fn sides(&self) -> impl Iterator<Item = (&'static str, &[KeyRow])> {
        [("left", &self.left), ("right", &self.right)]
            .into_iter()
            .filter_map(|(side, rows)| rows.as_deref().map(|rows| (side, rows)))
    }

    fn side(&self, side: &str) -> Option<&[KeyRow]> {
        if side == "left" {
            self.left.as_deref()
        } else {
            self.right.as_deref()
        }
    }
}

impl KeySlot {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Named(name) => Some(name),
            Self::Table(KeyTable {
                key: Some(name), ..
            }) => Some(name),
            _ => None,
        }
    }

    fn layers(&self) -> impl Iterator<Item = &str> {
        let held = match self {
            Self::Table(KeyTable {
                hold: Some(Hold::Key(held)),
                ..
            }) => held.name(),
            _ => None,
        };
        self.name()
            .into_iter()
            .chain(held)
            .filter_map(|name| name.strip_prefix(LAYER_PREFIX))
    }

    fn latches(&self) -> bool {
        self.name().is_some_and(latches)
    }
}

impl KeyTable {
    fn checked(mut self) -> Result<Self, String> {
        let actions = [&self.key, &self.text, &self.send, &self.prefix];
        if actions.iter().filter(|action| action.is_some()).count() != 1 {
            return Err("a key table needs exactly one of key, text, send and prefix".into());
        }
        self.key = self.key.as_deref().map(canonical_name).transpose()?;
        if self.text.as_deref() == Some("") {
            return Err("text must not be empty".into());
        }
        if let Some(keys) = &self.prefix {
            if parse_sequence(keys).is_err() {
                return Err(format!(
                    "prefix is {keys:?}, which isn't a key or keys such as \"c\" or \"s p\""
                ));
            }
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
        let extra = self.hold.is_some() || self.taps.is_some();
        if extra && self.key.as_deref().is_some_and(latches) {
            return Err(
                "Ctrl, Alt, Shift and layer keys can't have hold or taps, since they \
                        already stay on while held and lock on a second tap"
                    .into(),
            );
        }
        if self.hold.is_some() && self.repeats == Some(true) {
            return Err("a key can't both repeat and have a hold".into());
        }
        match &self.hold {
            Some(Hold::Key(key)) => check_extra("hold", key, true)?,
            Some(Hold::Choices(keys)) if keys.is_empty() => {
                return Err("hold needs at least one key".into());
            }
            Some(Hold::Choices(keys)) => {
                for key in keys {
                    check_extra("a hold list", key, false)?;
                }
            }
            None => {}
        }
        if let Some(taps) = &self.taps {
            if taps.is_empty() {
                return Err("taps needs at least one key".into());
            }
            for key in taps {
                check_extra("taps", key, false)?;
            }
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

    fn resolved(mut self, prefix: &str) -> Self {
        if self.key.as_deref() == Some(PREFIX) {
            self.key = None;
            self.send = Some(prefix.to_owned());
            self.label = self.label.or_else(|| Some(PREFIX.to_owned()));
        }
        if let Some(keys) = self.prefix.take() {
            let sequence = parse_sequence(&keys).unwrap_or_default();
            self.send = Some(format!("{prefix}{}", encoded(&sequence)));
            self.label = self.label.or(Some(keys));
        }
        self.hold = self.hold.map(|hold| match hold {
            Hold::Key(key) => Hold::Key(Box::new(resolve_slot(*key, prefix))),
            Hold::Choices(keys) => Hold::Choices(resolve_slots(keys, prefix)),
        });
        self.taps = self.taps.map(|keys| resolve_slots(keys, prefix));
        self
    }
}

fn check_extra(context: &str, key: &KeySlot, may_latch: bool) -> Result<(), String> {
    match key {
        KeySlot::Base => Err(format!("{context} can't use false")),
        KeySlot::Named(name) if name.is_empty() => Err(format!("{context} can't use an empty key")),
        KeySlot::Table(table)
            if table.hold.is_some()
                || table.taps.is_some()
                || table.width.is_some()
                || table.repeats.is_some() =>
        {
            Err(format!(
                "a key in {context} can't set hold, taps, width or repeats"
            ))
        }
        key if key.latches() && !may_latch => Err(format!(
            "{context} can't use Ctrl, Alt, Shift or layer keys"
        )),
        _ => Ok(()),
    }
}

fn latches(name: &str) -> bool {
    MODIFIERS.contains(&name) || name.starts_with(LAYER_PREFIX)
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
        Ok(key) if key.code != KeyCode::BackTab => Ok(key.to_string()),
        _ => Err(format!(
            "unknown key {name:?}, expected a character, \"\", \"layer:<name>\" or one of {}, \
             with C-, M- or S- in front for Ctrl, Alt or Shift",
            KEY_NAMES.join(", ")
        )),
    }
}

fn us_shifted(character: char) -> Option<char> {
    US_KEYS
        .chars()
        .position(|key| key == character)
        .and_then(|index| US_SHIFTED.chars().nth(index))
}

fn encoded(keys: &[Key]) -> String {
    let bytes: Vec<u8> = keys
        .iter()
        .filter_map(|key| key.encodings().into_iter().next())
        .flatten()
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn resolve_rows(rows: &[KeyRow], base: &[KeyRow], prefix: &str) -> Vec<KeyRow> {
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
            KeyRow::Keys(resolve_slots(keys, prefix))
        })
        .collect()
}

fn resolve_slots(keys: Vec<KeySlot>, prefix: &str) -> Vec<KeySlot> {
    keys.into_iter()
        .map(|key| resolve_slot(key, prefix))
        .collect()
}

fn resolve_slot(key: KeySlot, prefix: &str) -> KeySlot {
    match key {
        KeySlot::Named(name) if name == PREFIX => KeySlot::Table(
            KeyTable {
                key: Some(name),
                ..KeyTable::default()
            }
            .resolved(prefix),
        ),
        KeySlot::Table(table) => KeySlot::Table(table.resolved(prefix)),
        key => key,
    }
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

impl Serialize for Hold {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Key(key) => key.serialize(serializer),
            Self::Choices(keys) => keys.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Hold {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(HoldVisitor)
    }
}

struct HoldVisitor;

impl<'de> Visitor<'de> for HoldVisitor {
    type Value = Hold;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(HOLD_EXPECTED)
    }

    fn visit_str<E: de::Error>(self, name: &str) -> Result<Hold, E> {
        KeySlotVisitor
            .visit_str(name)
            .map(|key| Hold::Key(Box::new(key)))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Hold, A::Error> {
        KeySlotVisitor
            .visit_map(map)
            .map(|key| Hold::Key(Box::new(key)))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Hold, A::Error> {
        let mut keys = Vec::new();
        while let Some(key) = seq.next_element()? {
            keys.push(key);
        }
        Ok(Hold::Choices(keys))
    }
}

fn named(name: &str) -> KeySlot {
    KeySlot::Named(name.to_owned())
}

fn blank() -> KeySlot {
    named("")
}

fn keys(names: &[&str]) -> Vec<KeySlot> {
    names.iter().map(|name| named(name)).collect()
}

fn row(names: &[&str]) -> KeyRow {
    KeyRow::Keys(keys(names))
}

fn letter(character: char) -> KeySlot {
    let upper = character.to_ascii_uppercase().to_string();
    let ctrl = format!("C-{character}");
    let alt = format!("M-{character}");
    let choices = if matches!(character, 'i' | 'm') {
        vec![upper, alt]
    } else {
        vec![upper, ctrl, alt]
    };
    KeySlot::Table(KeyTable {
        key: Some(character.to_string()),
        hold: Some(Hold::Choices(
            choices.iter().map(|choice| named(choice)).collect(),
        )),
        ..KeyTable::default()
    })
}

fn letters(characters: &str) -> Vec<KeySlot> {
    characters.chars().map(letter).collect()
}

fn unshifted(character: char) -> KeySlot {
    if us_shifted(character).is_none() {
        return named(&character.to_string());
    }
    KeySlot::Table(KeyTable {
        key: Some(character.to_string()),
        shift: Some(character.to_string()),
        ..KeyTable::default()
    })
}

fn symbols(characters: &str) -> KeyRow {
    KeyRow::Keys(characters.chars().map(unshifted).collect())
}

fn digit(number: u8) -> KeySlot {
    let function = if number == 0 { 10 } else { number };
    KeySlot::Table(KeyTable {
        key: Some(number.to_string()),
        shift: Some(number.to_string()),
        hold: Some(Hold::Key(Box::new(named(&format!("F{function}"))))),
        ..KeyTable::default()
    })
}

fn prefixed(keys: &str, label: &str) -> KeySlot {
    KeySlot::Table(KeyTable {
        prefix: Some(keys.to_owned()),
        label: Some(label.to_owned()),
        ..KeyTable::default()
    })
}

fn wide_space() -> KeySlot {
    KeySlot::Table(KeyTable {
        key: Some(SPACE.to_owned()),
        width: Some(2.0),
        ..KeyTable::default()
    })
}

fn amux_key() -> KeySlot {
    KeySlot::Table(KeyTable {
        key: Some(PREFIX.to_owned()),
        hold: Some(Hold::Key(Box::new(named("layer:amux")))),
        ..KeyTable::default()
    })
}

fn default_layers() -> BTreeMap<String, KeyboardLayer> {
    let base = KeyboardLayer {
        left: Some(vec![
            row(&["Escape", "Tab", "'", "-", ""]),
            KeyRow::Keys(letters("qwert")),
            KeyRow::Keys(letters("asdfg")),
            KeyRow::Keys(letters("zxcvb")),
            KeyRow::Keys(vec![named("Shift"), named("layer:nav"), amux_key()]),
        ]),
        right: Some(vec![
            row(&["", "Ctrl", "Alt", "Paste", "Enter"]),
            KeyRow::Keys(letters("yuiop")),
            KeyRow::Keys(letters("hjkl").into_iter().chain([named(";")]).collect()),
            KeyRow::Keys(
                letters("nm")
                    .into_iter()
                    .chain(keys(&[",", ".", "/"]))
                    .collect(),
            ),
            KeyRow::Keys(vec![
                wide_space(),
                named("Backspace"),
                named("layer:sym"),
                named("layer:num"),
            ]),
        ]),
    };
    let sym = KeyboardLayer {
        left: Some(vec![
            symbols("!@#$%"),
            symbols("^&*()"),
            symbols("[]=\\`"),
            symbols("{}+|~"),
            KeyRow::Base,
        ]),
        right: None,
    };
    let num = KeyboardLayer {
        left: Some(vec![
            KeyRow::Keys(vec![blank(), digit(1), digit(2), digit(3), named("F11")]),
            KeyRow::Keys(vec![blank(), digit(4), digit(5), digit(6), named("F12")]),
            KeyRow::Keys(vec![blank(), digit(7), digit(8), digit(9), blank()]),
            KeyRow::Keys(vec![blank(), blank(), digit(0), blank(), blank()]),
            KeyRow::Base,
        ]),
        right: None,
    };
    let nav = KeyboardLayer {
        left: None,
        right: Some(vec![
            KeyRow::Base,
            row(&["", "", "", "", ""]),
            row(&["Left", "Down", "Up", "Right", "Delete"]),
            row(&["Home", "PageDown", "PageUp", "End", "Insert"]),
            KeyRow::Base,
        ]),
    };
    let amux = KeyboardLayer {
        left: None,
        right: Some(vec![
            KeyRow::Keys(vec![
                prefixed("c", "+win"),
                prefixed("p", "‹win"),
                prefixed("n", "win›"),
                prefixed(",", "rename"),
                prefixed("&", "✕win"),
            ]),
            KeyRow::Keys(vec![
                prefixed("%", "split│"),
                prefixed("\"", "split─"),
                prefixed("o", "pane›"),
                prefixed("x", "✕pane"),
                prefixed("?", "help"),
            ]),
            KeyRow::Keys(vec![
                prefixed("s p", "projects"),
                prefixed("s w", "worktrees"),
                prefixed("s s", "tree"),
                prefixed("r", "reload"),
                prefixed("d", "detach"),
            ]),
            KeyRow::Keys(vec![
                prefixed("$", "session"),
                named("Hide"),
                blank(),
                blank(),
                blank(),
            ]),
            KeyRow::Base,
        ]),
    };
    BTreeMap::from([
        (BASE_LAYER.to_owned(), base),
        ("sym".to_owned(), sym),
        ("num".to_owned(), num),
        ("nav".to_owned(), nav),
        ("amux".to_owned(), amux),
    ])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
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

    fn keys_of(row: &KeyRow) -> &[KeySlot] {
        match row {
            KeyRow::Keys(keys) => keys,
            KeyRow::Base => panic!("the row falls through"),
        }
    }

    fn half(rows: &Option<Vec<KeyRow>>) -> &[KeyRow] {
        rows.as_deref().expect("the layer has this half")
    }

    fn table(key: &str) -> KeyTable {
        KeyTable {
            key: Some(key.into()),
            ..KeyTable::default()
        }
    }

    fn resolved_defaults() -> KeyboardSettings {
        KeyboardSettings::default().resolved(Settings::default().prefix)
    }

    #[test]
    fn the_defaults_are_a_valid_five_column_split_qwerty() {
        let keyboard = KeyboardSettings::default();
        assert_eq!(keyboard.validate(), Ok(()));
        assert_eq!(
            keyboard.width,
            KeyboardWidth {
                left: 21.0,
                right: 21.0
            }
        );
        assert_eq!((keyboard.hold_ms, keyboard.taps_ms), (300, 250));
        assert_eq!(
            keyboard.layers.keys().collect::<Vec<_>>(),
            ["amux", "base", "nav", "num", "sym"]
        );
        let base = &keyboard.layers["base"];
        for rows in [half(&base.left), half(&base.right)] {
            assert_eq!(rows.len(), 5);
            for row in &rows[..4] {
                assert_eq!(keys_of(row).len(), 5);
            }
        }
        assert_eq!(keys_of(&half(&base.left)[1])[0], letter('q'));
        assert_eq!(keys_of(&half(&base.right)[4])[1], named("Backspace"));
        for (name, changes) in [
            ("sym", "left"),
            ("num", "left"),
            ("nav", "right"),
            ("amux", "right"),
        ] {
            let layer = &keyboard.layers[name];
            let sides: Vec<_> = layer.sides().map(|(side, _)| side).collect();
            assert_eq!(sides, [changes], "{name}");
        }
    }

    #[test]
    fn letters_offer_their_capital_ctrl_and_alt_forms_on_a_hold() {
        let KeySlot::Table(f) = letter('f') else {
            panic!("a letter is a key table");
        };
        assert_eq!(f.hold, Some(Hold::Choices(keys(&["F", "C-f", "M-f"]))));
        let KeySlot::Table(i) = letter('i') else {
            panic!("a letter is a key table");
        };
        assert_eq!(i.hold, Some(Hold::Choices(keys(&["I", "M-i"]))));
    }

    #[test]
    fn the_built_in_layout_has_every_key_once_and_types_all_of_ascii() {
        let keyboard = KeyboardSettings::default();
        let mut homes: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (name, layer) in &keyboard.layers {
            for (side, rows) in layer.sides() {
                for (row, keys) in rows.iter().enumerate() {
                    let KeyRow::Keys(keys) = keys else {
                        continue;
                    };
                    for (column, key) in keys.iter().enumerate() {
                        let place = format!("{name}.{side}[{}][{}]", row + 1, column + 1);
                        for output in outputs(key) {
                            homes.entry(output).or_default().insert(place.clone());
                        }
                    }
                }
            }
        }
        let repeated: Vec<_> = homes
            .iter()
            .filter(|(_, places)| places.len() > 1)
            .collect();
        assert!(repeated.is_empty(), "{repeated:?}");

        let typed: BTreeSet<char> = homes
            .keys()
            .filter_map(|output| match output.as_str() {
                SPACE => Some(' '),
                single if single.chars().count() == 1 => single.chars().next(),
                _ => None,
            })
            .collect();
        let missing: Vec<char> = (' '..='~').filter(|c| !typed.contains(c)).collect();
        assert!(missing.is_empty(), "{missing:?}");
        for number in 1..=12 {
            assert!(homes.contains_key(&format!("F{number}")), "F{number}");
        }
    }

    fn outputs(key: &KeySlot) -> Vec<String> {
        match key {
            KeySlot::Base => Vec::new(),
            KeySlot::Named(name) if name.is_empty() => Vec::new(),
            KeySlot::Named(name) => typed(name, None),
            KeySlot::Table(table) => {
                let mut found = match (&table.key, &table.text, &table.send, &table.prefix) {
                    (Some(name), ..) => typed(name, table.shift.as_deref()),
                    (_, Some(text), ..) => vec![text.clone()],
                    (_, _, Some(send), _) => vec![format!("send {send:?}")],
                    (_, _, _, Some(keys)) => vec![format!("prefix {keys}")],
                    _ => Vec::new(),
                };
                let held = match &table.hold {
                    Some(Hold::Key(key)) => vec![key.as_ref().clone()],
                    Some(Hold::Choices(keys)) => keys.clone(),
                    None => Vec::new(),
                };
                for extra in held.iter().chain(table.taps.iter().flatten()) {
                    found.extend(outputs(extra));
                }
                found
            }
        }
    }

    fn typed(name: &str, shift: Option<&str>) -> Vec<String> {
        let mut characters = name.chars();
        let shifted = match (shift, characters.next(), characters.next()) {
            (Some(shift), ..) => shift.to_owned(),
            (None, Some(character), None) => us_shifted(character)
                .unwrap_or_else(|| character.to_ascii_uppercase())
                .to_string(),
            _ => name.to_owned(),
        };
        vec![name.to_owned(), shifted]
    }

    #[test]
    fn a_config_sets_widths_timings_and_one_half_of_a_layer() {
        let keyboard = load(
            "amux.opt.android.keyboard.width.left = 30\n\
             amux.opt.android.keyboard.hold_ms = 400\n\
             amux.opt.android.keyboard.layers.sym.right = {\n\
               { { send = '\\2c', label = 'new' }, false },\n\
               false,\n\
             }\n",
        )
        .unwrap();
        assert_eq!(
            keyboard.width,
            KeyboardWidth {
                left: 30.0,
                right: 21.0
            }
        );
        assert_eq!((keyboard.hold_ms, keyboard.taps_ms), (400, 250));
        let sym = &keyboard.layers["sym"];
        assert_eq!(
            half(&sym.right),
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
        assert_eq!(sym.left, KeyboardSettings::default().layers["sym"].left);
    }

    #[test]
    fn a_config_adds_and_removes_layers() {
        let keyboard = load(
            "local keyboard = amux.opt.android.keyboard\n\
             keyboard.layers.sym = nil\n\
             keyboard.layers.base.right[5][3] = 'layer:git'\n\
             keyboard.layers.git = {\n\
               left = { false, { { text = 'git status\\r', label = 'st' } } },\n\
             }\n",
        )
        .unwrap();
        assert_eq!(
            keyboard.layers.keys().collect::<Vec<_>>(),
            ["amux", "base", "git", "nav", "num"]
        );
        let resolved = keyboard.resolved(Settings::default().prefix);
        let git = &resolved.layers["git"];
        assert_eq!(git.right, None);
        assert_eq!(half(&git.left)[0], half(&resolved.layers["base"].left)[0]);
        assert_eq!(
            keys_of(&half(&git.left)[1]),
            [KeySlot::Table(KeyTable {
                text: Some("git status\r".into()),
                label: Some("st".into()),
                ..KeyTable::default()
            })]
        );
    }

    #[test]
    fn keys_hold_and_tap_more_than_once() {
        let keyboard = load(
            "local base = amux.opt.android.keyboard.layers.base\n\
             base.left[2][1] = { key = 'q', hold = { 'Q', 'C-Q', { text = 'qq', label = '2q' } } }\n\
             base.left[2][2] = { key = 'w', hold = 'f2', taps = { 'Escape', 'M-w' } }\n\
             base.right[5][1] = { key = 'space', hold = 'layer:nav' }\n\
             base.right[5][2] = { key = 'Enter', hold = 'Ctrl' }\n",
        )
        .unwrap();
        let base = &keyboard.layers["base"];
        let row = keys_of(&half(&base.left)[1]);
        assert_eq!(
            row[0],
            KeySlot::Table(KeyTable {
                hold: Some(Hold::Choices(vec![
                    named("Q"),
                    named("C-q"),
                    KeySlot::Table(KeyTable {
                        text: Some("qq".into()),
                        label: Some("2q".into()),
                        ..KeyTable::default()
                    }),
                ])),
                ..table("q")
            })
        );
        assert_eq!(
            row[1],
            KeySlot::Table(KeyTable {
                hold: Some(Hold::Key(Box::new(named("F2")))),
                taps: Some(keys(&["Escape", "M-w"])),
                ..table("w")
            })
        );
        let thumbs = keys_of(&half(&base.right)[4]);
        assert_eq!(
            thumbs[0],
            KeySlot::Table(KeyTable {
                hold: Some(Hold::Key(Box::new(named("layer:nav")))),
                ..table("Space")
            })
        );
        assert_eq!(
            thumbs[1],
            KeySlot::Table(KeyTable {
                hold: Some(Hold::Key(Box::new(named("Ctrl")))),
                ..table("Enter")
            })
        );
    }

    #[test]
    fn unknown_keys_are_errors_that_count_from_one() {
        let error = load("amux.opt.android.keyboard.layers.nav.right[3][3] = 'Escap'").unwrap_err();
        assert!(
            error.starts_with(
                "amux.opt.android.keyboard.layers.nav.right[3][3]: unknown key \"Escap\", \
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
            ("C-c", "C-c"),
            ("C-C", "C-c"),
            ("M-Left", "M-Left"),
            ("C-M-x", "C-M-x"),
            ("S-a", "A"),
            ("C-i", "Tab"),
        ];
        for (written, canonical) in spellings {
            assert_eq!(
                canonical_name(written).as_deref(),
                Ok(canonical),
                "{written}"
            );
        }
        for rejected in ["S-Tab", "BackTab", "F13", "layer:", "Bogus", "C-", "c-x"] {
            assert!(canonical_name(rejected).is_err(), "{rejected}");
        }

        let keyboard = load(
            "amux.opt.android.keyboard.layers.nav.right[2][1] = 'pgup'\n\
             amux.opt.android.keyboard.layers.nav.right[2][2] = { key = 'bspace', repeats = false }",
        )
        .unwrap();
        let resolved = keyboard.resolved(Settings::default().prefix);
        let row = keys_of(&half(&resolved.layers["nav"].right)[1]);
        assert_eq!(row[0], named("PageUp"));
        assert_eq!(
            row[1],
            KeySlot::Table(KeyTable {
                repeats: Some(false),
                ..table("Backspace")
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
        let expected = [
            (
                "{ key = 'a', text = 'b' }",
                "a key table needs exactly one of key, text, send and prefix",
            ),
            (
                "{ key = 'esc', shift = 'x' }",
                "shift only applies to keys that type text",
            ),
            ("{ key = 'a', width = 0 }", "width must be more than 0"),
            ("{ text = '' }", "text must not be empty"),
            (
                "{ prefix = 'Bogus' }",
                "prefix is \"Bogus\", which isn't a key or keys such as \"c\" or \"s p\"",
            ),
            (
                "{ key = 'Ctrl', hold = 'x' }",
                "Ctrl, Alt, Shift and layer keys can't have hold or taps, since they already \
                 stay on while held and lock on a second tap",
            ),
            (
                "{ key = 'layer:nav', taps = { 'x' } }",
                "Ctrl, Alt, Shift and layer keys can't have hold or taps, since they already \
                 stay on while held and lock on a second tap",
            ),
            (
                "{ key = 'a', hold = 'x', repeats = true }",
                "a key can't both repeat and have a hold",
            ),
            (
                "{ key = 'a', hold = { 'x', 'layer:nav' } }",
                "a hold list can't use Ctrl, Alt, Shift or layer keys",
            ),
            (
                "{ key = 'a', taps = { 'Alt' } }",
                "taps can't use Ctrl, Alt, Shift or layer keys",
            ),
            (
                "{ key = 'a', hold = { '' } }",
                "a hold list can't use an empty key",
            ),
            (
                "{ key = 'a', taps = { { key = 'b', hold = 'c' } } }",
                "a key in taps can't set hold, taps, width or repeats",
            ),
        ];
        for (key, problem) in expected {
            assert_eq!(failure(key), format!("{at}: {problem}"), "{key}");
        }
        let unknown = failure("{ key = 'a', colour = 'red' }");
        assert!(unknown.contains("unknown field `colour`"), "{unknown}");
        let truth = failure("true");
        assert!(
            truth.contains("expected a key name such as \"Escape\""),
            "{truth}"
        );
        let number = failure("7");
        assert!(number.contains("integer `7`"), "{number}");
        for held in ["7", "false"] {
            let error = failure(&format!("{{ key = 'a', hold = {held} }}"));
            assert!(
                error.starts_with(&format!("{at}.hold: invalid type"))
                    && error.ends_with("expected a key, or a list of keys to pick from"),
                "{error}"
            );
        }
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
            rejected(|keyboard| keyboard.hold_ms = 10),
            "amux.opt.android.keyboard.hold_ms must be between 50 and 2000 milliseconds, not 10"
        );
        assert_eq!(
            rejected(|keyboard| keyboard.taps_ms = 5000),
            "amux.opt.android.keyboard.taps_ms must be between 50 and 2000 milliseconds, not 5000"
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.remove(BASE_LAYER);
            }),
            format!("{layers} needs a base layer")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.get_mut(BASE_LAYER).unwrap().right = None;
            }),
            format!("{layers}.base needs both left and right")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.get_mut("nav").unwrap().right = None;
            }),
            format!("{layers}.nav needs left, right or both")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard
                    .layers
                    .get_mut(BASE_LAYER)
                    .unwrap()
                    .left
                    .as_mut()
                    .unwrap()[0] = KeyRow::Base;
            }),
            format!(
                "{layers}.base.left[1] is false, which falls through to the base layer, so the \
                 base layer can't use it"
            )
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard
                    .layers
                    .get_mut(BASE_LAYER)
                    .unwrap()
                    .right
                    .as_mut()
                    .unwrap()[2] = KeyRow::Keys(vec![named("a"), KeySlot::Base]);
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
                    .as_mut()
                    .unwrap()
                    .push(KeyRow::Base);
            }),
            format!("{layers}.sym.left[6] is false, but the base layer has no row there")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.remove("sym");
            }),
            format!("{layers}.base.right[5][3] uses the unknown layer \"sym\"")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard.layers.remove("amux");
            }),
            format!("{layers}.base.left[5][3] uses the unknown layer \"amux\"")
        );
        assert_eq!(
            rejected(|keyboard| keyboard
                .layers
                .get_mut("nav")
                .unwrap()
                .right
                .as_mut()
                .unwrap()
                .clear()),
            format!("{layers}.nav.right needs at least one row")
        );
        assert_eq!(
            rejected(|keyboard| {
                keyboard
                    .layers
                    .get_mut("nav")
                    .unwrap()
                    .right
                    .as_mut()
                    .unwrap()[0] = KeyRow::Keys(Vec::new());
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
        let defaults = serde_json::to_string_pretty(&resolved_defaults());
        fixture_is_current("default.json", &format!("{}\n", defaults.unwrap()));
        fixture_is_current("key-names.txt", &format!("{}\n", KEY_NAMES.join("\n")));
    }

    #[test]
    fn resolving_fills_false_from_base_and_sends_the_prefix() {
        let resolved = resolved_defaults();
        let base = &resolved.layers[BASE_LAYER];
        let sym = &resolved.layers["sym"];
        assert_eq!(keys_of(&half(&sym.left)[0])[0], named("!"));
        assert_eq!(half(&sym.left)[4], half(&base.left)[4]);
        assert_eq!(sym.right, None);
        let nav = &resolved.layers["nav"];
        assert_eq!(half(&nav.right)[0], half(&base.right)[0]);
        let serialized = serde_json::to_string(&resolved).unwrap();
        assert!(!serialized.contains("false"), "{serialized}");
        assert!(!serialized.contains("\"prefix\""), "{serialized}");

        assert_eq!(
            keys_of(&half(&base.left)[4])[2],
            KeySlot::Table(KeyTable {
                send: Some("\u{2}".into()),
                label: Some("Prefix".into()),
                hold: Some(Hold::Key(Box::new(named("layer:amux")))),
                ..KeyTable::default()
            })
        );
        let amux = keys_of(&half(&resolved.layers["amux"].right)[2])[0].clone();
        assert_eq!(
            amux,
            KeySlot::Table(KeyTable {
                send: Some("\u{2}sp".into()),
                label: Some("projects".into()),
                ..KeyTable::default()
            })
        );
        let other = KeyboardSettings::default().resolved("M-a".parse().unwrap());
        assert_eq!(
            keys_of(&half(&other.layers["amux"].right)[0])[0],
            KeySlot::Table(KeyTable {
                send: Some("\u{1b}ac".into()),
                label: Some("+win".into()),
                ..KeyTable::default()
            })
        );

        let mut longer = KeyboardSettings::default();
        longer.layers.get_mut("sym").unwrap().left.as_mut().unwrap()[0] =
            KeyRow::Keys(keys(&["a"; 5]).into_iter().chain([KeySlot::Base]).collect());
        let resolved = longer.resolved(Settings::default().prefix);
        assert_eq!(
            keys_of(&half(&resolved.layers["sym"].left)[0])[5],
            named("")
        );
    }
}
