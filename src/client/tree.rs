use std::collections::BTreeMap;
use std::mem;
use std::sync::Arc;

use super::chrome::{self, draw_row, Panel, PanelEvent, Placement, Rect, Span, Style};
use crate::keys::{Decoded, KeyDecoder};
use crate::project::ProjectId;
use crate::protocol::{ClientMessage, ServerStatus, ServerView, SessionInfo, WindowSummary};
use crate::settings::{Settings, Table, TreeAction};
use crate::target::Target;

const NO_PROJECT: &str = "(no project)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeEvent {
    Pending,
    Pick(Target),
    Cancel,
}

impl From<TreeEvent> for PanelEvent {
    fn from(event: TreeEvent) -> Self {
        match event {
            TreeEvent::Pending => Self::Pending,
            TreeEvent::Pick(target) => Self::Done(ClientMessage::Switch(target)),
            TreeEvent::Cancel => Self::Cancel,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Server,
    Project,
    Session(Target),
    Window(Target),
}

impl Entry {
    fn depth(&self) -> usize {
        match self {
            Self::Server => 0,
            Self::Project => 1,
            Self::Session(_) => 2,
            Self::Window(_) => 3,
        }
    }
}

#[derive(Debug, Clone)]
struct Node {
    entry: Entry,
    label: String,
    details: Vec<String>,
    stale: bool,
    expanded: bool,
    parent: Option<usize>,
    has_children: bool,
}

impl Node {
    fn new(entry: Entry, label: String, details: Vec<String>, stale: bool) -> Self {
        Self {
            expanded: !matches!(entry, Entry::Session(_)),
            entry,
            label,
            details,
            stale,
            parent: None,
            has_children: false,
        }
    }

    fn depth(&self) -> usize {
        self.entry.depth()
    }
}

#[derive(Debug)]
pub struct ClusterTree {
    nodes: Vec<Node>,
    cursor: usize,
    scroll: usize,
    keys: KeyDecoder,
    bindings: Table<TreeAction>,
    settings: Arc<Settings>,
}

impl ClusterTree {
    pub fn new(
        servers: &[ServerView],
        attached: Option<&Target>,
        bindings: Table<TreeAction>,
        settings: Arc<Settings>,
    ) -> Self {
        let nodes = build(servers);
        let cursor = attached
            .and_then(|attached| {
                nodes.iter().position(|node| {
                    matches!(&node.entry, Entry::Session(target) if is_attached(target, attached))
                })
            })
            .unwrap_or(0);
        Self {
            nodes,
            cursor,
            scroll: 0,
            keys: KeyDecoder::default(),
            bindings,
            settings,
        }
    }

    pub fn handle(&mut self, input: &[u8]) -> TreeEvent {
        for decoded in self.keys.feed(input) {
            if let Some(event) = self.press(&decoded) {
                return event;
            }
        }
        TreeEvent::Pending
    }

    pub fn time_out(&mut self) -> TreeEvent {
        self.keys
            .time_out()
            .and_then(|decoded| self.press(&decoded))
            .unwrap_or(TreeEvent::Pending)
    }

    fn press(&mut self, decoded: &Decoded) -> Option<TreeEvent> {
        self.bindings
            .resolve(decoded)
            .into_iter()
            .find_map(|(_, action)| action.and_then(|action| self.apply(action)))
    }

    fn apply(&mut self, action: TreeAction) -> Option<TreeEvent> {
        match action {
            TreeAction::Down => self.move_by(1),
            TreeAction::Up => self.move_by(-1),
            TreeAction::Top => self.move_by(isize::MIN),
            TreeAction::Bottom => self.move_by(isize::MAX),
            TreeAction::Collapse => self.collapse(),
            TreeAction::Expand => self.expand(),
            TreeAction::Pick => return self.pick(),
            TreeAction::Cancel => return Some(TreeEvent::Cancel),
        }
        None
    }

    fn visible(&self) -> Vec<usize> {
        let mut rows = Vec::new();
        let mut hidden_below = None;
        for (index, node) in self.nodes.iter().enumerate() {
            if hidden_below.is_some_and(|depth| node.depth() > depth) {
                continue;
            }
            hidden_below = (node.has_children && !node.expanded).then(|| node.depth());
            rows.push(index);
        }
        rows
    }

    fn move_by(&mut self, offset: isize) {
        let visible = self.visible();
        let Some(position) = visible.iter().position(|&index| index == self.cursor) else {
            return;
        };
        let target = position
            .saturating_add_signed(offset)
            .min(visible.len() - 1);
        self.cursor = visible[target];
    }

    fn collapse(&mut self) {
        let Some(node) = self.nodes.get_mut(self.cursor) else {
            return;
        };
        if node.has_children && node.expanded {
            node.expanded = false;
        } else if let Some(parent) = node.parent {
            self.cursor = parent;
        }
    }

    fn expand(&mut self) {
        if let Some(node) = self.nodes.get_mut(self.cursor) {
            node.expanded |= node.has_children;
        }
    }

    fn pick(&mut self) -> Option<TreeEvent> {
        let node = self.nodes.get_mut(self.cursor)?;
        match &node.entry {
            Entry::Session(target) | Entry::Window(target) if !node.stale => {
                Some(TreeEvent::Pick(target.clone()))
            }
            Entry::Server | Entry::Project if node.has_children => {
                node.expanded = !node.expanded;
                None
            }
            _ => None,
        }
    }

    fn scroll_into_view(&mut self, visible: &[usize], height: usize) {
        let position = visible
            .iter()
            .position(|&index| index == self.cursor)
            .unwrap_or(0);
        if position < self.scroll {
            self.scroll = position;
        } else if position >= self.scroll + height {
            self.scroll = position + 1 - height;
        }
        self.scroll = self.scroll.min(visible.len().saturating_sub(height));
    }

    fn row(&self, index: usize) -> (Vec<Span>, Style) {
        let node = &self.nodes[index];
        let theme = &self.settings.theme;
        let base = if node.stale {
            theme.tree.merge(theme.tree_stale)
        } else if node.entry == Entry::Server {
            theme.tree.merge(theme.tree_server)
        } else {
            theme.tree
        };
        let style = Style::from(if index == self.cursor {
            base.merge(theme.tree_cursor)
        } else {
            base
        });

        let tree = &self.settings.tree;
        let marker = if !node.has_children && node.entry != Entry::Server {
            &tree.leaf_marker
        } else if node.expanded {
            &tree.expanded_marker
        } else {
            &tree.collapsed_marker
        };
        let mut text = format!("{}{marker}{}", tree.indent.repeat(node.depth()), node.label);
        for detail in &node.details {
            text.push_str(&tree.detail_gap);
            text.push_str(detail);
        }
        (vec![Span::new(text, style)], style)
    }
}

impl Panel for ClusterTree {
    fn handle(&mut self, input: &[u8], _attached: &Target) -> PanelEvent {
        ClusterTree::handle(self, input).into()
    }

    fn time_out(&mut self, _attached: &Target) -> PanelEvent {
        ClusterTree::time_out(self).into()
    }

    fn is_partial(&self) -> bool {
        self.keys.is_partial()
    }

    fn render(&mut self, rect: Rect) -> Vec<u8> {
        let mut out = Vec::new();
        if rect.rows == 0 || rect.cols == 0 {
            return out;
        }
        let visible = self.visible();
        let height = usize::from(rect.rows);
        self.scroll_into_view(&visible, height);

        out.extend_from_slice(chrome::HIDE_CURSOR);
        for line in 0..height {
            let (spans, fill) = match visible.get(self.scroll + line) {
                Some(&index) => self.row(index),
                None => (Vec::new(), Style::from(self.settings.theme.tree)),
            };
            let row = usize::from(rect.row) + line;
            draw_row(
                &mut out,
                row,
                usize::from(rect.col),
                usize::from(rect.cols),
                &spans,
                fill,
            );
        }
        out
    }

    fn placement(&self) -> Option<Placement> {
        Some(Placement::SessionArea)
    }

    fn hides_session(&self) -> bool {
        true
    }
}

#[derive(Debug)]
pub struct Loading {
    pending: Vec<u8>,
    bindings: Table<TreeAction>,
    settings: Arc<Settings>,
}

impl Loading {
    pub fn new(bindings: Table<TreeAction>, settings: Arc<Settings>) -> Self {
        Self {
            pending: Vec::new(),
            bindings,
            settings,
        }
    }
}

impl Panel for Loading {
    fn handle(&mut self, input: &[u8], _attached: &Target) -> PanelEvent {
        self.pending.extend_from_slice(input);
        PanelEvent::Unchanged
    }

    fn time_out(&mut self, _attached: &Target) -> PanelEvent {
        PanelEvent::Unchanged
    }

    fn is_partial(&self) -> bool {
        false
    }

    fn render(&mut self, _area: Rect) -> Vec<u8> {
        Vec::new()
    }

    fn placement(&self) -> Option<Placement> {
        None
    }

    fn hides_session(&self) -> bool {
        true
    }

    fn cluster_listed(&mut self, servers: &[ServerView], attached: &Target) -> PanelEvent {
        let tree = ClusterTree::new(
            servers,
            Some(attached),
            self.bindings.clone(),
            Arc::clone(&self.settings),
        );
        PanelEvent::Replace(Box::new(tree), mem::take(&mut self.pending))
    }
}

fn build(servers: &[ServerView]) -> Vec<Node> {
    let mut ordered: Vec<&ServerView> = servers.iter().collect();
    ordered.sort_by(|a, b| (!is_local(a), &a.name).cmp(&(!is_local(b), &b.name)));

    let mut nodes = Vec::new();
    for server in ordered {
        let stale = !matches!(
            server.status,
            ServerStatus::Local | ServerStatus::Online { .. }
        );
        let details = vec![server_detail(&server.status)];
        nodes.push(Node::new(
            Entry::Server,
            server.name.clone(),
            details,
            stale,
        ));
        for (project, sessions) in project_groups(server) {
            nodes.push(Node::new(Entry::Project, project, Vec::new(), stale));
            for session in sessions {
                session_nodes(&mut nodes, server, session, stale);
            }
        }
    }
    link(&mut nodes);
    nodes
}

fn session_nodes(nodes: &mut Vec<Node>, server: &ServerView, session: &SessionInfo, stale: bool) {
    let target = Target {
        session: Some(session.name.clone()),
        server: Some(server.name.clone()),
        window: None,
        pane: None,
    };
    nodes.push(Node::new(
        Entry::Session(target.clone()),
        session.name.clone(),
        session_details(session, stale),
        stale,
    ));

    let mut windows: Vec<&WindowSummary> = session.windows.iter().collect();
    windows.sort_by_key(|window| window.index);
    for window in windows {
        let target = Target {
            window: u32::try_from(window.index).ok(),
            ..target.clone()
        };
        let label = format!("{}:{}", window.index, window.name);
        nodes.push(Node::new(
            Entry::Window(target),
            label,
            window_detail(window).into_iter().collect(),
            stale,
        ));
    }
}

fn link(nodes: &mut [Node]) {
    let mut ancestors: Vec<usize> = Vec::new();
    for index in 0..nodes.len() {
        ancestors.truncate(nodes[index].depth());
        if let Some(&parent) = ancestors.last() {
            nodes[index].parent = Some(parent);
            nodes[parent].has_children = true;
        }
        ancestors.push(index);
    }
}

fn project_groups(server: &ServerView) -> Vec<(String, Vec<&SessionInfo>)> {
    let mut groups: BTreeMap<(bool, String, Option<&str>), Vec<&SessionInfo>> = BTreeMap::new();
    for session in &server.sessions {
        let project = session.project.as_ref().map(ProjectId::as_str);
        let key = (project.is_none(), project_label(server, project), project);
        groups.entry(key).or_default().push(session);
    }
    groups
        .into_iter()
        .map(|((_, label, _), mut sessions)| {
            sessions.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
            (label, sessions)
        })
        .collect()
}

fn project_label(server: &ServerView, project: Option<&str>) -> String {
    let Some(project) = project else {
        return NO_PROJECT.to_owned();
    };
    match server
        .projects
        .iter()
        .find(|checkout| checkout.id.as_str() == project)
    {
        Some(checkout) => checkout.name.clone(),
        None => project
            .rsplit('/')
            .find(|part| !part.is_empty())
            .unwrap_or(project)
            .to_owned(),
    }
}

fn server_detail(status: &ServerStatus) -> String {
    match status {
        ServerStatus::Local => "(this server)".to_owned(),
        ServerStatus::Online {
            latency: Some(latency),
        } => chrome::format_latency(*latency),
        ServerStatus::Online { latency: None } => "online".to_owned(),
        ServerStatus::Offline { stopped: true, .. } => "offline (stopped)".to_owned(),
        ServerStatus::Offline { .. } => "offline".to_owned(),
        ServerStatus::Incompatible { .. } => "incompatible".to_owned(),
    }
}

fn session_details(session: &SessionInfo, stale: bool) -> Vec<String> {
    let windows = match session.windows.len() {
        1 => "1 window".to_owned(),
        count => format!("{count} windows"),
    };
    let flag = if stale {
        Some("stale")
    } else {
        (session.attached_clients > 0).then_some("attached")
    };
    [windows]
        .into_iter()
        .chain(flag.map(str::to_owned))
        .collect()
}

fn window_detail(window: &WindowSummary) -> Option<String> {
    match window.panes {
        0 | 1 => None,
        panes => Some(format!("{panes} panes")),
    }
}

fn is_local(server: &ServerView) -> bool {
    matches!(server.status, ServerStatus::Local)
}

fn is_attached(target: &Target, attached: &Target) -> bool {
    target.session == attached.session
        && (attached.server.is_none() || target.server == attached.server)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::client::chrome::testing::{screen_text, terminal};
    use crate::protocol::{ProjectCheckout, SessionId, Version};
    use crate::settings::{Keymap, Settings};

    const AMUX: &str = "github.com/blendonl/amux";

    fn window(index: usize, name: &str, panes: usize) -> WindowSummary {
        WindowSummary {
            index,
            name: name.into(),
            panes,
        }
    }

    fn session(name: &str, project: Option<&str>, windows: Vec<WindowSummary>) -> SessionInfo {
        SessionInfo {
            id: SessionId(1),
            name: name.into(),
            windows,
            attached_clients: 0,
            last_activity: SystemTime::UNIX_EPOCH,
            project: project.map(Into::into),
            branch: None,
        }
    }

    fn checkout(id: &str, name: &str) -> ProjectCheckout {
        ProjectCheckout {
            id: id.into(),
            name: name.into(),
            path: format!("/src/{name}").into(),
            origin: None,
        }
    }

    fn server(name: &str, status: ServerStatus, sessions: Vec<SessionInfo>) -> ServerView {
        ServerView {
            id: None,
            name: name.into(),
            address: None,
            version: None,
            status,
            sessions,
            projects: Vec::new(),
        }
    }

    fn cluster() -> Vec<ServerView> {
        let mut main = session(
            "amux/main",
            Some(AMUX),
            vec![window(1, "vim", 2), window(0, "sh", 1)],
        );
        main.attached_clients = 1;
        let mut desktop = server(
            "desktop",
            ServerStatus::Local,
            vec![
                session("scratch", None, vec![window(0, "sh", 1)]),
                main,
                session(
                    "api/main",
                    Some("github.com/me/api"),
                    vec![window(0, "sh", 1)],
                ),
                session("amux/feature-x", Some(AMUX), vec![window(0, "sh", 1)]),
            ],
        );
        desktop.projects = vec![checkout(AMUX, "amux")];

        let mut laptop = server(
            "laptop",
            ServerStatus::Online {
                latency: Some(Duration::from_millis(12)),
            },
            vec![session(
                "notes/main",
                Some("notes-id"),
                vec![window(0, "sh", 1)],
            )],
        );
        laptop.projects = vec![checkout("notes-id", "notes")];

        let mut home = server(
            "home-server",
            ServerStatus::Offline {
                last_seen: None,
                stopped: false,
            },
            vec![session(
                "infra/main",
                Some("github.com/me/infra"),
                vec![window(0, "sh", 1), window(1, "logs", 1)],
            )],
        );
        home.projects = vec![checkout("github.com/me/infra", "infra")];

        let attic = server(
            "attic",
            ServerStatus::Incompatible {
                version: Version {
                    release: "9.0.0".into(),
                    major: 9,
                    minor: 0,
                },
            },
            Vec::new(),
        );
        vec![laptop, home, desktop, attic]
    }

    fn attached_tree() -> ClusterTree {
        let attached: Target = "amux/main@desktop".parse().unwrap();
        ClusterTree::new(
            &cluster(),
            Some(&attached),
            Keymap::default().tree,
            Arc::new(Settings::default()),
        )
    }

    fn lines(tree: &mut ClusterTree, rows: u16, cols: u16) -> Vec<String> {
        let mut parser = terminal(rows, cols);
        parser.process(&tree.render(Rect {
            row: 0,
            col: 0,
            rows,
            cols,
        }));
        screen_text(&parser)
    }

    fn current(tree: &ClusterTree) -> &str {
        &tree.nodes[tree.cursor].label
    }

    fn press(tree: &mut ClusterTree, keys: &[u8]) {
        assert_eq!(tree.handle(keys), TreeEvent::Pending);
    }

    fn picked(tree: &mut ClusterTree) -> String {
        match tree.handle(b"\r") {
            TreeEvent::Pick(target) => target.to_string(),
            other => panic!("expected a pick, got {other:?}"),
        }
    }

    #[test]
    fn servers_projects_sessions_and_windows_are_grouped_and_ordered() {
        let mut tree = attached_tree();
        assert_eq!(
            lines(&mut tree, 16, 60),
            vec![
                "- desktop  (this server)",
                "  - amux",
                "    + amux/feature-x  1 window",
                "    + amux/main  2 windows  attached",
                "  - api",
                "    + api/main  1 window",
                "  - (no project)",
                "    + scratch  1 window",
                "- attic  incompatible",
                "- home-server  offline",
                "  - infra",
                "    + infra/main  2 windows  stale",
                "- laptop  12 ms",
                "  - notes",
                "    + notes/main  1 window",
                "",
            ]
        );
    }

    #[test]
    fn the_cursor_starts_on_the_attached_session_and_is_highlighted() {
        let mut tree = attached_tree();
        assert_eq!(current(&tree), "amux/main");

        let mut parser = terminal(16, 40);
        parser.process(&tree.render(Rect {
            row: 0,
            col: 0,
            rows: 16,
            cols: 40,
        }));
        let screen = parser.screen();
        assert!(screen.cell(3, 6).unwrap().inverse());
        assert!(screen.cell(3, 39).unwrap().inverse());
        assert!(!screen.cell(2, 6).unwrap().inverse());
        assert!(screen.cell(0, 2).unwrap().bold());
        assert!(screen.cell(9, 2).unwrap().dim());
        assert!(screen.cell(11, 6).unwrap().dim());
        assert!(screen.hide_cursor());
    }

    #[test]
    fn without_an_attached_session_the_cursor_starts_at_the_top() {
        assert_eq!(
            current(&ClusterTree::new(
                &cluster(),
                None,
                Keymap::default().tree,
                Arc::new(Settings::default())
            )),
            "desktop"
        );
        let elsewhere: Target = "amux/main@laptop".parse().unwrap();
        assert_eq!(
            current(&ClusterTree::new(
                &cluster(),
                Some(&elsewhere),
                Keymap::default().tree,
                Arc::new(Settings::default())
            )),
            "desktop"
        );
        let unqualified: Target = "notes/main".parse().unwrap();
        assert_eq!(
            current(&ClusterTree::new(
                &cluster(),
                Some(&unqualified),
                Keymap::default().tree,
                Arc::new(Settings::default())
            )),
            "notes/main"
        );
    }

    #[test]
    fn vertical_keys_walk_the_visible_rows_and_stop_at_the_ends() {
        let mut tree = attached_tree();
        let steps: [(&[u8], &str); 10] = [
            (b"j", "api"),
            (b"\x1b[B", "api/main"),
            (b"k", "api"),
            (b"\x1bOA", "amux/main"),
            (b"kkkkkkkk", "desktop"),
            (b"G", "notes/main"),
            (b"j", "notes/main"),
            (b"g", "desktop"),
            (b"\x1b[F", "notes/main"),
            (b"\x1b[H", "desktop"),
        ];
        for (keys, expected) in steps {
            press(&mut tree, keys);
            assert_eq!(current(&tree), expected, "after {keys:?}");
        }
    }

    #[test]
    fn horizontal_keys_expand_collapse_and_climb_to_the_parent() {
        let mut tree = attached_tree();
        press(&mut tree, b"l");
        press(&mut tree, b"j");
        assert_eq!(current(&tree), "0:sh");
        press(&mut tree, b"j");
        assert_eq!(current(&tree), "1:vim");
        assert_eq!(
            lines(&mut tree, 7, 60)[3..],
            [
                "    - amux/main  2 windows  attached",
                "        0:sh",
                "        1:vim  2 panes",
                "  - api",
            ]
        );

        press(&mut tree, b"h");
        assert_eq!(current(&tree), "amux/main");
        press(&mut tree, b"\x1b[D");
        assert_eq!(current(&tree), "amux/main");
        assert_eq!(lines(&mut tree, 5, 60)[4], "  - api");

        press(&mut tree, b"h");
        assert_eq!(current(&tree), "amux");
        press(&mut tree, b"h");
        assert_eq!(
            lines(&mut tree, 3, 60),
            vec!["- desktop  (this server)", "  + amux", "  - api"]
        );

        press(&mut tree, b"\x1b[C");
        assert_eq!(lines(&mut tree, 3, 60)[2], "    + amux/feature-x  1 window");

        press(&mut tree, b"hh");
        assert_eq!(current(&tree), "desktop");
        assert_eq!(
            lines(&mut tree, 2, 60),
            vec!["- desktop  (this server)", "  + amux"]
        );
        press(&mut tree, b"h");
        assert_eq!(current(&tree), "desktop");
        assert_eq!(
            lines(&mut tree, 2, 60),
            vec!["+ desktop  (this server)", "- attic  incompatible"]
        );
        press(&mut tree, b"h");
        assert_eq!(current(&tree), "desktop");
        press(&mut tree, b"\r");
        assert_eq!(lines(&mut tree, 2, 60)[1], "  + amux");
    }

    #[test]
    fn picking_yields_session_and_window_targets() {
        let mut tree = attached_tree();
        assert_eq!(picked(&mut tree), "amux/main@desktop");

        press(&mut tree, b"ljj");
        assert_eq!(picked(&mut tree), "amux/main@desktop:1");

        press(&mut tree, b"G");
        assert_eq!(picked(&mut tree), "notes/main@laptop");
    }

    #[test]
    fn stale_sessions_on_offline_servers_cannot_be_picked() {
        let mut tree = attached_tree();
        press(&mut tree, b"G");
        press(&mut tree, b"kkk");
        assert_eq!(current(&tree), "infra/main");
        assert_eq!(tree.handle(b"\r"), TreeEvent::Pending);

        press(&mut tree, b"lj");
        assert_eq!(current(&tree), "0:sh");
        assert_eq!(tree.handle(b"\r"), TreeEvent::Pending);
    }

    #[test]
    fn escape_q_and_ctrl_c_cancel() {
        assert_eq!(attached_tree().handle(b"q"), TreeEvent::Cancel);
        assert_eq!(attached_tree().handle(b"\x03"), TreeEvent::Cancel);
        assert_eq!(attached_tree().handle(b"\x1bj"), TreeEvent::Cancel);

        let mut tree = attached_tree();
        assert_eq!(tree.handle(b"\x1b"), TreeEvent::Pending);
        assert!(tree.is_partial());
        assert_eq!(tree.time_out(), TreeEvent::Cancel);
    }

    #[test]
    fn arrow_keys_split_across_chunks_still_move() {
        let mut tree = attached_tree();
        press(&mut tree, b"\x1b");
        press(&mut tree, b"[");
        press(&mut tree, b"B");
        assert_eq!(current(&tree), "api");
        assert!(!tree.is_partial());
        assert_eq!(tree.time_out(), TreeEvent::Pending);
    }

    #[test]
    fn the_viewport_scrolls_to_keep_the_cursor_visible() {
        let mut tree = attached_tree();
        let viewport = |tree: &mut ClusterTree| {
            let rows = lines(tree, 4, 40);
            rows.iter()
                .map(|row| row.trim_start().to_owned())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            viewport(&mut tree),
            vec![
                "- desktop  (this server)",
                "- amux",
                "+ amux/feature-x  1 window",
                "+ amux/main  2 windows  attached",
            ]
        );

        press(&mut tree, b"j");
        assert_eq!(viewport(&mut tree)[3], "- api");

        press(&mut tree, b"jjjjj");
        assert_eq!(current(&tree), "home-server");
        assert_eq!(viewport(&mut tree)[0], "- (no project)");

        press(&mut tree, b"kkk");
        assert_eq!(viewport(&mut tree)[0], "- (no project)");
        press(&mut tree, b"k");
        assert_eq!(viewport(&mut tree)[0], "+ api/main  1 window");

        press(&mut tree, b"G");
        assert_eq!(viewport(&mut tree)[0], "+ infra/main  2 windows  stale");

        press(&mut tree, b"hh");
        assert_eq!(current(&tree), "notes");
        assert_eq!(
            viewport(&mut tree),
            vec![
                "- infra",
                "+ infra/main  2 windows  stale",
                "- laptop  12 ms",
                "+ notes",
            ]
        );

        press(&mut tree, b"g");
        assert_eq!(viewport(&mut tree)[0], "- desktop  (this server)");
    }

    #[test]
    fn the_overlay_covers_exactly_its_area_and_truncates_rows() {
        let mut parser = terminal(6, 20);
        for row in 1..=6 {
            parser.process(format!("\x1b[{row};1H{}", ".".repeat(20)).as_bytes());
        }
        let mut tree = attached_tree();
        parser.process(&tree.render(Rect {
            row: 1,
            col: 2,
            rows: 4,
            cols: 12,
        }));

        assert_eq!(
            screen_text(&parser),
            vec![
                "....................",
                "..- desktop  …......",
                "..  - amux    ......",
                "..    + amux/…......",
                "..    + amux/…......",
                "....................",
            ]
        );
    }

    #[test]
    fn an_empty_cluster_renders_blank_and_ignores_keys() {
        let mut tree = ClusterTree::new(
            &[],
            None,
            Keymap::default().tree,
            Arc::new(Settings::default()),
        );
        assert_eq!(lines(&mut tree, 2, 10), vec!["", ""]);
        assert_eq!(tree.handle(b"jkhl\r"), TreeEvent::Pending);
        assert_eq!(tree.handle(b"q"), TreeEvent::Cancel);
        assert!(tree.render(Rect::default()).is_empty());
    }
}
