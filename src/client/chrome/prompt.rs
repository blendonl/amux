use super::draw::{self, Rect, Span, Style};
use super::panel::{Panel, PanelEvent, Placement};
use crate::keys::{Decoded, Key, KeyDecoder};
use crate::protocol::{ClientMessage, SessionCommand};
use crate::settings::{CallbackId, PromptAction, Table, Theme};
use crate::target::Target;

const LABEL_SEPARATOR: &str = ": ";
const MIN_INPUT_COLUMNS: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptPurpose {
    RenameWindow,
    RenameSession,
    Callback(CallbackId),
}

impl PromptPurpose {
    fn submit(&self, text: String, attached: &Target) -> PanelEvent {
        match self {
            Self::Callback(id) => PanelEvent::Callback(*id, text),
            _ if text.is_empty() => PanelEvent::Cancel,
            Self::RenameWindow => {
                PanelEvent::Done(ClientMessage::Command(SessionCommand::RenameWindow(text)))
            }
            Self::RenameSession => PanelEvent::Done(ClientMessage::RenameSession {
                target: attached.clone(),
                name: text,
            }),
        }
    }
}

#[derive(Debug)]
pub struct Prompt {
    purpose: PromptPurpose,
    label: String,
    text: Vec<char>,
    cursor: usize,
    keys: KeyDecoder,
    bindings: Table<PromptAction>,
    style: Style,
    label_style: Style,
}

impl Prompt {
    pub fn new(
        purpose: PromptPurpose,
        label: impl Into<String>,
        initial: &str,
        bindings: Table<PromptAction>,
        theme: &Theme,
    ) -> Self {
        let text: Vec<char> = initial
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        Self {
            purpose,
            label: label.into(),
            cursor: text.len(),
            text,
            keys: KeyDecoder::default(),
            bindings,
            style: theme.prompt.into(),
            label_style: theme.prompt.merge(theme.prompt_label).into(),
        }
    }

    pub fn text(&self) -> String {
        self.text.iter().collect()
    }

    fn visible_text(&self, columns: usize) -> (String, usize) {
        let widths: Vec<usize> = self
            .text
            .iter()
            .map(|&character| draw::char_width(character))
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

    fn press(&mut self, decoded: &Decoded, attached: &Target) -> Option<PanelEvent> {
        self.bindings
            .resolve(decoded)
            .into_iter()
            .find_map(|(key, action)| self.apply(key, action, attached))
    }

    fn apply(
        &mut self,
        key: Key,
        action: Option<PromptAction>,
        attached: &Target,
    ) -> Option<PanelEvent> {
        match action {
            Some(PromptAction::Submit) => return Some(self.purpose.submit(self.text(), attached)),
            Some(PromptAction::Cancel) => return Some(PanelEvent::Cancel),
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
                    self.text.insert(self.cursor, character);
                    self.cursor += 1;
                }
            }
            _ => {}
        }
        None
    }
}

impl Panel for Prompt {
    fn handle(&mut self, input: &[u8], attached: &Target) -> PanelEvent {
        for decoded in self.keys.feed(input) {
            if let Some(event) = self.press(&decoded, attached) {
                return event;
            }
        }
        PanelEvent::Pending
    }

    fn time_out(&mut self, attached: &Target) -> PanelEvent {
        self.keys
            .time_out()
            .and_then(|decoded| self.press(&decoded, attached))
            .unwrap_or(PanelEvent::Pending)
    }

    fn is_partial(&self) -> bool {
        self.keys.is_partial()
    }

    fn render(&mut self, area: Rect) -> Vec<u8> {
        let columns = usize::from(area.cols);
        let mut out = Vec::new();
        if columns == 0 {
            return out;
        }

        let label = format!("{}{LABEL_SEPARATOR}", self.label);
        let label_width = draw::width(&label);
        let label_budget = if label_width + MIN_INPUT_COLUMNS <= columns {
            label_width
        } else {
            label_width.min(columns / 2)
        };
        let label = draw::truncate(&label, label_budget);
        let label_width = draw::width(&label);
        let (visible, cursor_column) = self.visible_text(columns - label_width);

        let (row, col) = (usize::from(area.row), usize::from(area.col));
        let spans = [
            Span::new(label, self.label_style),
            Span::new(visible, self.style),
        ];
        draw::draw_row(&mut out, row, col, columns, &spans, self.style);
        draw::move_to(&mut out, row, col + label_width + cursor_column);
        out.extend_from_slice(draw::SHOW_CURSOR);
        out
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::StatusRow)
    }

    fn hides_session(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::chrome::testing::{row_text, terminal, Event};
    use crate::settings::Keymap;

    fn prompt_for(purpose: PromptPurpose, label: &str, initial: &str) -> Prompt {
        Prompt::new(
            purpose,
            label,
            initial,
            Keymap::default().prompt,
            &Theme::default(),
        )
    }

    fn new_prompt(label: &str, initial: &str) -> Prompt {
        prompt_for(PromptPurpose::RenameWindow, label, initial)
    }

    fn handle(prompt: &mut Prompt, input: &[u8]) -> Event {
        Panel::handle(prompt, input, &Target::default()).into()
    }

    fn time_out(prompt: &mut Prompt) -> Event {
        Panel::time_out(prompt, &Target::default()).into()
    }

    fn renamed(name: &str) -> Event {
        Event::Done(ClientMessage::Command(SessionCommand::RenameWindow(
            name.into(),
        )))
    }

    fn edited(initial: &str, chunks: &[&[u8]]) -> Prompt {
        let mut prompt = new_prompt("name", initial);
        for chunk in chunks {
            assert_eq!(handle(&mut prompt, chunk), Event::Pending);
        }
        prompt
    }

    fn draw(prompt: &mut Prompt, width: u16) -> vt100::Parser {
        let mut parser = terminal(3, width);
        parser.process(b"\x1b[?25l");
        parser.process(&prompt.render(Rect {
            row: 2,
            col: 0,
            rows: 1,
            cols: width,
        }));
        parser
    }

    #[test]
    fn the_initial_text_is_edited_at_its_end() {
        let prompt = edited("sh", &[b"ell"]);
        assert_eq!(prompt.text(), "shell");
        assert_eq!(prompt.cursor, 5);
    }

    #[test]
    fn keys_move_the_cursor_and_edit_around_it() {
        let cases: [(&[&[u8]], &str, usize); 10] = [
            (&[b"\x1b[D\x1b[DX"], "abXcd", 3),
            (&[b"\x1bOD!"], "abc!d", 4),
            (&[b"\x01<\x05>"], "<abcd>", 6),
            (&[b"\x1b[H1\x1b[F2"], "1abcd2", 6),
            (&[b"\x1b[1~^\x1b[4~$"], "^abcd$", 6),
            (&[b"\x7f\x08"], "ab", 2),
            (&[b"\x01\x7f"], "abcd", 0),
            (&[b"\x01\x1b[3~\x1b[C"], "bcd", 1),
            (&[b"\x15new"], "new", 3),
            (&[b"\x1b[C\x1b[C\x01\x1b[D"], "abcd", 0),
        ];
        for (chunks, text, cursor) in cases {
            let prompt = edited("abcd", chunks);
            assert_eq!(
                (prompt.text().as_str(), prompt.cursor),
                (text, cursor),
                "{chunks:?}"
            );
        }
    }

    #[test]
    fn enter_submits_and_escape_cancels() {
        assert_eq!(
            handle(&mut new_prompt("name", "sh"), b"ell\r"),
            renamed("shell")
        );
        assert_eq!(handle(&mut new_prompt("name", ""), b"a\nb"), renamed("a"));
        assert_eq!(
            handle(&mut new_prompt("name", "sh"), b"\x1bx"),
            Event::Cancel
        );
        assert_eq!(
            handle(&mut new_prompt("name", "sh"), b"\x03"),
            Event::Cancel
        );
    }

    #[test]
    fn a_callback_prompt_submits_any_text_to_its_callback() {
        let purpose = PromptPurpose::Callback(CallbackId(4));
        assert_eq!(
            handle(&mut prompt_for(purpose.clone(), "name", "sh"), b"ell\r"),
            Event::Callback(CallbackId(4), "shell".into())
        );
        assert_eq!(
            handle(&mut prompt_for(purpose.clone(), "name", ""), b"\r"),
            Event::Callback(CallbackId(4), String::new())
        );
        assert_eq!(
            handle(&mut prompt_for(purpose, "name", "sh"), b"\x03"),
            Event::Cancel
        );
        assert_eq!(handle(&mut new_prompt("name", ""), b"\r"), Event::Cancel);
    }

    #[test]
    fn escape_sequences_split_across_chunks_still_edit() {
        let prompt = edited("abc", &[b"\x1b[", b"D", b"X"]);
        assert_eq!((prompt.text().as_str(), prompt.cursor), ("abXc", 3));

        let prompt = edited("abc", &[b"\x1b", b"[H", b"X"]);
        assert_eq!((prompt.text().as_str(), prompt.cursor), ("Xabc", 1));

        let prompt = edited("abc", &[b"\x1b[1", b";5", b"D"]);
        assert_eq!(prompt.cursor, 2);

        let prompt = edited("", &[&[0xc3], &[0xa9], &[0xe6, 0x97], &[0xa5]]);
        assert_eq!(prompt.text(), "é日");
    }

    #[test]
    fn a_lone_escape_cancels_once_the_timeout_expires() {
        let mut prompt = new_prompt("name", "sh");
        assert_eq!(handle(&mut prompt, b"\x1b"), Event::Pending);
        assert!(prompt.is_partial());
        assert_eq!(time_out(&mut prompt), Event::Cancel);

        let mut prompt = new_prompt("name", "sh");
        assert_eq!(time_out(&mut prompt), Event::Pending);
        assert_eq!(handle(&mut prompt, b"\x1b[D"), Event::Pending);
        assert!(!prompt.is_partial());
    }

    #[test]
    fn unknown_keys_do_not_change_the_text() {
        let prompt = edited("sh", &[b"\x1b[15~\x1b[<0;3;4M\t\x1b[A\x02"]);
        assert_eq!((prompt.text().as_str(), prompt.cursor), ("sh", 2));
        assert_eq!(new_prompt("name", "a\x1bb").text(), "ab");
    }

    #[test]
    fn rendering_draws_the_label_and_text_and_places_the_cursor() {
        let parser = draw(&mut new_prompt("rename window", "sh"), 40);
        let screen = parser.screen();

        assert_eq!(row_text(&parser, 2), "rename window: sh");
        assert_eq!(screen.cursor_position(), (2, 17));
        assert!(!screen.hide_cursor());
        assert!(screen.cell(2, 0).unwrap().bold());
        assert!(!screen.cell(2, 15).unwrap().bold());
        assert_eq!(screen.cell(2, 39).unwrap().bgcolor(), vt100::Color::Idx(3));
    }

    #[test]
    fn rendering_leaves_default_attributes() {
        let mut parser = draw(&mut new_prompt("name", ""), 20);
        parser.process(b"x");
        let cell = parser.screen().cell(2, 6).unwrap();
        assert_eq!(cell.contents(), "x");
        assert_eq!(cell.bgcolor(), vt100::Color::Default);
        assert!(!cell.bold());
    }

    #[test]
    fn long_text_scrolls_to_keep_the_cursor_visible() {
        let mut prompt = new_prompt("name", "abcdefghijklmnopqrstuvwxyz");
        let parser = draw(&mut prompt, 20);
        assert_eq!(row_text(&parser, 2), "name: nopqrstuvwxyz");
        assert_eq!(parser.screen().cursor_position(), (2, 19));

        handle(&mut prompt, b"\x01");
        let parser = draw(&mut prompt, 20);
        assert_eq!(row_text(&parser, 2), "name: abcdefghijklmn");
        assert_eq!(parser.screen().cursor_position(), (2, 6));
    }

    #[test]
    fn wide_characters_scroll_by_whole_characters() {
        let mut prompt = new_prompt("n", "日本語");
        let parser = draw(&mut prompt, 12);
        assert_eq!(row_text(&parser, 2), "n: 日本語");
        assert_eq!(parser.screen().cursor_position(), (2, 9));

        let parser = draw(&mut prompt, 7);
        assert_eq!(row_text(&parser, 2), "n: 語");
        assert_eq!(parser.screen().cursor_position(), (2, 5));

        handle(&mut prompt, b"\x1b[D");
        let parser = draw(&mut prompt, 7);
        assert_eq!(row_text(&parser, 2), "n: 本語");
        assert_eq!(parser.screen().cursor_position(), (2, 5));
    }

    #[test]
    fn a_narrow_row_shortens_the_label_first() {
        let mut prompt = new_prompt("rename window", "sh");
        let parser = draw(&mut prompt, 12);
        assert_eq!(row_text(&parser, 2), "renam…sh");
        assert_eq!(parser.screen().cursor_position(), (2, 8));

        for width in 1..=30 {
            let parser = draw(&mut prompt, width);
            assert!(row_text(&parser, 1).is_empty(), "width {width}");
            let (row, col) = parser.screen().cursor_position();
            assert_eq!(row, 2, "width {width}");
            assert!(col < width, "width {width}");
        }
    }
}
