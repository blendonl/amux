use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ServerId;
use crate::protocol::{ForgottenPeer, PublicKey, TrustUpdate, TrustedPeer};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Witnessed {
    Unchanged,
    Renamed,
    Confirmed,
    Trusted,
    Replaced(PublicKey),
    HeldBy(ServerId),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Merged {
    pub changed: bool,
    pub forgotten: Vec<ServerId>,
}

impl Witnessed {
    pub fn changed(self) -> bool {
        !matches!(self, Self::Unchanged | Self::HeldBy(_))
    }
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
        let id_forgotten = self.forgotten.iter().any(|forgotten| forgotten.id == id);
        key.is_some_and(|key| self.is_key_forgotten(key)) || (id_forgotten && !self.is_repaired(id))
    }

    fn is_repaired(&self, id: ServerId) -> bool {
        self.trusted
            .iter()
            .any(|trusted| trusted.id == id && !self.is_key_forgotten(&trusted.key))
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

    pub fn is_trusted(&self, key: &PublicKey) -> bool {
        self.trusted.iter().any(|trusted| trusted.key == *key)
    }

    pub fn update(&self) -> TrustUpdate {
        TrustUpdate {
            trusted: self.trusted.clone(),
            forgotten: self.forgotten.clone(),
        }
    }

    pub fn witness(&mut self, id: ServerId, name: &str, key: PublicKey) -> Witnessed {
        if let Some(holder) = self
            .trusted
            .iter()
            .find(|trusted| trusted.key == key && trusted.id != id)
        {
            return Witnessed::HeldBy(holder.id);
        }
        let direct = TrustedPeer {
            id,
            name: name.to_owned(),
            key,
            introduced_by: None,
            direct: true,
        };
        let Some(entry) = self.trusted.iter_mut().find(|trusted| trusted.id == id) else {
            self.trusted.push(direct);
            return Witnessed::Trusted;
        };
        let witnessed = if entry.key != key {
            Witnessed::Replaced(entry.key)
        } else if !entry.direct {
            Witnessed::Confirmed
        } else if entry.name != name {
            Witnessed::Renamed
        } else {
            Witnessed::Unchanged
        };
        *entry = direct;
        witnessed
    }

    pub fn merge(&mut self, update: &TrustUpdate, sender: ServerId, own: ServerId) -> Merged {
        let mut merged = Merged::default();
        for tombstone in &update.forgotten {
            if tombstone.id == own || self.covers(tombstone) || self.outdates(tombstone) {
                continue;
            }
            if !self.is_forgotten(tombstone.id, None) {
                merged.forgotten.push(tombstone.id);
            }
            self.forget(tombstone.id, tombstone.key);
            merged.changed = true;
        }
        for entry in &update.trusted {
            let introducer = match entry.introduced_by {
                Some(introducer) if !entry.direct => introducer,
                _ => sender,
            };
            let known = self.key_of(entry.id).is_some() || self.is_trusted(&entry.key);
            let forgotten = if entry.direct {
                self.is_key_forgotten(&entry.key)
            } else {
                self.is_forgotten(entry.id, Some(&entry.key))
            };
            if entry.id == own
                || introducer == own
                || known
                || forgotten
                || self.is_forgotten(introducer, None)
            {
                continue;
            }
            self.trusted.push(TrustedPeer {
                id: entry.id,
                name: entry.name.clone(),
                key: entry.key,
                introduced_by: Some(introducer),
                direct: false,
            });
            merged.changed = true;
        }
        merged
    }

    pub fn forget(&mut self, id: ServerId, key: Option<PublicKey>) {
        self.trusted.retain(|trusted| {
            let introduced = !trusted.direct && trusted.introduced_by == Some(id);
            trusted.id != id && Some(trusted.key) != key && !introduced
        });
        if key.is_some() {
            self.forgotten
                .retain(|forgotten| forgotten.id != id || forgotten.key.is_some());
        }
        let tombstone = ForgottenPeer { id, key };
        if !self.covers(&tombstone) {
            self.forgotten.push(tombstone);
        }
    }

    pub fn unforget(&mut self, id: ServerId) -> bool {
        let before = self.forgotten.len();
        self.forgotten.retain(|forgotten| forgotten.id != id);
        self.forgotten.len() != before
    }

    fn covers(&self, tombstone: &ForgottenPeer) -> bool {
        self.forgotten.iter().any(|forgotten| {
            forgotten.id == tombstone.id
                && (tombstone.key.is_none() || forgotten.key == tombstone.key)
        })
    }

    fn outdates(&self, tombstone: &ForgottenPeer) -> bool {
        tombstone.key.is_some_and(|key| {
            self.trusted
                .iter()
                .any(|trusted| trusted.id == tombstone.id && trusted.direct && trusted.key != key)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    fn id() -> ServerId {
        ServerId::random().unwrap()
    }

    fn introduced(id: ServerId, byte: u8, by: ServerId) -> TrustedPeer {
        TrustedPeer {
            introduced_by: Some(by),
            direct: false,
            ..trusted(id, byte)
        }
    }

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

    #[test]
    fn forgetting_a_member_drops_the_keys_it_introduced_unless_seen_directly() {
        let (desk, laptop, phone, server) = (id(), id(), id(), id());
        let mut store = TrustStore {
            trusted: vec![
                trusted(desk, 1),
                introduced(laptop, 2, desk),
                TrustedPeer {
                    introduced_by: Some(desk),
                    ..trusted(phone, 3)
                },
                introduced(server, 4, phone),
            ],
            forgotten: Vec::new(),
        };

        store.forget(desk, Some(key(1)));

        let kept: Vec<ServerId> = store.trusted.iter().map(|trusted| trusted.id).collect();
        assert_eq!(kept, [phone, server]);
    }

    #[test]
    fn direct_evidence_adds_confirms_and_replaces_one_key_per_id() {
        let (desk, laptop, introducer) = (id(), id(), id());
        let mut store = TrustStore {
            trusted: vec![introduced(laptop, 2, introducer)],
            forgotten: Vec::new(),
        };

        assert_eq!(store.witness(desk, "desk", key(1)), Witnessed::Trusted);
        assert_eq!(store.witness(desk, "desk", key(1)), Witnessed::Unchanged);
        assert_eq!(store.witness(desk, "desk-2", key(1)), Witnessed::Renamed);
        assert_eq!(store.witness(laptop, "desk", key(2)), Witnessed::Confirmed);
        assert_eq!(
            store.witness(desk, "desk", key(5)),
            Witnessed::Replaced(key(1))
        );
        assert_eq!(
            store.witness(id(), "thief", key(5)),
            Witnessed::HeldBy(desk)
        );

        assert_eq!(
            store.trusted,
            vec![trusted(laptop, 2), trusted(desk, 5)]
                .into_iter()
                .map(|entry| TrustedPeer {
                    name: "desk".into(),
                    ..entry
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn gossip_adds_unknown_keys_under_their_first_hand_witness() {
        let (own, sender, desk, laptop, phone) = (id(), id(), id(), id(), id());
        let witness = id();
        let mut store = TrustStore {
            trusted: vec![trusted(phone, 3)],
            forgotten: Vec::new(),
        };
        let update = TrustUpdate {
            trusted: vec![
                trusted(desk, 1),
                introduced(laptop, 2, witness),
                trusted(phone, 9),
                trusted(own, 7),
                introduced(id(), 8, own),
            ],
            forgotten: Vec::new(),
        };

        let merged = store.merge(&update, sender, own);

        assert!(merged.changed);
        assert!(merged.forgotten.is_empty());
        assert_eq!(
            store.trusted,
            [
                trusted(phone, 3),
                introduced(desk, 1, sender),
                introduced(laptop, 2, witness),
            ]
        );
        assert_eq!(store.merge(&update, sender, own), Merged::default());
    }

    #[test]
    fn gossiped_tombstones_forget_and_cascade_and_block_later_gossip() {
        let (own, sender, desk, laptop) = (id(), id(), id(), id());
        let mut store = TrustStore {
            trusted: vec![trusted(desk, 1), introduced(laptop, 2, desk)],
            forgotten: Vec::new(),
        };
        let update = TrustUpdate {
            trusted: Vec::new(),
            forgotten: vec![
                ForgottenPeer {
                    id: desk,
                    key: Some(key(1)),
                },
                ForgottenPeer { id: own, key: None },
            ],
        };

        let merged = store.merge(&update, sender, own);

        assert_eq!(
            merged,
            Merged {
                changed: true,
                forgotten: vec![desk],
            }
        );
        assert!(store.trusted.is_empty());
        assert!(!store.is_forgotten(own, None));
        let stale = TrustUpdate {
            trusted: vec![trusted(desk, 1), introduced(laptop, 2, desk)],
            forgotten: Vec::new(),
        };
        assert_eq!(store.merge(&stale, sender, own), Merged::default());
        assert!(store.trusted.is_empty());
    }

    #[test]
    fn tombstones_for_one_id_accumulate_instead_of_flapping() {
        let desk = id();
        let tombstone = |byte: Option<u8>| ForgottenPeer {
            id: desk,
            key: byte.map(key),
        };
        let mut store = TrustStore::default();

        store.forget(desk, None);
        store.forget(desk, Some(key(1)));
        store.forget(desk, Some(key(2)));
        store.forget(desk, None);
        store.forget(desk, Some(key(1)));

        assert_eq!(store.forgotten, [tombstone(Some(1)), tombstone(Some(2))]);
    }

    #[test]
    fn a_repaired_server_is_unforgotten_and_its_old_tombstone_ignored() {
        let (own, sender, desk) = (id(), id(), id());
        let mut store = TrustStore::default();
        store.forget(desk, Some(key(1)));

        assert!(store.unforget(desk));
        assert!(!store.unforget(desk));
        assert_eq!(store.witness(desk, "desk", key(2)), Witnessed::Trusted);
        let old = TrustUpdate {
            trusted: Vec::new(),
            forgotten: vec![ForgottenPeer {
                id: desk,
                key: Some(key(1)),
            }],
        };

        assert_eq!(store.merge(&old, sender, own), Merged::default());
        assert_eq!(store.key_of(desk), Some(key(2)));
        assert!(!store.is_forgotten(desk, None));
    }

    #[test]
    fn a_new_key_seen_first_hand_brings_back_a_forgotten_server_but_not_its_old_key() {
        let (own, sender, desk, witness) = (id(), id(), id(), id());
        let mut store = TrustStore {
            trusted: vec![trusted(desk, 1)],
            forgotten: Vec::new(),
        };
        store.forget(desk, Some(key(1)));
        let update = |entry: TrustedPeer| TrustUpdate {
            trusted: vec![entry],
            forgotten: Vec::new(),
        };

        for refused in [trusted(desk, 1), introduced(desk, 2, witness)] {
            assert_eq!(
                store.merge(&update(refused), sender, own),
                Merged::default()
            );
        }
        assert!(store.is_forgotten(desk, None));

        let merged = store.merge(&update(trusted(desk, 2)), sender, own);

        assert!(merged.changed);
        assert_eq!(store.trusted, [introduced(desk, 2, sender)]);
        assert!(!store.is_forgotten(desk, None));
        assert!(!store.is_forgotten(desk, Some(&key(2))));
        assert!(store.is_forgotten(desk, Some(&key(1))));
        assert!(store.is_key_forgotten(&key(1)));
        let old = TrustUpdate {
            trusted: Vec::new(),
            forgotten: vec![ForgottenPeer {
                id: desk,
                key: Some(key(1)),
            }],
        };
        assert_eq!(store.merge(&old, sender, own), Merged::default());
        assert_eq!(store.key_of(desk), Some(key(2)));
    }
}
