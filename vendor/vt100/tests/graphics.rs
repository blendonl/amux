const PLACEHOLDER: &str = "\u{10EEEE}\u{0305}\u{030D}\u{030E}";

fn parser(rows: u16, cols: u16, input: &str) -> vt100::Parser {
    let mut parser = vt100::Parser::new(rows, cols, 10);
    parser.process(input.as_bytes());
    parser
}

fn flagged(screen: &vt100::Screen) -> Vec<u16> {
    (0..screen.size().0)
        .filter(|&row| screen.row_has_placeholders(row))
        .collect()
}

fn graphic(screen: &vt100::Screen, row: u16) -> Vec<u16> {
    (0..screen.size().1)
        .filter(|&col| screen.cell(row, col).unwrap().is_graphic())
        .collect()
}

#[test]
fn a_placeholder_flags_its_row_and_keeps_its_diacritics() {
    let parser = parser(3, 10, &format!("a\r\n{PLACEHOLDER}"));
    let screen = parser.screen();
    assert_eq!(flagged(screen), [1]);
    assert_eq!(screen.cell(1, 0).unwrap().contents(), PLACEHOLDER);
    assert_eq!(screen.cell(1, 0).unwrap().contents().chars().count(), 4);
    assert_eq!(screen.cursor_position(), (1, 1));
}

#[test]
fn a_placeholder_keeps_four_byte_diacritics() {
    let placeholder = "\u{10EEEE}\u{1D242}\u{1D243}\u{1D244}";
    let parser = parser(3, 10, placeholder);
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), placeholder);
}

#[test]
fn a_placeholder_in_the_last_column_keeps_its_diacritics() {
    let parser = parser(2, 3, &format!("ab{PLACEHOLDER}"));
    let screen = parser.screen();
    assert_eq!(screen.cell(0, 2).unwrap().contents(), PLACEHOLDER);
    assert_eq!(flagged(screen), [0]);
}

#[test]
fn plain_text_does_not_flag_rows() {
    let parser = parser(3, 10, "hello\r\n\u{2588}e\u{0301}");
    assert_eq!(flagged(parser.screen()), [] as [u16; 0]);
}

#[test]
fn the_flag_moves_with_its_row() {
    let mut parser = parser(3, 10, &format!("\x1b[3;1H{PLACEHOLDER}"));
    parser.process(b"\n");
    assert_eq!(flagged(parser.screen()), [1]);
    parser.process(b"\x1b[1;1H\x1b[L");
    assert_eq!(flagged(parser.screen()), [2]);
}

#[test]
fn clearing_the_whole_row_drops_the_flag() {
    for erase in [
        "\x1b[2K",
        "\x1b[2J",
        "\x1b[1;1H\x1b[J",
        "\x1b[3;1H\x1b[1J",
        "\x1bc",
    ] {
        let mut parser = parser(3, 10, &format!("\r\n{PLACEHOLDER}x"));
        parser.process(erase.as_bytes());
        assert_eq!(flagged(parser.screen()), [] as [u16; 0], "{erase:?}");
    }
}

#[test]
fn erasing_part_of_the_row_keeps_the_flag() {
    for erase in ["\x1b[K", "\x1b[1K", "\x1b[3X", "\x1b[P"] {
        let mut parser = parser(3, 10, &format!("{PLACEHOLDER}xyz\x1b[2G"));
        parser.process(erase.as_bytes());
        assert_eq!(flagged(parser.screen()), [0], "{erase:?}");
    }
}

#[test]
fn mark_graphic_sets_the_bit_on_existing_cells() {
    let mut parser = parser(3, 10, "");
    let screen = parser.screen_mut();
    screen.mark_graphic(1, 2..5);
    screen.mark_graphic(2, 8..20);
    screen.mark_graphic(9, 0..3);
    assert_eq!(graphic(screen, 0), [] as [u16; 0]);
    assert_eq!(graphic(screen, 1), [2, 3, 4]);
    assert_eq!(graphic(screen, 2), [8, 9]);
    assert!(!screen.cell(1, 2).unwrap().has_contents());
}

#[test]
fn printing_over_a_graphic_cell_clears_the_bit() {
    let mut parser = parser(3, 10, "");
    parser.screen_mut().mark_graphic(0, 0..10);
    parser.process("\x1b[1;3Hab\x1b[1;7H\u{4e2d}".as_bytes());
    assert_eq!(graphic(parser.screen(), 0), [0, 1, 4, 5, 8, 9]);
}

#[test]
fn a_combining_mark_clears_the_bit_of_the_cell_it_joins() {
    let mut parser = parser(3, 10, "\x1b[1;2H");
    parser.screen_mut().mark_graphic(0, 0..3);
    parser.process("\u{0301}".as_bytes());
    assert_eq!(graphic(parser.screen(), 0), [1, 2]);
}

#[test]
fn erasing_clears_the_bit() {
    for (erase, left) in [
        ("\x1b[2J", vec![]),
        ("\x1b[J", vec![0, 1, 2]),
        ("\x1b[1J", vec![4, 5, 6, 7, 8, 9]),
        ("\x1b[K", vec![0, 1, 2]),
        ("\x1b[1K", vec![4, 5, 6, 7, 8, 9]),
        ("\x1b[2K", vec![]),
        ("\x1b[2X", vec![0, 1, 2, 5, 6, 7, 8, 9]),
    ] {
        let mut parser = parser(3, 10, "\x1b[1;4H");
        parser.screen_mut().mark_graphic(0, 0..10);
        parser.process(erase.as_bytes());
        assert_eq!(graphic(parser.screen(), 0), left, "{erase:?}");
    }
}

#[test]
fn linefeed_scrolls_at_the_bottom_like_lf() {
    let mut parser = parser(3, 10, "\x1b[3;4H");
    let before: Vec<u64> = parser.screen().row_ids().collect();
    parser.screen_mut().linefeed();
    let after: Vec<u64> = parser.screen().row_ids().collect();
    assert_eq!(after[..2], before[1..]);
    assert!(!before.contains(&after[2]));
    assert_eq!(parser.screen().cursor_position(), (2, 3));
}

#[test]
fn linefeed_honours_the_scroll_region() {
    let mut parser = parser(5, 10, "\x1b[2;3r\x1b[3;1H");
    let before: Vec<u64> = parser.screen().row_ids().collect();
    parser.screen_mut().linefeed();
    let after: Vec<u64> = parser.screen().row_ids().collect();
    assert_eq!(after[1], before[2]);
    assert_eq!(after[3..], before[3..]);
    assert_eq!(parser.screen().cursor_position(), (2, 0));
}

#[test]
fn set_cursor_col_moves_and_clamps() {
    let mut parser = parser(3, 10, "\x1b[2;2H");
    parser.screen_mut().set_cursor_col(6);
    assert_eq!(parser.screen().cursor_position(), (1, 6));
    parser.screen_mut().set_cursor_col(40);
    assert_eq!(parser.screen().cursor_position(), (1, 9));
}

#[test]
fn scroll_region_reports_the_margins() {
    let mut parser = parser(5, 10, "");
    assert_eq!(parser.screen().scroll_region(), (0, 4));
    parser.process(b"\x1b[2;4r");
    assert_eq!(parser.screen().scroll_region(), (1, 3));
    parser.process(b"\x1b[r");
    assert_eq!(parser.screen().scroll_region(), (0, 4));
}
