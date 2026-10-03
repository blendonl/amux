const ROWS: u16 = 3;
const COLS: u16 = 10;
const CUT: u16 = COLS - 1;
const WIDE_AT_THE_CUT: &str = "abcdefgh中";
const PRINT_AT_THE_EDGE: &str = "\x1b[1;9Hx";

fn cut(scrollback: usize, input: &str) -> vt100::Parser {
    let mut parser = vt100::Parser::new(ROWS, COLS, scrollback);
    parser.process(input.as_bytes());
    parser.screen_mut().set_size(ROWS, CUT);
    parser
}

fn assert_cut_cell_is_blank(cell: &vt100::Cell) {
    assert!(!cell.is_wide());
    assert!(!cell.is_wide_continuation());
    assert!(!cell.has_contents());
}

#[test]
fn printing_at_the_edge_after_a_resize_cuts_a_wide_character() {
    let mut parser = cut(0, WIDE_AT_THE_CUT);
    parser.process(PRINT_AT_THE_EDGE.as_bytes());
    let screen = parser.screen();
    assert_eq!(screen.contents(), "abcdefghx");
    let edge = screen.cell(0, CUT - 1).unwrap();
    assert_eq!(edge.contents(), "x");
    assert!(!edge.is_wide());
    assert_eq!(screen.cursor_position(), (0, CUT));
}

#[test]
fn a_wide_character_cut_by_a_resize_becomes_a_blank_with_its_attributes() {
    let parser = cut(0, &format!("\x1b[31m{WIDE_AT_THE_CUT}"));
    let screen = parser.screen();
    assert_eq!(screen.contents(), "abcdefgh");
    let edge = screen.cell(0, CUT - 1).unwrap();
    assert_cut_cell_is_blank(edge);
    assert_eq!(edge.fgcolor(), vt100::Color::Idx(1));
    assert_eq!(screen.cell(0, CUT), None);
}

#[test]
fn erasing_or_deleting_at_the_edge_after_a_resize_cuts_a_wide_character() {
    for input in ["\x1b[1;9H\x1b[K", "\x1b[1;9H\x1b[X", "\x1b[1;9H\x1b[P"] {
        let mut parser = cut(0, WIDE_AT_THE_CUT);
        parser.process(input.as_bytes());
        let screen = parser.screen();
        assert_eq!(screen.contents(), "abcdefgh", "{input:?}");
        assert_cut_cell_is_blank(screen.cell(0, CUT - 1).unwrap());
    }
}

#[test]
fn a_wide_character_ending_at_the_new_edge_stays_whole() {
    let mut parser = cut(0, "abcdefg中");
    let screen = parser.screen();
    assert_eq!(screen.contents(), "abcdefg中");
    let wide = screen.cell(0, CUT - 2).unwrap();
    assert_eq!(wide.contents(), "中");
    assert!(wide.is_wide());
    assert!(screen.cell(0, CUT - 1).unwrap().is_wide_continuation());

    parser.process(PRINT_AT_THE_EDGE.as_bytes());
    assert_eq!(parser.screen().contents(), "abcdefg x");
}

#[test]
fn a_wide_character_cut_in_either_grid_is_repaired() {
    let wide = WIDE_AT_THE_CUT;
    let edge = PRINT_AT_THE_EDGE;
    let cases = [
        (
            "the active alternate screen",
            format!("\x1b[?1049h{wide}"),
            edge.to_owned(),
            true,
        ),
        (
            "the hidden alternate screen",
            format!("\x1b[?47h{wide}\x1b[?47l"),
            format!("\x1b[?47h{edge}"),
            true,
        ),
        (
            "the hidden main screen",
            format!("{wide}\x1b[?47h"),
            format!("\x1b[?47l{edge}"),
            false,
        ),
    ];
    for (grid, before_resize, after_resize, alternate) in cases {
        let mut parser = cut(0, &before_resize);
        parser.process(after_resize.as_bytes());
        let screen = parser.screen();
        assert_eq!(screen.alternate_screen(), alternate, "{grid}");
        assert_eq!(screen.contents(), "abcdefghx", "{grid}");
    }
}

#[test]
fn a_wide_character_cut_before_scrolling_into_history_stays_blank() {
    let mut parser = cut(10, WIDE_AT_THE_CUT);
    parser.process(b"\n\n\n");
    parser.screen_mut().set_size(ROWS, COLS);
    parser.screen_mut().set_scrollback(1);
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 1);
    assert_eq!(screen.contents(), "abcdefgh");
    assert_eq!(screen.visible_row(0).unwrap().cells.len(), usize::from(CUT));
    assert_cut_cell_is_blank(screen.cell(0, CUT - 1).unwrap());
    assert_cut_cell_is_blank(screen.line_cell(0, CUT - 1).unwrap());
    assert_eq!(screen.cell(0, CUT), None);
}
