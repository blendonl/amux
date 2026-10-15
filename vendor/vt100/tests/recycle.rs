const ROWS: u16 = 4;
const COLS: u16 = 10;
const PLACEHOLDER: &str = "\u{10EEEE}\u{0305}\u{030D}";

struct Recycling {
    name: &'static str,
    scrollback: usize,
    dirty_row: u16,
    recycle: &'static str,
    recycled_row: u16,
}

const RECYCLINGS: &[Recycling] = &[
    Recycling {
        name: "a linefeed without history",
        scrollback: 0,
        dirty_row: 0,
        recycle: "\x1b[4;1H\n",
        recycled_row: 3,
    },
    Recycling {
        name: "a linefeed with full history",
        scrollback: 1,
        dirty_row: 0,
        recycle: "\x1b[4;1H\n\n",
        recycled_row: 3,
    },
    Recycling {
        name: "a linefeed in a scroll region",
        scrollback: 10,
        dirty_row: 0,
        recycle: "\x1b[1;3r\x1b[3;1H\n",
        recycled_row: 2,
    },
    Recycling {
        name: "scrolling down",
        scrollback: 10,
        dirty_row: 1,
        recycle: "\x1b[1;2r\x1b[T",
        recycled_row: 0,
    },
    Recycling {
        name: "inserting a line",
        scrollback: 10,
        dirty_row: 1,
        recycle: "\x1b[1;2r\x1b[1;1H\x1b[L",
        recycled_row: 0,
    },
    Recycling {
        name: "deleting a line",
        scrollback: 10,
        dirty_row: 0,
        recycle: "\x1b[1;1H\x1b[M",
        recycled_row: 3,
    },
];

fn blank() -> vt100::Cell {
    vt100::Parser::new(1, 1, 0)
        .screen()
        .cell(0, 0)
        .unwrap()
        .clone()
}

fn ids(screen: &vt100::Screen) -> Vec<u64> {
    screen.row_ids().collect()
}

fn assert_blank(screen: &vt100::Screen, row: u16, case: &str) {
    let blank = blank();
    for col in 0..COLS {
        assert_eq!(
            screen.cell(row, col),
            Some(&blank),
            "{case}: cell ({row}, {col})"
        );
    }
    assert!(screen.cell(row, COLS).is_none(), "{case}: row is too wide");
    assert!(!screen.row_wrapped(row), "{case}: row still wraps");
    assert!(
        !screen.row_has_placeholders(row),
        "{case}: row still has placeholders"
    );
}

fn assert_recycled_blank(dirty: impl Fn(&mut vt100::Parser, u16)) {
    for recycling in RECYCLINGS {
        let mut parser = vt100::Parser::new(ROWS, COLS, recycling.scrollback);
        parser.process(
            format!("\x1b[{};1H", recycling.dirty_row + 1).as_bytes(),
        );
        dirty(&mut parser, recycling.dirty_row);
        let before = ids(parser.screen());
        parser.process(recycling.recycle.as_bytes());

        let screen = parser.screen();
        let id = ids(screen)[usize::from(recycling.recycled_row)];
        assert!(!before.contains(&id), "{}: id {id} reused", recycling.name);
        assert_blank(screen, recycling.recycled_row, recycling.name);
    }
}

#[test]
fn a_recycled_row_comes_back_blank_after_a_coloured_erase() {
    assert_recycled_blank(|parser, _| {
        parser.process(b"\x1b[41m\x1b[K\x1b[m");
    });
}

#[test]
fn a_recycled_row_comes_back_blank_after_wide_characters() {
    assert_recycled_blank(|parser, _| {
        parser.process("中文字".as_bytes());
    });
}

#[test]
fn a_recycled_row_comes_back_blank_after_a_wrap() {
    assert_recycled_blank(|parser, row| {
        parser.process(b"0123456789x");
        assert!(parser.screen().row_wrapped(row));
    });
}

#[test]
fn a_recycled_row_comes_back_blank_after_a_placeholder() {
    assert_recycled_blank(|parser, row| {
        parser.process(PLACEHOLDER.as_bytes());
        assert!(parser.screen().row_has_placeholders(row));
    });
}

#[test]
fn a_recycled_row_comes_back_blank_after_a_graphic_mark() {
    assert_recycled_blank(|parser, row| {
        parser.screen_mut().mark_graphic(row, 0..COLS);
        assert!(parser.screen().cell(row, COLS - 1).unwrap().is_graphic());
    });
}

#[test]
fn a_history_row_narrowed_by_a_resize_comes_back_full_width() {
    let mut parser = vt100::Parser::new(ROWS, COLS, 1);
    parser.process(b"\x1b[31m0123456789");
    parser.screen_mut().set_size(ROWS, COLS / 2);
    parser.process(b"\x1b[4;1H\n");
    parser.screen_mut().set_size(ROWS, COLS);
    parser.process(b"\n");
    assert_eq!(parser.screen().history_len(), 1);
    assert_blank(parser.screen(), ROWS - 1, "narrowed history row");
}

#[test]
fn a_history_row_wider_than_the_screen_comes_back_narrowed() {
    let mut parser = vt100::Parser::new(ROWS, COLS * 2, 1);
    parser.process("\x1b[41m\x1b[K0123456789中文字".as_bytes());
    parser.process(b"\x1b[4;1H\n");
    parser.screen_mut().set_size(ROWS, COLS);
    parser.process(b"\n");
    assert_eq!(parser.screen().history_len(), 1);
    assert_blank(parser.screen(), ROWS - 1, "widened history row");
}
