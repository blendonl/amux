use std::collections::VecDeque;
use std::mem;

use memchr::{memchr, memchr3};

pub const MAX_BODY_LEN: usize = 4 * 1024 * 1024;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const APC: u8 = b'_';
const DCS: u8 = b'P';
const KITTY: u8 = b'G';
const ST: u8 = b'\\';
const ESCAPE: &[u8] = b"\x1b";
const OPEN: &[u8] = b"\x1b_";
const CLOSE: &[u8] = b"\x1b\\";
const TMUX: &[u8] = b"\x1bPtmux;";

#[derive(Debug, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a [u8]),
    Unwrapped(Vec<u8>),
    Graphics(Vec<u8>),
}

impl Segment<'_> {
    fn into_owned(self) -> Segment<'static> {
        match self {
            Segment::Text(text) => Segment::Unwrapped(text.to_vec()),
            Segment::Unwrapped(text) => Segment::Unwrapped(text),
            Segment::Graphics(body) => Segment::Graphics(body),
        }
    }
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
    Wrapper(usize),
    Passthrough,
    PassthroughEscape,
}

#[derive(Debug)]
pub struct ApcScanner {
    state: State,
    body: Vec<u8>,
    discarding: bool,
    passthrough: Option<Box<Passthrough>>,
}

#[derive(Debug)]
struct Passthrough {
    scanner: ApcScanner,
    unwrapped: VecDeque<Segment<'static>>,
}

impl Passthrough {
    fn rescan(&mut self, body: &[u8]) {
        self.unwrapped
            .extend(self.scanner.split(body).map(Segment::into_owned));
    }
}

impl ApcScanner {
    pub fn new() -> Self {
        Self {
            passthrough: Some(Box::new(Passthrough {
                scanner: Self::nested(),
                unwrapped: VecDeque::new(),
            })),
            ..Self::nested()
        }
    }

    fn nested() -> Self {
        Self {
            state: State::Ground,
            body: Vec::new(),
            discarding: false,
            passthrough: None,
        }
    }

    pub fn split<'s, 'a>(&'s mut self, chunk: &'a [u8]) -> Segments<'s, 'a> {
        Segments {
            scanner: self,
            chunk,
            position: 0,
        }
    }

    fn open(&mut self, state: State) {
        self.state = state;
        self.body.clear();
        self.discarding = false;
    }

    fn wrap(&mut self) {
        self.open(State::Passthrough);
        self.discarding = self.passthrough.is_none();
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.discarding {
            return;
        }
        if self.body.len() + bytes.len() > MAX_BODY_LEN {
            self.discarding = true;
            self.body = Vec::new();
            return;
        }
        self.body.extend_from_slice(bytes);
    }

    fn abort(&mut self, state: State) {
        self.state = state;
        self.body = Vec::new();
        self.discarding = false;
    }

    fn finish(&mut self, state: State) -> Option<Vec<u8>> {
        self.state = state;
        let body = mem::take(&mut self.body);
        (!mem::take(&mut self.discarding)).then_some(body)
    }

    fn unwrap_passthrough(&mut self) {
        if let (Some(body), Some(passthrough)) =
            (self.finish(State::Ground), self.passthrough.as_deref_mut())
        {
            passthrough.rescan(&body);
        }
    }

    fn next_unwrapped(&mut self) -> Option<Segment<'static>> {
        self.passthrough.as_deref_mut()?.unwrapped.pop_front()
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
            State::Escape if byte == DCS => self.wrapper(ESCAPE.len()),
            State::Escape => {
                self.scanner.state = State::Ground;
                Some(Segment::Text(ESCAPE))
            }
            State::Introducer => {
                if byte == KITTY {
                    self.position += 1;
                    self.scanner.open(State::Body);
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
            State::Wrapper(matched) => self.wrapper(matched),
            State::Passthrough => self.passthrough(),
            State::PassthroughEscape if byte == ESC => {
                self.position += 1;
                self.scanner.push(ESCAPE);
                self.scanner.state = State::Passthrough;
                None
            }
            State::PassthroughEscape if byte == ST => {
                self.position += 1;
                self.scanner.unwrap_passthrough();
                self.scanner.next_unwrapped()
            }
            State::PassthroughEscape => {
                self.scanner.abort(State::Escape);
                None
            }
        }
    }

    fn wrapper(&mut self, matched: usize) -> Option<Segment<'a>> {
        if self.chunk[self.position] != TMUX[matched] {
            self.scanner.state = State::Ground;
            return Some(Segment::Text(&TMUX[..matched]));
        }
        self.position += 1;
        if matched + 1 == TMUX.len() {
            self.scanner.wrap();
        } else {
            self.scanner.state = State::Wrapper(matched + 1);
        }
        None
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
                    self.scanner.open(State::Body);
                    return Some(Segment::Text(&self.chunk[start..escape + 2]));
                }
                (Some(&APC), Some(_)) => {
                    search = escape + 2;
                    continue;
                }
                (Some(&APC), None) => State::Introducer,
                (Some(&DCS), _) => {
                    let end = self.chunk.len().min(escape + TMUX.len());
                    let wrapper = &self.chunk[escape..end];
                    if !TMUX.starts_with(wrapper) {
                        search = escape + 2;
                        continue;
                    }
                    if wrapper.len() == TMUX.len() {
                        self.position = end;
                        self.scanner.wrap();
                        return (escape > start).then(|| Segment::Text(&self.chunk[start..escape]));
                    }
                    State::Wrapper(wrapper.len())
                }
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

    fn passthrough(&mut self) -> Option<Segment<'a>> {
        let rest = &self.chunk[self.position..];
        let Some(found) = memchr3(ESC, CAN, SUB, rest) else {
            self.scanner.push(rest);
            self.position = self.chunk.len();
            return None;
        };
        self.scanner.push(&rest[..found]);
        let at = self.position + found;
        if rest[found] == ESC {
            self.scanner.state = State::PassthroughEscape;
            self.position = at + 1;
        } else {
            self.scanner.abort(State::Ground);
            self.position = at;
        }
        None
    }
}

impl<'a> Iterator for Segments<'_, 'a> {
    type Item = Segment<'a>;

    fn next(&mut self) -> Option<Segment<'a>> {
        if let Some(segment) = self.scanner.next_unwrapped() {
            return Some(segment);
        }
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

    fn wrap(sequence: &[u8]) -> Vec<u8> {
        let mut wrapped = TMUX.to_vec();
        for &byte in sequence {
            if byte == ESC {
                wrapped.push(ESC);
            }
            wrapped.push(byte);
        }
        wrapped.extend_from_slice(CLOSE);
        wrapped
    }

    fn record(scanner: &mut ApcScanner, chunk: &[u8], events: &mut Vec<Event>) {
        for segment in scanner.split(chunk) {
            match (segment, events.last_mut()) {
                (Segment::Text(bytes), Some(Event::Text(last))) => last.extend_from_slice(bytes),
                (Segment::Unwrapped(bytes), Some(Event::Text(last))) => {
                    last.extend_from_slice(&bytes);
                }
                (Segment::Text(bytes), _) => events.push(text(bytes)),
                (Segment::Unwrapped(bytes), _) => events.push(Event::Text(bytes)),
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
            b"\x1bPq#0;2;0;0;0#0~~-\x1b\\\x1bP$qm\x1b\\\x1bPtmux\x1b\\",
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

    #[test]
    fn a_wrapped_kitty_command_comes_out_as_if_it_were_sent_unwrapped() {
        let command = b"\x1b_Ga=T,f=100,i=1;iVBORw0K\x1b\\";
        let wrapped = b"ab\x1bPtmux;\x1b\x1b_Ga=T,f=100,i=1;iVBORw0K\x1b\x1b\\\x1b\\cd";
        assert_eq!(
            wrapped.as_slice(),
            [b"ab".as_slice(), &wrap(command), b"cd"].concat()
        );
        let expected = [
            text(b"ab\x1b_"),
            graphics(b"a=T,f=100,i=1;iVBORw0K"),
            text(b"\x1b\\cd"),
        ];
        assert_eq!(
            scan(&[&[b"ab".as_slice(), command, b"cd"].concat()]),
            expected
        );
        assert_every_split(wrapped, &expected);
    }

    #[test]
    fn unwrapped_text_comes_back_owned_between_the_borrowed_slices() {
        let mut scanner = ApcScanner::new();
        let segments: Vec<Segment<'_>> = scanner.split(b"a\x1bPtmux;\x1b\x1b[1mb\x1b\\c").collect();
        assert_eq!(
            segments,
            [
                Segment::Text(b"a"),
                Segment::Unwrapped(b"\x1b[1mb".to_vec()),
                Segment::Text(b"c"),
            ]
        );
    }

    #[test]
    fn a_wrapped_sixel_reaches_vt100_as_the_unwrapped_dcs() {
        let sixel = b"\x1bPq\"1;1;2;6#0;2;100;0;0#0~~-\x1b\\";
        assert_every_split(
            &[b"a".as_slice(), &wrap(sixel), b"b"].concat(),
            &[text(&[b"a".as_slice(), sixel, b"b"].concat())],
        );
    }

    #[test]
    fn commands_are_unwrapped_however_they_are_spread_over_passthroughs() {
        let expected = [
            text(b"\x1b_"),
            graphics(b"a=T,i=1,m=1;AAAA"),
            text(b"\x1b\\\x1b_"),
            graphics(b"m=0;BBBB"),
            text(b"\x1b\\"),
        ];
        let first = b"\x1b_Ga=T,i=1,m=1;AAAA\x1b\\";
        let second = b"\x1b_Gm=0;BBBB\x1b\\";
        assert_every_split(&[wrap(first), wrap(second)].concat(), &expected);
        assert_every_split(&wrap(&[first.as_slice(), second].concat()), &expected);
        assert_every_split(
            &[
                wrap(b"\x1b_Ga=T,i=1,m=1;AA"),
                wrap(b"AA\x1b\\\x1b_Gm=0;BBBB\x1b"),
                wrap(b"\\"),
            ]
            .concat(),
            &expected,
        );
    }

    #[test]
    fn text_around_a_passthrough_keeps_its_bytes_and_order() {
        let stream = [
            b"one\x1b[1m".as_slice(),
            &wrap(b"two\x1b[0m"),
            b"three",
            &wrap(b"\x1b_Gi=5\x1b\\"),
            b"\x1b[2Jfour",
        ]
        .concat();
        assert_every_split(
            &stream,
            &[
                text(b"one\x1b[1mtwo\x1b[0mthree\x1b_"),
                graphics(b"i=5"),
                text(b"\x1b\\\x1b[2Jfour"),
            ],
        );
    }

    #[test]
    fn a_dcs_that_is_not_a_passthrough_passes_through_untouched() {
        for stream in [
            b"a\x1bPq#0;2;0;0;0#0~~-\x1b\\b".as_slice(),
            b"a\x1bP$qm\x1b\\b",
            b"a\x1bPtmux\x1b\\b",
            b"a\x1bPtmuxx;\x1b\\b",
            b"a\x1bPtm\x1bPtmux\x1b\\b",
        ] {
            assert_every_split(stream, &[text(stream)]);
        }
    }

    #[test]
    fn can_and_sub_abort_the_passthrough_and_reach_vt100() {
        for abort in [b'\x18', b'\x1a'] {
            let stream = [b"a\x1bPtmux;\x1b\x1b_Ga=t;AA".as_slice(), &[abort], b"b"].concat();
            let expected = [b"a".as_slice(), &[abort], b"b"].concat();
            assert_every_split(&stream, &[text(&expected)]);
        }
    }

    #[test]
    fn an_escape_that_is_not_doubled_aborts_the_passthrough_and_starts_a_new_sequence() {
        assert_every_split(b"a\x1bPtmux;\x1b\x1b_Gi=1\x1b[1mb", &[text(b"a\x1b[1mb")]);
        assert_every_split(
            b"\x1bPtmux;x\x1b_Gi=2\x1b\\",
            &[text(b"\x1b_"), graphics(b"i=2"), text(b"\x1b\\")],
        );
        assert_every_split(
            &[b"\x1bPtmux;x".as_slice(), &wrap(b"\x1b_Gi=3\x1b\\")].concat(),
            &[text(b"\x1b_"), graphics(b"i=3"), text(b"\x1b\\")],
        );
    }

    #[test]
    fn a_passthrough_inside_a_passthrough_is_dropped() {
        let nested = wrap(b"\x1b_Gi=1\x1b\\");
        assert_every_split(
            &[
                b"a".as_slice(),
                &wrap(&[b"b".as_slice(), &nested, b"c"].concat()),
                b"d",
            ]
            .concat(),
            &[text(b"abcd")],
        );
        assert_every_split(
            &wrap(&[nested.as_slice(), b"\x1b_Gi=2\x1b\\"].concat()),
            &[text(b"\x1b_"), graphics(b"i=2"), text(b"\x1b\\")],
        );
    }

    #[test]
    fn a_passthrough_over_the_cap_is_discarded_up_to_its_terminator() {
        let fits = wrap(&vec![b'A'; MAX_BODY_LEN]);
        let events = scan(&[&fits]);
        assert!(matches!(&events[..], [Event::Text(body)] if body.len() == MAX_BODY_LEN));

        let after = [b"x".as_slice(), &wrap(b"\x1b_Gi=1\x1b\\")].concat();
        let stream = [wrap(&vec![b'A'; MAX_BODY_LEN + 1]), after.clone()].concat();
        let expected = [text(b"x\x1b_"), graphics(b"i=1"), text(b"\x1b\\")];
        let tail = stream.len() - after.len();
        for split in [
            0,
            1,
            3,
            7,
            8,
            MAX_BODY_LEN,
            tail - 2,
            tail - 1,
            tail,
            tail + 1,
            tail + 2,
        ] {
            let (head, rest) = stream.split_at(split);
            assert_eq!(scan(&[head, rest]), expected, "split at {split}");
        }
        let pieces: Vec<&[u8]> = stream.chunks(64 * 1024 + 1).collect();
        assert_eq!(scan(&pieces), expected);
    }

    #[test]
    fn vt100_ends_up_with_the_screen_the_unwrapped_bytes_draw() {
        let cases: [(&[u8], &[u8], &[u8]); 4] = [
            (b"a", b"\x1b[31mred\x1b[0m", b"b"),
            (b"\x1b[2;3H", b"\x1b_Gi=1;x\x1b\\x\x1b_Xy\x1b\\", b"z"),
            (b"a", b"\x1bPq#0~-\x1b\\b", b"\x1b[1mc"),
            (b"\x1b]0;title", b"\x1b[3Cx", b"y"),
        ];
        for (before, inner, after) in cases {
            let mut original = vt100::Parser::new(4, 20, 0);
            original.process(&[before, inner, after].concat());
            let stream = [before, &wrap(inner), after].concat();
            let mut scanned = vt100::Parser::new(4, 20, 0);
            let mut scanner = ApcScanner::new();
            for piece in stream.chunks(2) {
                for segment in scanner.split(piece) {
                    match segment {
                        Segment::Text(text) => scanned.process(text),
                        Segment::Unwrapped(text) => scanned.process(&text),
                        Segment::Graphics(_) => {}
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
}
