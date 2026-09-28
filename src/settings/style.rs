use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, Result};
use serde::de::{self, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];
const DEFAULT_NAME: &str = "default";
const EXPECTED: &str =
    "a color name such as \"green\" or \"bright-blue\", \"default\", an index 0-255 or \"#rrggbb\"";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub const BLACK: Self = Self::Indexed(0);
    pub const RED: Self = Self::Indexed(1);
    pub const GREEN: Self = Self::Indexed(2);
    pub const YELLOW: Self = Self::Indexed(3);
    pub const BLUE: Self = Self::Indexed(4);
    pub const MAGENTA: Self = Self::Indexed(5);
    pub const CYAN: Self = Self::Indexed(6);
    pub const WHITE: Self = Self::Indexed(7);
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Default => f.write_str(DEFAULT_NAME),
            Self::Indexed(index) => match NAMES.get(usize::from(index)) {
                Some(name) => f.write_str(name),
                None => write!(f, "{index}"),
            },
            Self::Rgb(red, green, blue) => write!(f, "#{red:02x}{green:02x}{blue:02x}"),
        }
    }
}

impl FromStr for Color {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text == DEFAULT_NAME {
            return Ok(Self::Default);
        }
        if let Some((_, index)) = NAMES.iter().zip(0..).find(|(name, _)| **name == text) {
            return Ok(Self::Indexed(index));
        }
        if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
            if let Ok(index) = text.parse() {
                return Ok(Self::Indexed(index));
            }
        }
        text.strip_prefix('#')
            .and_then(parse_hex)
            .ok_or_else(|| anyhow!("invalid color {text:?}: expected {EXPECTED}"))
    }
}

fn parse_hex(digits: &str) -> Option<Color> {
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?))
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match *self {
            Self::Indexed(index) if usize::from(index) >= NAMES.len() => {
                serializer.serialize_u8(index)
            }
            _ => serializer.collect_str(self),
        }
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ColorVisitor)
    }
}

struct ColorVisitor;

impl Visitor<'_> for ColorVisitor {
    type Value = Color;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(EXPECTED)
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Color, E> {
        text.parse().map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, index: u64) -> Result<Color, E> {
        u8::try_from(index)
            .map(Color::Indexed)
            .map_err(|_| E::invalid_value(Unexpected::Unsigned(index), &self))
    }

    fn visit_i64<E: de::Error>(self, index: i64) -> Result<Color, E> {
        u8::try_from(index)
            .map(Color::Indexed)
            .map_err(|_| E::invalid_value(Unexpected::Signed(index), &self))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StyleSpec {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: Option<bool>,
    pub dim: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub reverse: Option<bool>,
}

impl StyleSpec {
    pub const EMPTY: Self = Self {
        fg: None,
        bg: None,
        bold: None,
        dim: None,
        italic: None,
        underline: None,
        reverse: None,
    };
    pub const BOLD: Self = Self {
        bold: Some(true),
        ..Self::EMPTY
    };
    pub const DIM: Self = Self {
        dim: Some(true),
        ..Self::EMPTY
    };
    pub const REVERSE: Self = Self {
        reverse: Some(true),
        ..Self::EMPTY
    };

    pub const fn colors(fg: Color, bg: Color) -> Self {
        Self {
            fg: Some(fg),
            bg: Some(bg),
            ..Self::EMPTY
        }
    }

    pub fn merge(self, other: Self) -> Self {
        Self {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: other.bold.or(self.bold),
            dim: other.dim.or(self.dim),
            italic: other.italic.or(self.italic),
            underline: other.underline.or(self.underline),
            reverse: other.reverse.or(self.reverse),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(value: toml::Value) -> Result<Color, toml::de::Error> {
        value.try_into()
    }

    fn serialized(color: Color) -> toml::Value {
        toml::Value::try_from(color).unwrap()
    }

    #[test]
    fn colors_parse_from_names_indexes_and_hex() {
        for (text, color) in [
            ("default", Color::Default),
            ("black", Color::BLACK),
            ("green", Color::GREEN),
            ("white", Color::WHITE),
            ("bright-black", Color::Indexed(8)),
            ("bright-blue", Color::Indexed(12)),
            ("bright-white", Color::Indexed(15)),
            ("#8ec07c", Color::Rgb(0x8e, 0xc0, 0x7c)),
            ("#FF00aa", Color::Rgb(0xff, 0x00, 0xaa)),
            ("42", Color::Indexed(42)),
        ] {
            assert_eq!(text.parse::<Color>().unwrap(), color, "{text}");
            assert_eq!(parsed(text.into()).unwrap(), color, "{text}");
        }
        assert_eq!(parsed(0.into()).unwrap(), Color::BLACK);
        assert_eq!(parsed(42.into()).unwrap(), Color::Indexed(42));
        assert_eq!(parsed(255.into()).unwrap(), Color::Indexed(255));
    }

    #[test]
    fn invalid_colors_are_rejected() {
        for text in [
            "", "purple", "Green", "bright", "bright-", "#", "#12345", "#1234567", "#gg0000",
            "#+12345", "8ec07c", "256", "-1", "+1",
        ] {
            assert!(text.parse::<Color>().is_err(), "{text:?}");
            assert!(parsed(text.into()).is_err(), "{text:?}");
        }
        for index in [-1, 256, 1000] {
            assert!(parsed(index.into()).is_err(), "{index}");
        }
        assert!(parsed(true.into()).is_err());
        assert!(parsed(1.5.into()).is_err());

        let error = parsed("purple".into()).unwrap_err().to_string();
        assert!(error.contains("\"purple\""), "{error}");
        assert!(error.contains("#rrggbb"), "{error}");
    }

    #[test]
    fn colors_serialize_to_their_most_natural_form() {
        for (color, expected) in [
            (Color::Default, toml::Value::from("default")),
            (Color::YELLOW, "yellow".into()),
            (Color::Indexed(9), "bright-red".into()),
            (Color::Indexed(16), 16.into()),
            (Color::Indexed(255), 255.into()),
            (Color::Rgb(0x8e, 0xc0, 0x7c), "#8ec07c".into()),
            (Color::Rgb(0, 10, 255), "#000aff".into()),
        ] {
            assert_eq!(serialized(color), expected, "{color:?}");
            assert_eq!(parsed(expected).unwrap(), color);
        }
    }

    #[test]
    fn a_style_reads_only_the_fields_it_sets() {
        let spec: StyleSpec =
            toml::from_str("fg = \"bright-cyan\"\nbg = 236\nitalic = true\nunderline = false")
                .unwrap();
        assert_eq!(
            spec,
            StyleSpec {
                fg: Some(Color::Indexed(14)),
                bg: Some(Color::Indexed(236)),
                italic: Some(true),
                underline: Some(false),
                ..StyleSpec::EMPTY
            }
        );
        assert_eq!(toml::from_str::<StyleSpec>("").unwrap(), StyleSpec::EMPTY);
        assert!(toml::from_str::<StyleSpec>("blink = true").is_err());
        assert!(toml::from_str::<StyleSpec>("fg = \"purple\"").is_err());
        assert!(toml::from_str::<StyleSpec>("bold = \"yes\"").is_err());

        let written = toml::to_string(&spec).unwrap();
        assert_eq!(toml::from_str::<StyleSpec>(&written).unwrap(), spec);
        assert!(!written.contains("bold"), "{written}");
    }

    #[test]
    fn merging_lets_the_other_style_win_where_it_is_set() {
        let base = StyleSpec {
            underline: Some(true),
            ..StyleSpec::colors(Color::BLACK, Color::GREEN)
        };
        let delta = StyleSpec {
            fg: Some(Color::Rgb(1, 2, 3)),
            underline: Some(false),
            ..StyleSpec::BOLD
        };
        assert_eq!(
            base.merge(delta),
            StyleSpec {
                fg: Some(Color::Rgb(1, 2, 3)),
                bg: Some(Color::GREEN),
                bold: Some(true),
                underline: Some(false),
                ..StyleSpec::EMPTY
            }
        );
        assert_eq!(base.merge(StyleSpec::EMPTY), base);
        assert_eq!(StyleSpec::EMPTY.merge(delta), delta);
        assert_eq!(delta.merge(base).fg, Some(Color::BLACK));
    }
}
