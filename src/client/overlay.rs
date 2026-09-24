use crate::protocol::Size;

pub fn reconnecting(server: &str, size: Size) -> Vec<u8> {
    let text: String = format!(" reconnecting to {server}… ")
        .chars()
        .take(usize::from(size.cols))
        .collect();
    let width = u16::try_from(text.chars().count()).unwrap_or(size.cols);
    let row = size.rows / 2 + 1;
    let col = size.cols.saturating_sub(width) / 2 + 1;
    format!("\x1b7\x1b[{row};{col}H\x1b[0;7m{text}\x1b[0m\x1b8").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(bytes: &[u8], size: Size) -> vt100::Parser {
        let mut screen = vt100::Parser::new(size.rows, size.cols, 0);
        screen.process(b"$ echo hi\r\nhi\r\n$ ");
        screen.process(bytes);
        screen
    }

    #[test]
    fn the_overlay_is_centered_and_keeps_the_cursor() {
        let size = Size { rows: 24, cols: 80 };
        let screen = draw(&reconnecting("desktop", size), size);

        let text = " reconnecting to desktop… ";
        let width = text.chars().count() as u16;
        let row = screen.screen().contents_between(12, 0, 12, 80);
        assert_eq!(
            row.trim_end(),
            format!("{}{text}", " ".repeat(usize::from((80 - width) / 2))).trim_end()
        );
        assert!(screen
            .screen()
            .cell(12, (80 - width) / 2)
            .unwrap()
            .inverse());
        assert_eq!(screen.screen().cursor_position(), (2, 2));
        assert!(screen.screen().contents().starts_with("$ echo hi\nhi"));
    }

    #[test]
    fn the_overlay_is_cut_to_a_narrow_terminal() {
        let size = Size { rows: 2, cols: 10 };
        let screen = draw(&reconnecting("desktop", size), size);
        assert_eq!(screen.screen().contents_between(1, 0, 1, 10), " reconnect");
    }
}
