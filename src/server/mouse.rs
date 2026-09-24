use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use crate::server::layout::Rect;

const ESC: u8 = 0x1b;
const LEGACY_OFFSET: u16 = 32;
const MAX_SGR_REPORT_LEN: usize = 32;
const MAX_UTF8_VALUE: u16 = 2047;
const BUTTON_BITS: u16 = 0b11;
const RELEASE_BUTTON: u16 = 0b11;
const MOTION_BIT: u16 = 32;
const EXTENDED_BUTTON_BITS: u16 = 64 | 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    Bytes(Vec<u8>),
    Mouse(MouseEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    pub code: u16,
    pub pressed: bool,
    pub row: u16,
    pub col: u16,
}

impl MouseEvent {
    pub fn is_motion(&self) -> bool {
        self.code & MOTION_BIT != 0
    }

    pub fn is_click(&self) -> bool {
        self.pressed
            && !self.is_motion()
            && self.code & EXTENDED_BUTTON_BITS == 0
            && self.code & BUTTON_BITS != RELEASE_BUTTON
    }

    pub fn reported_in(&self, mode: MouseProtocolMode) -> bool {
        match mode {
            MouseProtocolMode::None => false,
            MouseProtocolMode::Press => self.pressed && !self.is_motion(),
            MouseProtocolMode::PressRelease => !self.is_motion(),
            MouseProtocolMode::ButtonMotion => {
                !self.is_motion() || self.code & BUTTON_BITS != RELEASE_BUTTON
            }
            MouseProtocolMode::AnyMotion => true,
        }
    }

    pub fn encode_for(&self, rect: Rect, encoding: MouseProtocolEncoding) -> Option<Vec<u8>> {
        if !rect.contains(self.row, self.col) {
            return None;
        }
        let row = self.row - rect.row + 1;
        let col = self.col - rect.col + 1;
        match encoding {
            MouseProtocolEncoding::Sgr => {
                let action = if self.pressed { 'M' } else { 'm' };
                Some(format!("\x1b[<{};{col};{row}{action}", self.code).into_bytes())
            }
            MouseProtocolEncoding::Default => {
                let mut report = b"\x1b[M".to_vec();
                for value in self.legacy_values(row, col) {
                    report.push(u8::try_from(value?).ok()?);
                }
                Some(report)
            }
            MouseProtocolEncoding::Utf8 => {
                let mut report = String::from("\x1b[M");
                for value in self.legacy_values(row, col) {
                    let value = value.filter(|&value| value <= MAX_UTF8_VALUE)?;
                    report.push(char::from_u32(u32::from(value))?);
                }
                Some(report.into_bytes())
            }
        }
    }

    fn legacy_values(&self, row: u16, col: u16) -> [Option<u16>; 3] {
        let code = if self.pressed {
            self.code
        } else {
            self.code | RELEASE_BUTTON
        };
        [code, col, row].map(|value| value.checked_add(LEGACY_OFFSET))
    }
}

#[derive(Debug, Default)]
pub struct MouseDecoder {
    pending: Vec<u8>,
}

enum Report {
    Mouse(MouseEvent, usize),
    Incomplete,
    Other,
}

impl MouseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn flush(&mut self) -> Option<InputEvent> {
        self.has_pending()
            .then(|| InputEvent::Bytes(std::mem::take(&mut self.pending)))
    }

    pub fn decode(&mut self, chunk: &[u8]) -> Vec<InputEvent> {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(chunk);

        let mut events = Vec::new();
        let mut bytes = Vec::new();
        let mut index = 0;
        while index < input.len() {
            if input[index] != ESC {
                bytes.push(input[index]);
                index += 1;
                continue;
            }
            match parse_report(&input[index..]) {
                Report::Mouse(event, len) => {
                    if !bytes.is_empty() {
                        events.push(InputEvent::Bytes(std::mem::take(&mut bytes)));
                    }
                    events.push(InputEvent::Mouse(event));
                    index += len;
                }
                Report::Incomplete => {
                    self.pending = input.split_off(index);
                    break;
                }
                Report::Other => {
                    bytes.push(ESC);
                    index += 1;
                }
            }
        }
        if !bytes.is_empty() {
            events.push(InputEvent::Bytes(bytes));
        }
        events
    }
}

fn parse_report(input: &[u8]) -> Report {
    match input.get(1..3) {
        None if b"\x1b[".starts_with(input) => Report::Incomplete,
        Some(b"[<") => parse_sgr(&input[3..]),
        Some(b"[M") => parse_legacy(&input[3..]),
        _ => Report::Other,
    }
}

fn parse_sgr(input: &[u8]) -> Report {
    let mut params = [0u16; 3];
    let mut param = 0;
    for (offset, &byte) in input.iter().enumerate().take(MAX_SGR_REPORT_LEN) {
        match byte {
            b'0'..=b'9' => {
                params[param] = params[param]
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
            }
            b';' if param < 2 => param += 1,
            b'M' | b'm' if param == 2 => {
                let [code, col, row] = params;
                let event = MouseEvent {
                    code,
                    pressed: byte == b'M',
                    row: row.saturating_sub(1),
                    col: col.saturating_sub(1),
                };
                return Report::Mouse(event, 3 + offset + 1);
            }
            _ => return Report::Other,
        }
    }
    if input.len() < MAX_SGR_REPORT_LEN {
        Report::Incomplete
    } else {
        Report::Other
    }
}

fn parse_legacy(input: &[u8]) -> Report {
    let Some(&[code, col, row]) = input.get(..3) else {
        return Report::Incomplete;
    };
    let code = u16::from(code).saturating_sub(LEGACY_OFFSET);
    let released =
        code & BUTTON_BITS == RELEASE_BUTTON && code & (MOTION_BIT | EXTENDED_BUTTON_BITS) == 0;
    let event = MouseEvent {
        code,
        pressed: !released,
        row: u16::from(row).saturating_sub(LEGACY_OFFSET + 1),
        col: u16::from(col).saturating_sub(LEGACY_OFFSET + 1),
    };
    Report::Mouse(event, 6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: u16, row: u16, col: u16) -> MouseEvent {
        MouseEvent {
            code,
            pressed: true,
            row,
            col,
        }
    }

    fn release(code: u16, row: u16, col: u16) -> MouseEvent {
        MouseEvent {
            pressed: false,
            ..press(code, row, col)
        }
    }

    fn rect(row: u16, col: u16, rows: u16, cols: u16) -> Rect {
        Rect {
            row,
            col,
            rows,
            cols,
        }
    }

    fn bytes(text: &[u8]) -> InputEvent {
        InputEvent::Bytes(text.to_vec())
    }

    fn merged(events: Vec<InputEvent>) -> Vec<InputEvent> {
        let mut merged: Vec<InputEvent> = Vec::new();
        for event in events {
            match (merged.last_mut(), event) {
                (Some(InputEvent::Bytes(last)), InputEvent::Bytes(more)) => last.extend(more),
                (_, event) => merged.push(event),
            }
        }
        merged
    }

    fn decode_in_chunks(chunks: &[&[u8]]) -> Vec<InputEvent> {
        let mut decoder = MouseDecoder::new();
        let mut events: Vec<InputEvent> = chunks
            .iter()
            .flat_map(|chunk| decoder.decode(chunk))
            .collect();
        events.extend(decoder.flush());
        merged(events)
    }

    const STREAM: &[u8] = b"ls\r\x1b[<0;10;5M\x1b[A\x1b[<35;11;6M\x1b[M\x20\x2a\x25\x1bOB\x1b[<0;10;5mq\x1b[M\x23\x2a\x25\x1b";

    fn stream_events() -> Vec<InputEvent> {
        vec![
            bytes(b"ls\r"),
            InputEvent::Mouse(press(0, 4, 9)),
            bytes(b"\x1b[A"),
            InputEvent::Mouse(press(35, 5, 10)),
            InputEvent::Mouse(press(0, 4, 9)),
            bytes(b"\x1bOB"),
            InputEvent::Mouse(release(0, 4, 9)),
            bytes(b"q"),
            InputEvent::Mouse(release(3, 4, 9)),
            bytes(b"\x1b"),
        ]
    }

    #[test]
    fn plain_input_passes_through_untouched() {
        let mut decoder = MouseDecoder::new();
        let input = b"echo hi\r\x1b[A\x1bOP\x1b[200~paste\x1b[201~\x1b[1;5C";
        assert_eq!(decoder.decode(input), vec![bytes(input)]);
        assert!(!decoder.has_pending());
    }

    #[test]
    fn mouse_reports_are_separated_from_other_input() {
        assert_eq!(decode_in_chunks(&[STREAM]), stream_events());
    }

    #[test]
    fn reports_split_at_any_byte_are_still_decoded() {
        for split in 0..=STREAM.len() {
            let (first, second) = STREAM.split_at(split);
            assert_eq!(
                decode_in_chunks(&[first, second]),
                stream_events(),
                "split at {split}"
            );
        }
        let single_bytes: Vec<&[u8]> = STREAM.chunks(1).collect();
        assert_eq!(decode_in_chunks(&single_bytes), stream_events());
    }

    #[test]
    fn a_trailing_prefix_waits_for_more_input_or_a_flush() {
        let mut decoder = MouseDecoder::new();
        assert_eq!(decoder.decode(b"a\x1b[<1;2"), vec![bytes(b"a")]);
        assert!(decoder.has_pending());
        assert_eq!(
            decoder.decode(b";3M"),
            vec![InputEvent::Mouse(press(1, 2, 1))]
        );
        assert!(!decoder.has_pending());

        assert!(decoder.decode(b"\x1b").is_empty());
        assert_eq!(decoder.flush(), Some(bytes(b"\x1b")));
        assert_eq!(decoder.flush(), None);

        assert!(decoder.decode(b"\x1b[").is_empty());
        assert_eq!(decoder.decode(b"B"), vec![bytes(b"\x1b[B")]);
    }

    #[test]
    fn malformed_reports_are_passed_on_as_input() {
        let mut decoder = MouseDecoder::new();
        for input in [
            &b"\x1b[<0;1M"[..],
            b"\x1b[<0;1;2;3M",
            b"\x1b[<a;1;2M",
            b"\x1b[<0;1;2X",
        ] {
            assert_eq!(decoder.decode(input), vec![bytes(input)]);
            assert!(!decoder.has_pending());
        }

        let endless = [b"\x1b[<".as_slice(), &[b'1'; 40]].concat();
        assert_eq!(decoder.decode(&endless), vec![bytes(&endless)]);
        assert!(!decoder.has_pending());
    }

    #[test]
    fn legacy_reports_keep_wheel_and_modifier_codes() {
        let mut decoder = MouseDecoder::new();
        assert_eq!(
            decoder.decode(b"\x1b[M\x60\x21\x21\x1b[M\x34\x22\x21"),
            vec![
                InputEvent::Mouse(press(64, 0, 0)),
                InputEvent::Mouse(press(20, 0, 1)),
            ]
        );
    }

    #[test]
    fn events_are_encoded_relative_to_the_pane() {
        let pane = rect(3, 41, 10, 39);
        let event = press(2, 5, 50);

        assert_eq!(
            event.encode_for(pane, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<2;10;3M".to_vec())
        );
        assert_eq!(
            release(2, 5, 50).encode_for(pane, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<2;10;3m".to_vec())
        );
        assert_eq!(
            event.encode_for(pane, MouseProtocolEncoding::Default),
            Some(b"\x1b[M\x22\x2a\x23".to_vec())
        );
        assert_eq!(
            release(2, 5, 50).encode_for(pane, MouseProtocolEncoding::Default),
            Some(b"\x1b[M\x23\x2a\x23".to_vec())
        );
        assert_eq!(
            event.encode_for(pane, MouseProtocolEncoding::Utf8),
            Some(b"\x1b[M\x22\x2a\x23".to_vec())
        );
    }

    #[test]
    fn events_outside_the_pane_are_not_encoded() {
        let pane = rect(3, 41, 10, 39);
        for (row, col) in [(2, 50), (13, 50), (5, 40), (5, 80)] {
            assert_eq!(
                press(0, row, col).encode_for(pane, MouseProtocolEncoding::Sgr),
                None
            );
        }
    }

    #[test]
    fn large_coordinates_need_a_wide_enough_encoding() {
        let pane = rect(0, 0, 50, 1000);
        let event = press(0, 1, 300);

        assert_eq!(event.encode_for(pane, MouseProtocolEncoding::Default), None);
        assert_eq!(
            event.encode_for(pane, MouseProtocolEncoding::Utf8),
            Some("\x1b[M\u{20}\u{14d}\u{22}".as_bytes().to_vec())
        );
        assert_eq!(
            press(0, 1, 2020).encode_for(rect(0, 0, 50, 3000), MouseProtocolEncoding::Utf8),
            None
        );
        assert_eq!(
            event.encode_for(pane, MouseProtocolEncoding::Sgr),
            Some(b"\x1b[<0;301;2M".to_vec())
        );
    }

    #[test]
    fn encoded_events_decode_to_the_same_event() {
        let window = rect(0, 0, 100, 200);
        let events = [
            press(0, 0, 0),
            release(0, 7, 3),
            press(35, 99, 199),
            press(65, 12, 150),
            press(16, 4, 4),
        ];
        for event in events {
            let sgr = event
                .encode_for(window, MouseProtocolEncoding::Sgr)
                .unwrap();
            assert_eq!(
                MouseDecoder::new().decode(&sgr),
                vec![InputEvent::Mouse(event)]
            );
        }
        for event in events.into_iter().filter(|event| event.pressed) {
            let legacy = event
                .encode_for(window, MouseProtocolEncoding::Default)
                .unwrap();
            assert_eq!(
                MouseDecoder::new().decode(&legacy),
                vec![InputEvent::Mouse(event)]
            );
        }
    }

    #[test]
    fn clicks_are_button_presses_without_motion_or_wheel() {
        assert!(press(0, 0, 0).is_click());
        assert!(press(2 | 16, 0, 0).is_click());
        assert!(!release(0, 0, 0).is_click());
        assert!(!press(32, 0, 0).is_click());
        assert!(!press(64, 0, 0).is_click());
        assert!(!press(3, 0, 0).is_click());
    }

    #[test]
    fn each_mouse_mode_reports_its_own_events() {
        let click = press(0, 0, 0);
        let up = release(0, 0, 0);
        let drag = press(32, 0, 0);
        let hover = press(35, 0, 0);
        let reported = |mode| {
            [click, up, drag, hover]
                .iter()
                .map(|event| event.reported_in(mode))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            reported(MouseProtocolMode::None),
            [false, false, false, false]
        );
        assert_eq!(
            reported(MouseProtocolMode::Press),
            [true, false, false, false]
        );
        assert_eq!(
            reported(MouseProtocolMode::PressRelease),
            [true, true, false, false]
        );
        assert_eq!(
            reported(MouseProtocolMode::ButtonMotion),
            [true, true, true, false]
        );
        assert_eq!(
            reported(MouseProtocolMode::AnyMotion),
            [true, true, true, true]
        );
    }
}
