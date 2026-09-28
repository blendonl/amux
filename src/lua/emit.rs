use std::collections::BTreeMap;

use anyhow::Result;
use serde::de::value::Error;
use serde::ser::{self, Error as _, Serialize};

const INDENT: &str = "  ";
const KEYWORDS: [&str; 22] = [
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

pub fn literal<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    let mut out = String::new();
    value.serialize(Emitter)?.write(&mut out, 0);
    Ok(out)
}

pub fn chunk<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(format!("return {}\n", literal(value)?))
}

pub fn assignments<T: Serialize + ?Sized>(target: &str, value: &T) -> Result<String> {
    let Literal::Table(entries) = value.serialize(Emitter)? else {
        anyhow::bail!("only the fields of a table can be assigned to {target}");
    };
    let mut out = String::new();
    for (key, value) in entries.iter().filter(|(_, value)| !value.is_empty()) {
        out.push_str(target);
        key.write_index(&mut out);
        out.push_str(" = ");
        value.write(&mut out, 0);
        out.push('\n');
    }
    Ok(out)
}

enum Literal {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(Vec<u8>),
    List(Vec<Literal>),
    Table(BTreeMap<Key, Literal>),
}

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Integer(i64),
    String(Vec<u8>),
}

impl Literal {
    fn is_scalar(&self) -> bool {
        !matches!(self, Self::List(_) | Self::Table(_))
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::List(items) => items.is_empty(),
            Self::Table(entries) => entries.is_empty(),
            _ => false,
        }
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Self::Nil => out.push_str("nil"),
            Self::Boolean(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Integer(value) => write_integer(out, *value),
            Self::Float(value) => write_float(out, *value),
            Self::String(bytes) => write_string(out, bytes),
            Self::List(items) if items.is_empty() => out.push_str("{}"),
            Self::Table(entries) if entries.is_empty() => out.push_str("{}"),
            Self::List(items) if items.iter().all(Literal::is_scalar) => {
                out.push_str("{ ");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    item.write(out, depth);
                }
                out.push_str(" }");
            }
            Self::List(items) => write_block(out, depth, items.iter().map(|item| (None, item))),
            Self::Table(entries) => write_block(
                out,
                depth,
                entries.iter().map(|(key, value)| (Some(key), value)),
            ),
        }
    }
}

impl Key {
    fn write_index(&self, out: &mut String) {
        if let Self::String(name) = self {
            if let Some(name) = identifier(name) {
                out.push('.');
                out.push_str(name);
                return;
            }
        }
        match self {
            Self::String(name) => {
                out.push('[');
                write_string(out, name);
                out.push(']');
            }
            Self::Integer(_) => self.write(out),
        }
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::String(name) => match identifier(name) {
                Some(name) => out.push_str(name),
                None => {
                    out.push('[');
                    write_string(out, name);
                    out.push(']');
                }
            },
            Self::Integer(index) => {
                out.push('[');
                write_integer(out, *index);
                out.push(']');
            }
        }
    }
}

impl TryFrom<Literal> for Key {
    type Error = Error;

    fn try_from(literal: Literal) -> Result<Self, Error> {
        match literal {
            Literal::Integer(index) => Ok(Self::Integer(index)),
            Literal::String(name) => Ok(Self::String(name)),
            _ => Err(Error::custom(
                "a Lua table key must be a string or an integer",
            )),
        }
    }
}

fn write_block<'a>(
    out: &mut String,
    depth: usize,
    entries: impl Iterator<Item = (Option<&'a Key>, &'a Literal)>,
) {
    out.push_str("{\n");
    for (key, value) in entries {
        out.push_str(&INDENT.repeat(depth + 1));
        if let Some(key) = key {
            key.write(out);
            out.push_str(" = ");
        }
        value.write(out, depth + 1);
        out.push_str(",\n");
    }
    out.push_str(&INDENT.repeat(depth));
    out.push('}');
}

fn identifier(bytes: &[u8]) -> Option<&str> {
    let name = std::str::from_utf8(bytes).ok()?;
    let mut characters = name.chars();
    let leads = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_');
    let rest = characters.all(|character| character.is_ascii_alphanumeric() || character == '_');
    (leads && rest && !KEYWORDS.contains(&name)).then_some(name)
}

fn write_integer(out: &mut String, value: i64) {
    if value == i64::MIN {
        out.push_str(&format!("({} - 1)", i64::MIN + 1));
    } else {
        out.push_str(&value.to_string());
    }
}

fn write_float(out: &mut String, value: f64) {
    if value.is_nan() {
        out.push_str("(0/0)");
    } else if value.is_infinite() {
        out.push_str(if value > 0.0 { "(1/0)" } else { "(-1/0)" });
    } else {
        out.push_str(&format!("{value:?}"));
    }
}

fn write_string(out: &mut String, bytes: &[u8]) {
    out.push('"');
    match std::str::from_utf8(bytes) {
        Ok(text) => text.chars().for_each(|character| escape(out, character)),
        Err(_) => {
            for &byte in bytes {
                if byte.is_ascii() {
                    escape(out, char::from(byte));
                } else {
                    out.push_str(&format!("\\{byte:03}"));
                }
            }
        }
    }
    out.push('"');
}

fn escape(out: &mut String, character: char) {
    match character {
        '\\' => out.push_str("\\\\"),
        '"' => out.push_str("\\\""),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        control if control.is_ascii_control() => {
            out.push_str(&format!("\\{:03}", u32::from(control)));
        }
        other => out.push(other),
    }
}

fn integer<T: TryInto<i64> + Copy + std::fmt::Display>(value: T) -> Result<Literal, Error> {
    value
        .try_into()
        .map(Literal::Integer)
        .map_err(|_| Error::custom(format!("{value} does not fit in a Lua integer")))
}

fn variant(name: Option<&'static str>, literal: Literal) -> Literal {
    match name {
        Some(name) => Literal::Table(BTreeMap::from([(Key::String(name.into()), literal)])),
        None => literal,
    }
}

struct Emitter;

impl ser::Serializer for Emitter {
    type Ok = Literal;
    type Error = Error;
    type SerializeSeq = ListEmitter;
    type SerializeTuple = ListEmitter;
    type SerializeTupleStruct = ListEmitter;
    type SerializeTupleVariant = ListEmitter;
    type SerializeMap = TableEmitter;
    type SerializeStruct = TableEmitter;
    type SerializeStructVariant = TableEmitter;

    fn serialize_bool(self, value: bool) -> Result<Literal, Error> {
        Ok(Literal::Boolean(value))
    }

    fn serialize_i8(self, value: i8) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_i16(self, value: i16) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_i32(self, value: i32) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_i64(self, value: i64) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_i128(self, value: i128) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_u8(self, value: u8) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_u16(self, value: u16) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_u32(self, value: u32) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_u64(self, value: u64) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_u128(self, value: u128) -> Result<Literal, Error> {
        integer(value)
    }

    fn serialize_f32(self, value: f32) -> Result<Literal, Error> {
        Ok(Literal::Float(value.into()))
    }

    fn serialize_f64(self, value: f64) -> Result<Literal, Error> {
        Ok(Literal::Float(value))
    }

    fn serialize_char(self, value: char) -> Result<Literal, Error> {
        Ok(Literal::String(value.to_string().into_bytes()))
    }

    fn serialize_str(self, value: &str) -> Result<Literal, Error> {
        Ok(Literal::String(value.as_bytes().to_vec()))
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Literal, Error> {
        Ok(Literal::String(value.to_vec()))
    }

    fn serialize_none(self) -> Result<Literal, Error> {
        Ok(Literal::Nil)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Literal, Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Literal, Error> {
        Ok(Literal::Nil)
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Literal, Error> {
        Ok(Literal::Nil)
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
    ) -> Result<Literal, Error> {
        self.serialize_str(name)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Literal, Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        value: &T,
    ) -> Result<Literal, Error> {
        Ok(variant(Some(name), value.serialize(self)?))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<ListEmitter, Error> {
        Ok(ListEmitter::new(None, len.unwrap_or_default()))
    }

    fn serialize_tuple(self, len: usize) -> Result<ListEmitter, Error> {
        Ok(ListEmitter::new(None, len))
    }

    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<ListEmitter, Error> {
        Ok(ListEmitter::new(None, len))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        len: usize,
    ) -> Result<ListEmitter, Error> {
        Ok(ListEmitter::new(Some(name), len))
    }

    fn serialize_map(self, _: Option<usize>) -> Result<TableEmitter, Error> {
        Ok(TableEmitter::new(None))
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<TableEmitter, Error> {
        Ok(TableEmitter::new(None))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        name: &'static str,
        _: usize,
    ) -> Result<TableEmitter, Error> {
        Ok(TableEmitter::new(Some(name)))
    }
}

struct ListEmitter {
    variant: Option<&'static str>,
    items: Vec<Literal>,
}

impl ListEmitter {
    fn new(variant: Option<&'static str>, len: usize) -> Self {
        Self {
            variant,
            items: Vec::with_capacity(len),
        }
    }

    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.items.push(value.serialize(Emitter)?);
        Ok(())
    }

    fn finish(self) -> Result<Literal, Error> {
        Ok(variant(self.variant, Literal::List(self.items)))
    }
}

impl ser::SerializeSeq for ListEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

impl ser::SerializeTuple for ListEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for ListEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for ListEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.push(value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

struct TableEmitter {
    variant: Option<&'static str>,
    entries: BTreeMap<Key, Literal>,
    key: Option<Key>,
}

impl TableEmitter {
    fn new(variant: Option<&'static str>) -> Self {
        Self {
            variant,
            entries: BTreeMap::new(),
            key: None,
        }
    }

    fn insert<T: Serialize + ?Sized>(&mut self, key: Key, value: &T) -> Result<(), Error> {
        match value.serialize(Emitter)? {
            Literal::Nil => {}
            value => {
                self.entries.insert(key, value);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Literal, Error> {
        Ok(variant(self.variant, Literal::Table(self.entries)))
    }
}

impl ser::SerializeMap for TableEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.key = Some(key.serialize(Emitter)?.try_into()?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let key = self
            .key
            .take()
            .ok_or_else(|| Error::custom("a map value was serialized before its key"))?;
        self.insert(key, value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

impl ser::SerializeStruct for TableEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.insert(Key::String(name.into()), value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for TableEmitter {
    type Ok = Literal;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.insert(Key::String(name.into()), value)
    }

    fn end(self) -> Result<Literal, Error> {
        self.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::lua::{data, runtime, Process};
    use crate::settings::Settings;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Shape {
        Dot,
        Circle(u32),
        Line { length: u32 },
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        zeta: bool,
        alpha: Option<String>,
        missing: Option<u8>,
        tags: Vec<String>,
        shapes: Vec<Shape>,
        servers: BTreeMap<String, u16>,
        empty: Vec<u8>,
    }

    struct Bytes<'a>(&'a [u8]);

    impl Serialize for Bytes<'_> {
        fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    fn sample() -> Sample {
        Sample {
            zeta: true,
            alpha: Some("a".into()),
            missing: None,
            tags: vec!["x".into(), "y".into()],
            shapes: vec![Shape::Dot, Shape::Circle(2), Shape::Line { length: 3 }],
            servers: BTreeMap::from([
                ("my-host".into(), 22),
                ("end".into(), 1),
                ("web".into(), 80),
            ]),
            empty: Vec::new(),
        }
    }

    #[test]
    fn tables_are_written_with_sorted_keys_and_indentation() {
        assert_eq!(
            literal(&sample()).unwrap(),
            "{\n\
             \x20 alpha = \"a\",\n\
             \x20 empty = {},\n\
             \x20 servers = {\n\
             \x20   [\"end\"] = 1,\n\
             \x20   [\"my-host\"] = 22,\n\
             \x20   web = 80,\n\
             \x20 },\n\
             \x20 shapes = {\n\
             \x20   \"dot\",\n\
             \x20   {\n\
             \x20     circle = 2,\n\
             \x20   },\n\
             \x20   {\n\
             \x20     line = {\n\
             \x20       length = 3,\n\
             \x20     },\n\
             \x20   },\n\
             \x20 },\n\
             \x20 tags = { \"x\", \"y\" },\n\
             \x20 zeta = true,\n\
             }"
        );
        assert_eq!(literal(&sample()).unwrap(), literal(&sample()).unwrap());
    }

    #[test]
    fn assignments_set_each_field_and_skip_empty_tables() {
        assert_eq!(
            assignments("config", &sample()).unwrap(),
            "config.alpha = \"a\"\n\
             config.servers = {\n\
             \x20 [\"end\"] = 1,\n\
             \x20 [\"my-host\"] = 22,\n\
             \x20 web = 80,\n\
             }\n\
             config.shapes = {\n\
             \x20 \"dot\",\n\
             \x20 {\n\
             \x20   circle = 2,\n\
             \x20 },\n\
             \x20 {\n\
             \x20   line = {\n\
             \x20     length = 3,\n\
             \x20   },\n\
             \x20 },\n\
             }\n\
             config.tags = { \"x\", \"y\" }\n\
             config.zeta = true\n"
        );
        let odd = BTreeMap::from([("my-host", 1), ("end", 2)]);
        assert_eq!(
            assignments("t", &odd).unwrap(),
            "t[\"end\"] = 2\nt[\"my-host\"] = 1\n"
        );
        assert!(assignments("t", &[1, 2]).is_err());
        assert!(assignments("t", &5).is_err());
    }

    #[test]
    fn default_settings_assigned_to_amux_opt_load_unchanged() {
        let written = assignments("amux.opt", &Settings::default()).unwrap();
        let loaded = runtime::load_init(&written, Process::Server).unwrap();
        assert_eq!(loaded.settings, Settings::default());
        assert!(
            written.contains("amux.opt.escape_time_ms = 50\n"),
            "{written}"
        );
        assert!(written.contains("amux.opt.cluster = {\n"), "{written}");
        assert!(!written.contains("amux.opt.name"), "{written}");
        assert!(!written.contains("amux.opt.servers"), "{written}");
    }

    #[test]
    fn a_written_value_reads_back_as_data() {
        let written = chunk(&sample()).unwrap();
        assert!(written.starts_with("return {\n"), "{written}");
        assert!(written.ends_with("}\n"), "{written}");
        let read: Sample = data::parse(Path::new("sample.lua"), written.as_bytes()).unwrap();
        assert_eq!(read, sample());
    }

    #[test]
    fn the_default_settings_read_back_unchanged() {
        let written = chunk(&Settings::default()).unwrap();
        let read: Settings = data::parse(Path::new("defaults.lua"), written.as_bytes()).unwrap();
        assert_eq!(read, Settings::default());
        assert!(written.contains("  prefix = \"C-b\",\n"), "{written}");
        assert!(written.contains("  escape_time_ms = 50,\n"), "{written}");
    }

    #[test]
    fn strings_are_escaped_for_lua() {
        assert_eq!(
            literal("q\"b\\n\nt\tr\r\u{1}\u{7f}9é").unwrap(),
            "\"q\\\"b\\\\n\\nt\\tr\\r\\001\\1279é\""
        );
        assert_eq!(
            literal(&Bytes(&[b'a', 0xff, b'"'])).unwrap(),
            "\"a\\255\\\"\""
        );
        let text = "q\"b\\n\nt\tr\r\u{1}\u{7f}9é]]";
        let read: Vec<String> =
            data::parse(Path::new("strings.lua"), chunk(&[text]).unwrap().as_bytes()).unwrap();
        assert_eq!(read, [text]);
    }

    #[test]
    fn keys_that_are_not_identifiers_are_quoted() {
        let keys = BTreeMap::from([
            ("_ok", 1),
            ("ok2", 2),
            ("2nd", 3),
            ("while", 4),
            ("a b", 5),
            ("", 6),
            ("é", 7),
        ]);
        assert_eq!(
            literal(&keys).unwrap(),
            "{\n\
             \x20 [\"\"] = 6,\n\
             \x20 [\"2nd\"] = 3,\n\
             \x20 _ok = 1,\n\
             \x20 [\"a b\"] = 5,\n\
             \x20 ok2 = 2,\n\
             \x20 [\"while\"] = 4,\n\
             \x20 [\"é\"] = 7,\n\
             }"
        );
        let numbered = BTreeMap::from([(10, "b"), (-1, "a")]);
        assert_eq!(
            literal(&numbered).unwrap(),
            "{\n  [-1] = \"a\",\n  [10] = \"b\",\n}"
        );
        assert!(literal(&BTreeMap::from([(true, 1)])).is_err());
    }

    #[test]
    fn numbers_keep_their_lua_type() {
        assert_eq!(literal(&1.0).unwrap(), "1.0");
        assert_eq!(literal(&-0.5).unwrap(), "-0.5");
        assert_eq!(literal(&1e300).unwrap(), "1e300");
        assert_eq!(literal(&f64::INFINITY).unwrap(), "(1/0)");
        assert_eq!(literal(&f64::NEG_INFINITY).unwrap(), "(-1/0)");
        assert_eq!(literal(&f64::NAN).unwrap(), "(0/0)");
        assert_eq!(literal(&i64::MIN).unwrap(), "(-9223372036854775807 - 1)");
        assert_eq!(
            literal(&u64::MAX.to_string()).unwrap(),
            "\"18446744073709551615\""
        );
        assert!(literal(&u64::MAX).is_err());

        let numbers = [i64::MIN, -1, 0, i64::MAX];
        let read: Vec<i64> =
            data::parse(Path::new("n.lua"), chunk(&numbers).unwrap().as_bytes()).unwrap();
        assert_eq!(read, numbers);
        let floats = [1.0, 0.1, -2.5e-8, 1e300, f64::INFINITY, f64::NEG_INFINITY];
        let read: Vec<f64> =
            data::parse(Path::new("f.lua"), chunk(&floats).unwrap().as_bytes()).unwrap();
        assert_eq!(read, floats);
        let read: Vec<f64> =
            data::parse(Path::new("nan.lua"), chunk(&[f64::NAN]).unwrap().as_bytes()).unwrap();
        assert!(read[0].is_nan());
    }
}
