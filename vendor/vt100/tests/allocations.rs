use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

const ROWS: u16 = 50;
const COLS: u16 = 200;
const SCROLLBACK: usize = 100;
const MEASURED_LINES: usize = 1000;

struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations() -> usize {
    ALLOCATIONS.with(Cell::get)
}

fn lines(count: usize) -> Vec<u8> {
    (1..=count)
        .flat_map(|number| {
            let colour = number % 8;
            format!(
                "{number} \x1b[3{colour};4{colour}mcoloured\x1b[K\x1b[m 中文\r\n"
            )
            .into_bytes()
        })
        .collect()
}

fn allocations_while_scrolling(
    parser: &mut vt100::Parser,
    warm_up_lines: usize,
) -> usize {
    parser.process(&lines(warm_up_lines));
    let measured = lines(MEASURED_LINES);
    let before = allocations();
    parser.process(&measured);
    allocations() - before
}

#[test]
fn scrolling_with_full_history_allocates_nothing() {
    let mut parser = vt100::Parser::new(ROWS, COLS, SCROLLBACK);
    let warm_up = usize::from(ROWS) + SCROLLBACK;
    assert_eq!(allocations_while_scrolling(&mut parser, warm_up), 0);
    assert_eq!(parser.screen().history_len(), SCROLLBACK);
}

#[test]
fn scrolling_without_history_allocates_nothing() {
    let mut parser = vt100::Parser::new(ROWS, COLS, 0);
    assert_eq!(allocations_while_scrolling(&mut parser, 0), 0);
    assert_eq!(parser.screen().history_len(), 0);
}

#[test]
fn scrolling_a_region_allocates_nothing() {
    let mut parser = vt100::Parser::new(ROWS, COLS, SCROLLBACK);
    parser.process(b"\x1b[5;45r\x1b[45;1H");
    assert_eq!(allocations_while_scrolling(&mut parser, 0), 0);
    assert_eq!(parser.screen().history_len(), 0);
}
