mod decode;
mod encode;
mod notation;
mod scan;

use std::ops::{BitOr, RangeInclusive};

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

pub use decode::{Decoded, KeyDecoder};
pub use notation::{parse_sequence, spell_sequence};
pub use scan::Scanner;

pub const ESC: u8 = 0x1b;
const FUNCTION_KEYS: RangeInclusive<u8> = 1..=12;
const X10_MOUSE_PAYLOAD_LEN: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mods(u8);

impl Mods {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(1);
    pub const ALT: Self = Self(2);
    pub const CTRL: Self = Self(4);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    fn from_xterm(parameter: u8) -> Option<Self> {
        (1..=8).contains(&parameter).then(|| Self(parameter - 1))
    }

    fn xterm(self) -> u8 {
        self.0 + 1
    }
}

impl BitOr for Mods {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub code: KeyCode,
    pub mods: Mods,
}

impl Key {
    pub fn new(code: KeyCode, mods: Mods) -> Self {
        match code {
            KeyCode::Tab if mods.contains(Mods::SHIFT) => Self {
                code: KeyCode::BackTab,
                mods: mods.without(Mods::SHIFT),
            },
            KeyCode::Char(character) if mods.contains(Mods::CTRL) => ctrl_char(character, mods),
            KeyCode::Char(character)
                if mods.contains(Mods::SHIFT) && character.is_ascii_lowercase() =>
            {
                Self {
                    code: KeyCode::Char(character.to_ascii_uppercase()),
                    mods: mods.without(Mods::SHIFT),
                }
            }
            _ => Self { code, mods },
        }
    }

    pub fn char(character: char) -> Self {
        Self::from(KeyCode::Char(character))
    }

    pub fn ctrl(character: char) -> Self {
        Self::new(KeyCode::Char(character), Mods::CTRL)
    }

    pub fn with(self, mods: Mods) -> Self {
        Self::new(self.code, self.mods | mods)
    }

    pub fn without(self, mods: Mods) -> Self {
        Self::new(self.code, self.mods.without(mods))
    }

    pub fn printable(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(character) if self.mods.is_empty() => Some(character),
            _ => None,
        }
    }

    pub fn label(&self) -> String {
        self.spelled(["Ctrl-", "Alt-", "Shift-"])
    }
}

fn ctrl_char(character: char, mods: Mods) -> Key {
    let named = match character.to_ascii_lowercase() {
        'i' => KeyCode::Tab,
        'm' => KeyCode::Enter,
        '[' => KeyCode::Escape,
        '@' => KeyCode::Char(' '),
        lower => KeyCode::Char(lower),
    };
    match named {
        KeyCode::Char(_) => Key { code: named, mods },
        _ => Key {
            code: named,
            mods: mods.without(Mods::CTRL),
        },
    }
}

impl From<KeyCode> for Key {
    fn from(code: KeyCode) -> Self {
        Self {
            code,
            mods: Mods::NONE,
        }
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let notation = String::deserialize(deserializer)?;
        notation.parse().map_err(de::Error::custom)
    }
}
