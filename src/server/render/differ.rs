use super::escape;
use super::grid::{Cell, Grid, Style};
use super::{Frame, InputModes};
use crate::protocol::Size;

pub struct GridDiffer {
    client_size: Size,
    shown: Option<Vec<Cell>>,
    modes: Option<InputModes>,
    cursor: Option<(u16, u16)>,
    cursor_visible: Option<bool>,
}

impl GridDiffer {
    pub fn new(client_size: Size) -> Self {
        Self {
            client_size,
            shown: None,
            modes: None,
            cursor: None,
            cursor_visible: None,
        }
    }

    #[cfg(test)]
    pub fn client_size(&self) -> Size {
        self.client_size
    }

    pub fn set_client_size(&mut self, size: Size) {
        if size != self.client_size {
            self.client_size = size;
            self.shown = None;
            self.cursor = None;
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new(self.client_size);
    }

    pub fn diff(&mut self, frame: &Frame) -> Vec<u8> {
        let mut out = Vec::new();
        escape::set_modes(&mut out, self.modes, frame.modes);
        self.modes = Some(frame.modes);

        let view = self.view(&frame.grid);
        let changed_rows = self.changed_rows(&view);
        let drawn_cursor = if changed_rows.is_empty() {
            None
        } else {
            if self.cursor_visible != Some(false) {
                out.extend_from_slice(escape::HIDE_CURSOR);
                self.cursor_visible = Some(false);
            }
            let mut painter = Painter {
                out: &mut out,
                cursor: None,
                style: self.shown.is_some().then(Style::default),
            };
            let width = usize::from(self.client_size.cols);
            for row in changed_rows {
                let span = usize::from(row) * width..(usize::from(row) + 1) * width;
                let old = self.shown.as_ref().map(|shown| &shown[span.clone()]);
                painter.paint_row(row, &view[span], old);
            }
            painter.finish();
            Some(painter.cursor)
        };

        let target = frame
            .cursor
            .filter(|&(row, col)| row < self.client_size.rows && col < self.client_size.cols);
        if let Some(position) = target {
            match drawn_cursor {
                Some(from) => escape::move_cursor(&mut out, from, position),
                None if self.cursor != Some(position) => {
                    escape::move_cursor(&mut out, None, position);
                }
                None => {}
            }
            self.cursor = Some(position);
        } else if let Some(from) = drawn_cursor {
            self.cursor = from;
        }

        let visible = target.is_some() && !frame.modes.hide_cursor;
        if self.cursor_visible != Some(visible) {
            out.extend_from_slice(if visible {
                escape::SHOW_CURSOR
            } else {
                escape::HIDE_CURSOR
            });
            self.cursor_visible = Some(visible);
        }

        self.shown = Some(view);
        out
    }

    fn view(&self, grid: &Grid) -> Vec<Cell> {
        let Size { rows, cols } = self.client_size;
        let mut cells = Vec::with_capacity(usize::from(rows) * usize::from(cols));
        for row in 0..rows {
            for col in 0..cols {
                cells.push(match grid.cell(row, col) {
                    Some(cell) if cell.is_wide() && col + 1 >= cols => Cell::blank(cell.style()),
                    Some(cell) => *cell,
                    None => Cell::default(),
                });
            }
        }
        cells
    }

    fn changed_rows(&self, view: &[Cell]) -> Vec<u16> {
        let width = usize::from(self.client_size.cols);
        (0..self.client_size.rows)
            .filter(|&row| {
                let span = usize::from(row) * width..(usize::from(row) + 1) * width;
                width > 0
                    && self
                        .shown
                        .as_ref()
                        .is_none_or(|shown| shown[span.clone()] != view[span])
            })
            .collect()
    }
}

struct Painter<'a> {
    out: &'a mut Vec<u8>,
    cursor: Option<(u16, u16)>,
    style: Option<Style>,
}

impl Painter<'_> {
    fn paint_row(&mut self, row: u16, new: &[Cell], old: Option<&[Cell]>) {
        let changed = |col: usize| old.is_none_or(|old| old[col] != new[col]);
        let tail = trailing_blanks_start(new);

        let mut col = 0;
        while col < new.len() {
            let cell = new[col];
            if !changed(col) || cell.is_wide_continuation() {
                col += 1;
                continue;
            }
            self.move_to((row, column(col)));
            self.set_style(cell.style());
            if col >= tail {
                self.out.extend_from_slice(escape::ERASE_LINE);
                return;
            }
            if cell.is_erased() {
                let run = new[col..tail]
                    .iter()
                    .take_while(|other| other.is_erased() && other.style() == cell.style())
                    .count();
                escape::erase_chars(self.out, column(run));
                col += run;
                continue;
            }
            self.out.extend_from_slice(cell.text().as_bytes());
            col += if cell.is_wide() { 2 } else { 1 };
            self.cursor = (col < new.len()).then(|| (row, column(col)));
        }
    }

    fn move_to(&mut self, position: (u16, u16)) {
        escape::move_cursor(self.out, self.cursor, position);
        self.cursor = Some(position);
    }

    fn set_style(&mut self, style: Style) {
        if self.style != Some(style) {
            escape::set_style(self.out, self.style, style);
            self.style = Some(style);
        }
    }

    fn finish(&mut self) {
        if self.style.is_some_and(|style| style != Style::default()) {
            self.set_style(Style::default());
        }
    }
}

fn trailing_blanks_start(cells: &[Cell]) -> usize {
    let Some(last) = cells.last().filter(|cell| cell.is_erased()) else {
        return cells.len();
    };
    let blanks = cells
        .iter()
        .rev()
        .take_while(|cell| cell.is_erased() && cell.style() == last.style())
        .count();
    cells.len() - blanks
}

fn column(index: usize) -> u16 {
    u16::try_from(index).unwrap_or(u16::MAX)
}
