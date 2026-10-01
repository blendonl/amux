use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use super::placeholder::{DIACRITICS, PLACEHOLDER};
use super::{compose, GraphicsParser, GridDiffer};
use crate::client::graphics::KittyWriter;
use crate::protocol::Size;
use crate::server::connection::Origin;
use crate::server::graphics::store::ImageStore;
use crate::server::graphics::PaneGraphics;
use crate::server::layout::{Layout, PaneId, SplitDirection};
use crate::server::replies::PaneCallbacks;
use crate::server::upload::{Turn, Uploader};
use crate::settings::Settings;

const ANDROID_FIXTURES: &str = "android/terminal-emulator/src/test/resources/graphics";
const UPDATE_FIXTURES: &str = "AMUX_UPDATE_FIXTURES";
const TINY_PNG: &[u8] = include_bytes!("../../../tests/fixtures/tiny.png");
const PROGRAM_CHUNK_LEN: usize = 4096;
const LEFT: PaneId = PaneId(0);
const RIGHT: PaneId = PaneId(1);

struct Session {
    window: Size,
    store: Arc<ImageStore>,
    layout: Layout,
    parsers: BTreeMap<PaneId, GraphicsParser>,
    graphics: BTreeMap<PaneId, PaneGraphics>,
}

impl Session {
    fn single(window: Size) -> Self {
        Self::with_layout(window, Layout::new(LEFT))
    }

    fn side_by_side(window: Size) -> Self {
        let mut layout = Layout::new(LEFT);
        layout
            .split(LEFT, RIGHT, SplitDirection::LeftRight, window)
            .unwrap();
        Self::with_layout(window, layout)
    }

    fn with_layout(window: Size, layout: Layout) -> Self {
        let store = Arc::new(ImageStore::new(1 << 30));
        let mut parsers = BTreeMap::new();
        let mut graphics = BTreeMap::new();
        for (pane, rect) in layout.rects(window) {
            let images = store.open_pane();
            let callbacks = PaneCallbacks::new(mpsc::channel().0, Some(images.clone()));
            let parser = vt100::Parser::new_with_callbacks(rect.rows, rect.cols, 0, callbacks);
            parsers.insert(pane, Mutex::new(parser));
            graphics.insert(pane, PaneGraphics::new(images));
        }
        Self {
            window,
            store,
            layout,
            parsers,
            graphics,
        }
    }

    fn run(&mut self, pane: PaneId, output: &str) {
        self.graphics
            .get_mut(&pane)
            .unwrap()
            .process(output.as_bytes(), &self.parsers[&pane]);
    }
}

struct Terminal {
    differ: GridDiffer,
    uploader: Uploader,
    kitty: KittyWriter,
}

impl Terminal {
    fn new(session: &Session) -> Self {
        let mut uploader = Uploader::new(Arc::clone(&session.store), Origin::Local, 1 << 30);
        uploader.set_graphics(true);
        Self {
            differ: GridDiffer::new(session.window),
            uploader,
            kitty: KittyWriter::default(),
        }
    }

    fn show(&mut self, session: &Session) -> (Vec<u8>, Vec<(u32, u16, u16)>) {
        let mut written = Vec::new();
        let mut shown = Vec::new();
        let mut dirty = true;
        loop {
            while let Some(job) = self.uploader.take_job() {
                self.uploader.finish(job.run());
            }
            let Some(turn) = self.uploader.choose(dirty) else {
                break;
            };
            match turn {
                Turn::Frame => {
                    dirty = false;
                    let frame = compose(
                        &session.layout,
                        session.window,
                        LEFT,
                        &session.parsers,
                        &Settings::default(),
                        self.uploader.viewer(),
                    );
                    self.uploader.frame(&frame.images);
                    written.extend(self.differ.diff(&frame));
                    shown = frame
                        .images
                        .iter()
                        .map(|image| (image.key, image.cols, image.rows))
                        .collect();
                }
                Turn::Upload => {
                    if let Some(op) = self.uploader.next() {
                        self.kitty.write(op, &mut written);
                    }
                }
            }
        }
        (written, shown)
    }
}

fn transmit(control: &str, data: &[u8]) -> String {
    let encoded = STANDARD.encode(data);
    let mut chunks = encoded.as_bytes().chunks(PROGRAM_CHUNK_LEN).peekable();
    let mut keys = format!("{control},");
    let mut commands = String::new();
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        let payload = std::str::from_utf8(chunk).unwrap();
        write!(commands, "\x1b_G{keys}m={more};{payload}\x1b\\").unwrap();
        keys.clear();
    }
    commands
}

fn noise(len: usize) -> Vec<u8> {
    let mut state: u32 = 1;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        })
        .collect()
}

fn printed_placeholders(id: u8, (row, col): (u16, u16), cols: u16, rows: u16) -> String {
    let mut printed = format!("\x1b[38;5;{id}m");
    for image_row in 0..rows {
        write!(printed, "\x1b[{};{}H", row + image_row + 1, col + 1).unwrap();
        printed.push(PLACEHOLDER);
        printed.push(DIACRITICS[usize::from(image_row)]);
        printed.extend(std::iter::repeat_n(PLACEHOLDER, usize::from(cols - 1)));
    }
    printed + "\x1b[0m"
}

fn controls(written: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(written)
        .split("\x1b_G")
        .skip(1)
        .map(|command| {
            let end = command.find([';', '\x1b']).unwrap();
            command[..end].to_owned()
        })
        .collect()
}

fn fixture_is_current(name: &str, contents: &[u8]) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(ANDROID_FIXTURES);
    let path = dir.join(name);
    if std::env::var_os(UPDATE_FIXTURES).is_some() {
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, contents).unwrap();
    }
    assert!(
        fs::read(&path).unwrap_or_default() == contents,
        "{} is out of date, run the tests again with {UPDATE_FIXTURES}=1",
        path.display()
    );
}

#[test]
fn a_png_is_uploaded_placed_and_painted_below_the_text() {
    let mut session = Session::single(Size { rows: 6, cols: 20 });
    session.run(LEFT, "tiny\r\n");
    session.run(LEFT, &transmit("a=T,f=100,i=1,c=4,r=4,q=2", TINY_PNG));
    let (written, shown) = Terminal::new(&session).show(&session);

    assert_eq!(shown, [(1, 4, 4)]);
    assert_eq!(
        controls(&written),
        ["a=t,i=1,f=100,s=20,v=40,q=2,m=0", "a=p,U=1,i=1,c=4,r=4,q=2"]
    );
    fixture_is_current("png-upload.txt", &written);
}

#[test]
fn raw_rgba_goes_compressed_in_several_chunks() {
    let mut session = Session::single(Size { rows: 6, cols: 20 });
    session.run(
        LEFT,
        &transmit(
            "a=T,f=32,s=48,v=32,i=2,U=1,c=6,r=3,q=2",
            &noise(48 * 32 * 4),
        ),
    );
    session.run(LEFT, &printed_placeholders(2, (1, 2), 6, 3));
    let (written, shown) = Terminal::new(&session).show(&session);

    assert_eq!(shown, [(1, 6, 3)]);
    let controls = controls(&written);
    let (first, rest) = controls.split_first().unwrap();
    let (place, chunks) = rest.split_last().unwrap();
    let (last, middle) = chunks.split_last().unwrap();
    assert_eq!(first, "a=t,i=1,f=32,s=48,v=32,o=z,q=2,m=1");
    assert!(!middle.is_empty() && middle.iter().all(|chunk| chunk == "m=1"));
    assert_eq!(last, "m=0");
    assert_eq!(place, "a=p,U=1,i=1,c=6,r=3,q=2");
    fixture_is_current("rgba-chunks.txt", &written);
}

#[test]
fn placeholders_stop_at_the_border_and_the_window_edges() {
    let mut session = Session::side_by_side(Size { rows: 6, cols: 21 });
    let image = transmit("a=T,f=24,s=2,v=2,i=1,c=5,r=2,C=1,q=2", &noise(12));
    session.run(LEFT, &format!("\x1b[2;8H{image}"));
    session.run(RIGHT, &format!("\x1b[6;8H{image}"));
    let (written, shown) = Terminal::new(&session).show(&session);

    assert_eq!(shown, [(1, 5, 2), (2, 5, 2)]);
    assert_eq!(
        controls(&written),
        [
            "a=t,i=1,f=32,s=50,v=40,o=z,q=2,m=0",
            "a=p,U=1,i=1,c=5,r=2,q=2",
            "a=t,i=2,f=32,s=50,v=40,o=z,q=2,m=0",
            "a=p,U=1,i=2,c=5,r=2,q=2"
        ]
    );
    fixture_is_current("clipped.txt", &written);
}

#[test]
fn a_key_above_2_24_spells_its_high_byte_in_the_third_diacritic() {
    let mut session = Session::single(Size { rows: 3, cols: 12 });
    session.store.skip_display_keys(0x2a12_3455);
    session.run(LEFT, "\x1b[2;2H");
    session.run(LEFT, &transmit("a=T,f=100,i=1,c=3,r=1,C=1,q=2", TINY_PNG));
    let (written, shown) = Terminal::new(&session).show(&session);

    assert_eq!(shown, [(0x2a12_3456, 3, 1)]);
    assert_eq!(
        controls(&written),
        [
            "a=t,i=705836118,f=32,s=30,v=20,o=z,q=2,m=0",
            "a=p,U=1,i=705836118,c=3,r=1,q=2"
        ]
    );
    let text = String::from_utf8_lossy(&written);
    assert!(text.contains("38;2;18;52;86m"), "{text:?}");
    fixture_is_current("high-key.txt", &written);
}

#[test]
fn a_deleted_image_is_erased_and_deleted_from_the_terminal() {
    let mut session = Session::single(Size { rows: 3, cols: 12 });
    session.run(
        LEFT,
        &transmit("a=T,f=24,s=1,v=1,i=7,c=2,r=1,C=1,q=2", &[255, 0, 0]),
    );
    let mut terminal = Terminal::new(&session);
    let (shown_bytes, shown) = terminal.show(&session);
    assert_eq!(shown, [(1, 2, 1)]);
    fixture_is_current("delete-shown.txt", &shown_bytes);

    session.run(LEFT, "\x1b_Ga=d,d=I,i=7,q=2\x1b\\");
    let (gone_bytes, gone) = terminal.show(&session);
    assert!(gone.is_empty());
    assert_eq!(controls(&gone_bytes), ["a=d,d=I,i=1,q=2"]);
    fixture_is_current("delete-gone.txt", &gone_bytes);
}
