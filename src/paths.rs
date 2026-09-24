use std::env;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nix::unistd::getuid;

const PRIVATE_DIR_MODE: u32 = 0o700;
const CONFIG_FILE: &str = "config.toml";

pub fn default_socket(name: &str) -> Result<PathBuf> {
    Ok(runtime_dir()?.join(name))
}

pub fn log_path(socket: &Path) -> PathBuf {
    let mut path = OsString::from(socket);
    path.push(".log");
    PathBuf::from(path)
}

pub fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn config_path() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?
        .join("amux")
        .join(CONFIG_FILE))
}

pub fn state_dir(socket: &Path) -> Result<PathBuf> {
    let socket_name = socket
        .file_name()
        .with_context(|| format!("{} has no file name", socket.display()))?;
    let dir = xdg_dir("XDG_STATE_HOME", ".local/state")?
        .join("amux")
        .join(socket_name);

    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(&dir, Permissions::from_mode(PRIVATE_DIR_MODE))
        .with_context(|| format!("restricting {}", dir.display()))?;
    Ok(dir)
}

fn xdg_dir(variable: &str, fallback: &str) -> Result<PathBuf> {
    match env::var_os(variable).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        _ => Ok(home_dir()?.join(fallback)),
    }
}

fn runtime_dir() -> Result<PathBuf> {
    let uid = getuid();
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    let dir = base.join(format!("amux-{uid}"));

    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
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
