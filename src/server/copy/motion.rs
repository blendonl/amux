use super::snapshot::Snapshot;
use super::Point;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Word,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punctuation,
}

pub fn next_word(snapshot: &Snapshot, cols: u16, from: Point, span: Span) -> Point {
    let walk = Walk {
        snapshot,
        cols,
        span,
    };
    let start = walk.class(from);
    let mut point = from;
    loop {
        let Some((next, broke)) = walk.next(point) else {
            return from;
        };
        point = next;
        if broke || walk.class(point) != start {
            break;
        }
    }
    walk.skip_blanks(point, Walk::next).unwrap_or(from)
}

pub fn next_word_end(snapshot: &Snapshot, cols: u16, from: Point, span: Span) -> Point {
    let walk = Walk {
        snapshot,
        cols,
        span,
    };
    let Some(point) = walk
        .next(from)
        .and_then(|(next, _)| walk.skip_blanks(next, Walk::next))
    else {
        return from;
    };
    walk.extend(point, Walk::next)
}

pub fn previous_word(snapshot: &Snapshot, cols: u16, from: Point, span: Span) -> Point {
    let walk = Walk {
        snapshot,
        cols,
        span,
    };
    let Some(point) = walk
        .previous(from)
        .and_then(|(previous, _)| walk.skip_blanks(previous, Walk::previous))
    else {
        return from;
    };
    walk.extend(point, Walk::previous)
}

pub fn first_non_blank(snapshot: &Snapshot, cols: u16, line: usize) -> u16 {
    non_blank(snapshot, cols, line).next().unwrap_or(0)
}

pub fn last_non_blank(snapshot: &Snapshot, cols: u16, line: usize) -> u16 {
    non_blank(snapshot, cols, line).last().unwrap_or(0)
}

fn non_blank(snapshot: &Snapshot, cols: u16, line: usize) -> impl Iterator<Item = u16> + '_ {
    (0..cols).filter(move |&col| {
        snapshot
            .cell(Point { line, col })
            .is_some_and(|cell| !cell.contents().trim().is_empty())
    })
}

struct Walk<'a> {
    snapshot: &'a Snapshot,
    cols: u16,
    span: Span,
}

impl Walk<'_> {
    fn class(&self, point: Point) -> Class {
        let first = self
            .snapshot
            .cell(point)
            .and_then(|cell| cell.contents().chars().next());
        match first {
            None => Class::Blank,
            Some(character) if character.is_whitespace() => Class::Blank,
            Some(_) if self.span == Span::Space => Class::Word,
            Some(character) if character.is_alphanumeric() || character == '_' => Class::Word,
            Some(_) => Class::Punctuation,
        }
    }

    fn next(&self, mut point: Point) -> Option<(Point, bool)> {
        let mut broke = false;
        loop {
            if point.col + 1 < self.cols {
                point.col += 1;
            } else if point.line + 1 < self.snapshot.lines() {
                broke |= !self.snapshot.wrapped(point.line);
                point = Point {
                    line: point.line + 1,
                    col: 0,
                };
            } else {
                return None;
            }
            if !self.snapshot.is_continuation(point) {
                return Some((point, broke));
            }
        }
    }

    fn previous(&self, mut point: Point) -> Option<(Point, bool)> {
        let mut broke = false;
        loop {
            if point.col > 0 {
                point.col = point.col.min(self.cols) - 1;
            } else if point.line > 0 {
                point = Point {
                    line: point.line - 1,
                    col: self.cols - 1,
                };
                broke |= !self.snapshot.wrapped(point.line);
            } else {
                return None;
            }
            if !self.snapshot.is_continuation(point) {
                return Some((point, broke));
            }
        }
    }

    fn skip_blanks(
        &self,
        mut point: Point,
        step: impl Fn(&Self, Point) -> Option<(Point, bool)>,
    ) -> Option<Point> {
        while self.class(point) == Class::Blank {
            point = step(self, point)?.0;
        }
        Some(point)
    }

    fn extend(
        &self,
        mut point: Point,
        step: impl Fn(&Self, Point) -> Option<(Point, bool)>,
    ) -> Point {
        let class = self.class(point);
        while let Some((next, false)) = step(self, point) {
            if self.class(next) != class {
                break;
            }
            point = next;
        }
        point
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLS: u16 = 12;

    fn snapshot(input: &str) -> Snapshot {
        let mut parser = vt100::Parser::new(4, COLS, 10);
        parser.process(input.as_bytes());
        Snapshot::new(parser.screen().clone())
    }

    fn at(line: usize, col: u16) -> Point {
        Point { line, col }
    }

    fn walk(snapshot: &Snapshot, from: Point, motion: impl Fn(Point) -> Point) -> Vec<Point> {
        let mut points = Vec::new();
        let mut point = from;
        loop {
            let next = motion(point);
            if next == point {
                return points;
            }
            points.push(next);
            point = next;
            assert!(points.len() < 100, "{:?}", snapshot.cursor());
        }
    }

    #[test]
    fn w_stops_at_words_and_punctuation_and_crosses_lines() {
        let text = snapshot("foo.bar  baz\r\n  (qux)");
        let stops = walk(&text, at(0, 0), |point| {
            next_word(&text, COLS, point, Span::Word)
        });
        assert_eq!(
            stops,
            [at(0, 3), at(0, 4), at(0, 9), at(1, 2), at(1, 3), at(1, 6)]
        );
    }

    #[test]
    fn capital_w_only_stops_after_blanks() {
        let text = snapshot("foo.bar  baz\r\n  (qux)");
        let stops = walk(&text, at(0, 0), |point| {
            next_word(&text, COLS, point, Span::Space)
        });
        assert_eq!(stops, [at(0, 9), at(1, 2)]);
    }

    #[test]
    fn e_and_capital_e_stop_at_word_ends() {
        let text = snapshot("foo.bar  baz\r\n  (qux)");
        let ends = walk(&text, at(0, 0), |point| {
            next_word_end(&text, COLS, point, Span::Word)
        });
        assert_eq!(
            ends,
            [
                at(0, 2),
                at(0, 3),
                at(0, 6),
                at(0, 11),
                at(1, 2),
                at(1, 5),
                at(1, 6)
            ]
        );
        let ends = walk(&text, at(0, 0), |point| {
            next_word_end(&text, COLS, point, Span::Space)
        });
        assert_eq!(ends, [at(0, 6), at(0, 11), at(1, 6)]);
    }

    #[test]
    fn b_and_capital_b_go_back_to_word_starts() {
        let text = snapshot("foo.bar  baz\r\n  (qux)");
        let starts = walk(&text, at(1, 6), |point| {
            previous_word(&text, COLS, point, Span::Word)
        });
        assert_eq!(
            starts,
            [at(1, 3), at(1, 2), at(0, 9), at(0, 4), at(0, 3), at(0, 0)]
        );
        let starts = walk(&text, at(1, 6), |point| {
            previous_word(&text, COLS, point, Span::Space)
        });
        assert_eq!(starts, [at(1, 2), at(0, 9), at(0, 0)]);
    }

    #[test]
    fn a_full_line_ends_its_word_unless_it_wraps() {
        let text = snapshot("abcdefghijkl\r\nmn\r\nopqrstuvwxyzAB");
        assert_eq!(next_word(&text, COLS, at(0, 0), Span::Word), at(1, 0));
        assert_eq!(next_word(&text, COLS, at(1, 0), Span::Word), at(2, 0));
        assert_eq!(next_word(&text, COLS, at(2, 0), Span::Word), at(2, 0));
        assert_eq!(next_word_end(&text, COLS, at(2, 0), Span::Word), at(3, 1));
        assert_eq!(previous_word(&text, COLS, at(3, 1), Span::Word), at(2, 0));
    }

    #[test]
    fn wide_characters_are_stepped_over_whole() {
        let text = snapshot("a 日本 b");
        assert_eq!(next_word(&text, COLS, at(0, 0), Span::Word), at(0, 2));
        assert_eq!(next_word_end(&text, COLS, at(0, 2), Span::Word), at(0, 4));
        assert_eq!(next_word(&text, COLS, at(0, 2), Span::Word), at(0, 7));
        assert_eq!(previous_word(&text, COLS, at(0, 7), Span::Word), at(0, 2));
    }

    #[test]
    fn line_motions_find_the_first_and_last_text() {
        let text = snapshot(" indented  \r\n\r\nx日");
        assert_eq!(first_non_blank(&text, COLS, 0), 1);
        assert_eq!(last_non_blank(&text, COLS, 0), 8);
        assert_eq!(first_non_blank(&text, COLS, 1), 0);
        assert_eq!(last_non_blank(&text, COLS, 1), 0);
        assert_eq!(last_non_blank(&text, COLS, 2), 1);
    }
}
