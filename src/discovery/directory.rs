use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{watch, Notify};
use tracing::{debug, warn};

use super::{Advertisement, LanDiscovery};
use crate::config::ServerId;

pub const LAN_DIR_ENV: &str = "AMUX_LAN_DIR";
const TEMPORARY_SUFFIX: &str = ".tmp";

pub struct DirectoryLan {
    dir: PathBuf,
    advertised: Mutex<Option<ServerId>>,
    found: watch::Sender<Vec<Advertisement>>,
    stop: Arc<Notify>,
}

impl DirectoryLan {
    pub fn start(dir: PathBuf, interval: Duration) -> Result<Arc<Self>> {
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let lan = Arc::new(Self {
            dir,
            advertised: Mutex::new(None),
            found: watch::channel(Vec::new()).0,
            stop: Arc::new(Notify::new()),
        });
        lan.scan();
        tokio::spawn(poll(Arc::downgrade(&lan), Arc::clone(&lan.stop), interval));
        Ok(lan)
    }

    fn scan(&self) {
        let found = match read_advertisements(&self.dir) {
            Ok(found) => found,
            Err(err) => {
                debug!("scanning {} failed: {err:#}", self.dir.display());
                return;
            }
        };
        self.found.send_if_modified(|current| {
            let changed = *current != found;
            if changed {
                *current = found;
            }
            changed
        });
    }

    fn file(&self, id: ServerId) -> PathBuf {
        self.dir.join(id.to_string())
    }

    fn advertised(&self) -> MutexGuard<'_, Option<ServerId>> {
        self.advertised
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

impl LanDiscovery for DirectoryLan {
    fn advertise(&self, advertisement: &Advertisement) -> Result<()> {
        let path = self.file(advertisement.id);
        let temporary = self
            .dir
            .join(format!("{}{TEMPORARY_SUFFIX}", advertisement.id));
        let text = serde_json::to_string_pretty(advertisement)?;
        fs::write(&temporary, text).with_context(|| format!("writing {}", temporary.display()))?;
        fs::rename(&temporary, &path).with_context(|| format!("replacing {}", path.display()))?;
        *self.advertised() = Some(advertisement.id);
        self.scan();
        Ok(())
    }

    fn withdraw(&self) -> Result<()> {
        let Some(id) = self.advertised().take() else {
            return Ok(());
        };
        let path = self.file(id);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err).with_context(|| format!("removing {}", path.display())),
        }
        self.scan();
        Ok(())
    }

    fn browse(&self) -> watch::Receiver<Vec<Advertisement>> {
        self.found.subscribe()
    }

    fn shutdown(&self) {
        if let Err(err) = self.withdraw() {
            warn!("withdrawing the LAN advertisement failed: {err:#}");
        }
        self.stop.notify_one();
    }
}

async fn poll(lan: Weak<DirectoryLan>, stop: Arc<Notify>, interval: Duration) {
    loop {
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = stop.notified() => return,
        }
        let Some(lan) = lan.upgrade() else {
            return;
        };
        lan.scan();
    }
}

fn read_advertisements(dir: &Path) -> Result<Vec<Advertisement>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') || name.ends_with(TEMPORARY_SUFFIX) {
            continue;
        }
        let text = match fs::read_to_string(entry.path()) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("reading {name}")),
        };
        match serde_json::from_str::<Advertisement>(&text) {
            Ok(advertisement) => found.push(advertisement),
            Err(err) => debug!(
                file = name,
                "ignoring an advertisement that does not parse: {err}"
            ),
        }
    }
    found.sort_by_key(|advertisement| advertisement.id);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PublicKey, PROTOCOL_MAJOR};

    const PATIENCE: Duration = Duration::from_secs(10);
    const INTERVAL: Duration = Duration::from_millis(10);

    fn advertisement(name: &str) -> Advertisement {
        Advertisement {
            id: ServerId::random().unwrap(),
            name: name.into(),
            key: PublicKey([5; 32]),
            cluster: "default".into(),
            proto: PROTOCOL_MAJOR,
            pairing: Some("k7".into()),
            port: 40123,
            addresses: vec!["192.168.0.10".parse().unwrap(), "fe80::2".parse().unwrap()],
        }
    }

    async fn seen(browsing: &mut watch::Receiver<Vec<Advertisement>>, expected: &[Advertisement]) {
        tokio::time::timeout(PATIENCE, browsing.wait_for(|found| found == expected))
            .await
            .expect("timed out waiting for the advertisements")
            .unwrap();
    }

    #[tokio::test]
    async fn servers_sharing_a_directory_see_each_others_advertisements() {
        let dir = tempfile::tempdir().unwrap();
        let desk = DirectoryLan::start(dir.path().to_owned(), INTERVAL).unwrap();
        let laptop = DirectoryLan::start(dir.path().to_owned(), INTERVAL).unwrap();
        let mut browsing = laptop.browse();
        fs::write(dir.path().join("garbage"), "not json").unwrap();
        fs::write(dir.path().join("half-written.tmp"), "{").unwrap();
        let first = advertisement("desk");

        desk.advertise(&first).unwrap();
        seen(&mut browsing, &[first.clone()]).await;

        let second = Advertisement {
            pairing: None,
            ..first.clone()
        };
        desk.advertise(&second).unwrap();
        seen(&mut browsing, &[second]).await;

        desk.withdraw().unwrap();
        seen(&mut browsing, &[]).await;

        desk.advertise(&first).unwrap();
        seen(&mut browsing, &[first.clone()]).await;
        desk.shutdown();
        seen(&mut browsing, &[]).await;
        assert!(!dir.path().join(first.id.to_string()).exists());
    }

    #[test]
    fn an_advertisement_is_readable_json() {
        let advertisement = advertisement("desk");
        let text = serde_json::to_string(&advertisement).unwrap();
        assert!(
            text.contains(&format!("\"id\":\"{}\"", advertisement.id)),
            "{text}"
        );
        assert!(text.contains(&advertisement.key.to_hex()), "{text}");
        assert_eq!(
            serde_json::from_str::<Advertisement>(&text).unwrap(),
            advertisement
        );
    }
}
