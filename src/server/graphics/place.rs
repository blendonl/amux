use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::debug;

use super::command::Command;
use super::respond::{Code, Failure};
use super::store::{Buffer, ImageKey, Name, PaneImages, Stored};
use crate::protocol::CellPixels;
use crate::server::render::placeholder::MAX_IMAGE_CELLS;

const ASSUMED_CELL_PIXELS: CellPixels = CellPixels {
    width: 10,
    height: 20,
};
const MAX_PARENT_HOPS: usize = 8;

static NEXT_PLACEMENT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlacementId(pub u64);

impl PlacementId {
    fn next() -> Self {
        Self(NEXT_PLACEMENT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneSpan {
    pub key: ImageKey,
    pub placement: PlacementId,
    pub row: u16,
    pub col: u16,
    pub image_row: u16,
    pub image_col: u16,
    pub cols: u16,
    pub z: i32,
    pub under_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellOffset {
    pub x: u32,
    pub y: u32,
}

#[derive(Debug)]
enum Position {
    Rows {
        anchors: Vec<(u64, u16)>,
        pending_tail: Range<u16>,
    },
    Relative {
        parent: PlacementId,
        rows: i32,
        cols: i32,
    },
    Virtual,
}

#[derive(Debug)]
struct Placement {
    serial: PlacementId,
    key: ImageKey,
    image: u32,
    id: u32,
    col: u16,
    cols: u16,
    rows: u16,
    z: i32,
    #[cfg_attr(not(test), expect(dead_code))]
    source: SourceRect,
    #[cfg_attr(not(test), expect(dead_code))]
    offset: CellOffset,
    position: Position,
}

impl Placement {
    fn is_virtual(&self) -> bool {
        matches!(self.position, Position::Virtual)
    }

    fn is_anchored(&self) -> bool {
        matches!(self.position, Position::Rows { .. })
    }

    fn parent(&self) -> Option<PlacementId> {
        match self.position {
            Position::Relative { parent, .. } => Some(parent),
            Position::Rows { .. } | Position::Virtual => None,
        }
    }

    fn paints_before(&self, other: &Self) -> bool {
        (self.z, self.image, self.serial) <= (other.z, other.image, other.serial)
    }
}

pub struct Placements {
    images: PaneImages,
    main: Vec<Placement>,
    alt: Vec<Placement>,
}

impl Placements {
    pub fn new(images: PaneImages) -> Self {
        Self {
            images,
            main: Vec::new(),
            alt: Vec::new(),
        }
    }

    pub fn place(
        &mut self,
        screen: &mut vt100::Screen,
        image: Stored,
        command: &Command,
        cell_pixels: Option<CellPixels>,
    ) -> Result<(), Failure> {
        if command.unicode_placeholder && command.parent_id != 0 {
            return Err(Failure::new(
                Code::Einval,
                "a virtual placement can't be relative to another",
            ));
        }
        let buffer = active_buffer(screen);
        let id = if image.id == 0 { 0 } else { command.placement };
        let list = self.list(buffer);
        let replaced = list
            .iter()
            .position(|placement| id != 0 && placement.key == image.key && placement.id == id);
        let serial = replaced.map_or_else(PlacementId::next, |at| list[at].serial);
        let parent = match command.parent_id {
            0 => None,
            _ => Some(parent_of(list, command, serial)?),
        };
        let cell = cell_pixels.unwrap_or_else(assumed_cell_pixels);
        let source = source_rect(command, image);
        let offset = CellOffset {
            x: command
                .cell_x_offset
                .min(u32::from(cell.width).saturating_sub(1)),
            y: command
                .cell_y_offset
                .min(u32::from(cell.height).saturating_sub(1)),
        };
        let (cols, rows) = extent(command, source, offset, cell);
        if replaced.is_none() && !self.images.add_placement(image.key) {
            return Err(Failure::new(Code::Enoent, "the image is no longer stored"));
        }
        let (col, position) = match parent {
            Some(parent) => (
                0,
                Position::Relative {
                    parent,
                    rows: command.vertical_offset,
                    cols: command.horizontal_offset,
                },
            ),
            None if command.unicode_placeholder => (0, Position::Virtual),
            None => anchor(screen, cols, rows, command.cursor_movement != 1),
        };
        let placement = Placement {
            serial,
            key: image.key,
            image: image.id,
            id,
            col,
            cols,
            rows,
            z: command.z_index,
            source,
            offset,
            position,
        };
        let list = self.list_mut(buffer);
        if let Some(at) = replaced {
            list.remove(at);
        }
        let at = list.partition_point(|other| other.paints_before(&placement));
        list.insert(at, placement);
        Ok(())
    }

    pub fn forget_replaced(&mut self, buffer: Buffer, image: Stored) {
        if image.id == 0 {
            return;
        }
        let stale = select(self.list(buffer), |placement| {
            placement.image == image.id && placement.key != image.key
        });
        self.remove(buffer, stale, false);
    }

    pub fn delete(&mut self, screen: &vt100::Screen, command: &Command) {
        let buffer = active_buffer(screen);
        let list = self.list(buffer);
        let named =
            |placement: &Placement| command.placement == 0 || placement.id == command.placement;
        let selector = command.delete.to_ascii_lowercase();
        let selected = match selector {
            b'a' => select(list, |placement| !placement.is_virtual()),
            b'i' => select(list, |placement| {
                command.id != 0 && placement.image == command.id && named(placement)
            }),
            b'n' => {
                let key = match command.number {
                    0 => None,
                    number => self.images.lookup(buffer, Name::Number(number)),
                };
                select(list, |placement| {
                    Some(placement.key) == key && named(placement)
                })
            }
            b'r' => select(list, |placement| {
                placement.image != 0
                    && (command.source_x..=command.source_y).contains(&placement.image)
            }),
            b'z' => select(list, |placement| {
                !placement.is_virtual() && placement.z == command.z_index
            }),
            b'c' | b'p' | b'q' | b'x' | b'y' => match Target::of(selector, command, screen) {
                Some(target) => target.covered(list, screen),
                None => return,
            },
            _ => return,
        };
        self.remove(buffer, selected, command.delete.is_ascii_uppercase());
    }

    pub fn erase(&mut self, screen: &vt100::Screen, mode: u16) {
        if matches!(mode, 2 | 3) {
            self.clear(active_buffer(screen));
        }
    }

    pub fn reset(&mut self) {
        self.clear(Buffer::Main);
        self.clear(Buffer::Alt);
    }

    pub fn clear(&mut self, buffer: Buffer) {
        let visible = select(self.list(buffer), |placement| !placement.is_virtual());
        self.remove(buffer, visible, false);
        self.images.free_unplaced(buffer);
    }

    pub fn settle(&mut self, screen: &vt100::Screen) {
        let buffer = active_buffer(screen);
        let list = self.list_mut(buffer);
        if !list.iter().any(Placement::is_anchored) {
            return;
        }
        let rows = RowMap::new(screen);
        let mut dead = Vec::new();
        for placement in list.iter_mut() {
            let Position::Rows {
                anchors,
                pending_tail,
            } = &mut placement.position
            else {
                continue;
            };
            adopt(anchors, pending_tail, &rows, screen);
            if !anchors.iter().any(|&(id, _)| rows.row(id).is_some()) {
                dead.push(placement.serial);
            }
        }
        if !dead.is_empty() {
            self.remove(buffer, dead, false);
        }
    }

    #[cfg_attr(not(test), expect(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.main.is_empty() && self.alt.is_empty()
    }

    #[cfg_attr(not(test), expect(dead_code))]
    pub fn spans(&self, screen: &vt100::Screen) -> impl Iterator<Item = PaneSpan> {
        let list = self.list(active_buffer(screen));
        let mut spans = Vec::new();
        if list.iter().any(|placement| !placement.is_virtual()) {
            let layout = Layout::new(list, screen);
            let (_, screen_cols) = layout.size;
            for placement in list {
                layout.rows(placement, |row, image_row, col| {
                    spans.extend(span(placement, row, image_row, col, screen_cols));
                });
            }
        }
        spans.into_iter()
    }

    fn list(&self, buffer: Buffer) -> &[Placement] {
        match buffer {
            Buffer::Main => &self.main,
            Buffer::Alt => &self.alt,
        }
    }

    fn list_mut(&mut self, buffer: Buffer) -> &mut Vec<Placement> {
        match buffer {
            Buffer::Main => &mut self.main,
            Buffer::Alt => &mut self.alt,
        }
    }

    fn remove(&mut self, buffer: Buffer, mut doomed: Vec<PlacementId>, mut free: bool) {
        let Self { images, main, alt } = self;
        let list = match buffer {
            Buffer::Main => main,
            Buffer::Alt => alt,
        };
        while !doomed.is_empty() {
            let mut children = Vec::new();
            list.retain(|placement| {
                if doomed.contains(&placement.serial) {
                    images.remove_placement(placement.key, free);
                    return false;
                }
                if placement
                    .parent()
                    .is_some_and(|parent| doomed.contains(&parent))
                {
                    children.push(placement.serial);
                }
                true
            });
            doomed = children;
            free = true;
        }
    }
}

pub fn active_buffer(screen: &vt100::Screen) -> Buffer {
    if screen.alternate_screen() {
        Buffer::Alt
    } else {
        Buffer::Main
    }
}

fn assumed_cell_pixels() -> CellPixels {
    debug!(
        width = ASSUMED_CELL_PIXELS.width,
        height = ASSUMED_CELL_PIXELS.height,
        "sizing a kitty placement with an assumed cell size"
    );
    ASSUMED_CELL_PIXELS
}

fn select(list: &[Placement], chosen: impl Fn(&Placement) -> bool) -> Vec<PlacementId> {
    list.iter()
        .filter(|placement| chosen(placement))
        .map(|placement| placement.serial)
        .collect()
}

fn screen_cell(position: u32) -> Option<u16> {
    position
        .checked_sub(1)
        .and_then(|cell| u16::try_from(cell).ok())
}

fn parent_of(
    list: &[Placement],
    command: &Command,
    child: PlacementId,
) -> Result<PlacementId, Failure> {
    let missing = || Failure::new(Code::Enoparent, "the parent placement doesn't exist");
    let parent = list
        .iter()
        .filter(|placement| placement.image == command.parent_id)
        .filter(|placement| {
            command.parent_placement == 0 || placement.id == command.parent_placement
        })
        .min_by_key(|placement| placement.serial)
        .ok_or_else(missing)?;
    if parent.is_virtual() {
        return Err(Failure::new(
            Code::Einval,
            "placing relative to a virtual placement isn't supported",
        ));
    }
    if parent.serial == child {
        return Err(Failure::new(
            Code::Einval,
            "a placement can't be relative to itself",
        ));
    }
    let mut ancestor = parent;
    for _ in 1..MAX_PARENT_HOPS {
        let Some(next) = ancestor.parent() else {
            return Ok(parent.serial);
        };
        if next == child {
            return Err(Failure::new(
                Code::Ecycle,
                "the parent placement is relative to this one",
            ));
        }
        ancestor = list
            .iter()
            .find(|placement| placement.serial == next)
            .ok_or_else(missing)?;
    }
    match ancestor.parent() {
        None => Ok(parent.serial),
        Some(_) => Err(Failure::new(
            Code::Etoodeep,
            "too many levels of relative placements",
        )),
    }
}

fn source_rect(command: &Command, image: Stored) -> SourceRect {
    let x = command.source_x.min(image.width);
    let y = command.source_y.min(image.height);
    let width = match command.source_width {
        0 => image.width,
        width => width,
    };
    let height = match command.source_height {
        0 => image.height,
        height => height,
    };
    SourceRect {
        x,
        y,
        width: width.min(image.width - x),
        height: height.min(image.height - y),
    }
}

fn extent(
    command: &Command,
    source: SourceRect,
    offset: CellOffset,
    cell: CellPixels,
) -> (u16, u16) {
    let cell_width = u64::from(cell.width.max(1));
    let cell_height = u64::from(cell.height.max(1));
    let width = u64::from(source.width.max(1));
    let height = u64::from(source.height.max(1));
    let (x_offset, y_offset) = (u64::from(offset.x), u64::from(offset.y));
    let (cols, rows) = match (u64::from(command.columns), u64::from(command.rows)) {
        (0, 0) => (
            (width + x_offset).div_ceil(cell_width),
            (height + y_offset).div_ceil(cell_height),
        ),
        (0, rows) => {
            let height_px = cell_height.saturating_mul(rows).saturating_add(y_offset);
            let cols = height_px
                .saturating_mul(width)
                .div_ceil(height * cell_width);
            (cols, rows)
        }
        (cols, 0) => {
            let width_px = cell_width.saturating_mul(cols).saturating_add(x_offset);
            let rows = width_px
                .saturating_mul(height)
                .div_ceil(width * cell_height);
            (cols, rows)
        }
        given => given,
    };
    (cells(cols), cells(rows))
}

fn cells(count: u64) -> u16 {
    u16::try_from(count.clamp(1, u64::from(MAX_IMAGE_CELLS))).unwrap_or(MAX_IMAGE_CELLS)
}

fn anchor(screen: &mut vt100::Screen, cols: u16, rows: u16, move_cursor: bool) -> (u16, Position) {
    let (cursor_row, cursor_col) = screen.cursor_position();
    let (_, screen_cols) = screen.size();
    let col = cursor_col.min(screen_cols.saturating_sub(1));
    let anchors: Vec<(u64, u16)> = if move_cursor {
        let anchors = follow_cursor(screen, rows);
        let end = col.saturating_add(cols);
        if end >= screen_cols {
            screen.linefeed();
            screen.set_cursor_col(0);
        } else {
            screen.set_cursor_col(end);
        }
        anchors
    } else {
        screen
            .row_ids()
            .skip(usize::from(cursor_row))
            .zip(0..rows)
            .collect()
    };
    let anchored = u16::try_from(anchors.len()).unwrap_or(rows);
    let position = Position::Rows {
        anchors,
        pending_tail: anchored..rows,
    };
    (col, position)
}

fn follow_cursor(screen: &mut vt100::Screen, rows: u16) -> Vec<(u64, u16)> {
    let mut anchors: Vec<(u64, u16)> = Vec::with_capacity(usize::from(rows));
    for image_row in 0..rows {
        if image_row > 0 {
            screen.linefeed();
        }
        let (row, _) = screen.cursor_position();
        let Some(id) = screen.row_ids().nth(usize::from(row)) else {
            break;
        };
        if anchors.last().is_some_and(|&(last, _)| last == id) {
            break;
        }
        anchors.push((id, image_row));
    }
    anchors
}

fn adopt(
    anchors: &mut Vec<(u64, u16)>,
    pending_tail: &mut Range<u16>,
    rows: &RowMap,
    screen: &vt100::Screen,
) {
    while pending_tail.start < pending_tail.end {
        let Some(below) = anchors
            .last()
            .and_then(|&(id, _)| rows.row(id))
            .and_then(|row| screen.row_ids().nth(usize::from(row) + 1))
        else {
            return;
        };
        if anchors.iter().any(|&(id, _)| id == below) {
            return;
        }
        anchors.push((below, pending_tail.start));
        pending_tail.start += 1;
    }
}

fn span(
    placement: &Placement,
    row: u16,
    image_row: u16,
    col: i32,
    screen_cols: u16,
) -> Option<PaneSpan> {
    let image_col = u16::try_from(col.min(0).unsigned_abs()).ok()?;
    let start = u16::try_from(col.max(0)).ok()?;
    let cols = placement
        .cols
        .checked_sub(image_col)?
        .min(screen_cols.checked_sub(start)?);
    (cols > 0).then_some(PaneSpan {
        key: placement.key,
        placement: placement.serial,
        row,
        col: start,
        image_row,
        image_col,
        cols,
        z: placement.z,
        under_text: placement.z < 0,
    })
}

struct RowMap {
    sorted: Vec<(u64, u16)>,
}

impl RowMap {
    fn new(screen: &vt100::Screen) -> Self {
        let mut sorted: Vec<(u64, u16)> = screen.row_ids().zip(0..).collect();
        sorted.sort_unstable();
        Self { sorted }
    }

    fn row(&self, id: u64) -> Option<u16> {
        let at = self.sorted.binary_search_by_key(&id, |&(id, _)| id).ok()?;
        Some(self.sorted[at].1)
    }
}

struct Layout<'a> {
    list: &'a [Placement],
    rows: RowMap,
    size: (u16, u16),
}

impl<'a> Layout<'a> {
    fn new(list: &'a [Placement], screen: &vt100::Screen) -> Self {
        Self {
            list,
            rows: RowMap::new(screen),
            size: screen.size(),
        }
    }

    fn origin(&self, placement: &Placement) -> Option<(i32, i32)> {
        let (mut row, mut col) = (0_i32, 0_i32);
        let mut placement = placement;
        for _ in 0..=MAX_PARENT_HOPS {
            match &placement.position {
                Position::Rows { anchors, .. } => {
                    let (screen_row, image_row) = anchors
                        .iter()
                        .find_map(|&(id, image_row)| Some((self.rows.row(id)?, image_row)))?;
                    let top = i32::from(screen_row) - i32::from(image_row);
                    return Some((
                        row.saturating_add(top),
                        col.saturating_add(i32::from(placement.col)),
                    ));
                }
                Position::Relative { parent, rows, cols } => {
                    row = row.saturating_add(*rows);
                    col = col.saturating_add(*cols);
                    placement = self.list.iter().find(|other| other.serial == *parent)?;
                }
                Position::Virtual => return None,
            }
        }
        None
    }

    fn rows(&self, placement: &Placement, mut visit: impl FnMut(u16, u16, i32)) {
        match &placement.position {
            Position::Rows { anchors, .. } => {
                for &(id, image_row) in anchors {
                    if let Some(row) = self.rows.row(id) {
                        visit(row, image_row, i32::from(placement.col));
                    }
                }
            }
            Position::Relative { .. } => {
                let Some((top, col)) = self.origin(placement) else {
                    return;
                };
                let (screen_rows, _) = self.size;
                for image_row in 0..placement.rows {
                    let row = top.saturating_add(i32::from(image_row));
                    if let Some(row) = u16::try_from(row).ok().filter(|row| *row < screen_rows) {
                        visit(row, image_row, col);
                    }
                }
            }
            Position::Virtual => {}
        }
    }
}

struct Target {
    col: Option<u16>,
    row: Option<u16>,
    z: Option<i32>,
}

impl Target {
    fn of(selector: u8, command: &Command, screen: &vt100::Screen) -> Option<Self> {
        let col = || screen_cell(command.source_x);
        let row = || screen_cell(command.source_y);
        let target = match selector {
            b'c' => {
                let (row, col) = screen.cursor_position();
                let (_, cols) = screen.size();
                Self {
                    col: Some(col.min(cols.saturating_sub(1))),
                    row: Some(row),
                    z: None,
                }
            }
            b'p' => Self {
                col: Some(col()?),
                row: Some(row()?),
                z: None,
            },
            b'q' => Self {
                col: Some(col()?),
                row: Some(row()?),
                z: Some(command.z_index),
            },
            b'x' => Self {
                col: Some(col()?),
                row: None,
                z: None,
            },
            b'y' => Self {
                col: None,
                row: Some(row()?),
                z: None,
            },
            _ => return None,
        };
        Some(target)
    }

    fn covered(&self, list: &[Placement], screen: &vt100::Screen) -> Vec<PlacementId> {
        let layout = Layout::new(list, screen);
        select(list, |placement| {
            if placement.is_virtual() || self.z.is_some_and(|z| placement.z != z) {
                return false;
            }
            let mut hit = false;
            layout.rows(placement, |row, _, start| {
                let end = start.saturating_add(i32::from(placement.cols));
                hit |= self.row.is_none_or(|wanted| wanted == row)
                    && self
                        .col
                        .is_none_or(|wanted| (start..end).contains(&i32::from(wanted)));
            });
            hit
        })
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout as Allocation, System};
    use std::cell::Cell;
    use std::sync::{mpsc, Arc, Mutex};

    use super::super::store::ImageStore;
    use super::super::transmit::Image;
    use super::super::{lock, PaneGraphics};
    use super::*;
    use crate::protocol::ImageFormat;
    use crate::server::replies::PaneCallbacks;

    struct Counting;

    thread_local! {
        static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Allocation) -> *mut u8 {
            let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
            System.alloc(layout)
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Allocation) {
            System.dealloc(pointer, layout);
        }
    }

    #[global_allocator]
    static ALLOCATOR: Counting = Counting;

    fn allocations() -> usize {
        ALLOCATIONS.with(Cell::get)
    }

    struct Terminal {
        graphics: PaneGraphics,
        parser: Mutex<vt100::Parser<PaneCallbacks>>,
        store: Arc<ImageStore>,
        images: PaneImages,
        replies: mpsc::Receiver<Vec<u8>>,
    }

    impl Terminal {
        fn new(rows: u16, cols: u16) -> Self {
            let (input, replies) = mpsc::channel();
            let store = Arc::new(ImageStore::new(1 << 30));
            let images = store.open_pane();
            let callbacks = PaneCallbacks::new(input, Some(images.clone()));
            Self {
                graphics: PaneGraphics::new(images.clone()),
                parser: Mutex::new(vt100::Parser::new_with_callbacks(rows, cols, 0, callbacks)),
                store,
                images,
                replies,
            }
        }

        fn set_cell(&self, width: u16, height: u16) {
            lock(&self.parser)
                .callbacks()
                .set_cell_pixels(Some(CellPixels { width, height }));
        }

        fn output(&mut self, output: &str) -> Vec<String> {
            self.graphics.process(output.as_bytes(), &self.parser);
            self.replies
                .try_iter()
                .map(|reply| String::from_utf8(reply).unwrap())
                .collect()
        }

        fn send(&mut self, keys: &str) -> Vec<String> {
            self.output(&format!("\x1b_G{keys}\x1b\\"))
        }

        fn transmit(&mut self, keys: &str, width: u32, height: u32) -> Vec<String> {
            let pixels = "AAAA".repeat(usize::try_from(width * height).unwrap());
            self.send(&format!("f=24,s={width},v={height},{keys};{pixels}"))
        }

        fn image(&self, id: u32, width: u32, height: u32) -> Stored {
            self.numbered(id, 0, width, height)
        }

        fn numbered(&self, id: u32, number: u32, width: u32, height: u32) -> Stored {
            let buffer = active_buffer(lock(&self.parser).screen());
            let image = Image {
                width,
                height,
                format: ImageFormat::Png,
                compressed: false,
                bytes: vec![0; 8],
                decoded_len: 0,
            };
            self.images.insert(buffer, id, number, image).unwrap()
        }

        fn place(&self, image: Stored, keys: &str) -> Result<(), Failure> {
            let command = Command::parse(keys.as_bytes()).unwrap();
            let mut parser = lock(&self.parser);
            let (screen, callbacks) = parser.parts_mut();
            let cell_pixels = callbacks.cell_pixels();
            callbacks
                .placements_mut()
                .unwrap()
                .place(screen, image, &command, cell_pixels)
        }

        fn resize(&self, rows: u16, cols: u16) {
            let mut parser = lock(&self.parser);
            let (screen, callbacks) = parser.parts_mut();
            screen.set_size(rows, cols);
            callbacks.placements_mut().unwrap().settle(screen);
        }

        fn spans(&self) -> Vec<PaneSpan> {
            let parser = lock(&self.parser);
            let placements = parser.callbacks().placements().unwrap();
            placements.spans(parser.screen()).collect()
        }

        fn layout(&self) -> Vec<(u16, u16, u16, u16)> {
            self.spans()
                .iter()
                .map(|span| (span.row, span.col, span.image_row, span.cols))
                .collect()
        }

        fn rows(&self) -> Vec<(u16, u16)> {
            self.spans()
                .iter()
                .map(|span| (span.row, span.image_row))
                .collect()
        }

        fn with_list<R>(&self, buffer: Buffer, read: impl FnOnce(&[Placement]) -> R) -> R {
            let parser = lock(&self.parser);
            read(parser.callbacks().placements().unwrap().list(buffer))
        }

        fn count(&self, buffer: Buffer) -> usize {
            self.with_list(buffer, <[Placement]>::len)
        }

        fn ids(&self) -> Vec<(u32, u32)> {
            self.with_list(Buffer::Main, |list| {
                list.iter()
                    .map(|placement| (placement.image, placement.id))
                    .collect()
            })
        }

        fn extent(&self, image: Stored, keys: &str) -> (u16, u16) {
            self.place(image, &format!("a=p,i=1,C=1,{keys}")).unwrap();
            self.with_list(Buffer::Main, |list| {
                let newest = list
                    .iter()
                    .max_by_key(|placement| placement.serial)
                    .unwrap();
                (newest.cols, newest.rows)
            })
        }

        fn stored(&self, image: Stored) -> bool {
            self.store.get(image.key).is_some()
        }

        fn cursor(&self) -> (u16, u16) {
            lock(&self.parser).screen().cursor_position()
        }

        fn text(&self) -> String {
            lock(&self.parser).screen().contents()
        }
    }

    #[test]
    fn a_placement_moves_the_cursor_past_its_last_row() {
        let mut terminal = Terminal::new(10, 20);
        let image = terminal.image(1, 30, 50);
        terminal.output("\x1b[3;5H");
        terminal.place(image, "a=p,i=1").unwrap();
        assert_eq!(terminal.cursor(), (4, 7));
        assert_eq!(
            terminal.layout(),
            [(2, 4, 0, 3), (3, 4, 1, 3), (4, 4, 2, 3)]
        );
    }

    #[test]
    fn transmit_and_place_at_the_bottom_scrolls_the_text_up() {
        let mut terminal = Terminal::new(5, 20);
        terminal.output("top\x1b[4;1H");
        assert!(terminal.transmit("a=T,i=1,q=2", 20, 80).is_empty());
        assert_eq!(terminal.cursor(), (4, 2));
        assert_eq!(terminal.rows(), [(1, 0), (2, 1), (3, 2), (4, 3)]);
        assert!(!terminal.text().contains("top"));
    }

    #[test]
    fn a_placement_reaching_the_last_column_wraps_the_cursor() {
        let mut terminal = Terminal::new(5, 10);
        let image = terminal.image(1, 30, 40);
        terminal.output("\x1b[1;8H");
        terminal.place(image, "a=p,i=1").unwrap();
        assert_eq!(terminal.cursor(), (2, 0));

        terminal.output("\x1b[4;1H0123456789");
        assert_eq!(terminal.cursor(), (3, 10));
        let cell = terminal.image(2, 10, 20);
        terminal.place(cell, "a=p,i=2").unwrap();
        assert_eq!(terminal.cursor(), (4, 0));
        assert_eq!(terminal.layout().last(), Some(&(3, 9, 0, 1)));
    }

    #[test]
    fn c1_leaves_the_cursor_where_it_was() {
        let mut terminal = Terminal::new(5, 10);
        let image = terminal.image(1, 30, 40);
        terminal.output("\x1b[2;3H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        assert_eq!(terminal.cursor(), (1, 2));
        assert_eq!(terminal.layout(), [(1, 2, 0, 3), (2, 2, 1, 3)]);
    }

    #[test]
    fn the_size_comes_from_the_cell_count_or_from_pixels_over_the_cell_size() {
        let terminal = Terminal::new(10, 40);
        let image = terminal.image(1, 20, 33);
        assert_eq!(terminal.extent(image, "c=5,r=4"), (5, 4));
        assert_eq!(terminal.extent(image, ""), (2, 2));
        terminal.set_cell(8, 16);
        assert_eq!(terminal.extent(image, ""), (3, 3));
        assert_eq!(terminal.extent(image, "c=4"), (4, 4));
        assert_eq!(terminal.extent(image, "r=2"), (3, 2));
        assert_eq!(terminal.extent(image, "x=16,w=8,h=16"), (1, 1));
        assert_eq!(terminal.extent(image, "X=7,Y=15"), (4, 3));
        assert_eq!(terminal.extent(image, "X=100,Y=100"), (4, 3));
        assert_eq!(terminal.extent(image, "c=1000"), (297, 297));
    }

    #[test]
    fn the_source_rectangle_and_cell_offset_are_kept_for_later() {
        let terminal = Terminal::new(10, 40);
        terminal.set_cell(8, 16);
        let image = terminal.image(1, 20, 33);
        terminal
            .place(image, "a=p,i=1,x=4,y=40,w=100,h=5,X=3,Y=99")
            .unwrap();
        let (source, offset) =
            terminal.with_list(Buffer::Main, |list| (list[0].source, list[0].offset));
        assert_eq!(
            source,
            SourceRect {
                x: 4,
                y: 33,
                width: 16,
                height: 0
            }
        );
        assert_eq!(offset, CellOffset { x: 3, y: 15 });
    }

    #[test]
    fn scrolling_moves_the_image_up_and_drops_its_top_rows() {
        let mut terminal = Terminal::new(5, 10);
        let image = terminal.image(1, 10, 60);
        terminal.output("\x1b[2;1H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        assert_eq!(terminal.rows(), [(1, 0), (2, 1), (3, 2)]);
        terminal.output("\x1b[5;1H\n");
        assert_eq!(terminal.rows(), [(0, 0), (1, 1), (2, 2)]);
        terminal.output("\x1b[S");
        assert_eq!(terminal.rows(), [(0, 1), (1, 2)]);
        terminal.output("\n");
        assert_eq!(terminal.rows(), [(0, 2)]);
        assert_eq!(terminal.count(Buffer::Main), 1);
        terminal.output("\n");
        assert!(terminal.rows().is_empty());
        assert_eq!(terminal.count(Buffer::Main), 0);
    }

    #[test]
    fn a_scroll_region_moves_only_the_images_inside_it() {
        let mut terminal = Terminal::new(6, 10);
        let image = terminal.image(1, 10, 20);
        terminal.output("\x1b[3;1H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        terminal.output("\x1b[6;1H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        terminal.output("\x1b[2;4r\x1b[4;1H\n");
        assert_eq!(terminal.rows(), [(1, 0), (5, 0)]);
        terminal.output("\n");
        assert_eq!(terminal.rows(), [(5, 0)]);
        assert_eq!(terminal.count(Buffer::Main), 1);
    }

    #[test]
    fn inserted_and_deleted_lines_carry_the_image_rows_with_them() {
        let mut terminal = Terminal::new(8, 10);
        let image = terminal.image(1, 10, 80);
        terminal.output("\x1b[2;1H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        terminal.output("\x1b[3;1H\x1b[L");
        assert_eq!(terminal.rows(), [(1, 0), (3, 1), (4, 2), (5, 3)]);
        terminal.output("\x1b[4;1H\x1b[M");
        assert_eq!(terminal.rows(), [(1, 0), (3, 2), (4, 3)]);
    }

    #[test]
    fn shrinking_cuts_the_image_and_growing_does_not_bring_it_back() {
        let mut terminal = Terminal::new(6, 10);
        let image = terminal.image(1, 40, 60);
        terminal.output("\x1b[4;5H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        assert_eq!(
            terminal.layout(),
            [(3, 4, 0, 4), (4, 4, 1, 4), (5, 4, 2, 4)]
        );
        terminal.resize(4, 6);
        assert_eq!(terminal.layout(), [(3, 4, 0, 2)]);
        terminal.resize(8, 10);
        assert_eq!(terminal.layout(), [(3, 4, 0, 4)]);
        terminal.resize(2, 10);
        assert!(terminal.layout().is_empty());
        assert_eq!(terminal.count(Buffer::Main), 0);
    }

    #[test]
    fn a_c1_image_past_the_bottom_adopts_its_tail_as_the_screen_scrolls() {
        let mut terminal = Terminal::new(5, 10);
        let image = terminal.image(1, 10, 60);
        terminal.output("\x1b[5;1H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        assert_eq!(terminal.rows(), [(4, 0)]);
        terminal.output("\n");
        assert_eq!(terminal.rows(), [(3, 0), (4, 1)]);
        terminal.output("\n\n");
        assert_eq!(terminal.rows(), [(1, 0), (2, 1), (3, 2)]);
        terminal.output("\n");
        assert_eq!(terminal.rows(), [(0, 0), (1, 1), (2, 2)]);
    }

    #[test]
    fn growing_the_screen_uncovers_the_tail() {
        let mut terminal = Terminal::new(5, 10);
        let image = terminal.image(1, 10, 60);
        terminal.output("\x1b[5;1H");
        terminal.place(image, "a=p,i=1,C=1").unwrap();
        terminal.resize(7, 10);
        assert_eq!(terminal.rows(), [(4, 0), (5, 1), (6, 2)]);
    }

    #[test]
    fn a_pruned_placement_lets_go_of_its_image() {
        let mut terminal = Terminal::new(3, 10);
        let named = terminal.image(1, 10, 20);
        let anonymous = terminal.image(0, 10, 20);
        terminal.place(named, "a=p,i=1,C=1").unwrap();
        terminal.place(anonymous, "a=p,C=1").unwrap();
        terminal.output("\n\n");
        assert_eq!(terminal.count(Buffer::Main), 2);
        terminal.output("\n");
        assert_eq!(terminal.count(Buffer::Main), 0);
        assert!(terminal.stored(named));
        assert!(!terminal.stored(anonymous));
    }

    #[test]
    fn the_alternate_screen_hides_main_placements_and_1049_clears_its_own() {
        let mut terminal = Terminal::new(5, 10);
        let main = terminal.image(1, 10, 20);
        terminal.place(main, "a=p,i=1").unwrap();
        terminal.output("\x1b[?1049h");
        assert!(terminal.rows().is_empty());
        let alt = terminal.image(1, 10, 20);
        let shown = terminal.image(2, 10, 20);
        terminal.output("\x1b[3;1H");
        terminal.place(alt, "a=p,i=1").unwrap();
        terminal.place(shown, "a=p,i=2,U=1").unwrap();
        assert_eq!(terminal.rows(), [(2, 0)]);

        terminal.output("\x1b[?1049l");
        assert_eq!(terminal.rows(), [(0, 0)]);
        assert_eq!(terminal.count(Buffer::Alt), 2);
        terminal.output("\x1b[?1049h");
        assert!(terminal.rows().is_empty());
        assert_eq!(terminal.count(Buffer::Alt), 1);
        assert!(!terminal.stored(alt));
        assert!(terminal.stored(shown) && terminal.stored(main));
        terminal.output("\x1b[?1049l");
        assert_eq!(terminal.rows(), [(0, 0)]);
    }

    #[test]
    fn mode_47_switches_screens_without_clearing_them() {
        let mut terminal = Terminal::new(5, 10);
        terminal.output("\x1b[?47h");
        let alt = terminal.image(1, 10, 20);
        terminal.place(alt, "a=p,i=1").unwrap();
        terminal.output("\x1b[?47l");
        assert!(terminal.rows().is_empty());
        terminal.output("\x1b[?47h");
        assert_eq!(terminal.rows(), [(0, 0)]);
        assert!(terminal.stored(alt));
    }

    #[test]
    fn erasing_the_display_clears_the_placements_of_the_active_screen() {
        for erase in ["\x1b[2J", "\x1b[3J", "\x1b[?2J"] {
            let mut terminal = Terminal::new(5, 10);
            let placed = terminal.image(1, 10, 20);
            let shown = terminal.image(2, 10, 20);
            let loose = terminal.image(3, 10, 20);
            terminal.place(placed, "a=p,i=1").unwrap();
            terminal.place(shown, "a=p,i=2,U=1").unwrap();
            terminal.output("\x1b[J\x1b[1J\x1b[?1J");
            assert_eq!(terminal.rows(), [(0, 0)], "{erase:?}");
            terminal.output(erase);
            assert!(terminal.rows().is_empty(), "{erase:?}");
            assert_eq!(terminal.ids(), [(2, 0)], "{erase:?}");
            assert!(
                !terminal.stored(placed) && !terminal.stored(loose),
                "{erase:?}"
            );
            assert!(terminal.stored(shown), "{erase:?}");
        }
    }

    #[test]
    fn erasing_one_screen_leaves_the_other_alone() {
        let mut terminal = Terminal::new(5, 10);
        let main = terminal.image(1, 10, 20);
        terminal.place(main, "a=p,i=1").unwrap();
        terminal.output("\x1b[?47h");
        let alt = terminal.image(1, 10, 20);
        terminal.place(alt, "a=p,i=1").unwrap();
        terminal.output("\x1b[2J");
        assert_eq!(terminal.count(Buffer::Alt), 0);
        assert_eq!(terminal.count(Buffer::Main), 1);
        terminal.output("\x1b[?47l");
        assert_eq!(terminal.rows(), [(0, 0)]);
        assert!(terminal.stored(main));
    }

    #[test]
    fn a_reset_clears_both_screens_but_not_virtual_placements() {
        let mut terminal = Terminal::new(5, 10);
        let main = terminal.image(1, 10, 20);
        let shown = terminal.image(2, 10, 20);
        terminal.place(main, "a=p,i=1").unwrap();
        terminal.place(shown, "a=p,i=2,U=1").unwrap();
        terminal.output("\x1b[?47h");
        let alt = terminal.image(1, 10, 20);
        terminal.place(alt, "a=p,i=1").unwrap();
        terminal.output("\x1bc");
        assert!(terminal.rows().is_empty());
        assert_eq!(terminal.ids(), [(2, 0)]);
        assert_eq!(terminal.count(Buffer::Alt), 0);
        assert!(!terminal.stored(main) && !terminal.stored(alt));
        assert!(terminal.stored(shown));
    }

    #[test]
    fn deleting_everything_visible_keeps_virtual_placements() {
        let mut terminal = Terminal::new(5, 20);
        lock(&terminal.parser).callbacks().set_graphics(true);
        let image = terminal.image(1, 10, 20);
        let shown = terminal.image(2, 10, 20);
        let anonymous = terminal.image(0, 10, 20);
        terminal.place(image, "a=p,i=1").unwrap();
        terminal.place(shown, "a=p,i=2,U=1").unwrap();
        terminal.place(anonymous, "a=p").unwrap();
        assert_eq!(terminal.rows().len(), 2);
        assert!(terminal.send("a=d,i=1").is_empty());
        assert!(terminal.rows().is_empty());
        assert_eq!(terminal.ids(), [(2, 0)]);
        assert!(terminal.stored(image) && terminal.stored(shown));
        assert!(!terminal.stored(anonymous));

        terminal.place(image, "a=p,i=1").unwrap();
        assert!(terminal.send("a=d,d=A").is_empty());
        assert!(!terminal.stored(image));
        assert!(terminal.stored(shown));
        assert_eq!(terminal.ids(), [(2, 0)]);
    }

    #[test]
    fn deleting_by_id_takes_one_placement_or_every_one() {
        let mut terminal = Terminal::new(5, 20);
        let image = terminal.image(1, 10, 20);
        let other = terminal.image(2, 10, 20);
        for keys in ["p=1", "p=2", "p=3,U=1"] {
            terminal.place(image, &format!("a=p,i=1,{keys}")).unwrap();
        }
        terminal.place(other, "a=p,i=2").unwrap();
        terminal.send("a=d,d=i,i=1,p=1");
        assert_eq!(terminal.ids(), [(1, 2), (1, 3), (2, 0)]);
        terminal.send("a=d,d=i,i=1");
        assert_eq!(terminal.ids(), [(2, 0)]);
        assert!(terminal.stored(image));

        terminal.place(image, "a=p,i=1,p=1").unwrap();
        terminal.place(image, "a=p,i=1,p=2").unwrap();
        terminal.send("a=d,d=I,i=1,p=1");
        assert!(terminal.stored(image));
        terminal.send("a=d,d=I,i=1,p=2");
        assert!(!terminal.stored(image));
        assert_eq!(terminal.ids(), [(2, 0)]);
        terminal.send("a=d,d=I,i=2");
        assert!(!terminal.stored(other));
        assert!(terminal.ids().is_empty());
    }

    #[test]
    fn deleting_by_number_takes_the_newest_image_with_that_number() {
        let mut terminal = Terminal::new(5, 20);
        let older = terminal.numbered(0, 7, 10, 20);
        let newer = terminal.numbered(0, 7, 10, 20);
        terminal.place(older, "a=p,I=7,p=1").unwrap();
        terminal.place(newer, "a=p,I=7,p=1").unwrap();
        terminal.place(newer, "a=p,I=7,p=2,U=1").unwrap();
        terminal.send("a=d,d=n,I=7,p=2");
        assert_eq!(terminal.ids(), [(older.id, 1), (newer.id, 1)]);
        terminal.send("a=d,d=N,I=7");
        assert_eq!(terminal.ids(), [(older.id, 1)]);
        assert!(!terminal.stored(newer));
        assert!(terminal.stored(older));
    }

    #[test]
    fn deleting_an_id_range_takes_placements_and_frees_images_in_it() {
        let mut terminal = Terminal::new(5, 20);
        let images: Vec<Stored> = (1..=4).map(|id| terminal.image(id, 10, 20)).collect();
        terminal.place(images[1], "a=p,i=2,U=1").unwrap();
        terminal.place(images[3], "a=p,i=4").unwrap();
        terminal.send("a=d,d=r,x=2,y=3");
        assert_eq!(terminal.ids(), [(4, 0)]);
        assert!(images.iter().all(|image| terminal.stored(*image)));
        terminal.send("a=d,d=R,x=1,y=3");
        let stored: Vec<bool> = images.iter().map(|image| terminal.stored(*image)).collect();
        assert_eq!(stored, [false, false, false, true]);
    }

    fn left_after_deleting(keys: &str) -> Vec<u32> {
        let mut terminal = Terminal::new(6, 20);
        let image = terminal.image(1, 30, 40);
        for (at, keys) in [
            ("1;1", "p=1"),
            ("2;3", "p=2,z=5"),
            ("5;10", "p=3"),
            ("1;1", "p=4,U=1"),
        ] {
            terminal.output(&format!("\x1b[{at}H"));
            terminal
                .place(image, &format!("a=p,i=1,C=1,{keys}"))
                .unwrap();
        }
        terminal.output("\x1b[5;11H");
        assert!(terminal.send(&format!("a=d,{keys}")).is_empty());
        let mut ids: Vec<u32> = terminal.ids().into_iter().map(|(_, id)| id).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn deleting_by_position_takes_the_placements_covering_it() {
        assert_eq!(left_after_deleting("d=p,x=3,y=2"), [3, 4]);
        assert_eq!(left_after_deleting("d=P,x=3,y=2"), [3, 4]);
        assert_eq!(left_after_deleting("d=q,x=3,y=2,z=5"), [1, 3, 4]);
        assert_eq!(left_after_deleting("d=Q,x=3,y=2,z=1"), [1, 2, 3, 4]);
        assert_eq!(left_after_deleting("d=c"), [1, 2, 4]);
        assert_eq!(left_after_deleting("d=C"), [1, 2, 4]);
        assert_eq!(left_after_deleting("d=x,x=1"), [2, 3, 4]);
        assert_eq!(left_after_deleting("d=X,x=12"), [1, 2, 4]);
        assert_eq!(left_after_deleting("d=y,y=6"), [1, 2, 4]);
        assert_eq!(left_after_deleting("d=Y,y=2"), [3, 4]);
        assert_eq!(left_after_deleting("d=z,z=5"), [1, 3, 4]);
        assert_eq!(left_after_deleting("d=Z,z=0"), [2, 4]);
        assert_eq!(left_after_deleting("d=p,x=0,y=0"), [1, 2, 3, 4]);
        assert_eq!(left_after_deleting("d=f"), [1, 2, 3, 4]);
        assert_eq!(left_after_deleting("d=F"), [1, 2, 3, 4]);
        assert_eq!(left_after_deleting("d=r,x=1,y=1"), Vec::<u32>::new());
        assert_eq!(left_after_deleting("d=r,x=2,y=9"), [1, 2, 3, 4]);
    }

    #[test]
    fn placing_again_with_the_same_placement_id_moves_it_and_keeps_its_children() {
        let mut terminal = Terminal::new(6, 20);
        let image = terminal.image(1, 10, 20);
        let child = terminal.image(2, 10, 20);
        terminal.place(image, "a=p,i=1,p=1").unwrap();
        terminal.place(child, "a=p,i=2,P=1,Q=1,H=1").unwrap();
        terminal.output("\x1b[3;5H");
        terminal.place(image, "a=p,i=1,p=1").unwrap();
        assert_eq!(terminal.layout(), [(2, 4, 0, 1), (2, 5, 0, 1)]);
        terminal.place(image, "a=p,i=1").unwrap();
        terminal.place(image, "a=p,i=1,p=0").unwrap();
        assert_eq!(terminal.ids(), [(1, 1), (1, 0), (1, 0), (2, 0)]);

        terminal.send("a=d,d=I,i=1,p=1");
        assert!(terminal.stored(image));
        assert_eq!(terminal.ids(), [(1, 0), (1, 0)]);
        assert!(!terminal.stored(child));
    }

    #[test]
    fn a_relative_placement_follows_its_parent() {
        let mut terminal = Terminal::new(8, 20);
        let parent = terminal.image(1, 20, 40);
        let child = terminal.image(2, 10, 20);
        terminal.output("\x1b[3;4H");
        terminal.place(parent, "a=p,i=1,p=1,C=1").unwrap();
        terminal.place(child, "a=p,i=2,P=1,Q=1,H=3,V=-1").unwrap();
        assert_eq!(terminal.cursor(), (2, 3));
        assert_eq!(
            terminal.layout(),
            [(2, 3, 0, 2), (3, 3, 1, 2), (1, 6, 0, 1)]
        );
        terminal.output("\x1b[8;1H\n");
        assert_eq!(
            terminal.layout(),
            [(1, 3, 0, 2), (2, 3, 1, 2), (0, 6, 0, 1)]
        );
        terminal.output("\n");
        assert_eq!(terminal.layout(), [(0, 3, 0, 2), (1, 3, 1, 2)]);
        assert_eq!(terminal.count(Buffer::Main), 2);
        terminal.output("\n\n");
        assert!(terminal.layout().is_empty());
        assert_eq!(terminal.count(Buffer::Main), 0);
        assert!(!terminal.stored(child));
    }

    #[test]
    fn a_chain_of_relative_placements_resolves_through_every_parent() {
        let terminal = Terminal::new(10, 30);
        let image = terminal.image(1, 10, 20);
        terminal.place(image, "a=p,i=1,p=1,C=1").unwrap();
        for id in 2..=4 {
            terminal
                .place(image, &format!("a=p,i=1,p={id},P=1,Q={},H=2,V=1", id - 1))
                .unwrap();
        }
        assert_eq!(
            terminal.layout(),
            [(0, 0, 0, 1), (1, 2, 0, 1), (2, 4, 0, 1), (3, 6, 0, 1)]
        );
    }

    #[test]
    fn deleting_a_parent_deletes_its_children() {
        let mut terminal = Terminal::new(10, 30);
        let image = terminal.image(1, 10, 20);
        let other = terminal.image(2, 10, 20);
        terminal.place(image, "a=p,i=1,p=1,C=1").unwrap();
        terminal.place(image, "a=p,i=1,p=2,P=1,V=1").unwrap();
        terminal.place(image, "a=p,i=1,p=3,P=1,Q=2,V=1").unwrap();
        terminal.place(other, "a=p,i=2,p=1,P=1,Q=2,H=2").unwrap();
        terminal.place(image, "a=p,i=1,p=5,P=1,Q=1,H=5").unwrap();
        terminal.send("a=d,d=i,i=1,p=2");
        assert_eq!(terminal.ids(), [(1, 1), (1, 5)]);
        assert!(terminal.stored(image));
        assert!(!terminal.stored(other));
    }

    #[test]
    fn relative_placements_refuse_cycles_deep_chains_and_missing_parents() {
        let terminal = Terminal::new(10, 30);
        let image = terminal.image(1, 10, 20);
        terminal.place(image, "a=p,i=1,p=1,C=1").unwrap();
        for id in 2..=9 {
            terminal
                .place(image, &format!("a=p,i=1,p={id},P=1,Q={}", id - 1))
                .unwrap();
        }
        let code = |keys: &str| terminal.place(image, keys).unwrap_err().code;
        assert_eq!(code("a=p,i=1,p=10,P=1,Q=9"), Code::Etoodeep);
        assert_eq!(code("a=p,i=1,p=1,P=1,Q=3"), Code::Ecycle);
        assert_eq!(code("a=p,i=1,p=2,P=1,Q=2"), Code::Einval);
        assert_eq!(code("a=p,i=1,p=11,P=1,Q=99"), Code::Enoparent);
        assert_eq!(code("a=p,i=1,p=11,P=5"), Code::Enoparent);
        assert_eq!(code("a=p,i=1,p=11,P=1,U=1"), Code::Einval);
        terminal.place(image, "a=p,i=1,p=12,U=1").unwrap();
        assert_eq!(code("a=p,i=1,p=11,P=1,Q=12"), Code::Einval);
        assert_eq!(terminal.count(Buffer::Main), 10);
        assert_eq!(terminal.cursor(), (0, 0));
    }

    #[test]
    fn spans_come_in_paint_order_and_are_clipped_to_the_screen() {
        let mut terminal = Terminal::new(5, 10);
        let high = terminal.image(1, 30, 20);
        let low = terminal.image(2, 30, 20);
        terminal.output("\x1b[1;9H");
        terminal.place(high, "a=p,i=1,p=1,C=1,z=3").unwrap();
        terminal.place(low, "a=p,i=2,p=1,C=1,z=-2").unwrap();
        terminal.place(low, "a=p,i=2,p=2,P=1,Q=1,H=-9,V=1").unwrap();
        terminal.place(low, "a=p,i=2,p=3,P=1,Q=1,H=5").unwrap();
        let spans: Vec<_> = terminal
            .spans()
            .iter()
            .map(|span| {
                (
                    span.key,
                    span.row,
                    span.col,
                    span.image_col,
                    span.cols,
                    span.z,
                    span.under_text,
                )
            })
            .collect();
        assert_eq!(
            spans,
            [
                (low.key, 0, 8, 0, 2, -2, true),
                (low.key, 1, 0, 1, 2, 0, false),
                (high.key, 0, 8, 0, 2, 3, false),
            ]
        );
    }

    #[test]
    fn spans_allocate_nothing_without_placements_to_draw() {
        let terminal = Terminal::new(5, 10);
        let count_spans = |terminal: &Terminal| {
            let parser = lock(&terminal.parser);
            let placements = parser.callbacks().placements().unwrap();
            let before = allocations();
            let spans = placements.spans(parser.screen()).count();
            (spans, allocations() - before)
        };
        assert_eq!(count_spans(&terminal), (0, 0));
        let image = terminal.image(1, 10, 20);
        terminal.place(image, "a=p,i=1,U=1").unwrap();
        assert_eq!(count_spans(&terminal), (0, 0));
        terminal.place(image, "a=p,i=1").unwrap();
        assert_eq!(count_spans(&terminal).0, 1);
    }
}
