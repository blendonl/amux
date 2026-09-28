use std::fmt;

use vt100::Color;

use crate::protocol::Size;
use crate::server::layout::{Border, BorderLine, Rect};
use crate::settings::{self, BorderSettings, Settings, StyleSpec};

const TEXT_CAPACITY: usize = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

impl From<StyleSpec> for Style {
    fn from(spec: StyleSpec) -> Self {
        Self {
            fg: spec.fg.map_or(Color::Default, vt100_color),
            bg: spec.bg.map_or(Color::Default, vt100_color),
            bold: spec.bold.unwrap_or_default(),
            dim: spec.dim.unwrap_or_default(),
            italic: spec.italic.unwrap_or_default(),
            underline: spec.underline.unwrap_or_default(),
            inverse: spec.reverse.unwrap_or_default(),
        }
    }
}

fn vt100_color(color: settings::Color) -> Color {
    match color {
        settings::Color::Default => Color::Default,
        settings::Color::Indexed(index) => Color::Idx(index),
        settings::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BorderLook {
    glyphs: BorderSettings,
    style: Style,
    active_style: Style,
}

impl BorderLook {
    pub fn new(settings: &Settings) -> Self {
        Self {
            glyphs: settings.borders,
            style: settings.theme.pane_border.into(),
            active_style: settings.theme.pane_border_active.into(),
        }
    }
}

impl Style {
    fn from_vt100(cell: &vt100::Cell) -> Self {
        Self {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Cell {
    text: [u8; TEXT_CAPACITY],
    len: u8,
    style: Style,
    wide: bool,
    wide_continuation: bool,
}

impl Cell {
    pub fn blank(style: Style) -> Self {
        Self {
            style,
            ..Self::default()
        }
    }

    pub fn glyph(glyph: char, style: Style) -> Self {
        Self::with_text(glyph.encode_utf8(&mut [0; 4]), style)
    }

    pub fn from_vt100(cell: &vt100::Cell) -> Self {
        Self {
            wide: cell.is_wide(),
            wide_continuation: cell.is_wide_continuation(),
            ..Self::with_text(cell.contents(), Style::from_vt100(cell))
        }
    }

    fn with_text(text: &str, style: Style) -> Self {
        let mut end = text.len().min(TEXT_CAPACITY);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0; TEXT_CAPACITY];
        bytes[..end].copy_from_slice(&text.as_bytes()[..end]);
        Self {
            text: bytes,
            len: u8::try_from(end).unwrap_or_default(),
            style,
            wide: false,
            wide_continuation: false,
        }
    }

    pub fn text(&self) -> &str {
        std::str::from_utf8(&self.text[..usize::from(self.len)]).unwrap_or_default()
    }

    pub fn style(&self) -> Style {
        self.style
    }

    pub fn is_wide(&self) -> bool {
        self.wide
    }

    pub fn is_wide_continuation(&self) -> bool {
        self.wide_continuation
    }

    pub fn is_erased(&self) -> bool {
        self.len == 0 && !self.wide_continuation
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cell")
            .field("text", &self.text())
            .field("style", &self.style)
            .field("wide", &self.wide)
            .field("wide_continuation", &self.wide_continuation)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    size: Size,
    cells: Vec<Cell>,
}

impl Grid {
    pub fn new(size: Size) -> Self {
        Self {
            size,
            cells: vec![Cell::default(); usize::from(size.rows) * usize::from(size.cols)],
        }
    }

    #[cfg(test)]
    pub fn size(&self) -> Size {
        self.size
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&Cell> {
        self.index(row, col).map(|index| &self.cells[index])
    }

    pub fn paint(&mut self, screen: &vt100::Screen, rect: Rect) {
        let area = self.clip(rect);
        for row in 0..area.rows {
            for col in 0..area.cols {
                let cell = screen.cell(row, col).map(Cell::from_vt100);
                self.set(area.row + row, area.col + col, cell.unwrap_or_default());
            }
            self.repair_wide_cells(area.row + row, area.col, area.right());
        }
    }

    pub fn draw_borders(&mut self, borders: &[Border], active: Option<Rect>, look: &BorderLook) {
        let mut lines = vec![None; self.cells.len()];
        for border in borders {
            let area = self.clip(border.rect);
            for row in area.row..area.bottom() {
                for col in area.col..area.right() {
                    if let Some(index) = self.index(row, col) {
                        lines[index] = Some(border.line);
                    }
                }
            }
        }

        let is_border = |row: Option<u16>, col: Option<u16>| match (row, col) {
            (Some(row), Some(col)) => self
                .index(row, col)
                .is_some_and(|index| lines[index].is_some()),
            _ => false,
        };
        let mut glyphs = Vec::new();
        for row in 0..self.size.rows {
            for col in 0..self.size.cols {
                let Some(line) = self.index(row, col).and_then(|index| lines[index]) else {
                    continue;
                };
                let joins = Joins {
                    up: is_border(row.checked_sub(1), Some(col)),
                    down: is_border(row.checked_add(1), Some(col)),
                    left: is_border(Some(row), col.checked_sub(1)),
                    right: is_border(Some(row), col.checked_add(1)),
                };
                let style = if active.is_some_and(|rect| touches(rect, row, col)) {
                    look.active_style
                } else {
                    look.style
                };
                glyphs.push((
                    row,
                    col,
                    Cell::glyph(joins.glyph(line, &look.glyphs), style),
                ));
            }
        }
        for (row, col, cell) in glyphs {
            self.set(row, col, cell);
        }
    }

    fn repair_wide_cells(&mut self, row: u16, start: u16, end: u16) {
        for col in start..end {
            let Some(cell) = self.cell(row, col).copied() else {
                continue;
            };
            let orphaned = if cell.is_wide() {
                col + 1 >= end
                    || !self
                        .cell(row, col + 1)
                        .is_some_and(Cell::is_wide_continuation)
            } else if cell.is_wide_continuation() {
                col == start || !self.cell(row, col - 1).is_some_and(Cell::is_wide)
            } else {
                false
            };
            if orphaned {
                self.set(row, col, Cell::blank(cell.style()));
            }
        }
    }

    fn clip(&self, rect: Rect) -> Rect {
        let row = rect.row.min(self.size.rows);
        let col = rect.col.min(self.size.cols);
        Rect {
            row,
            col,
            rows: rect.rows.min(self.size.rows - row),
            cols: rect.cols.min(self.size.cols - col),
        }
    }

    fn index(&self, row: u16, col: u16) -> Option<usize> {
        (row < self.size.rows && col < self.size.cols)
            .then(|| usize::from(row) * usize::from(self.size.cols) + usize::from(col))
    }

    fn set(&mut self, row: u16, col: u16, cell: Cell) {
        if let Some(index) = self.index(row, col) {
            self.cells[index] = cell;
        }
    }
}

struct Joins {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
}

impl Joins {
    fn glyph(&self, line: BorderLine, glyphs: &BorderSettings) -> char {
        match (self.up, self.down, self.left, self.right) {
            (true, true, true, true) => glyphs.cross,
            (true, true, true, false) => glyphs.right_tee,
            (true, true, false, true) => glyphs.left_tee,
            (true, false, true, true) => glyphs.bottom_tee,
            (false, true, true, true) => glyphs.top_tee,
            (false, true, false, true) => glyphs.top_left,
            (false, true, true, false) => glyphs.top_right,
            (true, false, false, true) => glyphs.bottom_left,
            (true, false, true, false) => glyphs.bottom_right,
            (true, true, false, false) => glyphs.vertical,
            (false, false, true, true) => glyphs.horizontal,
            _ => match line {
                BorderLine::Vertical => glyphs.vertical,
                BorderLine::Horizontal => glyphs.horizontal,
            },
        }
    }
}

fn touches(rect: Rect, row: u16, col: u16) -> bool {
    (rect.row.saturating_sub(1)..=rect.bottom()).contains(&row)
        && (rect.col.saturating_sub(1)..=rect.right()).contains(&col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::layout::{Layout, PaneId, SplitDirection};

    fn size(cols: u16, rows: u16) -> Size {
        Size { rows, cols }
    }

    fn rect(row: u16, col: u16, rows: u16, cols: u16) -> Rect {
        Rect {
            row,
            col,
            rows,
            cols,
        }
    }

    fn default_look() -> BorderLook {
        BorderLook::new(&Settings::default())
    }

    fn screen(cols: u16, rows: u16, input: &str) -> vt100::Parser {
        let mut parser = vt100::Parser::new(rows, cols, 0);
        parser.process(input.as_bytes());
        parser
    }

    fn row_text(grid: &Grid, row: u16) -> String {
        (0..grid.size().cols)
            .map(|col| {
                let cell = grid.cell(row, col).unwrap();
                match cell.text() {
                    "" if cell.is_wide_continuation() => "",
                    "" => ".",
                    text => text,
                }
            })
            .collect()
    }

    #[test]
    fn painting_copies_text_colours_and_attributes_into_the_rect() {
        let pane = screen(
            6,
            2,
            "\x1b[1;31mab\x1b[0;7;44mc\x1b[0m\r\n\x1b[3;4;2mi\x1b[0m",
        );
        let mut grid = Grid::new(size(10, 4));
        grid.paint(pane.screen(), rect(1, 2, 2, 6));

        assert_eq!(row_text(&grid, 0), "..........");
        assert_eq!(row_text(&grid, 1), "..abc.....");
        assert_eq!(row_text(&grid, 2), "..i.......");

        let bold_red = grid.cell(1, 2).unwrap().style();
        assert!(bold_red.bold && !bold_red.dim);
        assert_eq!(bold_red.fg, Color::Idx(1));
        let inverse = grid.cell(1, 4).unwrap().style();
        assert!(inverse.inverse && !inverse.bold);
        assert_eq!(inverse.bg, Color::Idx(4));
        let styled = grid.cell(2, 2).unwrap().style();
        assert!(styled.italic && styled.underline && styled.dim);
    }

    #[test]
    fn painting_keeps_wide_characters_and_combining_marks() {
        let pane = screen(6, 1, "中e\u{301}x");
        let mut grid = Grid::new(size(6, 1));
        grid.paint(pane.screen(), rect(0, 0, 1, 6));

        let wide = grid.cell(0, 0).unwrap();
        assert_eq!(wide.text(), "中");
        assert!(wide.is_wide());
        assert!(grid.cell(0, 1).unwrap().is_wide_continuation());
        assert_eq!(grid.cell(0, 2).unwrap().text(), "e\u{301}");
        assert_eq!(grid.cell(0, 3).unwrap().text(), "x");
    }

    #[test]
    fn painting_clips_a_larger_screen_and_blanks_a_smaller_one() {
        let mut grid = Grid::new(size(4, 3));
        grid.paint(screen(8, 1, "abcdefgh").screen(), rect(0, 0, 3, 4));

        assert_eq!(row_text(&grid, 0), "abcd");
        assert_eq!(row_text(&grid, 1), "....");
        assert_eq!(row_text(&grid, 2), "....");
    }

    #[test]
    fn a_wide_character_cut_by_the_rect_becomes_a_blank() {
        let pane = screen(4, 1, "a\x1b[42m中\x1b[0m");
        let mut grid = Grid::new(size(2, 1));
        grid.paint(pane.screen(), rect(0, 0, 1, 2));

        assert_eq!(row_text(&grid, 0), "a.");
        let cut = grid.cell(0, 1).unwrap();
        assert!(cut.is_erased() && !cut.is_wide());
        assert_eq!(cut.style().bg, Color::Idx(2));
    }

    #[test]
    fn borders_join_with_box_drawing_characters() {
        let window = size(7, 5);
        let mut layout = Layout::new(PaneId(0));
        layout
            .split(PaneId(0), PaneId(1), SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(PaneId(1), PaneId(2), SplitDirection::TopBottom, window)
            .unwrap();
        layout
            .split(PaneId(0), PaneId(3), SplitDirection::TopBottom, window)
            .unwrap();

        let mut grid = Grid::new(window);
        grid.draw_borders(&layout.borders(window), None, &default_look());
        assert_eq!(row_text(&grid, 0), "...│...");
        assert_eq!(row_text(&grid, 1), "...│...");
        assert_eq!(row_text(&grid, 2), "───┼───");
        assert_eq!(row_text(&grid, 3), "...│...");

        let mut layout = Layout::new(PaneId(0));
        layout
            .split(PaneId(0), PaneId(1), SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(PaneId(1), PaneId(2), SplitDirection::TopBottom, window)
            .unwrap();
        let mut grid = Grid::new(window);
        grid.draw_borders(&layout.borders(window), None, &default_look());
        assert_eq!(row_text(&grid, 1), "...│...");
        assert_eq!(row_text(&grid, 2), "...├───");
    }

    #[test]
    fn a_single_cell_border_keeps_its_orientation() {
        let window = size(3, 1);
        let mut layout = Layout::new(PaneId(0));
        layout
            .split(PaneId(0), PaneId(1), SplitDirection::LeftRight, window)
            .unwrap();
        let mut grid = Grid::new(window);
        grid.draw_borders(&layout.borders(window), None, &default_look());
        assert_eq!(row_text(&grid, 0), ".│.");
    }

    #[test]
    fn borders_next_to_the_active_pane_are_highlighted() {
        let window = size(7, 5);
        let mut layout = Layout::new(PaneId(0));
        layout
            .split(PaneId(0), PaneId(1), SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(PaneId(1), PaneId(2), SplitDirection::TopBottom, window)
            .unwrap();
        let rects = layout.rects(window);
        let highlighted = |active: Rect| {
            let mut grid = Grid::new(window);
            grid.draw_borders(&layout.borders(window), Some(active), &default_look());
            let mut cells = Vec::new();
            for row in 0..window.rows {
                for col in 0..window.cols {
                    if grid.cell(row, col).unwrap().style() == default_look().active_style {
                        cells.push((row, col));
                    }
                }
            }
            cells
        };

        assert_eq!(
            highlighted(rects[0].1),
            vec![(0, 3), (1, 3), (2, 3), (3, 3), (4, 3)]
        );
        assert_eq!(
            highlighted(rects[1].1),
            vec![(0, 3), (1, 3), (2, 3), (2, 4), (2, 5), (2, 6)]
        );
        assert_eq!(
            highlighted(rects[2].1),
            vec![(2, 3), (2, 4), (2, 5), (2, 6), (3, 3), (4, 3)]
        );
    }

    #[test]
    fn the_default_look_is_plain_with_a_green_active_border() {
        let look = default_look();
        assert_eq!(look.style, Style::default());
        assert_eq!(
            look.active_style,
            Style {
                fg: Color::Idx(2),
                ..Style::default()
            }
        );
        assert_eq!(look.glyphs, BorderSettings::default());
    }

    #[test]
    fn a_style_spec_sets_only_what_it_names() {
        assert_eq!(Style::from(StyleSpec::EMPTY), Style::default());
        let spec = StyleSpec {
            fg: Some(settings::Color::Rgb(1, 2, 3)),
            bg: Some(settings::Color::Indexed(236)),
            bold: Some(true),
            dim: Some(false),
            italic: Some(true),
            underline: Some(true),
            reverse: Some(true),
        };
        assert_eq!(
            Style::from(spec),
            Style {
                fg: Color::Rgb(1, 2, 3),
                bg: Color::Idx(236),
                bold: true,
                dim: false,
                italic: true,
                underline: true,
                inverse: true,
            }
        );
        assert_eq!(
            Style::from(StyleSpec::colors(
                settings::Color::Default,
                settings::Color::GREEN
            ))
            .bg,
            Color::Idx(2)
        );
    }

    #[test]
    fn borders_use_the_configured_glyphs() {
        let look = BorderLook::new(&Settings {
            borders: BorderSettings {
                horizontal: 'h',
                vertical: 'v',
                left_tee: 'l',
                cross: 'x',
                ..BorderSettings::default()
            },
            ..Settings::default()
        });
        let window = size(7, 5);
        let mut layout = Layout::new(PaneId(0));
        layout
            .split(PaneId(0), PaneId(1), SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(PaneId(1), PaneId(2), SplitDirection::TopBottom, window)
            .unwrap();
        let mut grid = Grid::new(window);
        grid.draw_borders(&layout.borders(window), None, &look);
        assert_eq!(row_text(&grid, 0), "...v...");
        assert_eq!(row_text(&grid, 2), "...lhhh");

        layout
            .split(PaneId(0), PaneId(3), SplitDirection::TopBottom, window)
            .unwrap();
        let mut grid = Grid::new(window);
        grid.draw_borders(&layout.borders(window), None, &look);
        assert_eq!(row_text(&grid, 2), "hhhxhhh");
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary() {
        let cell = Cell::with_text(&"é".repeat(12), Style::default());
        assert_eq!(cell.text(), "é".repeat(11));
    }
}
