use crate::protocol::{Direction, SessionCommand, Split};

pub const DEFAULT_PREFIX: u8 = 0x02;
const DETACH_KEY: u8 = b'd';
const ESC: u8 = 0x1b;
const SEQUENCE_INTRODUCERS: [u8; 2] = [b'[', b'O'];
const SEQUENCE_PARAMETERS: std::ops::RangeInclusive<u8> = 0x20..=0x3f;
const SEQUENCE_FINALS: std::ops::RangeInclusive<u8> = 0x40..=0x7e;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Forward(Vec<u8>),
    Detach,
    Command(SessionCommand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Typing,
    Prefix,
    PrefixEscape,
    PrefixSequence { parameters: bool },
}

enum Step {
    Forward(u8),
    Act(Action),
    Swallow,
}

pub struct PrefixRouter {
    prefix: u8,
    state: State,
}

impl PrefixRouter {
    pub fn new(prefix: u8) -> Self {
        Self {
            prefix,
            state: State::Typing,
        }
    }

    pub fn route(&mut self, input: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut forward = Vec::new();

        for &byte in input {
            match self.step(byte) {
                Step::Forward(byte) => forward.push(byte),
                Step::Act(action) => {
                    if !forward.is_empty() {
                        actions.push(Action::Forward(std::mem::take(&mut forward)));
                    }
                    actions.push(action);
                }
                Step::Swallow => {}
            }
        }

        if !forward.is_empty() {
            actions.push(Action::Forward(forward));
        }
        actions
    }

    fn step(&mut self, byte: u8) -> Step {
        match self.state {
            State::Typing if byte == self.prefix => {
                self.state = State::Prefix;
                Step::Swallow
            }
            State::Typing => Step::Forward(byte),
            State::Prefix => {
                self.state = State::Typing;
                match byte {
                    _ if byte == self.prefix => Step::Forward(byte),
                    ESC => {
                        self.state = State::PrefixEscape;
                        Step::Swallow
                    }
                    DETACH_KEY => Step::Act(Action::Detach),
                    _ => command_for(byte)
                        .map_or(Step::Swallow, |command| Step::Act(Action::Command(command))),
                }
            }
            State::PrefixEscape if SEQUENCE_INTRODUCERS.contains(&byte) => {
                self.state = State::PrefixSequence { parameters: false };
                Step::Swallow
            }
            State::PrefixSequence { .. } if SEQUENCE_PARAMETERS.contains(&byte) => {
                self.state = State::PrefixSequence { parameters: true };
                Step::Swallow
            }
            State::PrefixSequence { parameters } if SEQUENCE_FINALS.contains(&byte) => {
                self.state = State::Typing;
                match arrow(byte) {
                    Some(direction) if !parameters => {
                        Step::Act(Action::Command(SessionCommand::SelectPane(direction)))
                    }
                    _ => Step::Swallow,
                }
            }
            State::PrefixEscape | State::PrefixSequence { .. } => {
                self.state = State::Typing;
                self.step(byte)
            }
        }
    }
}

fn command_for(key: u8) -> Option<SessionCommand> {
    Some(match key {
        b'c' => SessionCommand::NewWindow,
        b'n' => SessionCommand::NextWindow,
        b'p' => SessionCommand::PreviousWindow,
        b'0'..=b'9' => SessionCommand::SelectWindow(usize::from(key - b'0')),
        b'%' => SessionCommand::SplitPane(Split::LeftRight),
        b'"' => SessionCommand::SplitPane(Split::TopBottom),
        b'o' => SessionCommand::NextPane,
        b'x' => SessionCommand::KillPane,
        b'&' => SessionCommand::KillWindow,
        _ => return None,
    })
}

fn arrow(key: u8) -> Option<Direction> {
    match key {
        b'A' => Some(Direction::Up),
        b'B' => Some(Direction::Down),
        b'C' => Some(Direction::Right),
        b'D' => Some(Direction::Left),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> PrefixRouter {
        PrefixRouter::new(DEFAULT_PREFIX)
    }

    fn command(command: SessionCommand) -> Action {
        Action::Command(command)
    }

    #[test]
    fn plain_input_is_forwarded_untouched() {
        assert_eq!(
            router().route(b"ls -la\r"),
            vec![Action::Forward(b"ls -la\r".to_vec())]
        );
    }

    #[test]
    fn prefix_then_detach_key_detaches_after_flushing_earlier_input() {
        assert_eq!(
            router().route(b"ab\x02d"),
            vec![Action::Forward(b"ab".to_vec()), Action::Detach]
        );
    }

    #[test]
    fn double_prefix_sends_a_literal_prefix() {
        assert_eq!(
            router().route(b"\x02\x02"),
            vec![Action::Forward(vec![DEFAULT_PREFIX])]
        );
    }

    #[test]
    fn prefix_state_carries_across_chunks() {
        let mut router = router();
        assert_eq!(router.route(b"\x02"), vec![]);
        assert_eq!(router.route(b"d"), vec![Action::Detach]);
    }

    #[test]
    fn unbound_keys_after_prefix_are_swallowed() {
        assert_eq!(
            router().route(b"\x02zx"),
            vec![Action::Forward(b"x".to_vec())]
        );
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
                router().route(&[b'a', DEFAULT_PREFIX, key, b'b']),
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
                router().route(keys),
                vec![command(SessionCommand::SelectPane(direction))]
            );
        }
    }

    #[test]
    fn an_arrow_split_across_chunks_still_selects_a_pane() {
        let mut router = router();
        assert_eq!(router.route(b"\x02\x1b"), vec![]);
        assert_eq!(router.route(b"["), vec![]);
        assert_eq!(
            router.route(b"Cls"),
            vec![
                command(SessionCommand::SelectPane(Direction::Right)),
                Action::Forward(b"ls".to_vec()),
            ]
        );
    }

    #[test]
    fn other_sequences_after_prefix_are_swallowed() {
        assert_eq!(
            router().route(b"\x02\x1b[1;5Aa\x02\x1b[<0;3;4Mb"),
            vec![Action::Forward(b"ab".to_vec())]
        );
    }

    #[test]
    fn a_lone_escape_after_prefix_leaves_the_next_key_alone() {
        assert_eq!(
            router().route(b"\x02\x1bx"),
            vec![Action::Forward(b"x".to_vec())]
        );
        assert_eq!(router().route(b"\x02\x1b\x02d"), vec![Action::Detach]);
    }
}
