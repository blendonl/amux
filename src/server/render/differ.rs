use super::escape;
use super::grid::{Cell, Grid, Style};
use super::{Frame, InputModes};
use crate::protocol::Size;

pub struct GridDiffer {
    client_size: Size,
    shown: Option<Vec<Cell>>,
    row: Vec<Cell>,
    seen: Option<(u64, u64)>,
    modes: Option<InputModes>,
    cursor: Option<(u16, u16)>,
    cursor_visible: Option<bool>,
}

impl GridDiffer {
    pub fn new(client_size: Size) -> Self {
        Self {
            client_size,
            shown: None,
            row: Vec::new(),
            seen: None,
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

        let drawn_cursor = self.paint_changed_rows(frame, &mut out);

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
        out
    }

    fn paint_changed_rows(
        &mut self,
        frame: &Frame,
        out: &mut Vec<u8>,
    ) -> Option<Option<(u16, u16)>> {
        let Size { rows, cols } = self.client_size;
        let width = usize::from(cols);
        if width == 0 {
            return None;
        }
        let grid = &frame.grid;
        let fresh = self.shown.is_none();
        let unchanged_since = self
            .seen
            .filter(|&(source, _)| !fresh && source == frame.source)
            .map(|(_, generation)| generation);
        self.seen = Some((frame.source, grid.generation()));
        let shown = self
            .shown
            .get_or_insert_with(|| vec![Cell::default(); usize::from(rows) * width]);
        let mut painter = Painter {
            out,
            cursor: None,
            style: (!fresh).then(Style::default),
            drawing: false,
        };
        for (row, old) in (0..rows).zip(shown.chunks_exact_mut(width)) {
            if unchanged_since.is_some_and(|generation| !grid.changed_since(row, generation)) {
                if cfg!(debug_assertions) {
                    view_row(grid, row, cols, &mut self.row);
                    assert!(*old == *self.row, "row {row} changed without being marked");
                }
                continue;
            }
            view_row(grid, row, cols, &mut self.row);
            if !fresh && *old == *self.row {
                continue;
            }
            if !painter.drawing {
                painter.begin(&mut self.cursor_visible);
            }
            painter.paint_row(row, &self.row, (!fresh).then_some(&*old));
            old.copy_from_slice(&self.row);
        }
        painter.drawing.then(|| {
            painter.finish();
            painter.cursor
        })
    }
}

fn view_row(grid: &Grid, row: u16, cols: u16, view: &mut Vec<Cell>) {
    let cells = grid.row(row).unwrap_or_default();
    view.clear();
    view.extend_from_slice(&cells[..cells.len().min(usize::from(cols))]);
    view.resize(usize::from(cols), Cell::default());
    if let Some(last) = view.last_mut().filter(|cell| cell.is_wide()) {
        *last = Cell::blank(last.style());
    }
}

struct Painter<'a> {
    out: &'a mut Vec<u8>,
    cursor: Option<(u16, u16)>,
    style: Option<Style>,
    drawing: bool,
}

impl Painter<'_> {
    fn begin(&mut self, cursor_visible: &mut Option<bool>) {
        if *cursor_visible != Some(false) {
            self.out.extend_from_slice(escape::HIDE_CURSOR);
            *cursor_visible = Some(false);
        }
        self.drawing = true;
    }

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::server::graphics::place::tests::allocations;
    use crate::server::layout::{Layout, PaneId};
    use crate::server::render::{compose, Viewer};
    use crate::settings::Settings;

    const PANE: PaneId = PaneId(0);
    const SIZE: Size = Size { rows: 10, cols: 40 };

    fn frame_of(layout: &Layout, panes: &BTreeMap<PaneId, vt100::Parser>) -> Frame {
        compose(
            layout,
            SIZE,
            PANE,
            panes,
            &Settings::default(),
            Viewer::text(),
        )
    }

    #[test]
    fn diffing_a_frame_allocates_only_the_output() {
        let layout = Layout::new(PANE);
        let mut panes = BTreeMap::from([(PANE, vt100::Parser::new(SIZE.rows, SIZE.cols, 0))]);
        let mut differ = GridDiffer::new(SIZE);
        panes.get_mut(&PANE).unwrap().process(b"$ ");
        differ.diff(&frame_of(&layout, &panes));
        panes.get_mut(&PANE).unwrap().process(b"ls");
        let typed = frame_of(&layout, &panes);

        let before = allocations();
        let output = differ.diff(&typed);
        let spent = allocations() - before;
        assert!(!output.is_empty());
        let growth = output.capacity().ilog2() - 2;
        assert!(
            spent <= usize::try_from(growth).unwrap(),
            "{spent} allocations for {} bytes",
            output.len()
        );

        let before = allocations();
        assert!(differ.diff(&typed).is_empty());
        assert_eq!(allocations() - before, 0);
    }
}
