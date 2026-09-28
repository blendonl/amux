use super::{Key, KeyCode, Mods, ESC};

impl Key {
    pub fn encodings(&self) -> Vec<Vec<u8>> {
        let mut forms = direct_forms(self.code, self.mods);
        if self.mods.contains(Mods::ALT) {
            let unmeta = self.without(Mods::ALT);
            forms.extend(
                unmeta
                    .encodings()
                    .into_iter()
                    .map(|form| [&[ESC][..], &form].concat()),
            );
        }
        forms
    }
}

fn direct_forms(code: KeyCode, mods: Mods) -> Vec<Vec<u8>> {
    match code {
        KeyCode::Char(character) => char_form(character, mods).into_iter().collect(),
        KeyCode::Enter if mods.is_empty() => vec![b"\r".to_vec(), ss3(b'M')],
        KeyCode::Tab if mods.is_empty() => vec![b"\t".to_vec()],
        KeyCode::BackTab if mods.is_empty() => vec![b"\x1b[Z".to_vec()],
        KeyCode::Backspace if mods.is_empty() => vec![vec![0x7f]],
        KeyCode::Escape if mods.is_empty() => vec![vec![ESC]],
        KeyCode::Up => cursor(b'A', mods),
        KeyCode::Down => cursor(b'B', mods),
        KeyCode::Right => cursor(b'C', mods),
        KeyCode::Left => cursor(b'D', mods),
        KeyCode::Home => [cursor(b'H', mods), vec![tilde(1, mods), tilde(7, mods)]].concat(),
        KeyCode::End => [cursor(b'F', mods), vec![tilde(4, mods), tilde(8, mods)]].concat(),
        KeyCode::Insert => vec![tilde(2, mods)],
        KeyCode::Delete => vec![tilde(3, mods)],
        KeyCode::PageUp => vec![tilde(5, mods)],
        KeyCode::PageDown => vec![tilde(6, mods)],
        KeyCode::F(number) => {
            let mut forms = match number {
                1..=4 if mods.is_empty() => vec![ss3(b'O' + number)],
                1..=4 => vec![csi_letter(b'O' + number, mods)],
                _ => Vec::new(),
            };
            forms.extend(function_tilde(number).map(|code| tilde(code, mods)));
            forms
        }
        _ => Vec::new(),
    }
}

fn char_form(character: char, mods: Mods) -> Option<Vec<u8>> {
    if mods.is_empty() {
        let mut buffer = [0; 4];
        return Some(character.encode_utf8(&mut buffer).as_bytes().to_vec());
    }
    if mods != Mods::CTRL {
        return None;
    }
    let control = match character {
        'a'..='z' => character as u8 & 0x1f,
        ' ' => 0x00,
        '\\' => 0x1c,
        ']' => 0x1d,
        '^' => 0x1e,
        '_' => 0x1f,
        _ => return None,
    };
    Some(vec![control])
}

pub(super) fn function_tilde(number: u8) -> Option<u8> {
    Some(match number {
        1..=5 => 10 + number,
        6..=10 => 11 + number,
        11 | 12 => 12 + number,
        _ => return None,
    })
}

fn cursor(letter: u8, mods: Mods) -> Vec<Vec<u8>> {
    if mods.is_empty() {
        vec![csi_letter(letter, mods), ss3(letter)]
    } else {
        vec![csi_letter(letter, mods)]
    }
}

fn csi_letter(letter: u8, mods: Mods) -> Vec<u8> {
    let mut form = b"\x1b[".to_vec();
    if !mods.is_empty() {
        form.extend(format!("1;{}", mods.xterm()).bytes());
    }
    form.push(letter);
    form
}

fn ss3(letter: u8) -> Vec<u8> {
    vec![ESC, b'O', letter]
}

fn tilde(number: u8, mods: Mods) -> Vec<u8> {
    let parameters = if mods.is_empty() {
        number.to_string()
    } else {
        format!("{number};{}", mods.xterm())
    };
    format!("\x1b[{parameters}~").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encodings(notation: &str) -> Vec<Vec<u8>> {
        notation.parse::<Key>().unwrap().encodings()
    }

    fn forms(forms: &[&[u8]]) -> Vec<Vec<u8>> {
        forms.iter().map(|form| form.to_vec()).collect()
    }

    #[test]
    fn characters_and_control_keys_have_one_byte_form() {
        assert_eq!(encodings("a"), forms(&[b"a"]));
        assert_eq!(encodings("%"), forms(&[b"%"]));
        assert_eq!(encodings("Space"), forms(&[b" "]));
        assert_eq!(encodings("日"), forms(&["日".as_bytes()]));
        assert_eq!(encodings("C-b"), forms(&[b"\x02"]));
        assert_eq!(encodings("C-Space"), forms(&[b"\x00"]));
        assert_eq!(encodings("C-\\"), forms(&[b"\x1c"]));
        assert_eq!(encodings("Tab"), forms(&[b"\t"]));
        assert_eq!(encodings("Backspace"), forms(&[b"\x7f"]));
        assert_eq!(encodings("Escape"), forms(&[b"\x1b"]));
        assert_eq!(encodings("Enter"), forms(&[b"\r", b"\x1bOM"]));
        assert_eq!(encodings("BackTab"), forms(&[b"\x1b[Z"]));
    }

    #[test]
    fn meta_adds_an_escape_prefix() {
        assert_eq!(encodings("M-a"), forms(&[b"\x1ba"]));
        assert_eq!(encodings("M-C-b"), forms(&[b"\x1b\x02"]));
        assert_eq!(encodings("M-Escape"), forms(&[b"\x1b\x1b"]));
        assert_eq!(
            encodings("M-Up"),
            forms(&[b"\x1b[1;3A", b"\x1b\x1b[A", b"\x1b\x1bOA"])
        );
    }

    #[test]
    fn cursor_keys_have_csi_and_ss3_forms_and_xterm_modifiers() {
        assert_eq!(encodings("Up"), forms(&[b"\x1b[A", b"\x1bOA"]));
        assert_eq!(encodings("Left"), forms(&[b"\x1b[D", b"\x1bOD"]));
        assert_eq!(encodings("C-Left"), forms(&[b"\x1b[1;5D"]));
        assert_eq!(encodings("S-Left"), forms(&[b"\x1b[1;2D"]));
        assert_eq!(
            encodings("M-S-Up"),
            forms(&[b"\x1b[1;4A", b"\x1b\x1b[1;2A"])
        );
        assert_eq!(
            encodings("Home"),
            forms(&[b"\x1b[H", b"\x1bOH", b"\x1b[1~", b"\x1b[7~"])
        );
        assert_eq!(
            encodings("C-End"),
            forms(&[b"\x1b[1;5F", b"\x1b[4;5~", b"\x1b[8;5~"])
        );
    }

    #[test]
    fn editing_and_function_keys_use_their_xterm_codes() {
        assert_eq!(encodings("Delete"), forms(&[b"\x1b[3~"]));
        assert_eq!(encodings("C-Delete"), forms(&[b"\x1b[3;5~"]));
        assert_eq!(encodings("PageDown"), forms(&[b"\x1b[6~"]));
        assert_eq!(encodings("F1"), forms(&[b"\x1bOP", b"\x1b[11~"]));
        assert_eq!(encodings("C-F4"), forms(&[b"\x1b[1;5S", b"\x1b[14;5~"]));
        assert_eq!(encodings("F5"), forms(&[b"\x1b[15~"]));
        assert_eq!(encodings("F6"), forms(&[b"\x1b[17~"]));
        assert_eq!(encodings("F10"), forms(&[b"\x1b[21~"]));
        assert_eq!(encodings("F11"), forms(&[b"\x1b[23~"]));
        assert_eq!(encodings("F12"), forms(&[b"\x1b[24~"]));
        assert_eq!(encodings("S-F12"), forms(&[b"\x1b[24;2~"]));
    }

    #[test]
    fn keys_a_terminal_cannot_send_have_no_encoding() {
        assert!(encodings("C-S-a").is_empty());
        assert!(encodings("C-%").is_empty());
        assert!(encodings("C-Enter").is_empty());
    }
}
