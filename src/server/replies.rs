use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;

use super::graphics::place::{active_buffer, Placements, ASSUMED_CELL_PIXELS};
use super::graphics::sixel_display::{FinishedSixel, SixelDisplay};
use super::graphics::store::{Buffer, PaneImages};
use crate::protocol::{CellPixels, RELEASE};

const DEVICE_ATTRIBUTES: &[u16] = &[62, 22];
const SIXEL_DEVICE_ATTRIBUTES: &[u16] = &[62, 4, 22];

pub struct PaneCallbacks {
    input: mpsc::Sender<Vec<u8>>,
    cell_pixels: AtomicU32,
    graphics: AtomicBool,
    placements: Option<Placements>,
    sixel: SixelDisplay,
}

impl PaneCallbacks {
    pub fn new(input: mpsc::Sender<Vec<u8>>, images: Option<PaneImages>) -> Self {
        Self {
            input,
            cell_pixels: AtomicU32::new(0),
            graphics: AtomicBool::new(false),
            sixel: SixelDisplay::new(images.is_some()),
            placements: images.map(Placements::new),
        }
    }

    pub fn with_sixel(mut self, sixel: bool) -> Self {
        self.sixel = SixelDisplay::new(sixel && self.placements.is_some());
        self
    }

    pub fn take_sixels(&mut self) -> Vec<FinishedSixel> {
        self.sixel.take_finished()
    }

    pub fn placements(&self) -> Option<&Placements> {
        self.placements.as_ref()
    }

    pub fn placements_mut(&mut self) -> Option<&mut Placements> {
        self.placements.as_mut()
    }

    pub fn cell_pixels(&self) -> Option<CellPixels> {
        unpack_cell_pixels(self.cell_pixels.load(Ordering::Relaxed))
    }

    pub fn set_cell_pixels(&self, pixels: Option<CellPixels>) -> bool {
        let packed = pack_cell_pixels(pixels);
        self.cell_pixels.swap(packed, Ordering::Relaxed) != packed
    }

    pub fn graphics(&self) -> bool {
        self.graphics.load(Ordering::Relaxed)
    }

    pub fn set_graphics(&self, graphics: bool) {
        self.graphics.store(graphics, Ordering::Relaxed);
    }

    pub fn reply(&self, bytes: Vec<u8>) {
        let _ = self.input.send(bytes);
    }

    fn sixel_cell(&self) -> CellPixels {
        self.cell_pixels().unwrap_or(ASSUMED_CELL_PIXELS)
    }

    fn answer(
        &self,
        screen: &vt100::Screen,
        private: Option<u8>,
        param: u16,
        action: char,
    ) -> Option<String> {
        match (private, action, param) {
            (None, 'c', 0) if self.sixel.is_enabled() => {
                Some(primary_device_attributes(SIXEL_DEVICE_ATTRIBUTES))
            }
            (None, 'c', 0) => Some(primary_device_attributes(DEVICE_ATTRIBUTES)),
            (None, 'n', 5) => Some("\x1b[0n".to_owned()),
            (None, 'n', 6) => {
                let (row, col) = cursor_report(screen);
                Some(format!("\x1b[{row};{col}R"))
            }
            (Some(b'?'), 'n', 6) => {
                let (row, col) = cursor_report(screen);
                Some(format!("\x1b[?{row};{col};1R"))
            }
            (Some(b'>'), 'q', 0) => Some(format!("\x1bP>|amux {RELEASE}\x1b\\")),
            (None, 't', 18) => {
                let (rows, cols) = screen.size();
                Some(format!("\x1b[8;{rows};{cols}t"))
            }
            (None, 't', 14) => self.cell_pixels().map(|pixels| {
                let (rows, cols) = screen.size();
                let height = u32::from(rows) * u32::from(pixels.height);
                let width = u32::from(cols) * u32::from(pixels.width);
                format!("\x1b[4;{height};{width}t")
            }),
            (None, 't', 16) => self
                .cell_pixels()
                .map(|pixels| format!("\x1b[6;{};{}t", pixels.height, pixels.width)),
            _ => None,
        }
    }
}

impl vt100::Callbacks for PaneCallbacks {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let param = params
            .first()
            .and_then(|param| param.first())
            .copied()
            .unwrap_or(0);
        let reply = match (i1, i2, c) {
            (Some(b'?'), None, 'h' | 'l') => {
                self.sixel.set_modes(params, c == 'h');
                None
            }
            (Some(b'!'), None, 'p') => {
                self.sixel.soft_reset();
                None
            }
            (Some(b'?'), Some(b'$'), 'p') => self.sixel.report_mode(param),
            (Some(b'?'), None, 'S') => {
                let cell = self.sixel_cell();
                self.sixel.graphics_attribute(screen, params, cell)
            }
            (_, Some(_), _) => None,
            (private, None, action) => self.answer(screen, private, param, action),
        };
        if let Some(reply) = reply {
            self.reply(reply.into_bytes());
        }
    }

    fn erase_in_display(&mut self, screen: &mut vt100::Screen, mode: u16) {
        if matches!(mode, 2 | 3) {
            self.sixel.forget(active_buffer(screen));
        }
        if let Some(placements) = &mut self.placements {
            placements.erase(screen, mode);
        }
    }

    fn reset(&mut self, _: &mut vt100::Screen) {
        self.sixel.reset();
        if let Some(placements) = &mut self.placements {
            placements.reset();
        }
    }

    fn alternate_screen(&mut self, _: &mut vt100::Screen, _entered: bool, cleared: bool) {
        if !cleared {
            return;
        }
        self.sixel.forget(Buffer::Alt);
        if let Some(placements) = &mut self.placements {
            placements.clear(Buffer::Alt);
        }
    }

    fn dcs_hook(
        &mut self,
        screen: &mut vt100::Screen,
        params: &[&[u16]],
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        let cell = self.sixel_cell();
        self.sixel
            .hook(screen, params, intermediates, ignore, action, cell);
    }

    fn dcs_put(&mut self, _: &mut vt100::Screen, byte: u8) {
        self.sixel.put(byte);
    }

    fn dcs_unhook(&mut self, screen: &mut vt100::Screen) {
        let cell = self.sixel_cell();
        self.sixel.unhook(screen, cell);
    }
}

fn pack_cell_pixels(pixels: Option<CellPixels>) -> u32 {
    pixels
        .filter(|pixels| pixels.width != 0 && pixels.height != 0)
        .map_or(0, |pixels| {
            (u32::from(pixels.width) << 16) | u32::from(pixels.height)
        })
}

fn unpack_cell_pixels(packed: u32) -> Option<CellPixels> {
    let [width_high, width_low, height_high, height_low] = packed.to_be_bytes();
    let pixels = CellPixels {
        width: u16::from_be_bytes([width_high, width_low]),
        height: u16::from_be_bytes([height_high, height_low]),
    };
    (pixels.width != 0 && pixels.height != 0).then_some(pixels)
}

fn primary_device_attributes(attributes: &[u16]) -> String {
    let attributes: Vec<String> = attributes.iter().map(u16::to_string).collect();
    format!("\x1b[?{}c", attributes.join(";"))
}

fn cursor_report(screen: &vt100::Screen) -> (u16, u16) {
    let (row, col) = screen.cursor_position();
    let (_, cols) = screen.size();
    (row.saturating_add(1), col.saturating_add(1).min(cols))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::super::graphics::store::ImageStore;
    use super::*;

    const SIZE: (u16, u16) = (24, 80);

    struct Terminal {
        parser: vt100::Parser<PaneCallbacks>,
        replies: mpsc::Receiver<Vec<u8>>,
    }

    impl Terminal {
        fn new() -> Self {
            Self::sized(SIZE.0, SIZE.1)
        }

        fn sized(rows: u16, cols: u16) -> Self {
            Self::with_callbacks(rows, cols, |input| PaneCallbacks::new(input, None))
        }

        fn with_images(rows: u16, cols: u16, sixel: bool) -> Self {
            let images = Arc::new(ImageStore::new(1 << 20)).open_pane();
            Self::with_callbacks(rows, cols, |input| {
                PaneCallbacks::new(input, Some(images)).with_sixel(sixel)
            })
        }

        fn sixel() -> Self {
            Self::with_images(SIZE.0, SIZE.1, true)
        }

        fn with_callbacks(
            rows: u16,
            cols: u16,
            callbacks: impl FnOnce(mpsc::Sender<Vec<u8>>) -> PaneCallbacks,
        ) -> Self {
            let (input, replies) = mpsc::channel();
            Self {
                parser: vt100::Parser::new_with_callbacks(rows, cols, 0, callbacks(input)),
                replies,
            }
        }

        fn modes(&mut self) -> String {
            self.reply_to("\x1b[?80$p\x1b[?1070$p\x1b[?8452$p")
        }

        fn sixel_colours(&mut self, output: &str) -> Vec<[u8; 4]> {
            self.parser.process(output.as_bytes());
            self.parser
                .callbacks_mut()
                .take_sixels()
                .into_iter()
                .map(|sixel| sixel.image.rgba[..4].try_into().unwrap())
                .collect()
        }

        fn replies_to(&mut self, output: &str) -> Vec<String> {
            self.parser.process(output.as_bytes());
            self.replies
                .try_iter()
                .map(|reply| String::from_utf8(reply).unwrap())
                .collect()
        }

        fn reply_to(&mut self, output: &str) -> String {
            self.replies_to(output).concat()
        }

        fn set_cell_pixels(&self, width: u16, height: u16) {
            self.parser
                .callbacks()
                .set_cell_pixels(Some(CellPixels { width, height }));
        }
    }

    #[test]
    fn primary_device_attributes_name_a_vt220_with_ansi_colour() {
        let mut terminal = Terminal::new();
        assert_eq!(terminal.reply_to("\x1b[c"), "\x1b[?62;22c");
        assert_eq!(terminal.reply_to("\x1b[0c"), "\x1b[?62;22c");
    }

    #[test]
    fn primary_device_attributes_add_sixel_while_the_pane_takes_it() {
        assert_eq!(Terminal::sixel().reply_to("\x1b[c"), "\x1b[?62;4;22c");
        let mut without_sixel = Terminal::with_images(SIZE.0, SIZE.1, false);
        assert_eq!(without_sixel.reply_to("\x1b[c"), "\x1b[?62;22c");
        let mut without_images = Terminal::with_callbacks(SIZE.0, SIZE.1, |input| {
            PaneCallbacks::new(input, None).with_sixel(true)
        });
        assert_eq!(without_images.reply_to("\x1b[c"), "\x1b[?62;22c");
    }

    #[test]
    fn graphics_attributes_report_and_set_the_colour_registers() {
        let mut terminal = Terminal::sixel();
        let registers = |count: u32| format!("\x1b[?1;0;{count}S");
        assert_eq!(terminal.reply_to("\x1b[?1;1S"), registers(1024));
        assert_eq!(terminal.reply_to("\x1b[?1;4S"), registers(1024));
        assert_eq!(terminal.reply_to("\x1b[?1;3;256S"), registers(256));
        assert_eq!(terminal.reply_to("\x1b[?1;1S"), registers(256));
        assert_eq!(terminal.reply_to("\x1b[?1;4S"), registers(1024));
        assert_eq!(terminal.reply_to("\x1b[?1;3;1S"), registers(2));
        assert_eq!(terminal.reply_to("\x1b[?1;3;5000S"), registers(1024));
        assert_eq!(terminal.reply_to("\x1b[?1;3;16S"), registers(16));
        assert_eq!(terminal.reply_to("\x1b[?1;3S"), "\x1b[?1;3;0S");
        assert_eq!(terminal.reply_to("\x1b[?1;1S"), registers(16));
        assert_eq!(terminal.reply_to("\x1b[?1;2S"), registers(1024));
        assert_eq!(terminal.reply_to("\x1b[?1;1S"), registers(1024));
    }

    #[test]
    fn graphics_attributes_report_and_set_the_sixel_geometry() {
        let mut terminal = Terminal::sixel();
        let geometry = |width: u32, height: u32| format!("\x1b[?2;0;{width};{height}S");
        assert_eq!(terminal.reply_to("\x1b[?2;1S"), geometry(800, 480));
        terminal.set_cell_pixels(9, 18);
        assert_eq!(terminal.reply_to("\x1b[?2;1S"), geometry(720, 432));
        assert_eq!(terminal.reply_to("\x1b[?2;4S"), geometry(4096, 4096));
        assert_eq!(
            terminal.reply_to("\x1b[?2;3;640;9999S"),
            geometry(640, 4096)
        );
        assert_eq!(terminal.reply_to("\x1b[?2;1S"), geometry(640, 4096));
        assert_eq!(terminal.reply_to("\x1b[?2;3;0;7S"), geometry(1, 7));
        assert_eq!(terminal.reply_to("\x1b[?2;3;640S"), "\x1b[?2;3;0S");
        assert_eq!(terminal.reply_to("\x1b[?2;2S"), geometry(720, 432));
        assert_eq!(terminal.reply_to("\x1b[?2;1S"), geometry(720, 432));

        let mut huge = Terminal::with_images(300, 600, true);
        assert_eq!(huge.reply_to("\x1b[?2;1S"), geometry(4096, 4096));
    }

    #[test]
    fn graphics_attributes_name_a_bad_item_or_action() {
        let mut terminal = Terminal::sixel();
        assert_eq!(terminal.reply_to("\x1b[?3;1S"), "\x1b[?3;1;0S");
        assert_eq!(terminal.reply_to("\x1b[?7;4S"), "\x1b[?7;1;0S");
        assert_eq!(terminal.reply_to("\x1b[?S"), "\x1b[?0;1;0S");
        assert_eq!(terminal.reply_to("\x1b[?1;5S"), "\x1b[?1;2;0S");
        assert_eq!(terminal.reply_to("\x1b[?2;0S"), "\x1b[?2;2;0S");
        assert_eq!(terminal.reply_to("\x1b[?2S"), "\x1b[?2;2;0S");

        let mut without_sixel = Terminal::with_images(SIZE.0, SIZE.1, false);
        assert_eq!(without_sixel.reply_to("\x1b[?1;1S\x1b[?2;1S\x1b[?3;1S"), "");
    }

    #[test]
    fn mode_requests_report_the_sixel_modes() {
        let mut terminal = Terminal::sixel();
        assert_eq!(terminal.modes(), "\x1b[?80;2$y\x1b[?1070;1$y\x1b[?8452;2$y");
        terminal.reply_to("\x1b[?80;8452h\x1b[?1070l");
        assert_eq!(terminal.modes(), "\x1b[?80;1$y\x1b[?1070;2$y\x1b[?8452;1$y");
        terminal.reply_to("\x1b[?80;80h\x1b[?2004;80l\x1b[?8452;1049;1070h");
        assert_eq!(terminal.modes(), "\x1b[?80;2$y\x1b[?1070;1$y\x1b[?8452;1$y");
        assert!(!terminal.parser.screen().bracketed_paste());
        assert!(terminal.parser.screen().alternate_screen());
        assert_eq!(terminal.reply_to("\x1b[?81$p\x1b[80$p\x1b[?$p"), "");

        let mut without_sixel = Terminal::with_images(SIZE.0, SIZE.1, false);
        without_sixel.reply_to("\x1b[?80h");
        assert_eq!(without_sixel.modes(), "");
    }

    #[test]
    fn resets_restore_the_sixel_modes_and_a_full_reset_the_attributes_too() {
        let mut terminal = Terminal::sixel();
        let defaults = "\x1b[?80;2$y\x1b[?1070;1$y\x1b[?8452;2$y";
        let changed = "\x1b[?80h\x1b[?1070l\x1b[?8452h\x1b[?1;3;16S\x1b[?2;3;64;64S";
        terminal.reply_to(changed);
        terminal.reply_to("\x1b[!p");
        assert_eq!(terminal.modes(), defaults);
        assert_eq!(terminal.reply_to("\x1b[?1;1S"), "\x1b[?1;0;16S");
        assert_eq!(terminal.reply_to("\x1b[?2;1S"), "\x1b[?2;0;64;64S");

        terminal.reply_to(changed);
        terminal.reply_to("\x1bc");
        assert_eq!(terminal.modes(), defaults);
        assert_eq!(terminal.reply_to("\x1b[?1;1S"), "\x1b[?1;0;1024S");
        assert_eq!(terminal.reply_to("\x1b[?2;1S"), "\x1b[?2;0;800;480S");
    }

    #[test]
    fn colour_registers_are_shared_between_images_only_while_1070_is_off() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const VT340_CYAN: [u8; 4] = [51, 204, 204, 255];
        let mut terminal = Terminal::sixel();
        let reuse = "\x1bPq#5@\x1b\\";
        assert_eq!(
            terminal.sixel_colours(&format!("\x1bPq#5;2;100;0;0#5@\x1b\\{reuse}")),
            [RED, VT340_CYAN]
        );
        assert_eq!(
            terminal.sixel_colours(&format!(
                "\x1b[?1070l\x1bPq#5;2;100;0;0#5@\x1b\\{reuse}\x1b[?1070h{reuse}\x1b[?1070l{reuse}"
            )),
            [RED, RED, VT340_CYAN, RED]
        );
        assert_eq!(
            terminal.sixel_colours(&format!("\x1b[!p\x1b[?1070l{reuse}")),
            [VT340_CYAN]
        );
        terminal.sixel_colours("\x1bPq#5;2;100;0;0#5@\x1b\\");
        assert_eq!(
            terminal.sixel_colours(&format!("\x1b[?1;3;8S{reuse}")),
            [VT340_CYAN]
        );
        terminal.sixel_colours("\x1bPq#5;2;100;0;0#5@\x1b\\");
        assert_eq!(
            terminal.sixel_colours(&format!("\x1bc\x1b[?1070l{reuse}")),
            [VT340_CYAN]
        );
    }

    #[test]
    fn device_status_reports_ok() {
        assert_eq!(Terminal::new().reply_to("\x1b[5n"), "\x1b[0n");
    }

    #[test]
    fn cursor_position_is_one_based() {
        let mut terminal = Terminal::new();
        assert_eq!(terminal.reply_to("\x1b[6n"), "\x1b[1;1R");
        assert_eq!(terminal.reply_to("\x1b[3;7H\x1b[6n"), "\x1b[3;7R");
        assert_eq!(terminal.reply_to("\x1b[H\r\nabc\r\nde\x1b[6n"), "\x1b[3;3R");
        assert_eq!(terminal.reply_to("\x1b[99;99H\x1b[6n"), "\x1b[24;80R");
        assert_eq!(terminal.reply_to("\x1b[2A\x1b[5D\x1b[6n"), "\x1b[22;75R");
    }

    #[test]
    fn cursor_position_at_the_pending_wrap_stays_on_the_last_column() {
        let mut terminal = Terminal::sized(3, 5);
        assert_eq!(terminal.reply_to("abcde\x1b[6n"), "\x1b[1;5R");
        assert_eq!(terminal.reply_to("f\x1b[6n"), "\x1b[2;2R");
    }

    #[test]
    fn extended_cursor_position_adds_the_page() {
        let mut terminal = Terminal::new();
        assert_eq!(terminal.reply_to("\x1b[2;4H\x1b[?6n"), "\x1b[?2;4;1R");
    }

    #[test]
    fn version_names_amux() {
        let version = format!("\x1bP>|amux {}\x1b\\", env!("CARGO_PKG_VERSION"));
        let mut terminal = Terminal::new();
        assert_eq!(terminal.reply_to("\x1b[>q"), version);
        assert_eq!(terminal.reply_to("\x1b[>0q"), version);
    }

    #[test]
    fn text_area_size_is_in_cells() {
        assert_eq!(Terminal::new().reply_to("\x1b[18t"), "\x1b[8;24;80t");
        assert_eq!(Terminal::sized(3, 5).reply_to("\x1b[18t"), "\x1b[8;3;5t");
    }

    #[test]
    fn pixel_sizes_are_answered_once_the_cell_size_is_known() {
        let mut terminal = Terminal::new();
        assert_eq!(terminal.reply_to("\x1b[14t\x1b[16t"), "");

        terminal.set_cell_pixels(9, 18);
        assert_eq!(terminal.reply_to("\x1b[14t"), "\x1b[4;432;720t");
        assert_eq!(terminal.reply_to("\x1b[16t"), "\x1b[6;18;9t");

        terminal.parser.callbacks().set_cell_pixels(None);
        assert_eq!(terminal.reply_to("\x1b[14t\x1b[16t"), "");
    }

    #[test]
    fn a_cell_size_with_a_zero_side_is_unknown() {
        let mut terminal = Terminal::new();
        terminal.set_cell_pixels(0, 18);
        assert_eq!(terminal.parser.callbacks().cell_pixels(), None);
        terminal.set_cell_pixels(9, 0);
        assert_eq!(terminal.parser.callbacks().cell_pixels(), None);
        assert_eq!(terminal.reply_to("\x1b[16t"), "");
    }

    #[test]
    fn cell_pixels_round_trip_through_the_packed_cell() {
        let terminal = Terminal::new();
        let callbacks = terminal.parser.callbacks();
        let wide = CellPixels {
            width: u16::MAX,
            height: 1,
        };
        let tall = CellPixels {
            width: 1,
            height: u16::MAX,
        };
        assert!(callbacks.set_cell_pixels(Some(wide)));
        assert_eq!(callbacks.cell_pixels(), Some(wide));
        assert!(!callbacks.set_cell_pixels(Some(wide)));
        assert!(callbacks.set_cell_pixels(Some(tall)));
        assert_eq!(callbacks.cell_pixels(), Some(tall));
        assert!(callbacks.set_cell_pixels(None));
        assert!(!callbacks.set_cell_pixels(Some(CellPixels {
            width: 0,
            height: 7
        })));
    }

    #[test]
    fn several_queries_in_one_read_are_answered_in_order() {
        let mut terminal = Terminal::new();
        terminal.set_cell_pixels(8, 16);
        let replies = terminal.replies_to("\x1b[2;3H\x1b[5n\x1b[6nab\x1b[6n\x1b[16t\x1b[c");
        assert_eq!(
            replies,
            [
                "\x1b[0n",
                "\x1b[2;3R",
                "\x1b[2;5R",
                "\x1b[6;16;8t",
                "\x1b[?62;22c"
            ]
        );
    }

    #[test]
    fn other_sequences_go_unanswered() {
        let mut terminal = Terminal::new();
        let ignored = [
            "\x1b[>c",
            "\x1b[=c",
            "\x1b[1c",
            "\x1b[?5n",
            "\x1b[7n",
            "\x1b[>1q",
            "\x1b[ q",
            "\x1b[21t",
            "\x1b[8;10;10t",
            "\x1b[?u",
            "\x1b[x",
            "\x1b[?$p",
        ];
        for query in ignored {
            assert_eq!(terminal.reply_to(query), "", "{query:?}");
        }
    }

    #[test]
    fn replies_after_the_pane_has_gone_are_dropped() {
        let (input, replies) = mpsc::channel();
        drop(replies);
        let mut parser =
            vt100::Parser::new_with_callbacks(2, 4, 0, PaneCallbacks::new(input, None));
        parser.process(b"\x1b[6n\x1b[cok");
        assert_eq!(parser.screen().contents(), "ok");
    }
}
