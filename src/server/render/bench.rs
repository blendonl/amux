use std::collections::BTreeMap;
use std::fmt::Write;
use std::hint::black_box;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{compose, GraphicsParser, GridDiffer, Viewer};
use crate::protocol::Size;
use crate::server::graphics::place::tests::allocations;
use crate::server::graphics::store::ImageStore;
use crate::server::graphics::PaneGraphics;
use crate::server::layout::{Layout, PaneId};
use crate::server::replies::PaneCallbacks;
use crate::settings::Settings;

const WINDOW: Size = Size {
    rows: 50,
    cols: 200,
};
const PANE: PaneId = PaneId(0);
const SCROLLBACK: usize = 2_000;
const WARMUP_FRAMES: usize = 200;
const MEASURED_FRAMES: usize = 2_000;
const LINES_PER_FRAME: usize = 64;
const KEYSTROKES_PER_LINE: usize = 150;
const PROMPT: &str = "$ ";
const CLEAR_THE_PROMPT_LINE: &str = "\r\x1b[K$ ";
const FILLER: &str = "the quick brown fox jumps over the lazy dog ";

struct Terminal {
    layout: Layout,
    parsers: BTreeMap<PaneId, GraphicsParser>,
    graphics: PaneGraphics,
    settings: Settings,
    differ: GridDiffer,
}

impl Terminal {
    fn new() -> Self {
        let settings = Settings::default();
        let store = Arc::new(ImageStore::new(settings.images.memory_bytes()));
        let images = store.open_pane();
        let callbacks = PaneCallbacks::new(mpsc::channel().0, Some(images.clone()));
        let parser =
            vt100::Parser::new_with_callbacks(WINDOW.rows, WINDOW.cols, SCROLLBACK, callbacks);
        Self {
            layout: Layout::new(PANE),
            parsers: BTreeMap::from([(PANE, Mutex::new(parser))]),
            graphics: PaneGraphics::new(images),
            settings,
            differ: GridDiffer::new(WINDOW),
        }
    }

    fn output(&mut self, bytes: &[u8]) {
        self.graphics.process(bytes, &self.parsers[&PANE]);
    }

    fn frame(&mut self) -> Vec<u8> {
        let frame = compose(
            &self.layout,
            WINDOW,
            PANE,
            &self.parsers,
            &self.settings,
            Viewer::text(),
        );
        self.differ.diff(&frame)
    }

    fn timed_frame(&mut self, output: &[u8]) -> Sample {
        let allocations_before = allocations();
        let started = Instant::now();
        self.output(black_box(output));
        let sent = black_box(self.frame());
        let elapsed = started.elapsed();
        Sample {
            elapsed,
            allocations: allocations() - allocations_before,
            bytes: sent.len(),
        }
    }
}

struct Sample {
    elapsed: Duration,
    allocations: usize,
    bytes: usize,
}

fn report(name: &str, mut samples: Vec<Sample>) {
    samples.sort_by_key(|sample| sample.elapsed);
    let micros = |index: usize| samples[index].elapsed.as_secs_f64() * 1e6;
    let count = samples.len();
    let per_frame = |total: usize| total as f64 / count as f64;
    let allocations = per_frame(samples.iter().map(|sample| sample.allocations).sum());
    let bytes = per_frame(samples.iter().map(|sample| sample.bytes).sum());
    println!();
    println!(
        "{:<18} {:>10} {:>10} {:>13} {:>13}",
        format!("{}x{}", WINDOW.cols, WINDOW.rows),
        "median µs",
        "p90 µs",
        "allocs/frame",
        "bytes/frame"
    );
    println!(
        "{name:<18} {:>10.1} {:>10.1} {allocations:>13.1} {bytes:>13.0}",
        micros(count / 2),
        micros(count * 9 / 10),
    );
}

fn full_screen() -> String {
    let label_width = 16;
    let filler: String = FILLER
        .chars()
        .cycle()
        .take(usize::from(WINDOW.cols) - 1 - label_width)
        .collect();
    let mut screen = String::new();
    for row in 0..WINDOW.rows - 1 {
        let _ = write!(screen, "{row:>4} \x1b[1;34mdrwxr-xr-x\x1b[0m {filler}\r\n");
    }
    screen.push_str(PROMPT);
    screen
}

fn letter(index: usize) -> u8 {
    b'a' + (index % 26) as u8
}

fn seq_lines(next: &mut usize, count: usize) -> String {
    let mut lines = String::new();
    for number in *next..*next + count {
        let _ = write!(lines, "{number}\r\n");
    }
    *next += count;
    lines
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn bench_keystroke_frame() {
    let mut terminal = Terminal::new();
    terminal.output(full_screen().as_bytes());
    terminal.frame();
    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    for keystroke in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        if keystroke % KEYSTROKES_PER_LINE == KEYSTROKES_PER_LINE - 1 {
            terminal.output(CLEAR_THE_PROMPT_LINE.as_bytes());
            terminal.frame();
        }
        let sample = terminal.timed_frame(&[letter(keystroke)]);
        if keystroke >= WARMUP_FRAMES {
            samples.push(sample);
        }
    }
    report("keystroke frame", samples);
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn bench_scrolling_frame() {
    let mut terminal = Terminal::new();
    terminal.frame();
    let mut next_line = 1;
    let mut samples = Vec::with_capacity(MEASURED_FRAMES);
    for frame in 0..WARMUP_FRAMES + MEASURED_FRAMES {
        let lines = seq_lines(&mut next_line, LINES_PER_FRAME);
        let sample = terminal.timed_frame(lines.as_bytes());
        if frame >= WARMUP_FRAMES {
            samples.push(sample);
        }
    }
    report(&format!("{LINES_PER_FRAME} seq lines"), samples);
}
