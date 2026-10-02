use std::mem;

use super::place::{active_buffer, Marked};
use super::sixel::{SixelDecoder, SixelImage, SixelLimits, SixelParams};
use super::sixel_palette::Palette;
use super::store::Buffer;
use super::transmit::Image;
use crate::protocol::{CellPixels, ImageFormat};
use crate::server::render::placeholder::MAX_IMAGE_CELLS;

const MAX_GEOMETRY: u32 = 4096;
const MIN_REGISTERS: usize = 2;
const DISPLAY_MODE: u16 = 80;
const PRIVATE_REGISTERS_MODE: u16 = 1070;
const CURSOR_RIGHT_MODE: u16 = 8452;
const REGISTERS_ITEM: u16 = 1;
const GEOMETRY_ITEM: u16 = 2;
const SUCCESS: u16 = 0;
const BAD_ITEM: u16 = 1;
const BAD_ACTION: u16 = 2;
const FAILURE: u16 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Modes {
    display: bool,
    private_registers: bool,
    cursor_right: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            display: false,
            private_registers: true,
            cursor_right: false,
        }
    }
}

#[derive(Debug)]
pub struct FinishedSixel {
    pub image: SixelImage,
    cell: CellPixels,
    marked: Marked,
}

impl FinishedSixel {
    pub fn into_padded(self) -> (Image, Marked) {
        let Self {
            image,
            cell,
            marked,
        } = self;
        let width = u32::from(marked.cols) * u32::from(cell.width);
        let height = u32::from(marked.rows) * u32::from(cell.height);
        let stride = width as usize * 4;
        let mut rgba = vec![0; stride * height as usize];
        let copied = image.width.min(width) as usize * 4;
        let rows = image.rgba.chunks_exact(image.width as usize * 4);
        for (padded, row) in rgba.chunks_exact_mut(stride).zip(rows) {
            padded[..copied].copy_from_slice(&row[..copied]);
        }
        let decoded_len = rgba.len();
        let padded = Image {
            width,
            height,
            format: ImageFormat::Rgba32,
            compressed: false,
            bytes: rgba,
            decoded_len,
        };
        (padded, marked)
    }
}

pub struct SixelDisplay {
    enabled: bool,
    modes: Modes,
    registers: usize,
    geometry: Option<(u32, u32)>,
    shared: Option<Palette>,
    decoder: Option<SixelDecoder>,
    finished: Vec<FinishedSixel>,
}

impl SixelDisplay {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            modes: Modes::default(),
            registers: Palette::MAX_REGISTERS,
            geometry: None,
            shared: None,
            decoder: None,
            finished: Vec::new(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn hook(
        &mut self,
        screen: &vt100::Screen,
        params: &[&[u16]],
        intermediates: &[u8],
        ignore: bool,
        action: char,
        cell: CellPixels,
    ) {
        if !self.enabled || action != 'q' || ignore || !intermediates.is_empty() {
            return;
        }
        let params: Vec<u16> = params
            .iter()
            .map(|param| param.first().copied().unwrap_or(0))
            .collect();
        let palette = if self.modes.private_registers {
            Palette::new(self.registers)
        } else {
            self.shared
                .take()
                .filter(|shared| shared.registers() == self.registers)
                .unwrap_or_else(|| Palette::new(self.registers))
        };
        let (max_width, max_height) = self.geometry(screen, cell);
        self.decoder = Some(SixelDecoder::new(
            SixelParams::from_dcs(&params),
            palette,
            SixelLimits {
                max_width,
                max_height,
            },
        ));
    }

    pub fn put(&mut self, byte: u8) {
        if let Some(decoder) = &mut self.decoder {
            decoder.put(byte);
        }
    }

    pub fn unhook(&mut self, screen: &mut vt100::Screen, cell: CellPixels) {
        let Some(decoder) = self.decoder.take() else {
            return;
        };
        let (image, palette) = decoder.finish();
        if !self.modes.private_registers {
            self.shared = Some(palette);
        }
        if let Some(image) = image {
            let marked = mark(screen, &image, cell, self.modes);
            self.finished.push(FinishedSixel {
                image,
                cell,
                marked,
            });
        }
    }

    pub fn take_finished(&mut self) -> Vec<FinishedSixel> {
        mem::take(&mut self.finished)
    }

    pub fn forget(&mut self, buffer: Buffer) {
        self.finished.retain(|sixel| sixel.marked.buffer != buffer);
    }

    pub fn set_modes(&mut self, params: &[&[u16]], on: bool) {
        if !self.enabled {
            return;
        }
        for param in params {
            match param {
                [DISPLAY_MODE] => self.modes.display = on,
                [PRIVATE_REGISTERS_MODE] => self.modes.private_registers = on,
                [CURSOR_RIGHT_MODE] => self.modes.cursor_right = on,
                _ => {}
            }
        }
    }

    pub fn report_mode(&self, mode: u16) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let set = match mode {
            DISPLAY_MODE => self.modes.display,
            PRIVATE_REGISTERS_MODE => self.modes.private_registers,
            CURSOR_RIGHT_MODE => self.modes.cursor_right,
            _ => return None,
        };
        Some(format!("\x1b[?{mode};{}$y", if set { 1 } else { 2 }))
    }

    pub fn graphics_attribute(
        &mut self,
        screen: &vt100::Screen,
        params: &[&[u16]],
        cell: CellPixels,
    ) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let value = |at: usize| {
            params
                .get(at)
                .map(|param| param.first().copied().unwrap_or(0))
        };
        let item = value(0).unwrap_or(0);
        let reply = |status: u16, values: &[u32]| {
            let values: Vec<String> = values.iter().map(u32::to_string).collect();
            format!("\x1b[?{item};{status};{}S", values.join(";"))
        };
        let registers = |count: usize| u32::try_from(count).unwrap_or(u32::MAX);
        let answer = match (item, value(1).unwrap_or(0)) {
            (REGISTERS_ITEM, 1) => reply(SUCCESS, &[registers(self.registers)]),
            (REGISTERS_ITEM, 2) => {
                self.registers = Palette::MAX_REGISTERS;
                reply(SUCCESS, &[registers(self.registers)])
            }
            (REGISTERS_ITEM, 3) => match value(2) {
                Some(count) => {
                    self.registers =
                        usize::from(count).clamp(MIN_REGISTERS, Palette::MAX_REGISTERS);
                    self.shared = None;
                    reply(SUCCESS, &[registers(self.registers)])
                }
                None => reply(FAILURE, &[0]),
            },
            (REGISTERS_ITEM, 4) => reply(SUCCESS, &[registers(Palette::MAX_REGISTERS)]),
            (GEOMETRY_ITEM, 1) => {
                let (width, height) = self.geometry(screen, cell);
                reply(SUCCESS, &[width, height])
            }
            (GEOMETRY_ITEM, 2) => {
                self.geometry = None;
                let (width, height) = self.geometry(screen, cell);
                reply(SUCCESS, &[width, height])
            }
            (GEOMETRY_ITEM, 3) => match (value(2), value(3)) {
                (Some(width), Some(height)) => {
                    let (width, height) = (pixels(width), pixels(height));
                    self.geometry = Some((width, height));
                    reply(SUCCESS, &[width, height])
                }
                _ => reply(FAILURE, &[0]),
            },
            (GEOMETRY_ITEM, 4) => reply(SUCCESS, &[MAX_GEOMETRY, MAX_GEOMETRY]),
            (REGISTERS_ITEM | GEOMETRY_ITEM, _) => reply(BAD_ACTION, &[0]),
            _ => reply(BAD_ITEM, &[0]),
        };
        Some(answer)
    }

    pub fn soft_reset(&mut self) {
        self.modes = Modes::default();
        self.shared = None;
    }

    pub fn reset(&mut self) {
        self.soft_reset();
        self.registers = Palette::MAX_REGISTERS;
        self.geometry = None;
        self.finished.clear();
    }

    fn geometry(&self, screen: &vt100::Screen, cell: CellPixels) -> (u32, u32) {
        self.geometry.unwrap_or_else(|| {
            let (rows, cols) = screen.size();
            (
                (u32::from(cols) * u32::from(cell.width)).clamp(1, MAX_GEOMETRY),
                (u32::from(rows) * u32::from(cell.height)).clamp(1, MAX_GEOMETRY),
            )
        })
    }
}

fn pixels(value: u16) -> u32 {
    u32::from(value).clamp(1, MAX_GEOMETRY)
}

fn cells(pixels: u32, cell: u16) -> u16 {
    let count = pixels.div_ceil(u32::from(cell.max(1)));
    u16::try_from(count)
        .unwrap_or(u16::MAX)
        .clamp(1, MAX_IMAGE_CELLS)
}

fn mark(screen: &mut vt100::Screen, image: &SixelImage, cell: CellPixels, modes: Modes) -> Marked {
    let buffer = active_buffer(screen);
    let cols = cells(image.width, cell.width);
    let rows = cells(image.height, cell.height);
    let (screen_rows, screen_cols) = screen.size();
    if modes.display {
        let cols = cols.min(screen_cols);
        let rows = rows.min(screen_rows);
        let anchors = screen.row_ids().zip(0..rows).collect();
        for row in 0..rows {
            screen.mark_graphic(row, 0..cols);
        }
        return Marked {
            buffer,
            anchors,
            col: 0,
            cols,
            rows,
        };
    }
    let (_, cursor_col) = screen.cursor_position();
    let col = cursor_col.min(screen_cols.saturating_sub(1));
    let cols = cols.min(screen_cols - col);
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
        screen.mark_graphic(row, col..col + cols);
        anchors.push((id, image_row));
    }
    let end = col + cols;
    if !modes.cursor_right {
        screen.set_cursor_col(col);
    } else if end >= screen_cols {
        screen.linefeed();
        screen.set_cursor_col(0);
    } else {
        screen.set_cursor_col(end);
    }
    let rows = u16::try_from(anchors.len()).unwrap_or(rows);
    Marked {
        buffer,
        anchors,
        col,
        cols,
        rows,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc, Mutex};

    use super::super::place::tests::allocations;
    use super::super::store::{ImageData, ImageKey, ImageStore};
    use super::super::{lock, PaneGraphics};
    use super::*;
    use crate::server::replies::PaneCallbacks;

    const HI: &str = include_str!("fixtures/hi.six");

    struct Terminal {
        graphics: PaneGraphics,
        parser: Mutex<vt100::Parser<PaneCallbacks>>,
        store: Arc<ImageStore>,
    }

    impl Terminal {
        fn new(rows: u16, cols: u16) -> Self {
            Self::with_sixel(rows, cols, true)
        }

        fn with_sixel(rows: u16, cols: u16, sixel: bool) -> Self {
            let store = Arc::new(ImageStore::new(1 << 30));
            let images = store.open_pane();
            let callbacks =
                PaneCallbacks::new(mpsc::channel().0, Some(images.clone())).with_sixel(sixel);
            Self {
                graphics: PaneGraphics::new(images),
                parser: Mutex::new(vt100::Parser::new_with_callbacks(rows, cols, 0, callbacks)),
                store,
            }
        }

        fn set_cell(&self, width: u16, height: u16) {
            lock(&self.parser)
                .callbacks()
                .set_cell_pixels(Some(CellPixels { width, height }));
        }

        fn output(&mut self, output: &str) {
            self.graphics.process(output.as_bytes(), &self.parser);
        }

        fn picture(&mut self, width: u32, height: u32) {
            self.output(&picture(width, height));
        }

        fn cursor(&self) -> (u16, u16) {
            lock(&self.parser).screen().cursor_position()
        }

        fn text(&self) -> String {
            lock(&self.parser).screen().contents()
        }

        fn layout(&self) -> Vec<(u16, u16, u16, u16)> {
            let parser = lock(&self.parser);
            let placements = parser.callbacks().placements().unwrap();
            placements
                .spans(parser.screen())
                .map(|span| (span.row, span.col, span.image_row, span.cols))
                .collect()
        }

        fn keys(&self) -> Vec<ImageKey> {
            let parser = lock(&self.parser);
            let placements = parser.callbacks().placements().unwrap();
            let mut keys: Vec<ImageKey> = placements
                .spans(parser.screen())
                .map(|span| span.key)
                .collect();
            keys.dedup();
            keys
        }

        fn image(&self) -> ImageData {
            let keys = self.keys();
            assert_eq!(keys.len(), 1, "{keys:?}");
            self.store.get(keys[0]).unwrap()
        }

        fn placed(&self) -> bool {
            !lock(&self.parser)
                .callbacks()
                .placements()
                .unwrap()
                .is_empty()
        }

        fn marks(&self) -> Vec<String> {
            let parser = lock(&self.parser);
            let screen = parser.screen();
            let (rows, cols) = screen.size();
            (0..rows)
                .map(|row| {
                    (0..cols)
                        .map(|col| {
                            let cell = screen.cell(row, col).unwrap();
                            if cell.is_graphic() {
                                '#'
                            } else {
                                cell.contents().chars().next().unwrap_or('.')
                            }
                        })
                        .collect()
                })
                .collect()
        }
    }

    fn picture(width: u32, height: u32) -> String {
        format!("\x1bPq\"1;1;{width};{height}#1~\x1b\\")
    }

    #[test]
    fn the_cursor_ends_on_the_last_image_row_at_the_start_column() {
        let mut terminal = Terminal::new(6, 10);
        terminal.set_cell(8, 16);
        terminal.output("\x1b[3;5H");
        terminal.picture(20, 40);
        assert_eq!(terminal.cursor(), (4, 4));
        assert_eq!(
            terminal.layout(),
            [(2, 4, 0, 3), (3, 4, 1, 3), (4, 4, 2, 3)]
        );
        terminal.output("ab");
        assert_eq!(
            terminal.marks(),
            [
                "..........",
                "..........",
                "....###...",
                "....###...",
                "....ab#...",
                "..........",
            ]
        );
    }

    #[test]
    fn an_image_past_the_bottom_scrolls_the_text_up() {
        let mut terminal = Terminal::new(5, 10);
        terminal.output("top\x1b[5;1H");
        terminal.picture(10, 60);
        assert_eq!(terminal.cursor(), (4, 0));
        assert_eq!(
            terminal.layout(),
            [(2, 0, 0, 1), (3, 0, 1, 1), (4, 0, 2, 1)]
        );
        assert!(!terminal.text().contains("top"));
    }

    #[test]
    fn an_image_taller_than_the_screen_keeps_its_last_rows_in_view() {
        let mut terminal = Terminal::new(4, 10);
        terminal.output("\x1b[?2;3;100;200S\x1b[2;1H");
        terminal.picture(10, 200);
        assert_eq!(terminal.cursor(), (3, 0));
        assert_eq!(
            terminal.layout(),
            [(0, 0, 6, 1), (1, 0, 7, 1), (2, 0, 8, 1), (3, 0, 9, 1)]
        );
        let image = terminal.image();
        assert_eq!((image.width, image.height), (10, 200));
    }

    #[test]
    fn the_default_geometry_crops_an_image_to_the_pane() {
        let mut terminal = Terminal::new(4, 10);
        terminal.output("\x1b[2;1H");
        terminal.picture(10, 200);
        assert_eq!(terminal.cursor(), (3, 0));
        assert_eq!(
            terminal.layout(),
            [(0, 0, 0, 1), (1, 0, 1, 1), (2, 0, 2, 1), (3, 0, 3, 1)]
        );
        let image = terminal.image();
        assert_eq!((image.width, image.height), (10, 80));
    }

    #[test]
    fn sixel_display_mode_draws_at_the_top_left_and_leaves_the_cursor() {
        let mut terminal = Terminal::new(4, 10);
        terminal.output("keep\x1b[?80h\x1b[?2;3;150;100S\x1b[3;5H");
        terminal.picture(150, 100);
        assert_eq!(terminal.cursor(), (2, 4));
        assert_eq!(
            terminal.layout(),
            [(0, 0, 0, 10), (1, 0, 1, 10), (2, 0, 2, 10), (3, 0, 3, 10)]
        );
        assert_eq!(terminal.marks(), ["##########"; 4]);
        assert!(terminal.text().starts_with("keep"));
        let image = terminal.image();
        assert_eq!((image.width, image.height), (100, 80));
    }

    #[test]
    fn mode_8452_leaves_the_cursor_right_of_the_image_and_wraps_at_the_edge() {
        let mut terminal = Terminal::new(5, 10);
        terminal.output("\x1b[?8452h\x1b[2;3H");
        terminal.picture(25, 30);
        assert_eq!(terminal.cursor(), (2, 5));
        terminal.output("\x1b[1;8H");
        terminal.picture(30, 20);
        assert_eq!(terminal.cursor(), (1, 0));
        terminal.output("\x1b[5;8H");
        terminal.picture(30, 20);
        assert_eq!(terminal.cursor(), (4, 0));
        assert_eq!(
            terminal.layout(),
            [(0, 2, 0, 3), (1, 2, 1, 3), (3, 7, 0, 3)]
        );
        terminal.output("\x1b[?8452l\x1b[2;3H");
        terminal.picture(25, 30);
        assert_eq!(terminal.cursor(), (2, 2));
    }

    #[test]
    fn printing_over_a_sixel_cuts_it_and_the_last_mark_ends_it() {
        let mut terminal = Terminal::new(3, 10);
        terminal.picture(30, 40);
        assert_eq!(terminal.cursor(), (1, 0));
        let key = terminal.keys()[0];
        terminal.output("ab");
        assert_eq!(terminal.marks(), ["###.......", "ab#.......", ".........."]);
        assert_eq!(terminal.layout(), [(0, 0, 0, 3), (1, 0, 1, 3)]);
        terminal.output("\x1b[2;3Hc\x1b[1;1Hdef");
        assert!(!terminal.placed());
        assert!(terminal.store.get(key).is_none());
    }

    #[test]
    fn erasing_the_display_removes_a_sixel_even_in_the_same_read() {
        let mut terminal = Terminal::new(3, 10);
        terminal.picture(10, 20);
        let key = terminal.keys()[0];
        terminal.output("\x1b[2J");
        assert!(!terminal.placed());
        assert!(terminal.store.get(key).is_none());

        terminal.output(&format!("{}\x1b[2J", picture(10, 20)));
        assert!(!terminal.placed());
        terminal.picture(10, 20);
        assert_eq!(terminal.keys(), [ImageKey(key.0 + 1)]);
    }

    #[test]
    fn a_newer_sixel_deletes_the_older_ones_it_covers() {
        let mut terminal = Terminal::new(5, 10);
        terminal.picture(20, 40);
        let covered = terminal.keys()[0];
        terminal.output("\x1b[H");
        terminal.picture(30, 60);
        assert_eq!(
            terminal.layout(),
            [(0, 0, 0, 3), (1, 0, 1, 3), (2, 0, 2, 3)]
        );
        assert!(terminal.store.get(covered).is_none());

        terminal.output("\x1b[H");
        terminal.picture(10, 20);
        assert_eq!(terminal.keys().len(), 2);
        terminal.output("\x1b[1;3H");
        terminal.picture(20, 40);
        assert_eq!(terminal.keys().len(), 3);
    }

    #[test]
    fn frames_drawn_over_each_other_keep_one_image() {
        let mut terminal = Terminal::new(5, 10);
        let frame = format!("\x1b[H{}", picture(40, 60));
        for _ in 0..5 {
            terminal.output(&frame);
        }
        let first = terminal.keys()[0];
        terminal.output(&frame.repeat(3));
        let keys = terminal.keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, first.0 + 3);
        assert!(terminal.store.get(first).is_none());
    }

    #[test]
    fn an_image_is_padded_with_clear_pixels_to_whole_cells() {
        const YELLOW: [u8; 4] = [255, 255, 0, 255];
        const CLEAR: [u8; 4] = [0; 4];
        let mut terminal = Terminal::new(5, 10);
        terminal.output(&format!("\x1bPq{HI}\x1b\\"));
        assert_eq!(terminal.layout(), [(0, 0, 0, 2)]);
        let image = terminal.image();
        assert_eq!(
            (image.width, image.height, image.format, image.compressed),
            (20, 20, ImageFormat::Rgba32, true)
        );
        assert_eq!(image.decoded_len, 20 * 20 * 4);
        let rgba = miniz_oxide::inflate::decompress_to_vec_zlib(&image.bytes).unwrap();
        let pixel = |x: usize, y: usize| -> [u8; 4] {
            let at = (y * 20 + x) * 4;
            rgba[at..at + 4].try_into().unwrap()
        };
        assert_eq!(pixel(0, 0), YELLOW);
        assert_eq!(pixel(13, 0), YELLOW);
        assert_eq!(pixel(0, 6), YELLOW);
        assert_eq!(pixel(14, 0), CLEAR);
        assert_eq!(pixel(0, 7), CLEAR);
        assert_eq!(pixel(19, 19), CLEAR);
    }

    #[test]
    fn kitty_deletes_leave_sixels_alone() {
        let mut terminal = Terminal::new(3, 10);
        terminal.picture(20, 20);
        for keys in [
            "a=d",
            "a=d,d=A",
            "a=d,d=z,z=0",
            "a=d,d=p,x=1,y=1",
            "a=d,d=c",
        ] {
            terminal.output(&format!("\x1b[H\x1b_G{keys}\x1b\\"));
            assert_eq!(terminal.layout(), [(0, 0, 0, 2)], "{keys}");
        }
    }

    #[test]
    fn a_sixel_finished_before_the_alternate_screen_stays_on_the_main_one() {
        let mut terminal = Terminal::new(3, 10);
        terminal.output(&format!("{}\x1b[?1049h", picture(10, 20)));
        assert!(terminal.layout().is_empty());
        terminal.output("\x1b[?1049l");
        assert_eq!(terminal.layout(), [(0, 0, 0, 1)]);
    }

    #[test]
    fn other_device_control_strings_and_panes_without_sixel_draw_nothing() {
        let mut terminal = Terminal::new(3, 10);
        terminal.output("\x1bP$qm\x1b\\\x1bP+q544e\x1b\\\x1bPp\x1b\\\x1bP1;1|17/ab\x1b\\");
        assert!(!terminal.placed());
        assert_eq!(terminal.marks(), [".........."; 3]);

        let mut off = Terminal::with_sixel(3, 10, false);
        off.picture(30, 40);
        assert_eq!(off.cursor(), (0, 0));
        assert_eq!(off.marks(), [".........."; 3]);
        assert!(!off.placed());
        assert!(off.store.get(ImageKey(1)).is_none());
    }

    #[test]
    fn ordinary_text_never_reaches_the_sixel_path() {
        let mut terminal = Terminal::new(5, 20);
        terminal.output("warm up\r");
        let before = allocations();
        terminal.output("\rhello \x1b[1mworld\x1b[m\x1b[2;3Hagain");
        assert_eq!(allocations() - before, 0);
        assert!(lock(&terminal.parser)
            .callbacks_mut()
            .take_sixels()
            .is_empty());
    }
}
