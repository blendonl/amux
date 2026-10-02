fn parser(input: &str) -> vt100::Parser {
    let mut parser = vt100::Parser::new(3, 10, 0);
    parser.process(input.as_bytes());
    parser
}

#[test]
fn narrow_characters_take_the_current_attributes_and_move_the_cursor() {
    let parser = parser("\x1b[31mab\x1b[mc");
    let screen = parser.screen();
    assert_eq!(screen.contents(), "abc");
    assert_eq!(screen.cell(0, 1).unwrap().fgcolor(), vt100::Color::Idx(1));
    assert_eq!(screen.cell(0, 2).unwrap().fgcolor(), vt100::Color::Default);
    assert_eq!(screen.cursor_position(), (0, 3));
}

#[test]
fn a_narrow_character_over_the_first_half_of_a_wide_one_blanks_the_second() {
    let parser = parser("中\x1b[1;1Ha");
    let screen = parser.screen();
    let first = screen.cell(0, 0).unwrap();
    let second = screen.cell(0, 1).unwrap();
    assert_eq!(first.contents(), "a");
    assert!(!first.is_wide());
    assert_eq!(second.contents(), " ");
    assert!(!second.is_wide_continuation());
    assert_eq!(screen.cursor_position(), (0, 1));
}

#[test]
fn a_narrow_character_over_the_second_half_of_a_wide_one_clears_the_first() {
    let parser = parser("中\x1b[1;2Ha");
    let screen = parser.screen();
    let first = screen.cell(0, 0).unwrap();
    let second = screen.cell(0, 1).unwrap();
    assert!(!first.has_contents());
    assert!(!first.is_wide());
    assert_eq!(second.contents(), "a");
    assert!(!second.is_wide_continuation());
    assert_eq!(screen.cursor_position(), (0, 2));
}

#[test]
fn a_narrow_character_past_the_last_column_wraps() {
    let parser = parser("0123456789x");
    let screen = parser.screen();
    assert!(screen.row_wrapped(0));
    assert_eq!(screen.cell(1, 0).unwrap().contents(), "x");
    assert_eq!(screen.cursor_position(), (1, 1));
}
