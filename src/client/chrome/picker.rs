use super::draw::{self, Rect, Span, Style};
use super::panel::{Panel, PanelEvent, Placement};
use crate::client::fuzzy::{self, Ranked};
use crate::client::search::{self, Candidate, Job};
use crate::keys::{Decoded, Key, KeyDecoder};
use crate::protocol::ServerView;
use crate::settings::{PickerAction, StyleSpec, Table, Theme};
use crate::target::Target;

const PROMPT: &str = "> ";
const SELECTED: &str = "> ";
const UNSELECTED: &str = "  ";
const DETAIL_GAP: &str = "  ";
const LOADING: &str = "loading…";

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    FindingProject,
    Loading,
    Ready,
    Failed(String),
}

#[derive(Debug)]
pub struct Picker {
    label: String,
    empty: String,
    state: State,
    query: Vec<char>,
    candidates: Vec<Candidate>,
    ranked: Vec<Ranked>,
    cursor: usize,
    scroll: usize,
    keys: KeyDecoder,
    bindings: Table<PickerAction>,
    theme: Theme,
}

impl Picker {
    pub fn projects(empty: String, bindings: Table<PickerAction>, theme: &Theme) -> Self {
        Self::new("project", empty, State::Loading, bindings, theme)
    }

    pub fn worktrees(bindings: Table<PickerAction>, theme: &Theme) -> Self {
        Self::new(
            "worktree",
            "no worktrees".into(),
            State::FindingProject,
            bindings,
            theme,
        )
    }

    fn new(
        label: &str,
        empty: String,
        state: State,
        bindings: Table<PickerAction>,
        theme: &Theme,
    ) -> Self {
        Self {
            label: label.into(),
            empty,
            state,
            query: Vec::new(),
            candidates: Vec::new(),
            ranked: Vec::new(),
            cursor: 0,
            scroll: 0,
            keys: KeyDecoder::default(),
            bindings,
            theme: theme.clone(),
        }
    }

    fn query(&self) -> String {
        self.query.iter().collect()
    }

    fn rank(&mut self) {
        let query = self.query();
        self.ranked = fuzzy::rank(
            &query,
            self.candidates
                .iter()
                .map(|candidate| candidate.label.as_str()),
        );
        self.cursor = 0;
        self.scroll = 0;
    }

    fn press(&mut self, decoded: &Decoded) -> Option<PanelEvent> {
        self.bindings
            .resolve(decoded)
            .into_iter()
            .find_map(|(key, action)| self.apply(key, action))
    }

    fn apply(&mut self, key: Key, action: Option<PickerAction>) -> Option<PanelEvent> {
        match action {
            Some(PickerAction::Down) => self.move_by(1),
            Some(PickerAction::Up) => self.move_by(-1),
            Some(PickerAction::Pick) => return self.pick(),
            Some(PickerAction::Cancel) => return Some(PanelEvent::Cancel),
            Some(PickerAction::DeleteBackward) => self.edit(|query| {
                query.pop();
            }),
            Some(PickerAction::DeleteWord) => self.edit(|query| {
                while query
                    .last()
                    .is_some_and(|character| character.is_whitespace())
                {
                    query.pop();
                }
                while query
                    .last()
                    .is_some_and(|character| !character.is_whitespace())
                {
                    query.pop();
                }
            }),
            Some(PickerAction::DeleteLine) => self.edit(Vec::clear),
            None => {
                if let Some(character) = key.printable() {
                    self.edit(|query| query.push(character));
                }
            }
        }
        None
    }

    fn edit(&mut self, change: impl FnOnce(&mut Vec<char>)) {
        let before = self.query.len();
        change(&mut self.query);
        if self.query.len() != before {
            self.rank();
        }
    }

    fn move_by(&mut self, offset: isize) {
        let last = self.ranked.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(offset).min(last);
    }

    fn pick(&self) -> Option<PanelEvent> {
        if self.state != State::Ready {
            return None;
        }
        let ranked = self.ranked.get(self.cursor)?;
        Some(PanelEvent::Open(self.candidates[ranked.index].path.clone()))
    }

    fn status(&self) -> String {
        match self.state {
            State::FindingProject | State::Loading => LOADING.into(),
            State::Ready | State::Failed(_) => {
                format!("{}/{}", self.ranked.len(), self.candidates.len())
            }
        }
    }

    fn message(&self) -> Option<&str> {
        match &self.state {
            State::Failed(message) => Some(message),
            State::Ready if self.candidates.is_empty() => Some(&self.empty),
            _ => None,
        }
    }

    fn style(&self, over: StyleSpec) -> Style {
        self.theme.picker.merge(over).into()
    }

    fn render_query(&self, out: &mut Vec<u8>, rect: Rect) {
        let columns = usize::from(rect.cols);
        let label = draw::truncate(&format!("{}{PROMPT}", self.label), columns);
        let label_width = draw::width(&label);
        let room = columns - label_width;
        let status = self.status();
        let status_width = draw::width(&status);
        let shows_status = room >= status_width + 2;
        let query_room = if shows_status {
            room - status_width - 1
        } else {
            room
        };
        let query = tail(&self.query(), query_room.saturating_sub(1));
        let query_width = draw::width(&query);

        let plain = self.style(StyleSpec::EMPTY);
        let mut spans = vec![
            Span::new(label, self.style(self.theme.picker_label)),
            Span::new(query, plain),
        ];
        if shows_status {
            let gap = room - query_width - status_width;
            spans.push(Span::new(" ".repeat(gap), plain));
            spans.push(Span::new(status, self.style(self.theme.picker_detail)));
        }
        let (row, col) = (usize::from(rect.row), usize::from(rect.col));
        draw::draw_row(out, row, col, columns, &spans, plain);
        let cursor = (label_width + query_width).min(columns - 1);
        draw::move_to(out, row, col + cursor);
    }

    fn render_list(&mut self, out: &mut Vec<u8>, rect: Rect) {
        let height = usize::from(rect.rows);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        let fill = self.style(StyleSpec::EMPTY);
        for line in 0..height {
            let spans = match self.message() {
                Some(message) if line == 0 => {
                    vec![Span::new(
                        format!("{UNSELECTED}{message}"),
                        self.style(self.theme.picker_detail),
                    )]
                }
                Some(_) => Vec::new(),
                None => self.row(self.scroll + line).unwrap_or_default(),
            };
            let row_fill = if self.message().is_none() && self.scroll + line == self.cursor {
                self.style(self.theme.picker_cursor)
            } else {
                fill
            };
            draw::draw_row(
                out,
                usize::from(rect.row) + line,
                usize::from(rect.col),
                usize::from(rect.cols),
                &spans,
                row_fill,
            );
        }
    }

    fn row(&self, position: usize) -> Option<Vec<Span>> {
        let ranked = self.ranked.get(position)?;
        let candidate = &self.candidates[ranked.index];
        let base = if position == self.cursor {
            self.theme.picker.merge(self.theme.picker_cursor)
        } else {
            self.theme.picker
        };
        let plain = Style::from(base);
        let matched = Style::from(base.merge(self.theme.picker_match));
        let marker = if position == self.cursor {
            SELECTED
        } else {
            UNSELECTED
        };
        let mut spans = vec![Span::new(marker, plain)];
        for (at, character) in candidate.label.chars().enumerate() {
            let style = if ranked.positions.binary_search(&at).is_ok() {
                matched
            } else {
                plain
            };
            match spans.last_mut() {
                Some(last) if last.style == style => last.text.push(character),
                _ => spans.push(Span::new(character.to_string(), style)),
            }
        }
        spans.push(Span::new(DETAIL_GAP, plain));
        spans.push(Span::new(
            candidate.detail.clone(),
            Style::from(base.merge(self.theme.picker_detail)),
        ));
        Some(spans)
    }
}

fn tail(text: &str, columns: usize) -> String {
    let mut kept: Vec<char> = Vec::new();
    let mut used = 0;
    for character in text.chars().rev() {
        let width = draw::char_width(character);
        if used + width > columns {
            break;
        }
        used += width;
        kept.push(character);
    }
    kept.into_iter().rev().collect()
}

impl Panel for Picker {
    fn handle(&mut self, input: &[u8], _attached: &Target) -> PanelEvent {
        for decoded in self.keys.feed(input) {
            if let Some(event) = self.press(&decoded) {
                return event;
            }
        }
        PanelEvent::Pending
    }

    fn time_out(&mut self, _attached: &Target) -> PanelEvent {
        self.keys
            .time_out()
            .and_then(|decoded| self.press(&decoded))
            .unwrap_or(PanelEvent::Pending)
    }

    fn is_partial(&self) -> bool {
        self.keys.is_partial()
    }

    fn render(&mut self, rect: Rect) -> Vec<u8> {
        let mut out = Vec::new();
        if rect.rows == 0 || rect.cols == 0 {
            return out;
        }
        let list = Rect {
            row: rect.row + 1,
            rows: rect.rows - 1,
            ..rect
        };
        self.render_list(&mut out, list);
        self.render_query(&mut out, rect);
        out.extend_from_slice(draw::SHOW_CURSOR);
        out
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::SessionArea)
    }

    fn hides_session(&self) -> bool {
        true
    }

    fn cluster_listed(&mut self, servers: &[ServerView], attached: &Target) -> PanelEvent {
        if self.state != State::FindingProject {
            return PanelEvent::Unchanged;
        }
        match search::attached_checkout(servers, attached) {
            Ok((name, checkout)) => {
                self.label = format!("{name} {}", self.label);
                self.state = State::Loading;
                PanelEvent::Load(Job::Worktrees { checkout })
            }
            Err(message) => {
                self.state = State::Failed(message);
                PanelEvent::Pending
            }
        }
    }

    fn found(&mut self, candidates: Result<Vec<Candidate>, String>) -> PanelEvent {
        match candidates {
            Ok(candidates) => {
                self.candidates = candidates;
                self.state = State::Ready;
                self.rank();
            }
            Err(message) => self.state = State::Failed(message),
        }
        PanelEvent::Pending
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::SystemTime;

    use super::*;
    use crate::client::chrome::testing::{row_text, terminal, Event};
    use crate::protocol::{ProjectCheckout, ServerStatus, SessionId, SessionInfo};
    use crate::settings::Keymap;

    const DOWN: &[u8] = b"\x1b[B";
    const UP: &[u8] = b"\x1b[A";

    fn projects() -> Picker {
        Picker::projects(
            "no git repositories in ~/projects".into(),
            Keymap::default().picker,
            &Theme::default(),
        )
    }

    fn candidate(label: &str) -> Candidate {
        Candidate {
            label: label.into(),
            detail: format!("~/projects/{label}"),
            path: PathBuf::from("/home/tester/projects").join(label),
        }
    }

    fn loaded(labels: &[&str]) -> Picker {
        let mut picker = projects();
        let found = picker.found(Ok(labels.iter().map(|label| candidate(label)).collect()));
        assert_eq!(Event::from(found), Event::Pending);
        picker
    }

    fn handle(picker: &mut Picker, input: &[u8]) -> Event {
        Panel::handle(picker, input, &Target::default()).into()
    }

    fn screen(picker: &mut Picker, rows: u16, cols: u16) -> vt100::Parser {
        let mut parser = terminal(rows, cols);
        parser.process(&picker.render(Rect {
            row: 0,
            col: 0,
            rows,
            cols,
        }));
        parser
    }

    fn opened(label: &str) -> Event {
        Event::Open(candidate(label).path)
    }

    #[test]
    fn typing_filters_the_candidates_and_enter_opens_the_selected_one() {
        let mut picker = loaded(&["amux", "notes", "tamux"]);
        assert_eq!(handle(&mut picker, b"am"), Event::Pending);

        let parser = screen(&mut picker, 5, 40);
        assert_eq!(row_text(&parser, 0), format!("{:<37}2/3", "project> am"));
        assert_eq!(row_text(&parser, 1), "> amux  ~/projects/amux");
        assert_eq!(row_text(&parser, 2), "  tamux  ~/projects/tamux");
        assert_eq!(row_text(&parser, 3), "");
        assert_eq!(parser.screen().cursor_position(), (0, 11));
        let cell = |row, col| parser.screen().cell(row, col).unwrap().clone();
        assert!(cell(1, 2).bold() && cell(1, 2).inverse());
        assert_eq!(cell(1, 2).fgcolor(), vt100::Color::Idx(3));
        assert!(!cell(1, 4).bold());
        assert!(!cell(2, 2).bold() && cell(2, 3).bold() && cell(2, 4).bold());
        assert!(cell(1, 30).inverse() && !cell(2, 30).inverse());

        assert_eq!(handle(&mut picker, DOWN), Event::Pending);
        assert_eq!(handle(&mut picker, DOWN), Event::Pending);
        assert_eq!(handle(&mut picker, b"\r"), opened("tamux"));
        assert_eq!(handle(&mut picker, UP), Event::Pending);
        assert_eq!(handle(&mut picker, b"\x10\x10\r"), opened("amux"));
    }

    #[test]
    fn keys_typed_while_loading_filter_what_arrives() {
        let mut picker = projects();
        assert_eq!(handle(&mut picker, b"nts"), Event::Pending);
        assert_eq!(handle(&mut picker, b"\r"), Event::Pending);
        assert_eq!(
            row_text(&screen(&mut picker, 3, 30), 0),
            format!("{:<22}{LOADING}", "project> nts")
        );

        picker.found(Ok(vec![candidate("amux"), candidate("notes")]));

        assert_eq!(
            row_text(&screen(&mut picker, 3, 30), 1),
            "> notes  ~/projects/notes"
        );
        assert_eq!(handle(&mut picker, b"\r"), opened("notes"));
    }

    #[test]
    fn editing_the_query_ranks_again_from_the_top() {
        let mut picker = loaded(&["work/api", "work/web", "api"]);
        handle(&mut picker, b"work api");
        assert_eq!(handle(&mut picker, b"\r"), opened("work/api"));

        handle(&mut picker, b"\x17");
        assert_eq!(picker.query(), "work ");
        handle(&mut picker, DOWN);
        assert_eq!(handle(&mut picker, b"\r"), opened("work/web"));
        handle(&mut picker, b"\x7f\x7f");
        assert_eq!(picker.query(), "wor");
        handle(&mut picker, b"\x15");
        assert_eq!(picker.query(), "");
        assert_eq!(handle(&mut picker, b"\r"), opened("work/api"));

        handle(&mut picker, b"zzz");
        assert_eq!(handle(&mut picker, b"\r"), Event::Pending);
        assert_eq!(row_text(&screen(&mut picker, 3, 30), 1), "");
    }

    #[test]
    fn escape_and_ctrl_c_cancel() {
        let mut picker = loaded(&["amux"]);
        assert_eq!(handle(&mut picker, b"\x03"), Event::Cancel);
        assert_eq!(handle(&mut picker, b"\x1b"), Event::Pending);
        assert!(picker.is_partial());
        assert_eq!(
            Event::from(picker.time_out(&Target::default())),
            Event::Cancel
        );
    }

    #[test]
    fn an_empty_scan_says_where_it_looked() {
        let mut picker = loaded(&[]);
        let parser = screen(&mut picker, 3, 40);
        assert_eq!(row_text(&parser, 0), format!("{:<37}0/0", "project>"));
        assert_eq!(row_text(&parser, 1), "  no git repositories in ~/projects");
        assert_eq!(handle(&mut picker, b"\r"), Event::Pending);
    }

    #[test]
    fn a_narrow_picker_drops_the_count_and_shows_the_end_of_the_query() {
        let mut picker = loaded(&["amux"]);
        handle(&mut picker, b"abcdef");
        let parser = screen(&mut picker, 2, 12);
        assert_eq!(row_text(&parser, 0), "project> ef");
        assert_eq!(parser.screen().cursor_position(), (0, 11));
    }

    #[test]
    fn a_long_list_scrolls_with_the_selection() {
        let labels: Vec<String> = (0..10).map(|index| format!("repo{index}")).collect();
        let mut picker = loaded(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        for _ in 0..5 {
            handle(&mut picker, DOWN);
        }
        let parser = screen(&mut picker, 4, 30);
        assert_eq!(row_text(&parser, 1), "  repo3  ~/projects/repo3");
        assert_eq!(row_text(&parser, 3), "> repo5  ~/projects/repo5");
        for _ in 0..20 {
            handle(&mut picker, DOWN);
        }
        assert_eq!(handle(&mut picker, b"\r"), opened("repo9"));
    }

    fn cluster(project: Option<&str>) -> Vec<ServerView> {
        vec![ServerView {
            id: None,
            name: "desk".into(),
            address: None,
            version: None,
            status: ServerStatus::Local,
            sessions: vec![SessionInfo {
                id: SessionId(1),
                name: "work".into(),
                windows: Vec::new(),
                attached_clients: 1,
                last_activity: SystemTime::UNIX_EPOCH,
                project: project.map(Into::into),
                branch: None,
            }],
            projects: vec![ProjectCheckout {
                id: "github.com/blendonl/amux".into(),
                name: "amux".into(),
                path: "/src/amux".into(),
                origin: None,
            }],
        }]
    }

    #[test]
    fn the_worktree_picker_loads_the_worktrees_of_the_attached_project() {
        let mut picker = Picker::worktrees(Keymap::default().picker, &Theme::default());
        let attached: Target = "work@desk".parse().unwrap();
        assert_eq!(handle(&mut picker, b"fe"), Event::Pending);

        let listed = picker.cluster_listed(&cluster(Some("github.com/blendonl/amux")), &attached);
        assert_eq!(
            Event::from(listed),
            Event::Load(Job::Worktrees {
                checkout: "/src/amux".into()
            })
        );
        assert_eq!(
            Event::from(picker.cluster_listed(&cluster(None), &attached)),
            Event::Unchanged
        );
        picker.found(Ok(vec![
            Candidate {
                label: "main".into(),
                detail: "~/src/amux".into(),
                path: "/src/amux".into(),
            },
            Candidate {
                label: "feature-x".into(),
                detail: "~/src/amux-worktrees/feature-x".into(),
                path: "/src/amux-worktrees/feature-x".into(),
            },
        ]));

        let parser = screen(&mut picker, 3, 50);
        assert_eq!(
            row_text(&parser, 0),
            format!("{:<47}1/2", "amux worktree> fe")
        );
        assert_eq!(
            handle(&mut picker, b"\r"),
            Event::Open("/src/amux-worktrees/feature-x".into())
        );
    }

    #[test]
    fn the_worktree_picker_says_why_it_has_nothing_to_show() {
        let mut picker = Picker::worktrees(Keymap::default().picker, &Theme::default());
        let listed = picker.cluster_listed(&cluster(None), &"work@desk".parse().unwrap());
        assert_eq!(Event::from(listed), Event::Pending);
        assert_eq!(
            row_text(&screen(&mut picker, 3, 40), 1),
            "  work is not in a project"
        );

        let mut failed = Picker::worktrees(Keymap::default().picker, &Theme::default());
        failed.cluster_listed(
            &cluster(Some("github.com/blendonl/amux")),
            &"work@desk".parse().unwrap(),
        );
        failed.found(Err("git worktree list failed".into()));
        assert_eq!(
            row_text(&screen(&mut failed, 3, 40), 1),
            "  git worktree list failed"
        );
        assert_eq!(handle(&mut failed, b"\r"), Event::Pending);
    }
}
