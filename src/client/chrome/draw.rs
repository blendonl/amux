use std::io::Write;
use std::iter;

use unicode_width::UnicodeWidthChar;

pub const SAVE_CURSOR: &[u8] = b"\x1b7";
pub const RESTORE_CURSOR: &[u8] = b"\x1b8";
pub const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
pub const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
pub const RESET_STYLE: &[u8] = b"\x1b[0m";
pub const MOVE_TO_LAST_ROW: &[u8] = b"\x1b[9999;1H";

const ELLIPSIS: char = '…';
const REPLACEMENT: char = '?';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Black = 0,
    Red = 1,
    Green = 2,
    Yellow = 3,
    White = 7,
}

impl Color {
    fn code(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    dim: bool,
    reverse: bool,
}

impl Style {
    pub const PLAIN: Self = Self {
        fg: None,
        bg: None,
        bold: false,
        dim: false,
        reverse: false,
    };

    pub const fn fg(self, color: Color) -> Self {
        Self {
            fg: Some(color),
            ..self
        }
    }

    pub const fn bg(self, color: Color) -> Self {
        Self {
            bg: Some(color),
            ..self
        }
    }

    pub const fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    pub const fn dim(self) -> Self {
        Self { dim: true, ..self }
    }

    pub const fn reverse(self) -> Self {
        Self {
            reverse: true,
            ..self
        }
    }

    fn write(self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        for (enabled, param) in [(self.bold, "1"), (self.dim, "2"), (self.reverse, "7")] {
            if enabled {
                let _ = write!(out, ";{param}");
            }
        }
        if let Some(fg) = self.fg {
            let _ = write!(out, ";3{}", fg.code());
        }
        if let Some(bg) = self.bg {
            let _ = write!(out, ";4{}", bg.code());
        }
        out.push(b'm');
    }
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
            Span::new("ab", Style::PLAIN.bold()),
            Span::new("cd", Style::PLAIN.fg(Color::Red)),
        ];
        draw_row(&mut out, 1, 2, 6, &spans, Style::PLAIN.bg(Color::Yellow));
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
}
