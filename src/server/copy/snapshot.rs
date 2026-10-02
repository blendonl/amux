use super::Point;

pub struct Snapshot {
    screen: vt100::Screen,
    cursor: Point,
}

impl Snapshot {
    pub fn new(screen: vt100::Screen) -> Self {
        let (row, col) = screen.cursor_position();
        let cursor = Point {
            line: screen.history_len() + usize::from(row),
            col,
        };
        Self { screen, cursor }
    }

    pub fn screen(&self) -> &vt100::Screen {
        &self.screen
    }

    pub fn history(&self) -> usize {
        self.screen.history_len()
    }

    pub fn lines(&self) -> usize {
        self.history() + usize::from(self.screen.size().0)
    }

    pub fn cursor(&self) -> Point {
        self.cursor
    }

    pub fn cell(&self, point: Point) -> Option<&vt100::Cell> {
        self.screen.line_cell(point.line, point.col)
    }

    pub fn wrapped(&self, line: usize) -> bool {
        self.screen.line_wrapped(line)
    }

    pub fn is_continuation(&self, point: Point) -> bool {
        self.cell(point)
            .is_some_and(vt100::Cell::is_wide_continuation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(rows: u16, cols: u16, input: &str) -> (vt100::Parser, Snapshot) {
        let mut parser = vt100::Parser::new(rows, cols, 100);
        parser.process(input.as_bytes());
        let snapshot = Snapshot::new(parser.screen().clone());
        (parser, snapshot)
    }

    fn text(snapshot: &Snapshot, line: usize) -> String {
        snapshot.screen().line_contents(line, 0, 80)
    }

    #[test]
    fn history_lines_come_before_the_screen_and_the_cursor_is_absolute() {
        let (_, snapshot) = snapshot(3, 10, "one\r\ntwo\r\nthree\r\nfour\r\nfi");
        assert_eq!(snapshot.history(), 2);
        assert_eq!(snapshot.lines(), 5);
        assert_eq!(
            (0..5).map(|line| text(&snapshot, line)).collect::<Vec<_>>(),
            ["one", "two", "three", "four", "fi"]
        );
        assert_eq!(snapshot.cursor(), Point { line: 4, col: 2 });
        assert_eq!(
            snapshot.cell(Point { line: 0, col: 1 }).unwrap().contents(),
            "n"
        );
    }

    #[test]
    fn the_live_screen_keeps_running_without_moving_the_snapshot() {
        let (mut parser, snapshot) = snapshot(3, 10, "one\r\ntwo\r\nthree\r\nfour");
        parser.process(b"\r\nfive\r\nsix");
        assert_eq!(parser.screen().scrollback(), 0);
        assert_eq!(parser.screen().history_len(), 3);
        assert_eq!(snapshot.history(), 1);
        assert_eq!(text(&snapshot, 3), "four");
    }

    #[test]
    fn the_alternate_screen_has_no_history() {
        let (_, snapshot) = snapshot(3, 10, "one\r\ntwo\r\nthree\r\nfour\x1b[?1049h\x1b[Hvim");
        assert_eq!(snapshot.history(), 0);
        assert_eq!(snapshot.lines(), 3);
        assert_eq!(text(&snapshot, 0), "vim");
    }

    #[test]
    fn wrapped_lines_and_narrow_history_rows_read_as_written() {
        let (mut parser, _) = snapshot(2, 4, "abcdefgh\r\nij\r\nkl");
        parser.screen_mut().set_size(2, 8);
        let snapshot = Snapshot::new(parser.screen().clone());
        assert!(snapshot.wrapped(0));
        assert!(!snapshot.wrapped(1));
        assert!(snapshot.cell(Point { line: 0, col: 5 }).is_none());
        assert!(snapshot.cell(Point { line: 2, col: 5 }).is_some());
    }

    #[test]
    fn the_second_half_of_a_wide_character_is_a_continuation() {
        let (_, snapshot) = snapshot(2, 10, "a日b");
        assert!(!snapshot.is_continuation(Point { line: 0, col: 1 }));
        assert!(snapshot.is_continuation(Point { line: 0, col: 2 }));
        assert!(!snapshot.is_continuation(Point { line: 0, col: 3 }));
    }
}
