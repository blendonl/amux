use super::command::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    Einval,
    Enoent,
    Ebadf,
    Enodata,
    Efbig,
    Ebadpng,
    Enoparent,
    Ecycle,
    Etoodeep,
}

impl Code {
    fn name(self) -> &'static str {
        match self {
            Self::Einval => "EINVAL",
            Self::Enoent => "ENOENT",
            Self::Ebadf => "EBADF",
            Self::Enodata => "ENODATA",
            Self::Efbig => "EFBIG",
            Self::Ebadpng => "EBADPNG",
            Self::Enoparent => "ENOPARENT",
            Self::Ecycle => "ECYCLE",
            Self::Etoodeep => "ETOODEEP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: Code,
    pub message: String,
}

impl Failure {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Recipient {
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub quiet: u32,
}

impl Recipient {
    pub fn of(command: &Command) -> Self {
        Self {
            id: command.id,
            number: command.number,
            placement: command.placement,
            quiet: command.quiet,
        }
    }

    pub fn reply(&self, outcome: &Result<(), Failure>) -> Option<Vec<u8>> {
        if self.id == 0 && self.number == 0 {
            return None;
        }
        let message = match outcome {
            Ok(()) if self.quiet == 0 => "OK".to_owned(),
            Err(failure) if self.quiet < 2 => {
                format!("{}:{}", failure.code.name(), failure.message)
            }
            _ => return None,
        };
        let keys: Vec<String> = [("i", self.id), ("I", self.number), ("p", self.placement)]
            .into_iter()
            .filter(|(_, value)| *value != 0)
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        Some(format!("\x1b_G{};{message}\x1b\\", keys.join(",")).into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to(id: u32, number: u32, placement: u32, quiet: u32) -> Recipient {
        Recipient {
            id,
            number,
            placement,
            quiet,
        }
    }

    fn reply(recipient: Recipient, outcome: &Result<(), Failure>) -> Option<String> {
        recipient
            .reply(outcome)
            .map(|bytes| String::from_utf8(bytes).unwrap())
    }

    fn missing() -> Result<(), Failure> {
        Err(Failure::new(Code::Enoent, "no such image"))
    }

    #[test]
    fn an_ok_reply_names_the_image() {
        assert_eq!(
            reply(to(31, 0, 0, 0), &Ok(())).as_deref(),
            Some("\x1b_Gi=31;OK\x1b\\")
        );
        assert_eq!(
            reply(to(7, 3, 2, 0), &Ok(())).as_deref(),
            Some("\x1b_Gi=7,I=3,p=2;OK\x1b\\")
        );
        assert_eq!(
            reply(to(0, 9, 0, 0), &Ok(())).as_deref(),
            Some("\x1b_GI=9;OK\x1b\\")
        );
    }

    #[test]
    fn an_error_reply_carries_the_code_and_message() {
        assert_eq!(
            reply(to(4, 0, 0, 0), &missing()).as_deref(),
            Some("\x1b_Gi=4;ENOENT:no such image\x1b\\")
        );
        for (code, name) in [
            (Code::Einval, "EINVAL"),
            (Code::Enoent, "ENOENT"),
            (Code::Ebadf, "EBADF"),
            (Code::Enodata, "ENODATA"),
            (Code::Efbig, "EFBIG"),
            (Code::Ebadpng, "EBADPNG"),
            (Code::Enoparent, "ENOPARENT"),
            (Code::Ecycle, "ECYCLE"),
            (Code::Etoodeep, "ETOODEEP"),
        ] {
            let failed = Err(Failure::new(code, "why"));
            assert_eq!(
                reply(to(1, 0, 0, 0), &failed),
                Some(format!("\x1b_Gi=1;{name}:why\x1b\\"))
            );
        }
    }

    #[test]
    fn without_an_id_or_number_nothing_is_replied() {
        assert_eq!(reply(to(0, 0, 0, 0), &Ok(())), None);
        assert_eq!(reply(to(0, 0, 5, 0), &missing()), None);
    }

    #[test]
    fn quiet_one_hides_ok_and_quiet_two_hides_everything() {
        assert_eq!(reply(to(1, 0, 0, 1), &Ok(())), None);
        assert!(reply(to(1, 0, 0, 1), &missing()).is_some());
        assert_eq!(reply(to(1, 0, 0, 2), &Ok(())), None);
        assert_eq!(reply(to(1, 0, 0, 2), &missing()), None);
    }

    #[test]
    fn the_recipient_comes_from_the_command() {
        let command = Command::parse(b"i=3,I=4,p=5,q=1").unwrap();
        assert_eq!(Recipient::of(&command), to(3, 4, 5, 1));
    }
}
