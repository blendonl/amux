#[cfg(test)]
mod android_fixtures;
mod differ;
mod escape;
mod grid;
mod images;
pub mod placeholder;
#[cfg(test)]
mod round_trip;

use std::collections::BTreeSet;
use std::ops::Range;

pub use differ::GridDiffer;
use grid::{text_width, BorderLook, Grid};

use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use crate::protocol::Size;
use crate::server::graphics::derive::Look;
use crate::server::graphics::place::Placements;
use crate::server::graphics::store::ImageKey;
use crate::server::layout::{Layout, PaneId, Rect};
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InputModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub mouse_protocol_mode: MouseProtocolMode,
    pub mouse_protocol_encoding: MouseProtocolEncoding,
    pub hide_cursor: bool,
}

const COPY_MODES: InputModes = InputModes {
    application_cursor: false,
    application_keypad: false,
    bracketed_paste: true,
    mouse_protocol_mode: MouseProtocolMode::None,
    mouse_protocol_encoding: MouseProtocolEncoding::Default,
    hide_cursor: false,
};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageUse {
    pub key: u32,
    pub image: ImageKey,
    pub cols: u16,
    pub rows: u16,
    pub look: Option<Look>,
}

#[derive(Debug, Clone, Copy)]
pub struct Viewer<'a> {
    pub graphics: bool,
    pub hidden: &'a BTreeSet<u32>,
}

impl Viewer<'_> {
    #[cfg(test)]
    pub fn text() -> Viewer<'static> {
        static NOTHING_HIDDEN: BTreeSet<u32> = BTreeSet::new();
        Viewer {
            graphics: false,
            hidden: &NOTHING_HIDDEN,
        }
    }

    fn shows(&self, display: u32) -> bool {
        self.graphics && !self.hidden.contains(&display)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub grid: Grid,
    pub cursor: Option<(u16, u16)>,
    pub modes: InputModes,
    pub images: Vec<ImageUse>,
}

pub struct CopyView<'a> {
    pub screen: &'a vt100::Screen,
    pub top: usize,
    pub cursor: (u16, u16),
    pub position: String,
    pub selection: Vec<(u16, Range<u16>)>,
}

pub trait Screens {
    fn with_pane<R>(
        &self,
        pane: PaneId,
        read: impl FnOnce(&vt100::Screen, Option<&Placements>) -> R,
    ) -> Option<R>;

    fn copy_view(&self, _pane: PaneId) -> Option<CopyView<'_>> {
        None
    }
}

pub fn compose(
    layout: &Layout,
    size: Size,
    active: PaneId,
    screens: &impl Screens,
    settings: &Settings,
    viewer: Viewer<'_>,
) -> Frame {
    let size = layout.fit(size);
    let mut grid = Grid::new(size);
    let mut cursor = None;
    let mut modes = InputModes::default();
    let mut active_rect = None;
    let mut images = Vec::new();

    for (pane, rect) in layout.rects(size) {
        let focus = match screens.copy_view(pane) {
            Some(view) => {
                paint_copy(&mut grid, &view, rect, settings);
                Some((pane == active).then(|| (cursor_in(rect, view.cursor), COPY_MODES)))
            }
            None => screens.with_pane(pane, |screen, placements| {
                grid.paint(screen, rect);
                images::paint(&mut grid, screen, placements, rect, viewer, &mut images);
                (pane == active).then(|| {
                    (
                        cursor_in(rect, screen.cursor_position()),
                        InputModes::from_screen(screen),
                    )
                })
            }),
        };
        if pane == active {
            active_rect = Some(rect);
            if let Some(Some((position, active_modes))) = focus {
                cursor = Some(position);
                modes = active_modes;
            }
        }
    }
    grid.draw_borders(
        &layout.borders(size),
        active_rect,
        &BorderLook::new(settings),
    );
    images.sort_unstable();
    images.dedup();

    Frame {
        grid,
        cursor,
        modes,
        images,
    }
}

fn paint_copy(grid: &mut Grid, view: &CopyView<'_>, rect: Rect, settings: &Settings) {
    grid.paint_with(rect, |row, col| {
        view.screen
            .line_cell(view.top + usize::from(row), col)
            .map(images::text_cell)
    });
    let selected = settings.theme.copy_selection.into();
    for (row, cols) in &view.selection {
        let cols = rect.col.saturating_add(cols.start)..rect.col.saturating_add(cols.end);
        grid.restyle(rect.row.saturating_add(*row), cols, rect, selected);
    }
    let position = rect.right().saturating_sub(text_width(&view.position));
    grid.write_text(
        rect.row,
        position,
        rect,
        &view.position,
        settings.theme.copy_position.into(),
    );
}

fn cursor_in(rect: Rect, (row, col): (u16, u16)) -> (u16, u16) {
    (
        rect.row + row.min(rect.rows.saturating_sub(1)),
        rect.col + col.min(rect.cols.saturating_sub(1)),
    )
}

#[cfg(test)]
impl Screens for std::collections::BTreeMap<PaneId, vt100::Parser> {
    fn with_pane<R>(
        &self,
        pane: PaneId,
        read: impl FnOnce(&vt100::Screen, Option<&Placements>) -> R,
    ) -> Option<R> {
        self.get(&pane).map(|parser| read(parser.screen(), None))
    }
}

#[cfg(test)]
type GraphicsParser = std::sync::Mutex<vt100::Parser<crate::server::replies::PaneCallbacks>>;

#[cfg(test)]
impl Screens for std::collections::BTreeMap<PaneId, GraphicsParser> {
    fn with_pane<R>(
        &self,
        pane: PaneId,
        read: impl FnOnce(&vt100::Screen, Option<&Placements>) -> R,
    ) -> Option<R> {
        self.get(&pane).map(|parser| {
            let parser = parser.lock().unwrap();
            read(parser.screen(), parser.callbacks().placements())
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::grid::Style;
    use super::*;
    use crate::server::layout::SplitDirection;
    use crate::settings::{Color, StyleSpec};

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

        let frame = compose(
            &layout,
            window,
            RIGHT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
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
    fn composing_draws_borders_with_the_configured_glyphs_and_colors() {
        let window = size(9, 3);
        let (mut layout, mut panes) = side_by_side(window);
        let mut settings = Settings::default();
        settings.borders.vertical = '|';
        settings.theme.pane_border = StyleSpec {
            fg: Some(Color::Indexed(8)),
            ..StyleSpec::EMPTY
        };
        settings.theme.pane_border_active = StyleSpec {
            fg: Some(Color::Rgb(0x8e, 0xc0, 0x7c)),
            bold: Some(true),
            ..StyleSpec::EMPTY
        };

        let active = compose(&layout, window, RIGHT, &panes, &settings, Viewer::text());
        assert_eq!(active.grid.cell(1, 4).unwrap().text(), "|");
        assert_eq!(
            active.grid.cell(1, 4).unwrap().style(),
            Style {
                fg: vt100::Color::Rgb(0x8e, 0xc0, 0x7c),
                bold: true,
                ..Style::default()
            }
        );

        layout
            .split(RIGHT, PaneId(2), SplitDirection::TopBottom, window)
            .unwrap();
        panes.insert(PaneId(2), vt100::Parser::new(1, 4, 0));
        let frame = compose(
            &layout,
            window,
            PaneId(2),
            &panes,
            &settings,
            Viewer::text(),
        );
        assert_eq!(frame.grid.cell(0, 4).unwrap().text(), "|");
        assert_eq!(
            frame.grid.cell(0, 4).unwrap().style(),
            Style {
                fg: vt100::Color::Idx(8),
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

        let left = compose(
            &layout,
            window,
            LEFT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(left.cursor, Some((1, 2)));
        assert_eq!(
            left.modes,
            InputModes {
                application_cursor: true,
                bracketed_paste: true,
                ..InputModes::default()
            }
        );

        let right = compose(
            &layout,
            window,
            RIGHT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
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

        let frame = compose(
            &layout,
            window,
            LEFT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(frame.cursor, Some((0, 3)));
    }

    #[test]
    fn a_missing_pane_leaves_its_rect_blank_and_hides_the_cursor() {
        let window = size(9, 3);
        let (layout, mut panes) = side_by_side(window);
        panes.remove(&RIGHT);

        let frame = compose(
            &layout,
            window,
            RIGHT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(frame.cursor, None);
        assert_eq!(frame.modes, InputModes::default());
        assert!(frame.grid.cell(0, 5).unwrap().is_erased());
    }

    struct Browsing {
        panes: BTreeMap<PaneId, vt100::Parser>,
        copy: BTreeMap<PaneId, (vt100::Screen, usize)>,
        selection: Vec<(u16, Range<u16>)>,
    }

    impl Screens for Browsing {
        fn with_pane<R>(
            &self,
            pane: PaneId,
            read: impl FnOnce(&vt100::Screen, Option<&Placements>) -> R,
        ) -> Option<R> {
            self.panes.with_pane(pane, read)
        }

        fn copy_view(&self, pane: PaneId) -> Option<CopyView<'_>> {
            self.copy.get(&pane).map(|(screen, top)| CopyView {
                screen,
                top: *top,
                cursor: (1, 2),
                position: "[3/5]".into(),
                selection: self.selection.clone(),
            })
        }
    }

    fn browsing(window: Size, left: &str) -> (Layout, Browsing) {
        let (layout, mut panes) = side_by_side(window);
        let rect = layout.rects(window)[0].1;
        let mut parser = vt100::Parser::new(rect.rows, rect.cols, 10);
        parser.process(left.as_bytes());
        let snapshot = parser.screen().clone();
        parser.process(b"\r\nlive output\x1b[?25l\x1b[?1000h");
        panes.insert(LEFT, parser);
        let copy = BTreeMap::from([(LEFT, (snapshot, 1))]);
        (
            layout,
            Browsing {
                panes,
                copy,
                selection: Vec::new(),
            },
        )
    }

    fn text_of(frame: &Frame, row: u16, cols: std::ops::Range<u16>) -> String {
        cols.map(|col| match frame.grid.cell(row, col).unwrap().text() {
            "" => ".".to_owned(),
            text => text.to_owned(),
        })
        .collect()
    }

    #[test]
    fn a_copy_pane_shows_its_snapshot_from_the_top_line_with_its_position() {
        let window = size(21, 3);
        let (layout, browsing) = browsing(window, "a\r\nb\r\nc\r\nd\r\ne");
        let frame = compose(
            &layout,
            window,
            RIGHT,
            &browsing,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(text_of(&frame, 0, 0..10), "b....[3/5]");
        assert_eq!(text_of(&frame, 1, 0..10), "c.........");
        assert_eq!(text_of(&frame, 2, 0..10), "d.........");
        assert_eq!(
            frame.grid.cell(0, 5).unwrap().style(),
            Style::from(Settings::default().theme.copy_position)
        );
        assert_eq!(frame.grid.cell(0, 4).unwrap().style(), Style::default());
        assert_eq!(frame.modes, InputModes::default());
    }

    #[test]
    fn an_active_copy_pane_takes_the_cursor_and_accepts_pastes() {
        let window = size(21, 3);
        let (layout, browsing) = browsing(window, "a\r\nb\r\nc\r\nd\r\ne");
        let frame = compose(
            &layout,
            window,
            LEFT,
            &browsing,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(frame.cursor, Some((1, 2)));
        assert_eq!(
            frame.modes,
            InputModes {
                bracketed_paste: true,
                ..InputModes::default()
            }
        );
    }

    #[test]
    fn the_position_is_cut_to_a_narrow_pane() {
        let window = size(9, 3);
        let (layout, browsing) = browsing(window, "a\r\nb\r\nc\r\nd\r\ne");
        let frame = compose(
            &layout,
            window,
            LEFT,
            &browsing,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(text_of(&frame, 0, 0..5), "[3/5│");
    }

    #[test]
    fn a_copy_pane_draws_its_selection_under_the_position() {
        let window = size(21, 3);
        let (layout, mut browsing) = browsing(window, "a\r\nb\r\nx日y\r\nd\r\ne");
        browsing.selection = vec![(0, 2..10), (1, 0..2)];
        let frame = compose(
            &layout,
            window,
            RIGHT,
            &browsing,
            &Settings::default(),
            Viewer::text(),
        );
        let theme = Settings::default().theme;
        let styles = |row: u16| -> Vec<Style> {
            (0..10)
                .map(|col| frame.grid.cell(row, col).unwrap().style())
                .collect()
        };
        let plain = Style::default();
        let selected = Style::from(theme.copy_selection);
        let position = Style::from(theme.copy_position);
        assert!(selected.inverse);
        assert_eq!(
            styles(0),
            [plain, plain, selected, selected, selected]
                .into_iter()
                .chain([position; 5])
                .collect::<Vec<_>>()
        );
        assert_eq!(
            styles(1),
            [selected; 3]
                .into_iter()
                .chain([plain; 7])
                .collect::<Vec<_>>()
        );
        assert_eq!(styles(2), [plain; 10]);
        assert_eq!(text_of(&frame, 1, 0..4), "x日.y");
    }

    #[test]
    fn a_copy_pane_blanks_image_placeholders_the_way_a_pane_without_images_does() {
        let window = size(21, 3);
        let placeholders = "a\r\nb\x1b[38;5;7;44m\u{10eeee}\u{305}\u{305}\u{10eeee}\x1b[0mc";
        let (layout, mut browsing) = browsing(window, placeholders);
        browsing.copy.get_mut(&LEFT).unwrap().1 = 0;
        let browsed = compose(
            &layout,
            window,
            LEFT,
            &browsing,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(text_of(&browsed, 1, 0..10), "b..c......");
        assert_eq!(
            browsed.grid.cell(1, 1).unwrap().style(),
            Style {
                bg: vt100::Color::Idx(4),
                ..Style::default()
            }
        );
        assert!(browsed.images.is_empty());

        let mut live = vt100::Parser::new(3, 10, 10);
        live.process(placeholders.as_bytes());
        let panes = BTreeMap::from([(LEFT, live)]);
        let shown = compose(
            &layout,
            window,
            LEFT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
        for col in 0..10 {
            assert_eq!(browsed.grid.cell(1, col), shown.grid.cell(1, col), "{col}");
        }
    }

    #[test]
    fn a_window_too_small_for_the_layout_is_composed_at_its_minimum() {
        let (layout, panes) = side_by_side(size(9, 3));
        let frame = compose(
            &layout,
            size(1, 1),
            LEFT,
            &panes,
            &Settings::default(),
            Viewer::text(),
        );
        assert_eq!(frame.grid.size(), size(3, 1));
    }
}
