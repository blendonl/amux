use vt100::{Color, MouseProtocolEncoding, MouseProtocolMode};

use super::grid::Style;
use super::InputModes;

pub const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
pub const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
pub const ERASE_LINE: &[u8] = b"\x1b[K";

const ALL_MOUSE_MODES_OFF: &[u8] = b"\x1b[?9;1000;1002;1003l";
const ALL_MOUSE_ENCODINGS_OFF: &[u8] = b"\x1b[?1005;1006l";

pub fn move_cursor(out: &mut Vec<u8>, from: Option<(u16, u16)>, (row, col): (u16, u16)) {
    match from {
        Some(from) if from == (row, col) => {}
        Some((from_row, _)) if from_row == row && col == 0 => out.push(b'\r'),
        Some((from_row, from_col)) if from_row == row && from_col < col => {
            csi(out, &[col - from_col], b'C');
        }
        _ if col == 0 => csi(out, &[row + 1], b'H'),
        _ => csi(out, &[row + 1, col + 1], b'H'),
    }
}

pub fn erase_chars(out: &mut Vec<u8>, count: u16) {
    csi(out, &[count], b'X');
}

pub fn set_style(out: &mut Vec<u8>, from: Option<Style>, to: Style) {
    let absolute = absolute_style(to);
    let params = match from {
        Some(from) => {
            let relative = relative_style(from, to);
            if encoded_len(&relative) <= encoded_len(&absolute) {
                relative
            } else {
                absolute
            }
        }
        None => absolute,
    };
    csi(out, &params, b'm');
}

pub fn set_modes(out: &mut Vec<u8>, from: Option<InputModes>, to: InputModes) {
    toggle(
        out,
        from.map(|modes| modes.application_cursor),
        to.application_cursor,
        b"\x1b[?1h",
        b"\x1b[?1l",
    );
    toggle(
        out,
        from.map(|modes| modes.application_keypad),
        to.application_keypad,
        b"\x1b=",
        b"\x1b>",
    );
    toggle(
        out,
        from.map(|modes| modes.bracketed_paste),
        to.bracketed_paste,
        b"\x1b[?2004h",
        b"\x1b[?2004l",
    );
    switch(
        out,
        from.map(|modes| mouse_mode(modes.mouse_protocol_mode)),
        mouse_mode(to.mouse_protocol_mode),
        ALL_MOUSE_MODES_OFF,
    );
    switch(
        out,
        from.map(|modes| mouse_encoding(modes.mouse_protocol_encoding)),
        mouse_encoding(to.mouse_protocol_encoding),
        ALL_MOUSE_ENCODINGS_OFF,
    );
}

fn toggle(out: &mut Vec<u8>, from: Option<bool>, to: bool, on: &[u8], off: &[u8]) {
    if from != Some(to) {
        out.extend_from_slice(if to { on } else { off });
    }
}

fn switch(out: &mut Vec<u8>, from: Option<Option<u16>>, to: Option<u16>, all_off: &[u8]) {
    if from == Some(to) {
        return;
    }
    match from {
        Some(Some(mode)) => private_mode(out, mode, false),
        Some(None) => {}
        None => out.extend_from_slice(all_off),
    }
    if let Some(mode) = to {
        private_mode(out, mode, true);
    }
}

fn mouse_mode(mode: MouseProtocolMode) -> Option<u16> {
    match mode {
        MouseProtocolMode::None => None,
        MouseProtocolMode::Press => Some(9),
        MouseProtocolMode::PressRelease => Some(1000),
        MouseProtocolMode::ButtonMotion => Some(1002),
        MouseProtocolMode::AnyMotion => Some(1003),
    }
}

fn mouse_encoding(encoding: MouseProtocolEncoding) -> Option<u16> {
    match encoding {
        MouseProtocolEncoding::Default => None,
        MouseProtocolEncoding::Utf8 => Some(1005),
        MouseProtocolEncoding::Sgr => Some(1006),
    }
}

fn private_mode(out: &mut Vec<u8>, mode: u16, enabled: bool) {
    out.extend_from_slice(b"\x1b[?");
    push_number(out, mode);
    out.push(if enabled { b'h' } else { b'l' });
}

fn absolute_style(style: Style) -> Vec<u16> {
    let mut params = vec![0];
    if style.bold {
        params.push(1);
    }
    if style.dim {
        params.push(2);
    }
    if style.italic {
        params.push(3);
    }
    if style.underline {
        params.push(4);
    }
    if style.inverse {
        params.push(7);
    }
    if style.fg != Color::Default {
        push_color(&mut params, style.fg, Layer::Foreground);
    }
    if style.bg != Color::Default {
        push_color(&mut params, style.bg, Layer::Background);
    }
    params
}

fn relative_style(from: Style, to: Style) -> Vec<u16> {
    let mut params = Vec::new();
    let lost_intensity = (from.bold && !to.bold) || (from.dim && !to.dim);
    if lost_intensity {
        params.push(22);
    }
    if to.bold && (lost_intensity || !from.bold) {
        params.push(1);
    }
    if to.dim && (lost_intensity || !from.dim) {
        params.push(2);
    }
    for (was, is, on, off) in [
        (from.italic, to.italic, 3, 23),
        (from.underline, to.underline, 4, 24),
        (from.inverse, to.inverse, 7, 27),
    ] {
        if was != is {
            params.push(if is { on } else { off });
        }
    }
    if from.fg != to.fg {
        push_color(&mut params, to.fg, Layer::Foreground);
    }
    if from.bg != to.bg {
        push_color(&mut params, to.bg, Layer::Background);
    }
    params
}

#[derive(Clone, Copy)]
enum Layer {
    Foreground,
    Background,
}

fn push_color(params: &mut Vec<u16>, color: Color, layer: Layer) {
    let (base, bright, extended) = match layer {
        Layer::Foreground => (30, 90, 38),
        Layer::Background => (40, 100, 48),
    };
    match color {
        Color::Default => params.push(extended + 1),
        Color::Idx(index) if index < 8 => params.push(base + u16::from(index)),
        Color::Idx(index) if index < 16 => params.push(bright + u16::from(index) - 8),
        Color::Idx(index) => params.extend([extended, 5, u16::from(index)]),
        Color::Rgb(red, green, blue) => params.extend([
            extended,
            2,
            u16::from(red),
            u16::from(green),
            u16::from(blue),
        ]),
    }
}

fn encoded_len(params: &[u16]) -> usize {
    params.iter().map(|&param| digits(param) + 1).sum()
}

fn digits(number: u16) -> usize {
    number.checked_ilog10().map_or(1, |log| log as usize + 1)
}

fn csi(out: &mut Vec<u8>, params: &[u16], command: u8) {
    out.extend_from_slice(b"\x1b[");
    for (index, &param) in params.iter().enumerate() {
        if index > 0 {
            out.push(b';');
        }
        push_number(out, param);
    }
    out.push(command);
}

fn push_number(out: &mut Vec<u8>, number: u16) {
    if number >= 10 {
        push_number(out, number / 10);
    }
    out.push(b'0' + (number % 10) as u8);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles() -> Vec<Style> {
        let colors = [
            Color::Default,
            Color::Idx(1),
            Color::Idx(12),
            Color::Idx(200),
            Color::Rgb(1, 128, 255),
        ];
        let mut styles = Vec::new();
        for (index, &fg) in colors.iter().enumerate() {
            for &bg in &colors[..=index] {
                for intensity in 0..3 {
                    for flags in 0u8..8 {
                        styles.push(Style {
                            fg,
                            bg,
                            bold: intensity == 1,
                            dim: intensity == 2,
                            italic: flags & 1 != 0,
                            underline: flags & 2 != 0,
                            inverse: flags & 4 != 0,
                        });
                    }
                }
            }
        }
        styles
    }

    fn parsed_style(bytes: &[u8]) -> Style {
        let mut parser = vt100::Parser::new(1, 2, 0);
        parser.process(bytes);
        parser.process(b"x");
        let cell = parser.screen().cell(0, 0).unwrap();
        Style {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
        }
    }

    #[test]
    fn every_style_transition_lands_on_the_target_style() {
        let styles = styles();
        for &from in styles.iter().step_by(7) {
            let mut setup = Vec::new();
            set_style(&mut setup, None, from);
            assert_eq!(parsed_style(&setup), from);
            for &to in &styles {
                if from == to {
                    continue;
                }
                let mut bytes = setup.clone();
                set_style(&mut bytes, Some(from), to);
                assert_eq!(parsed_style(&bytes), to, "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn a_small_style_change_is_sent_relative_to_the_current_one() {
        let from = Style {
            fg: Color::Rgb(10, 20, 30),
            bold: true,
            ..Style::default()
        };
        let mut bytes = Vec::new();
        set_style(
            &mut bytes,
            Some(from),
            Style {
                underline: true,
                ..from
            },
        );
        assert_eq!(bytes, b"\x1b[4m");

        let mut bytes = Vec::new();
        set_style(&mut bytes, Some(from), Style::default());
        assert_eq!(bytes, b"\x1b[0m");
    }

    #[test]
    fn cursor_moves_use_the_shortest_sequence() {
        let moved = |from, to| {
            let mut bytes = Vec::new();
            move_cursor(&mut bytes, from, to);
            String::from_utf8(bytes).unwrap()
        };
        assert_eq!(moved(Some((3, 4)), (3, 4)), "");
        assert_eq!(moved(Some((3, 4)), (3, 9)), "\x1b[5C");
        assert_eq!(moved(Some((3, 4)), (3, 0)), "\r");
        assert_eq!(moved(Some((3, 4)), (3, 2)), "\x1b[4;3H");
        assert_eq!(moved(Some((3, 4)), (5, 0)), "\x1b[6H");
        assert_eq!(moved(None, (0, 0)), "\x1b[1H");
        assert_eq!(moved(None, (9, 79)), "\x1b[10;80H");
    }

    #[test]
    fn numbers_are_written_in_decimal() {
        for number in [0, 7, 10, 99, 1000, u16::MAX] {
            let mut bytes = Vec::new();
            push_number(&mut bytes, number);
            assert_eq!(bytes, number.to_string().as_bytes());
            assert_eq!(digits(number), bytes.len());
        }
    }
}
