use memchr::memchr;

use crate::protocol::{AnimationControl, AnimationState, FrameSpec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Action {
    #[default]
    Transmit,
    TransmitAndPlace,
    Query,
    Place,
    Delete,
    Frame,
    Animate,
    Compose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingValue(Vec<u8>),
    BadKey(Vec<u8>),
    BadValue(char),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Composition {
    pub source: u32,
    pub dest: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub replace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub action: Action,
    pub medium: u8,
    pub format: u32,
    pub compression: Option<u8>,
    pub width: u32,
    pub height: u32,
    pub data_size: u32,
    pub data_offset: u32,
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub more: bool,
    pub quiet: u32,
    pub unicode_placeholder: bool,
    pub columns: u32,
    pub rows: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
    pub cursor_movement: u32,
    pub z_index: i32,
    pub parent_id: u32,
    pub parent_placement: u32,
    pub horizontal_offset: i32,
    pub vertical_offset: i32,
    pub delete: u8,
    pub payload: Vec<u8>,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: Action::Transmit,
            medium: b'd',
            format: 32,
            compression: None,
            width: 0,
            height: 0,
            data_size: 0,
            data_offset: 0,
            id: 0,
            number: 0,
            placement: 0,
            more: false,
            quiet: 0,
            unicode_placeholder: false,
            columns: 0,
            rows: 0,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
            cell_x_offset: 0,
            cell_y_offset: 0,
            cursor_movement: 0,
            z_index: 0,
            parent_id: 0,
            parent_placement: 0,
            horizontal_offset: 0,
            vertical_offset: 0,
            delete: b'a',
            payload: Vec::new(),
        }
    }
}

impl Command {
    pub fn parse(body: &[u8]) -> Result<Self, ParseError> {
        let (control, payload) = match memchr(b';', body) {
            Some(semicolon) => (&body[..semicolon], &body[semicolon + 1..]),
            None => (body, &[][..]),
        };
        let mut command = Self::default();
        for field in control.split(|&byte| byte == b',') {
            if field.is_empty() {
                continue;
            }
            let Some(equals) = memchr(b'=', field) else {
                return Err(ParseError::MissingValue(field.to_vec()));
            };
            let (key, value) = (&field[..equals], &field[equals + 1..]);
            let &[key] = key else {
                return Err(ParseError::BadKey(key.to_vec()));
            };
            command.set(key, value)?;
        }
        command.payload = payload.to_vec();
        Ok(command)
    }

    pub fn frame(&self) -> FrameSpec {
        FrameSpec {
            edit: self.rows,
            base: self.columns,
            x: self.source_x,
            y: self.source_y,
            background: self.cell_y_offset,
            replace: self.cell_x_offset == 1,
            gap: self.z_index,
        }
    }

    pub fn control(&self) -> AnimationControl {
        AnimationControl {
            frame: self.rows,
            gap: self.z_index,
            current: self.columns,
            state: match self.width {
                1 => Some(AnimationState::Stopped),
                2 => Some(AnimationState::Loading),
                3 => Some(AnimationState::Running),
                _ => None,
            },
            loops: self.height,
        }
    }

    pub fn composition(&self) -> Composition {
        Composition {
            source: self.rows,
            dest: self.columns,
            x: self.source_x,
            y: self.source_y,
            width: self.source_width,
            height: self.source_height,
            source_x: self.cell_x_offset,
            source_y: self.cell_y_offset,
            replace: self.cursor_movement != 0,
        }
    }

    fn set(&mut self, key: u8, value: &[u8]) -> Result<(), ParseError> {
        let unsigned = || unsigned(key, value);
        let signed = || signed(key, value);
        match key {
            b'a' => self.action = action(value)?,
            b't' => self.medium = flag(key, value)?,
            b'o' => self.compression = Some(flag(key, value)?),
            b'd' => self.delete = flag(key, value)?,
            b'f' => self.format = unsigned()?,
            b's' => self.width = unsigned()?,
            b'v' => self.height = unsigned()?,
            b'S' => self.data_size = unsigned()?,
            b'O' => self.data_offset = unsigned()?,
            b'i' => self.id = unsigned()?,
            b'I' => self.number = unsigned()?,
            b'p' => self.placement = unsigned()?,
            b'm' => self.more = unsigned()? != 0,
            b'q' => self.quiet = unsigned()?,
            b'U' => self.unicode_placeholder = unsigned()? != 0,
            b'c' => self.columns = unsigned()?,
            b'r' => self.rows = unsigned()?,
            b'x' => self.source_x = unsigned()?,
            b'y' => self.source_y = unsigned()?,
            b'w' => self.source_width = unsigned()?,
            b'h' => self.source_height = unsigned()?,
            b'X' => self.cell_x_offset = unsigned()?,
            b'Y' => self.cell_y_offset = unsigned()?,
            b'C' => self.cursor_movement = unsigned()?,
            b'z' => self.z_index = signed()?,
            b'P' => self.parent_id = unsigned()?,
            b'Q' => self.parent_placement = unsigned()?,
            b'H' => self.horizontal_offset = signed()?,
            b'V' => self.vertical_offset = signed()?,
            _ => {}
        }
        Ok(())
    }
}

fn action(value: &[u8]) -> Result<Action, ParseError> {
    Ok(match flag(b'a', value)? {
        b't' => Action::Transmit,
        b'T' => Action::TransmitAndPlace,
        b'q' => Action::Query,
        b'p' => Action::Place,
        b'd' => Action::Delete,
        b'f' => Action::Frame,
        b'a' => Action::Animate,
        b'c' => Action::Compose,
        _ => return Err(ParseError::BadValue('a')),
    })
}

fn flag(key: u8, value: &[u8]) -> Result<u8, ParseError> {
    match value {
        &[flag] if flag.is_ascii_alphabetic() => Ok(flag),
        _ => Err(ParseError::BadValue(char::from(key))),
    }
}

fn unsigned(key: u8, value: &[u8]) -> Result<u32, ParseError> {
    digits(value)
        .and_then(|digits| digits.parse().ok())
        .ok_or(ParseError::BadValue(char::from(key)))
}

fn signed(key: u8, value: &[u8]) -> Result<i32, ParseError> {
    let (negative, magnitude) = match value.strip_prefix(b"-") {
        Some(magnitude) => (true, magnitude),
        None => (false, value),
    };
    digits(magnitude)
        .and_then(|digits| {
            let magnitude: i64 = digits.parse().ok()?;
            i32::try_from(if negative { -magnitude } else { magnitude }).ok()
        })
        .ok_or(ParseError::BadValue(char::from(key)))
}

fn digits(value: &[u8]) -> Option<&str> {
    let all_digits = !value.is_empty() && value.iter().all(u8::is_ascii_digit);
    all_digits
        .then(|| std::str::from_utf8(value).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Command {
        Command::parse(body.as_bytes()).unwrap()
    }

    #[test]
    fn an_empty_command_takes_kittys_defaults() {
        let command = parse("");
        assert_eq!(command, Command::default());
        assert_eq!(command.action, Action::Transmit);
        assert_eq!(command.medium, b'd');
        assert_eq!(command.format, 32);
        assert_eq!(command.delete, b'a');
        assert!(command.payload.is_empty());
    }

    #[test]
    fn every_key_is_read() {
        let command = parse(
            "a=T,t=f,f=100,o=z,s=10,v=20,S=30,O=40,i=1,I=2,p=3,m=1,q=2,U=1,c=4,r=5,\
             x=6,y=7,w=8,h=9,X=11,Y=12,C=1,z=-13,P=14,Q=15,H=-16,V=17,d=I;cGF5bG9hZA==",
        );
        assert_eq!(
            command,
            Command {
                action: Action::TransmitAndPlace,
                medium: b'f',
                format: 100,
                compression: Some(b'z'),
                width: 10,
                height: 20,
                data_size: 30,
                data_offset: 40,
                id: 1,
                number: 2,
                placement: 3,
                more: true,
                quiet: 2,
                unicode_placeholder: true,
                columns: 4,
                rows: 5,
                source_x: 6,
                source_y: 7,
                source_width: 8,
                source_height: 9,
                cell_x_offset: 11,
                cell_y_offset: 12,
                cursor_movement: 1,
                z_index: -13,
                parent_id: 14,
                parent_placement: 15,
                horizontal_offset: -16,
                vertical_offset: 17,
                delete: b'I',
                payload: b"cGF5bG9hZA==".to_vec(),
            }
        );
    }

    #[test]
    fn every_action_is_known() {
        for (letter, action) in [
            ("t", Action::Transmit),
            ("T", Action::TransmitAndPlace),
            ("q", Action::Query),
            ("p", Action::Place),
            ("d", Action::Delete),
            ("f", Action::Frame),
            ("a", Action::Animate),
            ("c", Action::Compose),
        ] {
            assert_eq!(parse(&format!("a={letter}")).action, action);
        }
    }

    #[test]
    fn every_frame_key_is_read() {
        let command = parse("a=f,i=1,r=3,c=2,x=4,y=5,X=1,Y=4278190335,z=-1,s=6,v=7,f=24;AAAA");
        assert_eq!(
            command.frame(),
            FrameSpec {
                edit: 3,
                base: 2,
                x: 4,
                y: 5,
                background: 0xff00_00ff,
                replace: true,
                gap: -1,
            }
        );
        assert_eq!((command.width, command.height, command.format), (6, 7, 24));
        assert_eq!(parse("a=f").frame(), FrameSpec::default());
        assert!(!parse("a=f,X=2").frame().replace);
        assert_eq!(parse("a=f,z=80").frame().gap, 80);
    }

    #[test]
    fn every_animation_control_key_is_read() {
        assert_eq!(
            parse("a=a,i=1,s=3,v=5,r=2,z=80,c=4").control(),
            AnimationControl {
                frame: 2,
                gap: 80,
                current: 4,
                state: Some(AnimationState::Running),
                loops: 5,
            }
        );
        assert_eq!(parse("a=a").control(), AnimationControl::default());
        for (value, state) in [
            ("0", None),
            ("1", Some(AnimationState::Stopped)),
            ("2", Some(AnimationState::Loading)),
            ("3", Some(AnimationState::Running)),
            ("4", None),
        ] {
            assert_eq!(parse(&format!("a=a,s={value}")).control().state, state);
        }
        assert_eq!(parse("a=a,r=1,z=-5").control().gap, -5);
    }

    #[test]
    fn every_composition_key_is_read() {
        assert_eq!(
            parse("a=c,i=1,r=7,c=9,w=23,h=27,X=4,Y=8,x=1,y=3,C=1").composition(),
            Composition {
                source: 7,
                dest: 9,
                x: 1,
                y: 3,
                width: 23,
                height: 27,
                source_x: 4,
                source_y: 8,
                replace: true,
            }
        );
        assert_eq!(parse("a=c").composition(), Composition::default());
        assert!(parse("a=c,C=2").composition().replace);
    }

    #[test]
    fn the_payload_is_everything_after_the_first_semicolon() {
        assert_eq!(parse("i=1;abc;def=,").payload, b"abc;def=,");
        assert_eq!(parse(";AAAA").payload, b"AAAA");
        assert_eq!(parse("m=0;").payload, b"");
        assert_eq!(parse("m=1").payload, b"");
    }

    #[test]
    fn unknown_keys_and_empty_fields_are_ignored() {
        let command = parse("i=5,,e=whatever,k=1,i=6,;x");
        assert_eq!(command.id, 6);
        assert_eq!(command.payload, b"x");
    }

    #[test]
    fn the_extremes_of_each_number_fit() {
        let command = parse("i=4294967295,z=-2147483648,H=2147483647,m=0,U=0");
        assert_eq!(command.id, u32::MAX);
        assert_eq!(command.z_index, i32::MIN);
        assert_eq!(command.horizontal_offset, i32::MAX);
        assert!(!command.more && !command.unicode_placeholder);
    }

    #[test]
    fn malformed_values_are_errors() {
        let bad = |body: &str| Command::parse(body.as_bytes()).unwrap_err();
        assert_eq!(bad("i=4294967296"), ParseError::BadValue('i'));
        assert_eq!(bad("i=-1"), ParseError::BadValue('i'));
        assert_eq!(bad("i=+1"), ParseError::BadValue('i'));
        assert_eq!(bad("s=1x"), ParseError::BadValue('s'));
        assert_eq!(bad("v="), ParseError::BadValue('v'));
        assert_eq!(bad("z=-"), ParseError::BadValue('z'));
        assert_eq!(bad("z=2147483648"), ParseError::BadValue('z'));
        assert_eq!(bad("V=--1"), ParseError::BadValue('V'));
        assert_eq!(bad("a=x"), ParseError::BadValue('a'));
        assert_eq!(bad("a=tt"), ParseError::BadValue('a'));
        assert_eq!(bad("t="), ParseError::BadValue('t'));
        assert_eq!(bad("t=1"), ParseError::BadValue('t'));
        assert_eq!(bad("o=zz"), ParseError::BadValue('o'));
        assert_eq!(bad("d=ab"), ParseError::BadValue('d'));
        assert_eq!(bad("i=1,q"), ParseError::MissingValue(b"q".to_vec()));
        assert_eq!(bad("ia=1"), ParseError::BadKey(b"ia".to_vec()));
        assert_eq!(bad("=1"), ParseError::BadKey(Vec::new()));
    }
}
