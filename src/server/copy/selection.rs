use std::ops::Range;

use super::snapshot::Snapshot;
use super::Point;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Char,
    Line,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Point,
    pub kind: Kind,
}

impl Selection {
    pub fn columns(&self, snapshot: &Snapshot, cursor: Point, line: usize) -> Option<Range<u16>> {
        let (start, end) = self.ends(snapshot, cursor);
        (start.line..=end.line)
            .contains(&line)
            .then(|| span(start, end, line))
    }

    pub fn text(&self, snapshot: &Snapshot, cursor: Point) -> String {
        let (start, end) = self.ends(snapshot, cursor);
        let mut text = String::new();
        for line in start.line..=end.line {
            let cols = span(start, end, line);
            let wrapped = snapshot.wrapped(line);
            let part = snapshot.text(line, cols.clone());
            let ends_the_line = !wrapped && is_blank(&snapshot.text(line, cols.end..u16::MAX));
            text.push_str(if ends_the_line {
                part.trim_end()
            } else {
                &part
            });
            if line < end.line && !wrapped {
                text.push('\n');
            }
        }
        text
    }

    fn ends(&self, snapshot: &Snapshot, cursor: Point) -> (Point, Point) {
        let (first, last) = if self.anchor <= cursor {
            (self.anchor, cursor)
        } else {
            (cursor, self.anchor)
        };
        match self.kind {
            Kind::Line => (
                Point { col: 0, ..first },
                Point {
                    col: u16::MAX,
                    ..last
                },
            ),
            Kind::Char => {
                let start = if first.col > 0 && snapshot.is_continuation(first) {
                    first.col - 1
                } else {
                    first.col
                };
                let width = if snapshot.cell(last).is_some_and(vt100::Cell::is_wide) {
                    2
                } else {
                    1
                };
                (
                    Point {
                        col: start,
                        ..first
                    },
                    Point {
                        col: last.col.saturating_add(width),
                        ..last
                    },
                )
            }
        }
    }
}

fn span(start: Point, end: Point, line: usize) -> Range<u16> {
    let from = if line == start.line { start.col } else { 0 };
    let to = if line == end.line { end.col } else { u16::MAX };
    from..to
}

fn is_blank(text: &str) -> bool {
    text.trim_end().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(rows: u16, cols: u16, input: &str) -> Snapshot {
        let mut parser = vt100::Parser::new(rows, cols, 100);
        parser.process(input.as_bytes());
        Snapshot::new(parser.screen().clone())
    }

    fn at(line: usize, col: u16) -> Point {
        Point { line, col }
    }

    fn chars_from(anchor: Point) -> Selection {
        Selection {
            anchor,
            kind: Kind::Char,
        }
    }

    fn lines_from(anchor: Point) -> Selection {
        Selection {
            anchor,
            kind: Kind::Line,
        }
    }

    #[test]
    fn a_character_selection_takes_both_ends_in_either_direction() {
        let text = snapshot(4, 20, "one two\r\nthree four\r\nfive");
        assert_eq!(chars_from(at(0, 4)).text(&text, at(1, 4)), "two\nthree");
        assert_eq!(chars_from(at(1, 4)).text(&text, at(0, 4)), "two\nthree");
        assert_eq!(chars_from(at(1, 6)).text(&text, at(1, 8)), "fou");
        assert_eq!(chars_from(at(1, 8)).text(&text, at(1, 6)), "fou");
        assert_eq!(
            chars_from(at(0, 0)).text(&text, at(2, 1)),
            "one two\nthree four\nfi"
        );
    }

    #[test]
    fn a_line_selection_takes_whole_lines_in_either_direction() {
        let text = snapshot(4, 20, "one two\r\nthree four\r\nfive");
        assert_eq!(
            lines_from(at(0, 5)).text(&text, at(1, 2)),
            "one two\nthree four"
        );
        assert_eq!(
            lines_from(at(1, 2)).text(&text, at(0, 5)),
            "one two\nthree four"
        );
        assert_eq!(lines_from(at(2, 3)).text(&text, at(2, 0)), "five");
    }

    #[test]
    fn wide_characters_are_taken_whole_at_both_ends() {
        let text = snapshot(2, 20, "a日本b");
        assert_eq!(chars_from(at(0, 2)).text(&text, at(0, 3)), "日本");
        assert_eq!(chars_from(at(0, 3)).text(&text, at(0, 1)), "日本");
        assert_eq!(chars_from(at(0, 1)).columns(&text, at(0, 3), 0), Some(1..5));
        assert_eq!(chars_from(at(0, 4)).columns(&text, at(0, 4), 0), Some(3..5));
    }

    #[test]
    fn a_wrapped_line_joins_the_next_without_a_newline() {
        let long = "x".repeat(70) + &"y".repeat(30);
        let text = snapshot(4, 80, &format!("{long}\r\nnext"));
        assert!(text.wrapped(0));
        assert_eq!(
            lines_from(at(0, 0)).text(&text, at(2, 0)),
            format!("{long}\nnext")
        );
        assert_eq!(chars_from(at(0, 75)).text(&text, at(1, 1)), "yyyyyyy");
    }

    #[test]
    fn blanks_at_the_end_of_a_line_are_trimmed_but_not_inside_it() {
        let text = snapshot(4, 20, "trailing   \r\na b\r\n  ");
        assert_eq!(
            lines_from(at(0, 0)).text(&text, at(2, 0)),
            "trailing\na b\n"
        );
        assert_eq!(chars_from(at(1, 0)).text(&text, at(1, 1)), "a ");
        assert_eq!(chars_from(at(0, 4)).text(&text, at(0, 15)), "ling");
    }

    #[test]
    fn history_rows_narrower_than_the_view_read_as_far_as_they_go() {
        let mut parser = vt100::Parser::new(2, 4, 100);
        parser.process(b"abcdefgh\r\nij\r\nkl");
        parser.screen_mut().set_size(2, 8);
        let text = Snapshot::new(parser.screen().clone());
        assert_eq!(chars_from(at(0, 2)).text(&text, at(1, 6)), "cdefgh");
        assert_eq!(chars_from(at(1, 6)).text(&text, at(3, 7)), "\nij\nkl");
        assert_eq!(
            lines_from(at(0, 0)).text(&text, at(3, 0)),
            "abcdefgh\nij\nkl"
        );
    }

    #[test]
    fn a_selection_of_blank_cells_is_empty() {
        let text = snapshot(4, 20, "word\r\n\r\n");
        assert_eq!(chars_from(at(0, 9)).text(&text, at(0, 12)), "");
        assert_eq!(chars_from(at(1, 3)).text(&text, at(1, 3)), "");
        assert_eq!(lines_from(at(1, 3)).text(&text, at(1, 3)), "");
    }

    #[test]
    fn the_columns_cover_the_selected_part_of_each_line() {
        let text = snapshot(4, 20, "one\r\ntwo\r\nthree");
        let selection = chars_from(at(2, 1));
        assert_eq!(selection.columns(&text, at(0, 2), 0), Some(2..u16::MAX));
        assert_eq!(selection.columns(&text, at(0, 2), 1), Some(0..u16::MAX));
        assert_eq!(selection.columns(&text, at(0, 2), 2), Some(0..2));
        assert_eq!(selection.columns(&text, at(0, 2), 3), None);
        assert_eq!(
            lines_from(at(1, 1)).columns(&text, at(1, 1), 1),
            Some(0..u16::MAX)
        );
    }
}
