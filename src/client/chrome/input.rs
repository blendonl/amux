use std::time::Duration;

pub const ESCAPE_TIMEOUT: Duration = Duration::from_millis(50);

const ESC: u8 = 0x1b;
const MAX_SEQUENCE_LEN: usize = 32;
const X10_MOUSE_LEN: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Unknown,
}

#[derive(Debug, Default)]
pub struct KeyDecoder {
    pending: Vec<u8>,
}

impl KeyDecoder {
    pub fn feed(&mut self, input: &[u8]) -> Vec<Key> {
        self.pending.extend_from_slice(input);
        let mut keys = Vec::new();
        let mut offset = 0;
        while offset < self.pending.len() {
            let Some((key, len)) = decode(&self.pending[offset..]) else {
                break;
            };
            keys.push(key);
            offset += len;
        }
        self.pending.drain(..offset);
        keys
    }

    pub fn is_partial(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn time_out(&mut self) -> Option<Key> {
        (std::mem::take(&mut self.pending) == [ESC]).then_some(Key::Escape)
    }
}

fn decode(input: &[u8]) -> Option<(Key, usize)> {
    match input[0] {
        ESC => decode_escape(input),
        b'\r' | b'\n' => Some((Key::Enter, 1)),
        b'\t' => Some((Key::Tab, 1)),
        0x7f | 0x08 => Some((Key::Backspace, 1)),
        byte @ 0x00..=0x1f => Some((Key::Ctrl(char::from(byte | 0x40).to_ascii_lowercase()), 1)),
        _ => decode_utf8(input),
    }
}

fn decode_escape(input: &[u8]) -> Option<(Key, usize)> {
    match *input.get(1)? {
        b'[' => decode_csi(input),
        b'O' => match *input.get(2)? {
            byte @ 0x40..=0x7e => Some((ss3_key(byte), 3)),
            _ => Some((Key::Escape, 1)),
        },
        _ => Some((Key::Escape, 1)),
    }
}

fn decode_csi(input: &[u8]) -> Option<(Key, usize)> {
    for (offset, &byte) in input.iter().enumerate().skip(2) {
        match byte {
            0x20..=0x3f => {}
            0x40..=0x7e => {
                let params = &input[2..offset];
                if byte == b'M' && params.is_empty() {
                    let end = offset + 1 + X10_MOUSE_LEN;
                    return (input.len() >= end).then_some((Key::Unknown, end));
                }
                return Some((csi_key(params, byte), offset + 1));
            }
            _ => return Some((Key::Unknown, offset)),
        }
    }
    (input.len() >= MAX_SEQUENCE_LEN).then_some((Key::Unknown, input.len()))
}

fn csi_key(params: &[u8], final_byte: u8) -> Key {
    match final_byte {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'~' => match params.split(|&byte| byte == b';').next() {
            Some(b"1" | b"7") => Key::Home,
            Some(b"4" | b"8") => Key::End,
            Some(b"3") => Key::Delete,
            _ => Key::Unknown,
        },
        _ => Key::Unknown,
    }
}

fn ss3_key(final_byte: u8) -> Key {
    match final_byte {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'M' => Key::Enter,
        _ => Key::Unknown,
    }
}

fn decode_utf8(input: &[u8]) -> Option<(Key, usize)> {
    let len = match input[0] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some((Key::Unknown, 1)),
    };
    let Some(bytes) = input.get(..len) else {
        let valid_so_far = input[1..].iter().all(|&byte| is_continuation(byte));
        return (!valid_so_far).then_some((Key::Unknown, 1));
    };
    match std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| text.chars().next())
    {
        Some(character) if !character.is_control() => Some((Key::Char(character), len)),
        Some(_) => Some((Key::Unknown, len)),
        None => Some((Key::Unknown, 1)),
    }
}

fn is_continuation(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(input: &[u8]) -> Vec<Key> {
        KeyDecoder::default().feed(input)
    }

    #[test]
    fn printable_text_becomes_characters() {
        assert_eq!(
            keys("aé日".as_bytes()),
            vec![Key::Char('a'), Key::Char('é'), Key::Char('日')]
        );
    }

    #[test]
    fn control_bytes_become_named_keys() {
        assert_eq!(
            keys(b"\r\n\t\x7f\x08\x01\x05\x15"),
            vec![
                Key::Enter,
                Key::Enter,
                Key::Tab,
                Key::Backspace,
                Key::Backspace,
                Key::Ctrl('a'),
                Key::Ctrl('e'),
                Key::Ctrl('u'),
            ]
        );
    }

    #[test]
    fn cursor_keys_decode_in_normal_and_application_mode() {
        assert_eq!(
            keys(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1bOA\x1bOB\x1bOC\x1bOD"),
            vec![
                Key::Up,
                Key::Down,
                Key::Right,
                Key::Left,
                Key::Up,
                Key::Down,
                Key::Right,
                Key::Left,
            ]
        );
        assert_eq!(
            keys(b"\x1b[H\x1b[F\x1bOH\x1bOF\x1b[1~\x1b[4~\x1b[7~\x1b[8~\x1b[3~\x1b[1;5D"),
            vec![
                Key::Home,
                Key::End,
                Key::Home,
                Key::End,
                Key::Home,
                Key::End,
                Key::Home,
                Key::End,
                Key::Delete,
                Key::Left,
            ]
        );
    }

    #[test]
    fn unknown_sequences_are_consumed_whole() {
        assert_eq!(
            keys(b"\x1b[15~\x1b[<0;10;5M\x1b[M !!x"),
            vec![Key::Unknown, Key::Unknown, Key::Unknown, Key::Char('x')]
        );
    }

    #[test]
    fn sequences_split_across_chunks_wait_for_the_rest() {
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.feed(b"a\x1b"), vec![Key::Char('a')]);
        assert!(decoder.is_partial());
        assert_eq!(decoder.feed(b"["), vec![]);
        assert_eq!(decoder.feed(b"1;5"), vec![]);
        assert_eq!(decoder.feed(b"Cb"), vec![Key::Right, Key::Char('b')]);
        assert!(!decoder.is_partial());

        assert_eq!(decoder.feed(&[0xe6, 0x97]), vec![]);
        assert_eq!(decoder.feed(&[0xa5]), vec![Key::Char('日')]);
    }

    #[test]
    fn a_lone_escape_is_resolved_by_the_timeout() {
        let mut decoder = KeyDecoder::default();
        assert_eq!(decoder.feed(b"\x1b"), vec![]);
        assert_eq!(decoder.time_out(), Some(Key::Escape));
        assert!(!decoder.is_partial());
        assert_eq!(decoder.time_out(), None);

        assert_eq!(decoder.feed(b"\x1b["), vec![]);
        assert_eq!(decoder.time_out(), None);
        assert_eq!(decoder.feed(b"D"), vec![Key::Char('D')]);
    }

    #[test]
    fn escape_followed_by_another_key_is_an_escape() {
        assert_eq!(keys(b"\x1bq"), vec![Key::Escape, Key::Char('q')]);
        assert_eq!(keys(b"\x1b\x1b[A"), vec![Key::Escape, Key::Up]);
    }

    #[test]
    fn invalid_utf8_is_dropped() {
        assert_eq!(
            keys(&[0xff, b'a', 0xc3, b'b', 0x80]),
            vec![
                Key::Unknown,
                Key::Char('a'),
                Key::Unknown,
                Key::Char('b'),
                Key::Unknown
            ]
        );
    }
}
