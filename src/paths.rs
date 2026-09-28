use std::env;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nix::unistd::getuid;

use crate::lua::{ConfigPaths, INIT_FILE, SYSTEM_INIT};

const PRIVATE_DIR_MODE: u32 = 0o700;
const CONFIG_DIR: &str = "amux";

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

pub fn config_dir(given: Option<&Path>) -> Result<PathBuf> {
    match given {
        Some(init) => Ok(init
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_owned()),
        None => Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join(CONFIG_DIR)),
    }
}

pub fn config_paths(given: Option<&Path>) -> Result<ConfigPaths> {
    Ok(ConfigPaths::find(
        config_dir(given)?,
        given,
        Path::new(SYSTEM_INIT),
    ))
}

pub fn user_init(given: Option<&Path>) -> Result<PathBuf> {
    match given {
        Some(init) => Ok(init.to_owned()),
        None => Ok(config_dir(None)?.join(INIT_FILE)),
    }
}

pub fn expand_home(path: PathBuf, home: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) if rest.as_os_str().is_empty() => home.to_owned(),
        Ok(rest) => home.join(rest),
        Err(_) => path,
    }
}

pub fn socket_name(socket: &Path) -> Result<String> {
    let name = socket
        .file_name()
        .with_context(|| format!("{} has no file name", socket.display()))?;
    Ok(name.to_string_lossy().into_owned())
}

pub fn state_dir(socket: &Path) -> Result<PathBuf> {
    let dir = xdg_dir("XDG_STATE_HOME", ".local/state")?
        .join("amux")
        .join(socket_name(socket)?);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_leading_tilde_is_expanded() {
        let home = Path::new("/home/tester");
        assert_eq!(expand_home("~".into(), home), PathBuf::from("/home/tester"));
        assert_eq!(
            expand_home("~/a".into(), home),
            PathBuf::from("/home/tester/a")
        );
        assert_eq!(
            expand_home("~other/a".into(), home),
            PathBuf::from("~other/a")
        );
        assert_eq!(expand_home("/srv/~".into(), home), PathBuf::from("/srv/~"));
    }

    #[test]
    fn a_given_config_file_sets_the_config_dir() {
        let given = Path::new("/etc/amux-test/desk.lua");
        assert_eq!(
            config_dir(Some(given)).unwrap(),
            PathBuf::from("/etc/amux-test")
        );
        assert_eq!(
            config_dir(Some(Path::new("init.lua"))).unwrap(),
            PathBuf::from(".")
        );
        assert_eq!(user_init(Some(given)).unwrap(), given);

        let paths = config_paths(Some(given)).unwrap();
        assert_eq!(paths.dir, PathBuf::from("/etc/amux-test"));
        assert_eq!(paths.init.as_deref(), Some(given));
        assert_eq!(paths.servers, PathBuf::from("/etc/amux-test/servers.lua"));
    }
}
