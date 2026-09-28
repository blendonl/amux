use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{bail, Context, Result};

use crate::lua::{self, emit, ConfigPaths, Process, INIT_FILE};
use crate::settings::ServerConfig;

type Servers = BTreeMap<String, ServerConfig>;

pub fn add_server(paths: &ConfigPaths, name: &str, server: &ServerConfig) -> Result<()> {
    let mut servers = lua::load_servers(&paths.servers)?;
    if servers.contains_key(name) {
        bail!("{name} is already in {}", paths.servers.display());
    }
    if configured(paths)?.contains_key(name) {
        bail!("{name} is already in {}", init_name(paths));
    }
    servers.insert(name.to_owned(), server.clone());
    write(paths, &servers).map(drop)
}

pub fn remove_server(paths: &ConfigPaths, name: &str) -> Result<()> {
    let mut servers = lua::load_servers(&paths.servers)?;
    if servers.remove(name).is_none() {
        if configured(paths)?.contains_key(name) {
            let init = init_name(paths);
            bail!("{name} is set in {init}, not by `amux servers add`; remove it from {init}");
        }
        bail!("no server named {name} in the config");
    }
    write(paths, &servers).map(drop)
}

pub fn forget_servers(paths: &ConfigPaths, names: &[String]) -> Result<Vec<String>> {
    let mut servers = lua::load_servers(&paths.servers)?;
    let before = servers.len();
    servers.retain(|name, _| !names.contains(name));
    let configured = if servers.len() == before {
        configured(paths)?
    } else {
        write(paths, &servers)?
    };
    Ok(names
        .iter()
        .filter(|name| configured.contains_key(*name))
        .cloned()
        .collect())
}

pub fn init_name(paths: &ConfigPaths) -> String {
    paths
        .init
        .as_deref()
        .map_or_else(|| INIT_FILE.to_owned(), |init| init.display().to_string())
}

fn configured(paths: &ConfigPaths) -> Result<Servers> {
    Ok(lua::load(paths, Process::Server)?.settings.servers)
}

fn write(paths: &ConfigPaths, servers: &Servers) -> Result<Servers> {
    let text = emit::chunk(servers)?;
    fs::create_dir_all(&paths.dir).with_context(|| format!("creating {}", paths.dir.display()))?;
    let temporary = temporary_path(&paths.servers);
    fs::write(&temporary, text).with_context(|| format!("writing {}", temporary.display()))?;
    let proposed = ConfigPaths {
        servers: temporary.clone(),
        ..paths.clone()
    };
    let configured = configured(&proposed).and_then(|configured| {
        fs::rename(&temporary, &paths.servers)
            .with_context(|| format!("replacing {}", paths.servers.display()))?;
        Ok(configured)
    });
    if configured.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    configured.with_context(|| format!("{} was left unchanged", paths.servers.display()))
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(format!(".{}.tmp", process::id()));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::SERVERS_FILE;

    struct Config {
        dir: tempfile::TempDir,
        paths: ConfigPaths,
    }

    impl Config {
        fn new(init: Option<&str>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("amux");
            let init = init.map(|source| {
                fs::create_dir_all(&config).unwrap();
                let path = config.join(INIT_FILE);
                fs::write(&path, source).unwrap();
                path
            });
            let paths = ConfigPaths::new(config, init);
            Self { dir, paths }
        }

        fn servers(&self) -> String {
            fs::read_to_string(&self.paths.servers).unwrap()
        }

        fn leftovers(&self) -> Vec<String> {
            fs::read_dir(&self.paths.dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name.ends_with(".tmp"))
                .collect()
        }
    }

    fn laptop() -> ServerConfig {
        ServerConfig {
            address: "ssh://laptop".into(),
            amux_path: None,
            socket: None,
        }
    }

    #[test]
    fn adding_a_server_creates_servers_lua() {
        let config = Config::new(None);
        let server = ServerConfig {
            amux_path: Some("~/.cargo/bin/amux".into()),
            socket: Some("dev".into()),
            ..laptop()
        };

        add_server(&config.paths, "laptop", &server).unwrap();

        assert_eq!(
            config.servers(),
            "return {\n  laptop = {\n    address = \"ssh://laptop\",\n    amux_path = \"~/.cargo/bin/amux\",\n    socket = \"dev\",\n  },\n}\n"
        );
        assert_eq!(
            config.paths.servers,
            config.dir.path().join("amux").join(SERVERS_FILE)
        );
        assert!(config.leftovers().is_empty());
    }

    #[test]
    fn adding_and_removing_servers_leaves_init_lua_alone() {
        let init = "amux.opt.name = 'desk'\namux.opt.projects.amux = { default_server = 'desk' }\n";
        let config = Config::new(Some(init));

        add_server(&config.paths, "laptop", &laptop()).unwrap();
        add_server(&config.paths, "build-box", &laptop()).unwrap();
        assert_eq!(
            config.servers(),
            "return {\n  [\"build-box\"] = {\n    address = \"ssh://laptop\",\n  },\n  laptop = {\n    address = \"ssh://laptop\",\n  },\n}\n"
        );

        remove_server(&config.paths, "laptop").unwrap();
        remove_server(&config.paths, "build-box").unwrap();
        assert_eq!(config.servers(), "return {}\n");
        assert_eq!(
            fs::read_to_string(config.paths.init.as_ref().unwrap()).unwrap(),
            init
        );
        assert!(config.leftovers().is_empty());
    }

    #[test]
    fn a_name_already_in_either_file_cannot_be_added_again() {
        let config = Config::new(Some("amux.opt.servers.desk = { address = 'ssh://desk' }"));
        add_server(&config.paths, "laptop", &laptop()).unwrap();

        let duplicate = format!(
            "{:#}",
            add_server(&config.paths, "laptop", &laptop()).unwrap_err()
        );
        let servers = config.paths.servers.display().to_string();
        assert_eq!(duplicate, format!("laptop is already in {servers}"));
        let in_init = format!(
            "{:#}",
            add_server(&config.paths, "desk", &laptop()).unwrap_err()
        );
        let init = init_name(&config.paths);
        assert!(init.ends_with(INIT_FILE), "{init}");
        assert_eq!(in_init, format!("desk is already in {init}"));
        assert!(!config.servers().contains("desk"));
    }

    #[test]
    fn a_server_that_only_init_lua_sets_is_removed_there() {
        let config = Config::new(Some("amux.opt.servers.desk = { address = 'ssh://desk' }"));

        let error = remove_server(&config.paths, "desk").unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("desk is set in "), "{error}");
        assert!(
            error.contains("init.lua, not by `amux servers add`"),
            "{error}"
        );
        let missing = remove_server(&config.paths, "nas").unwrap_err();
        assert!(
            format!("{missing:#}").contains("no server named nas"),
            "{missing:#}"
        );
        assert!(!config.paths.servers.exists());
    }

    #[test]
    fn a_change_that_breaks_the_config_is_not_written() {
        let config = Config::new(Some("assert(amux.opt.servers.laptop.address)"));
        fs::write(
            &config.paths.servers,
            "return { laptop = { address = 'ssh://laptop' } }",
        )
        .unwrap();
        let before = config.servers();

        let error = remove_server(&config.paths, "laptop").unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("was left unchanged"), "{error}");
        assert!(error.contains("init.lua:1:"), "{error}");
        assert_eq!(config.servers(), before);
        assert!(config.leftovers().is_empty());

        fs::write(&config.paths.servers, "servers = 3").unwrap();
        assert!(add_server(&config.paths, "nas", &laptop()).is_err());
        assert_eq!(config.servers(), "servers = 3");
    }

    #[test]
    fn forgetting_removes_names_from_servers_lua_and_reports_init_lua_ones() {
        let config = Config::new(Some("amux.opt.servers.desk = { address = 'ssh://desk' }"));
        add_server(&config.paths, "laptop", &laptop()).unwrap();
        add_server(&config.paths, "nas", &laptop()).unwrap();

        let names = ["laptop".to_owned(), "desk".to_owned()];
        assert_eq!(forget_servers(&config.paths, &names).unwrap(), ["desk"]);
        assert!(
            !config.servers().contains("laptop ="),
            "{}",
            config.servers()
        );
        assert!(config.servers().contains("nas ="), "{}", config.servers());

        let none = forget_servers(&config.paths, &["gone".to_owned()]).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn the_temporary_file_sits_next_to_servers_lua() {
        let path = Path::new("/config/amux/servers.lua");
        let temporary = temporary_path(path);
        assert_eq!(temporary.parent(), path.parent());
        assert_eq!(
            temporary.file_name().unwrap().to_string_lossy(),
            format!("servers.lua.{}.tmp", process::id())
        );
    }
}
