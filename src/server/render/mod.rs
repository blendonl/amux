mod differ;
mod escape;
mod grid;
#[cfg(test)]
mod round_trip;

pub use differ::GridDiffer;
use grid::Grid;

use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use crate::protocol::Size;
use crate::server::layout::{Layout, PaneId, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_protocol_mode: MouseProtocolMode,
    pub mouse_protocol_encoding: MouseProtocolEncoding,
    pub hide_cursor: bool,
}

impl InputModes {
    pub fn from_screen(screen: &vt100::Screen) -> Self {
        Self {
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            mouse_protocol_mode: screen.mouse_protocol_mode(),
            mouse_protocol_encoding: screen.mouse_protocol_encoding(),
            hide_cursor: screen.hide_cursor(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub grid: Grid,
    pub cursor: Option<(u16, u16)>,
    pub modes: InputModes,
}

pub trait Screens {
    fn with_screen<R>(&self, pane: PaneId, read: impl FnOnce(&vt100::Screen) -> R) -> Option<R>;
}

pub fn compose(layout: &Layout, size: Size, active: PaneId, screens: &impl Screens) -> Frame {
    let size = layout.fit(size);
    let mut grid = Grid::new(size);
    let mut cursor = None;
    let mut modes = InputModes::default();
    let mut active_rect = None;

    for (pane, rect) in layout.rects(size) {
        let focus = screens.with_screen(pane, |screen| {
            grid.paint(screen, rect);
            (pane == active).then(|| (pane_cursor(screen, rect), InputModes::from_screen(screen)))
        });
        if pane == active {
            active_rect = Some(rect);
            if let Some(Some((position, active_modes))) = focus {
                cursor = Some(position);
                modes = active_modes;
            }
        }
    }
    grid.draw_borders(&layout.borders(size), active_rect);

    Frame {
        grid,
        cursor,
        modes,
    }
}

fn pane_cursor(screen: &vt100::Screen, rect: Rect) -> (u16, u16) {
    let (row, col) = screen.cursor_position();
    (
        rect.row + row.min(rect.rows.saturating_sub(1)),
        rect.col + col.min(rect.cols.saturating_sub(1)),
    )
}

#[cfg(test)]
impl Screens for std::collections::BTreeMap<PaneId, vt100::Parser> {
    fn with_screen<R>(&self, pane: PaneId, read: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        self.get(&pane).map(|parser| read(parser.screen()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::grid::Style;
    use super::*;
    use crate::server::layout::SplitDirection;

    const LEFT: PaneId = PaneId(0);
    const RIGHT: PaneId = PaneId(1);

    fn size(cols: u16, rows: u16) -> Size {
        Size { rows, cols }
    }

    fn side_by_side(window: Size) -> (Layout, BTreeMap<PaneId, vt100::Parser>) {
        let mut layout = Layout::new(LEFT);
        layout
            .split(LEFT, RIGHT, SplitDirection::LeftRight, window)
            .unwrap();
        let panes = layout
            .rects(window)
            .into_iter()
            .map(|(pane, rect)| (pane, vt100::Parser::new(rect.rows, rect.cols, 0)))
            .collect();
        (layout, panes)
    }

    #[test]
    fn composing_paints_every_pane_and_the_borders() {
        let window = size(9, 3);
        let (layout, mut panes) = side_by_side(window);
        panes.get_mut(&LEFT).unwrap().process(b"left");
        panes.get_mut(&RIGHT).unwrap().process(b"righ");

        let frame = compose(&layout, window, RIGHT, &panes);
        let row: String = (0..9)
            .map(|col| frame.grid.cell(0, col).unwrap().text().to_owned())
            .collect();
        assert_eq!(row, "left│righ");
        assert_eq!(frame.grid.cell(2, 4).unwrap().text(), "│");
        assert_eq!(
            frame.grid.cell(2, 4).unwrap().style(),
            Style {
                fg: vt100::Color::Idx(2),
                ..Style::default()
            }
        );
    }

    #[test]
    fn the_cursor_and_modes_come_from_the_active_pane() {
        let window = size(9, 3);
        let (layout, mut panes) = side_by_side(window);
        panes
            .get_mut(&LEFT)
            .unwrap()
            .process(b"\x1b[?1h\x1b[?2004h\x1b[2;3H");
        panes
            .get_mut(&RIGHT)
            .unwrap()
            .process(b"\x1b=\x1b[?1000h\x1b[?1006h\x1b[?25l\r\nab");

        let left = compose(&layout, window, LEFT, &panes);
        assert_eq!(left.cursor, Some((1, 2)));
        assert_eq!(
            left.modes,
            InputModes {
                application_cursor: true,
                bracketed_paste: true,
                ..InputModes::default()
            }
        );

        let right = compose(&layout, window, RIGHT, &panes);
        assert_eq!(right.cursor, Some((1, 7)));
        assert_eq!(
            right.modes,
            InputModes {
                application_keypad: true,
                mouse_protocol_mode: MouseProtocolMode::PressRelease,
                mouse_protocol_encoding: MouseProtocolEncoding::Sgr,
                hide_cursor: true,
                ..InputModes::default()
            }
        );
    }

    #[test]
    fn a_cursor_waiting_to_wrap_stays_inside_its_pane() {
        let window = size(9, 3);
        let (layout, mut panes) = side_by_side(window);
        panes.get_mut(&LEFT).unwrap().process(b"abcd");
        assert_eq!(panes[&LEFT].screen().cursor_position(), (0, 4));

        let frame = compose(&layout, window, LEFT, &panes);
        assert_eq!(frame.cursor, Some((0, 3)));
    }

    #[test]
    fn a_missing_pane_leaves_its_rect_blank_and_hides_the_cursor() {
        let window = size(9, 3);
        let (layout, mut panes) = side_by_side(window);
        panes.remove(&RIGHT);

        let frame = compose(&layout, window, RIGHT, &panes);
        assert_eq!(frame.cursor, None);
        assert_eq!(frame.modes, InputModes::default());
        assert!(frame.grid.cell(0, 5).unwrap().is_erased());
    }

    #[test]
    fn a_window_too_small_for_the_layout_is_composed_at_its_minimum() {
        let (layout, panes) = side_by_side(size(9, 3));
        let frame = compose(&layout, size(1, 1), LEFT, &panes);
        assert_eq!(frame.grid.size(), size(3, 1));
    }
}
