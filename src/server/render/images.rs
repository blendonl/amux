use vt100::Color;

use super::grid::{Cell, Grid, Style};
use super::placeholder::{placeholder_cell, ImageSpan, DIACRITICS, PLACEHOLDER};
use super::{ImageUse, Viewer};
use crate::server::graphics::place::{PaneSpan, PlacementId, PlacementKind, Placements};
use crate::server::layout::Rect;

pub fn paint(
    grid: &mut Grid,
    screen: &vt100::Screen,
    placements: Option<&Placements>,
    rect: Rect,
    viewer: Viewer<'_>,
    uses: &mut Vec<ImageUse>,
    image_rows: &mut Vec<u16>,
) {
    image_rows.clear();
    let mut resolver = Resolver {
        screen,
        placements: placements.filter(|_| viewer.graphics),
        viewer,
        last: None,
    };
    for (row, visible) in (0..rect.rows).zip(screen.visible_rows()) {
        if visible.has_placeholders {
            rewrite_row(grid, visible.cells, row, rect, &mut resolver, uses);
        }
    }
    if let Some(placements) = resolver
        .placements
        .filter(|placements| !placements.is_empty())
    {
        paint_spans(grid, screen, placements, rect, viewer, uses, image_rows);
    }
}

pub fn text_cell(cell: &vt100::Cell) -> Cell {
    if cell.contents().starts_with(PLACEHOLDER) {
        blanked(cell.bgcolor())
    } else {
        Cell::from_vt100(cell)
    }
}

fn blanked(background: Color) -> Cell {
    Cell::blank(Style {
        bg: background,
        ..Style::default()
    })
}

fn paint_spans(
    grid: &mut Grid,
    screen: &vt100::Screen,
    placements: &Placements,
    rect: Rect,
    viewer: Viewer<'_>,
    uses: &mut Vec<ImageUse>,
    image_rows: &mut Vec<u16>,
) {
    let mut shown: Option<(PlacementId, bool)> = None;
    for span in placements.spans(screen) {
        let visible = match shown {
            Some((placement, visible)) if placement == span.placement => visible,
            _ => {
                let visible = viewer.shows(span.display.key) && placements.is_stored(span.key);
                if visible {
                    uses.push(span.display);
                }
                shown = Some((span.placement, visible));
                visible
            }
        };
        if !visible {
            continue;
        }
        image_rows.push(span.row);
        let cells = screen
            .visible_row(span.row)
            .map_or(&[][..], |row| row.cells);
        match (span.kind, span.under_text) {
            (PlacementKind::Sixel, _) => paint_where(grid, cells, &span, rect, |cell| {
                cell.is_some_and(vt100::Cell::is_graphic)
            }),
            (PlacementKind::Kitty, true) => {
                paint_where(grid, cells, &span, rect, |cell| cell.is_none_or(is_blank));
            }
            (PlacementKind::Kitty, false) => {
                grid.paint_image(&window_span(&span, rect, 0, span.cols), rect);
            }
        }
    }
}

fn paint_where(
    grid: &mut Grid,
    cells: &[vt100::Cell],
    span: &PaneSpan,
    rect: Rect,
    shows: impl Fn(Option<&vt100::Cell>) -> bool,
) {
    let mut start = None;
    for offset in 0..=span.cols {
        let col = usize::from(span.col.saturating_add(offset));
        let free = offset < span.cols && shows(cells.get(col));
        match (free, start) {
            (true, None) => start = Some(offset),
            (false, Some(first)) => {
                grid.paint_image(&window_span(span, rect, first, offset - first), rect);
                start = None;
            }
            _ => {}
        }
    }
}

fn is_blank(cell: &vt100::Cell) -> bool {
    !cell.is_wide_continuation() && cell.contents().trim().is_empty()
}

fn window_span(span: &PaneSpan, rect: Rect, skip: u16, cols: u16) -> ImageSpan {
    ImageSpan {
        key: span.display.key,
        image_row: span.image_row,
        image_col: span.image_col.saturating_add(skip),
        row: rect.row.saturating_add(span.row),
        col: rect.col.saturating_add(span.col).saturating_add(skip),
        cols,
    }
}

fn rewrite_row(
    grid: &mut Grid,
    cells: &[vt100::Cell],
    row: u16,
    rect: Rect,
    resolver: &mut Resolver<'_>,
    uses: &mut Vec<ImageUse>,
) {
    let mut decoder = RowDecoder::default();
    for (col, cell) in (0..rect.cols).zip(cells) {
        let Some(placeholder) = decoder.decode(cell) else {
            continue;
        };
        let background = cell.bgcolor();
        let rewritten = match resolver.resolve(placeholder.image, placeholder.placement, uses) {
            Some(shown) => placeholder_cell(
                shown.key,
                placeholder.image_row,
                placeholder.image_col,
                background,
            ),
            None => blanked(background),
        };
        grid.set(rect.row + row, rect.col + col, rewritten);
    }
}

struct Resolver<'a> {
    screen: &'a vt100::Screen,
    placements: Option<&'a Placements>,
    viewer: Viewer<'a>,
    last: Option<((u32, u32), Option<ImageUse>)>,
}

impl Resolver<'_> {
    fn resolve(
        &mut self,
        image: u32,
        placement: u32,
        uses: &mut Vec<ImageUse>,
    ) -> Option<ImageUse> {
        if let Some((ids, shown)) = self.last {
            if ids == (image, placement) {
                return shown;
            }
        }
        let shown = self.placements.and_then(|placements| {
            placements
                .virtual_placement(self.screen, image, placement)
                .filter(|shown| self.viewer.shows(shown.key) && placements.is_stored(shown.image))
        });
        uses.extend(shown);
        self.last = Some(((image, placement), shown));
        shown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placeholder {
    image: u32,
    placement: u32,
    image_row: u16,
    image_col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Left {
    foreground: Color,
    underline: Color,
    image_row: u16,
    image_col: u16,
    high: u16,
}

#[derive(Default)]
struct RowDecoder {
    left: Option<Left>,
}

impl RowDecoder {
    fn decode(&mut self, cell: &vt100::Cell) -> Option<Placeholder> {
        let decoded = decode(self.left, cell);
        self.left = decoded.map(|(left, _)| left);
        decoded.map(|(_, placeholder)| placeholder)
    }
}

fn decode(left: Option<Left>, cell: &vt100::Cell) -> Option<(Left, Placeholder)> {
    let mut chars = cell.contents().chars();
    if chars.next() != Some(PLACEHOLDER) {
        return None;
    }
    let foreground = cell.fgcolor();
    let underline = cell.underline_color();
    let mut marks = chars.map(diacritic_index);
    let (row, col, high) = (
        marks.next().flatten(),
        marks.next().flatten(),
        marks.next().flatten(),
    );
    let inherited = left.filter(|left| {
        left.foreground == foreground
            && left.underline == underline
            && row.is_none_or(|row| row == left.image_row)
            && col.is_none_or(|col| Some(col) == left.image_col.checked_add(1))
            && high.is_none_or(|high| high == left.high)
    });
    let this = match inherited {
        Some(left) => Left {
            image_col: left.image_col.saturating_add(1),
            ..left
        },
        None => Left {
            foreground,
            underline,
            image_row: row.unwrap_or(0),
            image_col: col.unwrap_or(0),
            high: high.unwrap_or(0),
        },
    };
    let image = match color_id(foreground) {
        Some(low) => (u32::from(this.high.min(u16::from(u8::MAX))) << 24) | low,
        None => 0,
    };
    let placeholder = Placeholder {
        image,
        placement: color_id(underline).unwrap_or(0),
        image_row: this.image_row,
        image_col: this.image_col,
    };
    Some((this, placeholder))
}

fn color_id(color: Color) -> Option<u32> {
    match color {
        Color::Default => None,
        Color::Idx(index) => Some(u32::from(index)),
        Color::Rgb(red, green, blue) => Some(u32::from_be_bytes([0, red, green, blue])),
    }
}

fn diacritic_index(mark: char) -> Option<u16> {
    DIACRITICS
        .binary_search(&mark)
        .ok()
        .and_then(|index| u16::try_from(index).ok())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{mpsc, Arc, Mutex};

    use super::super::{compose, Composer, Frame, GraphicsParser};
    use super::*;
    use crate::protocol::Size;
    use crate::server::graphics::derive::{Look, Sizing};
    use crate::server::graphics::place::tests::allocations;
    use crate::server::graphics::place::{CellOffset, SourceRect, ASSUMED_CELL_PIXELS};
    use crate::server::graphics::store::ImageStore;
    use crate::server::graphics::PaneGraphics;
    use crate::server::layout::{Layout, PaneId, SplitDirection};
    use crate::server::replies::PaneCallbacks;
    use crate::settings::Settings;

    const LEFT: PaneId = PaneId(0);
    const RIGHT: PaneId = PaneId(1);
    const WINDOW: Size = Size { rows: 6, cols: 21 };
    static NOTHING_HIDDEN: BTreeSet<u32> = BTreeSet::new();

    struct Panes {
        layout: Layout,
        store: Arc<ImageStore>,
        parsers: BTreeMap<PaneId, GraphicsParser>,
        graphics: BTreeMap<PaneId, PaneGraphics>,
    }

    impl Panes {
        fn side_by_side() -> Self {
            let store = Arc::new(ImageStore::new(1 << 30));
            let mut layout = Layout::new(LEFT);
            layout
                .split(LEFT, RIGHT, SplitDirection::LeftRight, WINDOW)
                .unwrap();
            let mut parsers = BTreeMap::new();
            let mut graphics = BTreeMap::new();
            for (pane, rect) in layout.rects(WINDOW) {
                let images = store.open_pane();
                let callbacks = PaneCallbacks::new(mpsc::channel().0, Some(images.clone()));
                let parser = vt100::Parser::new_with_callbacks(rect.rows, rect.cols, 0, callbacks);
                parsers.insert(pane, Mutex::new(parser));
                graphics.insert(pane, PaneGraphics::new(images));
            }
            Self {
                layout,
                store,
                parsers,
                graphics,
            }
        }

        fn feed(&mut self, pane: PaneId, output: &str) {
            let parser = &self.parsers[&pane];
            self.graphics
                .get_mut(&pane)
                .unwrap()
                .process(output.as_bytes(), parser);
        }

        fn transmit(&mut self, pane: PaneId, keys: &str, width: u32, height: u32) {
            let pixels = "AAAA".repeat(usize::try_from(width * height).unwrap());
            self.feed(
                pane,
                &format!("\x1b_Ga=T,q=2,f=24,s={width},v={height},{keys};{pixels}\x1b\\"),
            );
        }

        fn frame(&self, viewer: Viewer<'_>) -> Frame {
            compose(
                &self.layout,
                WINDOW,
                LEFT,
                &self.parsers,
                &Settings::default(),
                viewer,
            )
        }

        fn shown(&self) -> Frame {
            self.frame(graphics())
        }
    }

    fn graphics() -> Viewer<'static> {
        Viewer {
            graphics: true,
            hidden: &NOTHING_HIDDEN,
        }
    }

    fn image_cell(frame: &Frame, row: u16, col: u16) -> Option<(u32, u16, u16)> {
        let cell = frame.grid.cell(row, col)?;
        let mut chars = cell.text().chars();
        if chars.next()? != PLACEHOLDER {
            return None;
        }
        let mut index = || chars.next().and_then(diacritic_index);
        let (image_row, image_col, high) = (index()?, index()?, index()?);
        let Color::Rgb(red, green, blue) = cell.style().fg else {
            return None;
        };
        let key = u32::from_be_bytes([u8::try_from(high).ok()?, red, green, blue]);
        Some((key, image_row, image_col))
    }

    fn row_text(frame: &Frame, row: u16) -> String {
        (0..WINDOW.cols)
            .map(|col| {
                let cell = frame.grid.cell(row, col).unwrap();
                match cell.text() {
                    "" if cell.is_wide_continuation() => "",
                    "" => ".",
                    text if text.starts_with(PLACEHOLDER) => "#",
                    text => text,
                }
            })
            .collect()
    }

    #[test]
    fn a_placement_is_painted_at_its_pane_offset_and_stops_at_the_border() {
        let mut panes = Panes::side_by_side();
        panes.feed(LEFT, "\x1b[2;8H");
        panes.transmit(LEFT, "i=1,C=1", 50, 40);
        panes.feed(RIGHT, "\x1b[1;3H");
        panes.transmit(RIGHT, "i=1,C=1", 30, 20);
        let frame = panes.shown();

        let [left, right] = frame.images[..] else {
            panic!("expected two images, got {:?}", frame.images);
        };
        assert_eq!((left.cols, left.rows), (5, 2));
        assert_eq!((right.cols, right.rows), (3, 1));
        assert_eq!(row_text(&frame, 0), "..........│..###.....");
        assert_eq!(row_text(&frame, 1), ".......###│..........");
        assert_eq!(row_text(&frame, 2), ".......###│..........");
        assert_eq!(image_cell(&frame, 1, 7), Some((left.key, 0, 0)));
        assert_eq!(image_cell(&frame, 2, 9), Some((left.key, 1, 2)));
        assert_eq!(image_cell(&frame, 0, 13), Some((right.key, 0, 0)));
        assert_eq!(image_cell(&frame, 0, 15), Some((right.key, 0, 2)));
    }

    #[test]
    fn the_highest_z_wins_where_placements_overlap() {
        let mut panes = Panes::side_by_side();
        panes.transmit(LEFT, "i=1,C=1,z=5", 40, 20);
        panes.feed(LEFT, "\x1b[1;3H");
        panes.transmit(LEFT, "i=2,C=1,z=1", 40, 20);
        let frame = panes.shown();

        let keys: Vec<u32> = (0..6)
            .map(|col| image_cell(&frame, 0, col).unwrap().0)
            .collect();
        let high = keys[0];
        let low = keys[5];
        assert_ne!(high, low);
        assert_eq!(keys, [high, high, high, high, low, low]);
        assert_eq!(image_cell(&frame, 0, 4), Some((low, 0, 2)));
    }

    #[test]
    fn text_wins_over_an_image_below_it() {
        let mut panes = Panes::side_by_side();
        panes.transmit(LEFT, "i=1,C=1,z=-1", 100, 20);
        panes.feed(LEFT, "ab  c中e");
        let frame = panes.shown();

        assert_eq!(row_text(&frame, 0), "ab##c中e##│..........");
        let key = frame.images[0].key;
        assert_eq!(image_cell(&frame, 0, 2), Some((key, 0, 2)));
        assert_eq!(image_cell(&frame, 0, 9), Some((key, 0, 9)));
    }

    #[test]
    fn a_client_without_graphics_sees_the_text_under_the_image() {
        let mut panes = Panes::side_by_side();
        panes.feed(LEFT, "hello\x1b[1;1H");
        panes.transmit(LEFT, "i=1,C=1", 30, 20);

        let text = panes.frame(Viewer::text());
        assert_eq!(row_text(&text, 0), "hello.....│..........");
        assert!(text.images.is_empty());
        let shown = panes.shown();
        assert_eq!(row_text(&shown, 0), "###lo.....│..........");
    }

    #[test]
    fn an_image_the_client_cannot_hold_is_left_as_text() {
        let mut panes = Panes::side_by_side();
        panes.feed(LEFT, "hello\x1b[1;1H");
        panes.transmit(LEFT, "i=1,C=1", 30, 20);
        let key = panes.shown().images[0].key;

        let hidden = BTreeSet::from([key]);
        let frame = panes.frame(Viewer {
            graphics: true,
            hidden: &hidden,
        });
        assert_eq!(row_text(&frame, 0), "hello.....│..........");
        assert!(frame.images.is_empty());
    }

    fn sixel(width: u32, height: u32) -> String {
        format!("\x1bPq\"1;1;{width};{height}#1~\x1b\\")
    }

    #[test]
    fn text_printed_over_a_sixel_shows_through_it() {
        let mut panes = Panes::side_by_side();
        panes.feed(LEFT, &sixel(30, 40));
        panes.feed(LEFT, "\x1b[1;2Hx");
        let frame = panes.shown();

        let [shown] = frame.images[..] else {
            panic!("expected one image, got {:?}", frame.images);
        };
        assert_eq!((shown.cols, shown.rows, shown.look), (3, 2, None));
        assert_eq!(row_text(&frame, 0), "#x#.......│..........");
        assert_eq!(row_text(&frame, 1), "###.......│..........");
        assert_eq!(image_cell(&frame, 0, 2), Some((shown.key, 0, 2)));
        assert_eq!(image_cell(&frame, 1, 0), Some((shown.key, 1, 0)));

        let text = panes.frame(Viewer::text());
        assert_eq!(row_text(&text, 0), ".x........│..........");
        assert!(text.images.is_empty());
    }

    #[test]
    fn the_newest_sixel_wins_where_two_overlap() {
        let mut panes = Panes::side_by_side();
        panes.feed(LEFT, &sixel(30, 20));
        panes.feed(LEFT, &format!("\x1b[1;2H{}", sixel(30, 20)));
        let frame = panes.shown();

        let [older, newer] = frame.images[..] else {
            panic!("expected two images, got {:?}", frame.images);
        };
        assert_eq!(row_text(&frame, 0), "####......│..........");
        assert_eq!(image_cell(&frame, 0, 0), Some((older.key, 0, 0)));
        assert_eq!(image_cell(&frame, 0, 1), Some((newer.key, 0, 0)));
        assert_eq!(image_cell(&frame, 0, 3), Some((newer.key, 0, 2)));
    }

    #[test]
    fn composing_a_cropped_placement_reports_its_look_without_deriving_it() {
        let mut panes = Panes::side_by_side();
        panes.transmit(LEFT, "i=1,C=1,x=10,w=20,h=20,X=3", 40, 20);
        let frame = panes.shown();

        let [shown] = frame.images[..] else {
            panic!("expected one image, got {:?}", frame.images);
        };
        assert_eq!(
            shown.look,
            Some(Look {
                source: SourceRect {
                    x: 10,
                    y: 0,
                    width: 20,
                    height: 20
                },
                offset: CellOffset { x: 3, y: 0 },
                sizing: Sizing::Native,
            })
        );
        assert_eq!(panes.store.derived(shown.key, ASSUMED_CELL_PIXELS), None);
        assert_eq!(image_cell(&frame, 0, 2), Some((shown.key, 0, 2)));
    }

    #[test]
    fn placeholders_a_program_prints_are_rewritten_to_the_display_key() {
        let mut panes = Panes::side_by_side();
        panes.transmit(RIGHT, "i=42,p=7,U=1,c=3,r=2", 30, 40);
        panes.feed(
            RIGHT,
            "\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\u{10eeee}\u{10eeee}\x1b[0m\r\n\
             \x1b[38;2;0;0;42;58;5;7m\u{10eeee}\u{30d}\u{305}\u{10eeee}\u{30d}\x1b[0m\r\n\
             \x1b[38;5;43;48;5;4m\u{10eeee}\u{305}\u{305}\x1b[0m",
        );
        let frame = panes.shown();

        let [shown] = frame.images[..] else {
            panic!("expected one image, got {:?}", frame.images);
        };
        assert_eq!((shown.cols, shown.rows), (3, 2));
        let key = shown.key;
        assert_eq!(image_cell(&frame, 0, 11), Some((key, 0, 0)));
        assert_eq!(image_cell(&frame, 0, 12), Some((key, 0, 1)));
        assert_eq!(image_cell(&frame, 0, 13), Some((key, 0, 2)));
        assert_eq!(image_cell(&frame, 1, 11), Some((key, 1, 0)));
        assert_eq!(image_cell(&frame, 1, 12), Some((key, 1, 1)));
        let unknown = frame.grid.cell(2, 11).unwrap();
        assert!(unknown.is_erased());
        assert_eq!(unknown.style().bg, Color::Idx(4));

        let text = panes.frame(Viewer::text());
        assert!(text.images.is_empty());
        for (row, col) in [(0, 11), (0, 13), (1, 12), (2, 11)] {
            assert!(
                text.grid.cell(row, col).unwrap().is_erased(),
                "({row}, {col})"
            );
        }
    }

    #[test]
    fn a_placeholder_of_a_pane_without_images_is_blanked() {
        let layout = Layout::new(LEFT);
        let mut parser = vt100::Parser::new(WINDOW.rows, WINDOW.cols, 0);
        parser.process("a\x1b[38;5;1m\u{10eeee}\u{305}\u{305}\x1b[0mb".as_bytes());
        let panes = BTreeMap::from([(LEFT, parser)]);
        let frame = compose(
            &layout,
            WINDOW,
            LEFT,
            &panes,
            &Settings::default(),
            graphics(),
        );
        assert!(row_text(&frame, 0).starts_with("a.b."));
        assert!(frame.images.is_empty());
    }

    fn decoded(output: &str) -> Vec<Option<(u32, u32, u16, u16)>> {
        let mut parser = vt100::Parser::new(1, 12, 0);
        parser.process(output.as_bytes());
        let mut decoder = RowDecoder::default();
        (0..12)
            .map(|col| {
                let cell = parser.screen().cell(0, col).unwrap();
                decoder.decode(cell).map(|placeholder| {
                    (
                        placeholder.image,
                        placeholder.placement,
                        placeholder.image_row,
                        placeholder.image_col,
                    )
                })
            })
            .take_while(Option::is_some)
            .collect()
    }

    #[test]
    fn missing_diacritics_are_inherited_from_the_cell_to_the_left() {
        let p = "\u{10eeee}";
        assert_eq!(
            decoded(&format!(
                "\x1b[38;5;9m{p}\u{30d}\u{30e}\u{30d}{p}{p}\u{30d}{p}\u{30d}\u{33d}{p}\u{30d}\u{312}\u{310}"
            )),
            [
                Some((0x0100_0009, 0, 1, 2)),
                Some((0x0100_0009, 0, 1, 3)),
                Some((0x0100_0009, 0, 1, 4)),
                Some((0x0100_0009, 0, 1, 5)),
                Some((0x0300_0009, 0, 1, 4)),
            ]
        );
    }

    #[test]
    fn inheritance_stops_at_a_new_row_a_jump_or_another_colour() {
        let p = "\u{10eeee}";
        assert_eq!(
            decoded(&format!(
                "\x1b[38;5;9m{p}\u{30d}\u{30e}\u{30d}{p}\u{30e}{p}\u{30e}\u{305}\x1b[38;5;8m{p}\x1b[58;5;3m{p}"
            )),
            [
                Some((0x0100_0009, 0, 1, 2)),
                Some((9, 0, 2, 0)),
                Some((9, 0, 2, 0)),
                Some((8, 0, 0, 0)),
                Some((8, 3, 0, 0)),
            ]
        );
        assert_eq!(
            decoded(&format!("\x1b[38;2;1;2;3m{p}\u{305}\x1b[39m{p}")),
            [Some((0x0001_0203, 0, 0, 0)), Some((0, 0, 0, 0))]
        );
        assert_eq!(decoded("x"), []);
    }

    #[test]
    fn the_diacritics_are_sorted_so_they_can_be_searched() {
        assert!(DIACRITICS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(diacritic_index('\u{305}'), Some(0));
        assert_eq!(diacritic_index('\u{1d244}'), Some(296));
        assert_eq!(diacritic_index('a'), None);
    }

    #[test]
    fn a_moved_image_leaves_no_cells_behind() {
        let mut panes = Panes::side_by_side();
        let settings = Arc::new(Settings::default());
        let mut composer = Composer::default();
        let mut recompose = |panes: &Panes| {
            let frame = composer.compose(
                &panes.layout,
                WINDOW,
                LEFT,
                &panes.parsers,
                &settings,
                graphics(),
            );
            assert_eq!(*frame, panes.shown());
            frame.clone()
        };
        panes.feed(LEFT, "text\x1b[2;3H");
        panes.transmit(LEFT, "i=1,p=1,C=1", 30, 40);
        let placed = recompose(&panes);
        assert_eq!(row_text(&placed, 1), "..###.....│..........");
        assert_eq!(row_text(&placed, 2), "..###.....│..........");

        panes.feed(LEFT, "\x1b[4;6H\x1b_Ga=p,i=1,p=1,C=1,q=2\x1b\\");
        let moved = recompose(&panes);
        assert_eq!(row_text(&moved, 0), "text......│..........");
        assert_eq!(row_text(&moved, 1), "..........│..........");
        assert_eq!(row_text(&moved, 2), "..........│..........");
        assert_eq!(row_text(&moved, 3), ".....###..│..........");

        panes.feed(LEFT, "\x1b_Ga=d,q=2\x1b\\");
        let deleted = recompose(&panes);
        assert_eq!(row_text(&deleted, 3), "..........│..........");
        assert!(deleted.images.is_empty());
    }

    #[test]
    fn painting_a_pane_without_images_allocates_nothing() {
        let panes = Panes::side_by_side();
        let parser = panes.parsers[&LEFT].lock().unwrap();
        let mut grid = Grid::new(WINDOW);
        let mut uses = Vec::new();
        let rect = panes.layout.rects(WINDOW)[0].1;
        let before = allocations();
        paint(
            &mut grid,
            parser.screen(),
            parser.callbacks().placements(),
            rect,
            graphics(),
            &mut uses,
            &mut Vec::new(),
        );
        assert_eq!(allocations() - before, 0);
    }
}
