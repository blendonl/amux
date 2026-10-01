use std::mem;

use memchr::{memchr, memchr3};

pub const MAX_BODY_LEN: usize = 4 * 1024 * 1024;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const APC: u8 = b'_';
const KITTY: u8 = b'G';
const ST: u8 = b'\\';
const ESCAPE: &[u8] = b"\x1b";
const OPEN: &[u8] = b"\x1b_";
const CLOSE: &[u8] = b"\x1b\\";

#[derive(Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a [u8]),
    Graphics(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Introducer,
    Body,
    BodyEscape,
    Closing,
}

#[derive(Debug, Default)]
pub struct ApcScanner {
    state: State,
    body: Vec<u8>,
    overflowed: bool,
}

impl ApcScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn split<'s, 'a>(&'s mut self, chunk: &'a [u8]) -> Segments<'s, 'a> {
        Segments {
            scanner: self,
            chunk,
            position: 0,
        }
    }

    fn open(&mut self) {
        self.state = State::Body;
        self.body.clear();
        self.overflowed = false;
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.overflowed {
            return;
        }
        if self.body.len() + bytes.len() > MAX_BODY_LEN {
            self.overflowed = true;
            self.body = Vec::new();
            return;
        }
        self.body.extend_from_slice(bytes);
    }

    fn abort(&mut self, state: State) {
        self.state = state;
        self.body = Vec::new();
        self.overflowed = false;
    }

    fn finish(&mut self, state: State) -> Option<Vec<u8>> {
        self.state = state;
        let body = mem::take(&mut self.body);
        (!mem::take(&mut self.overflowed)).then_some(body)
    }
}

pub struct Segments<'s, 'a> {
    scanner: &'s mut ApcScanner,
    chunk: &'a [u8],
    position: usize,
}

impl<'a> Segments<'_, 'a> {
    fn step(&mut self) -> Option<Segment<'a>> {
        let byte = self.chunk[self.position];
        match self.scanner.state {
            State::Ground => self.ground(),
            State::Escape if byte == APC => {
                self.position += 1;
                self.scanner.state = State::Introducer;
                None
            }
            State::Escape => {
                self.scanner.state = State::Ground;
                Some(Segment::Text(ESCAPE))
            }
            State::Introducer => {
                if byte == KITTY {
                    self.position += 1;
                    self.scanner.open();
                } else {
                    self.scanner.state = State::Ground;
                }
                Some(Segment::Text(OPEN))
            }
            State::Body => self.body(),
            State::BodyEscape if byte == ST => {
                self.position += 1;
                match self.scanner.finish(State::Closing) {
                    Some(body) => Some(Segment::Graphics(body)),
                    None => {
                        self.scanner.state = State::Ground;
                        Some(Segment::Text(CLOSE))
                    }
                }
            }
            State::BodyEscape => {
                self.scanner.abort(State::Escape);
                None
            }
            State::Closing => {
                self.scanner.state = State::Ground;
                Some(Segment::Text(CLOSE))
            }
        }
    }

    fn ground(&mut self) -> Option<Segment<'a>> {
        let start = self.position;
        let mut search = start;
        loop {
            let Some(found) = memchr(ESC, &self.chunk[search..]) else {
                self.position = self.chunk.len();
                return Some(Segment::Text(&self.chunk[start..]));
            };
            let escape = search + found;
            let held = match (self.chunk.get(escape + 1), self.chunk.get(escape + 2)) {
                (Some(&APC), Some(&KITTY)) => {
                    self.position = escape + 3;
                    self.scanner.open();
                    return Some(Segment::Text(&self.chunk[start..escape + 2]));
                }
                (Some(&APC), Some(_)) => {
                    search = escape + 2;
                    continue;
                }
                (Some(&APC), None) => State::Introducer,
                (Some(_), _) => {
                    search = escape + 1;
                    continue;
                }
                (None, _) => State::Escape,
            };
            self.scanner.state = held;
            self.position = self.chunk.len();
            return (escape > start).then(|| Segment::Text(&self.chunk[start..escape]));
        }
    }

    fn body(&mut self) -> Option<Segment<'a>> {
        let rest = &self.chunk[self.position..];
        let Some(found) = memchr3(ESC, CAN, SUB, rest) else {
            self.scanner.push(rest);
            self.position = self.chunk.len();
            return None;
        };
        self.scanner.push(&rest[..found]);
        let at = self.position + found;
        if rest[found] != ESC {
            self.scanner.abort(State::Ground);
            self.position = at;
            return None;
        }
        match self.chunk.get(at + 1) {
            None => {
                self.scanner.state = State::BodyEscape;
                self.position = at + 1;
                None
            }
            Some(&ST) => {
                self.position = at;
                self.scanner.finish(State::Ground).map(Segment::Graphics)
            }
            Some(_) => {
                self.scanner.abort(State::Ground);
                self.position = at;
                None
            }
        }
    }
}

impl<'a> Iterator for Segments<'_, 'a> {
    type Item = Segment<'a>;

    fn next(&mut self) -> Option<Segment<'a>> {
        if self.scanner.state == State::Closing {
            self.scanner.state = State::Ground;
            return Some(Segment::Text(CLOSE));
        }
        while self.position < self.chunk.len() {
            if let Some(segment) = self.step() {
                return Some(segment);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Text(Vec<u8>),
        Graphics(Vec<u8>),
    }

    fn text(bytes: &[u8]) -> Event {
        Event::Text(bytes.to_vec())
    }

    fn graphics(bytes: &[u8]) -> Event {
        Event::Graphics(bytes.to_vec())
    }

    fn record(scanner: &mut ApcScanner, chunk: &[u8], events: &mut Vec<Event>) {
        for segment in scanner.split(chunk) {
            match (segment, events.last_mut()) {
                (Segment::Text(bytes), Some(Event::Text(last))) => last.extend_from_slice(bytes),
                (Segment::Text(bytes), _) => events.push(text(bytes)),
                (Segment::Graphics(body), _) => events.push(Event::Graphics(body)),
            }
        }
    }

    fn scan(chunks: &[&[u8]]) -> Vec<Event> {
        let mut scanner = ApcScanner::new();
        let mut events = Vec::new();
        for chunk in chunks {
            record(&mut scanner, chunk, &mut events);
        }
        events
    }

    fn assert_every_split(stream: &[u8], expected: &[Event]) {
        assert_eq!(scan(&[stream]), expected, "whole {stream:?}");
        for first in 0..=stream.len() {
            let (head, tail) = stream.split_at(first);
            assert_eq!(
                scan(&[head, tail]),
                expected,
                "split at {first} of {stream:?}"
            );
            for second in first..=stream.len() {
                let (middle, last) = tail.split_at(second - first);
                assert_eq!(
                    scan(&[head, middle, last]),
                    expected,
                    "split at {first} and {second} of {stream:?}"
                );
            }
        }
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(scan(&bytes), expected, "byte at a time {stream:?}");
    }

    #[test]
    fn a_kitty_apc_is_cut_out_and_its_framing_passed_through() {
        assert_every_split(
            b"ab\x1b_Ga=t,i=1;AAAA\x1b\\cd",
            &[
                text(b"ab\x1b_"),
                graphics(b"a=t,i=1;AAAA"),
                text(b"\x1b\\cd"),
            ],
        );
    }

    #[test]
    fn back_to_back_commands_come_out_in_order_with_the_text_between() {
        assert_every_split(
            b"\x1b_Gi=1\x1b\\x\x1b_G\x1b\\\x1b_Gi=3\x1b\\",
            &[
                text(b"\x1b_"),
                graphics(b"i=1"),
                text(b"\x1b\\x\x1b_"),
                graphics(b""),
                text(b"\x1b\\\x1b_"),
                graphics(b"i=3"),
                text(b"\x1b\\"),
            ],
        );
    }

    #[test]
    fn other_apcs_and_escape_sequences_pass_through_untouched() {
        let stream = b"\x1b_Xnot kitty\x1b\\\x1b[31mred\x1b]0;t\x07\x1bP1$r\x1b\\\x1b_\x1b\\";
        assert_every_split(stream, &[text(stream)]);
    }

    #[test]
    fn an_unterminated_osc_before_a_kitty_apc_reaches_vt100_first() {
        assert_every_split(
            b"\x1b]0;title\x1b_Gi=7;x\x1b\\",
            &[
                text(b"\x1b]0;title\x1b_"),
                graphics(b"i=7;x"),
                text(b"\x1b\\"),
            ],
        );
    }

    #[test]
    fn can_and_sub_abort_the_apc_and_reach_vt100() {
        for abort in [b'\x18', b'\x1a'] {
            let stream = [b"\x1b_Ga=t;AA".as_slice(), &[abort], b"after"].concat();
            let expected = [b"\x1b_".as_slice(), &[abort], b"after"].concat();
            assert_every_split(&stream, &[text(&expected)]);
        }
    }

    #[test]
    fn an_escape_that_does_not_end_the_apc_aborts_it_and_starts_a_new_sequence() {
        assert_every_split(b"\x1b_Ga=t;AA\x1b[1mbold", &[text(b"\x1b_\x1b[1mbold")]);
        assert_every_split(
            b"\x1b_Ga=t;AA\x1b_Gi=2\x1b\\",
            &[text(b"\x1b_\x1b_"), graphics(b"i=2"), text(b"\x1b\\")],
        );
        assert_every_split(
            b"\x1b\x1b_Gi=4\x1b\\",
            &[text(b"\x1b\x1b_"), graphics(b"i=4"), text(b"\x1b\\")],
        );
    }

    #[test]
    fn a_trailing_escape_is_held_until_the_next_read() {
        let mut scanner = ApcScanner::new();
        let first: Vec<Segment<'_>> = scanner.split(b"abc\x1b").collect();
        assert_eq!(first, [Segment::Text(b"abc")]);
        let second: Vec<Segment<'_>> = scanner.split(b"[1m").collect();
        assert_eq!(second, [Segment::Text(b"\x1b"), Segment::Text(b"[1m")]);

        let held: Vec<Segment<'_>> = scanner.split(b"\x1b_").collect();
        assert!(held.is_empty(), "{held:?}");
        let opened: Vec<Segment<'_>> = scanner.split(b"Gi=1\x1b").collect();
        assert_eq!(opened, [Segment::Text(b"\x1b_")]);
        let closed: Vec<Segment<'_>> = scanner.split(b"\\").collect();
        assert_eq!(
            closed,
            [Segment::Graphics(b"i=1".to_vec()), Segment::Text(b"\x1b\\")]
        );
        assert_every_split(b"abc\x1b[1mx\x1b", &[text(b"abc\x1b[1mx")]);
    }

    #[test]
    fn a_body_over_the_cap_is_discarded_up_to_its_terminator() {
        let fits = [b"\x1b_G".as_slice(), &vec![b'A'; MAX_BODY_LEN], b"\x1b\\"].concat();
        let events = scan(&[&fits]);
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[1], Event::Graphics(body) if body.len() == MAX_BODY_LEN));

        let stream = [
            b"\x1b_G".as_slice(),
            &vec![b'A'; MAX_BODY_LEN + 1],
            b"\x1b\\x\x1b_Gi=1\x1b\\",
        ]
        .concat();
        let expected = [
            text(b"\x1b_\x1b\\x\x1b_"),
            graphics(b"i=1"),
            text(b"\x1b\\"),
        ];
        let tail = stream.len() - b"\x1b\\x\x1b_Gi=1\x1b\\".len();
        for split in [0, 1, 3, 4, MAX_BODY_LEN, tail, tail + 1, tail + 2] {
            let (head, rest) = stream.split_at(split);
            assert_eq!(scan(&[head, rest]), expected, "split at {split}");
        }
        let pieces: Vec<&[u8]> = stream.chunks(64 * 1024 + 1).collect();
        assert_eq!(scan(&pieces), expected);
    }

    #[test]
    fn vt100_ends_up_with_the_same_screen_once_the_bodies_are_cut_out() {
        let streams: [&[u8]; 5] = [
            b"a\x1b]0;title\x1b_Gi=7;x\x1b\\b",
            b"a\x1b_Ga=t;AA\x18b\x1b[1mc",
            b"a\x1b_Ga=t;AA\x1b[31mb\x1b_Gq=2\x1b\\c",
            b"\x1bP1$r\x1b_Gi=1\x1b\\a\x1b_Xb\x1b\\c",
            b"a\x1b_G\x1b\x1b_Gi=1;\x1b\\b",
        ];
        for stream in streams {
            let mut original = vt100::Parser::new(4, 20, 0);
            original.process(stream);
            let mut scanned = vt100::Parser::new(4, 20, 0);
            let mut scanner = ApcScanner::new();
            for piece in stream.chunks(2) {
                for segment in scanner.split(piece) {
                    if let Segment::Text(text) = segment {
                        scanned.process(text);
                    }
                }
            }
            assert_eq!(
                scanned.screen().contents_formatted(),
                original.screen().contents_formatted(),
                "{stream:?}"
            );
        }
    }

    #[test]
    fn plain_text_comes_back_as_one_borrowed_slice() {
        let mut scanner = ApcScanner::new();
        for chunk in [
            b"hello world".as_slice(),
            b"\x1b[31mred\x1b[0m\x1b]0;title\x07\r\n",
        ] {
            let segments: Vec<Segment<'_>> = scanner.split(chunk).collect();
            assert_eq!(segments.len(), 1);
            let Segment::Text(text) = segments[0] else {
                panic!("{segments:?}");
            };
            assert!(std::ptr::eq(text, chunk));
        }
    }

    #[test]
    fn a_mebibyte_of_output_passes_as_a_single_borrowed_slice() {
        let chunk: Vec<u8> = b"0123456789 \x1b[1mabc\x1b[0m\r\n"
            .iter()
            .copied()
            .cycle()
            .take(1024 * 1024)
            .collect();
        let mut scanner = ApcScanner::new();
        let mut segments = scanner.split(&chunk);
        let Some(Segment::Text(text)) = segments.next() else {
            panic!("expected text");
        };
        assert!(std::ptr::eq(text, chunk.as_slice()));
        assert_eq!(segments.next(), None);
    }
}
