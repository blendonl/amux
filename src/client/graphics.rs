use std::borrow::Cow;
use std::collections::BTreeSet;
use std::mem;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use crossterm::terminal::WindowSize;
use tokio::time::Instant;

use crate::protocol::{
    AnimationControl, AnimationState, CellPixels, ClientTerminal, FrameSpec, ImageFormat, ImageOp,
};
use crate::settings::ClientImages;

pub const PROBE: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[>q\x1b[16t\x1b[c";
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const PROBE_KEY: &str = "i=31";
const KITTY_OK: &str = "OK";
const OLDEST_KITTY: [u32; 3] = [0, 28, 0];
const MAX_REPLY_LEN: usize = 256;
const ESC: u8 = 0x1b;
const APC: &[u8] = b"\x1b_G";
const ST: &[u8] = b"\x1b\\";
const END_TRANSMISSION: &[u8] = b"\x1b_Gm=0;\x1b\\";
const CHUNK_LEN: usize = 4096;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Replies {
    kitty: Option<bool>,
    version: Option<String>,
    cell_pixels: Option<CellPixels>,
}

impl Replies {
    fn shows_images(&self) -> bool {
        self.kitty == Some(true) && self.version.as_deref().is_some_and(places_images)
    }
}

fn places_images(version: &str) -> bool {
    if let Some(release) = version
        .strip_prefix("kitty(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return release_at_least(release, OLDEST_KITTY);
    }
    version == "ghostty" || version.starts_with("ghostty ") || version.starts_with("amux-android(")
}

fn release_at_least(release: &str, oldest: [u32; 3]) -> bool {
    let mut parts = [0; 3];
    for (part, text) in parts.iter_mut().zip(release.split('.')) {
        let digits = text
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or_default();
        let Ok(number) = digits.parse() else {
            return false;
        };
        *part = number;
    }
    parts >= oldest
}

pub fn cell_pixels(window: &WindowSize) -> Option<CellPixels> {
    let width = window.width.checked_div(window.columns)?;
    let height = window.height.checked_div(window.rows)?;
    (width > 0 && height > 0).then_some(CellPixels { width, height })
}

enum Scan {
    Partial,
    Mismatch,
    Reply(Reply),
}

enum Reply {
    Kitty(bool),
    Version(String),
    CellPixels(Option<CellPixels>),
    Attributes,
    Unrelated,
}

fn scan(held: &[u8]) -> Scan {
    if held.len() > MAX_REPLY_LEN {
        return Scan::Mismatch;
    }
    match held {
        [] | [ESC] | [ESC, b'['] => Scan::Partial,
        [ESC, b'_', rest @ ..] => terminated(rest, b"G", kitty_reply),
        [ESC, b'P', rest @ ..] => terminated(rest, b">|", |name| Reply::Version(name.to_owned())),
        [ESC, b'[', b'?', rest @ ..] => device_attributes(rest),
        [ESC, b'[', b'6', rest @ ..] => cell_size(rest),
        _ => Scan::Mismatch,
    }
}

fn terminated(rest: &[u8], intro: &[u8], reply: impl FnOnce(&str) -> Reply) -> Scan {
    let Some(text) = rest.strip_prefix(intro) else {
        return if intro.starts_with(rest) {
            Scan::Partial
        } else {
            Scan::Mismatch
        };
    };
    let printable = |bytes: &[u8]| bytes.iter().all(|byte| (b' '..=b'~').contains(byte));
    match text.iter().position(|&byte| byte == ESC) {
        None if printable(text) => Scan::Partial,
        Some(end) if printable(&text[..end]) => match &text[end + 1..] {
            [] => Scan::Partial,
            [b'\\'] => Scan::Reply(reply(&String::from_utf8_lossy(&text[..end]))),
            _ => Scan::Mismatch,
        },
        _ => Scan::Mismatch,
    }
}

fn kitty_reply(body: &str) -> Reply {
    match body.split_once(';') {
        Some((keys, message)) if keys.split(',').any(|key| key == PROBE_KEY) => {
            Reply::Kitty(message == KITTY_OK)
        }
        _ => Reply::Unrelated,
    }
}

fn device_attributes(rest: &[u8]) -> Scan {
    match rest.split_last() {
        Some((b'c', params)) if parameters(params) => Scan::Reply(Reply::Attributes),
        _ if parameters(rest) => Scan::Partial,
        _ => Scan::Mismatch,
    }
}

fn cell_size(rest: &[u8]) -> Scan {
    let partial = matches!(rest.first(), None | Some(b';'))
        && parameters(rest)
        && rest.iter().filter(|byte| **byte == b';').count() <= 2;
    match rest.split_last() {
        Some((b't', params)) => cell_size_reply(params).map_or(Scan::Mismatch, Scan::Reply),
        _ if partial => Scan::Partial,
        _ => Scan::Mismatch,
    }
}

fn cell_size_reply(params: &[u8]) -> Option<Reply> {
    let (height, width) = std::str::from_utf8(params)
        .ok()?
        .strip_prefix(';')?
        .split_once(';')?;
    let number = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !number(height) || !number(width) {
        return None;
    }
    let pixels = match (width.parse(), height.parse()) {
        (Ok(width), Ok(height)) if width > 0 && height > 0 => Some(CellPixels { width, height }),
        _ => None,
    };
    Some(Reply::CellPixels(pixels))
}

fn parameters(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| byte.is_ascii_digit() || *byte == b';')
}

#[derive(Debug, Default)]
pub struct ReplyFilter {
    held: Vec<u8>,
    replies: Replies,
    done: bool,
}

impl ReplyFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn filter(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut input = Vec::with_capacity(chunk.len());
        self.feed(chunk, &mut input);
        input
    }

    pub fn flush(&mut self) -> Vec<u8> {
        self.done = true;
        mem::take(&mut self.held)
    }

    fn feed(&mut self, mut bytes: &[u8], input: &mut Vec<u8>) {
        while let Some((&byte, rest)) = bytes.split_first() {
            if self.done {
                input.extend_from_slice(bytes);
                return;
            }
            if self.held.is_empty() && byte != ESC {
                let end = bytes
                    .iter()
                    .position(|&byte| byte == ESC)
                    .unwrap_or(bytes.len());
                input.extend_from_slice(&bytes[..end]);
                bytes = &bytes[end..];
                continue;
            }
            self.held.push(byte);
            bytes = rest;
            match scan(&self.held) {
                Scan::Partial => {}
                Scan::Reply(reply) => {
                    self.held.clear();
                    self.record(reply);
                }
                Scan::Mismatch => {
                    let held = mem::take(&mut self.held);
                    if let Some((first, after)) = held.split_first() {
                        input.push(*first);
                        self.feed(after, input);
                    }
                }
            }
        }
    }

    fn record(&mut self, reply: Reply) {
        match reply {
            Reply::Kitty(ok) => self.replies.kitty = Some(ok),
            Reply::Version(name) => self.replies.version = Some(name),
            Reply::CellPixels(pixels) => self.replies.cell_pixels = pixels,
            Reply::Attributes => self.done = true,
            Reply::Unrelated => {}
        }
    }
}

#[derive(Debug)]
pub struct TerminalProbe {
    images: Option<ClientImages>,
    filter: Option<ReplyFilter>,
    deadline: Option<Instant>,
    probed: bool,
    replies: Replies,
    window: Option<CellPixels>,
    reported: Option<ClientTerminal>,
}

impl TerminalProbe {
    pub fn new(window: Option<CellPixels>) -> Self {
        Self {
            images: None,
            filter: None,
            deadline: None,
            probed: false,
            replies: Replies::default(),
            window,
            reported: None,
        }
    }

    pub fn set_images(&mut self, images: ClientImages, now: Instant) -> &'static [u8] {
        if self.images.replace(images) == Some(images) || images == ClientImages::Off || self.probed
        {
            return &[];
        }
        self.probed = true;
        self.filter = Some(ReplyFilter::new());
        self.deadline = Some(now + PROBE_TIMEOUT);
        PROBE
    }

    pub fn filter<'a>(&mut self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        let Some(filter) = self.filter.as_mut() else {
            return Cow::Borrowed(chunk);
        };
        let input = filter.filter(chunk);
        if filter.is_done() {
            self.finish();
        }
        Cow::Owned(input)
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub fn time_out(&mut self) -> Vec<u8> {
        let held = self
            .filter
            .as_mut()
            .map(ReplyFilter::flush)
            .unwrap_or_default();
        self.finish();
        held
    }

    pub fn resized(&mut self, window: Option<CellPixels>) {
        self.window = window;
    }

    pub fn take_report(&mut self) -> Option<ClientTerminal> {
        if self.filter.is_some() {
            return None;
        }
        let terminal = self.terminal()?;
        (self.reported.replace(terminal) != Some(terminal)).then_some(terminal)
    }

    fn terminal(&self) -> Option<ClientTerminal> {
        let graphics = match self.images? {
            ClientImages::Auto => self.replies.shows_images(),
            ClientImages::On => true,
            ClientImages::Off => false,
        };
        Some(ClientTerminal {
            graphics,
            cell_pixels: self.window.or(self.replies.cell_pixels),
        })
    }

    fn finish(&mut self) {
        if let Some(filter) = self.filter.take() {
            self.replies = filter.replies;
        }
        self.deadline = None;
    }
}

#[derive(Debug, Default)]
pub struct KittyWriter {
    open: Option<Transmission>,
    queued: Vec<ImageOp>,
    keys: BTreeSet<u32>,
}

impl KittyWriter {
    pub fn write(&mut self, op: ImageOp, out: &mut Vec<u8>) {
        let waits = match (&self.open, &op) {
            (None, _) => false,
            (Some(open), ImageOp::Transmit { key, .. }) => !open.continues(*key, false),
            (Some(open), ImageOp::Frame { key, .. }) => !open.continues(*key, true),
            (Some(_), _) => true,
        };
        if waits {
            self.queued.push(op);
            return;
        }
        match op {
            ImageOp::Transmit {
                key,
                format,
                width,
                height,
                compressed,
                data,
                last,
                ..
            } => {
                let control = || {
                    let pixels = pixel_keys(format, width, height, compressed);
                    format!("a=t,i={key},{pixels},q=2")
                };
                self.send(key, false, control, &data, last, out);
            }
            ImageOp::Frame {
                key,
                spec,
                format,
                width,
                height,
                compressed,
                data,
                last,
                ..
            } => {
                let control = || {
                    let pixels = pixel_keys(format, width, height, compressed);
                    format!("a=f,i={key},{pixels}{},q=2", frame_keys(spec))
                };
                self.send(key, true, control, &data, last, out);
            }
            ImageOp::Place { key, cols, rows } => {
                self.keys.insert(key);
                command(out, &format!("a=p,U=1,i={key},c={cols},r={rows},q=2"));
            }
            ImageOp::Delete { key } => {
                self.keys.remove(&key);
                delete(key, out);
            }
            ImageOp::Animate { key, control } => {
                command(out, &format!("a=a,i={key}{},q=2", animate_keys(control)));
            }
        }
    }

    fn send(
        &mut self,
        key: u32,
        frame: bool,
        control: impl FnOnce() -> String,
        data: &[u8],
        last: bool,
        out: &mut Vec<u8>,
    ) {
        self.keys.insert(key);
        self.open
            .get_or_insert_with(|| Transmission::new(key, frame, control()))
            .send(data, last, out);
        if last {
            self.open = None;
            for queued in mem::take(&mut self.queued) {
                self.write(queued, out);
            }
        }
    }

    pub fn abort(&mut self, out: &mut Vec<u8>) {
        if self.open.take().is_some_and(|open| open.started()) {
            out.extend_from_slice(END_TRANSMISSION);
        }
        self.queued.clear();
    }

    pub fn cleanup(&mut self, out: &mut Vec<u8>) {
        self.abort(out);
        for key in mem::take(&mut self.keys) {
            delete(key, out);
        }
    }
}

#[derive(Debug)]
struct Transmission {
    key: u32,
    frame: bool,
    control: Option<String>,
    carry: Vec<u8>,
}

impl Transmission {
    fn new(key: u32, frame: bool, control: String) -> Self {
        Self {
            key,
            frame,
            control: Some(control),
            carry: Vec::new(),
        }
    }

    fn continues(&self, key: u32, frame: bool) -> bool {
        self.key == key && self.frame == frame
    }

    fn started(&self) -> bool {
        self.control.is_none()
    }

    fn send(&mut self, data: &[u8], last: bool, out: &mut Vec<u8>) {
        self.carry.extend_from_slice(data);
        let whole = if last {
            self.carry.len()
        } else {
            self.carry.len() / 3 * 3
        };
        let encoded = STANDARD.encode(&self.carry[..whole]);
        self.carry.drain(..whole);
        let mut chunks = encoded.as_bytes().chunks(CHUNK_LEN).peekable();
        if last && chunks.peek().is_none() {
            self.chunk(b"", false, out);
        }
        while let Some(chunk) = chunks.next() {
            let more = !last || chunks.peek().is_some();
            self.chunk(chunk, more, out);
        }
    }

    fn chunk(&mut self, payload: &[u8], more: bool, out: &mut Vec<u8>) {
        out.extend_from_slice(APC);
        if let Some(control) = self.control.take() {
            out.extend_from_slice(control.as_bytes());
            out.push(b',');
        }
        out.extend_from_slice(if more { b"m=1;" } else { b"m=0;" });
        out.extend_from_slice(payload);
        out.extend_from_slice(ST);
    }
}

fn pixel_keys(format: ImageFormat, width: u32, height: u32, compressed: bool) -> String {
    let format = match format {
        ImageFormat::Rgb24 => 24,
        ImageFormat::Rgba32 => 32,
        ImageFormat::Png => 100,
    };
    let compression = if compressed { ",o=z" } else { "" };
    format!("f={format},s={width},v={height}{compression}")
}

fn frame_keys(spec: FrameSpec) -> String {
    let FrameSpec {
        edit,
        base,
        x,
        y,
        background,
        replace,
        gap,
    } = spec;
    set_keys(&[
        ("r", i64::from(edit)),
        ("c", i64::from(base)),
        ("x", i64::from(x)),
        ("y", i64::from(y)),
        ("Y", i64::from(background)),
        ("X", i64::from(replace)),
        ("z", i64::from(gap)),
    ])
}

fn animate_keys(control: AnimationControl) -> String {
    let state = match control.state {
        None => 0,
        Some(AnimationState::Stopped) => 1,
        Some(AnimationState::Loading) => 2,
        Some(AnimationState::Running) => 3,
    };
    set_keys(&[
        ("r", i64::from(control.frame)),
        ("z", i64::from(control.gap)),
        ("c", i64::from(control.current)),
        ("s", state),
        ("v", i64::from(control.loops)),
    ])
}

fn set_keys(keys: &[(&str, i64)]) -> String {
    let set: Vec<String> = keys
        .iter()
        .filter(|(_, value)| *value != 0)
        .map(|(key, value)| format!(",{key}={value}"))
        .collect();
    set.concat()
}

fn command(out: &mut Vec<u8>, control: &str) {
    out.extend_from_slice(APC);
    out.extend_from_slice(control.as_bytes());
    out.extend_from_slice(ST);
}

fn delete(key: u32, out: &mut Vec<u8>) {
    command(out, &format!("a=d,d=I,i={key},q=2"));
}

#[cfg(test)]
mod tests {
    use super::*;

    const KITTY_OK: &[u8] = b"\x1b_Gi=31;OK\x1b\\";
    const KITTY_ERROR: &[u8] = b"\x1b_Gi=31;ENOTSUPPORTED:unsupported medium\x1b\\";
    const KITTY_VERSION: &[u8] = b"\x1bP>|kitty(0.35.2)\x1b\\";
    const CELL_SIZE: &[u8] = b"\x1b[6;21;10t";
    const ATTRIBUTES: &[u8] = b"\x1b[?62;22;52c";
    const WINDOW: CellPixels = CellPixels {
        width: 8,
        height: 16,
    };
    const REPLIED: CellPixels = CellPixels {
        width: 10,
        height: 21,
    };

    fn filtered(chunks: &[&[u8]]) -> (Vec<u8>, ReplyFilter) {
        let mut filter = ReplyFilter::new();
        let input = chunks
            .iter()
            .flat_map(|chunk| filter.filter(chunk))
            .collect();
        (input, filter)
    }

    fn every_split(stream: &[u8], check: impl Fn(&str, Vec<u8>, ReplyFilter)) {
        for split in 0..=stream.len() {
            let (first, second) = stream.split_at(split);
            let (input, filter) = filtered(&[first, second]);
            check(&format!("split at {split}"), input, filter);
        }
        let bytes: Vec<&[u8]> = stream.chunks(1).collect();
        let (input, filter) = filtered(&bytes);
        check("one byte at a time", input, filter);
    }

    #[test]
    fn every_reply_is_taken_out_wherever_the_reads_split_it() {
        let cases = [
            (
                KITTY_OK,
                Replies {
                    kitty: Some(true),
                    ..Replies::default()
                },
            ),
            (
                KITTY_ERROR,
                Replies {
                    kitty: Some(false),
                    ..Replies::default()
                },
            ),
            (
                KITTY_VERSION,
                Replies {
                    version: Some("kitty(0.35.2)".into()),
                    ..Replies::default()
                },
            ),
            (
                CELL_SIZE,
                Replies {
                    cell_pixels: Some(REPLIED),
                    ..Replies::default()
                },
            ),
            (ATTRIBUTES, Replies::default()),
        ];
        for (reply, expected) in cases {
            let stream = [b"a", reply, b"b"].concat();
            every_split(&stream, |how, input, filter| {
                let reply = String::from_utf8_lossy(reply);
                assert_eq!(input, b"ab", "{reply:?}, {how}");
                assert_eq!(&filter.replies, &expected, "{reply:?}, {how}");
                assert_eq!(filter.is_done(), reply.ends_with('c'), "{reply:?}, {how}");
            });
        }
    }

    #[test]
    fn typed_ahead_input_keeps_its_place_around_the_replies() {
        let stream = [
            b"ls".as_slice(),
            KITTY_OK,
            b"\x1b[A",
            KITTY_VERSION,
            b"\x1b[6~\x1b[6;5~",
            CELL_SIZE,
            b"\x1bx\x1b",
            ATTRIBUTES,
            b"\x1b[B",
            KITTY_OK,
        ]
        .concat();
        let typed = [
            b"ls\x1b[A\x1b[6~\x1b[6;5~\x1bx\x1b\x1b[B".as_slice(),
            KITTY_OK,
        ]
        .concat();
        every_split(&stream, |how, input, filter| {
            assert_eq!(
                String::from_utf8_lossy(&input),
                String::from_utf8_lossy(&typed),
                "{how}"
            );
            assert!(filter.is_done(), "{how}");
            assert_eq!(
                &filter.replies,
                &Replies {
                    kitty: Some(true),
                    version: Some("kitty(0.35.2)".into()),
                    cell_pixels: Some(REPLIED),
                },
                "{how}"
            );
        });
    }

    #[test]
    fn look_alikes_that_turn_out_to_be_keys_are_input() {
        for typed in [
            b"\x1b[6~".as_slice(),
            b"\x1b[6;5~",
            b"\x1b[?1;2$y",
            b"\x1b_Gi=31;OK\r",
            b"\x1bP>q",
            b"\x1b[<0;3;4M",
            b"\x1b\x1b[A",
        ] {
            every_split(typed, |how, input, filter| {
                assert_eq!(input, typed, "{:?}, {how}", String::from_utf8_lossy(typed));
                assert_eq!(&filter.replies, &Replies::default());
            });
        }
        let long = [b"\x1b_G".as_slice(), &[b'x'; MAX_REPLY_LEN]].concat();
        assert_eq!(filtered(&[&long]).0, long);
    }

    #[test]
    fn a_held_escape_comes_back_when_the_probe_gives_up() {
        let mut filter = ReplyFilter::new();
        assert_eq!(filter.filter(b"q\x1b"), b"q");
        assert_eq!(filter.filter(b"_G"), b"");
        assert_eq!(filter.flush(), b"\x1b_G");
        assert!(filter.is_done());
        assert_eq!(filter.filter(KITTY_OK), KITTY_OK);
    }

    fn answers(version: &str) -> Vec<u8> {
        [
            KITTY_OK,
            format!("\x1bP>|{version}\x1b\\").as_bytes(),
            CELL_SIZE,
            ATTRIBUTES,
        ]
        .concat()
    }

    fn probe(images: ClientImages, window: Option<CellPixels>) -> TerminalProbe {
        let mut probe = TerminalProbe::new(window);
        probe.set_images(images, Instant::now());
        probe
    }

    fn answered(images: ClientImages, window: Option<CellPixels>, answer: &[u8]) -> ClientTerminal {
        let mut probe = probe(images, window);
        assert_eq!(probe.take_report(), None);
        assert_eq!(probe.filter(answer), b"".as_slice());
        assert_eq!(probe.deadline(), None);
        probe
            .take_report()
            .expect("a report once the terminal answered")
    }

    #[test]
    fn graphics_need_a_kitty_ok_from_a_terminal_that_places_images() {
        for (version, graphics) in [
            ("kitty(0.27.1)", false),
            ("kitty(0.28.0)", true),
            ("kitty(0.35.2)", true),
            ("kitty(1.2)", true),
            ("kitty()", false),
            ("ghostty 1.1.3", true),
            ("amux-android(1)", true),
            ("alacritty(0.15.1)", false),
            ("WezTerm 20240203-110809-5046fc22", false),
        ] {
            let terminal = answered(ClientImages::Auto, None, &answers(version));
            assert_eq!(terminal.graphics, graphics, "{version}");
            assert_eq!(terminal.cell_pixels, Some(REPLIED), "{version}");
        }

        let refused = [KITTY_ERROR, KITTY_VERSION, ATTRIBUTES].concat();
        assert!(!answered(ClientImages::Auto, None, &refused).graphics);
        let silent = [KITTY_VERSION, ATTRIBUTES].concat();
        assert!(!answered(ClientImages::Auto, None, &silent).graphics);
    }

    #[test]
    fn the_probe_gives_up_after_its_timeout() {
        let now = Instant::now();
        let mut probe = TerminalProbe::new(Some(WINDOW));
        assert_eq!(probe.set_images(ClientImages::Auto, now), PROBE);
        assert_eq!(probe.deadline(), Some(now + PROBE_TIMEOUT));
        assert_eq!(probe.filter(b"vi\x1b"), b"vi".as_slice());
        assert_eq!(probe.take_report(), None);

        assert_eq!(probe.time_out(), b"\x1b");
        assert_eq!(probe.deadline(), None);
        assert_eq!(
            probe.take_report(),
            Some(ClientTerminal {
                graphics: false,
                cell_pixels: Some(WINDOW),
            })
        );
        assert_eq!(probe.take_report(), None);
        assert!(matches!(probe.filter(b"\x1b"), Cow::Borrowed(b"\x1b")));
    }

    #[test]
    fn on_needs_no_kitty_answer_and_off_sends_no_probe() {
        let alacritty = [KITTY_OK, b"\x1bP>|alacritty(0.15.1)\x1b\\", ATTRIBUTES].concat();
        assert_eq!(
            answered(ClientImages::On, None, &alacritty),
            ClientTerminal {
                graphics: true,
                cell_pixels: None,
            }
        );
        let mut silent = probe(ClientImages::On, Some(WINDOW));
        assert_eq!(silent.time_out(), b"");
        assert_eq!(
            silent.take_report(),
            Some(ClientTerminal {
                graphics: true,
                cell_pixels: Some(WINDOW),
            })
        );

        let mut off = TerminalProbe::new(Some(WINDOW));
        assert_eq!(off.set_images(ClientImages::Off, Instant::now()), b"");
        assert_eq!(off.deadline(), None);
        assert!(matches!(off.filter(KITTY_OK), Cow::Borrowed(KITTY_OK)));
        assert_eq!(
            off.take_report(),
            Some(ClientTerminal {
                graphics: false,
                cell_pixels: Some(WINDOW),
            })
        );
    }

    #[test]
    fn the_window_size_wins_over_the_cell_size_reply() {
        let terminal = answered(ClientImages::Auto, Some(WINDOW), &answers("ghostty 1.1.3"));
        assert_eq!(
            terminal,
            ClientTerminal {
                graphics: true,
                cell_pixels: Some(WINDOW),
            }
        );
    }

    #[test]
    fn a_resize_reports_only_a_changed_terminal() {
        let mut probe = probe(ClientImages::Auto, None);
        probe.resized(Some(WINDOW));
        assert_eq!(probe.take_report(), None);
        probe.filter(&answers("kitty(0.35.2)"));
        assert_eq!(
            probe
                .take_report()
                .and_then(|terminal| terminal.cell_pixels),
            Some(WINDOW)
        );

        probe.resized(Some(WINDOW));
        assert_eq!(probe.take_report(), None);
        probe.resized(None);
        assert_eq!(
            probe.take_report(),
            Some(ClientTerminal {
                graphics: true,
                cell_pixels: Some(REPLIED),
            })
        );
        probe.resized(None);
        assert_eq!(probe.take_report(), None);
    }

    #[test]
    fn a_reload_can_turn_images_off_and_on_again() {
        let now = Instant::now();
        let mut probe = TerminalProbe::new(None);
        assert_eq!(probe.set_images(ClientImages::Off, now), b"");
        assert_eq!(
            probe.take_report().map(|terminal| terminal.graphics),
            Some(false)
        );
        assert_eq!(probe.set_images(ClientImages::Off, now), b"");
        assert_eq!(probe.set_images(ClientImages::Auto, now), PROBE);
        assert_eq!(probe.take_report(), None);
        probe.filter(&answers("kitty(0.35.2)"));
        assert_eq!(
            probe.take_report().map(|terminal| terminal.graphics),
            Some(true)
        );

        assert_eq!(probe.set_images(ClientImages::Off, now), b"");
        assert_eq!(
            probe.take_report().map(|terminal| terminal.graphics),
            Some(false)
        );
        assert_eq!(probe.set_images(ClientImages::Auto, now), b"");
        assert_eq!(
            probe.take_report().map(|terminal| terminal.graphics),
            Some(true)
        );
    }

    #[test]
    fn the_cell_size_comes_from_the_window_pixels_when_known() {
        let window = |width, height| WindowSize {
            rows: 24,
            columns: 80,
            width,
            height,
        };
        assert_eq!(
            cell_pixels(&window(800, 504)),
            Some(CellPixels {
                width: 10,
                height: 21,
            })
        );
        assert_eq!(cell_pixels(&window(0, 0)), None);
        assert_eq!(cell_pixels(&window(800, 0)), None);
        assert_eq!(
            cell_pixels(&WindowSize {
                rows: 0,
                columns: 0,
                width: 800,
                height: 504,
            }),
            None
        );
    }

    fn commands(mut bytes: &[u8]) -> Vec<(String, String)> {
        let mut commands = Vec::new();
        while !bytes.is_empty() {
            let body = bytes.strip_prefix(APC).unwrap_or_else(|| {
                panic!(
                    "not a graphics command: {:?}",
                    String::from_utf8_lossy(bytes)
                )
            });
            let end = body
                .windows(ST.len())
                .position(|pair| pair == ST)
                .expect("an unterminated graphics command");
            let text = std::str::from_utf8(&body[..end]).unwrap();
            let (control, payload) = text.split_once(';').unwrap_or((text, ""));
            commands.push((control.to_owned(), payload.to_owned()));
            bytes = &body[end + ST.len()..];
        }
        commands
    }

    fn controls(bytes: &[u8]) -> Vec<String> {
        commands(bytes)
            .into_iter()
            .map(|(control, _)| control)
            .collect()
    }

    fn decoded(commands: &[(String, String)]) -> Vec<u8> {
        let payload: String = commands
            .iter()
            .map(|(_, payload)| payload.as_str())
            .collect();
        STANDARD.decode(payload).unwrap()
    }

    fn transmit(key: u32, data: &[u8], last: bool) -> ImageOp {
        ImageOp::Transmit {
            key,
            format: ImageFormat::Rgba32,
            width: 2,
            height: 1,
            compressed: false,
            total: 8,
            data: data.to_vec(),
            last,
        }
    }

    fn written(ops: impl IntoIterator<Item = ImageOp>) -> (KittyWriter, Vec<u8>) {
        let mut writer = KittyWriter::default();
        let mut out = Vec::new();
        for op in ops {
            writer.write(op, &mut out);
        }
        (writer, out)
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..=u8::MAX).cycle().take(len).collect()
    }

    #[test]
    fn small_images_are_single_quiet_commands() {
        let (_, out) = written([transmit(7, &[1, 2, 3, 4, 5, 6, 7, 8], true)]);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "\x1b_Ga=t,i=7,f=32,s=2,v=1,q=2,m=0;AQIDBAUGBwg=\x1b\\"
        );

        let png = ImageOp::Transmit {
            key: 9,
            format: ImageFormat::Png,
            width: 640,
            height: 480,
            compressed: true,
            total: 3,
            data: vec![0xff, 0, 0x80],
            last: true,
        };
        let rgb = ImageOp::Transmit {
            key: 10,
            format: ImageFormat::Rgb24,
            width: 1,
            height: 1,
            compressed: false,
            total: 3,
            data: vec![1, 2, 3],
            last: true,
        };
        let (_, out) = written([
            png,
            rgb,
            ImageOp::Place {
                key: 9,
                cols: 30,
                rows: 12,
            },
            ImageOp::Delete { key: 10 },
        ]);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(
                "\x1b_Ga=t,i=9,f=100,s=640,v=480,o=z,q=2,m=0;/wCA\x1b\\",
                "\x1b_Ga=t,i=10,f=24,s=1,v=1,q=2,m=0;AQID\x1b\\",
                "\x1b_Ga=p,U=1,i=9,c=30,r=12,q=2\x1b\\",
                "\x1b_Ga=d,d=I,i=10,q=2\x1b\\",
            )
        );
    }

    #[test]
    fn large_images_go_in_chunks_of_at_most_4096_bytes() {
        let data = pattern(10_000);
        let (_, out) = written([transmit(3, &data, true)]);
        let commands = commands(&out);
        let lens: Vec<usize> = commands.iter().map(|(_, payload)| payload.len()).collect();
        assert_eq!(lens, [4096, 4096, 4096, 1048]);
        assert_eq!(
            controls(&out),
            ["a=t,i=3,f=32,s=2,v=1,q=2,m=1", "m=1", "m=1", "m=0"]
        );
        assert_eq!(decoded(&commands), data);
    }

    #[test]
    fn an_image_sent_as_several_operations_decodes_to_its_bytes() {
        let data = pattern(9_000);
        let lens = [1000, 2, 1, 5000, 2997];
        let mut ops = Vec::new();
        let mut start = 0;
        for (index, len) in lens.iter().enumerate() {
            let last = index + 1 == lens.len();
            ops.push(transmit(5, &data[start..start + len], last));
            start += len;
        }
        let (writer, out) = written(ops);
        let commands = commands(&out);

        assert_eq!(commands[0].0, "a=t,i=5,f=32,s=2,v=1,q=2,m=1");
        let (last, rest) = commands.split_last().unwrap();
        assert_eq!(last.0, "m=0");
        for (control, payload) in &commands[1..commands.len() - 1] {
            assert_eq!(control, "m=1");
            assert!(payload.len() <= CHUNK_LEN, "{}", payload.len());
        }
        for (_, payload) in rest {
            assert_eq!(payload.len() % 4, 0, "{payload}");
            assert!(!payload.contains('='), "{payload}");
        }
        assert_eq!(decoded(&commands), data);
        assert!(writer.open.is_none());
    }

    #[test]
    fn places_and_deletes_wait_for_the_open_transmission() {
        let (mut writer, out) = written([
            transmit(1, &[0; 6000], false),
            ImageOp::Place {
                key: 2,
                cols: 4,
                rows: 2,
            },
            ImageOp::Delete { key: 3 },
            transmit(4, &[9; 8], true),
        ]);
        assert_eq!(controls(&out), ["a=t,i=1,f=32,s=2,v=1,q=2,m=1", "m=1"]);

        let mut out = Vec::new();
        writer.write(transmit(1, &[0; 2], true), &mut out);
        assert_eq!(
            controls(&out),
            [
                "m=0",
                "a=p,U=1,i=2,c=4,r=2,q=2",
                "a=d,d=I,i=3,q=2",
                "a=t,i=4,f=32,s=2,v=1,q=2,m=0",
            ]
        );
    }

    #[test]
    fn no_other_command_lands_inside_a_transmission() {
        let place = |key| ImageOp::Place {
            key,
            cols: 3,
            rows: 1,
        };
        let (writer, out) = written([
            transmit(1, &pattern(5000), false),
            place(1),
            transmit(2, &[2; 10], true),
            ImageOp::Delete { key: 9 },
            transmit(1, &pattern(5000), false),
            transmit(3, &[3; 5000], false),
            place(2),
            transmit(1, &pattern(100), true),
            transmit(3, &[3; 100], true),
            place(3),
        ]);
        let mut open = false;
        let continuation = |control: &str| control == "m=1" || control == "m=0";
        for control in controls(&out) {
            assert_eq!(continuation(&control), open, "{control}");
            if control.contains("m=") {
                open = control.ends_with("m=1");
            }
        }
        assert!(!open);
        assert!(writer.open.is_none() && writer.queued.is_empty());
        assert_eq!(
            controls(&out)
                .into_iter()
                .filter(|control| !continuation(control))
                .collect::<Vec<_>>(),
            [
                "a=t,i=1,f=32,s=2,v=1,q=2,m=1",
                "a=p,U=1,i=1,c=3,r=1,q=2",
                "a=t,i=2,f=32,s=2,v=1,q=2,m=0",
                "a=d,d=I,i=9,q=2",
                "a=t,i=3,f=32,s=2,v=1,q=2,m=1",
                "a=p,U=1,i=2,c=3,r=1,q=2",
                "a=p,U=1,i=3,c=3,r=1,q=2",
            ]
        );
    }

    fn frame(key: u32, spec: FrameSpec, data: &[u8], last: bool) -> ImageOp {
        ImageOp::Frame {
            key,
            spec,
            format: ImageFormat::Rgba32,
            width: 2,
            height: 1,
            compressed: false,
            total: 8,
            data: data.to_vec(),
            last,
        }
    }

    #[test]
    fn frames_are_quiet_a_f_commands_with_only_the_keys_they_set() {
        let spec = FrameSpec {
            edit: 0,
            base: 1,
            x: 1,
            y: 2,
            background: 255,
            replace: true,
            gap: 100,
        };
        let edit = ImageOp::Frame {
            key: 9,
            spec: FrameSpec {
                edit: 2,
                gap: -1,
                ..FrameSpec::default()
            },
            format: ImageFormat::Png,
            width: 640,
            height: 480,
            compressed: true,
            total: 3,
            data: vec![0xff, 0, 0x80],
            last: true,
        };
        let plain = ImageOp::Frame {
            key: 3,
            spec: FrameSpec::default(),
            format: ImageFormat::Rgb24,
            width: 1,
            height: 1,
            compressed: false,
            total: 3,
            data: vec![1, 2, 3],
            last: true,
        };
        let (writer, out) = written([frame(7, spec, &[1, 2, 3, 4, 5, 6, 7, 8], true), edit, plain]);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(
                "\x1b_Ga=f,i=7,f=32,s=2,v=1,c=1,x=1,y=2,Y=255,X=1,z=100,q=2,m=0;AQIDBAUGBwg=\x1b\\",
                "\x1b_Ga=f,i=9,f=100,s=640,v=480,o=z,r=2,z=-1,q=2,m=0;/wCA\x1b\\",
                "\x1b_Ga=f,i=3,f=24,s=1,v=1,q=2,m=0;AQID\x1b\\",
            )
        );
        assert_eq!(writer.keys, BTreeSet::from([3, 7, 9]));
    }

    #[test]
    fn animate_commands_carry_only_the_keys_they_set() {
        let animate = |control| ImageOp::Animate { key: 4, control };
        let (writer, out) = written([
            animate(AnimationControl {
                frame: 1,
                gap: 30,
                current: 2,
                state: Some(AnimationState::Running),
                loops: 1,
            }),
            animate(AnimationControl {
                frame: 3,
                gap: -1,
                ..AnimationControl::default()
            }),
            animate(AnimationControl {
                state: Some(AnimationState::Stopped),
                ..AnimationControl::default()
            }),
            animate(AnimationControl {
                state: Some(AnimationState::Loading),
                loops: 5,
                ..AnimationControl::default()
            }),
            animate(AnimationControl::default()),
        ]);
        assert_eq!(
            controls(&out),
            [
                "a=a,i=4,r=1,z=30,c=2,s=3,v=1,q=2",
                "a=a,i=4,r=3,z=-1,q=2",
                "a=a,i=4,s=1,q=2",
                "a=a,i=4,s=2,v=5,q=2",
                "a=a,i=4,q=2",
            ]
        );
        assert!(writer.keys.is_empty());
    }

    #[test]
    fn frames_are_chunked_and_nothing_lands_inside_one() {
        let data = pattern(10_000);
        let spec = FrameSpec {
            edit: 2,
            ..FrameSpec::default()
        };
        let (mut writer, out) = written([
            frame(5, spec, &data[..6000], false),
            ImageOp::Animate {
                key: 5,
                control: AnimationControl {
                    current: 2,
                    ..AnimationControl::default()
                },
            },
            frame(6, FrameSpec::default(), &[1; 8], true),
            transmit(5, &[2; 8], true),
            ImageOp::Delete { key: 7 },
            frame(5, spec, &data[6000..], false),
        ]);
        assert_eq!(
            controls(&out),
            ["a=f,i=5,f=32,s=2,v=1,r=2,q=2,m=1", "m=1", "m=1", "m=1"]
        );
        let mut rest = Vec::new();
        writer.write(frame(5, spec, &[], true), &mut rest);
        assert_eq!(
            controls(&rest),
            [
                "m=0",
                "a=a,i=5,c=2,q=2",
                "a=f,i=6,f=32,s=2,v=1,q=2,m=0",
                "a=t,i=5,f=32,s=2,v=1,q=2,m=0",
                "a=d,d=I,i=7,q=2",
            ]
        );
        let commands = commands(&[out, rest].concat());
        assert_eq!(decoded(&commands[..5]), data);
        assert!(writer.open.is_none() && writer.queued.is_empty());
    }

    #[test]
    fn abort_ends_the_transmission_and_cleanup_deletes_every_key() {
        let place = |key| ImageOp::Place {
            key,
            cols: 1,
            rows: 1,
        };
        let (mut writer, _) = written([
            transmit(8, &[1; 8], true),
            place(5),
            transmit(2, &[1; 8], true),
            ImageOp::Delete { key: 2 },
            transmit(6, &[1; 3000], false),
            place(8),
        ]);
        let mut out = Vec::new();
        writer.abort(&mut out);
        assert_eq!(out, END_TRANSMISSION);
        out.clear();
        writer.abort(&mut out);
        assert_eq!(out, b"");

        writer.cleanup(&mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(
                "\x1b_Ga=d,d=I,i=5,q=2\x1b\\",
                "\x1b_Ga=d,d=I,i=6,q=2\x1b\\",
                "\x1b_Ga=d,d=I,i=8,q=2\x1b\\",
            )
        );
        let mut out = Vec::new();
        writer.cleanup(&mut out);
        assert_eq!(out, b"");
    }

    #[test]
    fn cleanup_ends_an_open_transmission_before_deleting() {
        let (mut writer, _) = written([transmit(4, &[1; 3000], false)]);
        let mut out = Vec::new();
        writer.cleanup(&mut out);
        assert_eq!(out, b"\x1b_Gm=0;\x1b\\\x1b_Ga=d,d=I,i=4,q=2\x1b\\");

        let (mut writer, out) = written([transmit(4, &[1], false)]);
        assert_eq!(out, b"");
        let mut out = Vec::new();
        writer.cleanup(&mut out);
        assert_eq!(out, b"\x1b_Ga=d,d=I,i=4,q=2\x1b\\");
    }
}
