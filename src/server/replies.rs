use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};

use crate::protocol::{CellPixels, RELEASE};

const DEVICE_ATTRIBUTES: &[u16] = &[62, 22];

#[derive(Clone)]
pub struct PaneCallbacks {
    input: mpsc::Sender<Vec<u8>>,
    cell_pixels: Arc<AtomicU32>,
    graphics: Arc<AtomicBool>,
}

impl PaneCallbacks {
    pub fn new(input: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            input,
            cell_pixels: Arc::new(AtomicU32::new(0)),
            graphics: Arc::new(AtomicBool::new(false)),
        }
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

    fn answer(
        &self,
        screen: &vt100::Screen,
        private: Option<u8>,
        param: u16,
        action: char,
    ) -> Option<String> {
        match (private, action, param) {
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
        if i2.is_some() {
            return;
        }
        let param = params
            .first()
            .and_then(|param| param.first())
            .copied()
            .unwrap_or(0);
        if let Some(reply) = self.answer(screen, i1, param, c) {
            self.reply(reply.into_bytes());
        }
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
            let (input, replies) = mpsc::channel();
            Self {
                parser: vt100::Parser::new_with_callbacks(rows, cols, 0, PaneCallbacks::new(input)),
                replies,
            }
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
    fn cell_pixels_round_trip_through_the_shared_cell() {
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
        assert_eq!(callbacks.clone().cell_pixels(), Some(wide));
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
        let mut parser = vt100::Parser::new_with_callbacks(2, 4, 0, PaneCallbacks::new(input));
        parser.process(b"\x1b[6n\x1b[cok");
        assert_eq!(parser.screen().contents(), "ok");
    }

    #[test]
    fn the_graphics_flag_is_shared_with_clones() {
        let terminal = Terminal::new();
        let callbacks = terminal.parser.callbacks();
        assert!(!callbacks.graphics());
        callbacks.clone().set_graphics(true);
        assert!(callbacks.graphics());
    }
}
