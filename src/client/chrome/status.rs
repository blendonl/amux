use std::time::Duration;

use super::draw::{self, Span, Style};
use super::template;
use crate::protocol::WindowSummary;
use crate::settings::{StatusSettings, Theme};

const MIN_SESSION_COLUMNS: usize = 4;
const GAP: usize = 1;

struct Look<'a> {
    formats: &'a StatusSettings,
    bar: Style,
    session: Style,
    active_window: Style,
    offline: Style,
    message: Style,
}

impl<'a> Look<'a> {
    fn new(formats: &'a StatusSettings, theme: &Theme) -> Self {
        let bar = theme.status;
        Self {
            formats,
            bar: bar.into(),
            session: bar.merge(theme.status_session).into(),
            active_window: bar.merge(theme.status_active_window).into(),
            offline: bar.merge(theme.status_offline).into(),
            message: theme.message.into(),
        }
    }

    fn hidden_markers(&self, start: usize, end: usize, count: usize) -> usize {
        (usize::from(start > 0) + usize::from(end < count))
            * draw::width(&self.formats.hidden_marker)
    }

    fn hidden_marker(&self) -> Span {
        Span::new(self.formats.hidden_marker.as_str(), self.bar)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTab {
    pub index: usize,
    pub name: String,
    pub active: bool,
}

impl WindowTab {
    pub fn list(windows: &[WindowSummary], active: usize) -> Vec<Self> {
        windows
            .iter()
            .map(|window| Self {
                index: window.index,
                name: window.name.clone(),
                active: window.index == active,
            })
            .collect()
    }

    fn span(&self, look: &Look) -> Span {
        let style = if self.active {
            look.active_window
        } else {
            look.bar
        };
        let index = self.index.to_string();
        let values = [("index", index.as_str()), ("name", self.name.as_str())];
        Span::new(template::fill(&look.formats.window_format, &values), style)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusLine {
    pub session: String,
    pub server: String,
    pub local: bool,
    pub windows: Vec<WindowTab>,
    pub latency: Option<Duration>,
    pub offline: Vec<String>,
    pub message: Option<String>,
}

impl StatusLine {
    pub fn render(&self, width: u16, settings: &StatusSettings, theme: &Theme) -> Vec<u8> {
        let look = Look::new(settings, theme);
        let columns = usize::from(width);
        let (spans, fill) = match &self.message {
            Some(message) => (
                vec![Span::new(message.as_str(), look.message)],
                look.message,
            ),
            None => (self.spans(columns, &look), look.bar),
        };
        let mut out = Vec::new();
        out.extend_from_slice(draw::SAVE_CURSOR);
        out.extend_from_slice(draw::MOVE_TO_LAST_ROW);
        draw::write_spans(&mut out, &spans, columns, fill);
        out.extend_from_slice(draw::RESTORE_CURSOR);
        out
    }

    fn spans(&self, columns: usize, look: &Look) -> Vec<Span> {
        let tag = self.tag(columns, look);
        let tag_width = draw::width(&tag.text);
        let tabs: Vec<Span> = self.windows.iter().map(|tab| tab.span(look)).collect();
        let right = self.right_side(&tabs, columns.saturating_sub(tag_width), look);
        let right_width = draw::spans_width(&right);
        let reserved = tag_width + right_width + if right.is_empty() { 0 } else { GAP };

        let mut spans = vec![tag];
        spans.extend(self.fit_windows(tabs, columns.saturating_sub(reserved), look));
        let used = draw::spans_width(&spans) + right_width;
        spans.push(Span::new(
            " ".repeat(columns.saturating_sub(used)),
            look.bar,
        ));
        spans.extend(right);
        spans
    }

    fn tag(&self, columns: usize, look: &Look) -> Span {
        let format = look.formats.session_format.as_str();
        let values = [
            ("session", self.session.as_str()),
            ("server", self.server.as_str()),
        ];
        let full = template::fill(format, &values);
        let (open, address, close) = template::frame(format, &values);
        let frame_width = draw::width(open) + draw::width(close);
        if draw::width(&full) <= columns || columns <= frame_width {
            return Span::new(draw::truncate(&full, columns), look.session);
        }
        let inner = columns - frame_width;
        let fixed = draw::width(&template::fill(
            address,
            &[("session", ""), ("server", self.server.as_str())],
        ));
        let shortened = (fixed + MIN_SESSION_COLUMNS <= inner)
            .then(|| {
                let session = draw::truncate(&self.session, inner - fixed);
                template::fill(
                    format,
                    &[("session", &session), ("server", self.server.as_str())],
                )
            })
            .filter(|text| draw::width(text) <= columns);
        let text = shortened.unwrap_or_else(|| {
            let shown = draw::truncate(&template::fill(address, &values), inner);
            format!("{open}{shown}{close}")
        });
        Span::new(text, look.session)
    }

    fn right_side(&self, tabs: &[Span], room: usize, look: &Look) -> Vec<Span> {
        let all_windows = draw::spans_width(tabs);
        let windows = if all_windows <= room {
            all_windows
        } else {
            self.active_window_width(tabs, look)
        };
        self.right_variants(look)
            .into_iter()
            .find(|spans| spans.is_empty() || windows + GAP + draw::spans_width(spans) <= room)
            .unwrap_or_default()
    }

    fn right_variants(&self, look: &Look) -> [Vec<Span>; 4] {
        let formats = look.formats;
        let latency = self.latency.filter(|_| !self.local).map(|latency| {
            let latency = format_latency(latency);
            let text = template::fill(&formats.latency_format, &[("latency", &latency)]);
            Span::new(text, look.bar)
        });
        let peers = self.offline.iter().map(|peer| {
            let text = template::fill(&formats.offline_format, &[("server", peer)]);
            Span::new(text, look.offline)
        });
        let count = (!self.offline.is_empty()).then(|| {
            let count = self.offline.len().to_string();
            let text = template::fill(&formats.offline_count_format, &[("count", &count)]);
            Span::new(text, look.offline)
        });
        [
            peers.chain(latency.clone()).collect(),
            count.into_iter().chain(latency.clone()).collect(),
            latency.into_iter().collect(),
            Vec::new(),
        ]
    }

    fn active_position(&self) -> usize {
        self.windows
            .iter()
            .position(|window| window.active)
            .unwrap_or(0)
    }

    fn active_window_width(&self, tabs: &[Span], look: &Look) -> usize {
        let active = self.active_position();
        tabs.get(active).map_or(0, |tab| draw::width(&tab.text))
            + look.hidden_markers(active, active + 1, tabs.len())
    }

    fn fit_windows(&self, tabs: Vec<Span>, budget: usize, look: &Look) -> Vec<Span> {
        if draw::spans_width(&tabs) <= budget {
            return tabs;
        }
        let count = tabs.len();
        let widths: Vec<usize> = tabs.iter().map(|tab| draw::width(&tab.text)).collect();
        let active = self.active_position();
        let (mut start, mut end, mut used) = (active, active + 1, widths[active]);
        if used + look.hidden_markers(start, end, count) > budget {
            let tab = &tabs[active];
            return vec![Span::new(draw::truncate(&tab.text, budget), tab.style)];
        }
        loop {
            let mut grew = false;
            if end < count
                && used + widths[end] + look.hidden_markers(start, end + 1, count) <= budget
            {
                used += widths[end];
                end += 1;
                grew = true;
            }
            if start > 0
                && used + widths[start - 1] + look.hidden_markers(start - 1, end, count) <= budget
            {
                start -= 1;
                used += widths[start];
                grew = true;
            }
            if !grew {
                break;
            }
        }

        let mut spans = Vec::new();
        if start > 0 {
            spans.push(look.hidden_marker());
        }
        spans.extend(tabs.into_iter().take(end).skip(start));
        if end < count {
            spans.push(look.hidden_marker());
        }
        spans
    }
}

pub fn format_latency(latency: Duration) -> String {
    match latency.as_millis() {
        0 => "<1 ms".to_owned(),
        millis => format!("{millis} ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::chrome::testing::{row_text, terminal};
    use crate::settings::{Color, StyleSpec};

    const ROWS: u16 = 3;
    const STATUS_ROW: u16 = ROWS - 1;
    const WINDOWS: &str = "[work@desktop] 0:sh  1:vim  2:logs";

    fn tab(index: usize, name: &str, active: bool) -> WindowTab {
        WindowTab {
            index,
            name: name.into(),
            active,
        }
    }

    fn remote() -> StatusLine {
        StatusLine {
            session: "work".into(),
            server: "desktop".into(),
            local: false,
            windows: vec![
                tab(0, "sh", false),
                tab(1, "vim", true),
                tab(2, "logs", false),
            ],
            latency: Some(Duration::from_millis(12)),
            offline: vec!["laptop".into()],
            message: None,
        }
    }

    fn draw(status: &StatusLine, width: u16) -> vt100::Parser {
        draw_with(status, width, &StatusSettings::default(), &Theme::default())
    }

    fn draw_with(
        status: &StatusLine,
        width: u16,
        settings: &StatusSettings,
        theme: &Theme,
    ) -> vt100::Parser {
        let mut parser = terminal(ROWS, width);
        parser.process(b"T\x1b[2;1H");
        parser.process(&status.render(width, settings, theme));
        parser
    }

    fn custom_formats() -> StatusSettings {
        StatusSettings {
            session_format: "<{server}:{session}>".into(),
            window_format: "{index}|{name} ".into(),
            latency_format: "~{latency}".into(),
            offline_format: " !{server}".into(),
            offline_count_format: " !{count}".into(),
            hidden_marker: "<>".into(),
            ..StatusSettings::default()
        }
    }

    fn formatted(status: &StatusLine, width: u16) -> String {
        let parser = draw_with(status, width, &custom_formats(), &Theme::default());
        row_text(&parser, STATUS_ROW)
    }

    fn bottom(status: &StatusLine, width: u16) -> String {
        row_text(&draw(status, width), STATUS_ROW)
    }

    #[test]
    fn a_wide_terminal_shows_windows_offline_peers_and_latency() {
        assert_eq!(
            bottom(&remote(), 80),
            format!("{WINDOWS:<58}laptop offline  12 ms")
        );
    }

    #[test]
    fn drawing_restores_the_cursor_and_leaves_default_attributes() {
        let mut parser = draw(&remote(), 80);
        assert_eq!(parser.screen().cursor_position(), (1, 0));
        assert_eq!(row_text(&parser, 0), "T");

        parser.process(b"x");
        let cell = parser.screen().cell(1, 0).unwrap();
        assert_eq!(cell.contents(), "x");
        assert_eq!(cell.fgcolor(), vt100::Color::Default);
        assert_eq!(cell.bgcolor(), vt100::Color::Default);
        assert!(!cell.bold() && !cell.inverse());
    }

    #[test]
    fn the_bar_is_coloured_and_highlights_the_active_window() {
        let parser = draw(&remote(), 80);
        let screen = parser.screen();
        let cell = |col| screen.cell(STATUS_ROW, col).unwrap();

        assert!((0..80).all(|col| cell(col).bgcolor() != vt100::Color::Default));
        assert!(cell(1).bold());
        assert_eq!(cell(15).contents(), "0");
        assert!(!cell(15).inverse());
        assert_eq!(cell(21).contents(), "1");
        assert!(cell(21).inverse() && cell(21).bold());
        assert_eq!(cell(58).contents(), "l");
        assert_eq!(cell(58).bgcolor(), vt100::Color::Idx(1));
        assert_eq!(cell(75).bgcolor(), vt100::Color::Idx(2));
    }

    #[test]
    fn a_message_takes_the_whole_bar_until_it_is_cleared() {
        let status = StatusLine {
            message: Some("can't find session: gone@desktop".into()),
            ..remote()
        };
        let parser = draw(&status, 40);
        assert_eq!(
            row_text(&parser, STATUS_ROW),
            "can't find session: gone@desktop"
        );
        let cell = parser.screen().cell(STATUS_ROW, 39).unwrap();
        assert_eq!(cell.bgcolor(), vt100::Color::Idx(3));
        assert_eq!(parser.screen().cursor_position(), (1, 0));

        let parser = draw(&status, 10);
        assert_eq!(row_text(&parser, STATUS_ROW), "can't fin…");
    }

    #[test]
    fn a_local_session_shows_no_latency() {
        let status = StatusLine {
            local: true,
            offline: Vec::new(),
            ..remote()
        };
        assert_eq!(bottom(&status, 60), WINDOWS);
    }

    #[test]
    fn narrower_terminals_give_up_cluster_details_before_windows() {
        let mut status = remote();
        status.offline.push("home-server".into());

        for (width, expected) in [
            (
                80,
                format!("{WINDOWS:<37}laptop offline  home-server offline  12 ms"),
            ),
            (79, format!("{WINDOWS:<62}2 offline  12 ms")),
            (50, format!("{WINDOWS:<44}12 ms")),
            (40, WINDOWS.to_owned()),
            (30, "[work@desktop]… 1:vim  2:logs".to_owned()),
        ] {
            assert_eq!(bottom(&status, width), expected, "width {width}");
        }
    }

    #[test]
    fn overflowing_windows_keep_the_cluster_details_and_the_active_window() {
        let mut status = remote();
        status.windows = (0..10).map(|index| tab(index, "w", index == 1)).collect();

        assert_eq!(
            bottom(&status, 60),
            format!(
                "{:<38}laptop offline  12 ms",
                "[work@desktop] 0:w  1:w  2:w  3:w …"
            )
        );
    }

    #[test]
    fn the_active_window_stays_visible_when_the_list_is_cut() {
        let status = StatusLine {
            session: "s".into(),
            server: "h".into(),
            local: true,
            windows: (0..10).map(|index| tab(index, "w", index == 7)).collect(),
            latency: None,
            offline: Vec::new(),
            message: None,
        };
        let parser = draw(&status, 30);
        assert_eq!(row_text(&parser, STATUS_ROW), "[s@h]… 6:w  7:w  8:w  9:w");
        let active = parser.screen().cell(STATUS_ROW, 12).unwrap();
        assert_eq!(active.contents(), "7");
        assert!(active.inverse());
    }

    #[test]
    fn tiny_terminals_keep_the_session_tag_readable() {
        for (width, expected) in [
            (1, "…"),
            (2, "[…"),
            (3, "[…]"),
            (5, "[wo…]"),
            (13, "[work@deskt…]"),
            (14, "[work@desktop]"),
            (16, "[work@desktop] …"),
            (20, "[work@desktop] 1:vi…"),
            (23, "[work@desktop]… 1:vim …"),
        ] {
            assert_eq!(bottom(&remote(), width), expected, "width {width}");
        }
    }

    #[test]
    fn a_long_session_name_is_cut_before_the_server_name() {
        let status = StatusLine {
            session: "a-very-long-session-name".into(),
            ..remote()
        };
        assert_eq!(bottom(&status, 20), "[a-very-lo…@desktop]");
    }

    #[test]
    fn wide_characters_are_cut_on_column_boundaries() {
        let status = StatusLine {
            session: "日本語".into(),
            server: "東京".into(),
            local: true,
            windows: vec![tab(0, "編集", true)],
            latency: None,
            offline: Vec::new(),
            message: None,
        };
        for (width, expected) in [
            (21, "[日本語@東京] 0:編集"),
            (20, "[日本語@東京] 0:編…"),
            (12, "[日本…@東京]"),
            (10, "[日本語@…]"),
            (4, "[…]…"),
        ] {
            assert_eq!(bottom(&status, width), expected, "width {width}");
        }
    }

    #[test]
    fn no_width_scrolls_or_wraps_the_terminal() {
        let mut wide = remote();
        wide.session = "セッション".into();
        wide.windows.push(tab(3, "編集", false));
        for status in [remote(), wide] {
            for width in 1..=100 {
                let parser = draw(&status, width);
                assert_eq!(row_text(&parser, 0), "T", "width {width}");
                assert_eq!(parser.screen().cursor_position(), (1, 0), "width {width}");
                assert!(!row_text(&parser, STATUS_ROW).is_empty(), "width {width}");
            }
        }
    }

    #[test]
    fn control_characters_in_names_are_not_sent_to_the_terminal() {
        let status = StatusLine {
            session: "a\x1b[31mb".into(),
            server: "h".into(),
            local: true,
            ..StatusLine::default()
        };
        let parser = draw(&status, 20);
        assert_eq!(row_text(&parser, STATUS_ROW), "[a?[31mb@h]");
        assert_eq!(
            parser.screen().cell(STATUS_ROW, 2).unwrap().fgcolor(),
            vt100::Color::Idx(0)
        );
    }

    #[test]
    fn window_tabs_come_from_window_summaries() {
        let summaries = [
            WindowSummary {
                index: 0,
                name: "sh".into(),
                panes: 1,
            },
            WindowSummary {
                index: 3,
                name: "vim".into(),
                panes: 2,
            },
        ];
        assert_eq!(
            WindowTab::list(&summaries, 3),
            vec![tab(0, "sh", false), tab(3, "vim", true)]
        );
    }

    #[test]
    fn latency_is_rounded_to_milliseconds() {
        assert_eq!(format_latency(Duration::from_micros(300)), "<1 ms");
        assert_eq!(format_latency(Duration::from_millis(12)), "12 ms");
    }

    #[test]
    fn a_custom_theme_colours_the_bar_and_layers_the_other_slots_on_it() {
        let bar = Color::Rgb(0x28, 0x28, 0x28);
        let theme = Theme {
            status: StyleSpec::colors(Color::Indexed(230), bar),
            status_session: StyleSpec {
                fg: Some(Color::Indexed(10)),
                ..StyleSpec::EMPTY
            },
            status_active_window: StyleSpec {
                underline: Some(true),
                ..StyleSpec::EMPTY
            },
            status_offline: StyleSpec {
                bg: Some(Color::MAGENTA),
                ..StyleSpec::EMPTY
            },
            ..Theme::default()
        };
        let parser = draw_with(&remote(), 80, &StatusSettings::default(), &theme);
        let screen = parser.screen();
        let cell = |col| screen.cell(STATUS_ROW, col).unwrap();

        assert_eq!(
            row_text(&parser, STATUS_ROW),
            format!("{WINDOWS:<58}laptop offline  12 ms")
        );
        assert_eq!(cell(1).fgcolor(), vt100::Color::Idx(10));
        assert_eq!(cell(1).bgcolor(), vt100::Color::Rgb(0x28, 0x28, 0x28));
        assert!(!cell(1).bold());
        assert_eq!(cell(15).fgcolor(), vt100::Color::Idx(230));
        assert!(!cell(15).underline());
        assert!(cell(21).underline());
        assert!(!cell(21).inverse() && !cell(21).bold());
        assert_eq!(cell(21).bgcolor(), vt100::Color::Rgb(0x28, 0x28, 0x28));
        assert_eq!(cell(58).bgcolor(), vt100::Color::Idx(5));
        assert_eq!(cell(58).fgcolor(), vt100::Color::Idx(230));
        assert_eq!(cell(79).bgcolor(), vt100::Color::Rgb(0x28, 0x28, 0x28));

        let theme = Theme {
            message: StyleSpec {
                italic: Some(true),
                ..StyleSpec::colors(Color::Indexed(15), Color::Rgb(0x80, 0, 0))
            },
            ..theme
        };
        let status = StatusLine {
            message: Some("gone".into()),
            ..remote()
        };
        let parser = draw_with(&status, 20, &StatusSettings::default(), &theme);
        let cell = parser.screen().cell(STATUS_ROW, 19).unwrap();
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(15));
        assert_eq!(cell.bgcolor(), vt100::Color::Rgb(0x80, 0, 0));
        assert!(cell.italic());
    }

    #[test]
    fn custom_formats_shape_the_tag_the_tabs_and_the_cluster_details() {
        let mut status = remote();
        assert_eq!(
            formatted(&status, 80),
            format!("{:<66} !laptop~12 ms", "<desktop:work>0|sh 1|vim 2|logs")
        );

        status.offline.push("home-server".into());
        assert_eq!(
            formatted(&status, 50),
            format!("{:<41} !2~12 ms", "<desktop:work>0|sh 1|vim 2|logs")
        );

        let parser = draw_with(&status, 80, &custom_formats(), &Theme::default());
        assert_eq!(
            parser.screen().cell(STATUS_ROW, 19).unwrap().contents(),
            "1"
        );
        assert!(parser.screen().cell(STATUS_ROW, 19).unwrap().inverse());
        assert!(!parser.screen().cell(STATUS_ROW, 14).unwrap().inverse());
    }

    #[test]
    fn a_wider_hidden_marker_takes_its_width_from_the_window_budget() {
        let status = StatusLine {
            session: "s".into(),
            server: "h".into(),
            local: true,
            windows: (0..10).map(|index| tab(index, "w", index == 7)).collect(),
            latency: None,
            offline: Vec::new(),
            message: None,
        };
        assert_eq!(formatted(&status, 30), "<h:s><>5|w 6|w 7|w 8|w 9|w");
        assert_eq!(formatted(&status, 16), "<h:s><>7|w <>");
    }

    #[test]
    fn a_custom_session_format_still_cuts_the_session_before_the_server() {
        let status = StatusLine {
            session: "a-very-long-session-name".into(),
            ..remote()
        };
        assert_eq!(formatted(&status, 20), "<desktop:a-very-lo…>");
        assert_eq!(formatted(&status, 12), "<desktop:a…>");
        assert_eq!(formatted(&status, 2), "<…");
    }
}
