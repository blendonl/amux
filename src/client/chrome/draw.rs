use std::io::Write;
use std::iter;

use unicode_width::UnicodeWidthChar;

use crate::settings::{Color, StyleSpec};

pub const SAVE_CURSOR: &[u8] = b"\x1b7";
pub const RESTORE_CURSOR: &[u8] = b"\x1b8";
pub const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
pub const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
pub const RESET_STYLE: &[u8] = b"\x1b[0m";
pub const MOVE_TO_LAST_ROW: &[u8] = b"\x1b[9999;1H";

const ELLIPSIS: char = '…';
const REPLACEMENT: char = '?';
const FOREGROUND: u8 = 30;
const BACKGROUND: u8 = 40;
const BRIGHT_OFFSET: u8 = 60;
const EXTENDED_OFFSET: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    fg: Color,
    bg: Color,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    reverse: bool,
}

impl Style {
    pub const PLAIN: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        reverse: false,
    };

    fn write(self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (enabled, param) in [
            (self.bold, "1"),
            (self.dim, "2"),
            (self.italic, "3"),
            (self.underline, "4"),
            (self.reverse, "7"),
        ] {
            if enabled {
                let _ = write!(out, ";{param}");
            }
        }
        write_color(out, self.fg, FOREGROUND);
        write_color(out, self.bg, BACKGROUND);
        out.push(b'm');
    }
}

impl From<StyleSpec> for Style {
    fn from(spec: StyleSpec) -> Self {
        Self {
            fg: spec.fg.unwrap_or_default(),
            bg: spec.bg.unwrap_or_default(),
            bold: spec.bold.unwrap_or_default(),
            dim: spec.dim.unwrap_or_default(),
            italic: spec.italic.unwrap_or_default(),
            underline: spec.underline.unwrap_or_default(),
            reverse: spec.reverse.unwrap_or_default(),
        }
    }
}

fn write_color(out: &mut Vec<u8>, color: Color, base: u8) {
    let _ = match color {
        Color::Default => Ok(()),
        Color::Indexed(index @ 0..=7) => write!(out, ";{}", base + index),
        Color::Indexed(index @ 8..=15) => write!(out, ";{}", base + BRIGHT_OFFSET + index - 8),
        Color::Indexed(index) => write!(out, ";{};5;{index}", base + EXTENDED_OFFSET),
        Color::Rgb(red, green, blue) => {
            write!(out, ";{};2;{red};{green};{blue}", base + EXTENDED_OFFSET)
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
}

pub fn char_width(character: char) -> usize {
    printable(character).width().unwrap_or(0)
}

pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

pub fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|span| width(&span.text)).sum()
}

pub fn truncate(text: &str, columns: usize) -> String {
    fit(&[Span::new(text, Style::PLAIN)], columns)
        .into_iter()
        .map(|(character, _)| character)
        .collect()
}

pub fn move_to(out: &mut Vec<u8>, row: usize, col: usize) {
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
}

pub fn draw_row(
    out: &mut Vec<u8>,
    row: usize,
    col: usize,
    columns: usize,
    spans: &[Span],
    fill: Style,
) {
    move_to(out, row, col);
    write_spans(out, spans, columns, fill);
}

pub fn write_spans(out: &mut Vec<u8>, spans: &[Span], columns: usize, fill: Style) {
    let mut current = None;
    let mut used = 0;
    let mut encoded = [0; 4];
    for (character, style) in fit(spans, columns) {
        if current != Some(style) {
            style.write(out);
            current = Some(style);
        }
        out.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        used += char_width(character);
    }
    if used < columns {
        if current != Some(fill) {
            fill.write(out);
        }
        out.extend(iter::repeat_n(b' ', columns - used));
    }
    out.extend_from_slice(RESET_STYLE);
}

fn fit(spans: &[Span], columns: usize) -> Vec<(char, Style)> {
    let cells: Vec<(char, Style)> = spans
        .iter()
        .flat_map(|span| {
            span.text
                .chars()
                .map(move |character| (printable(character), span.style))
        })
        .collect();
    let total: usize = cells
        .iter()
        .map(|&(character, _)| char_width(character))
        .sum();
    if total <= columns {
        return cells;
    }

    let budget = columns.saturating_sub(1);
    let mut fitted = Vec::new();
    let mut used = 0;
    for (character, style) in cells {
        let width = char_width(character);
        if used + width > budget {
            if columns > 0 {
                fitted.push((ELLIPSIS, style));
            }
            break;
        }
        fitted.push((character, style));
        used += width;
    }
    fitted
}

fn printable(character: char) -> char {
    if character.is_control() {
        REPLACEMENT
    } else {
        character
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::chrome::testing::{row_text, terminal};

    fn fg(color: Color) -> Style {
        StyleSpec {
            fg: Some(color),
            ..StyleSpec::EMPTY
        }
        .into()
    }

    fn bg(color: Color) -> Style {
        StyleSpec {
            bg: Some(color),
            ..StyleSpec::EMPTY
        }
        .into()
    }

    fn sgr(spec: StyleSpec) -> String {
        let mut out = Vec::new();
        Style::from(spec).write(&mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn widths_count_terminal_columns() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("a\x1bb"), 3);
    }

    #[test]
    fn truncation_marks_the_cut_with_an_ellipsis() {
        assert_eq!(truncate("desktop", 7), "desktop");
        assert_eq!(truncate("desktop", 5), "desk…");
        assert_eq!(truncate("desktop", 1), "…");
        assert_eq!(truncate("desktop", 0), "");
        assert_eq!(truncate("日本語", 4), "日…");
        assert_eq!(truncate("日本語", 3), "日…");
        assert_eq!(truncate("a\x07b", 3), "a?b");
    }

    #[test]
    fn a_row_fills_exactly_its_columns_and_resets_the_style() {
        let mut out = Vec::new();
        let spans = [
            Span::new("ab", StyleSpec::BOLD.into()),
            Span::new("cd", fg(Color::RED)),
        ];
        draw_row(&mut out, 1, 2, 6, &spans, bg(Color::YELLOW));
        out.extend_from_slice(b"x");

        let mut parser = terminal(3, 10);
        parser.process(&out);
        let screen = parser.screen();
        assert_eq!(row_text(&parser, 1), "  abcd  x");
        assert!(screen.cell(1, 2).unwrap().bold());
        assert_eq!(screen.cell(1, 4).unwrap().fgcolor(), vt100::Color::Idx(1));
        assert_eq!(screen.cell(1, 7).unwrap().bgcolor(), vt100::Color::Idx(3));
        let after = screen.cell(1, 8).unwrap();
        assert!(!after.bold());
        assert_eq!(after.bgcolor(), vt100::Color::Default);
    }

    #[test]
    fn a_row_too_long_for_its_columns_is_cut_without_wrapping() {
        let mut out = Vec::new();
        draw_row(
            &mut out,
            2,
            0,
            5,
            &[Span::new("日本語です", Style::PLAIN)],
            Style::PLAIN,
        );

        let mut parser = terminal(3, 5);
        parser.process(b"top");
        parser.process(&out);
        assert_eq!(row_text(&parser, 0), "top");
        assert_eq!(row_text(&parser, 2), "日本…");
    }

    #[test]
    fn the_first_eight_colors_keep_their_classic_codes() {
        assert_eq!(sgr(StyleSpec::EMPTY), "\x1b[0m");
        assert_eq!(
            sgr(StyleSpec::colors(Color::BLACK, Color::GREEN).merge(StyleSpec::BOLD)),
            "\x1b[0;1;30;42m"
        );
        assert_eq!(
            sgr(StyleSpec::colors(Color::WHITE, Color::RED)),
            "\x1b[0;37;41m"
        );
        assert_eq!(
            sgr(StyleSpec::colors(Color::Default, Color::Default)),
            "\x1b[0m"
        );
    }

    #[test]
    fn bright_indexed_and_rgb_colors_use_their_own_codes() {
        for (fg, bg, expected) in [
            (Color::Indexed(8), Color::Indexed(15), "\x1b[0;90;107m"),
            (Color::Indexed(12), Color::Indexed(9), "\x1b[0;94;101m"),
            (
                Color::Indexed(16),
                Color::Indexed(255),
                "\x1b[0;38;5;16;48;5;255m",
            ),
            (
                Color::Rgb(142, 192, 124),
                Color::Rgb(0, 0, 255),
                "\x1b[0;38;2;142;192;124;48;2;0;0;255m",
            ),
        ] {
            assert_eq!(sgr(StyleSpec::colors(fg, bg)), expected, "{fg:?} {bg:?}");
        }
    }

    #[test]
    fn attributes_are_written_in_parameter_order() {
        let every = StyleSpec {
            bold: Some(true),
            dim: Some(true),
            italic: Some(true),
            underline: Some(true),
            reverse: Some(true),
            ..StyleSpec::EMPTY
        };
        assert_eq!(sgr(every), "\x1b[0;1;2;3;4;7m");
        let italic_underline = StyleSpec {
            italic: Some(true),
            underline: Some(true),
            bold: Some(false),
            ..StyleSpec::EMPTY
        };
        assert_eq!(sgr(italic_underline), "\x1b[0;3;4m");
    }

    #[test]
    fn a_terminal_reads_back_every_kind_of_color_and_attribute() {
        let spans = [
            Span::new("a", fg(Color::Indexed(9))),
            Span::new("b", bg(Color::Indexed(200))),
            Span::new("c", fg(Color::Rgb(1, 2, 3))),
            Span::new(
                "d",
                StyleSpec {
                    italic: Some(true),
                    underline: Some(true),
                    ..StyleSpec::EMPTY
                }
                .into(),
            ),
        ];
        let mut out = Vec::new();
        draw_row(&mut out, 0, 0, 4, &spans, Style::PLAIN);

        let mut parser = terminal(1, 4);
        parser.process(&out);
        let cell = |col| parser.screen().cell(0, col).unwrap().clone();
        assert_eq!(cell(0).fgcolor(), vt100::Color::Idx(9));
        assert_eq!(cell(1).bgcolor(), vt100::Color::Idx(200));
        assert_eq!(cell(2).fgcolor(), vt100::Color::Rgb(1, 2, 3));
        assert!(cell(3).italic() && cell(3).underline());
        assert!(!cell(2).italic() && !cell(2).underline());
    }
}
