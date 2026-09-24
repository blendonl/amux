use std::env;
use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nix::unistd::getuid;

const RUNTIME_DIR_MODE: u32 = 0o700;

pub fn default_socket(name: &str) -> Result<PathBuf> {
    Ok(runtime_dir()?.join(name))
}

pub fn log_path(socket: &Path) -> PathBuf {
    let mut path = OsString::from(socket);
    path.push(".log");
    PathBuf::from(path)
}

fn runtime_dir() -> Result<PathBuf> {
    let uid = getuid();
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let dir = base.join(format!("amux-{uid}"));

    DirBuilder::new()
        .recursive(true)
        .mode(RUNTIME_DIR_MODE)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    let metadata = fs::metadata(&dir).with_context(|| format!("reading {}", dir.display()))?;
    if metadata.uid() != uid.as_raw() || metadata.mode() & 0o077 != 0 {
        bail!(
            "{} must be owned by uid {uid} and not accessible to other users",
            dir.display()
        );
    }
    Ok(dir)
}
