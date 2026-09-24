pub fn terminal(rows: u16, cols: u16) -> vt100::Parser {
    vt100::Parser::new(rows, cols, 0)
}

pub fn row_text(parser: &vt100::Parser, row: u16) -> String {
    screen_text(parser).swap_remove(usize::from(row))
}

pub fn screen_text(parser: &vt100::Parser) -> Vec<String> {
    let screen = parser.screen();
    screen
        .rows(0, screen.size().1)
        .map(|row| row.trim_end().to_owned())
        .collect()
}
