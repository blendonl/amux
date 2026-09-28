use std::mem;

use super::encode::function_tilde;
use super::{Key, KeyCode, Mods, ESC, FUNCTION_KEYS, X10_MOUSE_PAYLOAD_LEN};

const MAX_SEQUENCE_LEN: usize = 32;
const X10_MOUSE: &[u8] = b"\x1b[M";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub key: Option<Key>,
    pub raw: Vec<u8>,
}

impl Decoded {
    pub fn split_escape(&self) -> Option<(Self, Self)> {
        let key = self.key.filter(|key| key.mods.contains(Mods::ALT))?;
        let rest = self.raw.strip_prefix(&[ESC])?;
        if matches!(rest.first(), None | Some(b'[' | b'O')) {
            return None;
        }
        let escape = Self {
            key: Some(Key::from(KeyCode::Escape)),
            raw: vec![ESC],
        };
        let unmeta = Self {
            key: Some(key.without(Mods::ALT)),
            raw: rest.to_vec(),
        };
        Some((escape, unmeta))
    }

    pub fn mouse_payload(&self) -> Option<&[u8]> {
        self.raw
            .strip_prefix(X10_MOUSE)
            .filter(|payload| payload.len() == X10_MOUSE_PAYLOAD_LEN)
    }
}

#[derive(Debug, Default)]
pub struct KeyDecoder {
    pending: Vec<u8>,
}

impl KeyDecoder {
    pub fn feed(&mut self, input: &[u8]) -> Vec<Decoded> {
        self.push(input);
        let mut decoded = Vec::new();
        let mut offset = 0;
        while offset < self.pending.len() {
            let Some((key, len)) = decode(&self.pending[offset..]) else {
                break;
            };
            decoded.push(Decoded {
                key,
                raw: self.pending[offset..offset + len].to_vec(),
            });
            offset += len;
        }
        self.pending.drain(..offset);
        decoded
    }

    pub fn push(&mut self, input: &[u8]) {
        self.pending.extend_from_slice(input);
    }

    pub fn next_key(&mut self) -> Option<Decoded> {
        if self.pending.is_empty() {
            return None;
        }
        let (key, len) = decode(&self.pending)?;
        let raw = self.pending.drain(..len).collect();
        Some(Decoded { key, raw })
    }

    pub fn take_pending(&mut self) -> Vec<u8> {
        mem::take(&mut self.pending)
    }

    pub fn is_partial(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn time_out(&mut self) -> Option<Decoded> {
        let raw = mem::take(&mut self.pending);
        let key = match raw.as_slice() {
            [ESC] => Key::from(KeyCode::Escape),
            [ESC, ESC] => Key::new(KeyCode::Escape, Mods::ALT),
            _ => return None,
        };
        Some(Decoded {
            key: Some(key),
            raw,
        })
    }
}

type Step = (Option<Key>, usize);

fn decode(input: &[u8]) -> Option<Step> {
    match input[0] {
        ESC => decode_escape(input),
        _ => decode_plain(input),
    }
}

fn decode_plain(input: &[u8]) -> Option<Step> {
    let code = match input[0] {
        b'\r' | b'\n' => KeyCode::Enter,
        b'\t' => KeyCode::Tab,
        0x7f | 0x08 => KeyCode::Backspace,
        byte @ 0x00..=0x1f => {
            return Some((Some(Key::ctrl(char::from(byte | 0x40))), 1));
        }
        _ => return decode_utf8(input),
    };
    Some((Some(Key::from(code)), 1))
}

fn decode_escape(input: &[u8]) -> Option<Step> {
    match *input.get(1)? {
        b'[' => decode_csi(input),
        b'O' => decode_ss3(input),
        ESC => match *input.get(2)? {
            b'[' | b'O' => decode_escape(&input[1..]).map(with_meta),
            _ => Some((Some(Key::new(KeyCode::Escape, Mods::ALT)), 2)),
        },
        _ => decode_plain(&input[1..]).map(with_meta),
    }
}

fn with_meta((key, len): Step) -> Step {
    match key {
        Some(key) => (Some(key.with(Mods::ALT)), len + 1),
        None => (Some(Key::from(KeyCode::Escape)), 1),
    }
}

enum Sequence<'a> {
    Complete {
        params: &'a [u8],
        last: u8,
        len: usize,
    },
    Broken(usize),
}

fn scan_sequence(input: &[u8]) -> Option<Sequence<'_>> {
    for (offset, &byte) in input.iter().enumerate().skip(2) {
        match byte {
            0x20..=0x3f => {}
            0x40..=0x7e => {
                return Some(Sequence::Complete {
                    params: &input[2..offset],
                    last: byte,
                    len: offset + 1,
                })
            }
            _ => return Some(Sequence::Broken(offset)),
        }
    }
    (input.len() >= MAX_SEQUENCE_LEN).then_some(Sequence::Broken(input.len()))
}

fn decode_csi(input: &[u8]) -> Option<Step> {
    Some(match scan_sequence(input)? {
        Sequence::Complete {
            params: [],
            last: b'M',
            len,
        } => {
            let end = len + X10_MOUSE_PAYLOAD_LEN;
            return (input.len() >= end).then_some((None, end));
        }
        Sequence::Complete { params, last, len } => (csi_key(params, last), len),
        Sequence::Broken(len) => (None, len),
    })
}

fn decode_ss3(input: &[u8]) -> Option<Step> {
    Some(match scan_sequence(input)? {
        Sequence::Complete { params, last, len } => (ss3_key(params, last), len),
        Sequence::Broken(len) => (None, len),
    })
}

fn csi_key(params: &[u8], last: u8) -> Option<Key> {
    match last {
        b'~' => tilde_key(params),
        b'Z' if params.is_empty() => Some(Key::from(KeyCode::BackTab)),
        _ => {
            let mods = match params {
                [] => Mods::NONE,
                _ => xterm_mods(params.strip_prefix(b"1;")?)?,
            };
            Some(Key::new(letter_code(last)?, mods))
        }
    }
}

fn ss3_key(params: &[u8], last: u8) -> Option<Key> {
    let mods = match params {
        [] => Mods::NONE,
        _ => xterm_mods(params)?,
    };
    let code = match last {
        b'M' => KeyCode::Enter,
        _ => letter_code(last)?,
    };
    Some(Key::new(code, mods))
}

fn letter_code(last: u8) -> Option<KeyCode> {
    Some(match last {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P'..=b'S' => KeyCode::F(last - b'O'),
        _ => return None,
    })
}

fn tilde_key(params: &[u8]) -> Option<Key> {
    let (number, mods) = match params.iter().position(|&byte| byte == b';') {
        Some(split) => (&params[..split], xterm_mods(&params[split + 1..])?),
        None => (params, Mods::NONE),
    };
    let number = decimal(number)?;
    let mut function_keys = FUNCTION_KEYS;
    let code = match number {
        1 | 7 => KeyCode::Home,
        2 => KeyCode::Insert,
        3 => KeyCode::Delete,
        4 | 8 => KeyCode::End,
        5 => KeyCode::PageUp,
        6 => KeyCode::PageDown,
        _ => KeyCode::F(function_keys.find(|&key| function_tilde(key) == Some(number))?),
    };
    Some(Key::new(code, mods))
}

fn xterm_mods(parameter: &[u8]) -> Option<Mods> {
    Mods::from_xterm(decimal(parameter)?)
}

fn decimal(digits: &[u8]) -> Option<u8> {
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn decode_utf8(input: &[u8]) -> Option<Step> {
    let len = match input[0] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some((None, 1)),
    };
    let Some(bytes) = input.get(..len) else {
        let valid_so_far = input[1..].iter().all(|&byte| is_continuation(byte));
        return (!valid_so_far).then_some((None, 1));
    };
    match std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.chars().next())
    {
        Some(character) if !character.is_control() => Some((Some(Key::char(character)), len)),
        Some(_) => Some((None, len)),
        None => Some((None, 1)),
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNKNOWN: Option<Key> = None;

    fn key(notation: &str) -> Option<Key> {
        Some(notation.parse().unwrap())
    }

    fn fed(decoder: &mut KeyDecoder, input: &[u8]) -> Vec<Option<Key>> {
        decoder
            .feed(input)
            .into_iter()
            .map(|decoded| decoded.key)
            .collect()
    }

    fn keys(input: &[u8]) -> Vec<Option<Key>> {
        fed(&mut KeyDecoder::default(), input)
    }

    fn timed_out(decoder: &mut KeyDecoder) -> Option<Key> {
        decoder.time_out().and_then(|decoded| decoded.key)
    }

    #[test]
    fn printable_text_becomes_characters() {
        assert_eq!(keys("aé日".as_bytes()), vec![key("a"), key("é"), key("日")]);
    }

    #[test]
    fn control_bytes_become_named_keys() {
        assert_eq!(
            keys(b"\r\n\t\x7f\x08\x01\x05\x15"),
            vec![
                key("Enter"),
                key("Enter"),
                key("Tab"),
                key("Backspace"),
                key("Backspace"),
                key("C-a"),
                key("C-e"),
                key("C-u"),
            ]
        );
    }

    #[test]
    fn cursor_keys_decode_in_normal_and_application_mode() {
        assert_eq!(
            keys(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1bOA\x1bOB\x1bOC\x1bOD"),
            vec![
                key("Up"),
                key("Down"),
                key("Right"),
                key("Left"),
                key("Up"),
                key("Down"),
                key("Right"),
                key("Left"),
            ]
        );
        assert_eq!(
            keys(b"\x1b[H\x1b[F\x1bOH\x1bOF\x1b[1~\x1b[4~\x1b[7~\x1b[8~\x1b[3~\x1b[1;5D"),
            vec![
                key("Home"),
                key("End"),
                key("Home"),
                key("End"),
                key("Home"),
                key("End"),
                key("Home"),
                key("End"),
                key("Delete"),
                key("C-Left"),
            ]
        );
    }

    #[test]
    fn unknown_sequences_are_consumed_whole() {
        assert_eq!(
            keys(b"\x1b[15~\x1b[<0;10;5M\x1b[M !!x"),
            vec![key("F5"), UNKNOWN, UNKNOWN, key("x")]
        );
    }

    #[test]
    fn sequences_split_across_chunks_wait_for_the_rest() {
        let mut decoder = KeyDecoder::default();
        assert_eq!(fed(&mut decoder, b"a\x1b"), vec![key("a")]);
        assert!(decoder.is_partial());
        assert_eq!(fed(&mut decoder, b"["), vec![]);
        assert_eq!(fed(&mut decoder, b"1;5"), vec![]);
        assert_eq!(fed(&mut decoder, b"Cb"), vec![key("C-Right"), key("b")]);
        assert!(!decoder.is_partial());

        assert_eq!(fed(&mut decoder, &[0xe6, 0x97]), vec![]);
        assert_eq!(fed(&mut decoder, &[0xa5]), vec![key("日")]);
    }

    #[test]
    fn a_lone_escape_is_resolved_by_the_timeout() {
        let mut decoder = KeyDecoder::default();
        assert_eq!(fed(&mut decoder, b"\x1b"), vec![]);
        assert_eq!(timed_out(&mut decoder), key("Escape"));
        assert!(!decoder.is_partial());
        assert_eq!(timed_out(&mut decoder), None);

        assert_eq!(fed(&mut decoder, b"\x1b["), vec![]);
        assert_eq!(timed_out(&mut decoder), None);
        assert_eq!(fed(&mut decoder, b"D"), vec![key("D")]);
    }

    #[test]
    fn escape_followed_by_another_key_is_a_meta_key() {
        assert_eq!(keys(b"\x1bq"), vec![key("M-q")]);
        assert_eq!(keys(b"\x1b\x1b[A"), vec![key("M-Up")]);
    }

    #[test]
    fn invalid_utf8_is_dropped() {
        assert_eq!(
            keys(&[0xff, b'a', 0xc3, b'b', 0x80]),
            vec![UNKNOWN, key("a"), UNKNOWN, key("b"), UNKNOWN]
        );
    }

    #[test]
    fn modifiers_are_kept() {
        assert_eq!(
            keys(b"\x1b[1;2A\x1b[1;3B\x1b[3;5~\x1bO5C\x1b\x02\x1b\r\x1b[Z\x00\x1c"),
            vec![
                key("S-Up"),
                key("M-Down"),
                key("C-Delete"),
                key("C-Right"),
                key("M-C-b"),
                key("M-Enter"),
                key("BackTab"),
                key("C-Space"),
                key("C-\\"),
            ]
        );
        assert_eq!(
            keys(b"\x1bOP\x1b[1;5P\x1b[24~\x1b[24;2~\x1b[5~\x1b[6~\x1b[2~"),
            vec![
                key("F1"),
                key("C-F1"),
                key("F12"),
                key("S-F12"),
                key("PageUp"),
                key("PageDown"),
                key("Insert"),
            ]
        );
    }

    #[test]
    fn parameters_a_terminal_never_sends_are_unknown() {
        assert_eq!(
            keys(b"\x1b[2A\x1b[1;9A\x1b[99~\x1b[200~"),
            vec![UNKNOWN, UNKNOWN, UNKNOWN, UNKNOWN]
        );
    }

    #[test]
    fn every_encoding_decodes_back_to_its_key() {
        for notation in [
            "a",
            "%",
            "Space",
            "日",
            "C-b",
            "C-Space",
            "C-\\",
            "M-a",
            "M-C-b",
            "M-é",
            "Enter",
            "M-Enter",
            "Tab",
            "BackTab",
            "Backspace",
            "Up",
            "M-Up",
            "C-Left",
            "M-S-Up",
            "Home",
            "C-End",
            "Insert",
            "Delete",
            "C-Delete",
            "PageUp",
            "PageDown",
            "F1",
            "C-F4",
            "F5",
            "F12",
            "S-F12",
            "M-F12",
        ] {
            let expected = notation.parse::<Key>().unwrap();
            for encoding in expected.encodings() {
                let decoded = KeyDecoder::default().feed(&encoding);
                assert_eq!(
                    decoded,
                    vec![Decoded {
                        key: Some(expected),
                        raw: encoding.clone(),
                    }],
                    "{notation} as {encoding:?}"
                );
            }
        }
    }

    #[test]
    fn keys_come_with_their_raw_bytes() {
        let raws: Vec<Vec<u8>> = KeyDecoder::default()
            .feed(b"a\x1b[1;5D\x1b[M !!\x1bx")
            .into_iter()
            .map(|decoded| decoded.raw)
            .collect();
        assert_eq!(
            raws,
            vec![
                b"a".to_vec(),
                b"\x1b[1;5D".to_vec(),
                b"\x1b[M !!".to_vec(),
                b"\x1bx".to_vec(),
            ]
        );
    }

    #[test]
    fn a_double_escape_times_out_as_meta_escape() {
        let mut decoder = KeyDecoder::default();
        assert_eq!(fed(&mut decoder, b"\x1b\x1b"), vec![]);
        assert_eq!(timed_out(&mut decoder), key("M-Escape"));
        assert_eq!(keys(b"\x1b\x1bx"), vec![key("M-Escape"), key("x")]);
    }

    #[test]
    fn next_key_decodes_one_key_and_leaves_the_rest() {
        let mut decoder = KeyDecoder::default();
        decoder.push(b"\x1b[Cls");
        assert_eq!(
            decoder.next_key().and_then(|decoded| decoded.key),
            key("Right")
        );
        assert_eq!(decoder.take_pending(), b"ls");
        assert_eq!(decoder.next_key(), None);

        decoder.push(b"\x1b[1;");
        assert_eq!(decoder.next_key(), None);
        assert!(decoder.is_partial());
    }

    #[test]
    fn meta_keys_split_into_escape_and_the_plain_key() {
        let split = |input: &[u8]| {
            KeyDecoder::default().feed(input)[0]
                .split_escape()
                .map(|(escape, rest)| (escape.key, escape.raw, rest.key, rest.raw))
        };
        assert_eq!(
            split(b"\x1bx"),
            Some((key("Escape"), b"\x1b".to_vec(), key("x"), b"x".to_vec()))
        );
        assert_eq!(
            split(b"\x1b\x1b[A"),
            Some((
                key("Escape"),
                b"\x1b".to_vec(),
                key("Up"),
                b"\x1b[A".to_vec()
            ))
        );
        assert_eq!(split(b"\x1b[1;3A"), None);
        assert_eq!(split(b"x"), None);
    }

    #[test]
    fn only_x10_mouse_reports_have_a_payload() {
        let decoded = KeyDecoder::default().feed(b"\x1b[M !#\x1b[<0;3;4M");
        assert_eq!(decoded[0].mouse_payload(), Some(&b" !#"[..]));
        assert_eq!(decoded[1].mouse_payload(), None);
    }
}
