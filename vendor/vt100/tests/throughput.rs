use std::hint::black_box;
use std::time::Instant;

const ROWS: u16 = 50;
const COLS: u16 = 200;
const SCROLLBACK: usize = 2000;
const LINES: usize = 200_000;
const CHUNK: usize = 16 * 1024;

fn seq_output(lines: usize) -> Vec<u8> {
    (1..=lines)
        .flat_map(|number| format!("{number}\r\n").into_bytes())
        .collect()
}

#[test]
#[ignore = "a benchmark; scripts/bench runs it"]
fn seq_lines_through_the_parser() {
    let output = seq_output(LINES);
    let mut parser = vt100::Parser::new(ROWS, COLS, SCROLLBACK);
    let started = Instant::now();
    for chunk in output.chunks(CHUNK) {
        parser.process(black_box(chunk));
    }
    let elapsed = started.elapsed();
    black_box(parser.screen().cursor_position());
    let per_line = elapsed.as_secs_f64() * 1e9 / LINES as f64;
    println!();
    println!(
        "{:<28} {:>10} {:>12} {:>10}",
        format!("{COLS}x{ROWS}, {SCROLLBACK} history"),
        "lines",
        "ns/line",
        "total ms"
    );
    println!(
        "{:<28} {LINES:>10} {per_line:>12.1} {:>10.1}",
        "seq through Parser",
        elapsed.as_secs_f64() * 1e3
    );
}
