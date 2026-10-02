mod motion;
mod selection;
mod snapshot;

use std::ops::Range;

use motion::Span;
use selection::{Kind, Selection};
pub use snapshot::Snapshot;

use super::render::CopyView;
use crate::keys::{Decoded, Key, Scanner};
use crate::protocol::Size;
use crate::settings::{CopyAction, Table};

const MAX_COUNT: usize = 99_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: usize,
    pub col: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Refresh,
    Exit,
    Copy(String),
}

pub struct CopyMode {
    snapshot: Snapshot,
    size: Size,
    top: usize,
    cursor: Point,
    count: Option<usize>,
    paste: Scanner,
    selection: Option<Selection>,
}

impl CopyMode {
    pub fn new(snapshot: Snapshot, size: Size) -> Self {
        let mut mode = Self {
            cursor: snapshot.cursor(),
            snapshot,
            size: size.clamped(),
            top: 0,
            count: None,
            paste: Scanner::default(),
            selection: None,
        };
        mode.top = mode.bottom();
        mode.clamp();
        mode
    }

    pub fn refresh(&mut self, snapshot: Snapshot) {
        let from_bottom = self.bottom() - self.top;
        let row = self.cursor.line - self.top;
        self.snapshot = snapshot;
        self.selection = None;
        self.top = self.bottom().saturating_sub(from_bottom);
        self.cursor.line = self.top + row;
        self.clamp();
    }

    pub fn resize(&mut self, size: Size) {
        self.size = size.clamped();
        self.clamp();
    }

    pub fn page_up(&mut self) {
        self.act(CopyAction::PageUp, 1);
    }

    pub fn press(&mut self, decoded: &Decoded, bindings: &Table<CopyAction>) -> Outcome {
        if self.swallows_paste(&decoded.raw) {
            return Outcome::Stay;
        }
        if let Some(digit) = decoded.key.and_then(|key| self.count_digit(key)) {
            let count = self.count.unwrap_or(0).saturating_mul(10) + digit;
            self.count = Some(count.min(MAX_COUNT));
            return Outcome::Stay;
        }
        for (_, action) in bindings.resolve(decoded) {
            let count = self.count.take().unwrap_or(1);
            let outcome = action.map_or(Outcome::Stay, |action| self.act(action, count));
            if outcome != Outcome::Stay {
                return outcome;
            }
        }
        Outcome::Stay
    }

    pub fn view(&self) -> CopyView<'_> {
        CopyView {
            screen: self.snapshot.screen(),
            top: self.top,
            cursor: (
                u16::try_from(self.cursor.line - self.top).unwrap_or(u16::MAX),
                self.cursor.col,
            ),
            position: format!("[{}/{}]", self.cursor.line + 1, self.snapshot.lines()),
            selection: self.selected_columns(),
        }
    }

    fn selected_columns(&self) -> Vec<(u16, Range<u16>)> {
        let Some(selection) = self.selection else {
            return Vec::new();
        };
        let cols = self.size.cols;
        (0..self.size.rows)
            .filter_map(|row| {
                let line = self.top + usize::from(row);
                let selected = selection.columns(&self.snapshot, self.cursor, line)?;
                let visible = selected.start.min(cols)..selected.end.min(cols);
                (!visible.is_empty()).then_some((row, visible))
            })
            .collect()
    }

    fn swallows_paste(&mut self, raw: &[u8]) -> bool {
        let pasting = self.paste.is_pasting();
        for &byte in raw {
            self.paste.feed(byte);
        }
        pasting || self.paste.is_pasting()
    }

    fn count_digit(&self, key: Key) -> Option<usize> {
        let digit = key.printable()?.to_digit(10)?;
        (digit != 0 || self.count.is_some()).then(|| usize::try_from(digit).unwrap_or_default())
    }

    fn act(&mut self, action: CopyAction, count: usize) -> Outcome {
        let cols = self.size.cols;
        match action {
            CopyAction::CursorLeft => self.repeat(count, |mode, from| mode.left_of(from)),
            CopyAction::CursorRight => self.repeat(count, |mode, from| mode.right_of(from)),
            CopyAction::CursorUp => self.move_to_line(self.cursor.line.saturating_sub(count)),
            CopyAction::CursorDown => self.move_to_line(self.cursor.line.saturating_add(count)),
            CopyAction::NextWord => self.repeat(count, |mode, from| {
                motion::next_word(&mode.snapshot, cols, from, Span::Word)
            }),
            CopyAction::NextSpace => self.repeat(count, |mode, from| {
                motion::next_word(&mode.snapshot, cols, from, Span::Space)
            }),
            CopyAction::NextWordEnd => self.repeat(count, |mode, from| {
                motion::next_word_end(&mode.snapshot, cols, from, Span::Word)
            }),
            CopyAction::NextSpaceEnd => self.repeat(count, |mode, from| {
                motion::next_word_end(&mode.snapshot, cols, from, Span::Space)
            }),
            CopyAction::PreviousWord => self.repeat(count, |mode, from| {
                motion::previous_word(&mode.snapshot, cols, from, Span::Word)
            }),
            CopyAction::PreviousSpace => self.repeat(count, |mode, from| {
                motion::previous_word(&mode.snapshot, cols, from, Span::Space)
            }),
            CopyAction::StartOfLine => self.cursor.col = 0,
            CopyAction::BackToIndentation => {
                self.cursor.col = motion::first_non_blank(&self.snapshot, cols, self.cursor.line);
            }
            CopyAction::EndOfLine => {
                self.cursor.col = motion::last_non_blank(&self.snapshot, cols, self.cursor.line);
            }
            CopyAction::HistoryTop => {
                self.top = 0;
                self.cursor = Point { line: 0, col: 0 };
            }
            CopyAction::HistoryBottom => {
                let line = self.last_line();
                self.top = self.bottom();
                self.cursor = Point {
                    line,
                    col: motion::last_non_blank(&self.snapshot, cols, line),
                };
            }
            CopyAction::TopLine => self.move_to_row(0),
            CopyAction::MiddleLine => self.move_to_row((self.visible() - 1) / 2),
            CopyAction::BottomLine => self.move_to_row(self.visible() - 1),
            CopyAction::ScrollUp => self.top = self.top.saturating_sub(count),
            CopyAction::ScrollDown => self.top = self.top.saturating_add(count).min(self.bottom()),
            CopyAction::HalfpageUp => self.scroll_up(count.saturating_mul(self.half_page())),
            CopyAction::HalfpageDown => self.scroll_down(count.saturating_mul(self.half_page())),
            CopyAction::PageUp => self.scroll_up(count.saturating_mul(self.page())),
            CopyAction::PageDown => self.scroll_down(count.saturating_mul(self.page())),
            CopyAction::BeginSelection => self.select(Kind::Char),
            CopyAction::SelectLine => self.select(Kind::Line),
            CopyAction::OtherEnd => self.swap_ends(),
            CopyAction::ClearSelection => self.selection = None,
            CopyAction::CopySelectionAndCancel => return self.copy_selection(),
            CopyAction::ClearSelectionOrCancel => {
                if self.selection.take().is_none() {
                    return Outcome::Exit;
                }
            }
            CopyAction::Cancel => return Outcome::Exit,
            CopyAction::RefreshFromPane => return Outcome::Refresh,
        }
        self.clamp();
        Outcome::Stay
    }

    fn select(&mut self, kind: Kind) {
        self.selection = Some(Selection {
            anchor: self.cursor,
            kind,
        });
    }

    fn swap_ends(&mut self) {
        if let Some(selection) = &mut self.selection {
            std::mem::swap(&mut selection.anchor, &mut self.cursor);
            self.follow_cursor();
        }
    }

    fn copy_selection(&self) -> Outcome {
        let text = self
            .selection
            .map(|selection| selection.text(&self.snapshot, self.cursor))
            .unwrap_or_default();
        if text.is_empty() {
            Outcome::Exit
        } else {
            Outcome::Copy(text)
        }
    }

    fn repeat(&mut self, count: usize, motion: impl Fn(&Self, Point) -> Point) {
        for _ in 0..count {
            let next = motion(self, self.cursor);
            if next == self.cursor {
                break;
            }
            self.cursor = next;
        }
        self.follow_cursor();
    }

    fn left_of(&self, from: Point) -> Point {
        let mut col = from.col;
        while col > 0 {
            col -= 1;
            if !self.snapshot.is_continuation(Point { col, ..from }) {
                return Point { col, ..from };
            }
        }
        from
    }

    fn right_of(&self, from: Point) -> Point {
        let mut col = from.col;
        while col + 1 < self.size.cols {
            col += 1;
            if !self.snapshot.is_continuation(Point { col, ..from }) {
                return Point { col, ..from };
            }
        }
        from
    }

    fn move_to_line(&mut self, line: usize) {
        self.cursor.line = line.min(self.last_line());
        self.follow_cursor();
    }

    fn move_to_row(&mut self, row: usize) {
        self.cursor = Point {
            line: self.top + row,
            col: 0,
        };
    }

    fn scroll_up(&mut self, lines: usize) {
        self.top = self.top.saturating_sub(lines);
        self.cursor.line = self.cursor.line.saturating_sub(lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.top = self.top.saturating_add(lines).min(self.bottom());
        self.cursor.line = self.cursor.line.saturating_add(lines);
    }

    fn follow_cursor(&mut self) {
        let rows = self.rows();
        if self.cursor.line < self.top {
            self.top = self.cursor.line;
        } else if self.cursor.line >= self.top + rows {
            self.top = self.cursor.line + 1 - rows;
        }
    }

    fn clamp(&mut self) {
        self.top = self.top.min(self.bottom());
        self.cursor.line = self
            .cursor
            .line
            .clamp(self.top, self.top + self.visible() - 1);
        self.cursor.col = self.cursor.col.min(self.size.cols - 1);
        if self.cursor.col > 0 && self.snapshot.is_continuation(self.cursor) {
            self.cursor.col -= 1;
        }
    }

    fn rows(&self) -> usize {
        usize::from(self.size.rows)
    }

    fn half_page(&self) -> usize {
        (self.rows() / 2).max(1)
    }

    fn page(&self) -> usize {
        self.rows().saturating_sub(1).max(1)
    }

    fn last_line(&self) -> usize {
        self.snapshot.lines() - 1
    }

    fn bottom(&self) -> usize {
        self.snapshot.lines().saturating_sub(self.rows())
    }

    fn visible(&self) -> usize {
        (self.snapshot.lines() - self.top).min(self.rows())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyDecoder;
    use crate::settings::Keymap;

    fn numbered(count: usize) -> String {
        (1..=count)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    fn browsing(rows: u16, cols: u16, input: &str) -> CopyMode {
        let mut parser = vt100::Parser::new(rows, cols, 1000);
        parser.process(input.as_bytes());
        CopyMode::new(Snapshot::new(parser.screen().clone()), Size { rows, cols })
    }

    fn type_keys(mode: &mut CopyMode, input: &[u8]) -> Vec<Outcome> {
        let bindings = Keymap::default().copy;
        KeyDecoder::default()
            .feed(input)
            .iter()
            .map(|decoded| mode.press(decoded, &bindings))
            .filter(|outcome| *outcome != Outcome::Stay)
            .collect()
    }

    fn type_keys_and_pause(mode: &mut CopyMode, input: &[u8]) -> Vec<Outcome> {
        let bindings = Keymap::default().copy;
        let mut decoder = KeyDecoder::default();
        let mut decoded = decoder.feed(input);
        decoded.extend(decoder.time_out());
        decoded
            .iter()
            .map(|decoded| mode.press(decoded, &bindings))
            .filter(|outcome| *outcome != Outcome::Stay)
            .collect()
    }

    fn copied(text: &str) -> Vec<Outcome> {
        vec![Outcome::Copy(text.into())]
    }

    fn top_and_cursor(mode: &CopyMode) -> (usize, usize, u16) {
        (mode.top, mode.cursor.line, mode.cursor.col)
    }

    #[test]
    fn copy_mode_starts_at_the_live_screen_with_the_live_cursor() {
        let mode = browsing(5, 20, &numbered(30));
        assert_eq!(mode.snapshot.lines(), 30);
        assert_eq!(top_and_cursor(&mode), (25, 29, 2));
        let view = mode.view();
        assert_eq!(view.top, 25);
        assert_eq!(view.cursor, (4, 2));
        assert_eq!(view.position, "[30/30]");
    }

    #[test]
    fn the_cursor_scrolls_the_view_when_it_leaves_it() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"kkkk");
        assert_eq!(top_and_cursor(&mode), (25, 25, 2));
        type_keys(&mut mode, b"k");
        assert_eq!(top_and_cursor(&mode), (24, 24, 2));
        type_keys(&mut mode, b"jjjjjjjjjj");
        assert_eq!(top_and_cursor(&mode), (25, 29, 2));
        type_keys(&mut mode, b"hhhl\x1b[D");
        assert_eq!(top_and_cursor(&mode), (25, 29, 0));
        assert_eq!(mode.view().position, "[30/30]");
    }

    #[test]
    fn scrolling_moves_the_view_and_keeps_the_cursor_inside_it() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"\x19");
        assert_eq!(top_and_cursor(&mode), (24, 28, 2));
        type_keys(&mut mode, b"\x15");
        assert_eq!(top_and_cursor(&mode), (22, 26, 2));
        type_keys(&mut mode, b"\x1b[5~");
        assert_eq!(top_and_cursor(&mode), (18, 22, 2));
        type_keys(&mut mode, b"\x04\x06");
        assert_eq!(top_and_cursor(&mode), (24, 28, 2));
        type_keys(&mut mode, b"\x1b[6~\x05");
        assert_eq!(top_and_cursor(&mode), (25, 29, 2));
        type_keys(&mut mode, b"kkkk\x05");
        assert_eq!(top_and_cursor(&mode), (25, 25, 2));
    }

    #[test]
    fn a_count_repeats_the_next_action() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"10k");
        assert_eq!(top_and_cursor(&mode), (19, 19, 2));
        type_keys(&mut mode, b"2\x15");
        assert_eq!(top_and_cursor(&mode), (15, 15, 2));
        type_keys(&mut mode, b"0");
        assert_eq!(top_and_cursor(&mode), (15, 15, 0));
        type_keys(&mut mode, b"3xj");
        assert_eq!(top_and_cursor(&mode), (15, 16, 0));
        type_keys(&mut mode, b"999999j");
        assert_eq!(top_and_cursor(&mode), (25, 29, 0));
    }

    #[test]
    fn g_and_capital_g_jump_to_the_ends_of_the_history() {
        let mut mode = browsing(5, 20, &format!("{}\r\n$ ", numbered(30)));
        type_keys(&mut mode, b"g");
        assert_eq!(top_and_cursor(&mode), (0, 0, 0));
        assert_eq!(mode.view().position, "[1/31]");
        type_keys(&mut mode, b"G");
        assert_eq!(top_and_cursor(&mode), (26, 30, 0));
        type_keys(&mut mode, b"k$");
        assert_eq!(top_and_cursor(&mode), (26, 29, 1));
    }

    #[test]
    fn capital_h_m_and_l_go_to_the_top_middle_and_bottom_of_the_view() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"H");
        assert_eq!(top_and_cursor(&mode), (25, 25, 0));
        type_keys(&mut mode, b"M");
        assert_eq!(top_and_cursor(&mode), (25, 27, 0));
        type_keys(&mut mode, b"L");
        assert_eq!(top_and_cursor(&mode), (25, 29, 0));

        let mut short = browsing(6, 20, "one\r\ntwo\r\nthree");
        type_keys(&mut short, b"Hj");
        assert_eq!(top_and_cursor(&short), (0, 1, 0));
        type_keys(&mut short, b"L");
        assert_eq!(top_and_cursor(&short), (0, 5, 0));
    }

    #[test]
    fn word_motions_move_the_cursor_across_lines() {
        let mut mode = browsing(4, 20, "echo foo-bar\r\nbaz");
        type_keys(&mut mode, b"gw");
        assert_eq!(top_and_cursor(&mode), (0, 0, 5));
        type_keys(&mut mode, b"2w");
        assert_eq!(top_and_cursor(&mode), (0, 0, 9));
        type_keys(&mut mode, b"w");
        assert_eq!(top_and_cursor(&mode), (0, 1, 0));
        type_keys(&mut mode, b"BE");
        assert_eq!(top_and_cursor(&mode), (0, 0, 11));
        type_keys(&mut mode, b"bbbe");
        assert_eq!(top_and_cursor(&mode), (0, 0, 7));
        type_keys(&mut mode, b"0W^");
        assert_eq!(top_and_cursor(&mode), (0, 0, 0));
    }

    #[test]
    fn the_cursor_never_rests_on_the_second_half_of_a_wide_character() {
        let mut mode = browsing(3, 10, "日本\r\nabcd");
        type_keys(&mut mode, b"gl");
        assert_eq!(top_and_cursor(&mode), (0, 0, 2));
        type_keys(&mut mode, b"jlk");
        assert_eq!(top_and_cursor(&mode), (0, 0, 2));
        type_keys(&mut mode, b"jllk");
        assert_eq!(top_and_cursor(&mode), (0, 0, 4));
        type_keys(&mut mode, b"jhk");
        assert_eq!(top_and_cursor(&mode), (0, 0, 2));
        type_keys(&mut mode, b"h");
        assert_eq!(top_and_cursor(&mode), (0, 0, 0));
    }

    #[test]
    fn resizing_keeps_the_view_and_cursor_inside_the_history() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"k");
        mode.resize(Size { rows: 10, cols: 2 });
        assert_eq!(top_and_cursor(&mode), (20, 28, 1));
        mode.resize(Size { rows: 3, cols: 20 });
        assert_eq!(top_and_cursor(&mode), (20, 22, 1));
        mode.resize(Size { rows: 50, cols: 20 });
        assert_eq!(top_and_cursor(&mode), (0, 22, 1));
        type_keys(&mut mode, b"L");
        assert_eq!(top_and_cursor(&mode), (0, 29, 0));
    }

    #[test]
    fn q_ctrl_c_and_escape_leave_and_r_asks_for_a_refresh() {
        for input in [&b"q"[..], b"\x03", b"\x1b[3~\x1b"] {
            let mut mode = browsing(5, 20, "text");
            let mut decoder = KeyDecoder::default();
            let bindings = Keymap::default().copy;
            let mut outcomes: Vec<Outcome> = decoder
                .feed(input)
                .iter()
                .map(|decoded| mode.press(decoded, &bindings))
                .collect();
            outcomes.extend(
                decoder
                    .time_out()
                    .map(|decoded| mode.press(&decoded, &bindings)),
            );
            assert_eq!(outcomes.last(), Some(&Outcome::Exit), "{input:?}");
        }
        assert_eq!(
            type_keys(&mut browsing(5, 20, "text"), b"r"),
            [Outcome::Refresh]
        );
    }

    #[test]
    fn a_refresh_keeps_the_distance_from_the_bottom() {
        let mut parser = vt100::Parser::new(5, 20, 1000);
        parser.process(numbered(30).as_bytes());
        let mut mode = CopyMode::new(
            Snapshot::new(parser.screen().clone()),
            Size { rows: 5, cols: 20 },
        );
        type_keys(&mut mode, b"\x15kv");
        assert_eq!(top_and_cursor(&mode), (23, 26, 2));
        parser.process(b"\r\n31\r\n32");
        mode.refresh(Snapshot::new(parser.screen().clone()));
        assert_eq!(top_and_cursor(&mode), (25, 28, 2));
        assert_eq!(mode.view().position, "[29/32]");
        assert!(mode.view().selection.is_empty());
    }

    #[test]
    fn pasted_text_is_ignored() {
        let mut mode = browsing(5, 20, &numbered(30));
        assert_eq!(type_keys(&mut mode, b"\x1b[200~qgk\x1b[201~"), []);
        assert_eq!(top_and_cursor(&mode), (25, 29, 2));
        type_keys(&mut mode, b"k");
        assert_eq!(top_and_cursor(&mode), (25, 28, 2));
    }

    #[test]
    fn unbound_keys_do_nothing_and_clear_the_count() {
        let mut mode = browsing(5, 20, &numbered(30));
        assert_eq!(type_keys(&mut mode, b"5zZ\x1b[1;5Pk"), []);
        assert_eq!(top_and_cursor(&mode), (25, 28, 2));
    }

    #[test]
    fn page_up_scrolls_a_page_less_one_line() {
        let mut mode = browsing(5, 20, &numbered(30));
        mode.page_up();
        assert_eq!(top_and_cursor(&mode), (21, 25, 2));
    }

    #[test]
    fn v_selects_from_the_cursor_and_y_copies_it_and_leaves() {
        let mut mode = browsing(5, 20, "one two\r\nthree four\r\n$ ");
        type_keys(&mut mode, b"kk0wv");
        assert_eq!(mode.view().selection, [(0, 4..5)]);
        type_keys(&mut mode, b"je");
        assert_eq!(top_and_cursor(&mode), (0, 1, 9));
        assert_eq!(mode.view().selection, [(0, 4..20), (1, 0..10)]);
        assert_eq!(type_keys(&mut mode, b"y"), copied("two\nthree four"));
    }

    #[test]
    fn a_selection_runs_backwards_and_o_swaps_its_ends() {
        let mut mode = browsing(5, 20, "one two\r\nthree four\r\n$ ");
        type_keys(&mut mode, b"k$ kb");
        assert_eq!(top_and_cursor(&mode), (0, 0, 4));
        assert_eq!(mode.view().selection, [(0, 4..20), (1, 0..10)]);
        type_keys(&mut mode, b"o");
        assert_eq!(top_and_cursor(&mode), (0, 1, 9));
        type_keys(&mut mode, b"h");
        assert_eq!(type_keys(&mut mode, b"\r"), copied("two\nthree fou"));
    }

    #[test]
    fn capital_v_selects_whole_lines_across_the_history() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"V6k");
        assert_eq!(top_and_cursor(&mode), (23, 23, 2));
        assert_eq!(
            mode.view().selection,
            (0..5).map(|row| (row, 0..20)).collect::<Vec<_>>()
        );
        assert_eq!(
            type_keys(&mut mode, b"y"),
            copied("24\n25\n26\n27\n28\n29\n30")
        );
    }

    #[test]
    fn a_selection_takes_wide_characters_whole() {
        let mut mode = browsing(3, 10, "a日本b\r\n");
        type_keys(&mut mode, b"glvl");
        assert_eq!(top_and_cursor(&mode), (0, 0, 3));
        assert_eq!(mode.view().selection, [(0, 1..5)]);
        assert_eq!(type_keys(&mut mode, b"y"), copied("日本"));
    }

    #[test]
    fn escape_clears_the_selection_and_then_leaves() {
        let mut mode = browsing(5, 20, "text");
        assert_eq!(type_keys_and_pause(&mut mode, b"hv\x1b"), []);
        assert!(mode.view().selection.is_empty());
        assert_eq!(type_keys_and_pause(&mut mode, b"\x1b"), [Outcome::Exit]);
        assert_eq!(
            type_keys(&mut browsing(5, 20, "text"), b"hvq"),
            [Outcome::Exit]
        );
    }

    #[test]
    fn yanking_nothing_or_blank_cells_leaves_without_copying() {
        assert_eq!(
            type_keys(&mut browsing(5, 20, "text"), b"y"),
            [Outcome::Exit]
        );
        assert_eq!(
            type_keys(&mut browsing(5, 20, "text"), b"vlly"),
            [Outcome::Exit]
        );
    }

    #[test]
    fn clear_selection_drops_the_selection_and_stays() {
        let mut mode = browsing(5, 20, "text");
        let mut bindings = Keymap::default().copy;
        bindings.insert(Key::char('c'), CopyAction::ClearSelection);
        let outcomes: Vec<Outcome> = KeyDecoder::default()
            .feed(b"0v$c")
            .iter()
            .map(|decoded| mode.press(decoded, &bindings))
            .collect();
        assert!(outcomes.iter().all(|outcome| *outcome == Outcome::Stay));
        assert!(mode.view().selection.is_empty());
        assert_eq!(type_keys(&mut mode, b"y"), [Outcome::Exit]);
    }
}
