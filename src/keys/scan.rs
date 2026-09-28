use super::{ESC, X10_MOUSE_PAYLOAD_LEN};

const PASTE_START: u16 = 200;
const PASTE_END: u16 = 201;
const SEQUENCE_BYTES: std::ops::RangeInclusive<u8> = 0x20..=0x7e;
const PARAMETER_BYTES: std::ops::RangeInclusive<u8> = 0x20..=0x3f;
const FINAL_BYTES: std::ops::RangeInclusive<u8> = 0x40..=0x7e;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi(Parameter),
    Ss3,
    Mouse(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parameter {
    Empty,
    Number(u16),
    Other,
}

impl Parameter {
    fn push(self, byte: u8) -> Self {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            _ => return Self::Other,
        };
        match self {
            Self::Empty => Self::Number(digit),
            Self::Number(number) => number
                .checked_mul(10)
                .and_then(|number| number.checked_add(digit))
                .map_or(Self::Other, Self::Number),
            Self::Other => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Scanner {
    state: State,
    paste: bool,
}

impl Scanner {
    pub fn is_ground(&self) -> bool {
        self.state == State::Ground
    }

    pub fn is_pasting(&self) -> bool {
        self.paste
    }

    pub fn starts_sequence(&self, byte: u8) -> bool {
        is_control(byte)
            || match self.state {
                State::Ground => true,
                State::Escape | State::Mouse(_) => false,
                State::Csi(_) | State::Ss3 => !SEQUENCE_BYTES.contains(&byte),
            }
    }

    pub fn feed(&mut self, byte: u8) {
        self.state = match (self.state, byte) {
            (State::Mouse(left), _) if left > 1 => State::Mouse(left - 1),
            (State::Mouse(_), _) => State::Ground,
            (_, ESC) => State::Escape,
            (State::Escape, b'[') => State::Csi(Parameter::Empty),
            (State::Escape, b'O') => State::Ss3,
            (State::Ground | State::Escape, _) => State::Ground,
            (State::Csi(parameter), _) if PARAMETER_BYTES.contains(&byte) => {
                State::Csi(parameter.push(byte))
            }
            (State::Csi(parameter), _) if FINAL_BYTES.contains(&byte) => {
                self.finish_csi(parameter, byte)
            }
            (State::Ss3, _) if PARAMETER_BYTES.contains(&byte) => State::Ss3,
            (state, _) if is_control(byte) => state,
            (State::Csi(_) | State::Ss3, _) => State::Ground,
        };
    }

    fn finish_csi(&mut self, parameter: Parameter, last: u8) -> State {
        match (parameter, last) {
            (Parameter::Empty, b'M') if !self.paste => State::Mouse(X10_MOUSE_PAYLOAD_LEN),
            (Parameter::Number(PASTE_START), b'~') => {
                self.paste = true;
                State::Ground
            }
            (Parameter::Number(PASTE_END), b'~') => {
                self.paste = false;
                State::Ground
            }
            _ => State::Ground,
        }
    }
}

fn is_control(byte: u8) -> bool {
    byte < 0x20
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(input: &[u8]) -> Vec<bool> {
        let mut scanner = Scanner::default();
        input
            .iter()
            .map(|&byte| {
                let starts = scanner.starts_sequence(byte);
                scanner.feed(byte);
                starts
            })
            .collect()
    }

    #[test]
    fn text_and_whole_sequences_start_where_expected() {
        assert_eq!(
            starts(b"a\x1b[1;5Ab\x1bOAc\x1bh"),
            [
                true, true, false, false, false, false, false, true, true, false, false, true,
                true, false,
            ]
        );
    }

    #[test]
    fn control_bytes_always_start_a_sequence() {
        assert_eq!(
            starts(b"\x1b[1\x02;5A\x1b\x02x"),
            [true, false, false, true, false, false, false, true, true, true]
        );
    }

    #[test]
    fn an_escape_or_a_stray_byte_ends_a_sequence() {
        assert_eq!(
            starts(b"\x1b[1\x1b[A\x1b[\xc3\xa9"),
            [true, false, false, true, false, false, true, false, true, true]
        );
    }

    #[test]
    fn x10_mouse_payloads_are_part_of_the_report() {
        assert_eq!(
            starts(b"\x1b[Ma!!a\x1b[<0;3;4Mb"),
            [
                true, false, false, false, false, false, true, true, false, false, false, false,
                false, false, false, false, true,
            ]
        );
    }

    #[test]
    fn pasting_lasts_from_the_start_marker_through_the_end_marker() {
        let mut scanner = Scanner::default();
        let pasting: Vec<bool> = b"a\x1b[200~b\x1b[201~c"
            .iter()
            .map(|&byte| {
                scanner.feed(byte);
                scanner.is_pasting()
            })
            .collect();
        assert_eq!(
            pasting,
            [
                false, false, false, false, false, false, true, true, true, true, true, true, true,
                false, false,
            ]
        );
    }

    #[test]
    fn a_pasted_mouse_introducer_is_plain_text() {
        assert_eq!(
            starts(b"\x1b[200~\x1b[M!a\x1b[201~\x1b[M!a"),
            [
                true, false, false, false, false, false, true, false, false, true, true, true,
                false, false, false, false, false, true, false, false, false, false,
            ]
        );
    }
}
