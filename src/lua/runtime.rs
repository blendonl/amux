use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use mlua::{Lua, LuaSerdeExt, Table};

use super::api::{self, Callbacks, Hooks, Registry};
use super::opt::{Opt, SERIALIZE};
use super::{chunk_name, data, describe, with_full_path};
use crate::settings::{Keymap, ServerConfig, Settings};

pub const INIT_FILE: &str = "init.lua";
pub const SERVERS_FILE: &str = "servers.lua";
pub const SYSTEM_INIT: &str = "/etc/amux/init.lua";
const SERVERS_OPTION: &str = "servers";
const MODULE_DIR: &str = "lua";
const MODULE_PATTERNS: [&str; 2] = ["?.lua", "?/init.lua"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {
    Client,
    Server,
}

impl Process {
    pub fn name(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub dir: PathBuf,
    pub init: Option<PathBuf>,
    pub servers: PathBuf,
}

impl ConfigPaths {
    pub fn new(dir: PathBuf, init: Option<PathBuf>) -> Self {
        let servers = dir.join(SERVERS_FILE);
        Self { dir, init, servers }
    }

    pub fn find(dir: PathBuf, given: Option<&Path>, system_init: &Path) -> Self {
        let init = find_init(&dir, given, system_init);
        Self::new(dir, init)
    }

    pub fn refreshed(&self, system_init: &Path) -> Self {
        match &self.init {
            Some(init) if init != system_init => self.clone(),
            _ => Self::find(self.dir.clone(), None, system_init),
        }
    }
}

pub fn find_init(dir: &Path, given: Option<&Path>, system_init: &Path) -> Option<PathBuf> {
    if let Some(given) = given {
        return Some(given.to_owned());
    }
    [dir.join(INIT_FILE), system_init.to_path_buf()]
        .into_iter()
        .find(|path| path.exists())
}

pub fn load_servers(path: &Path) -> Result<BTreeMap<String, ServerConfig>> {
    match fs::read(path) {
        Ok(source) => data::parse(path, &source),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    pub keymap: Keymap,
    pub callbacks: Callbacks,
    pub hooks: Hooks,
    pub lua: Lua,
}

pub fn load(paths: &ConfigPaths, process: Process) -> Result<Loaded> {
    let lua = Lua::new();
    add_module_path(&lua, &paths.dir)?;
    let opt = Opt::<Settings>::install(&lua)?;
    api::install(&lua, opt.proxy(), process)?;
    merge_servers(&lua, &opt, &paths.servers)?;
    if let Some(init) = &paths.init {
        let source = fs::read(init).with_context(|| format!("reading {}", init.display()))?;
        lua.load(source)
            .set_name(chunk_name(init))
            .exec()
            .map_err(|error| anyhow!(with_full_path(describe(&error), init)))?;
    }
    let Registry {
        keymap,
        mut callbacks,
        hooks,
    } = lua
        .remove_app_data::<Registry>()
        .context("the amux registry is missing")?;
    let settings = opt
        .settings(&lua, |callback| callbacks.register(callback).0)
        .and_then(|settings| settings.validate().map(|()| settings))
        .map_err(|message| match &paths.init {
            Some(init) => anyhow!("{}: {message}", init.display()),
            None => anyhow!(message),
        })?;
    opt.close();
    Ok(Loaded {
        settings,
        keymap,
        callbacks,
        hooks,
        lua,
    })
}

fn merge_servers(lua: &Lua, opt: &Opt<Settings>, path: &Path) -> Result<()> {
    let servers: Table = opt.proxy().get(SERVERS_OPTION)?;
    for (name, server) in load_servers(path)? {
        servers.set(name, lua.to_value_with(&server, SERIALIZE)?)?;
    }
    Ok(())
}

fn add_module_path(lua: &Lua, dir: &Path) -> mlua::Result<()> {
    let package: Table = lua.globals().get("package")?;
    let modules = dir.join(MODULE_DIR);
    let mut path = Vec::new();
    for pattern in MODULE_PATTERNS {
        path.extend_from_slice(modules.join(pattern).as_os_str().as_bytes());
        path.push(b';');
    }
    path.extend_from_slice(&package.get::<mlua::String>("path")?.as_bytes());
    package.set("path", lua.create_string(path)?)
}

#[cfg(test)]
pub(super) fn load_init(source: &str, process: Process) -> Result<Loaded> {
    let dir = tempfile::tempdir()?;
    let init = dir.path().join(INIT_FILE);
    fs::write(&init, source)?;
    load(
        &ConfigPaths::new(dir.path().to_owned(), Some(init)),
        process,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Key;
    use crate::settings::{
        ClusterSettings, Color, DiscoverySettings, LanSettings, ProjectConfig, SshSettings,
        StyleSpec, Theme,
    };

    fn loaded(source: &str) -> Loaded {
        load_init(source, Process::Client).unwrap()
    }

    fn failure(source: &str) -> String {
        match load_init(source, Process::Client) {
            Ok(_) => panic!("{source:?} loaded"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn without_an_init_file_everything_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::new(dir.path().to_owned(), None);
        let loaded = load(&paths, Process::Server).unwrap();
        assert_eq!(loaded.settings, Settings::default());
        assert_eq!(loaded.keymap, Keymap::default());
        assert!(loaded.callbacks.is_empty());
        assert_eq!(loaded.hooks.events().count(), 0);
    }

    #[test]
    fn the_defaults_round_trip_through_amux_opt() {
        let loaded = loaded(
            "local opt = amux.opt\n\
             assert(opt.escape_time_ms == 50, tostring(opt.escape_time_ms))\n\
             assert(opt.prefix == 'C-b')\n\
             assert(opt.status.window_format == ' {index}:{name} ')\n\
             assert(opt.theme.status.bg == 'green')\n\
             local function rewrite(proxy)\n\
               for key, value in pairs(proxy) do\n\
                 if type(value) == 'table' then rewrite(value) end\n\
                 proxy[key] = value\n\
               end\n\
             end\n\
             rewrite(opt)",
        );
        assert_eq!(loaded.settings, Settings::default());
    }

    #[test]
    fn options_set_the_prefix_theme_and_status() {
        let loaded = loaded(
            "local opt = amux.opt\n\
             opt.prefix = 'C-a'\n\
             opt.escape_time_ms = 30\n\
             opt.theme.status = { fg = 'black', bg = '#8ec07c' }\n\
             opt.theme.tree_cursor.fg = 'bright-yellow'\n\
             opt.status.window_format = '{index}|{name}'\n\
             assert(opt.prefix == 'C-a')",
        );
        assert_eq!(
            loaded.settings,
            Settings {
                prefix: Key::ctrl('a'),
                escape_time_ms: 30,
                theme: Theme {
                    status: StyleSpec::colors(Color::BLACK, Color::Rgb(0x8e, 0xc0, 0x7c)),
                    tree_cursor: StyleSpec {
                        fg: Some(Color::Indexed(11)),
                        ..StyleSpec::REVERSE
                    },
                    ..Theme::default()
                },
                status: crate::settings::StatusSettings {
                    window_format: "{index}|{name}".into(),
                    ..Default::default()
                },
                ..Settings::default()
            }
        );
    }

    #[test]
    fn host_options_are_set_through_amux_opt() {
        let loaded = loaded(
            "local opt = amux.opt\n\
             opt.pane.shell = { '/usr/bin/fish', '-l' }\n\
             opt.pane.term = 'tmux-256color'\n\
             opt.pane.env.EDITOR = 'vi'\n\
             opt.window.base_index = 1\n\
             opt.borders.vertical = '|'\n\
             opt.theme.pane_border_active = { fg = 'bright-blue' }\n\
             assert(opt.borders.cross == '┼')",
        );
        let settings = loaded.settings;
        assert_eq!(
            settings.pane.shell,
            Some(vec!["/usr/bin/fish".into(), "-l".into()])
        );
        assert_eq!(settings.pane.term, "tmux-256color");
        assert_eq!(
            settings.pane.env.get("EDITOR").map(String::as_str),
            Some("vi")
        );
        assert_eq!(settings.window.base_index, 1);
        assert_eq!(settings.borders.vertical, '|');
        assert_eq!(
            settings.theme.pane_border_active,
            StyleSpec {
                fg: Some(Color::Indexed(12)),
                ..StyleSpec::EMPTY
            }
        );

        let error = failure("amux.opt.borders.vertical = '||'");
        assert!(
            error.contains("init.lua:1: amux.opt.borders.vertical: invalid value"),
            "{error}"
        );
    }

    #[test]
    fn cluster_and_discovery_options_are_set_through_amux_opt() {
        let loaded = loaded(
            "local opt = amux.opt\n\
             assert(opt.cluster.ping_interval_ms == 5000)\n\
             assert(opt.cluster.ssh.program == 'ssh')\n\
             assert(opt.discovery.interval_ms == 30000)\n\
             assert(opt.discovery.tailscale_port == nil)\n\
             opt.cluster.ping_interval_ms = 1000\n\
             opt.cluster.backoff_max_ms = 5000\n\
             opt.cluster.ssh.program = 'autossh'\n\
             opt.cluster.ssh.options = { '-T', '-o', 'BatchMode=yes' }\n\
             opt.discovery.tailscale = false\n\
             opt.discovery.tailscale_tags = { 'tag:amux' }\n\
             opt.discovery.tailscale_port = 7500\n\
             opt.discovery.interval_ms = 10000\n\
             opt.lan.port = 7448\n\
             opt.lan.pairing_window_ms = 60000\n\
             assert(opt.cluster.ssh.default_amux_path == 'amux')",
        );
        let settings = loaded.settings;
        assert_eq!(
            settings.cluster,
            ClusterSettings {
                ping_interval_ms: 1000,
                backoff_max_ms: 5000,
                ssh: SshSettings {
                    program: "autossh".into(),
                    options: vec!["-T".into(), "-o".into(), "BatchMode=yes".into()],
                    ..SshSettings::default()
                },
                ..ClusterSettings::default()
            }
        );
        assert_eq!(
            settings.discovery,
            DiscoverySettings {
                tailscale: false,
                tailscale_tags: vec!["tag:amux".into()],
                tailscale_port: std::num::NonZeroU16::new(7500),
                interval_ms: 10_000,
                ..DiscoverySettings::default()
            }
        );
        assert_eq!(
            settings.lan,
            LanSettings {
                port: 7448,
                pairing_window_ms: 60_000,
                ..LanSettings::default()
            }
        );

        let error = failure("amux.opt.cluster.backof_max_ms = 1");
        assert!(
            error.contains("init.lua:1: unknown option amux.opt.cluster.backof_max_ms"),
            "{error}"
        );
        let error = failure("amux.opt.discovery.tailscale_port = 0");
        assert!(
            error.contains("init.lua:1: amux.opt.discovery.tailscale_port: invalid value"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_option_reports_the_init_line() {
        let error = failure("amux.opt.prefix = 'C-a'\n\namux.opt.bogus = true");
        assert!(
            error.contains("init.lua:3: unknown option amux.opt.bogus, expected one of "),
            "{error}"
        );
        assert!(
            error.contains(
                "borders, clipboard, cluster, discovery, escape_time_ms, images, lan, mouse, name, \
                 notice_ms, pane, prefix, projects, projects_dir, search, servers, session, \
                 status, theme, tree, which_key, window, worktrees"
            ),
            "{error}"
        );

        let error = failure("amux.opt.status.intervall = 5");
        assert!(
            error.contains("init.lua:1: unknown option amux.opt.status.intervall"),
            "{error}"
        );
    }

    #[test]
    fn a_wrong_type_reports_the_init_line() {
        let error = failure("\namux.opt.escape_time_ms = 'fast'");
        assert!(
            error.contains("init.lua:2: amux.opt.escape_time_ms: invalid type: string \"fast\""),
            "{error}"
        );
        let error = failure("amux.opt.status.enabled = 'yes'");
        assert!(
            error.contains("init.lua:1: amux.opt.status.enabled: invalid type"),
            "{error}"
        );
    }

    #[test]
    fn a_deserialize_error_names_the_settings_path() {
        let error = failure("amux.opt.status.window_format = 5");
        assert!(
            error.contains("init.lua:1: amux.opt.status.window_format: invalid type: integer `5`"),
            "{error}"
        );
        let error = failure("amux.opt.theme.status = { fg = 'purple' }");
        assert!(
            error.contains("init.lua:1: amux.opt.theme.status.fg: invalid color \"purple\""),
            "{error}"
        );
        let error = failure("amux.opt.prefix = 'C-'");
        assert!(
            error.contains("init.lua:1: amux.opt.prefix: invalid key \"C-\""),
            "{error}"
        );
    }

    #[test]
    fn lua_errors_carry_the_file_and_line() {
        let error = failure("local x = 1\nerror('boom')");
        assert!(error.ends_with("init.lua:2: boom"), "{error}");
        let error = failure("amux.opt.prefix = \n");
        assert!(error.contains("init.lua:2:"), "{error}");
        assert!(!error.contains("stack traceback"), "{error}");
    }

    #[test]
    fn errors_name_the_whole_path_of_a_deeply_nested_config() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir
            .path()
            .join("a-directory-name-long-enough")
            .join("to-push-the-path-past-what-lua-shows");
        fs::create_dir_all(&deep).unwrap();
        let init = deep.join(INIT_FILE);
        fs::write(&init, "\namux.opt.bogus = 1").unwrap();
        fs::write(
            deep.join(SERVERS_FILE),
            "return {\n  laptop = { address = nil + 1 },\n}",
        )
        .unwrap();
        let paths = ConfigPaths::new(deep.clone(), Some(init.clone()));

        let error = load(&paths, Process::Server).unwrap_err().to_string();
        let servers = deep.join(SERVERS_FILE).display().to_string();
        assert!(error.starts_with(&format!("{servers}:2: ")), "{error}");

        fs::remove_file(deep.join(SERVERS_FILE)).unwrap();
        let error = load(&paths, Process::Server).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!(
                "{}:2: unknown option amux.opt.bogus",
                init.display()
            )),
            "{error}"
        );
    }

    #[test]
    fn modules_are_required_from_the_lua_directory() {
        let dir = tempfile::tempdir().unwrap();
        let modules = dir.path().join(MODULE_DIR);
        fs::create_dir_all(modules.join("keys")).unwrap();
        fs::write(
            modules.join("theme.lua"),
            "amux.opt.theme.message = { fg = 'red' }\nreturn { applied = true }",
        )
        .unwrap();
        fs::write(
            modules.join("keys").join("init.lua"),
            "amux.keymap.set('prefix', '|', amux.action.split_pane('left-right'))",
        )
        .unwrap();
        fs::write(modules.join("broken.lua"), "\namux.opt.bogus = 1").unwrap();
        let init = dir.path().join(INIT_FILE);
        let paths = ConfigPaths::new(dir.path().to_owned(), Some(init.clone()));

        fs::write(&init, "assert(require('theme').applied)\nrequire('keys')").unwrap();
        let loaded = load(&paths, Process::Client).unwrap();
        assert_eq!(loaded.settings.theme.message.fg, Some(Color::RED));
        assert_eq!(
            loaded.keymap.prefix.get(&Key::char('|')),
            Some(&crate::settings::Binding::SplitPane(
                crate::protocol::Split::LeftRight
            ))
        );

        fs::write(&init, "require('broken')").unwrap();
        let error = load(&paths, Process::Client).unwrap_err().to_string();
        assert!(
            error.contains("broken.lua:2: unknown option amux.opt.bogus"),
            "{error}"
        );
    }

    #[test]
    fn the_process_and_hostname_are_exposed() {
        let source = "assert(amux.process == 'client')\n\
                      assert(#amux.hostname() > 0)\n\
                      amux.log('hello', 1, nil, true)";
        load_init(source, Process::Client).unwrap();
        let server = load_init("assert(amux.process == 'server')", Process::Server);
        assert!(server.is_ok());
        let hostname = nix::unistd::gethostname().unwrap();
        let loaded = loaded("return");
        let found: String = loaded.lua.load("amux.hostname()").eval().unwrap();
        assert_eq!(found, hostname.to_string_lossy());
    }

    #[test]
    fn the_amux_table_keeps_its_api() {
        let error = failure("amux.opt = { prefix = 'C-a' }");
        assert!(
            error.contains("init.lua:1: amux.opt cannot be replaced"),
            "{error}"
        );
        let error = failure("amux.keymap = nil");
        assert!(
            error.contains("init.lua:1: amux.keymap cannot be replaced"),
            "{error}"
        );
        loaded("amux.helpers = { answer = 42 }\nassert(amux.helpers.answer == 42)");
    }

    #[test]
    fn status_functions_are_stored_as_callbacks() {
        let loaded = loaded(
            "amux.opt.status.interval_ms = 1000\n\
             amux.opt.status.right = function(ctx) return 'right' end\n\
             amux.keymap.set('prefix', 'g', function() end)\n\
             amux.opt.status.left = function(ctx) return ctx.session end\n\
             assert(type(amux.opt.status.left) == 'function')",
        );
        let status = &loaded.settings.status;
        assert_eq!(status.interval_ms, 1000);
        assert_eq!(loaded.callbacks.len(), 3);
        let (left, right) = (status.left.unwrap(), status.right.unwrap());
        assert_ne!(left, right);
        let context = loaded.lua.create_table().unwrap();
        context.set("session", "work").unwrap();
        let session: String = loaded.callbacks.get(left).unwrap().call(context).unwrap();
        assert_eq!(session, "work");
        let text: String = loaded.callbacks.get(right).unwrap().call(()).unwrap();
        assert_eq!(text, "right");

        let error = failure("amux.opt.status.left = 'session'");
        assert!(
            error.contains("init.lua:1: amux.opt.status.left: expected a function, not a string"),
            "{error}"
        );
    }

    #[test]
    fn amux_opt_is_closed_once_the_configuration_is_loaded() {
        let loaded = loaded("amux.opt.prefix = 'C-a'");
        let error = describe(
            &loaded
                .lua
                .load("assert(amux.opt.prefix == 'C-a')\namux.opt.prefix = 'C-b'")
                .exec()
                .unwrap_err(),
        );
        assert!(
            error.contains("amux.opt.prefix cannot change after the configuration is loaded"),
            "{error}"
        );
        assert_eq!(loaded.settings.prefix, Key::ctrl('a'));
    }

    #[test]
    fn the_given_file_wins_then_the_user_init_then_the_system_init() {
        let config = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let system_init = system.path().join(INIT_FILE);
        let user_init = config.path().join(INIT_FILE);
        let given = system.path().join("given.lua");

        assert_eq!(find_init(config.path(), None, &system_init), None);
        assert_eq!(
            find_init(config.path(), Some(&given), &system_init),
            Some(given.clone())
        );

        fs::write(&system_init, "").unwrap();
        assert_eq!(
            find_init(config.path(), None, &system_init),
            Some(system_init.clone())
        );

        fs::write(&user_init, "").unwrap();
        assert_eq!(
            find_init(config.path(), None, &system_init),
            Some(user_init.clone())
        );
        assert_eq!(
            find_init(config.path(), Some(&given), &system_init),
            Some(given)
        );

        fs::remove_file(&system_init).unwrap();
        assert_eq!(
            ConfigPaths::find(config.path().to_owned(), None, &system_init),
            ConfigPaths {
                dir: config.path().to_owned(),
                init: Some(user_init),
                servers: config.path().join(SERVERS_FILE),
            }
        );
    }

    #[test]
    fn a_refresh_finds_an_init_file_created_after_the_start() {
        let config = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let system_init = system.path().join(INIT_FILE);
        let user_init = config.path().join(INIT_FILE);
        let given = system.path().join("given.lua");

        let started = ConfigPaths::find(config.path().to_owned(), None, &system_init);
        assert_eq!(started.init, None);
        fs::write(&system_init, "").unwrap();
        let refreshed = started.refreshed(&system_init);
        assert_eq!(refreshed.init, Some(system_init.clone()));
        fs::write(&user_init, "").unwrap();
        assert_eq!(
            refreshed.refreshed(&system_init),
            ConfigPaths::new(config.path().to_owned(), Some(user_init.clone()))
        );

        let chosen = ConfigPaths::find(config.path().to_owned(), Some(&given), &system_init);
        assert_eq!(chosen.refreshed(&system_init), chosen);
        fs::remove_file(&user_init).unwrap();
        let kept = ConfigPaths::new(config.path().to_owned(), Some(user_init));
        assert_eq!(kept.refreshed(&system_init), kept);
    }

    fn with_servers(servers: &str, init: &str) -> (tempfile::TempDir, Result<Loaded>) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(SERVERS_FILE), servers).unwrap();
        let path = dir.path().join(INIT_FILE);
        fs::write(&path, init).unwrap();
        let paths = ConfigPaths::new(dir.path().to_owned(), Some(path));
        let loaded = load(&paths, Process::Server);
        (dir, loaded)
    }

    fn server(address: &str) -> ServerConfig {
        ServerConfig {
            address: address.into(),
            amux_path: None,
            socket: None,
        }
    }

    #[test]
    fn servers_lua_is_merged_before_init_lua_runs() {
        let (_dir, loaded) = with_servers(
            "return {\n\
               laptop = { address = 'ssh://laptop', socket = 'dev' },\n\
               nas = { address = 'ssh://nas' },\n\
             }",
            "local servers = amux.opt.servers\n\
             assert(servers.laptop.address == 'ssh://laptop')\n\
             assert(servers.laptop.amux_path == nil)\n\
             servers.nas = { address = 'ssh://nas.lan' }\n\
             servers.desk = { address = 'ssh://desk' }",
        );
        let servers = loaded.unwrap().settings.servers;
        assert_eq!(
            servers,
            BTreeMap::from([
                ("desk".into(), server("ssh://desk")),
                (
                    "laptop".into(),
                    ServerConfig {
                        socket: Some("dev".into()),
                        ..server("ssh://laptop")
                    }
                ),
                ("nas".into(), server("ssh://nas.lan")),
            ])
        );
    }

    #[test]
    fn replacing_amux_opt_servers_drops_the_servers_lua_entries() {
        let (_dir, loaded) = with_servers(
            "return { laptop = { address = 'ssh://laptop' } }",
            "amux.opt.servers = { desk = { address = 'ssh://desk' } }",
        );
        assert_eq!(
            loaded.unwrap().settings.servers,
            BTreeMap::from([("desk".into(), server("ssh://desk"))])
        );
    }

    #[test]
    fn a_broken_servers_lua_names_its_file() {
        let (dir, loaded) = with_servers("return { laptop = { adress = 'x' } }", "");
        let error = loaded.unwrap_err().to_string();
        let path = dir.path().join(SERVERS_FILE);
        assert!(
            error.starts_with(&format!("{}: laptop", path.display())),
            "{error}"
        );
        assert!(error.contains("unknown field `adress`"), "{error}");

        let (_dir, loaded) =
            with_servers("return { laptop = { address = os.getenv('HOME') } }", "");
        let error = loaded.unwrap_err().to_string();
        assert!(
            error.contains("servers.lua:1: a data file cannot read the global os"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_servers_lua_means_no_servers() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_servers(&dir.path().join(SERVERS_FILE)).unwrap(),
            BTreeMap::new()
        );
        assert!(load_servers(dir.path()).is_err());
    }

    #[test]
    fn project_and_server_options_are_set_through_amux_opt() {
        let loaded = loaded(
            "local opt = amux.opt\n\
             assert(opt.name == nil)\n\
             assert(opt.projects_dir == '~/projects')\n\
             opt.name = 'desktop'\n\
             opt.projects_dir = '~/code'\n\
             opt.servers.laptop = { address = 'ssh://laptop' }\n\
             opt.servers['home-server'] = {\n\
               address = 'ssh://notpc@home-server',\n\
               amux_path = '~/.cargo/bin/amux',\n\
               socket = 'dev',\n\
             }\n\
             opt.projects.amux = { default_server = 'desktop', worktrees_dir = '~/projects/amux-worktrees' }\n\
             opt.projects.notes = {}\n\
             assert(opt.search.project_dirs[2] == '~/Projects')\n\
             opt.search.project_dirs = { '~/code', '/srv/git' }\n\
             opt.search.project_depth = 2",
        );
        let settings = loaded.settings;
        assert_eq!(settings.name.as_deref(), Some("desktop"));
        assert_eq!(settings.projects_dir, PathBuf::from("~/code"));
        assert_eq!(
            settings.search.project_dirs,
            [PathBuf::from("~/code"), PathBuf::from("/srv/git")]
        );
        assert_eq!(settings.search.project_depth, 2);
        assert_eq!(settings.servers["laptop"], server("ssh://laptop"));
        assert_eq!(
            settings.servers["home-server"],
            ServerConfig {
                address: "ssh://notpc@home-server".into(),
                amux_path: Some("~/.cargo/bin/amux".into()),
                socket: Some("dev".into()),
            }
        );
        assert_eq!(
            settings.projects["amux"],
            ProjectConfig {
                default_server: Some("desktop".into()),
                worktrees_dir: Some(PathBuf::from("~/projects/amux-worktrees")),
            }
        );
        assert_eq!(settings.projects["notes"], ProjectConfig::default());
    }

    #[test]
    fn unknown_project_and_server_fields_are_rejected() {
        let error = failure("amux.opt.servers.laptop = { address = 'ssh://laptop', port = 22 }");
        assert!(
            error.contains("init.lua: amux.opt.servers.laptop.port: unknown field `port`"),
            "{error}"
        );
        let error = failure("amux.opt.servers.laptop = { amux_path = 'amux' }");
        assert!(
            error.contains("amux.opt.servers.laptop: missing field `address`"),
            "{error}"
        );
        let error = failure("amux.opt.projects.amux = { branch = 'main' }");
        assert!(
            error.contains("init.lua: amux.opt.projects.amux.branch: unknown field `branch`"),
            "{error}"
        );
        let error = failure("amux.opt.discovery.mdns = true");
        assert!(
            error.contains("init.lua:1: unknown option amux.opt.discovery.mdns"),
            "{error}"
        );
    }

    #[test]
    fn invalid_values_are_reported_against_the_init_file() {
        let error = failure("amux.opt.name = ' '");
        assert!(
            error.ends_with("init.lua: amux.opt.name must not be blank"),
            "{error}"
        );
        let error = failure("amux.opt.cluster.status_interval_ms = 0");
        assert!(
            error.ends_with(
                "init.lua: amux.opt.cluster.status_interval_ms must be a positive number"
            ),
            "{error}"
        );
    }
}
