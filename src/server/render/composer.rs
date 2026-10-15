use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::grid::{BorderLook, Cell, Grid};
use super::images::ImageCache;
use super::{cursor_in, images, paint_copy, Frame, InputModes, Screens, Viewer, COPY_MODES};
use crate::protocol::Size;
use crate::server::layout::{Border, Layout, PaneId, Rect};
use crate::settings::Settings;

static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

pub struct Composer {
    frame: Frame,
    placed: Option<Placed>,
    active: Option<PaneId>,
    settings: Option<Arc<Settings>>,
    panes: BTreeMap<PaneId, PaneCache>,
}

struct Placed {
    layout: Layout,
    size: Size,
    fitted: Size,
    rects: Vec<(PaneId, Rect)>,
    borders: Vec<Border>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Live,
    Copy(u64),
}

#[derive(Default)]
struct PaneCache {
    rect: Rect,
    source: Option<Source>,
    rows: Vec<Option<(u64, u64)>>,
    images: ImageCache,
}

impl Default for Composer {
    fn default() -> Self {
        Self {
            frame: Frame {
                grid: Grid::new(Size { rows: 0, cols: 0 }),
                cursor: None,
                modes: InputModes::default(),
                images: Vec::new(),
                source: next_source(),
            },
            placed: None,
            active: None,
            settings: None,
            panes: BTreeMap::new(),
        }
    }
}

impl Composer {
    pub fn compose(
        &mut self,
        layout: &Layout,
        size: Size,
        active: PaneId,
        screens: &impl Screens,
        settings: &Arc<Settings>,
        viewer: Viewer<'_>,
    ) -> &mut Frame {
        let Self {
            frame,
            placed,
            active: painted_active,
            settings: painted_settings,
            panes,
        } = self;
        let reshaped = !placed
            .as_ref()
            .is_some_and(|placed| placed.layout == *layout && placed.size == size);
        if reshaped {
            *placed = None;
        }
        let placed = placed.get_or_insert_with(|| Placed::new(layout, size));
        let restyled = !painted_settings
            .as_ref()
            .is_some_and(|painted| Arc::ptr_eq(painted, settings));
        let repaint = reshaped || restyled || *painted_active != Some(active);
        frame.grid.next_generation();
        if repaint {
            frame.grid.reset(placed.fitted);
            frame.source = next_source();
            panes.clear();
        }
        frame.cursor = None;
        frame.modes = InputModes::default();
        frame.images.clear();

        for &(pane, rect) in &placed.rects {
            let cache = panes.entry(pane).or_default();
            if cache.rect != rect {
                cache.reset(rect, None);
            }
            let focus = match screens.copy_view(pane) {
                Some(view) => {
                    let source = Some(Source::Copy(view.generation));
                    if cache.source != source {
                        paint_copy(&mut frame.grid, &view, rect, settings);
                        cache.reset(rect, source);
                    }
                    Some(
                        (pane == active)
                            .then(|| (cursor_in(rect, view.focus(rect.rows)), COPY_MODES)),
                    )
                }
                None => screens.with_pane(pane, |screen, placements| {
                    if cache.source != Some(Source::Live) {
                        cache.reset(rect, Some(Source::Live));
                    }
                    cache.paint_rows(&mut frame.grid, screen);
                    images::paint(
                        &mut frame.grid,
                        screen,
                        placements,
                        rect,
                        viewer,
                        &mut frame.images,
                        &mut cache.images,
                    );
                    (pane == active).then(|| {
                        (
                            cursor_in(rect, screen.cursor_position()),
                            InputModes::from_screen(screen),
                        )
                    })
                }),
            };
            if focus.is_none() && cache.source.is_some() {
                blank(&mut frame.grid, rect);
                cache.reset(rect, None);
            }
            if let Some(Some((position, modes))) = focus.filter(|_| pane == active) {
                frame.cursor = Some(position);
                frame.modes = modes;
            }
        }
        if repaint {
            let active_rect = placed
                .rects
                .iter()
                .find(|(pane, _)| *pane == active)
                .map(|&(_, rect)| rect);
            frame
                .grid
                .draw_borders(&placed.borders, active_rect, &BorderLook::new(settings));
            *painted_active = Some(active);
            *painted_settings = Some(Arc::clone(settings));
        }
        frame.images.sort_unstable();
        frame.images.dedup();
        frame
    }
}

#[cfg(test)]
pub fn compose(
    layout: &Layout,
    size: Size,
    active: PaneId,
    screens: &impl Screens,
    settings: &Settings,
    viewer: Viewer<'_>,
) -> Frame {
    let mut composer = Composer::default();
    composer.compose(
        layout,
        size,
        active,
        screens,
        &Arc::new(settings.clone()),
        viewer,
    );
    composer.frame
}

impl Placed {
    fn new(layout: &Layout, size: Size) -> Self {
        Self {
            layout: layout.clone(),
            size,
            fitted: layout.fit(size),
            rects: layout.rects(size),
            borders: layout.borders(size),
        }
    }
}

impl PaneCache {
    fn reset(&mut self, rect: Rect, source: Option<Source>) {
        self.rect = rect;
        self.source = source;
        self.rows.clear();
        self.images.forget_rows();
    }

    fn paint_rows(&mut self, grid: &mut Grid, screen: &vt100::Screen) {
        let Rect {
            row,
            col,
            rows,
            cols,
        } = self.rect;
        let mut visible_rows = screen.visible_rows();
        for offset in 0..rows {
            let visible = visible_rows.next();
            let key = visible.map(|visible| (visible.id, visible.stamp));
            let index = usize::from(offset);
            let unchanged = self.rows.get(index) == Some(&key)
                && !visible.is_some_and(|visible| visible.has_placeholders)
                && !self.images.covers(offset);
            if unchanged {
                continue;
            }
            let cells = visible.map_or(&[][..], |visible| visible.cells);
            grid.paint_row(
                row.saturating_add(offset),
                col,
                cols,
                cells,
                Cell::from_vt100,
            );
            match self.rows.get_mut(index) {
                Some(painted) => *painted = key,
                None => self.rows.push(key),
            }
        }
    }
}

fn blank(grid: &mut Grid, rect: Rect) {
    for offset in 0..rect.rows {
        grid.paint_row(
            rect.row.saturating_add(offset),
            rect.col,
            rect.cols,
            &[],
            Cell::from_vt100,
        );
    }
}

pub fn next_source() -> u64 {
    NEXT_SOURCE.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::server::graphics::place::tests::allocations;
    use crate::server::layout::SplitDirection;
    use crate::server::render::GridDiffer;

    const LEFT: PaneId = PaneId(0);
    const RIGHT: PaneId = PaneId(1);
    const WINDOW: Size = Size { rows: 6, cols: 21 };

    struct Window {
        layout: Layout,
        panes: BTreeMap<PaneId, vt100::Parser>,
        settings: Arc<Settings>,
        composer: Composer,
        differ: GridDiffer,
    }

    impl Window {
        fn side_by_side() -> Self {
            let mut layout = Layout::new(LEFT);
            layout
                .split(LEFT, RIGHT, SplitDirection::LeftRight, WINDOW)
                .unwrap();
            let panes = layout
                .rects(WINDOW)
                .into_iter()
                .map(|(pane, rect)| (pane, vt100::Parser::new(rect.rows, rect.cols, 10)))
                .collect();
            Self {
                layout,
                panes,
                settings: Arc::new(Settings::default()),
                composer: Composer::default(),
                differ: GridDiffer::new(WINDOW),
            }
        }

        fn feed(&mut self, pane: PaneId, output: &str) {
            self.panes
                .get_mut(&pane)
                .unwrap()
                .process(output.as_bytes());
        }

        fn painted_rows(&mut self) -> usize {
            let frame = self.composer.compose(
                &self.layout,
                WINDOW,
                LEFT,
                &self.panes,
                &self.settings,
                Viewer::text(),
            );
            frame.grid.painted_rows()
        }

        fn show(&mut self) -> Vec<u8> {
            let frame = self.composer.compose(
                &self.layout,
                WINDOW,
                LEFT,
                &self.panes,
                &self.settings,
                Viewer::text(),
            );
            self.differ.diff(frame)
        }
    }

    #[test]
    fn an_unchanged_window_paints_no_rows_and_a_keystroke_paints_one() {
        let mut window = Window::side_by_side();
        window.feed(LEFT, "$ ");
        window.feed(RIGHT, "$ ");
        assert_eq!(window.painted_rows(), usize::from(WINDOW.rows));
        assert_eq!(window.painted_rows(), 0);

        window.feed(LEFT, "l");
        assert_eq!(window.painted_rows(), 1);
        window.feed(LEFT, "\x1b[4;2H");
        assert_eq!(window.painted_rows(), 0);
        window.feed(RIGHT, "\r\nfile\r\n$ ");
        assert_eq!(window.painted_rows(), 2);
    }

    #[test]
    fn recomposing_one_changed_row_allocates_only_the_output() {
        let mut window = Window::side_by_side();
        window.feed(LEFT, "$ ");
        window.show();
        window.feed(LEFT, "l");
        window.show();
        window.feed(LEFT, "s");

        let before = allocations();
        let output = window.show();
        let spent = allocations() - before;
        assert!(!output.is_empty());
        let growth = output.capacity().ilog2() - 2;
        assert!(
            spent <= usize::try_from(growth).unwrap(),
            "{spent} allocations for {} bytes",
            output.len()
        );
    }
}
