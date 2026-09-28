use std::time::Duration;

use mlua::{AppDataRefMut, Function, IntoLua, IntoLuaMulti, Lua, MultiValue, Table, Value};
use serde::Deserialize;

use super::api::{bound, registry, Bound, Callbacks, Registry};
use super::runtime::Loaded;
use super::{call_within, describe, from_lua, function};
use crate::client::scripting::{ClientContext, Effect, Scripting, StatusContext, StatusSpan};
use crate::protocol::WindowSummary;
use crate::settings::{CallbackId, Keymap, StyleSpec};
use crate::target::Target;

const BINDING_BUDGET: Duration = Duration::from_secs(1);
const STATUS_BUDGET: Duration = Duration::from_millis(50);
const PROMPT_FIELDS: [&str; 3] = ["initial", "label", "on_submit"];

pub struct LuaScripting {
    lua: Lua,
    keymap: Keymap,
}

impl LuaScripting {
    pub fn new(loaded: Loaded) -> Self {
        let Loaded {
            keymap,
            callbacks,
            hooks,
            lua,
            ..
        } = loaded;
        lua.set_app_data(Registry {
            keymap: keymap.clone(),
            callbacks,
            hooks,
        });
        Self { lua, keymap }
    }

    fn callback(&self, id: CallbackId) -> Result<Function, String> {
        self.lua
            .app_data_ref::<Registry>()
            .and_then(|registry| registry.callbacks.get(id).cloned())
            .ok_or_else(|| format!("callback {} is not registered", id.0))
    }

    fn run<R>(
        &self,
        callback: &Function,
        arguments: impl IntoLuaMulti,
        call: Call,
        budget: Duration,
        what: &'static str,
    ) -> (Result<R, String>, Call)
    where
        R: mlua::FromLuaMulti,
    {
        self.lua.set_app_data(call);
        let result = call_within(&self.lua, callback, arguments, budget, what);
        let call = self.lua.remove_app_data::<Call>().unwrap_or_default();
        (result.map_err(|error| describe(&error)), call)
    }

    fn saved_callbacks(&self) -> Option<Callbacks> {
        self.lua
            .app_data_ref::<Registry>()
            .map(|registry| registry.callbacks.clone())
    }

    fn settle(&mut self, succeeded: bool, saved: Option<Callbacks>) -> Option<Keymap> {
        let mut registry = self.lua.app_data_mut::<Registry>()?;
        if !succeeded {
            if let Some(saved) = saved {
                registry.callbacks = saved;
            }
            registry.keymap = self.keymap.clone();
            return None;
        }
        if registry.keymap == self.keymap {
            return None;
        }
        self.keymap = registry.keymap.clone();
        Some(self.keymap.clone())
    }
}

impl Scripting for LuaScripting {
    fn call(
        &mut self,
        id: CallbackId,
        input: Option<&str>,
        context: &ClientContext,
    ) -> Result<Vec<Effect>, String> {
        let callback = self.callback(id)?;
        let snapshot = client_table(&self.lua, context)
            .and_then(|table| frozen(&self.lua, table))
            .map_err(|error| describe(&error))?;
        let arguments = match input {
            Some(text) => (text, snapshot.clone()).into_lua_multi(&self.lua),
            None => snapshot.clone().into_lua_multi(&self.lua),
        }
        .map_err(|error| describe(&error))?;
        let call = Call {
            snapshot: Some(snapshot),
            effects: Some(Vec::new()),
        };
        let saved = self.saved_callbacks();
        let (result, call) =
            self.run::<MultiValue>(&callback, arguments, call, BINDING_BUDGET, "a key binding");
        let keymap = self.settle(result.is_ok(), saved);
        result?;
        Ok(keymap
            .map(Effect::Keymap)
            .into_iter()
            .chain(call.effects.unwrap_or_default())
            .collect())
    }

    fn status(
        &mut self,
        id: CallbackId,
        context: &StatusContext,
    ) -> Result<Option<Vec<StatusSpan>>, String> {
        let callback = self.callback(id)?;
        let snapshot = client_table(&self.lua, &context.client)
            .and_then(|table| {
                table.set("width", context.width)?;
                frozen(&self.lua, table)
            })
            .map_err(|error| describe(&error))?;
        let call = Call {
            snapshot: Some(snapshot.clone()),
            effects: None,
        };
        let (result, _) = self.run::<Value>(
            &callback,
            snapshot,
            call,
            STATUS_BUDGET,
            "a status function",
        );
        status_spans(result?)
    }

    fn release(&mut self, id: CallbackId) {
        if let Some(mut registry) = self.lua.app_data_mut::<Registry>() {
            registry.callbacks.release(id);
        }
    }
}

#[derive(Default)]
struct Call {
    snapshot: Option<Table>,
    effects: Option<Vec<Effect>>,
}

pub(super) fn install(lua: &Lua, api: &Table, constructors: Table) -> mlua::Result<()> {
    api.set(
        "notify",
        function(lua, |lua, message: String| {
            queue(lua, "amux.notify", Effect::Notify(message))
        })?,
    )?;
    api.set(
        "send_keys",
        function(lua, |lua, keys: mlua::String| {
            queue(
                lua,
                "amux.send_keys",
                Effect::SendKeys(keys.as_bytes().to_vec()),
            )
        })?,
    )?;
    api.set(
        "run",
        function(lua, move |lua, action: Value| {
            match bound(lua, &constructors, action)? {
                Bound::Binding(binding) => queue(lua, "amux.run", Effect::Run(binding)),
                Bound::Callback(_) => Err(mlua::Error::runtime(
                    "amux.run takes an action, not a function",
                )),
            }
        })?,
    )?;
    api.set(
        "switch",
        function(lua, |lua, target: String| {
            let target: Target = target.parse().map_err(mlua::Error::runtime)?;
            queue(lua, "amux.switch", Effect::Switch(target))
        })?,
    )?;
    api.set("prompt", function(lua, prompt)?)?;
    api.set("state", function(lua, |lua, ()| state(lua))?)?;
    Ok(())
}

fn pending<'a>(lua: &'a Lua, name: &str) -> mlua::Result<AppDataRefMut<'a, Call>> {
    let call = lua
        .app_data_mut::<Call>()
        .ok_or_else(|| mlua::Error::runtime(format!("{name} only works inside a key binding")))?;
    if call.effects.is_none() {
        return Err(mlua::Error::runtime(format!(
            "{name} cannot be used in a status function"
        )));
    }
    Ok(call)
}

fn queue(lua: &Lua, name: &str, effect: Effect) -> mlua::Result<()> {
    if let Some(effects) = pending(lua, name)?.effects.as_mut() {
        effects.push(effect);
    }
    Ok(())
}

fn state(lua: &Lua) -> mlua::Result<Table> {
    lua.app_data_ref::<Call>()
        .and_then(|call| call.snapshot.clone())
        .ok_or_else(|| {
            mlua::Error::runtime("amux.state only works inside a key binding or a status function")
        })
}

fn prompt(lua: &Lua, spec: Table) -> mlua::Result<()> {
    for pair in spec.pairs::<Value, Value>() {
        let (key, _) = pair?;
        let known = key
            .as_string()
            .and_then(|key| key.to_str().ok())
            .is_some_and(|key| PROMPT_FIELDS.contains(&&*key));
        if !known {
            return Err(mlua::Error::runtime(format!(
                "unknown amux.prompt field {}, expected one of {}",
                key.to_string()?,
                PROMPT_FIELDS.join(", ")
            )));
        }
    }
    let label: Option<String> = spec.get("label")?;
    let initial: Option<String> = spec.get("initial")?;
    let on_submit = spec
        .get::<Option<Function>>("on_submit")?
        .ok_or_else(|| mlua::Error::runtime("amux.prompt needs an on_submit function"))?;
    pending(lua, "amux.prompt")?;
    let submit = registry(lua)?.callbacks.register(on_submit);
    queue(
        lua,
        "amux.prompt",
        Effect::Prompt {
            label: label.unwrap_or_default(),
            initial: initial.unwrap_or_default(),
            submit,
        },
    )
}

fn client_table(lua: &Lua, context: &ClientContext) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("session", context.session.as_str())?;
    table.set("server", context.server.as_str())?;
    table.set("local_server", context.local.as_str())?;
    if let Some(window) = context.active_window() {
        table.set("window_index", window.index)?;
        table.set("window_name", window.name.as_str())?;
        table.set("panes", window.panes)?;
    }
    let windows = context
        .windows
        .iter()
        .map(|window| window_table(lua, window, context.active))
        .collect::<mlua::Result<Vec<_>>>()?;
    table.set("windows", windows)?;
    if let Some(latency) = context.latency {
        table.set("latency_ms", latency.as_millis().into_lua(lua)?)?;
    }
    table.set("offline", context.offline.clone())?;
    Ok(table)
}

fn window_table(lua: &Lua, window: &WindowSummary, active: Option<usize>) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set("index", window.index)?;
    table.set("name", window.name.as_str())?;
    table.set("panes", window.panes)?;
    table.set("active", Some(window.index) == active)?;
    Ok(table)
}

fn frozen(lua: &Lua, table: Table) -> mlua::Result<Table> {
    let mut nested = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        if let (key, Value::Table(inner)) = pair? {
            nested.push((key, inner));
        }
    }
    for (key, inner) in nested {
        table.raw_set(key, frozen(lua, inner)?)?;
    }
    let meta = lua.create_table()?;
    meta.set("__index", &table)?;
    meta.set(
        "__newindex",
        function(
            lua,
            |_, (_, key, _): (Value, Value, Value)| -> mlua::Result<()> {
                Err(mlua::Error::runtime(format!(
                    "amux.state() is read-only, {} cannot be set",
                    key.to_string()?
                )))
            },
        )?,
    )?;
    let length = table.raw_len();
    meta.set("__len", function(lua, move |_, _: Value| Ok(length))?)?;
    let next: Function = lua.globals().get("next")?;
    meta.set(
        "__pairs",
        function(lua, move |_, _: Value| {
            Ok((next.clone(), table.clone(), Value::Nil))
        })?,
    )?;
    meta.set("__metatable", "amux.state")?;
    let proxy = lua.create_table()?;
    proxy.set_metatable(Some(meta))?;
    Ok(proxy)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpanSpec {
    text: String,
    #[serde(default)]
    style: StyleSpec,
}

fn status_spans(value: Value) -> Result<Option<Vec<StatusSpan>>, String> {
    match value {
        Value::Nil => Ok(None),
        Value::String(text) => Ok(Some(vec![StatusSpan {
            text: text.to_string_lossy(),
            style: StyleSpec::EMPTY,
        }])),
        Value::Table(_) => from_lua::<Vec<SpanSpec>>(value)
            .map(|spans| {
                Some(
                    spans
                        .into_iter()
                        .map(|span| StatusSpan {
                            text: span.text,
                            style: span.style,
                        })
                        .collect(),
                )
            })
            .map_err(|invalid| format!("invalid status spans: {invalid}")),
        other => Err(format!(
            "a status function returns a string or a list of spans, not a {}",
            other.type_name()
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::lua::runtime::load_init;
    use crate::lua::Process;
    use crate::settings::{Binding, Color};

    fn scripting(source: &str) -> (LuaScripting, Keymap) {
        let loaded = load_init(source, Process::Client).unwrap();
        let keymap = loaded.keymap.clone();
        (LuaScripting::new(loaded), keymap)
    }

    fn callback_for(keymap: &Keymap, notation: &str) -> CallbackId {
        match keymap.prefix.get(&notation.parse().unwrap()) {
            Some(Binding::Callback(id)) => *id,
            other => panic!("{notation} is bound to {other:?}"),
        }
    }

    fn context() -> ClientContext {
        ClientContext {
            session: "work".into(),
            server: "desktop".into(),
            local: "laptop".into(),
            windows: vec![WindowSummary {
                index: 0,
                name: "sh".into(),
                panes: 2,
            }],
            active: Some(0),
            latency: Some(Duration::from_millis(12)),
            offline: vec!["attic".into()],
        }
    }

    fn pressed(source: &str) -> Result<Vec<Effect>, String> {
        let (mut scripting, keymap) = scripting(source);
        scripting.call(callback_for(&keymap, "g"), None, &context())
    }

    fn status(source: &str) -> Result<Option<Vec<StatusSpan>>, String> {
        let loaded = load_init(source, Process::Client).unwrap();
        let id = loaded.settings.status.right.unwrap();
        let context = StatusContext {
            client: context(),
            width: 80,
            tick: None,
        };
        LuaScripting::new(loaded).status(id, &context)
    }

    #[test]
    fn a_binding_sees_the_client_context_and_queues_effects_in_order() {
        let effects = pressed(
            "amux.keymap.set('prefix', 'g', function(ctx)\n\
               assert(ctx.server == 'desktop' and ctx.local_server == 'laptop')\n\
               assert(ctx.latency_ms == 12 and ctx.offline[1] == 'attic')\n\
               assert(ctx.window_name == 'sh' and ctx.panes == 2 and ctx.width == nil)\n\
               amux.notify('hi')\n\
               amux.run('next_pane')\n\
               amux.run(amux.action.select_window)\n\
             end)",
        );
        let error = effects.unwrap_err();
        assert!(
            error.ends_with(
                "init.lua:7: invalid action: select_window: \
                 invalid type: unit variant, expected newtype variant"
            ),
            "{error}"
        );

        let effects = pressed(
            "amux.keymap.set('prefix', 'g', function(ctx)\n\
               amux.notify('hi')\n\
               amux.keymap.set('root', 'M-x', 'detach')\n\
               amux.run(amux.action.split_pane('top-bottom'))\n\
               amux.send_keys('\\27[A')\n\
               amux.switch('notes@laptop')\n\
             end)",
        )
        .unwrap();
        let mut keymap = Keymap::default();
        keymap.root.insert("M-x".parse().unwrap(), Binding::Detach);
        keymap
            .prefix
            .insert("g".parse().unwrap(), Binding::Callback(CallbackId(0)));
        assert_eq!(
            effects,
            vec![
                Effect::Keymap(keymap),
                Effect::Notify("hi".into()),
                Effect::Run(Binding::SplitPane(crate::protocol::Split::TopBottom)),
                Effect::SendKeys(b"\x1b[A".to_vec()),
                Effect::Switch("notes@laptop".parse().unwrap()),
            ]
        );
    }

    #[test]
    fn a_prompt_registers_its_submit_callback_until_it_is_released() {
        let (mut scripting, keymap) = scripting(
            "amux.keymap.set('prefix', 'g', function(ctx)\n\
               amux.prompt { label = 'find', on_submit = function(text, ctx)\n\
                 amux.notify(text .. ' in ' .. ctx.session)\n\
               end }\n\
             end)",
        );
        let effects = scripting
            .call(callback_for(&keymap, "g"), None, &context())
            .unwrap();
        let [Effect::Prompt {
            label,
            initial,
            submit,
        }] = effects.as_slice()
        else {
            panic!("{effects:?}");
        };
        assert_eq!((label.as_str(), initial.as_str()), ("find", ""));
        assert_eq!(registered(&scripting), 2);
        assert_eq!(
            scripting.call(*submit, Some("todo"), &context()),
            Ok(vec![Effect::Notify("todo in work".into())])
        );

        scripting.release(*submit);
        assert_eq!(registered(&scripting), 1);
        assert_eq!(
            scripting.call(*submit, Some("todo"), &context()),
            Err(format!("callback {} is not registered", submit.0))
        );
        let effects = scripting
            .call(callback_for(&keymap, "g"), None, &context())
            .unwrap();
        let [Effect::Prompt { submit: next, .. }] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_ne!(next, submit);
        assert_eq!(registered(&scripting), 2);
    }

    #[test]
    fn a_failed_binding_keeps_the_callbacks_it_replaced_and_frees_the_ones_it_made() {
        let (mut scripting, keymap) = scripting(
            "amux.keymap.set('prefix', 'h', function() amux.notify('old h') end)\n\
             amux.keymap.set('prefix', 'g', function()\n\
               amux.keymap.set('prefix', 'h', function() end)\n\
               amux.prompt { on_submit = function() end }\n\
               error('boom')\n\
             end)\n\
             amux.keymap.set('prefix', 'n', function()\n\
               amux.keymap.set('prefix', 'h', function() amux.notify('new h') end)\n\
             end)",
        );
        assert_eq!(registered(&scripting), 3);
        let error = scripting
            .call(callback_for(&keymap, "g"), None, &context())
            .unwrap_err();
        assert!(error.ends_with("init.lua:5: boom"), "{error}");
        assert_eq!(registered(&scripting), 3);
        assert_eq!(
            scripting.call(callback_for(&keymap, "h"), None, &context()),
            Ok(vec![Effect::Notify("old h".into())])
        );

        let effects = scripting
            .call(callback_for(&keymap, "n"), None, &context())
            .unwrap();
        let [Effect::Keymap(changed)] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(registered(&scripting), 3);
        assert_eq!(
            scripting.call(callback_for(changed, "h"), None, &context()),
            Ok(vec![Effect::Notify("new h".into())])
        );
        assert_eq!(
            scripting.call(callback_for(&keymap, "h"), None, &context()),
            Err(format!(
                "callback {} is not registered",
                callback_for(&keymap, "h").0
            ))
        );
    }

    fn registered(scripting: &LuaScripting) -> usize {
        scripting
            .lua
            .app_data_ref::<Registry>()
            .map_or(0, |registry| registry.callbacks.len())
    }

    #[test]
    fn misused_client_calls_are_reported_with_their_line() {
        for (body, expected) in [
            (
                "amux.run(function() end)",
                "amux.run takes an action, not a function",
            ),
            ("amux.run('zoom')", "invalid action: unknown variant `zoom`"),
            (
                "amux.switch('work@')",
                "invalid target \"work@\": empty server name after '@'",
            ),
            (
                "amux.prompt { label = 'x' }",
                "amux.prompt needs an on_submit function",
            ),
            (
                "amux.prompt { lable = 'x', on_submit = print }",
                "unknown amux.prompt field lable, expected one of initial, label, on_submit",
            ),
            (
                "amux.state().session = 'x'",
                "amux.state() is read-only, session cannot be set",
            ),
        ] {
            let source = format!("amux.keymap.set('prefix', 'g', function()\n{body}\nend)");
            let error = pressed(&source).unwrap_err();
            assert!(
                error.contains(&format!("init.lua:2: {expected}")),
                "{body}: {error}"
            );
        }
    }

    #[test]
    fn the_client_api_only_works_inside_callbacks() {
        for (source, expected) in [
            (
                "amux.notify('x')",
                "init.lua:1: amux.notify only works inside a key binding",
            ),
            (
                "amux.state()",
                "init.lua:1: amux.state only works inside a key binding or a status function",
            ),
        ] {
            let error = load_init(source, Process::Client).unwrap_err().to_string();
            assert!(error.contains(expected), "{error}");
        }
        load_init(
            "assert(amux.notify == nil and amux.prompt == nil)",
            Process::Server,
        )
        .unwrap();

        let error =
            status("amux.opt.status.right = function(ctx) amux.send_keys('x') end").unwrap_err();
        assert!(
            error.ends_with("init.lua:1: amux.send_keys cannot be used in a status function"),
            "{error}"
        );
    }

    #[test]
    fn the_budget_also_stops_a_runaway_coroutine() {
        let started = Instant::now();
        let error = status(
            "amux.opt.status.right = function()\n\
               coroutine.wrap(function() while true do end end)()\n\
             end",
        )
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            error.contains("a status function ran past its 50 ms budget"),
            "{error}"
        );
    }

    #[test]
    fn status_functions_return_text_or_styled_spans() {
        assert_eq!(
            status("amux.opt.status.right = function(ctx) return amux.state().width .. '' end"),
            Ok(Some(vec![StatusSpan {
                text: "80".into(),
                style: StyleSpec::EMPTY,
            }]))
        );
        assert_eq!(
            status(
                "amux.opt.status.right = function(ctx)\n\
                   return { { text = ctx.session, style = { bg = '#102030', italic = true } }, { text = '!' } }\n\
                 end"
            ),
            Ok(Some(vec![
                StatusSpan {
                    text: "work".into(),
                    style: StyleSpec {
                        bg: Some(Color::Rgb(0x10, 0x20, 0x30)),
                        italic: Some(true),
                        ..StyleSpec::EMPTY
                    },
                },
                StatusSpan {
                    text: "!".into(),
                    style: StyleSpec::EMPTY,
                },
            ]))
        );
        assert_eq!(status("amux.opt.status.right = function() end"), Ok(None));

        let error = status(
            "amux.opt.status.right = function() return { { text = 'a', colour = 'red' } } end",
        )
        .unwrap_err();
        assert!(error.starts_with("invalid status spans: "), "{error}");
        assert!(error.contains("unknown field `colour`"), "{error}");
        let error = status("amux.opt.status.right = function() return true end").unwrap_err();
        assert_eq!(
            error,
            "a status function returns a string or a list of spans, not a boolean"
        );
    }
}
