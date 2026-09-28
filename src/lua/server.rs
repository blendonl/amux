use std::sync::{Arc, Weak};
use std::time::Duration;

use mlua::{AppDataRefMut, Function, Lua, LuaSerdeExt, SerializeOptions, Table, Value};

use super::api::Hooks;
use super::runtime::Loaded;
use super::{call_within, describe, from_lua, function};
use crate::protocol::{SessionInfo, WindowSummary};
use crate::server::lua_host::{HookAction, HookEvent, HookState};
use crate::target::validate_session_name;

const HOOK_BUDGET: Duration = Duration::from_secs(1);
const UNNAMED_HOOK: &str = "a hook";

pub struct LuaHooks {
    lua: Lua,
    hooks: Hooks,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HookRun {
    pub hook: String,
    pub result: Result<Vec<HookAction>, String>,
}

struct HookCall {
    actions: Vec<HookAction>,
    state: Option<Weak<dyn HookState>>,
}

impl LuaHooks {
    pub fn new(loaded: Loaded) -> Self {
        let Loaded { hooks, lua, .. } = loaded;
        Self { lua, hooks }
    }

    pub fn run(&self, event: &HookEvent, state: Option<&Weak<dyn HookState>>) -> Vec<HookRun> {
        self.hooks
            .get(event.name())
            .iter()
            .map(|hook| HookRun {
                hook: hook_name(hook),
                result: self.call(hook, event, state),
            })
            .collect()
    }

    fn call(
        &self,
        hook: &Function,
        event: &HookEvent,
        state: Option<&Weak<dyn HookState>>,
    ) -> Result<Vec<HookAction>, String> {
        let options = SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false);
        let payload = self
            .lua
            .to_value_with(event, options)
            .map_err(|error| describe(&error))?;
        self.lua.set_app_data(HookCall {
            actions: Vec::new(),
            state: state.cloned(),
        });
        let result = call_within::<()>(&self.lua, hook, payload, HOOK_BUDGET, UNNAMED_HOOK);
        let call = self.lua.remove_app_data::<HookCall>();
        result.map_err(|error| describe(&error))?;
        Ok(call.map(|call| call.actions).unwrap_or_default())
    }
}

fn hook_name(hook: &Function) -> String {
    let info = hook.info();
    match (info.short_src, info.line_defined) {
        (Some(source), Some(line)) => format!("{source}:{line}"),
        _ => UNNAMED_HOOK.to_owned(),
    }
}

pub(super) fn install(lua: &Lua, api: &Table) -> mlua::Result<()> {
    api.set(
        "server_name",
        function(lua, |lua, ()| {
            Ok(state(lua, "amux.server_name")?.server_name())
        })?,
    )?;
    api.set(
        "sessions",
        function(lua, |lua, ()| {
            state(lua, "amux.sessions")?
                .sessions()
                .iter()
                .map(|session| session_table(lua, session))
                .collect::<mlua::Result<Vec<_>>>()
        })?,
    )?;
    api.set(
        "session",
        function(lua, |lua, name: String| {
            state(lua, "amux.session")?
                .sessions()
                .iter()
                .find(|session| session.name == name)
                .map(|session| session_table(lua, session))
                .transpose()
        })?,
    )?;
    for name in ["new_window", "rename_window", "send_keys"] {
        api.set(
            name,
            function(lua, move |lua, spec: Value| request(lua, name, spec))?,
        )?;
    }
    api.set(
        "rename_session",
        function(lua, |lua, (session, name): (String, String)| {
            pending(lua, "amux.rename_session")?;
            validate_session_name(&name)
                .map_err(|error| mlua::Error::runtime(format!("{error:#}")))?;
            queue(
                lua,
                "amux.rename_session",
                HookAction::RenameSession { session, name },
            )
        })?,
    )?;
    api.set(
        "kill_session",
        function(lua, |lua, session: String| {
            queue(
                lua,
                "amux.kill_session",
                HookAction::KillSession { session },
            )
        })?,
    )?;
    Ok(())
}

fn pending<'a>(lua: &'a Lua, name: &str) -> mlua::Result<AppDataRefMut<'a, HookCall>> {
    lua.app_data_mut::<HookCall>()
        .ok_or_else(|| mlua::Error::runtime(format!("{name} only works inside a hook")))
}

fn queue(lua: &Lua, name: &str, action: HookAction) -> mlua::Result<()> {
    pending(lua, name)?.actions.push(action);
    Ok(())
}

fn request(lua: &Lua, name: &'static str, spec: Value) -> mlua::Result<()> {
    let qualified = format!("amux.{name}");
    pending(lua, &qualified)?;
    if !spec.is_table() {
        return Err(mlua::Error::runtime(format!(
            "{qualified} takes a table, not a {}",
            spec.type_name()
        )));
    }
    let tagged = lua.create_table_from([(name, spec)])?;
    let action = from_lua::<HookAction>(Value::Table(tagged))
        .map_err(|invalid| mlua::Error::runtime(invalid.within("amux")))?;
    queue(lua, &qualified, action)
}

fn state(lua: &Lua, name: &str) -> mlua::Result<Arc<dyn HookState>> {
    let call = lua
        .app_data_ref::<HookCall>()
        .ok_or_else(|| mlua::Error::runtime(format!("{name} only works inside a hook")))?;
    call.state
        .as_ref()
        .and_then(Weak::upgrade)
        .ok_or_else(|| mlua::Error::runtime(format!("{name}: the server is not available")))
}

fn session_table(lua: &Lua, session: &SessionInfo) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("name", session.name.as_str())?;
    table.set("clients", session.attached_clients)?;
    let windows = session
        .windows
        .iter()
        .map(|window| window_table(lua, window))
        .collect::<mlua::Result<Vec<_>>>()?;
    table.set("windows", windows)?;
    if let Some(project) = &session.project {
        table.set("project", project.as_str())?;
    }
    if let Some(branch) = &session.branch {
        table.set("branch", branch.as_str())?;
    }
    Ok(table)
}

fn window_table(lua: &Lua, window: &WindowSummary) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("index", window.index)?;
    table.set("name", window.name.as_str())?;
    table.set("panes", window.panes)?;
    Ok(table)
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::lua::runtime::load_init;
    use crate::lua::Process;
    use crate::protocol::SessionId;

    struct Fake;

    impl HookState for Fake {
        fn server_name(&self) -> String {
            "desktop".into()
        }

        fn sessions(&self) -> Vec<SessionInfo> {
            let window = |index, name: &str, panes| WindowSummary {
                index,
                name: name.into(),
                panes,
            };
            vec![
                SessionInfo {
                    id: SessionId(1),
                    name: "notes".into(),
                    windows: vec![window(0, "sh", 1)],
                    attached_clients: 0,
                    last_activity: SystemTime::UNIX_EPOCH,
                    project: None,
                    branch: None,
                },
                SessionInfo {
                    id: SessionId(2),
                    name: "work".into(),
                    windows: vec![window(0, "sh", 2), window(1, "logs", 1)],
                    attached_clients: 1,
                    last_activity: SystemTime::UNIX_EPOCH,
                    project: Some("github.com/blendonl/amux".into()),
                    branch: Some("main".into()),
                },
            ]
        }
    }

    fn hooks(source: &str) -> LuaHooks {
        LuaHooks::new(load_init(source, Process::Server).unwrap())
    }

    fn started() -> HookEvent {
        HookEvent::ServerStarted {
            server: "desktop".into(),
        }
    }

    fn run_once(source: &str, state: Option<&Weak<dyn HookState>>) -> HookRun {
        let mut runs = hooks(source).run(&started(), state);
        assert_eq!(runs.len(), 1, "{runs:?}");
        runs.remove(0)
    }

    fn failure(body: &str) -> String {
        let source = format!("amux.on('server_started', function(ev)\n{body}\nend)");
        match run_once(&source, None).result {
            Ok(actions) => panic!("{body}: {actions:?}"),
            Err(error) => error,
        }
    }

    #[test]
    fn queries_read_a_snapshot_of_the_server() {
        let fake: Arc<dyn HookState> = Arc::new(Fake);
        let state = Arc::downgrade(&fake);
        let run = run_once(
            "amux.on('server_started', function(ev)\n\
               assert(amux.server_name() == 'desktop')\n\
               local sessions = amux.sessions()\n\
               assert(#sessions == 2 and sessions[1].name == 'notes' and sessions[1].clients == 0)\n\
               assert(sessions[1].project == nil and sessions[1].branch == nil)\n\
               local work = amux.session('work')\n\
               assert(work.clients == 1 and #work.windows == 2)\n\
               assert(work.project == 'github.com/blendonl/amux' and work.branch == 'main')\n\
               assert(work.windows[2].index == 1 and work.windows[2].name == 'logs')\n\
               assert(work.windows[1].panes == 2)\n\
               assert(amux.session('missing') == nil)\n\
               amux.kill_session(work.name)\n\
             end)",
            Some(&state),
        );
        assert_eq!(
            run.result,
            Ok(vec![HookAction::KillSession {
                session: "work".into()
            }])
        );
        assert!(run.hook.ends_with("init.lua:1"), "{}", run.hook);

        drop(fake);
        let run = run_once(
            "amux.on('server_started', function() amux.sessions() end)",
            Some(&state),
        );
        let error = run.result.unwrap_err();
        assert!(
            error.ends_with("init.lua:1: amux.sessions: the server is not available"),
            "{error}"
        );
    }

    #[test]
    fn requests_are_queued_in_order() {
        let run = run_once(
            "amux.on('server_started', function(ev)\n\
               amux.new_window { session = 'work' }\n\
               amux.rename_session('work', 'play')\n\
               amux.rename_window { session = 'play', window = 1, name = 'logs' }\n\
               amux.send_keys { session = 'play', keys = '\\255\\27[A' }\n\
               amux.send_keys { session = 'play', window = 1, pane = 0, keys = 'ls\\r' }\n\
               amux.kill_session('old')\n\
             end)",
            None,
        );
        assert_eq!(
            run.result,
            Ok(vec![
                HookAction::NewWindow {
                    session: "work".into()
                },
                HookAction::RenameSession {
                    session: "work".into(),
                    name: "play".into()
                },
                HookAction::RenameWindow {
                    session: "play".into(),
                    window: 1,
                    name: "logs".into()
                },
                HookAction::SendKeys {
                    session: "play".into(),
                    window: None,
                    pane: None,
                    keys: b"\xff\x1b[A".to_vec()
                },
                HookAction::SendKeys {
                    session: "play".into(),
                    window: Some(1),
                    pane: Some(0),
                    keys: b"ls\r".to_vec()
                },
                HookAction::KillSession {
                    session: "old".into()
                },
            ])
        );
    }

    #[test]
    fn a_failing_hook_drops_its_requests() {
        let error = failure("amux.kill_session('work')\nerror('boom')");
        assert!(error.ends_with("init.lua:3: boom"), "{error}");
    }

    #[test]
    fn misused_requests_are_reported_with_their_line() {
        for (body, expected) in [
            (
                "amux.new_window { sesion = 'work' }",
                "amux.new_window.sesion: unknown field `sesion`, expected `session`",
            ),
            (
                "amux.new_window('work')",
                "amux.new_window takes a table, not a string",
            ),
            (
                "amux.rename_window { session = 'work', name = 'logs' }",
                "amux.rename_window: missing field `window`",
            ),
            (
                "amux.send_keys { session = 'work', keys = 5 }",
                "amux.send_keys.keys: invalid type: integer `5`, expected a string of keys",
            ),
            (
                "amux.rename_session('work', 'a:b')",
                "the session name \"a:b\" must not contain ':'",
            ),
        ] {
            let error = failure(body);
            assert!(
                error.contains(&format!("init.lua:2: {expected}")),
                "{body}: {error}"
            );
        }
    }

    #[test]
    fn the_server_api_only_works_inside_hooks() {
        for (source, expected) in [
            (
                "amux.new_window { session = 'work' }",
                "init.lua:1: amux.new_window only works inside a hook",
            ),
            (
                "amux.kill_session('work')",
                "init.lua:1: amux.kill_session only works inside a hook",
            ),
            (
                "amux.sessions()",
                "init.lua:1: amux.sessions only works inside a hook",
            ),
        ] {
            let error = load_init(source, Process::Server).unwrap_err().to_string();
            assert!(error.contains(expected), "{source}: {error}");
        }
        load_init(
            "assert(amux.sessions == nil and amux.kill_session == nil and amux.new_window == nil)",
            Process::Client,
        )
        .unwrap();
    }
}
