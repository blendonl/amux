use std::cmp::Reverse;
use std::fmt;

use anyhow::{anyhow, bail, Result};

use crate::protocol::Size;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(pub u32);

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

impl Rect {
    pub fn bottom(&self) -> u16 {
        self.row + self.rows
    }

    pub fn right(&self) -> u16 {
        self.col + self.cols
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        (self.row..self.bottom()).contains(&row) && (self.col..self.right()).contains(&col)
    }

    fn slice(&self, direction: SplitDirection, offset: u16, len: u16) -> Self {
        match direction {
            SplitDirection::LeftRight => Self {
                col: self.col + offset,
                cols: len,
                ..*self
            },
            SplitDirection::TopBottom => Self {
                row: self.row + offset,
                rows: len,
                ..*self
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    LeftRight,
    TopBottom,
}

impl SplitDirection {
    fn extent(self, rect: Rect) -> u16 {
        match self {
            Self::LeftRight => rect.cols,
            Self::TopBottom => rect.rows,
        }
    }

    fn border_line(self) -> BorderLine {
        match self {
            Self::LeftRight => BorderLine::Vertical,
            Self::TopBottom => BorderLine::Horizontal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderLine {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Border {
    pub rect: Rect,
    pub line: BorderLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    root: Option<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Pane(PaneId),
    Split {
        direction: SplitDirection,
        children: Vec<Node>,
    },
}

#[derive(Default)]
struct Placement {
    panes: Vec<(PaneId, Rect)>,
    borders: Vec<Border>,
}

impl Layout {
    pub fn new(pane: PaneId) -> Self {
        Self {
            root: Some(Node::Pane(pane)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.panes().contains(&pane)
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        if let Some(root) = &self.root {
            root.collect_panes(&mut panes);
        }
        panes
    }

    pub fn split(
        &mut self,
        target: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
        size: Size,
    ) -> Result<()> {
        if self.contains(new_pane) {
            bail!("pane {new_pane} is already in the layout");
        }
        let mut root = self
            .root
            .clone()
            .ok_or_else(|| anyhow!("can't find pane {target}"))?;
        if !root.split(target, new_pane, direction) {
            bail!("can't find pane {target}");
        }
        let needed = root.min_size();
        if needed.rows > size.rows || needed.cols > size.cols {
            bail!(
                "no space for a new pane: the layout needs {}x{} but the window is {}x{}",
                needed.cols,
                needed.rows,
                size.cols,
                size.rows
            );
        }
        self.root = Some(root);
        Ok(())
    }

    pub fn remove(&mut self, pane: PaneId) -> bool {
        match self.root.take() {
            Some(Node::Pane(only)) if only == pane => true,
            Some(mut root) => {
                let removed = root.remove(pane);
                self.root = Some(root);
                removed
            }
            None => false,
        }
    }

    pub fn min_size(&self) -> Size {
        self.root
            .as_ref()
            .map_or(Size { rows: 1, cols: 1 }, Node::min_size)
    }

    pub fn fit(&self, size: Size) -> Size {
        let needed = self.min_size();
        Size {
            rows: size.rows.max(needed.rows),
            cols: size.cols.max(needed.cols),
        }
    }

    pub fn rects(&self, size: Size) -> Vec<(PaneId, Rect)> {
        self.place(size).panes
    }

    pub fn borders(&self, size: Size) -> Vec<Border> {
        self.place(size).borders
    }

    pub fn neighbor(&self, pane: PaneId, side: Side, size: Size) -> Option<PaneId> {
        let rects = self.rects(size);
        let from = rects.iter().find(|(id, _)| *id == pane)?.1;
        rects
            .iter()
            .filter(|(id, _)| *id != pane)
            .filter_map(|&(id, rect)| {
                let overlap = edge_overlap(from, rect, side);
                (overlap > 0).then_some((overlap, id, rect))
            })
            .max_by_key(|&(overlap, _, rect)| (overlap, Reverse(rect.row), Reverse(rect.col)))
            .map(|(_, id, _)| id)
    }

    fn place(&self, size: Size) -> Placement {
        let size = self.fit(size);
        let window = Rect {
            row: 0,
            col: 0,
            rows: size.rows,
            cols: size.cols,
        };
        let mut placement = Placement::default();
        if let Some(root) = &self.root {
            root.place(window, &mut placement);
        }
        placement
    }
}

impl Node {
    fn collect_panes(&self, panes: &mut Vec<PaneId>) {
        match self {
            Self::Pane(id) => panes.push(*id),
            Self::Split { children, .. } => {
                for child in children {
                    child.collect_panes(panes);
                }
            }
        }
    }

    fn split(&mut self, target: PaneId, new_pane: PaneId, direction: SplitDirection) -> bool {
        match self {
            Self::Pane(id) if *id == target => {
                *self = Self::Split {
                    direction,
                    children: vec![Self::Pane(target), Self::Pane(new_pane)],
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split {
                direction: own,
                children,
            } => {
                let sibling = children
                    .iter()
                    .position(|child| *child == Self::Pane(target));
                match sibling {
                    Some(index) if *own == direction => {
                        children.insert(index + 1, Self::Pane(new_pane));
                        true
                    }
                    _ => children
                        .iter_mut()
                        .any(|child| child.split(target, new_pane, direction)),
                }
            }
        }
    }

    fn remove(&mut self, pane: PaneId) -> bool {
        let Self::Split { children, .. } = self else {
            return false;
        };
        let removed = match children.iter().position(|child| *child == Self::Pane(pane)) {
            Some(index) => {
                children.remove(index);
                true
            }
            None => children.iter_mut().any(|child| child.remove(pane)),
        };
        if removed && children.len() == 1 {
            *self = children.remove(0);
        }
        removed
    }

    fn min_size(&self) -> Size {
        match self {
            Self::Pane(_) => Size { rows: 1, cols: 1 },
            Self::Split {
                direction,
                children,
            } => {
                let count = u16::try_from(children.len()).unwrap_or(u16::MAX);
                let mins: Vec<Size> = children.iter().map(Self::min_size).collect();
                let rows = mins.iter().map(|size| size.rows).max().unwrap_or(1);
                let cols = mins.iter().map(|size| size.cols).max().unwrap_or(1);
                let stacked = |largest: u16| {
                    largest
                        .saturating_mul(count)
                        .saturating_add(count.saturating_sub(1))
                };
                match direction {
                    SplitDirection::LeftRight => Size {
                        rows,
                        cols: stacked(cols),
                    },
                    SplitDirection::TopBottom => Size {
                        rows: stacked(rows),
                        cols,
                    },
                }
            }
        }
    }

    fn place(&self, area: Rect, placement: &mut Placement) {
        match self {
            Self::Pane(id) => placement.panes.push((*id, area)),
            Self::Split {
                direction,
                children,
            } => {
                let count = u16::try_from(children.len()).unwrap_or(u16::MAX);
                let available = direction.extent(area) - (count - 1);
                let share = available / count;
                let extra = available % count;
                let mut offset = 0;
                for (index, child) in (0..count).zip(children) {
                    if index > 0 {
                        placement.borders.push(Border {
                            rect: area.slice(*direction, offset - 1, 1),
                            line: direction.border_line(),
                        });
                    }
                    let len = share + u16::from(index < extra);
                    child.place(area.slice(*direction, offset, len), placement);
                    offset += len + 1;
                }
            }
        }
    }
}

fn edge_overlap(from: Rect, to: Rect, side: Side) -> u16 {
    let adjacent = match side {
        Side::Left => from.col.checked_sub(1) == Some(to.right()),
        Side::Right => to.col.checked_sub(1) == Some(from.right()),
        Side::Up => from.row.checked_sub(1) == Some(to.bottom()),
        Side::Down => to.row.checked_sub(1) == Some(from.bottom()),
    };
    if !adjacent {
        return 0;
    }
    let (start, end) = match side {
        Side::Left | Side::Right => (from.row.max(to.row), from.bottom().min(to.bottom())),
        Side::Up | Side::Down => (from.col.max(to.col), from.right().min(to.right())),
    };
    end.saturating_sub(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PaneId = PaneId(0);
    const B: PaneId = PaneId(1);
    const C: PaneId = PaneId(2);
    const D: PaneId = PaneId(3);

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

    fn rect_of(layout: &Layout, pane: PaneId, window: Size) -> Rect {
        layout
            .rects(window)
            .into_iter()
            .find(|(id, _)| *id == pane)
            .map(|(_, rect)| rect)
            .expect("pane is in the layout")
    }

    fn grid_of_four(window: Size) -> Layout {
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(A, C, SplitDirection::TopBottom, window)
            .unwrap();
        layout
            .split(B, D, SplitDirection::TopBottom, window)
            .unwrap();
        layout
    }

    fn assert_tiles_exactly(layout: &Layout, window: Size) {
        let mut coverage = vec![0u8; usize::from(window.rows) * usize::from(window.cols)];
        let rects = layout
            .rects(window)
            .into_iter()
            .map(|(_, rect)| rect)
            .chain(layout.borders(window).into_iter().map(|border| border.rect));
        for rect in rects {
            assert!(rect.rows >= 1 && rect.cols >= 1, "{rect:?} is empty");
            assert!(rect.bottom() <= window.rows && rect.right() <= window.cols);
            for row in rect.row..rect.bottom() {
                for col in rect.col..rect.right() {
                    coverage[usize::from(row) * usize::from(window.cols) + usize::from(col)] += 1;
                }
            }
        }
        assert!(
            coverage.iter().all(|&count| count == 1),
            "{window:?} is not tiled exactly"
        );
    }

    #[test]
    fn a_new_layout_fills_the_window_with_one_pane() {
        let layout = Layout::new(A);
        assert_eq!(layout.panes(), vec![A]);
        assert_eq!(layout.rects(size(80, 24)), vec![(A, rect(0, 0, 24, 80))]);
        assert!(layout.borders(size(80, 24)).is_empty());
    }

    #[test]
    fn a_left_right_split_leaves_a_border_column() {
        let window = size(80, 24);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();

        assert_eq!(
            layout.rects(window),
            vec![(A, rect(0, 0, 24, 40)), (B, rect(0, 41, 24, 39))]
        );
        assert_eq!(
            layout.borders(window),
            vec![Border {
                rect: rect(0, 40, 24, 1),
                line: BorderLine::Vertical,
            }]
        );
    }

    #[test]
    fn a_top_bottom_split_leaves_a_border_row() {
        let window = size(80, 24);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::TopBottom, window)
            .unwrap();

        assert_eq!(
            layout.rects(window),
            vec![(A, rect(0, 0, 12, 80)), (B, rect(13, 0, 11, 80))]
        );
        assert_eq!(
            layout.borders(window),
            vec![Border {
                rect: rect(12, 0, 1, 80),
                line: BorderLine::Horizontal,
            }]
        );
    }

    #[test]
    fn splitting_the_same_way_as_the_parent_adds_an_equal_sibling() {
        let window = size(81, 24);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(A, C, SplitDirection::LeftRight, window)
            .unwrap();

        assert_eq!(layout.panes(), vec![A, C, B]);
        assert_eq!(
            layout.rects(window),
            vec![
                (A, rect(0, 0, 24, 27)),
                (C, rect(0, 28, 24, 26)),
                (B, rect(0, 55, 24, 26)),
            ]
        );
    }

    #[test]
    fn splitting_across_the_parent_nests_a_new_split() {
        let window = size(80, 25);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(B, C, SplitDirection::TopBottom, window)
            .unwrap();

        assert_eq!(
            layout.rects(window),
            vec![
                (A, rect(0, 0, 25, 40)),
                (B, rect(0, 41, 12, 39)),
                (C, rect(13, 41, 12, 39)),
            ]
        );
        assert_eq!(
            layout.borders(window),
            vec![
                Border {
                    rect: rect(0, 40, 25, 1),
                    line: BorderLine::Vertical,
                },
                Border {
                    rect: rect(12, 41, 1, 39),
                    line: BorderLine::Horizontal,
                },
            ]
        );
    }

    #[test]
    fn panes_and_borders_tile_every_window_size_exactly() {
        let mut layout = grid_of_four(size(7, 7));
        layout
            .split(D, PaneId(4), SplitDirection::LeftRight, size(7, 7))
            .unwrap();
        let needed = layout.min_size();
        for rows in needed.rows..40 {
            for cols in needed.cols..40 {
                assert_tiles_exactly(&layout, size(cols, rows));
            }
        }
    }

    #[test]
    fn a_split_that_cannot_fit_is_refused() {
        let window = size(3, 1);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        let before = layout.clone();

        assert!(layout
            .split(B, C, SplitDirection::LeftRight, window)
            .is_err());
        assert!(layout
            .split(B, C, SplitDirection::TopBottom, window)
            .is_err());
        assert_eq!(layout, before);
    }

    #[test]
    fn a_nested_split_accounts_for_its_siblings() {
        let window = size(5, 3);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(A, C, SplitDirection::TopBottom, window)
            .unwrap();
        layout
            .split(C, D, SplitDirection::LeftRight, window)
            .unwrap_err();

        assert_eq!(layout.min_size(), size(3, 3));
        assert_tiles_exactly(&layout, window);
    }

    #[test]
    fn splitting_needs_a_known_target_and_a_new_pane() {
        let window = size(80, 24);
        let mut layout = Layout::new(A);
        assert!(layout
            .split(B, C, SplitDirection::LeftRight, window)
            .is_err());
        assert!(layout
            .split(A, A, SplitDirection::LeftRight, window)
            .is_err());
        assert_eq!(layout.panes(), vec![A]);
    }

    #[test]
    fn removing_a_pane_collapses_its_split() {
        let window = size(80, 24);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(B, C, SplitDirection::TopBottom, window)
            .unwrap();

        assert!(layout.remove(C));
        let mut expected = Layout::new(A);
        expected
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        assert_eq!(layout, expected);

        assert!(layout.remove(A));
        assert_eq!(layout, Layout::new(B));
        assert_eq!(layout.rects(window), vec![(B, rect(0, 0, 24, 80))]);
    }

    #[test]
    fn removing_keeps_the_remaining_siblings_equal() {
        let window = size(80, 24);
        let mut layout = Layout::new(A);
        for pane in [B, C] {
            layout
                .split(A, pane, SplitDirection::LeftRight, window)
                .unwrap();
        }
        assert!(layout.remove(C));
        assert_eq!(
            layout.rects(window),
            vec![(A, rect(0, 0, 24, 40)), (B, rect(0, 41, 24, 39))]
        );
    }

    #[test]
    fn removing_the_last_pane_empties_the_layout() {
        let mut layout = Layout::new(A);
        assert!(!layout.remove(B));
        assert!(layout.remove(A));
        assert!(layout.is_empty());
        assert!(layout.panes().is_empty());
        assert!(layout.rects(size(80, 24)).is_empty());
        assert!(!layout.remove(A));
    }

    #[test]
    fn a_window_smaller_than_the_layout_is_laid_out_at_the_minimum() {
        let window = size(80, 24);
        let layout = grid_of_four(window);
        let tiny = size(2, 2);

        assert_eq!(layout.fit(tiny), size(3, 3));
        assert_eq!(rect_of(&layout, D, tiny), rect(2, 2, 1, 1));
        assert_tiles_exactly(&layout, layout.fit(tiny));
    }

    #[test]
    fn neighbours_are_found_on_every_side() {
        let window = size(80, 24);
        let layout = grid_of_four(window);

        assert_eq!(layout.neighbor(A, Side::Right, window), Some(B));
        assert_eq!(layout.neighbor(A, Side::Down, window), Some(C));
        assert_eq!(layout.neighbor(D, Side::Left, window), Some(C));
        assert_eq!(layout.neighbor(D, Side::Up, window), Some(B));
        assert_eq!(layout.neighbor(A, Side::Left, window), None);
        assert_eq!(layout.neighbor(A, Side::Up, window), None);
        assert_eq!(layout.neighbor(D, Side::Right, window), None);
        assert_eq!(layout.neighbor(D, Side::Down, window), None);
        assert_eq!(layout.neighbor(PaneId(9), Side::Down, window), None);
    }

    #[test]
    fn the_neighbour_with_the_most_edge_overlap_wins() {
        let window = size(80, 24);
        let mut layout = Layout::new(A);
        layout
            .split(A, B, SplitDirection::LeftRight, window)
            .unwrap();
        layout
            .split(B, C, SplitDirection::TopBottom, window)
            .unwrap();
        layout
            .split(C, D, SplitDirection::TopBottom, window)
            .unwrap();

        assert_eq!(rect_of(&layout, B, window), rect(0, 41, 8, 39));
        assert_eq!(rect_of(&layout, C, window), rect(9, 41, 7, 39));
        assert_eq!(layout.neighbor(A, Side::Right, window), Some(B));
        assert_eq!(layout.neighbor(C, Side::Left, window), Some(A));

        let mut uneven = Layout::new(A);
        uneven
            .split(A, B, SplitDirection::TopBottom, size(80, 5))
            .unwrap();
        uneven
            .split(A, C, SplitDirection::LeftRight, size(80, 5))
            .unwrap();
        uneven
            .split(C, D, SplitDirection::LeftRight, size(80, 5))
            .unwrap();
        let wide = size(10, 5);
        assert_eq!(rect_of(&uneven, A, wide), rect(0, 0, 2, 3));
        assert_eq!(rect_of(&uneven, C, wide), rect(0, 4, 2, 3));
        assert_eq!(rect_of(&uneven, D, wide), rect(0, 8, 2, 2));
        assert_eq!(uneven.neighbor(B, Side::Up, wide), Some(A));
    }

    #[test]
    fn rects_contain_their_own_cells_only() {
        let area = rect(2, 3, 4, 5);
        assert!(area.contains(2, 3));
        assert!(area.contains(5, 7));
        assert!(!area.contains(6, 3));
        assert!(!area.contains(2, 8));
        assert!(!area.contains(1, 3));
    }
}
