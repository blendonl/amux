use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::ServerId;
use crate::protocol::{ForgottenPeer, PublicKey, TrustedPeer};

pub const TRUST_FILE: &str = "trust.toml";
const TRUST_FILE_MODE: u32 = 0o600;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trusted: Vec<TrustedPeer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forgotten: Vec<ForgottenPeer>,
}

impl TrustStore {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string(self).context("encoding the trust store")?;
        let temporary = path.with_extension("toml.tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(TRUST_FILE_MODE)
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        file.set_permissions(Permissions::from_mode(TRUST_FILE_MODE))
            .with_context(|| format!("restricting {}", temporary.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
    }

    pub fn is_forgotten(&self, id: ServerId, key: Option<&PublicKey>) -> bool {
        self.forgotten.iter().any(|forgotten| {
            forgotten.id == id || key.is_some_and(|key| forgotten.key == Some(*key))
        })
    }

    pub fn is_key_forgotten(&self, key: &PublicKey) -> bool {
        self.forgotten
            .iter()
            .any(|forgotten| forgotten.key == Some(*key))
    }

    pub fn key_of(&self, id: ServerId) -> Option<PublicKey> {
        self.trusted
            .iter()
            .find(|trusted| trusted.id == id)
            .map(|trusted| trusted.key)
    }

    pub fn forget(&mut self, id: ServerId, key: Option<PublicKey>) {
        self.trusted
            .retain(|trusted| trusted.id != id && Some(trusted.key) != key);
        self.forgotten.retain(|forgotten| forgotten.id != id);
        self.forgotten.push(ForgottenPeer { id, key });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted(id: ServerId, key: u8) -> TrustedPeer {
        TrustedPeer {
            id,
            name: "desk".into(),
            key: PublicKey([key; 32]),
            introduced_by: None,
            direct: true,
        }
    }

    #[test]
    fn a_missing_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            TrustStore::load(&dir.path().join(TRUST_FILE)).unwrap(),
            TrustStore::default()
        );
    }

    #[test]
    fn the_store_round_trips_through_a_private_toml_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TRUST_FILE);
        let desk = ServerId::random().unwrap();
        let laptop = ServerId::random().unwrap();
        let store = TrustStore {
            trusted: vec![TrustedPeer {
                introduced_by: Some(laptop),
                direct: false,
                ..trusted(desk, 1)
            }],
            forgotten: vec![
                ForgottenPeer {
                    id: laptop,
                    key: Some(PublicKey([2; 32])),
                },
                ForgottenPeer {
                    id: ServerId::random().unwrap(),
                    key: None,
                },
            ],
        };

        store.save(&path).unwrap();

        assert_eq!(TrustStore::load(&path).unwrap(), store);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("id = \"{desk}\"")), "{text}");
        assert!(
            text.contains(&format!("key = \"{}\"", PublicKey([1; 32]).to_hex())),
            "{text}"
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, TRUST_FILE_MODE);
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn forgetting_drops_trust_and_matches_the_id_or_the_key() {
        let desk = ServerId::random().unwrap();
        let other = ServerId::random().unwrap();
        let mut store = TrustStore {
            trusted: vec![trusted(desk, 1), trusted(other, 3)],
            forgotten: Vec::new(),
        };
        assert_eq!(store.key_of(desk), Some(PublicKey([1; 32])));

        store.forget(desk, Some(PublicKey([1; 32])));
        store.forget(desk, Some(PublicKey([1; 32])));

        assert_eq!(store.key_of(desk), None);
        assert_eq!(store.key_of(other), Some(PublicKey([3; 32])));
        assert_eq!(store.forgotten.len(), 1);
        assert!(store.is_forgotten(desk, None));
        assert!(store.is_forgotten(other, Some(&PublicKey([1; 32]))));
        assert!(!store.is_forgotten(other, Some(&PublicKey([3; 32]))));
        assert!(store.is_key_forgotten(&PublicKey([1; 32])));
    }
}
