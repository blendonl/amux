fn ids(parser: &vt100::Parser) -> Vec<u64> {
    parser.screen().row_ids().collect()
}

fn assert_fresh(ids: &[u64], old: &[u64]) {
    for (i, id) in ids.iter().enumerate() {
        assert!(!old.contains(id), "{id} was already in use");
        assert!(!ids[i + 1..].contains(id), "{id} appears twice");
    }
}

fn parser(rows: u16, input: &str) -> vt100::Parser {
    let mut parser = vt100::Parser::new(rows, 10, 10);
    parser.process(input.as_bytes());
    parser
}

#[test]
fn a_new_screen_gives_every_row_its_own_id() {
    let parser = parser(5, "");
    let ids = ids(&parser);
    assert_eq!(ids.len(), 5);
    assert_fresh(&ids, &[]);
}

#[test]
fn a_linefeed_at_the_bottom_moves_ids_into_the_scrollback() {
    let mut parser = parser(3, "\x1b[3;1H");
    let before = ids(&parser);
    parser.process(b"\n\n");
    let after = ids(&parser);
    assert_eq!(after[0], before[2]);
    assert_fresh(&after[1..], &before);

    parser.screen_mut().set_scrollback(1);
    assert_eq!(ids(&parser), [before[1], before[2], after[1]]);
}

#[test]
fn scroll_up_and_down_keep_the_ids_of_rows_that_stay() {
    let mut parser = parser(4, "");
    let before = ids(&parser);
    parser.process(b"\x1b[2S");
    let up = ids(&parser);
    assert_eq!(up[..2], before[2..]);
    assert_fresh(&up[2..], &before);

    parser.process(b"\x1b[3T");
    let down = ids(&parser);
    assert_eq!(down[3], up[0]);
    assert_fresh(&down[..3], &[before.clone(), up].concat());
}

#[test]
fn inserted_lines_are_fresh_and_push_the_bottom_row_out() {
    let mut parser = parser(4, "\x1b[2;1H");
    let before = ids(&parser);
    parser.process(b"\x1b[L");
    let after = ids(&parser);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[2..], before[1..3]);
    assert_fresh(&after[1..2], &before);
}

#[test]
fn deleted_lines_pull_rows_up_and_add_fresh_ones_at_the_bottom() {
    let mut parser = parser(4, "\x1b[2;1H");
    let before = ids(&parser);
    parser.process(b"\x1b[2M");
    let after = ids(&parser);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[3]);
    assert_fresh(&after[2..], &before);
}

#[test]
fn rows_scrolled_out_of_a_region_are_destroyed() {
    let mut parser = parser(5, "\x1b[2;4r\x1b[4;1H");
    let before = ids(&parser);
    parser.process(b"\n");
    let after = ids(&parser);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1..3], before[2..4]);
    assert_eq!(after[4], before[4]);
    assert_fresh(&after[3..4], &before);

    parser.screen_mut().set_scrollback(usize::MAX);
    assert_eq!(parser.screen().scrollback(), 0);
}

#[test]
fn growing_adds_fresh_rows_and_shrinking_keeps_the_top_ones() {
    let mut parser = parser(3, "");
    let before = ids(&parser);
    parser.screen_mut().set_size(6, 10);
    let grown = ids(&parser);
    assert_eq!(grown[..3], before);
    assert_fresh(&grown[3..], &before);

    parser.screen_mut().set_size(2, 10);
    assert_eq!(ids(&parser), before[..2]);
}

#[test]
fn the_alternate_screen_has_its_own_ids() {
    let mut parser = parser(3, "");
    let main = ids(&parser);
    parser.process(b"\x1b[?1049h");
    let alternate = ids(&parser);
    assert_fresh(&alternate, &main);

    parser.process(b"\x1b[?1049l");
    assert_eq!(ids(&parser), main);
}

#[test]
fn a_reset_gives_every_row_a_fresh_id() {
    let mut parser = parser(3, "");
    let before = ids(&parser);
    parser.process(b"\x1bc");
    assert_fresh(&ids(&parser), &before);
}

#[test]
fn erasing_and_moving_the_cursor_keep_ids() {
    let mut parser = parser(3, "hello\r\nworld");
    let before = ids(&parser);
    parser.process(b"\x1b[2J\x1b[H\x1b[2K\x1b[3;1Hx");
    assert_eq!(ids(&parser), before);
}

fn scrolled(
    before: &[u64],
    region: std::ops::RangeInclusive<usize>,
    count: usize,
    up: bool,
) -> Vec<Option<u64>> {
    let mut expected: Vec<Option<u64>> =
        before.iter().copied().map(Some).collect();
    let rows = &mut expected[region];
    let count = count.min(rows.len());
    if up {
        rows.rotate_left(count);
        let kept = rows.len() - count;
        rows[kept..].fill(None);
    } else {
        rows.rotate_right(count);
        rows[..count].fill(None);
    }
    expected
}

#[test]
fn scrolling_a_region_moves_only_the_ids_inside_it() {
    for (top, bottom) in [(0, 2), (1, 3), (2, 5), (0, 5)] {
        for count in 1..=4 {
            for (code, up) in [('S', true), ('T', false)] {
                let mut parser =
                    parser(6, &format!("\x1b[{};{}r", top + 1, bottom + 1));
                let before = ids(&parser);
                parser.process(format!("\x1b[{count}{code}").as_bytes());
                let after = ids(&parser);
                let expected = scrolled(&before, top..=bottom, count, up);
                let case = format!("{count}{code} in {top}..={bottom}");
                let fresh: Vec<u64> = after
                    .iter()
                    .zip(&expected)
                    .filter(|(_, expected)| expected.is_none())
                    .map(|(&id, _)| id)
                    .collect();
                assert_fresh(&fresh, &before);
                for (row, (id, expected)) in
                    after.iter().zip(&expected).enumerate()
                {
                    if let Some(expected) = expected {
                        assert_eq!(id, expected, "{case}: row {row}");
                    }
                }
            }
        }
    }
}
