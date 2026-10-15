use unicode_width::UnicodeWidthChar;

use super::snapshot::Snapshot;
use super::{CopyMode, Point};
use crate::keys::Decoded;
use crate::line_editor::{Finish, LineEditor};
use crate::server::render::StatusLine;
use crate::settings::{PromptAction, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

impl Direction {
    fn reversed(self) -> Self {
        match self {
            Self::Forward => Self::Backward,
            Self::Backward => Self::Forward,
        }
    }

    fn sign(self) -> char {
        match self {
            Self::Forward => '/',
            Self::Backward => '?',
        }
    }

    fn wrap_message(self) -> &'static str {
        match self {
            Self::Forward => "search hit BOTTOM, continuing at TOP",
            Self::Backward => "search hit TOP, continuing at BOTTOM",
        }
    }
}

pub struct Prompt {
    direction: Direction,
    count: usize,
    line: LineEditor,
}

impl Prompt {
    pub fn paste(&mut self, raw: &[u8]) {
        self.line.insert(&String::from_utf8_lossy(raw));
    }
}

pub struct Search {
    pattern: String,
    direction: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Found {
    at: Point,
    wrapped: bool,
}

struct Needle {
    text: String,
    fold: bool,
}

impl Needle {
    fn new(pattern: &str) -> Self {
        let fold = !pattern.chars().any(char::is_uppercase);
        let text = if fold {
            pattern.to_lowercase()
        } else {
            pattern.to_owned()
        };
        Self { text, fold }
    }

    fn columns(&self, snapshot: &Snapshot, cols: u16, line: usize) -> Vec<u16> {
        let mut haystack = String::new();
        let mut starts = Vec::new();
        for col in 0..cols {
            let Some(cell) = snapshot.cell(Point { line, col }) else {
                break;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            starts.push((haystack.len(), col));
            match cell.contents() {
                "" => haystack.push(' '),
                text if self.fold => haystack.push_str(&text.to_lowercase()),
                text => haystack.push_str(text),
            }
        }
        let mut columns = Vec::new();
        let mut from = 0;
        while let Some(offset) = haystack[from..].find(&self.text) {
            let at = from + offset;
            let cell = starts.partition_point(|&(start, _)| start <= at) - 1;
            let col = starts[cell].1;
            if columns.last() != Some(&col) {
                columns.push(col);
            }
            from = at + haystack[at..].chars().next().map_or(1, char::len_utf8);
        }
        columns
    }
}

fn find(
    snapshot: &Snapshot,
    cols: u16,
    from: Point,
    needle: &Needle,
    direction: Direction,
) -> Option<Found> {
    let lines = snapshot.lines();
    let columns = |line| needle.columns(snapshot, cols, line);
    match direction {
        Direction::Forward => (from.line..lines)
            .chain(0..=from.line)
            .enumerate()
            .find_map(|(step, line)| {
                let found = columns(line);
                let col = if step == 0 {
                    found.into_iter().find(|&col| col > from.col)
                } else {
                    found.first().copied()
                };
                col.map(|col| Found {
                    at: Point { line, col },
                    wrapped: step >= lines - from.line,
                })
            }),
        Direction::Backward => (0..=from.line)
            .rev()
            .chain((from.line..lines).rev())
            .enumerate()
            .find_map(|(step, line)| {
                let found = columns(line);
                let col = if step == 0 {
                    found.into_iter().rev().find(|&col| col < from.col)
                } else {
                    found.last().copied()
                };
                col.map(|col| Found {
                    at: Point { line, col },
                    wrapped: step > from.line,
                })
            }),
    }
}

fn find_nth(
    snapshot: &Snapshot,
    cols: u16,
    from: Point,
    needle: &Needle,
    direction: Direction,
    count: usize,
) -> Option<Found> {
    let mut steps: Vec<Found> = Vec::new();
    let mut at = from;
    while steps.len() < count {
        let found = find(snapshot, cols, at, needle, direction)?;
        if steps.first().is_some_and(|first| first.at == found.at) {
            return Some(Found {
                at: steps[(count - 1) % steps.len()].at,
                wrapped: true,
            });
        }
        at = found.at;
        steps.push(found);
    }
    let wrapped = steps.iter().any(|step| step.wrapped);
    steps.last().map(|last| Found { wrapped, ..*last })
}

impl CopyMode {
    pub(super) fn open_prompt(&mut self, direction: Direction, count: usize) {
        self.prompt = Some(Prompt {
            direction,
            count,
            line: LineEditor::default(),
        });
    }

    pub(super) fn edit_prompt(&mut self, decoded: &Decoded, bindings: &Table<PromptAction>) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        match prompt.line.press(decoded, bindings) {
            None => {}
            Some(Finish::Cancel) => self.prompt = None,
            Some(Finish::Submit) => {
                let search = Search {
                    pattern: prompt.line.text(),
                    direction: prompt.direction,
                };
                let count = prompt.count;
                self.prompt = None;
                if !search.pattern.is_empty() {
                    self.jump(&search.pattern, search.direction, count);
                    self.search = Some(search);
                }
            }
        }
    }

    pub(super) fn search_again(&mut self, count: usize, reverse: bool) {
        let Some(search) = self.search.take() else {
            return;
        };
        let direction = if reverse {
            search.direction.reversed()
        } else {
            search.direction
        };
        self.jump(&search.pattern, direction, count);
        self.search = Some(search);
    }

    pub(super) fn status(&self) -> Option<StatusLine> {
        let Some(prompt) = &self.prompt else {
            return self
                .message
                .clone()
                .map(|text| StatusLine { text, cursor: None });
        };
        let columns = usize::from(self.size.cols).saturating_sub(1);
        let (text, cursor) = prompt
            .line
            .visible(columns, |character| character.width().unwrap_or(0));
        Some(StatusLine {
            text: format!("{}{text}", prompt.direction.sign()),
            cursor: Some(u16::try_from(cursor + 1).unwrap_or(u16::MAX)),
        })
    }

    fn jump(&mut self, pattern: &str, direction: Direction, count: usize) {
        let needle = Needle::new(pattern);
        let Some(found) = find_nth(
            &self.snapshot,
            self.size.cols,
            self.cursor,
            &needle,
            direction,
            count,
        ) else {
            self.message = Some(format!("pattern not found: {pattern}"));
            return;
        };
        if found.wrapped {
            self.message = Some(direction.wrap_message().to_owned());
        }
        self.cursor = found.at;
        if !(self.top..self.top + self.rows()).contains(&self.cursor.line) {
            self.top = self.cursor.line.saturating_sub((self.rows() - 1) / 2);
        }
        self.clamp();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyDecoder;
    use crate::protocol::Size;
    use crate::server::copy::Outcome;
    use crate::settings::Keymap;

    fn snapshot(rows: u16, cols: u16, input: &str) -> Snapshot {
        let mut parser = vt100::Parser::new(rows, cols, 100);
        parser.process(input.as_bytes());
        Snapshot::new(parser.screen().clone())
    }

    fn at(line: usize, col: u16) -> Point {
        Point { line, col }
    }

    fn search(text: &Snapshot, from: Point, pattern: &str, direction: Direction) -> Option<Found> {
        find(text, 20, from, &Needle::new(pattern), direction)
    }

    fn found(line: usize, col: u16, wrapped: bool) -> Option<Found> {
        Some(Found {
            at: at(line, col),
            wrapped,
        })
    }

    fn browsing(rows: u16, cols: u16, input: &str) -> CopyMode {
        CopyMode::new(snapshot(rows, cols, input), Size { rows, cols })
    }

    fn type_keys(mode: &mut CopyMode, input: &[u8]) -> Vec<Outcome> {
        let keymap = Keymap::default();
        let mut decoder = KeyDecoder::default();
        let mut decoded = decoder.feed(input);
        decoded.extend(decoder.time_out());
        decoded
            .iter()
            .map(|decoded| mode.press(decoded, &keymap))
            .filter(|outcome| *outcome != Outcome::Stay)
            .collect()
    }

    fn cursor(mode: &CopyMode) -> (usize, usize, u16) {
        (mode.top, mode.cursor.line, mode.cursor.col)
    }

    fn status(mode: &CopyMode) -> Option<(String, Option<u16>)> {
        mode.view()
            .status
            .map(|status| (status.text, status.cursor))
    }

    fn message(text: &str) -> Option<(String, Option<u16>)> {
        Some((text.into(), None))
    }

    fn numbered(count: usize) -> String {
        (1..=count)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\r\n")
    }

    #[test]
    fn a_lowercase_pattern_ignores_case_and_any_capital_makes_it_exact() {
        let text = snapshot(4, 20, "Foo foo FOO fOo");
        let all = Needle::new("foo");
        assert_eq!(all.columns(&text, 20, 0), [0, 4, 8, 12]);
        assert_eq!(Needle::new("Foo").columns(&text, 20, 0), [0]);
        assert_eq!(Needle::new("FOO").columns(&text, 20, 0), [8]);
        assert_eq!(Needle::new("o f").columns(&text, 20, 0), [2, 6, 10]);
        assert!(Needle::new("bar").columns(&text, 20, 1).is_empty());
    }

    #[test]
    fn overlapping_matches_each_count() {
        let text = snapshot(2, 20, "aaaa");
        assert_eq!(Needle::new("aa").columns(&text, 20, 0), [0, 1, 2]);
    }

    #[test]
    fn a_match_after_wide_characters_starts_at_its_display_column() {
        let text = snapshot(2, 20, "日本 x日y\u{e9}z");
        assert_eq!(Needle::new("x").columns(&text, 20, 0), [5]);
        assert_eq!(Needle::new("日y").columns(&text, 20, 0), [6]);
        assert!(Needle::new("\u{c9}Z").columns(&text, 20, 0).is_empty());
        assert_eq!(Needle::new("\u{e9}z").columns(&text, 20, 0), [9]);
        assert_eq!(Needle::new("本").columns(&text, 3, 0), [2]);
        assert!(Needle::new("x").columns(&text, 5, 0).is_empty());
    }

    #[test]
    fn searching_forward_starts_after_the_cursor_and_wraps_to_the_top() {
        let text = snapshot(4, 20, "ab ab\r\nxx\r\nab");
        assert_eq!(
            search(&text, at(0, 0), "ab", Direction::Forward),
            found(0, 3, false)
        );
        assert_eq!(
            search(&text, at(0, 3), "ab", Direction::Forward),
            found(2, 0, false)
        );
        assert_eq!(
            search(&text, at(2, 0), "ab", Direction::Forward),
            found(0, 0, true)
        );
        assert_eq!(
            search(&text, at(1, 0), "xx", Direction::Forward),
            found(1, 0, true)
        );
    }

    #[test]
    fn searching_backward_starts_before_the_cursor_and_wraps_to_the_bottom() {
        let text = snapshot(4, 20, "ab ab\r\nxx\r\nab");
        assert_eq!(
            search(&text, at(2, 0), "ab", Direction::Backward),
            found(0, 3, false)
        );
        assert_eq!(
            search(&text, at(0, 3), "ab", Direction::Backward),
            found(0, 0, false)
        );
        assert_eq!(
            search(&text, at(0, 0), "ab", Direction::Backward),
            found(2, 0, true)
        );
        assert_eq!(
            search(&text, at(1, 0), "xx", Direction::Backward),
            found(1, 0, true)
        );
    }

    #[test]
    fn a_pattern_that_is_nowhere_is_not_found() {
        let text = snapshot(4, 20, "ab ab\r\nxx\r\nab");
        assert_eq!(search(&text, at(1, 0), "abc", Direction::Forward), None);
        assert_eq!(search(&text, at(1, 0), "abc", Direction::Backward), None);
    }

    #[test]
    fn slash_searches_from_the_cursor_and_centres_a_match_out_of_view() {
        let mut mode = browsing(5, 20, &numbered(30));
        assert_eq!(cursor(&mode), (25, 29, 7));
        type_keys(&mut mode, b"?line 12\r");
        assert_eq!(cursor(&mode), (9, 11, 0));
        assert_eq!(mode.view().position, "[12/30]");
        assert_eq!(status(&mode), None);
        type_keys(&mut mode, b"/e 1\r");
        assert_eq!(cursor(&mode), (9, 11, 3));
        type_keys(&mut mode, b"/LINE\r");
        assert_eq!(cursor(&mode), (9, 11, 3));
        assert_eq!(status(&mode), message("pattern not found: LINE"));
    }

    #[test]
    fn a_search_that_wraps_says_so_until_the_next_key() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"/line 2\r");
        assert_eq!(cursor(&mode), (0, 1, 0));
        assert_eq!(
            status(&mode),
            message("search hit BOTTOM, continuing at TOP")
        );
        type_keys(&mut mode, b"j");
        assert_eq!(status(&mode), None);
        type_keys(&mut mode, b"gk?line 30\r");
        assert_eq!(cursor(&mode), (25, 29, 0));
        assert_eq!(
            status(&mode),
            message("search hit TOP, continuing at BOTTOM")
        );
    }

    #[test]
    fn n_repeats_the_search_and_capital_n_reverses_it_both_with_a_count() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"?line 1\r");
        assert_eq!(cursor(&mode), (16, 18, 0));
        assert_eq!(mode.view().position, "[19/30]");
        type_keys(&mut mode, b"n");
        assert_eq!(mode.view().position, "[18/30]");
        type_keys(&mut mode, b"3n");
        assert_eq!(mode.view().position, "[15/30]");
        type_keys(&mut mode, b"N");
        assert_eq!(mode.view().position, "[16/30]");
        type_keys(&mut mode, b"4N");
        assert_eq!(mode.view().position, "[1/30]");
        assert_eq!(
            status(&mode),
            message("search hit BOTTOM, continuing at TOP")
        );
        type_keys(&mut mode, b"2/line 3\r");
        assert_eq!(mode.view().position, "[30/30]");
        type_keys(&mut mode, b"25n");
        assert_eq!(mode.view().position, "[3/30]");
        assert_eq!(
            status(&mode),
            message("search hit BOTTOM, continuing at TOP")
        );
    }

    #[test]
    fn n_without_a_search_does_nothing() {
        let mut mode = browsing(5, 20, &numbered(30));
        assert_eq!(type_keys(&mut mode, b"nN"), []);
        assert_eq!(cursor(&mode), (25, 29, 7));
        assert_eq!(status(&mode), None);
    }

    #[test]
    fn the_prompt_shows_the_pattern_with_the_cursor_and_edits_it() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"/lime");
        assert_eq!(status(&mode), Some(("/lime".into(), Some(5))));
        type_keys(&mut mode, b"\x7f\x7fne\x1b[D\x1b[D");
        assert_eq!(status(&mode), Some(("/line".into(), Some(3))));
        type_keys(&mut mode, b"\x15?");
        assert_eq!(status(&mode), Some(("/?".into(), Some(2))));
        type_keys(&mut mode, b"\x7fline 7\r");
        assert_eq!(cursor(&mode), (4, 6, 0));
        type_keys(&mut mode, b"?");
        assert_eq!(status(&mode), Some(("?".into(), Some(1))));
        type_keys(&mut mode, b"\r");
        assert_eq!(status(&mode), None);
        assert_eq!(cursor(&mode), (4, 6, 0));
    }

    #[test]
    fn a_long_pattern_scrolls_inside_the_pane() {
        let mut mode = browsing(5, 8, "");
        type_keys(&mut mode, "/abcdef日本".as_bytes());
        assert_eq!(status(&mode), Some(("/ef日本".into(), Some(7))));
    }

    #[test]
    fn escape_closes_only_the_prompt() {
        let mut mode = browsing(5, 20, &numbered(30));
        type_keys(&mut mode, b"vk/line\x1b");
        assert_eq!(status(&mode), None);
        assert_eq!(mode.view().selection, [(3, 7..20), (4, 0..8)]);
        assert_eq!(type_keys(&mut mode, b"\x03"), [Outcome::Exit]);
    }

    #[test]
    fn a_paste_is_typed_into_the_prompt() {
        let mut mode = browsing(5, 20, &numbered(30));
        assert_eq!(type_keys(&mut mode, b"?\x1b[200~line\r\n 4q\x1b[201~"), []);
        assert_eq!(status(&mode), Some(("?line 4q".into(), Some(8))));
        type_keys(&mut mode, b"\x7f\r");
        assert_eq!(mode.view().position, "[4/30]");
    }

    #[test]
    fn a_search_extends_the_selection_to_the_match() {
        let mut mode = browsing(5, 20, "one two\r\nthree four\r\n$ ");
        type_keys(&mut mode, b"gv/our\r");
        assert_eq!(cursor(&mode), (0, 1, 7));
        assert_eq!(mode.view().selection, [(0, 0..20), (1, 0..8)]);
        assert_eq!(
            type_keys(&mut mode, b"y"),
            [Outcome::Copy("one two\nthree fo".into())]
        );
    }
}
