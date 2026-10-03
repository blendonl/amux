use std::collections::HashMap;

const ROWS: u16 = 4;
const COLS: u16 = 10;
const PLACEHOLDER: &str = "\u{10EEEE}\u{0305}\u{030D}";

fn stamps(screen: &vt100::Screen) -> Vec<(u64, u64)> {
    screen
        .visible_rows()
        .map(|row| (row.id, row.stamp))
        .collect()
}

fn filled() -> vt100::Parser {
    let mut parser = vt100::Parser::new(ROWS, COLS, 10);
    parser.process(b"zero\r\none\r\ntwo\r\nthree");
    parser
}

fn ids(stamps: &[(u64, u64)]) -> Vec<u64> {
    stamps.iter().map(|&(id, _)| id).collect()
}

fn restamped(
    parser: &mut vt100::Parser,
    change: impl FnOnce(&mut vt100::Parser),
) -> Vec<u16> {
    let before = stamps(parser.screen());
    change(parser);
    let after = stamps(parser.screen());
    assert_eq!(ids(&after), ids(&before));
    (0..)
        .zip(before.iter().zip(&after))
        .filter(|(_, (before, after))| before != after)
        .map(|(row, _)| row)
        .collect()
}

fn restamped_by(input: &str) -> Vec<u16> {
    let mut parser = filled();
    restamped(&mut parser, |parser| parser.process(input.as_bytes()))
}

#[test]
fn printing_restamps_only_its_row() {
    assert_eq!(restamped_by("\x1b[2;3Hx"), [1]);
    assert_eq!(restamped_by("\x1b[3;1H中"), [2]);
    assert_eq!(restamped_by(&format!("\x1b[4;1H{PLACEHOLDER}")), [3]);
}

#[test]
fn erasing_in_a_line_restamps_only_its_row() {
    for mode in ["", "1", "2"] {
        assert_eq!(restamped_by(&format!("\x1b[2;3H\x1b[{mode}K")), [1]);
    }
}

#[test]
fn erasing_in_the_display_restamps_only_the_rows_it_erases() {
    assert_eq!(restamped_by("\x1b[2;3H\x1b[J"), [1, 2, 3]);
    assert_eq!(restamped_by("\x1b[2;3H\x1b[1J"), [0, 1]);
    assert_eq!(restamped_by("\x1b[2;3H\x1b[2J"), [0, 1, 2, 3]);
}

#[test]
fn erasing_inserting_and_deleting_characters_restamp_only_their_row() {
    assert_eq!(restamped_by("\x1b[2;2H\x1b[2X"), [1]);
    assert_eq!(restamped_by("\x1b[2;2H\x1b[2@"), [1]);
    assert_eq!(restamped_by("\x1b[2;2H\x1b[2P"), [1]);
}

#[test]
fn a_wrap_restamps_the_row_it_leaves_and_the_row_it_enters() {
    let mut parser = filled();
    parser.process(b"\x1b[2;1H0123456789");
    let rows = restamped(&mut parser, |parser| parser.process(b"x"));
    assert_eq!(rows, [1, 2]);
    assert!(parser.screen().row_wrapped(1));
}

#[test]
fn rows_pushed_down_keep_their_stamps() {
    for input in ["\x1b[2;1H\x1b[L", "\x1b[2;4r\x1b[T"] {
        let mut parser = filled();
        let before = stamps(parser.screen());
        parser.process(input.as_bytes());
        let after = stamps(parser.screen());
        assert_eq!(after[0], before[0], "{input:?}");
        assert_eq!(after[2..], before[1..3], "{input:?}");
    }
}

#[test]
fn a_wrapped_row_pushed_to_the_bottom_is_restamped() {
    let mut parser = filled();
    parser.process(b"\x1b[3;1H0123456789x");
    assert!(parser.screen().row_wrapped(2));
    let before = stamps(parser.screen());
    parser.process(b"\x1b[2;1H\x1b[L");
    let after = stamps(parser.screen());
    assert_eq!(after[3].0, before[2].0);
    assert_ne!(after[3].1, before[2].1);
    assert!(!parser.screen().row_wrapped(3));
}

#[test]
fn a_resize_restamps_screen_rows_but_not_history_rows() {
    let mut parser = filled();
    parser.process(b"\r\nfour\r\nfive");
    parser.screen_mut().set_scrollback(1);
    let rows = restamped(&mut parser, |parser| {
        parser.screen_mut().set_size(ROWS, COLS + 2);
    });
    assert_eq!(rows, [1, 2, 3]);
}

#[test]
fn marking_a_graphic_restamps_only_its_row() {
    let mut parser = filled();
    let rows = restamped(&mut parser, |parser| {
        parser.screen_mut().mark_graphic(2, 1..4);
    });
    assert_eq!(rows, [2]);
}

#[test]
fn moving_the_cursor_and_setting_attributes_restamp_nothing() {
    assert_eq!(
        restamped_by(
            "\x1b[3;5H\x1b[A\x1b[2C\r\x1b[B\x08\t\x1b7\x1b[1;1H\x1b8\
             \x1b[31;1m\x1b[m\x1b[2;3r\x1b[r"
        ),
        [] as [u16; 0]
    );
}

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(bound).unwrap()).unwrap()
    }

    fn up_to(&mut self, most: u16) -> u16 {
        u16::try_from(self.below(usize::from(most) + 1)).unwrap()
    }

    fn pick<'a>(&mut self, choices: &[&'a str]) -> &'a str {
        choices[self.below(choices.len())]
    }
}

const WIDE_TEXT: &[&str] = &["中", "\u{1F600}", "0123中文字"];

const TEXT: &[&str] = &[
    "a",
    "Z",
    " ",
    "e\u{301}",
    PLACEHOLDER,
    "0123456789",
    "\r",
    "\n",
    "\r\n",
    "\x08",
    "\t",
    "\x1bM",
    "\x1b[31m",
    "\x1b[44;1m",
    "\x1b[m",
    "\x1b[K",
    "\x1b[1K",
    "\x1b[2K",
    "\x1b[J",
    "\x1b[1J",
    "\x1b[2J",
    "\x1b[r",
    "\x1b[?1049h",
    "\x1b[?1049l",
];

const COUNTED: &[&str] = &["X", "@", "P", "L", "M", "S", "T", "A", "B", "C"];

fn random_step(parser: &mut vt100::Parser, random: &mut Random) {
    let (rows, cols) = parser.screen().size();
    match random.below(8) {
        0 => parser.process(random.pick(WIDE_TEXT).as_bytes()),
        1 | 2 => parser.process(random.pick(TEXT).as_bytes()),
        3 => {
            let row = random.up_to(rows) + 1;
            let col = random.up_to(cols) + 1;
            parser.process(format!("\x1b[{row};{col}H").as_bytes());
        }
        4 => {
            let count = random.up_to(cols);
            let code = random.pick(COUNTED);
            parser.process(format!("\x1b[{count}{code}").as_bytes());
        }
        5 => {
            let top = random.up_to(rows);
            let bottom = random.up_to(rows) + 1;
            parser.process(format!("\x1b[{top};{bottom}r").as_bytes());
        }
        6 => {
            let row = random.up_to(rows);
            let start = random.up_to(cols);
            let end = random.up_to(cols + 1);
            match random.below(3) {
                0 => parser.screen_mut().mark_graphic(row, start..end),
                1 => parser.screen_mut().set_scrollback(random.below(8)),
                _ => parser.process(b"\x1bc"),
            }
        }
        _ => {
            let rows = random.up_to(4) + 2;
            let cols = random.up_to(9) + 3;
            parser.screen_mut().set_size(rows, cols);
        }
    }
}

#[derive(Debug, PartialEq)]
struct Contents {
    cells: Vec<vt100::Cell>,
    wrapped: bool,
    has_placeholders: bool,
}

fn random_walk(mut check: impl FnMut(&vt100::Screen, u64, usize)) {
    for seed in 1..=8 {
        let mut random = Random(0x9E37_79B9_7F4A_7C15 ^ seed);
        let mut parser = vt100::Parser::new(ROWS, COLS, 6);
        for step in 0..4000 {
            random_step(&mut parser, &mut random);
            check(parser.screen(), seed, step);
        }
    }
}

#[test]
fn equal_stamps_mean_equal_rows() {
    let mut seen: HashMap<(u64, u64), Contents> = HashMap::new();
    random_walk(|screen, seed, step| {
        for row in screen.visible_rows() {
            let contents = Contents {
                cells: row.cells.to_vec(),
                wrapped: row.wrapped,
                has_placeholders: row.has_placeholders,
            };
            match seen.get(&(row.id, row.stamp)) {
                Some(earlier) => assert_eq!(
                    earlier, &contents,
                    "seed {seed}, step {step}, row {}",
                    row.id
                ),
                None => {
                    seen.insert((row.id, row.stamp), contents);
                }
            }
        }
    });
}

#[test]
fn every_wide_character_keeps_both_halves() {
    random_walk(|screen, seed, step| {
        for row in screen.visible_rows() {
            let heads = std::iter::once(false)
                .chain(row.cells.iter().map(vt100::Cell::is_wide));
            let tails = row
                .cells
                .iter()
                .map(vt100::Cell::is_wide_continuation)
                .chain([false]);
            assert!(
                heads.eq(tails),
                "seed {seed}, step {step}, row {}",
                row.id
            );
        }
    });
}

#[test]
fn visible_rows_match_cell_lookups() {
    let mut parser = vt100::Parser::new(ROWS, COLS, 10);
    parser.process(
        format!(
            "\x1b[41mred\x1b[K\x1b[m\r\n中文 wide\r\n0123456789wrapped\r\n\
             {PLACEHOLDER} image\r\nplain"
        )
        .as_bytes(),
    );
    parser.screen_mut().set_size(ROWS, COLS - 3);
    parser.process(b"\r\nnarrow\r\nrows\r\n");
    parser.screen_mut().set_size(ROWS, COLS);
    parser.process("tail 中".as_bytes());

    let history = parser.screen().history_len();
    assert!(history > usize::from(ROWS));
    for offset in 0..=history + 1 {
        parser.screen_mut().set_scrollback(offset);
        let screen = parser.screen();
        let rows: Vec<vt100::VisibleRow<'_>> =
            screen.visible_rows().collect();
        assert_eq!(rows.len(), usize::from(ROWS), "offset {offset}");
        assert_eq!(screen.visible_row(ROWS), None, "offset {offset}");
        let ids: Vec<u64> = screen.row_ids().collect();
        for (index, row) in (0..).zip(&rows) {
            assert_eq!(screen.visible_row(index).as_ref(), Some(row));
            assert_eq!(row.id, ids[usize::from(index)]);
            assert_eq!(row.wrapped, screen.row_wrapped(index));
            assert_eq!(
                row.has_placeholders,
                screen.row_has_placeholders(index)
            );
            for col in 0..COLS + 2 {
                assert_eq!(
                    row.cells.get(usize::from(col)),
                    screen.cell(index, col),
                    "offset {offset}, cell ({index}, {col})"
                );
            }
        }
    }
}
