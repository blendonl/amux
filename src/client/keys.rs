pub const DEFAULT_PREFIX: u8 = 0x02;
const DETACH_KEY: u8 = b'd';

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Forward(Vec<u8>),
    Detach,
}

pub struct PrefixRouter {
    prefix: u8,
    awaiting_command: bool,
}

impl PrefixRouter {
    pub fn new(prefix: u8) -> Self {
        Self {
            prefix,
            awaiting_command: false,
        }
    }

    pub fn route(&mut self, input: &[u8]) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut forward = Vec::new();

        for &byte in input {
            if !self.awaiting_command {
                if byte == self.prefix {
                    self.awaiting_command = true;
                } else {
                    forward.push(byte);
                }
                continue;
            }

            self.awaiting_command = false;
            if byte == self.prefix {
                forward.push(byte);
            } else if byte == DETACH_KEY {
                if !forward.is_empty() {
                    actions.push(Action::Forward(std::mem::take(&mut forward)));
                }
                actions.push(Action::Detach);
            }
        }

        if !forward.is_empty() {
            actions.push(Action::Forward(forward));
        }
        actions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> PrefixRouter {
        PrefixRouter::new(DEFAULT_PREFIX)
    }

    #[test]
    fn plain_input_is_forwarded_untouched() {
        assert_eq!(router().route(b"ls -la\r"), vec![Action::Forward(b"ls -la\r".to_vec())]);
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
        assert_eq!(router().route(b"\x02\x02"), vec![Action::Forward(vec![DEFAULT_PREFIX])]);
    }

    #[test]
    fn prefix_state_carries_across_chunks() {
        let mut router = router();
        assert_eq!(router.route(b"\x02"), vec![]);
        assert_eq!(router.route(b"d"), vec![Action::Detach]);
    }

    #[test]
    fn unbound_keys_after_prefix_are_swallowed() {
        assert_eq!(router().route(b"\x02zx"), vec![Action::Forward(b"x".to_vec())]);
    }
}
