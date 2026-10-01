use vt100::Color;

fn printed(input: &str) -> vt100::Cell {
    let mut parser = vt100::Parser::default();
    parser.process(format!("{input}x").as_bytes());
    parser.screen().cell(0, 0).unwrap().clone()
}

#[test]
fn every_form_of_sgr_58_sets_the_underline_color() {
    for (input, color) in [
        ("\x1b[58;5;123m", Color::Idx(123)),
        ("\x1b[58;2;1;4;7m", Color::Rgb(1, 4, 7)),
        ("\x1b[58:5:123m", Color::Idx(123)),
        ("\x1b[58:2::1:4:7m", Color::Rgb(1, 4, 7)),
        ("\x1b[58:2:1:4:7m", Color::Rgb(1, 4, 7)),
        ("\x1b[58:2:0:1:4:7m", Color::Rgb(1, 4, 7)),
    ] {
        assert_eq!(printed(input).underline_color(), color, "{input:?}");
    }
}

#[test]
fn the_underline_color_components_are_not_read_as_attributes() {
    let cell = printed("\x1b[58;2;1;4;7m");
    assert!(!cell.bold());
    assert!(!cell.dim());
    assert!(!cell.underline());
    assert!(!cell.inverse());
    assert_eq!(cell.fgcolor(), Color::Default);
    assert_eq!(cell.bgcolor(), Color::Default);
}

#[test]
fn attributes_after_the_underline_color_still_apply() {
    let cell = printed("\x1b[58;5;9;1;4m");
    assert_eq!(cell.underline_color(), Color::Idx(9));
    assert!(cell.bold());
    assert!(cell.underline());

    let cell = printed("\x1b[58:2::1:2:3;3m");
    assert_eq!(cell.underline_color(), Color::Rgb(1, 2, 3));
    assert!(cell.italic());
}

#[test]
fn sgr_59_and_sgr_0_reset_the_underline_color() {
    for input in [
        "\x1b[58;5;1m\x1b[59m",
        "\x1b[58;5;1m\x1b[0m",
        "\x1b[58;5;1m\x1b[m",
    ] {
        assert_eq!(printed(input).underline_color(), Color::Default);
    }
}

#[test]
fn the_underline_color_leaves_the_other_colors_alone() {
    let cell = printed("\x1b[31;42m\x1b[58;5;3m");
    assert_eq!(cell.fgcolor(), Color::Idx(1));
    assert_eq!(cell.bgcolor(), Color::Idx(2));
    assert_eq!(cell.underline_color(), Color::Idx(3));
}

#[test]
fn colon_forms_with_a_colour_space_set_foreground_and_background() {
    let cell = printed("\x1b[38:2::10:20:30m\x1b[48:2::40:50:60m");
    assert_eq!(cell.fgcolor(), Color::Rgb(10, 20, 30));
    assert_eq!(cell.bgcolor(), Color::Rgb(40, 50, 60));
    assert_eq!(cell.underline_color(), Color::Default);
    assert!(!cell.bold());
    assert!(!cell.dim());
}

#[test]
fn formatted_contents_reproduce_the_underline_color() {
    let mut parser = vt100::Parser::default();
    parser.process(b"\x1b[4;58;2;1;2;3ma\x1b[58;5;7mb\x1b[59mc");
    let mut copy = vt100::Parser::default();
    copy.process(&parser.screen().contents_formatted());
    for col in 0..3 {
        assert_eq!(
            copy.screen().cell(0, col).unwrap().underline_color(),
            parser.screen().cell(0, col).unwrap().underline_color(),
        );
    }
    assert_eq!(
        copy.screen().cell(0, 0).unwrap().underline_color(),
        Color::Rgb(1, 2, 3)
    );
}
