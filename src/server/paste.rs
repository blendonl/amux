use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Default)]
pub struct PasteBuffer {
    text: Mutex<String>,
}

impl PasteBuffer {
    pub fn store(&self, text: String) {
        *self.lock() = text;
    }

    pub fn contents(&self) -> String {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, String> {
        self.text.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub fn typed(text: &str, bracketed: bool) -> Vec<u8> {
    let typed: String = text
        .chars()
        .map(|character| if character == '\n' { '\r' } else { character })
        .filter(|character| !character.is_control() || matches!(character, '\t' | '\r'))
        .collect();
    if bracketed {
        format!("\x1b[200~{typed}\x1b[201~").into_bytes()
    } else {
        typed.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_keeps_the_latest_text() {
        let buffer = PasteBuffer::default();
        assert_eq!(buffer.contents(), "");
        buffer.store("first".into());
        buffer.store("second\n".into());
        assert_eq!(buffer.contents(), "second\n");
    }

    #[test]
    fn newlines_become_carriage_returns_and_other_control_characters_go() {
        assert_eq!(
            typed("alpha\nbeta\tgamma\r", false),
            b"alpha\rbeta\tgamma\r"
        );
        assert_eq!(typed("a\x1b[201~b\x07\x7f\u{9b}c\x00", false), b"a[201~bc");
        assert_eq!(typed("naïve 日本", false), "naïve 日本".as_bytes());
    }

    #[test]
    fn a_program_that_asked_for_bracketed_paste_gets_the_text_bracketed() {
        assert_eq!(typed("one\ntwo", true), b"\x1b[200~one\rtwo\x1b[201~");
        assert_eq!(typed("\x1b[201~", true), b"\x1b[200~[201~\x1b[201~");
    }
}
