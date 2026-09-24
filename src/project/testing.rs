use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use super::git;

pub const LOCAL_ONLY: &str = "local-only";
pub const REMOTE_ONLY: &str = "remote-only";

pub struct Fixture {
    _root: TempDir,
    root: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("creating a temp dir");
        let path = fs::canonicalize(root.path()).expect("resolving the temp dir");
        Self {
            _root: root,
            root: path,
        }
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn repo(&self, relative: &str) -> PathBuf {
        let dir = self.path(relative);
        fs::create_dir_all(&dir).expect("creating the repo dir");
        git(&dir, &["init", "--quiet"]);
        commit(&dir, "initial");
        dir
    }

    pub fn origin(&self) -> PathBuf {
        let origin = self.path("origin.git");
        fs::create_dir_all(&origin).expect("creating the origin dir");
        git(&origin, &["init", "--quiet", "--bare"]);

        let seed = self.repo("seed");
        git(&seed, &["remote", "add", "origin", path_str(&origin)]);
        git(&seed, &["switch", "--quiet", "-c", REMOTE_ONLY]);
        commit(&seed, "only on the remote");
        git(&seed, &["switch", "--quiet", "main"]);
        git(&seed, &["push", "--quiet", "origin", "main", REMOTE_ONLY]);
        origin
    }

    pub fn clone_of(&self, origin: &Path, relative: &str) -> PathBuf {
        let dest = self.path(relative);
        git(
            &self.root,
            &["clone", "--quiet", path_str(origin), path_str(&dest)],
        );
        dest
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    git::run(dir, args).unwrap_or_else(|err| panic!("{err:#}"))
}

pub fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["commit", "--quiet", "--allow-empty", "-m", message]);
    head(dir)
}

pub fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"])
}

pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are UTF-8")
}
