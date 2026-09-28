use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{anyhow, bail, Context, Result};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const SERVER_ID_FILE: &str = "server-id";
const SERVER_ID_FILE_MODE: u32 = 0o600;
const SERVER_ID_HEX_DIGITS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerIdentity {
    pub id: ServerId,
    pub name: String,
    pub incarnation: Incarnation,
}

impl ServerIdentity {
    pub fn load(state_dir: &Path, name: String) -> Result<Self> {
        Ok(Self {
            id: ServerId::load_or_create(&state_dir.join(SERVER_ID_FILE))?,
            name,
            incarnation: Incarnation::random()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServerId(u128);

impl ServerId {
    pub fn random() -> Result<Self> {
        Ok(Self(u128::from_ne_bytes(random_bytes()?)))
    }

    fn load_or_create(path: &Path) -> Result<Self> {
        if let Some(id) = Self::read(path)? {
            return Ok(id);
        }
        let id = Self::random()?;
        let created = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(SERVER_ID_FILE_MODE)
            .open(path);
        match created {
            Ok(mut file) => {
                writeln!(file, "{id}").with_context(|| format!("writing {}", path.display()))?;
                Ok(id)
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Self::read(path)?
                .with_context(|| format!("{} disappeared while reading it", path.display())),
            Err(err) => Err(err).with_context(|| format!("creating {}", path.display())),
        }
    }

    fn read(path: &Path) -> Result<Option<Self>> {
        let Some(text) = read_optional(path)? else {
            return Ok(None);
        };
        let id = text
            .trim()
            .parse()
            .with_context(|| format!("reading the server id from {}", path.display()))?;
        Ok(Some(id))
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

impl FromStr for ServerId {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text.len() != SERVER_ID_HEX_DIGITS || !text.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("{text:?} is not a {SERVER_ID_HEX_DIGITS} digit hex server id");
        }
        Ok(Self(u128::from_str_radix(text, 16)?))
    }
}

impl Serialize for ServerId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            serializer.collect_str(self)
        } else {
            self.0.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for ServerId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let text = String::deserialize(deserializer)?;
            text.parse().map_err(D::Error::custom)
        } else {
            u128::deserialize(deserializer).map(Self)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Incarnation(u64);

impl Incarnation {
    pub fn random() -> Result<Self> {
        Ok(Self(u64::from_ne_bytes(random_bytes()?)))
    }
}

impl fmt::Display for Incarnation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

pub fn hostname() -> Result<String> {
    let name = nix::unistd::gethostname().context("reading the hostname")?;
    Ok(name.to_string_lossy().into_owned())
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|err| anyhow!("reading random bytes: {err}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn the_server_id_persists_and_the_incarnation_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let first = ServerIdentity::load(dir.path(), "a".into()).unwrap();
        let second = ServerIdentity::load(dir.path(), "a".into()).unwrap();

        assert_eq!(first.id, second.id);
        assert_ne!(first.incarnation, second.incarnation);

        let path = dir.path().join(SERVER_ID_FILE);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{}\n", first.id)
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, SERVER_ID_FILE_MODE);
    }

    #[test]
    fn a_corrupt_server_id_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SERVER_ID_FILE), "not-an-id\n").unwrap();
        assert!(ServerIdentity::load(dir.path(), "a".into()).is_err());
    }

    #[test]
    fn a_missing_file_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_optional(&dir.path().join(SERVER_ID_FILE)).unwrap(),
            None
        );
    }

    #[test]
    fn the_hostname_is_known() {
        assert!(!hostname().unwrap().is_empty());
    }

    #[test]
    fn server_ids_are_a_number_on_the_wire_and_hex_in_text() {
        let id: ServerId = "0123456789abcdef0123456789abcdef".parse().unwrap();
        let wire = postcard::to_stdvec(&id).unwrap();
        assert_eq!(wire, postcard::to_stdvec(&id.0).unwrap());
        assert_eq!(postcard::from_bytes::<ServerId>(&wire).unwrap(), id);

        let text = serde_json::to_string(&id).unwrap();
        assert_eq!(text, "\"0123456789abcdef0123456789abcdef\"");
        assert_eq!(serde_json::from_str::<ServerId>(&text).unwrap(), id);
        assert!(serde_json::from_str::<ServerId>("\"nope\"").is_err());
    }

    #[test]
    fn server_ids_round_trip_through_hex() {
        let id = ServerId::random().unwrap();
        let text = id.to_string();
        assert_eq!(text.len(), SERVER_ID_HEX_DIGITS);
        assert_eq!(text.parse::<ServerId>().unwrap(), id);
        assert!("12345".parse::<ServerId>().is_err());
        assert!("z"
            .repeat(SERVER_ID_HEX_DIGITS)
            .parse::<ServerId>()
            .is_err());
    }
}
