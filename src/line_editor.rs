use crate::keys::{Decoded, Key};
use crate::settings::{PromptAction, Table};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Submit,
    Cancel,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineEditor {
    text: Vec<char>,
    cursor: usize,
}

impl LineEditor {
    pub fn new(initial: &str) -> Self {
        let text: Vec<char> = initial
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        Self {
            cursor: text.len(),
            text,
        }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn press(&mut self, decoded: &Decoded, bindings: &Table<PromptAction>) -> Option<Finish> {
        bindings
            .resolve(decoded)
            .into_iter()
            .find_map(|(key, action)| self.apply(key, action))
    }

    pub fn insert(&mut self, text: &str) {
        for character in text.chars().filter(|character| !character.is_control()) {
            self.insert_char(character);
        }
    }

    pub fn visible(&self, columns: usize, width: impl Fn(char) -> usize) -> (String, usize) {
        let widths: Vec<usize> = self
            .text
            .iter()
            .map(|&character| width(character))
            .collect();
        let mut start = self.cursor;
        let mut cursor_column = 0;
        while start > 0 && cursor_column + widths[start - 1] < columns {
            start -= 1;
            cursor_column += widths[start];
        }

        let mut visible = String::new();
        let mut used = 0;
        for (&character, &width) in self.text[start..].iter().zip(&widths[start..]) {
            if used + width > columns {
                break;
            }
            visible.push(character);
            used += width;
        }
        (visible, cursor_column)
    }

    fn apply(&mut self, key: Key, action: Option<PromptAction>) -> Option<Finish> {
        match action {
            Some(PromptAction::Submit) => return Some(Finish::Submit),
            Some(PromptAction::Cancel) => return Some(Finish::Cancel),
            Some(PromptAction::DeleteBackward) if self.cursor > 0 => {
                self.cursor -= 1;
                self.text.remove(self.cursor);
            }
            Some(PromptAction::DeleteForward) if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            Some(PromptAction::DeleteLine) => {
                self.text.clear();
                self.cursor = 0;
            }
            Some(PromptAction::CursorLeft) => self.cursor = self.cursor.saturating_sub(1),
            Some(PromptAction::CursorRight) => {
                self.cursor = (self.cursor + 1).min(self.text.len());
            }
            Some(PromptAction::CursorStart) => self.cursor = 0,
            Some(PromptAction::CursorEnd) => self.cursor = self.text.len(),
            None => {
                if let Some(character) = key.printable() {
                    self.insert_char(character);
                }
            }
            _ => {}
        }
        None
    }

    fn insert_char(&mut self, character: char) {
        self.text.insert(self.cursor, character);
        self.cursor += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyDecoder;
    use crate::settings::Keymap;

    fn typed(initial: &str, input: &[u8]) -> (LineEditor, Vec<Finish>) {
        let mut line = LineEditor::new(initial);
        let bindings = Keymap::default().prompt;
        let finishes = KeyDecoder::default()
            .feed(input)
            .iter()
            .filter_map(|decoded| line.press(decoded, &bindings))
            .collect();
        (line, finishes)
    }

    #[test]
    fn keys_edit_the_line_until_it_is_submitted_or_cancelled() {
        let (line, finishes) = typed("ab", b"\x1b[Dx\x7f\x15new");
        assert_eq!((line.text().as_str(), line.cursor()), ("new", 3));
        assert!(finishes.is_empty());
        assert_eq!(typed("ab", b"c\r").1, [Finish::Submit]);
        assert_eq!(typed("ab", b"\x03").1, [Finish::Cancel]);
    }

    #[test]
    fn inserted_text_lands_at_the_cursor_without_control_characters() {
        let mut line = LineEditor::new("ad");
        line.insert("");
        assert_eq!(line, LineEditor::new("ad"));
        let (mut line, _) = typed("ad", b"\x1b[D");
        line.insert("b\r\n\tc");
        assert_eq!((line.text().as_str(), line.cursor()), ("abcd", 3));
    }

    #[test]
    fn the_visible_part_keeps_the_cursor_inside_the_columns() {
        let line = LineEditor::new("abcdef");
        assert_eq!(line.visible(4, |_| 1), ("def".into(), 3));
        assert_eq!(line.visible(10, |_| 1), ("abcdef".into(), 6));
        assert_eq!(line.visible(0, |_| 1), (String::new(), 0));
        assert_eq!(line.visible(5, |_| 2), ("ef".into(), 4));
    }
}
