use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use amux::project::ProjectId;
use amux::protocol::ProjectRef;
use tempfile::TempDir;

pub const PROJECT: &str = "amux";

const REPOSITORY_VARIABLES: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

pub struct Repos {
    _root: TempDir,
    root: PathBuf,
    origin: PathBuf,
    seed: PathBuf,
}

impl Repos {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().expect("creating a temp dir");
        let root = fs::canonicalize(temp.path()).expect("resolving the temp dir");
        let origin = root.join("origin.git");
        let seed = root.join("seed");
        git(&root, &["init", "--quiet", "--bare", path_str(&origin)]);
        git(&root, &["init", "--quiet", path_str(&seed)]);
        commit(&seed, "initial");
        git(&seed, &["remote", "add", "origin", path_str(&origin)]);
        git(&seed, &["push", "--quiet", "origin", "main"]);
        Self {
            _root: temp,
            root,
            origin,
            seed,
        }
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    pub fn origin(&self) -> &Path {
        &self.origin
    }

    pub fn clone_to(&self, dest: &Path) -> PathBuf {
        git(
            &self.root,
            &["clone", "--quiet", path_str(&self.origin), path_str(dest)],
        );
        dest.to_owned()
    }

    pub fn push_branch(&self, branch: &str, message: &str) -> String {
        git(&self.seed, &["switch", "--quiet", "-c", branch]);
        let pushed = commit(&self.seed, message);
        git(&self.seed, &["push", "--quiet", "origin", branch]);
        git(&self.seed, &["switch", "--quiet", "main"]);
        pushed
    }

    pub fn project_ref(&self) -> ProjectRef {
        ProjectRef {
            id: ProjectId::from_remote_url(path_str(&self.origin)),
            name: PROJECT.to_owned(),
            origin: Some(path_str(&self.origin).to_owned()),
        }
    }
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    for variable in REPOSITORY_VARIABLES {
        command.env_remove(variable);
    }
    let output = command
        .args([
            "-c",
            "user.name=amux tests",
            "-c",
            "user.email=amux-tests@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-C",
        ])
        .arg(dir)
        .args(args)
        .output()
        .expect("running git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf-8 git output")
        .trim()
        .to_owned()
}

pub fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["commit", "--quiet", "--allow-empty", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

pub fn path_str(path: &Path) -> &str {
    path.to_str().expect("test paths are UTF-8")
}
