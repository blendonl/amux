use std::fmt;
use std::str::FromStr;

use anyhow::{anyhow, Result};

use super::{Key, KeyCode, Mods, FUNCTION_KEYS};

const MODIFIERS: [(Mods, &str); 3] = [(Mods::CTRL, "C-"), (Mods::ALT, "M-"), (Mods::SHIFT, "S-")];

const NAMES: [(&str, KeyCode); 25] = [
    ("Space", KeyCode::Char(' ')),
    ("Enter", KeyCode::Enter),
    ("Tab", KeyCode::Tab),
    ("BackTab", KeyCode::BackTab),
    ("Backspace", KeyCode::Backspace),
    ("Escape", KeyCode::Escape),
    ("Up", KeyCode::Up),
    ("Down", KeyCode::Down),
    ("Left", KeyCode::Left),
    ("Right", KeyCode::Right),
    ("Home", KeyCode::Home),
    ("End", KeyCode::End),
    ("Insert", KeyCode::Insert),
    ("Delete", KeyCode::Delete),
    ("PageUp", KeyCode::PageUp),
    ("PageDown", KeyCode::PageDown),
    ("BTab", KeyCode::BackTab),
    ("BSpace", KeyCode::Backspace),
    ("Esc", KeyCode::Escape),
    ("IC", KeyCode::Insert),
    ("DC", KeyCode::Delete),
    ("PPage", KeyCode::PageUp),
    ("PgUp", KeyCode::PageUp),
    ("NPage", KeyCode::PageDown),
    ("PgDn", KeyCode::PageDown),
];

impl Key {
    pub(super) fn spelled(&self, modifiers: [&str; 3]) -> String {
        let mut spelled: String = MODIFIERS
            .iter()
            .zip(modifiers)
            .filter(|((modifier, _), _)| self.mods.contains(*modifier))
            .map(|(_, spelling)| spelling)
            .collect();
        spelled.push_str(&name(self.code));
        spelled
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.spelled(MODIFIERS.map(|(_, spelling)| spelling)))
    }
}

impl FromStr for Key {
    type Err = anyhow::Error;

    fn from_str(notation: &str) -> Result<Self> {
        let mut mods = Mods::NONE;
        let mut rest = notation;
        while let Some((modifier, name)) = split_modifier(rest) {
            mods = mods | modifier;
            rest = name;
        }
        let code = code_named(rest).ok_or_else(|| anyhow!("invalid key {notation:?}"))?;
        Ok(Self::new(code, mods))
    }
}

fn split_modifier(notation: &str) -> Option<(Mods, &str)> {
    MODIFIERS.iter().find_map(|&(modifier, spelling)| {
        notation
            .strip_prefix(spelling)
            .filter(|rest| !rest.is_empty())
            .map(|rest| (modifier, rest))
    })
}

fn code_named(name: &str) -> Option<KeyCode> {
    let mut characters = name.chars();
    if let (Some(character), None) = (characters.next(), characters.next()) {
        return (!character.is_control()).then_some(KeyCode::Char(character));
    }
    NAMES
        .iter()
        .find(|(spelling, _)| spelling.eq_ignore_ascii_case(name))
        .map(|&(_, code)| code)
        .or_else(|| function_key(name))
}

fn function_key(name: &str) -> Option<KeyCode> {
    let number = name.strip_prefix(['F', 'f'])?;
    if !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let number = number.parse().ok()?;
    FUNCTION_KEYS
        .contains(&number)
        .then_some(KeyCode::F(number))
}

fn name(code: KeyCode) -> String {
    match code {
        KeyCode::F(number) => format!("F{number}"),
        KeyCode::Char(character) if character != ' ' => character.to_string(),
        _ => NAMES
            .iter()
            .find(|&&(_, named)| named == code)
            .map_or_else(String::new, |(spelling, _)| (*spelling).to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(notation: &str) -> Key {
        notation.parse().unwrap()
    }

    #[test]
    fn notation_round_trips() {
        for notation in [
            "C-b",
            "M-h",
            "S-Left",
            "M-S-Up",
            "C-M-S-Up",
            "F12",
            "Space",
            "%",
            "Enter",
            "a",
            "A",
            "C-Space",
            "M-Enter",
            "BackTab",
            "Backspace",
            "Escape",
            "PageUp",
            "PageDown",
            "Insert",
            "Delete",
            "Home",
            "End",
            "C--",
            "-",
            "日",
            "M-é",
            "C-\\",
        ] {
            assert_eq!(key(notation).to_string(), notation);
        }
    }

    #[test]
    fn notation_builds_modified_keys() {
        assert_eq!(key("C-b"), Key::ctrl('b'));
        assert_eq!(
            key("M-S-Up"),
            Key::new(KeyCode::Up, Mods::ALT | Mods::SHIFT)
        );
        assert_eq!(key("S-M-Up"), key("M-S-Up"));
        assert_eq!(key("F1"), Key::from(KeyCode::F(1)));
        assert_eq!(key("%"), Key::char('%'));
        assert_eq!(key("Space"), Key::char(' '));
    }

    #[test]
    fn aliases_and_equivalent_spellings_become_one_key() {
        for (alias, canonical) in [
            ("BSpace", "Backspace"),
            ("bspace", "Backspace"),
            ("Esc", "Escape"),
            ("DC", "Delete"),
            ("IC", "Insert"),
            ("PPage", "PageUp"),
            ("NPage", "PageDown"),
            ("BTab", "BackTab"),
            ("S-Tab", "BackTab"),
            ("S-a", "A"),
            ("C-A", "C-a"),
            ("C-i", "Tab"),
            ("C-m", "Enter"),
            ("C-[", "Escape"),
            ("C-@", "C-Space"),
            ("M-C-b", "C-M-b"),
            ("f5", "F5"),
        ] {
            assert_eq!(key(alias), key(canonical), "{alias}");
        }
    }

    #[test]
    fn invalid_notation_is_rejected() {
        for notation in ["", "C-", "Bogus", "F0", "F13", "F+1", "\x01", "ab", "X-a"] {
            assert!(notation.parse::<Key>().is_err(), "{notation:?}");
        }
    }

    #[test]
    fn labels_spell_out_the_modifiers() {
        assert_eq!(key("C-b").label(), "Ctrl-b");
        assert_eq!(key("M-h").label(), "Alt-h");
        assert_eq!(key("M-S-Up").label(), "Alt-Shift-Up");
        assert_eq!(key("d").label(), "d");
        assert_eq!(key("F12").label(), "F12");
        assert_eq!(key("C-Space").label(), "Ctrl-Space");
    }

    #[test]
    fn keys_serialize_as_their_notation() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct Holder {
            key: Key,
        }

        let holder: Holder = toml::from_str("key = \"M-S-Up\"").unwrap();
        assert_eq!(holder.key, key("M-S-Up"));
        assert_eq!(toml::to_string(&holder).unwrap(), "key = \"M-S-Up\"\n");
        assert!(toml::from_str::<Holder>("key = \"Bogus\"").is_err());
    }
}
