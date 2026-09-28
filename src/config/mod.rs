mod servers;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::lua::{self, emit, ConfigPaths, Process};
use crate::paths;
use crate::settings::Settings;

pub use servers::{add_server, forget_servers, init_name, remove_server};

const OPT: &str = "amux.opt";
const NO_INIT: &str = "none (defaults)";
const NO_SERVERS: &str = "none";

pub fn check(given: Option<&Path>) -> Result<String> {
    let paths = paths::config_paths(given)?;
    for process in [Process::Client, Process::Server] {
        lua::load(&paths, process)?;
    }
    Ok(report(&paths))
}

pub fn defaults() -> Result<String> {
    emit::assignments(OPT, &Settings::default())
}

pub fn path(given: Option<&Path>) -> Result<PathBuf> {
    paths::user_init(given)
}

fn report(paths: &ConfigPaths) -> String {
    let init = paths
        .init
        .as_ref()
        .map_or_else(|| NO_INIT.to_owned(), |init| init.display().to_string());
    let servers = if paths.servers.exists() {
        paths.servers.display().to_string()
    } else {
        NO_SERVERS.to_owned()
    };
    format!("init     {init}\nservers  {servers}\n")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::lua::{INIT_FILE, SERVERS_FILE};

    #[test]
    fn the_defaults_are_a_valid_init_file_that_loads_as_the_defaults() {
        let defaults = defaults().unwrap();
        assert!(
            defaults
                .lines()
                .filter(|line| !line.starts_with(' ') && *line != "}")
                .all(|line| line.starts_with("amux.opt.")),
            "{defaults}"
        );
        assert!(
            defaults.contains("amux.opt.prefix = \"C-b\"\n"),
            "{defaults}"
        );
        assert!(
            defaults.contains("amux.opt.projects_dir = \"~/projects\"\n"),
            "{defaults}"
        );
        assert!(!defaults.contains("amux.opt.servers"), "{defaults}");

        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join(INIT_FILE);
        fs::write(&init, &defaults).unwrap();
        fs::write(
            dir.path().join(SERVERS_FILE),
            "return { laptop = { address = 'ssh://laptop' } }",
        )
        .unwrap();
        let paths = ConfigPaths::new(dir.path().to_owned(), Some(init));
        let loaded = lua::load(&paths, Process::Server).unwrap().settings;
        assert_eq!(loaded.servers.len(), 1);
        assert_eq!(
            Settings {
                servers: Default::default(),
                ..loaded
            },
            Settings::default()
        );
    }

    #[test]
    fn check_names_the_files_it_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join("desk.lua");
        fs::write(&init, "amux.opt.name = 'desk'").unwrap();
        assert_eq!(
            check(Some(&init)).unwrap(),
            format!("init     {}\nservers  none\n", init.display())
        );

        let servers = dir.path().join(SERVERS_FILE);
        fs::write(&servers, "return {}").unwrap();
        assert_eq!(
            check(Some(&init)).unwrap(),
            format!(
                "init     {}\nservers  {}\n",
                init.display(),
                servers.display()
            )
        );

        assert_eq!(
            report(&ConfigPaths::new(dir.path().join("empty"), None)),
            "init     none (defaults)\nservers  none\n"
        );
    }

    #[test]
    fn check_loads_the_config_as_both_the_client_and_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let init = dir.path().join(INIT_FILE);
        fs::write(
            &init,
            "if amux.process == 'server' then\n  amux.opt.bogus = 1\nend",
        )
        .unwrap();
        let error = check(Some(&init)).unwrap_err().to_string();
        assert!(
            error.contains("init.lua:2: unknown option amux.opt.bogus"),
            "{error}"
        );

        let missing = dir.path().join("missing.lua");
        let error = check(Some(&missing)).unwrap_err().to_string();
        assert!(error.starts_with("reading "), "{error}");
    }

    #[test]
    fn the_path_is_the_given_file_or_the_user_init() {
        let given = Path::new("/tmp/amux/desk.lua");
        assert_eq!(path(Some(given)).unwrap(), given);
        assert!(path(None).unwrap().ends_with("amux/init.lua"));
    }
}
