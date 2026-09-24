use super::draw::{self, Span, Style};
use crate::protocol::Size;

const DETACH_HINT: &str = "Ctrl-b d detaches";
const TEXT: Style = Style::PLAIN.bold();
const BORDER: Style = Style::PLAIN;
const PADDING: usize = 1;
const FRAME_COLUMNS: usize = 2 * (1 + PADDING);
const FRAME_ROWS: usize = 2;

pub fn render_reconnecting(server: &str, size: Size) -> Vec<u8> {
    let title = format!("reconnecting to {server}…");
    let rows = usize::from(size.rows);
    let cols = usize::from(size.cols);
    let mut out = Vec::new();
    if rows == 0 || cols == 0 {
        return out;
    }

    out.extend_from_slice(draw::SAVE_CURSOR);
    out.extend_from_slice(draw::HIDE_CURSOR);
    let lines: Vec<&str> = if rows >= 2 + FRAME_ROWS {
        vec![&title, DETACH_HINT]
    } else {
        vec![&title]
    };
    if rows >= lines.len() + FRAME_ROWS && cols > FRAME_COLUMNS {
        draw_box(&mut out, &lines, rows, cols);
    } else {
        draw_centered_line(&mut out, &title, rows, cols);
    }
    out.extend_from_slice(draw::RESTORE_CURSOR);
    out
}

fn draw_box(out: &mut Vec<u8>, lines: &[&str], rows: usize, cols: usize) {
    let inner = lines
        .iter()
        .map(|line| draw::width(line))
        .max()
        .unwrap_or(0)
        .min(cols - FRAME_COLUMNS);
    let box_width = inner + FRAME_COLUMNS;
    let top = (rows - lines.len() - FRAME_ROWS) / 2;
    let left = (cols - box_width) / 2;
    let horizontal = "─".repeat(box_width - 2);

    let edge = |out: &mut Vec<u8>, row: usize, corners: (char, char)| {
        let text = format!("{}{horizontal}{}", corners.0, corners.1);
        draw::draw_row(
            out,
            row,
            left,
            box_width,
            &[Span::new(text, BORDER)],
            BORDER,
        );
    };
    edge(out, top, ('┌', '┐'));
    for (offset, line) in lines.iter().enumerate() {
        let text = draw::truncate(line, inner);
        let slack = inner - draw::width(&text);
        let before = slack / 2;
        let spans = [
            Span::new(format!("│{}", " ".repeat(PADDING + before)), BORDER),
            Span::new(text, TEXT),
            Span::new(format!("{}│", " ".repeat(PADDING + slack - before)), BORDER),
        ];
        draw::draw_row(out, top + 1 + offset, left, box_width, &spans, BORDER);
    }
    edge(out, top + lines.len() + 1, ('└', '┘'));
}

fn draw_centered_line(out: &mut Vec<u8>, text: &str, rows: usize, cols: usize) {
    let text = draw::truncate(text, cols);
    let width = draw::width(&text);
    draw::draw_row(
        out,
        rows / 2,
        (cols - width) / 2,
        width,
        &[Span::new(text, TEXT)],
        TEXT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::chrome::testing::{screen_text, terminal};

    fn draw(server: &str, rows: u16, cols: u16) -> vt100::Parser {
        let mut parser = terminal(rows, cols);
        parser.process(b"shell$\x1b[1;4H");
        parser.process(&render_reconnecting(server, Size { rows, cols }));
        parser
    }

    #[test]
    fn a_centered_box_names_the_server_and_how_to_detach() {
        let parser = draw("desktop", 24, 80);
        let rows = screen_text(&parser);
        let pad = " ".repeat(26);

        assert_eq!(rows[0], "shell$");
        assert_eq!(rows[10], format!("{pad}┌{}┐", "─".repeat(26)));
        assert_eq!(rows[11], format!("{pad}│ reconnecting to desktop… │"));
        assert_eq!(rows[12], format!("{pad}│    Ctrl-b d detaches     │"));
        assert_eq!(rows[13], format!("{pad}└{}┘", "─".repeat(26)));
        assert!(rows[14].is_empty());
        assert!(parser.screen().cell(11, 28).unwrap().bold());
    }

    #[test]
    fn the_cursor_is_restored_hidden_with_default_attributes() {
        let mut parser = draw("desktop", 24, 80);
        assert_eq!(parser.screen().cursor_position(), (0, 3));
        assert!(parser.screen().hide_cursor());

        parser.process(b"x");
        let cell = parser.screen().cell(0, 3).unwrap();
        assert_eq!(cell.contents(), "x");
        assert!(!cell.bold());
    }

    #[test]
    fn a_small_terminal_cuts_the_text_inside_the_box() {
        let rows = screen_text(&draw("desktop", 5, 12));
        assert_eq!(
            rows,
            vec![
                "┌──────────┐",
                "│ reconne… │",
                "│ Ctrl-b … │",
                "└──────────┘",
                "",
            ]
        );
    }

    #[test]
    fn a_three_row_terminal_drops_the_hint() {
        let rows = screen_text(&draw("laptop", 3, 30));
        let horizontal = "─".repeat(25);
        assert_eq!(
            rows,
            vec![
                format!("s┌{horizontal}┐"),
                " │ reconnecting to laptop… │".to_owned(),
                format!(" └{horizontal}┘"),
            ]
        );
    }

    #[test]
    fn a_tiny_terminal_shows_only_the_text() {
        assert_eq!(
            screen_text(&draw("desktop", 2, 10)),
            vec!["shell$", "reconnect…"]
        );
        let mut single_cell = terminal(1, 1);
        single_cell.process(&render_reconnecting("desktop", Size { rows: 1, cols: 1 }));
        assert_eq!(screen_text(&single_cell), vec!["…"]);
        assert!(render_reconnecting("desktop", Size { rows: 0, cols: 0 }).is_empty());
    }

    #[test]
    fn wide_server_names_fit_the_box() {
        let rows = screen_text(&draw("東京", 4, 25));
        assert_eq!(rows[1], "│ reconnecting to 東京… │");

        let rows = screen_text(&draw("東京", 4, 22));
        assert_eq!(rows[1], "│ reconnecting to …  │");
        assert_eq!(rows[2], "│ Ctrl-b d detaches  │");
    }
}
