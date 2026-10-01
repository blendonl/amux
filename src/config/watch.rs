use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::lua::{ConfigPaths, INIT_FILE, MODULE_DIR};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Fingerprint(BTreeMap<PathBuf, (SystemTime, u64)>);

impl Fingerprint {
    fn of(paths: &ConfigPaths) -> Self {
        let mut fingerprint = Self::default();
        fingerprint.add(&paths.dir.join(INIT_FILE));
        if let Some(init) = &paths.init {
            fingerprint.add(init);
        }
        fingerprint.add(&paths.servers);
        fingerprint.add_tree(&paths.dir.join(MODULE_DIR));
        fingerprint
    }

    fn add(&mut self, path: &Path) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        if metadata.is_file() {
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            self.0.insert(path.to_owned(), (modified, metadata.len()));
        }
    }

    fn add_tree(&mut self, dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => self.add_tree(&path),
                Ok(_) => self.add(&path),
                Err(_) => {}
            }
        }
    }
}

#[derive(Debug)]
pub struct Watch {
    seen: Fingerprint,
    settling: Option<Fingerprint>,
}

impl Watch {
    pub fn new(paths: &ConfigPaths) -> Self {
        Self {
            seen: Fingerprint::of(paths),
            settling: None,
        }
    }

    pub fn reset(&mut self, paths: &ConfigPaths) {
        *self = Self::new(paths);
    }

    pub fn changed(&mut self, paths: &ConfigPaths) -> bool {
        let now = Fingerprint::of(paths);
        if now == self.seen {
            self.settling = None;
            return false;
        }
        if self.settling.as_ref() == Some(&now) {
            self.seen = now;
            self.settling = None;
            return true;
        }
        self.settling = Some(now);
        false
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::lua::SERVERS_FILE;

    fn settled(watch: &mut Watch, paths: &ConfigPaths) -> bool {
        let first = watch.changed(paths);
        assert!(!first, "a change is reported only once it has settled");
        watch.changed(paths)
    }

    #[test]
    fn a_saved_file_is_reported_once_it_stops_changing() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join(INIT_FILE);
        fs::write(&init, "amux.opt.prefix = 'C-b'").unwrap();
        let paths = ConfigPaths::new(dir.path().to_owned(), Some(init.clone()));
        let mut watch = Watch::new(&paths);
        assert!(!watch.changed(&paths));

        fs::write(&init, "amux.opt.prefix = 'C-a'\n").unwrap();
        assert!(settled(&mut watch, &paths));
        assert!(!watch.changed(&paths));

        fs::write(&init, "amux.opt.prefix = 'C-x'\n\n").unwrap();
        assert!(!watch.changed(&paths));
        fs::write(&init, "amux.opt.prefix = 'C-y'\n\n\n").unwrap();
        assert!(!watch.changed(&paths));
        assert!(watch.changed(&paths));
    }

    #[test]
    fn modules_servers_and_a_new_init_count_as_changes() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::new(dir.path().to_owned(), None);
        let mut watch = Watch::new(&paths);

        fs::write(dir.path().join(INIT_FILE), "amux.opt.notice_ms = 1").unwrap();
        assert!(settled(&mut watch, &paths));

        fs::write(dir.path().join(SERVERS_FILE), "return {}").unwrap();
        assert!(settled(&mut watch, &paths));

        let nested = dir.path().join(MODULE_DIR).join("keys");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("init.lua"), "return {}").unwrap();
        assert!(settled(&mut watch, &paths));

        fs::remove_file(nested.join("init.lua")).unwrap();
        assert!(settled(&mut watch, &paths));
    }

    #[test]
    fn a_config_outside_the_dir_is_watched_and_devices_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let given = dir.path().join("elsewhere.lua");
        fs::write(&given, "").unwrap();
        let paths = ConfigPaths::new(dir.path().join("config"), Some(given.clone()));
        let mut watch = Watch::new(&paths);
        fs::write(&given, "amux.opt.notice_ms = 10").unwrap();
        assert!(settled(&mut watch, &paths));

        let null = ConfigPaths::new(PathBuf::from("/dev"), Some(PathBuf::from("/dev/null")));
        assert_eq!(Fingerprint::of(&null), Fingerprint::default());
    }

    #[test]
    fn reset_takes_the_files_as_they_are_now() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join(INIT_FILE);
        fs::write(&init, "").unwrap();
        let paths = ConfigPaths::new(dir.path().to_owned(), Some(init.clone()));
        let mut watch = Watch::new(&paths);
        fs::write(&init, "amux.opt.notice_ms = 10").unwrap();
        assert!(!watch.changed(&paths));
        watch.reset(&paths);
        assert!(!watch.changed(&paths));
        assert!(!watch.changed(&paths));
    }
}
