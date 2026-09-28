use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::Peer;
use crate::config::ServerId;
use crate::protocol::Via;

pub const CACHE_FILE: &str = "cluster-cache";
const CACHE_MAGIC: &[u8; 8] = b"AMUXCL02";
const CACHE_MAGIC_V1: &[u8; 8] = b"AMUXCL01";

#[derive(Default, Serialize, Deserialize)]
pub struct Cache {
    pub peers: BTreeMap<ServerId, Peer>,
    pub targets: Vec<CachedTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedTarget {
    pub address: String,
    pub peer: Option<ServerId>,
    pub origin: CachedOrigin,
    pub verified: bool,
    pub last_seen: SystemTime,
    pub is_self: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CachedOrigin {
    Configured,
    Gossiped { name: String },
    Discovered { name: String, via: Via },
}

#[derive(Serialize, Deserialize)]
struct CacheV1 {
    peers: BTreeMap<ServerId, Peer>,
    targets: Vec<CachedTargetV1>,
}

#[derive(Serialize, Deserialize)]
struct CachedTargetV1 {
    address: String,
    peer: Option<ServerId>,
    gossiped_name: Option<String>,
    verified: bool,
    last_seen: SystemTime,
}

impl From<CacheV1> for Cache {
    fn from(old: CacheV1) -> Self {
        let targets = old
            .targets
            .into_iter()
            .map(|target| CachedTarget {
                address: target.address,
                peer: target.peer,
                origin: match target.gossiped_name {
                    Some(name) => CachedOrigin::Gossiped { name },
                    None => CachedOrigin::Configured,
                },
                verified: target.verified,
                last_seen: target.last_seen,
                is_self: false,
                last_error: None,
            })
            .collect();
        Self {
            peers: old.peers,
            targets,
        }
    }
}

pub fn load(path: &Path) -> Cache {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Cache::default(),
        Err(err) => {
            warn!(
                "reading {} failed, starting without it: {err}",
                path.display()
            );
            return Cache::default();
        }
    };
    match decode(&bytes) {
        Ok(cache) => cache,
        Err(err) => {
            warn!("dropping the cluster cache {}: {err:#}", path.display());
            Cache::default()
        }
    }
}

fn decode(bytes: &[u8]) -> Result<Cache> {
    if let Some(payload) = bytes.strip_prefix(CACHE_MAGIC) {
        return postcard::from_bytes(payload).context("decoding");
    }
    if let Some(payload) = bytes.strip_prefix(CACHE_MAGIC_V1) {
        let old: CacheV1 = postcard::from_bytes(payload).context("decoding a version 1 cache")?;
        return Ok(old.into());
    }
    bail!("unknown format")
}

pub fn write(path: &Path, cache: &Cache) -> Result<()> {
    let mut bytes = CACHE_MAGIC.to_vec();
    bytes.extend(postcard::to_stdvec(cache)?);
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::cluster::CachedState;
    use crate::config::Incarnation;
    use crate::protocol::{ServerState, Version};

    fn peer(name: &str) -> Peer {
        Peer {
            name: name.into(),
            version: Some(Version::current()),
            last_seen: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1000)),
            stopped: true,
            state: Some(CachedState {
                incarnation: Incarnation::random().unwrap(),
                seq: 4,
                state: ServerState::default(),
            }),
        }
    }

    #[test]
    fn a_version_1_cache_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let desk = ServerId::random().unwrap();
        let seen = SystemTime::UNIX_EPOCH + Duration::from_secs(2000);
        let old = CacheV1 {
            peers: BTreeMap::from([(desk, peer("desk"))]),
            targets: vec![
                CachedTargetV1 {
                    address: "ssh://desk".into(),
                    peer: Some(desk),
                    gossiped_name: None,
                    verified: true,
                    last_seen: seen,
                },
                CachedTargetV1 {
                    address: "ssh://laptop".into(),
                    peer: None,
                    gossiped_name: Some("laptop".into()),
                    verified: false,
                    last_seen: seen,
                },
            ],
        };
        let mut bytes = CACHE_MAGIC_V1.to_vec();
        bytes.extend(postcard::to_stdvec(&old).unwrap());
        fs::write(&path, bytes).unwrap();

        let cache = load(&path);

        assert_eq!(cache.peers.len(), 1);
        assert_eq!(cache.peers[&desk].name, "desk");
        assert!(cache.peers[&desk].stopped);
        assert_eq!(cache.peers[&desk].state.as_ref().unwrap().seq, 4);
        assert_eq!(
            cache.targets,
            vec![
                CachedTarget {
                    address: "ssh://desk".into(),
                    peer: Some(desk),
                    origin: CachedOrigin::Configured,
                    verified: true,
                    last_seen: seen,
                    is_self: false,
                    last_error: None,
                },
                CachedTarget {
                    address: "ssh://laptop".into(),
                    peer: None,
                    origin: CachedOrigin::Gossiped {
                        name: "laptop".into()
                    },
                    verified: false,
                    last_seen: seen,
                    is_self: false,
                    last_error: None,
                },
            ]
        );
    }

    #[test]
    fn a_version_2_cache_round_trips_every_target_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        let target = CachedTarget {
            address: "tcp://100.64.0.2:7447".into(),
            peer: Some(ServerId::random().unwrap()),
            origin: CachedOrigin::Discovered {
                name: "desk".into(),
                via: Via::Tailscale,
            },
            verified: true,
            last_seen: SystemTime::UNIX_EPOCH,
            is_self: true,
            last_error: Some("connection refused".into()),
        };
        let cache = Cache {
            peers: BTreeMap::new(),
            targets: vec![target.clone()],
        };

        write(&path, &cache).unwrap();

        assert!(fs::read(&path).unwrap().starts_with(CACHE_MAGIC));
        assert_eq!(load(&path).targets, vec![target]);
    }

    #[test]
    fn an_unknown_cache_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CACHE_FILE);
        fs::write(&path, b"AMUXCL99garbage").unwrap();
        assert!(load(&path).targets.is_empty());
    }
}
