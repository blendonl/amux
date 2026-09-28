use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use mlua::{Lua, Table};

use super::api::{self, Callbacks, Hooks, Registry};
use super::opt::Opt;
use super::{chunk_name, describe};
use crate::settings::{Keymap, Settings};

pub const INIT_FILE: &str = "init.lua";
pub const SYSTEM_INIT: &str = "/etc/amux/init.lua";
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
}

impl ConfigPaths {
    pub fn find(dir: PathBuf, system_init: &Path) -> Self {
        let init = find_init(&dir, system_init);
        Self { dir, init }
    }
}

pub fn find_init(dir: &Path, system_init: &Path) -> Option<PathBuf> {
    [dir.join(INIT_FILE), system_init.to_path_buf()]
        .into_iter()
        .find(|path| path.exists())
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
    if let Some(init) = &paths.init {
        let source = fs::read(init).with_context(|| format!("reading {}", init.display()))?;
        lua.load(source)
            .set_name(chunk_name(init))
            .exec()
            .map_err(|error| anyhow!(describe(&error)))?;
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
        &ConfigPaths {
            dir: dir.path().to_owned(),
            init: Some(init),
        },
        process,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Key;
    use crate::settings::{Color, StyleSpec, Theme};

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
        let paths = ConfigPaths {
            dir: dir.path().to_owned(),
            init: None,
        };
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
    fn an_unknown_option_reports_the_init_line() {
        let error = failure("amux.opt.prefix = 'C-a'\n\namux.opt.bogus = true");
        assert!(
            error.contains("init.lua:3: unknown option amux.opt.bogus, expected one of "),
            "{error}"
        );
        assert!(
            error.contains(
                "borders, escape_time_ms, mouse, notice_ms, pane, prefix, session, status, \
                 theme, tree, window, worktrees"
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
        let paths = ConfigPaths {
            dir: dir.path().to_owned(),
            init: Some(init.clone()),
        };

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
    fn the_user_init_wins_over_the_system_init() {
        let config = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let system_init = system.path().join(INIT_FILE);
        let user_init = config.path().join(INIT_FILE);

        assert_eq!(find_init(config.path(), &system_init), None);

        fs::write(&system_init, "").unwrap();
        assert_eq!(
            find_init(config.path(), &system_init),
            Some(system_init.clone())
        );

        fs::write(&user_init, "").unwrap();
        assert_eq!(
            find_init(config.path(), &system_init),
            Some(user_init.clone())
        );

        fs::remove_file(&system_init).unwrap();
        assert_eq!(
            ConfigPaths::find(config.path().to_owned(), &system_init),
            ConfigPaths {
                dir: config.path().to_owned(),
                init: Some(user_init),
            }
        );
    }
}
