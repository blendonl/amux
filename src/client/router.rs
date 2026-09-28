use std::collections::{BTreeMap, VecDeque};
use std::mem;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use crate::keys::{Decoded, Key, KeyDecoder, Scanner, ESC};
use crate::protocol::SessionCommand;
use crate::settings::{Binding, CallbackId, Keymap, PREFIX_TABLE, ROOT_TABLE};

const TRIE_ROOT: usize = 0;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Forward(Vec<u8>),
    Detach,
    Command(SessionCommand),
    Open(Panel),
    Callback(CallbackId),
    ReloadConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    RenameWindow,
    RenameSession,
    ClusterTree,
}

pub struct KeyRouter {
    prefix: Key,
    keymap: Arc<Keymap>,
    escape_time: Duration,
    trie: Trie,
    scanner: Scanner,
    held: Held,
    table: Option<String>,
    decoder: KeyDecoder,
    escape_at: Option<Instant>,
}

impl KeyRouter {
    pub fn new(prefix: Key, keymap: Arc<Keymap>, escape_time: Duration) -> Self {
        Self {
            prefix,
            trie: Trie::root(prefix, &keymap),
            keymap,
            escape_time,
            scanner: Scanner::default(),
            held: Held::default(),
            table: None,
            decoder: KeyDecoder::default(),
            escape_at: None,
        }
    }

    pub fn route(&mut self, input: &[u8]) -> (Vec<Action>, Vec<u8>) {
        self.run(input.iter().copied().collect(), false)
    }

    pub fn escape_deadline(&self) -> Option<Instant> {
        self.escape_at
    }

    pub fn time_out(&mut self) -> (Vec<Action>, Vec<u8>) {
        self.run(VecDeque::new(), true)
    }

    pub fn flush(&mut self) -> Vec<u8> {
        self.escape_at = None;
        mem::take(&mut self.held).bytes
    }

    pub fn run_binding(&mut self, binding: Binding) -> Vec<Action> {
        let mut routed = Routed::default();
        if let Some(action) = self.perform(binding, &mut routed) {
            routed.act(action);
        }
        routed.finish()
    }

    pub fn set_keymap(&mut self, keymap: Arc<Keymap>) -> Vec<u8> {
        let held = self.flush();
        self.trie = Trie::root(self.prefix, &keymap);
        self.table = self
            .table
            .take()
            .filter(|table| keymap.table(table).is_some());
        self.keymap = keymap;
        held
    }

    fn run(&mut self, mut queue: VecDeque<u8>, expire: bool) -> (Vec<Action>, Vec<u8>) {
        let mut routed = Routed::default();
        loop {
            let binding = if self.table.is_some() {
                if queue.is_empty() {
                    break;
                }
                self.table_input(&mut queue, &mut routed)
            } else if let Some(byte) = queue.pop_front() {
                self.root_byte(byte, &mut queue, &mut routed)
            } else if expire && self.holds_escape() {
                self.release(&mut queue, &mut routed, None)
            } else {
                break;
            };
            if let Some(action) = binding.and_then(|binding| self.perform(binding, &mut routed)) {
                routed.act(action);
                break;
            }
        }
        self.escape_at = self
            .holds_escape()
            .then(|| Instant::now() + self.escape_time);
        (routed.finish(), queue.into())
    }

    fn root_byte(
        &mut self,
        byte: u8,
        queue: &mut VecDeque<u8>,
        routed: &mut Routed,
    ) -> Option<Binding> {
        let holding = !self.held.bytes.is_empty();
        if !holding && (self.scanner.is_pasting() || !self.scanner.starts_sequence(byte)) {
            self.pass(byte, routed);
            return None;
        }
        let Some(node) = self.trie.step(self.held.node, byte) else {
            if !holding {
                self.pass(byte, routed);
                return None;
            }
            queue.push_front(byte);
            return self.release(queue, routed, Some(byte));
        };
        self.held.bytes.push(byte);
        self.held.node = node;
        if self.trie.binding(node).is_some() {
            self.held.matched = Some((self.held.bytes.len(), node));
        }
        if self.trie.is_leaf(node) && self.completes(&self.held.bytes) {
            return self.release(queue, routed, None);
        }
        None
    }

    fn release(
        &mut self,
        queue: &mut VecDeque<u8>,
        routed: &mut Routed,
        next: Option<u8>,
    ) -> Option<Binding> {
        let held = mem::take(&mut self.held);
        if let Some((len, node)) = held.matched {
            let scanner = self.scanner_after(&held.bytes[..len]);
            let after = held.bytes.get(len).copied().or(next);
            if after.is_none_or(|byte| scanner.starts_sequence(byte)) {
                requeue(queue, &held.bytes[len..]);
                self.scanner = scanner;
                return self.trie.binding(node).cloned();
            }
        }
        requeue(queue, &held.bytes[1..]);
        self.pass(held.bytes[0], routed);
        None
    }

    fn completes(&self, bytes: &[u8]) -> bool {
        bytes.first() != Some(&ESC) || self.scanner_after(bytes).is_ground()
    }

    fn scanner_after(&self, bytes: &[u8]) -> Scanner {
        let mut scanner = self.scanner.clone();
        for &byte in bytes {
            scanner.feed(byte);
        }
        scanner
    }

    fn pass(&mut self, byte: u8, routed: &mut Routed) {
        self.scanner.feed(byte);
        routed.forward.push(byte);
    }

    fn holds_escape(&self) -> bool {
        !self.held.bytes.is_empty() && self.held.bytes.iter().all(|&byte| byte == ESC)
    }

    fn table_input(&mut self, queue: &mut VecDeque<u8>, routed: &mut Routed) -> Option<Binding> {
        self.decoder.push(queue.make_contiguous());
        queue.clear();
        let decoded = self.decoder.next_key()?;
        requeue(queue, &self.decoder.take_pending());
        let table = self.table.take()?;
        self.table_key(&table, decoded, queue, routed)
    }

    fn table_key(
        &self,
        table: &str,
        decoded: Decoded,
        queue: &mut VecDeque<u8>,
        routed: &mut Routed,
    ) -> Option<Binding> {
        let bound = self
            .keymap
            .table(table)
            .zip(decoded.key)
            .and_then(|(bindings, key)| bindings.get(&key));
        if let Some(binding) = bound {
            return Some(binding.clone());
        }
        if self.scanner_after(&decoded.raw).is_pasting() {
            requeue(queue, &decoded.raw);
            return None;
        }
        if table == PREFIX_TABLE && decoded.key == Some(self.prefix) {
            routed.forward.extend(decoded.raw);
            return None;
        }
        if let Some((escape, rest)) = decoded.split_escape() {
            requeue(queue, &rest.raw);
            return self.table_key(table, escape, queue, routed);
        }
        None
    }

    fn perform(&mut self, binding: Binding, routed: &mut Routed) -> Option<Action> {
        Some(match binding {
            Binding::SendPrefix => {
                let encoding = self.prefix.encodings().into_iter().next();
                routed.forward.extend(encoding.unwrap_or_default());
                return None;
            }
            Binding::SwitchTable(table) => {
                let known = table != ROOT_TABLE && self.keymap.table(&table).is_some();
                self.table = known.then_some(table);
                return None;
            }
            Binding::Callback(id) => Action::Callback(id),
            Binding::ReloadConfig => Action::ReloadConfig,
            Binding::Detach => Action::Detach,
            Binding::RenameWindow => Action::Open(Panel::RenameWindow),
            Binding::RenameSession => Action::Open(Panel::RenameSession),
            Binding::ClusterTree => Action::Open(Panel::ClusterTree),
            Binding::NewWindow => Action::Command(SessionCommand::NewWindow),
            Binding::NextWindow => Action::Command(SessionCommand::NextWindow),
            Binding::PreviousWindow => Action::Command(SessionCommand::PreviousWindow),
            Binding::SelectWindow(index) => Action::Command(SessionCommand::SelectWindow(index)),
            Binding::SplitPane(split) => Action::Command(SessionCommand::SplitPane(split)),
            Binding::NextPane => Action::Command(SessionCommand::NextPane),
            Binding::SelectPane(direction) => {
                Action::Command(SessionCommand::SelectPane(direction))
            }
            Binding::KillPane => Action::Command(SessionCommand::KillPane),
            Binding::KillWindow => Action::Command(SessionCommand::KillWindow),
        })
    }
}

fn requeue(queue: &mut VecDeque<u8>, bytes: &[u8]) {
    for &byte in bytes.iter().rev() {
        queue.push_front(byte);
    }
}

#[derive(Default)]
struct Routed {
    actions: Vec<Action>,
    forward: Vec<u8>,
}

impl Routed {
    fn act(&mut self, action: Action) {
        self.flush();
        self.actions.push(action);
    }

    fn flush(&mut self) {
        if !self.forward.is_empty() {
            self.actions
                .push(Action::Forward(mem::take(&mut self.forward)));
        }
    }

    fn finish(mut self) -> Vec<Action> {
        self.flush();
        self.actions
    }
}

#[derive(Debug, Default)]
struct Held {
    bytes: Vec<u8>,
    node: usize,
    matched: Option<(usize, usize)>,
}

struct Trie {
    nodes: Vec<TrieNode>,
}

#[derive(Default)]
struct TrieNode {
    next: BTreeMap<u8, usize>,
    binding: Option<Binding>,
}

impl Trie {
    fn root(prefix: Key, keymap: &Keymap) -> Self {
        let enter_prefix = (prefix, Binding::SwitchTable(PREFIX_TABLE.to_owned()));
        Self::new(
            keymap
                .root
                .iter()
                .map(|(key, binding)| (*key, binding.clone()))
                .chain([enter_prefix]),
        )
    }

    fn new(bindings: impl IntoIterator<Item = (Key, Binding)>) -> Self {
        let mut trie = Self {
            nodes: vec![TrieNode::default()],
        };
        for (key, binding) in bindings {
            for encoding in key.encodings() {
                trie.insert(&encoding, binding.clone());
            }
        }
        trie
    }

    fn insert(&mut self, bytes: &[u8], binding: Binding) {
        let mut node = TRIE_ROOT;
        for &byte in bytes {
            node = match self.nodes[node].next.get(&byte) {
                Some(&next) => next,
                None => {
                    let next = self.nodes.len();
                    self.nodes.push(TrieNode::default());
                    self.nodes[node].next.insert(byte, next);
                    next
                }
            };
        }
        self.nodes[node].binding = Some(binding);
    }

    fn step(&self, node: usize, byte: u8) -> Option<usize> {
        self.nodes[node].next.get(&byte).copied()
    }

    fn binding(&self, node: usize) -> Option<&Binding> {
        self.nodes[node].binding.as_ref()
    }

    fn is_leaf(&self, node: usize) -> bool {
        self.nodes[node].next.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Direction, Split};
    use crate::settings::Settings;

    const DEFAULT_PREFIX: u8 = 0x02;
    const ESCAPE_TIME: Duration = Duration::from_millis(50);

    fn router() -> KeyRouter {
        KeyRouter::new(
            Settings::default().prefix,
            Arc::new(Keymap::default()),
            Settings::default().escape_time(),
        )
    }

    fn route(router: &mut KeyRouter, input: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut input = input.to_vec();
        loop {
            let (routed, rest) = router.route(&input);
            actions.extend(routed);
            if rest.is_empty() {
                return actions;
            }
            input = rest;
        }
    }

    fn routed(input: &[u8]) -> Vec<Action> {
        route(&mut router(), input)
    }

    #[test]
    fn panel_keys_open_a_panel_and_hand_it_the_rest_of_the_input() {
        for (key, panel) in [
            (b',', Panel::RenameWindow),
            (b'$', Panel::RenameSession),
            (b's', Panel::ClusterTree),
        ] {
            let input = [b'a', DEFAULT_PREFIX, key, b'x', DEFAULT_PREFIX, b'd'];
            let mut router = router();
            let (actions, rest) = router.route(&input);
            assert_eq!(
                actions,
                vec![Action::Forward(b"a".to_vec()), Action::Open(panel)],
                "key {:?}",
                char::from(key)
            );
            assert_eq!(rest, [b'x', DEFAULT_PREFIX, b'd']);
            assert_eq!(
                route(&mut router, b"y"),
                vec![Action::Forward(b"y".to_vec())]
            );
        }
    }

    fn command(command: SessionCommand) -> Action {
        Action::Command(command)
    }

    #[test]
    fn plain_input_is_forwarded_untouched() {
        assert_eq!(
            routed(b"ls -la\r"),
            vec![Action::Forward(b"ls -la\r".to_vec())]
        );
    }

    #[test]
    fn prefix_then_detach_key_detaches_after_flushing_earlier_input() {
        assert_eq!(
            routed(b"ab\x02d"),
            vec![Action::Forward(b"ab".to_vec()), Action::Detach]
        );
    }

    #[test]
    fn double_prefix_sends_a_literal_prefix() {
        assert_eq!(
            routed(b"\x02\x02"),
            vec![Action::Forward(vec![DEFAULT_PREFIX])]
        );
    }

    #[test]
    fn prefix_state_carries_across_chunks() {
        let mut router = router();
        assert_eq!(route(&mut router, b"\x02"), vec![]);
        assert_eq!(route(&mut router, b"d"), vec![Action::Detach]);
    }

    #[test]
    fn unbound_keys_after_prefix_are_swallowed() {
        assert_eq!(routed(b"\x02zx"), vec![Action::Forward(b"x".to_vec())]);
    }

    #[test]
    fn window_and_pane_keys_become_commands() {
        let bindings = [
            (b'c', SessionCommand::NewWindow),
            (b'n', SessionCommand::NextWindow),
            (b'p', SessionCommand::PreviousWindow),
            (b'0', SessionCommand::SelectWindow(0)),
            (b'7', SessionCommand::SelectWindow(7)),
            (b'%', SessionCommand::SplitPane(Split::LeftRight)),
            (b'"', SessionCommand::SplitPane(Split::TopBottom)),
            (b'o', SessionCommand::NextPane),
            (b'x', SessionCommand::KillPane),
            (b'&', SessionCommand::KillWindow),
        ];
        for (key, bound) in bindings {
            assert_eq!(
                routed(&[b'a', DEFAULT_PREFIX, key, b'b']),
                vec![
                    Action::Forward(b"a".to_vec()),
                    command(bound),
                    Action::Forward(b"b".to_vec()),
                ],
                "key {:?}",
                char::from(key)
            );
        }
    }

    #[test]
    fn arrow_keys_after_prefix_select_a_pane() {
        for (keys, direction) in [
            (&b"\x02\x1b[A"[..], Direction::Up),
            (b"\x02\x1b[B", Direction::Down),
            (b"\x02\x1bOC", Direction::Right),
            (b"\x02\x1bOD", Direction::Left),
        ] {
            assert_eq!(
                routed(keys),
                vec![command(SessionCommand::SelectPane(direction))]
            );
        }
    }

    #[test]
    fn an_arrow_split_across_chunks_still_selects_a_pane() {
        let mut router = router();
        assert_eq!(route(&mut router, b"\x02\x1b"), vec![]);
        assert_eq!(route(&mut router, b"["), vec![]);
        assert_eq!(
            route(&mut router, b"Cls"),
            vec![
                command(SessionCommand::SelectPane(Direction::Right)),
                Action::Forward(b"ls".to_vec()),
            ]
        );
    }

    #[test]
    fn other_sequences_after_prefix_are_swallowed() {
        assert_eq!(
            routed(b"\x02\x1b[1;5Aa\x02\x1b[<0;3;4Mb"),
            vec![Action::Forward(b"ab".to_vec())]
        );
    }

    #[test]
    fn a_lone_escape_after_prefix_leaves_the_next_key_alone() {
        assert_eq!(routed(b"\x02\x1bx"), vec![Action::Forward(b"x".to_vec())]);
        assert_eq!(routed(b"\x02\x1b\x02d"), vec![Action::Detach]);
    }

    fn key(notation: &str) -> Key {
        notation.parse().unwrap()
    }

    fn router_with(prefix: &str, root: &[(&str, Binding)]) -> KeyRouter {
        let mut keymap = Keymap::default();
        for (notation, binding) in root {
            keymap.root.insert(key(notation), binding.clone());
        }
        KeyRouter::new(key(prefix), Arc::new(keymap), ESCAPE_TIME)
    }

    fn forward(bytes: &[u8]) -> Action {
        Action::Forward(bytes.to_vec())
    }

    fn select_left() -> Binding {
        Binding::SelectPane(Direction::Left)
    }

    #[test]
    fn the_default_keymap_never_holds_input_back() {
        let mut router = router();
        for input in [
            &b"\x1b"[..],
            b"\x1b[",
            b"\x1b[<0;3;4M\x1b[M !!",
            b"\x1bO",
            b"\xe6\x97",
        ] {
            assert_eq!(router.route(input), (vec![forward(input)], Vec::new()));
            assert_eq!(router.escape_deadline(), None);
        }
    }

    #[test]
    fn a_pasted_prefix_reaches_the_pane() {
        let mut router = router();
        let paste = b"\x1b[200~ab\x02dcd\x1b[201~";
        assert_eq!(router.route(paste), (vec![forward(paste)], Vec::new()));
        assert_eq!(route(&mut router, b"\x02d"), vec![Action::Detach]);
    }

    #[test]
    fn a_paste_split_across_chunks_suppresses_bindings_until_it_ends() {
        let mut router = router_with("C-b", &[("M-h", select_left())]);
        let mut forwarded = Vec::new();
        for chunk in [&b"\x1b[20"[..], b"0~a\x1bh\x02", b"d\x1b", b"[201~"] {
            for action in route(&mut router, chunk) {
                match action {
                    Action::Forward(bytes) => forwarded.extend(bytes),
                    other => panic!("unexpected {other:?}"),
                }
            }
            assert_eq!(router.escape_deadline(), None);
        }
        assert_eq!(forwarded, b"\x1b[200~a\x1bh\x02d\x1b[201~");
        assert_eq!(
            route(&mut router, b"\x1bh"),
            vec![command(SessionCommand::SelectPane(Direction::Left))]
        );
    }

    #[test]
    fn a_paste_after_the_prefix_cancels_it_and_reaches_the_pane_whole() {
        let paste = b"\x1b[200~x\x02d\x1b[201~";
        let mut input = b"\x02".to_vec();
        input.extend(paste);
        assert_eq!(routed(&input), vec![forward(paste)]);
    }

    #[test]
    fn a_mouse_report_after_the_prefix_is_swallowed_with_its_payload() {
        assert_eq!(routed(b"\x02\x1b[M !!a"), vec![forward(b"a")]);
        let mut router = router();
        assert_eq!(route(&mut router, b"\x02\x1b[M"), vec![]);
        assert_eq!(route(&mut router, b" !!"), vec![]);
        assert_eq!(route(&mut router, b"b"), vec![forward(b"b")]);
    }

    #[test]
    fn a_meta_prefix_is_matched_on_its_escape_sequence() {
        let mut router = router_with("M-a", &[]);
        assert_eq!(
            route(&mut router, b"x\x1bad"),
            vec![forward(b"x"), Action::Detach]
        );
        assert_eq!(route(&mut router, b"\x1ba\x1ba"), vec![forward(b"\x1ba")]);
        assert_eq!(route(&mut router, b"\x02d"), vec![forward(b"\x02d")]);
        assert_eq!(
            route(&mut router, b"\x1b[A\x1bb\x1b\x1ba%"),
            vec![
                forward(b"\x1b[A\x1bb\x1b"),
                command(SessionCommand::SplitPane(Split::LeftRight)),
            ]
        );
    }

    #[test]
    fn a_function_key_prefix_is_matched_across_chunks() {
        let mut router = router_with("F12", &[]);
        assert_eq!(
            route(&mut router, b"\x1b[24~c"),
            vec![command(SessionCommand::NewWindow)]
        );
        assert_eq!(
            route(&mut router, b"\x1b[24~\x1b[24~"),
            vec![forward(b"\x1b[24~")]
        );

        assert_eq!(route(&mut router, b"\x1b[2"), vec![]);
        assert_eq!(router.escape_deadline(), None);
        assert_eq!(route(&mut router, b"4~"), vec![]);
        assert_eq!(route(&mut router, b"d"), vec![Action::Detach]);

        assert_eq!(route(&mut router, b"\x1b[2"), vec![]);
        assert_eq!(
            route(&mut router, b"1~\x1bOP\x1b[A"),
            vec![forward(b"\x1b[21~\x1bOP\x1b[A")]
        );
    }

    #[test]
    fn a_root_meta_binding_acts_without_the_prefix() {
        let mut router = router_with("C-b", &[("M-h", select_left())]);
        assert_eq!(
            route(&mut router, b"a\x1bhb"),
            vec![
                forward(b"a"),
                command(SessionCommand::SelectPane(Direction::Left)),
                forward(b"b"),
            ]
        );
        assert_eq!(
            route(&mut router, b"\x1b\x1bh"),
            vec![
                forward(b"\x1b"),
                command(SessionCommand::SelectPane(Direction::Left)),
            ]
        );
        assert_eq!(
            route(&mut router, b"\x1bx\x1b[Dh"),
            vec![forward(b"\x1bx\x1b[Dh")]
        );
        assert_eq!(route(&mut router, b"\x02d"), vec![Action::Detach]);
    }

    #[test]
    fn mouse_reports_pass_through_byte_exact_beside_escape_bindings() {
        let mut router = router_with(
            "F12",
            &[
                ("M-h", select_left()),
                ("M-[", Binding::NextPane),
                ("#", Binding::NewWindow),
            ],
        );
        for report in [
            &b"\x1b[<0;3;4M\x1b[<0;3;4m"[..],
            b"\x1b[<64;120;40M",
            b"\x1b[M #!",
            b"\x1b[M#h#",
            b"\x1b[M\"\xff\xff",
        ] {
            assert_eq!(
                route(&mut router, report),
                vec![forward(report)],
                "{report:?}"
            );
            assert_eq!(router.escape_deadline(), None);
        }
    }

    #[test]
    fn a_mouse_report_split_across_chunks_passes_through_in_order() {
        let mut router = router_with("C-b", &[("M-h", select_left())]);
        let mut forwarded = Vec::new();
        for chunk in [&b"\x1b"[..], b"[<0;3", b";4M", b"\x1b", b"[M", b" h!"] {
            for action in route(&mut router, chunk) {
                match action {
                    Action::Forward(bytes) => forwarded.extend(bytes),
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        assert_eq!(forwarded, b"\x1b[<0;3;4M\x1b[M h!");
    }

    #[test]
    fn a_lone_escape_is_held_until_the_timeout_when_a_binding_starts_with_it() {
        let mut router = router_with("C-b", &[("M-h", select_left())]);
        assert_eq!(route(&mut router, b"ab\x1b"), vec![forward(b"ab")]);
        assert!(router.escape_deadline().is_some());
        assert_eq!(router.time_out(), (vec![forward(b"\x1b")], Vec::new()));
        assert_eq!(router.escape_deadline(), None);
        assert_eq!(router.time_out(), (vec![], Vec::new()));

        assert_eq!(route(&mut router, b"\x1b"), vec![]);
        assert_eq!(
            route(&mut router, b"h"),
            vec![command(SessionCommand::SelectPane(Direction::Left))]
        );
        assert_eq!(router.escape_deadline(), None);

        assert_eq!(route(&mut router, b"\x1b\x1b"), vec![forward(b"\x1b")]);
        assert!(router.escape_deadline().is_some());
        assert_eq!(router.time_out(), (vec![forward(b"\x1b")], Vec::new()));
    }

    #[test]
    fn a_bound_escape_fires_on_the_timeout_and_longer_bindings_still_match() {
        let mut router = router_with(
            "C-b",
            &[("Escape", Binding::ClusterTree), ("M-h", select_left())],
        );
        assert_eq!(route(&mut router, b"\x1b"), vec![]);
        assert_eq!(
            router.time_out(),
            (vec![Action::Open(Panel::ClusterTree)], Vec::new())
        );
        assert_eq!(
            route(&mut router, b"\x1bh"),
            vec![command(SessionCommand::SelectPane(Direction::Left))]
        );
        assert_eq!(
            route(&mut router, b"\x1bx\x1b[A"),
            vec![forward(b"\x1bx\x1b[A")]
        );
        let (actions, rest) = router.route(b"\x1b\x1b[A");
        assert_eq!(actions, vec![Action::Open(Panel::ClusterTree)]);
        assert_eq!(rest, b"\x1b[A");
    }

    #[test]
    fn a_bound_escape_alone_still_lets_sequences_through() {
        let mut router = router_with("C-b", &[("Escape", Binding::ClusterTree)]);
        assert_eq!(
            route(&mut router, b"\x1b[A\x1bOB\x1bx"),
            vec![forward(b"\x1b[A\x1bOB\x1bx")]
        );
        assert_eq!(route(&mut router, b"\x1b"), vec![]);
        assert!(router.escape_deadline().is_some());
        assert_eq!(
            router.time_out(),
            (vec![Action::Open(Panel::ClusterTree)], Vec::new())
        );
        let (actions, rest) = router.route(b"\x1b\x02d");
        assert_eq!(actions, vec![Action::Open(Panel::ClusterTree)]);
        assert_eq!(rest, b"\x02d");
    }

    #[test]
    fn closing_stdin_flushes_held_bytes() {
        let mut router = router_with("C-b", &[("M-h", select_left())]);
        assert_eq!(route(&mut router, b"\x1b"), vec![]);
        assert_eq!(router.flush(), b"\x1b");
        assert_eq!(router.escape_deadline(), None);
        assert_eq!(router.flush(), b"");
    }

    #[test]
    fn a_custom_table_handles_one_key_then_returns_to_root() {
        let mut keymap = Keymap::default();
        keymap
            .root
            .insert(key("M-p"), Binding::SwitchTable("pane".into()));
        keymap
            .root
            .insert(key("M-q"), Binding::SwitchTable("bogus".into()));
        keymap.custom.insert(
            "pane".into(),
            [
                (key("h"), select_left()),
                (key("n"), Binding::SwitchTable(PREFIX_TABLE.into())),
            ]
            .into_iter()
            .collect(),
        );
        let mut router = KeyRouter::new(key("C-b"), Arc::new(keymap), ESCAPE_TIME);

        assert_eq!(
            route(&mut router, b"\x1bphh"),
            vec![
                command(SessionCommand::SelectPane(Direction::Left)),
                forward(b"h")
            ]
        );
        assert_eq!(route(&mut router, b"\x1bpzz"), vec![forward(b"z")]);
        assert_eq!(route(&mut router, b"\x1bpnd"), vec![Action::Detach]);
        assert_eq!(route(&mut router, b"\x1bqz"), vec![forward(b"z")]);
    }

    #[test]
    fn send_prefix_forwards_the_prefix_key() {
        let mut keymap = Keymap::default();
        keymap.prefix.insert(key("a"), Binding::SendPrefix);
        let mut router = KeyRouter::new(key("C-a"), Arc::new(keymap), ESCAPE_TIME);
        assert_eq!(
            route(&mut router, b"\x01a\x01\x01"),
            vec![forward(b"\x01\x01")]
        );
    }

    #[test]
    fn a_callback_binding_returns_before_the_rest_of_the_input() {
        let mut keymap = Keymap::default();
        keymap
            .prefix
            .insert(key("g"), Binding::Callback(CallbackId(2)));
        let mut router = KeyRouter::new(key("C-b"), Arc::new(keymap), ESCAPE_TIME);
        let (actions, rest) = router.route(b"a\x02gb\x02d");
        assert_eq!(
            actions,
            vec![forward(b"a"), Action::Callback(CallbackId(2))]
        );
        assert_eq!(rest, b"b\x02d");
    }

    #[test]
    fn running_a_binding_acts_like_pressing_it() {
        let mut router = router();
        assert_eq!(
            router.run_binding(Binding::NewWindow),
            vec![command(SessionCommand::NewWindow)]
        );
        assert_eq!(
            router.run_binding(Binding::RenameWindow),
            vec![Action::Open(Panel::RenameWindow)]
        );
        assert_eq!(
            router.run_binding(Binding::SendPrefix),
            vec![forward(b"\x02")]
        );
        assert_eq!(
            router.run_binding(Binding::SwitchTable(PREFIX_TABLE.into())),
            Vec::new()
        );
        assert_eq!(route(&mut router, b"d"), vec![Action::Detach]);
    }

    #[test]
    fn a_new_keymap_applies_to_the_next_key_and_hands_back_held_bytes() {
        let mut router = router_with("C-b", &[("M-h", select_left())]);
        assert_eq!(route(&mut router, b"\x1b"), Vec::new());

        let mut keymap = Keymap::default();
        keymap.root.insert(key("M-l"), Binding::NextPane);
        keymap.prefix.remove(&key("d"));
        assert_eq!(router.set_keymap(Arc::new(keymap)), b"\x1b");
        assert_eq!(router.escape_deadline(), None);

        assert_eq!(route(&mut router, b"\x1bh"), vec![forward(b"\x1bh")]);
        assert_eq!(
            route(&mut router, b"\x1bl\x02d"),
            vec![command(SessionCommand::NextPane)]
        );
    }
}
