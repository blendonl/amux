use std::collections::BTreeMap;

use vt100::{Color, MouseProtocolEncoding, MouseProtocolMode};

use super::grid::Cell;
use super::{compose, Frame, GridDiffer, InputModes};
use crate::protocol::Size;
use crate::server::layout::{Layout, PaneId, Rect, SplitDirection};

const A: PaneId = PaneId(0);
const B: PaneId = PaneId(1);
const C: PaneId = PaneId(2);
const WINDOW: Size = Size { rows: 12, cols: 40 };

struct Window {
    layout: Layout,
    panes: BTreeMap<PaneId, vt100::Parser>,
    active: PaneId,
}

impl Window {
    fn three_panes() -> Self {
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, WINDOW)
            .unwrap();
        layout
            .split(B, C, SplitDirection::TopBottom, WINDOW)
            .unwrap();
        let panes = layout
            .rects(WINDOW)
            .into_iter()
            .map(|(pane, rect)| (pane, vt100::Parser::new(rect.rows, rect.cols, 100)))
            .collect();
        Self {
            layout,
            panes,
            active: C,
        }
    }

    fn feed(&mut self, pane: PaneId, input: &str) {
        self.panes.get_mut(&pane).unwrap().process(input.as_bytes());
    }

    fn rect(&self, pane: PaneId) -> Rect {
        self.layout
            .rects(WINDOW)
            .into_iter()
            .find(|(id, _)| *id == pane)
            .unwrap()
            .1
    }

    fn frame(&self) -> Frame {
        compose(&self.layout, WINDOW, self.active, &self.panes)
    }

    fn fill(&mut self) {
        self.feed(
            A,
            "\x1b[31mred\x1b[0m \x1b[1;44mbold on blue\x1b[0m\r\n\
             \x1b[7minverse\x1b[27m \x1b[2mdim\x1b[22m \x1b[3;4mitalic under\x1b[0m\r\n\
             \x1b[38;5;200mindexed \x1b[38;2;10;20;30;48;2;200;100;50mrgb\x1b[0m\r\n\
             \x1b[93;104mbright\x1b[0m\r\n\
             \x1b[43m   \x1b[0m  gap  \x1b[46m\x1b[K\x1b[0m\r\n\
             abcdefghijklmnopqrstuvwxyz",
        );
        self.feed(
            B,
            "中文字符 wide\r\n\
             e\u{301}a\u{308} combining 한국어\r\n\
             \x1b[32m日本語\x1b[0mテキストです。改行されます",
        );
        self.feed(
            C,
            "\x1b[45m\x1b[K\x1b[0mmagenta erase\r\n\
             \x1b[1;7m bold inverse \x1b[0m\r\n$ prompt",
        );
    }
}

struct Client {
    parser: vt100::Parser,
    differ: GridDiffer,
}

impl Client {
    fn new(size: Size) -> Self {
        Self {
            parser: vt100::Parser::new(size.rows, size.cols, 0),
            differ: GridDiffer::new(size),
        }
    }

    fn show(&mut self, frame: &Frame) -> Vec<u8> {
        let bytes = self.differ.diff(frame);
        assert!(
            !clears_screen(&bytes),
            "{:?}",
            String::from_utf8_lossy(&bytes)
        );
        self.parser.process(&bytes);
        bytes
    }

    fn resize(&mut self, size: Size) {
        self.parser.screen_mut().set_size(size.rows, size.cols);
        self.differ.set_client_size(size);
    }

    fn cell(&self, row: u16, col: u16) -> Cell {
        Cell::from_vt100(self.parser.screen().cell(row, col).unwrap())
    }

    fn assert_shows(&self, frame: &Frame) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        for row in 0..rows {
            for col in 0..cols {
                let expected = match frame.grid.cell(row, col) {
                    Some(cell) if cell.is_wide() && col + 1 == cols => Cell::blank(cell.style()),
                    Some(cell) => *cell,
                    None => Cell::default(),
                };
                assert_eq!(self.cell(row, col), expected, "cell ({row}, {col})");
            }
        }

        let cursor = frame.cursor.filter(|&(row, col)| row < rows && col < cols);
        if let Some(position) = cursor {
            assert_eq!(screen.cursor_position(), position);
        }
        assert_eq!(
            screen.hide_cursor(),
            cursor.is_none() || frame.modes.hide_cursor
        );
        assert_eq!(
            InputModes {
                hide_cursor: false,
                ..InputModes::from_screen(screen)
            },
            InputModes {
                hide_cursor: false,
                ..frame.modes
            }
        );
    }

    fn assert_shows_pane(&self, window: &Window, pane: PaneId) {
        let rect = window.rect(pane);
        let screen = window.panes[&pane].screen();
        for row in 0..rect.rows {
            for col in 0..rect.cols {
                assert_eq!(
                    self.cell(rect.row + row, rect.col + col),
                    Cell::from_vt100(screen.cell(row, col).unwrap()),
                    "pane {pane} cell ({row}, {col})"
                );
            }
        }
    }
}

fn clears_screen(bytes: &[u8]) -> bool {
    bytes.windows(2).enumerate().any(|(start, pair)| {
        pair == b"\x1b["
            && bytes[start + 2..]
                .iter()
                .find(|byte| !byte.is_ascii_digit())
                .is_some_and(|&byte| byte == b'J')
    })
}

#[test]
fn a_composed_frame_survives_a_round_trip_through_a_terminal() {
    let mut window = Window::three_panes();
    window.fill();
    window.feed(C, "\x1b[3;5H");
    let frame = window.frame();

    let mut client = Client::new(WINDOW);
    client.show(&frame);

    client.assert_shows(&frame);
    for pane in [A, B, C] {
        client.assert_shows_pane(&window, pane);
    }
    assert_eq!(client.parser.screen().cursor_position(), (9, 25));
    assert_eq!(client.cell(0, 20).text(), "│");
    assert_eq!(client.cell(6, 20).text(), "├");
    assert_eq!(client.cell(6, 30).style().fg, Color::Idx(2));
    assert_eq!(client.cell(0, 20).style().fg, Color::Default);
    assert!(client.cell(0, 21).is_wide());
    assert!(client.cell(0, 22).is_wide_continuation());
    assert_eq!(client.cell(1, 21).text(), "e\u{301}");
}

#[test]
fn a_later_frame_is_sent_as_a_diff_that_reproduces_it() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(WINDOW);
    let first = client.show(&window.frame());

    window.feed(A, "\x1b[1;1Hab\x1b[2;3H中\x1b[5;1H\x1b[2K\x1b[35mnew");
    window.feed(
        B,
        "\x1b[1;1Hxy\x1b[1;3H\x1b[1;32m文\x1b[0m\x1b[2;1H\x1b[P\x1b[P",
    );
    window.feed(C, "\r\n\r\n\r\n\x1b[44mscrolled\x1b[0m");
    window.active = A;
    let frame = window.frame();
    let second = client.show(&frame);

    client.assert_shows(&frame);
    for pane in [A, B, C] {
        client.assert_shows_pane(&window, pane);
    }
    assert!(second.len() < first.len());
}

#[test]
fn an_unchanged_frame_sends_nothing() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(WINDOW);
    client.show(&window.frame());

    assert!(client.show(&window.frame()).is_empty());
}

#[test]
fn moving_only_the_cursor_sends_only_a_cursor_move() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(WINDOW);
    client.show(&window.frame());

    window.feed(C, "\x1b[1;1H");
    let frame = window.frame();
    assert_eq!(client.show(&frame), b"\x1b[8;22H");
    client.assert_shows(&frame);
}

#[test]
fn only_the_changed_cells_are_sent() {
    let layout = Layout::new(A);
    let mut panes = BTreeMap::from([(A, vt100::Parser::new(WINDOW.rows, WINDOW.cols, 0))]);
    panes.get_mut(&A).unwrap().process(b"hello world");
    let mut client = Client::new(WINDOW);
    client.show(&compose(&layout, WINDOW, A, &panes));

    panes
        .get_mut(&A)
        .unwrap()
        .process(b"\x1b[1;1HH\x1b[1;7HW\x1b[1;12H");
    let frame = compose(&layout, WINDOW, A, &panes);
    assert_eq!(
        client.show(&frame),
        b"\x1b[?25l\x1b[1HH\x1b[5CW\x1b[4C\x1b[?25h"
    );
    client.assert_shows(&frame);

    panes.get_mut(&A).unwrap().process(b"\x1b[1;7H\x1b[31;1mw");
    let frame = compose(&layout, WINDOW, A, &panes);
    assert_eq!(
        client.show(&frame),
        b"\x1b[?25l\x1b[1;7H\x1b[1;31mw\x1b[0m\x1b[?25h"
    );
    client.assert_shows(&frame);
}

#[test]
fn a_full_redraw_clears_line_by_line() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(WINDOW);
    client.parser.process("#".repeat(40 * 12).as_bytes());

    let bytes = client.show(&window.frame());
    client.assert_shows(&window.frame());
    assert!(bytes.windows(3).any(|seq| seq == b"\x1b[K"));

    client.differ.reset();
    client.parser.process(b"\x1b[1;1Hjunk\x1b[?1h\x1b[?25l");
    client.show(&window.frame());
    client.assert_shows(&window.frame());
}

#[test]
fn output_is_clipped_to_a_smaller_client() {
    let mut window = Window::three_panes();
    window.fill();
    window.feed(A, "\x1b[8;14H\x1b[41m中\x1b[0m");
    let small = Size { rows: 8, cols: 14 };
    let mut client = Client::new(small);
    let frame = window.frame();
    client.show(&frame);

    client.assert_shows(&frame);
    assert!(client.cell(7, 13).is_erased());
    assert_eq!(client.cell(7, 13).style().bg, Color::Idx(1));
    assert!(client.parser.screen().hide_cursor());

    window.feed(A, "\x1b[1;1HCHANGED\x1b[20;1Hbelow\x1b[2;3H");
    window.active = A;
    let frame = window.frame();
    client.show(&frame);
    client.assert_shows(&frame);
    assert!(!client.parser.screen().hide_cursor());
}

#[test]
fn a_client_larger_than_the_window_sees_blanks_outside_it() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(Size { rows: 8, cols: 14 });
    client.show(&window.frame());

    client.resize(Size { rows: 20, cols: 50 });
    client.parser.process("@".repeat(50 * 20).as_bytes());
    let frame = window.frame();
    client.show(&frame);
    client.assert_shows(&frame);
    assert!(client.cell(15, 45).is_erased());
}

#[test]
fn modes_are_sent_only_when_they_change() {
    let mut window = Window::three_panes();
    let mut client = Client::new(WINDOW);
    let first = client.show(&window.frame());
    assert!(first.starts_with(b"\x1b[?1l\x1b>\x1b[?2004l\x1b[?9;1000;1002;1003l\x1b[?1005;1006l"));
    client.assert_shows(&window.frame());

    window.feed(C, "\x1b[?2004h");
    assert_eq!(client.show(&window.frame()), b"\x1b[?2004h");

    window.feed(C, "\x1b[?1h\x1b=\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        client.show(&window.frame()),
        b"\x1b[?1h\x1b=\x1b[?1000h\x1b[?1006h"
    );
    client.assert_shows(&window.frame());

    window.feed(C, "\x1b[?1002h\x1b[?1005h");
    assert_eq!(
        client.show(&window.frame()),
        b"\x1b[?1000l\x1b[?1002h\x1b[?1006l\x1b[?1005h"
    );
    client.assert_shows(&window.frame());

    window.feed(C, "\x1b[?1002l\x1b[?1005l\x1b[?1l\x1b>\x1b[?2004l");
    assert_eq!(
        client.show(&window.frame()),
        b"\x1b[?1l\x1b>\x1b[?2004l\x1b[?1002l\x1b[?1005l"
    );
    let frame = window.frame();
    client.assert_shows(&frame);
    assert_eq!(frame.modes, InputModes::default());
}

#[test]
fn switching_the_active_pane_switches_its_modes() {
    let mut window = Window::three_panes();
    window.feed(A, "\x1b[?1003h\x1b[?1006h");
    let mut client = Client::new(WINDOW);
    client.show(&window.frame());
    assert_eq!(
        client.parser.screen().mouse_protocol_mode(),
        MouseProtocolMode::None
    );

    window.active = A;
    let frame = window.frame();
    client.show(&frame);
    client.assert_shows(&frame);
    assert_eq!(
        client.parser.screen().mouse_protocol_mode(),
        MouseProtocolMode::AnyMotion
    );
    assert_eq!(
        client.parser.screen().mouse_protocol_encoding(),
        MouseProtocolEncoding::Sgr
    );
}

#[test]
fn a_hidden_cursor_stays_hidden_until_the_pane_shows_it() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(WINDOW);
    client.show(&window.frame());
    assert!(!client.parser.screen().hide_cursor());

    window.feed(C, "\x1b[?25l");
    assert_eq!(client.show(&window.frame()), b"\x1b[?25l");

    window.feed(C, "more");
    let hidden = client.show(&window.frame());
    assert!(!hidden.windows(6).any(|seq| seq == b"\x1b[?25h"));
    client.assert_shows(&window.frame());

    window.feed(C, "\x1b[?25h");
    assert_eq!(client.show(&window.frame()), b"\x1b[?25h");
}

#[test]
fn drawing_hides_the_cursor_and_puts_it_back() {
    let mut window = Window::three_panes();
    window.fill();
    let mut client = Client::new(WINDOW);
    client.show(&window.frame());

    window.feed(A, "\x1b[1;1Hx");
    let bytes = client.show(&window.frame());
    assert!(bytes.starts_with(b"\x1b[?25l"));
    assert!(bytes.ends_with(b"\x1b[?25h"));
    client.assert_shows(&window.frame());
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }
}

fn random_input(random: &mut Random, rect: Rect) -> String {
    let words = [
        "hello",
        "中",
        "文字",
        "e\u{301}",
        "a\u{308}\u{323}",
        "한",
        " ",
        "x",
        "日本語",
        "|",
    ];
    let styles = [
        "\x1b[0m",
        "\x1b[1m",
        "\x1b[2m",
        "\x1b[3m",
        "\x1b[4m",
        "\x1b[7m",
        "\x1b[22m",
        "\x1b[27m",
        "\x1b[31m",
        "\x1b[44m",
        "\x1b[38;5;123m",
        "\x1b[48;2;1;2;3m",
        "\x1b[39m",
        "\x1b[49m",
        "\x1b[95m",
    ];
    let edits = [
        "\x1b[K", "\x1b[1K", "\x1b[2K", "\x1b[J", "\x1b[2X", "\x1b[3P", "\x1b[2@", "\x1b[L",
        "\x1b[M", "\r\n", "\r", "\x08", "\t",
    ];
    let mut input = String::new();
    for _ in 0..random.below(12) {
        match random.below(4) {
            0 | 1 => input.push_str(random.pick(&words)),
            2 => input.push_str(random.pick(&styles)),
            _ if random.below(3) == 0 => input.push_str(&format!(
                "\x1b[{};{}H",
                random.below(u64::from(rect.rows)) + 1,
                random.below(u64::from(rect.cols)) + 1
            )),
            _ => input.push_str(random.pick(&edits)),
        }
    }
    input
}

#[test]
fn random_updates_always_reproduce_the_frame() {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    let mut window = Window::three_panes();
    let mut client = Client::new(WINDOW);
    let sizes = [
        WINDOW,
        Size { rows: 7, cols: 23 },
        Size { rows: 15, cols: 45 },
        Size { rows: 12, cols: 21 },
    ];

    for _ in 0..400 {
        for pane in [A, B, C] {
            if random.below(3) > 0 {
                let input = random_input(&mut random, window.rect(pane));
                window.feed(pane, &input);
            }
        }
        match random.below(20) {
            0 => window.active = [A, B, C][random.below(3) as usize],
            1 => client.resize(sizes[random.below(4) as usize]),
            2 => client.differ.reset(),
            _ => {}
        }
        let frame = window.frame();
        client.show(&frame);
        if client.differ.client_size() == WINDOW {
            for pane in [A, B, C] {
                client.assert_shows_pane(&window, pane);
            }
        }
        client.assert_shows(&frame);
    }
}

#[test]
fn sent_bytes_never_clear_the_whole_screen() {
    assert!(clears_screen(b"ab\x1b[2Jcd"));
    assert!(clears_screen(b"\x1b[J"));
    assert!(!clears_screen(b"\x1b[K\x1b[2X\x1b[10;1H"));
}
