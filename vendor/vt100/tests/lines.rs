fn parser(
    rows: u16,
    cols: u16,
    scrollback: usize,
    input: &str,
) -> vt100::Parser {
    let mut parser = vt100::Parser::new(rows, cols, scrollback);
    parser.process(input.as_bytes());
    parser
}

fn numbered(count: usize) -> String {
    (1..=count)
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn lines(screen: &vt100::Screen) -> Vec<String> {
    let (rows, cols) = screen.size();
    (0..screen.history_len() + usize::from(rows))
        .map(|line| screen.line_contents(line, 0, cols))
        .collect()
}

#[test]
fn a_screen_without_history_starts_at_its_first_row() {
    let parser = parser(3, 10, 10, "one\r\ntwo");
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 0);
    assert_eq!(lines(screen), ["one", "two", ""]);
    assert_eq!(screen.line_cell(1, 0).unwrap().contents(), "t");
    assert!(screen.line_cell(3, 0).is_none());
    assert!(!screen.line_wrapped(3));
    assert_eq!(screen.line_contents(3, 0, 10), "");
}

#[test]
fn history_lines_come_before_the_screen_rows() {
    let parser = parser(3, 10, 10, &numbered(5));
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 2);
    assert_eq!(lines(screen), ["1", "2", "3", "4", "5"]);
    assert_eq!(screen.line_cell(0, 0).unwrap().contents(), "1");
    assert_eq!(screen.line_cell(4, 0).unwrap().contents(), "5");
    assert!(screen.line_cell(5, 0).is_none());
}

#[test]
fn full_history_keeps_only_the_newest_lines() {
    let parser = parser(3, 10, 4, &numbered(20));
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 4);
    assert_eq!(lines(screen), ["14", "15", "16", "17", "18", "19", "20"]);
}

#[test]
fn the_alternate_screen_has_no_history() {
    let mut parser = parser(3, 10, 10, &numbered(5));
    parser.process(b"\x1b[?1049h\x1b[Halt");
    assert_eq!(parser.screen().history_len(), 0);
    assert_eq!(lines(parser.screen()), ["alt", "", ""]);

    parser.process(b"\x1b[?1049l");
    assert_eq!(parser.screen().history_len(), 2);
    assert_eq!(lines(parser.screen()), ["1", "2", "3", "4", "5"]);
}

#[test]
fn wrapped_lines_keep_their_flag_in_history() {
    let parser = parser(2, 4, 10, "abcdefgh\r\nij\r\nkl");
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 2);
    assert_eq!(lines(screen), ["abcd", "efgh", "ij", "kl"]);
    assert!(screen.line_wrapped(0));
    assert!(!screen.line_wrapped(1));
    assert!(!screen.line_wrapped(2));
    assert_eq!(screen.line_contents_between(0, 2, 2, 1), "cdefgh\ni");
}

#[test]
fn history_rows_keep_the_width_they_were_written_at() {
    let mut parser = parser(2, 6, 10, "abcdef\r\nxy\r\nz");
    parser.screen_mut().set_size(2, 10);
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 1);
    assert_eq!(screen.line_cell(0, 5).unwrap().contents(), "f");
    assert!(screen.line_cell(0, 6).is_none());
    assert!(screen.line_cell(1, 9).is_some());
    assert_eq!(screen.line_contents(0, 4, 10), "ef");

    parser.screen_mut().set_size(2, 3);
    let screen = parser.screen();
    assert_eq!(screen.line_cell(0, 5).unwrap().contents(), "f");
    assert_eq!(screen.line_contents_between(0, 1, 1, 1), "bcdef\nx");
}

#[test]
fn the_scrollback_offset_does_not_move_lines() {
    let mut parser = parser(3, 10, 10, &numbered(8));
    let before = lines(parser.screen());
    parser.screen_mut().set_scrollback(3);
    assert_eq!(parser.screen().scrollback(), 3);
    assert_eq!(lines(parser.screen()), before);
    assert_eq!(parser.screen().line_cell(7, 0).unwrap().contents(), "8");
    assert_eq!(parser.screen().line_contents_between(4, 0, 6, 1), "5\n6\n7");
}

#[test]
fn wide_characters_are_read_whole() {
    let parser = parser(2, 6, 10, "a日本b\r\nx\r\ny");
    let screen = parser.screen();
    assert_eq!(screen.history_len(), 1);
    assert!(screen.line_cell(0, 1).unwrap().is_wide());
    assert!(screen.line_cell(0, 2).unwrap().is_wide_continuation());
    assert_eq!(screen.line_contents(0, 0, 6), "a日本b");
    assert_eq!(screen.line_contents(0, 3, 3), "本b");
    assert_eq!(screen.line_contents_between(0, 1, 0, 4), "日本");
    assert_eq!(screen.line_contents_between(0, 3, 1, 1), "本b\nx");
}

#[test]
fn reversed_or_empty_ranges_have_no_text() {
    let parser = parser(3, 10, 10, &numbered(5));
    let screen = parser.screen();
    assert_eq!(screen.line_contents_between(3, 0, 1, 0), "");
    assert_eq!(screen.line_contents_between(2, 1, 2, 1), "");
    assert_eq!(screen.line_contents_between(3, 0, 9, 1), "4\n5\n");
}
