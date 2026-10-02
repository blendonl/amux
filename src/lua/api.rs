use std::cell::Cell;
use std::collections::BTreeMap;

use mlua::{AppDataRefMut, Function, IntoLua, Lua, LuaSerdeExt, Table, Value, Variadic};
use serde::de::value::Error;
use serde::de::{self, DeserializeOwned, Deserializer, IntoDeserializer, Visitor};
use serde::{Deserialize, Serialize};
use tracing::info;

use super::runtime::Process;
use super::{client, from_lua, function, server};
use crate::keys::{parse_sequence, spell_sequence, Key};
use crate::settings::{
    Binding, CallbackId, CopyAction, Keymap, PickerAction, PromptAction, Table as Bindings,
    TreeAction, COPY_TABLE, PICKER_TABLE, PREFIX_TABLE, PROMPT_TABLE, ROOT_TABLE, TREE_TABLE,
};

const SET_OPTIONS: [&str; 1] = ["desc"];

pub const EVENTS: [&str; 11] = [
    "session_created",
    "session_closed",
    "session_renamed",
    "window_created",
    "window_closed",
    "pane_exited",
    "client_attached",
    "client_detached",
    "peer_online",
    "peer_offline",
    "server_started",
];

#[derive(Debug, Default, Clone)]
pub struct Callbacks {
    functions: BTreeMap<usize, Function>,
    next: usize,
}

impl Callbacks {
    pub fn get(&self, id: CallbackId) -> Option<&Function> {
        self.functions.get(&id.0)
    }

    pub fn len(&self) -> usize {
        self.functions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    pub(super) fn register(&mut self, callback: Function) -> CallbackId {
        let id = CallbackId(self.next);
        self.functions.insert(id.0, callback);
        self.next += 1;
        id
    }

    pub(super) fn release(&mut self, id: CallbackId) {
        self.functions.remove(&id.0);
    }
}

#[derive(Debug, Default)]
pub struct Hooks(BTreeMap<String, Vec<Function>>);

impl Hooks {
    pub fn get(&self, event: &str) -> &[Function] {
        self.0.get(event).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn events(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

#[derive(Debug, Default)]
pub struct Registry {
    pub keymap: Keymap,
    pub callbacks: Callbacks,
    pub hooks: Hooks,
}

pub fn install(lua: &Lua, opt: &Table, process: Process) -> mlua::Result<()> {
    lua.set_app_data(Registry::default());
    let (action, constructors) = actions(lua)?;
    let api = lua.create_table()?;
    api.set("opt", opt)?;
    api.set("keymap", keymap(lua, constructors.clone())?)?;
    api.set("action", guarded(lua, "amux.action", action)?)?;
    api.set("on", function(lua, on)?)?;
    api.set("process", process.name())?;
    match process {
        Process::Client => client::install(lua, &api, constructors)?,
        Process::Server => server::install(lua, &api)?,
    }
    api.set("hostname", function(lua, |_, ()| hostname())?)?;
    api.set("log", function(lua, log)?)?;
    lua.globals().set("amux", guarded(lua, "amux", api)?)
}

fn guarded(lua: &Lua, name: &'static str, api: Table) -> mlua::Result<Table> {
    let meta = lua.create_table()?;
    meta.set("__index", &api)?;
    meta.set("__newindex", {
        let api = api.clone();
        function(lua, move |_, (table, key, value): (Table, Value, Value)| {
            if !api.raw_get::<Value>(&key)?.is_nil() {
                return Err(mlua::Error::runtime(format!(
                    "{name}.{} cannot be replaced",
                    key.to_string()?
                )));
            }
            table.raw_set(key, value)
        })?
    })?;
    let next: Function = lua.globals().get("next")?;
    meta.set(
        "__pairs",
        function(lua, move |_, _: Table| {
            Ok((next.clone(), api.clone(), Value::Nil))
        })?,
    )?;
    meta.set("__metatable", name)?;
    let guarded = lua.create_table()?;
    guarded.set_metatable(Some(meta))?;
    Ok(guarded)
}

pub(super) fn registry(lua: &Lua) -> mlua::Result<AppDataRefMut<'_, Registry>> {
    lua.app_data_mut::<Registry>()
        .ok_or_else(|| mlua::Error::runtime("the amux configuration is closed"))
}

pub(super) enum Bound {
    Binding(Binding),
    Callback(Function),
}

fn keymap(lua: &Lua, constructors: Table) -> mlua::Result<Table> {
    let keymap = lua.create_table()?;
    keymap.set(
        "set",
        function(
            lua,
            move |lua, (table, keys, value, options): (String, String, Value, Option<Table>)| {
                let keys = parse_keys(&keys)?;
                let description = description(options)?;
                match table.as_str() {
                    PROMPT_TABLE => {
                        let key = single_key(&table, &keys)?;
                        let action = panel_action::<PromptAction>(&table, value)?;
                        bind(&mut registry(lua)?.keymap.prompt, key, action, description);
                    }
                    TREE_TABLE => {
                        let key = single_key(&table, &keys)?;
                        let action = panel_action::<TreeAction>(&table, value)?;
                        bind(&mut registry(lua)?.keymap.tree, key, action, description);
                    }
                    PICKER_TABLE => {
                        let key = single_key(&table, &keys)?;
                        let action = panel_action::<PickerAction>(&table, value)?;
                        bind(&mut registry(lua)?.keymap.picker, key, action, description);
                    }
                    COPY_TABLE => {
                        let key = single_key(&table, &keys)?;
                        let action = panel_action::<CopyAction>(&table, value)?;
                        bind(&mut registry(lua)?.keymap.copy, key, action, description);
                    }
                    "" => return Err(mlua::Error::runtime("a keymap table needs a name")),
                    name => {
                        let bound = bound(lua, &constructors, value)?;
                        let mut registry = registry(lua)?;
                        let Registry {
                            keymap, callbacks, ..
                        } = &mut *registry;
                        let (key, path) = split_sequence(&keys);
                        let submap = keymap
                            .open_submap(name, path)
                            .map_err(mlua::Error::runtime)?;
                        let binding = match bound {
                            Bound::Binding(binding) => binding,
                            Bound::Callback(callback) => {
                                Binding::Callback(callbacks.register(callback))
                            }
                        };
                        let bindings = keymap.table_entry(&submap);
                        unbound(callbacks, bind(bindings, key, binding, description));
                    }
                }
                Ok(())
            },
        )?,
    )?;
    keymap.set(
        "del",
        function(lua, |lua, (table, keys): (String, String)| {
            let keys = parse_keys(&keys)?;
            let mut registry = registry(lua)?;
            let Registry {
                keymap, callbacks, ..
            } = &mut *registry;
            let removed = match table.as_str() {
                PROMPT_TABLE => keymap.prompt.remove(&single_key(&table, &keys)?).is_some(),
                TREE_TABLE => keymap.tree.remove(&single_key(&table, &keys)?).is_some(),
                PICKER_TABLE => keymap.picker.remove(&single_key(&table, &keys)?).is_some(),
                COPY_TABLE => keymap.copy.remove(&single_key(&table, &keys)?).is_some(),
                name => {
                    let (key, path) = split_sequence(&keys);
                    let removed = keymap
                        .submap(name, path)
                        .map(str::to_owned)
                        .and_then(|submap| keymap.table_mut(&submap)?.remove(&key));
                    unbound(callbacks, removed)
                }
            };
            if !removed {
                return Err(mlua::Error::runtime(format!(
                    "{} is not bound in the {table} table",
                    spell_sequence(&keys)
                )));
            }
            Ok(())
        })?,
    )?;
    keymap.set(
        "get",
        function(lua, |lua, (table, keys): (String, String)| {
            let keys = parse_keys(&keys)?;
            let registry = registry(lua)?;
            let keymap = &registry.keymap;
            match table.as_str() {
                PROMPT_TABLE => to_lua(lua, keymap.prompt.get(&single_key(&table, &keys)?)),
                TREE_TABLE => to_lua(lua, keymap.tree.get(&single_key(&table, &keys)?)),
                PICKER_TABLE => to_lua(lua, keymap.picker.get(&single_key(&table, &keys)?)),
                COPY_TABLE => to_lua(lua, keymap.copy.get(&single_key(&table, &keys)?)),
                name => {
                    let (key, path) = split_sequence(&keys);
                    let bound = keymap
                        .submap(name, path)
                        .and_then(|submap| keymap.table(submap)?.get(&key));
                    match bound {
                        Some(Binding::Callback(id)) => registry.callbacks.get(*id).into_lua(lua),
                        binding => to_lua(lua, binding),
                    }
                }
            }
        })?,
    )?;
    keymap.set(
        "clear",
        function(lua, |lua, table: String| {
            let mut registry = registry(lua)?;
            let Registry {
                keymap, callbacks, ..
            } = &mut *registry;
            match table.as_str() {
                PROMPT_TABLE => keymap.prompt.clear(),
                TREE_TABLE => keymap.tree.clear(),
                PICKER_TABLE => keymap.picker.clear(),
                COPY_TABLE => keymap.copy.clear(),
                ROOT_TABLE => clear_bindings(callbacks, &mut keymap.root),
                PREFIX_TABLE => clear_bindings(callbacks, &mut keymap.prefix),
                name => {
                    if let Some(mut bindings) = keymap.custom.remove(name) {
                        clear_bindings(callbacks, &mut bindings);
                    }
                }
            }
            Ok(())
        })?,
    )?;
    Ok(keymap)
}

fn description(options: Option<Table>) -> mlua::Result<Option<String>> {
    let Some(options) = options else {
        return Ok(None);
    };
    for pair in options.pairs::<Value, Value>() {
        let (option, _) = pair?;
        let known = option
            .as_string()
            .and_then(|option| option.to_str().ok())
            .is_some_and(|option| SET_OPTIONS.contains(&&*option));
        if !known {
            return Err(mlua::Error::runtime(format!(
                "unknown amux.keymap.set option {}, expected {}",
                option.to_string()?,
                SET_OPTIONS.join(", ")
            )));
        }
    }
    options.get("desc")
}

fn bind<A>(
    bindings: &mut Bindings<A>,
    key: Key,
    action: A,
    description: Option<String>,
) -> Option<A> {
    let replaced = bindings.insert(key, action);
    if let Some(description) = description {
        bindings.describe(key, description);
    }
    replaced
}

fn unbound(callbacks: &mut Callbacks, removed: Option<Binding>) -> bool {
    if let Some(Binding::Callback(id)) = removed {
        callbacks.release(id);
    }
    removed.is_some()
}

fn clear_bindings(callbacks: &mut Callbacks, bindings: &mut Bindings<Binding>) {
    for (_, binding) in bindings.iter() {
        if let Binding::Callback(id) = binding {
            callbacks.release(*id);
        }
    }
    bindings.clear();
}

fn parse_keys(notation: &str) -> mlua::Result<Vec<Key>> {
    parse_sequence(notation).map_err(mlua::Error::runtime)
}

fn single_key(table: &str, keys: &[Key]) -> mlua::Result<Key> {
    match keys {
        [key] => Ok(*key),
        _ => Err(mlua::Error::runtime(format!(
            "the {table} table binds single keys, not the sequence {}",
            spell_sequence(keys)
        ))),
    }
}

fn split_sequence(keys: &[Key]) -> (Key, &[Key]) {
    let (last, path) = keys
        .split_last()
        .expect("a parsed key sequence holds at least one key");
    (*last, path)
}

fn to_lua<T: Serialize>(lua: &Lua, value: Option<&T>) -> mlua::Result<Value> {
    value.map_or(Ok(Value::Nil), |value| lua.to_value(value))
}

fn panel_action<A: DeserializeOwned>(table: &str, value: Value) -> mlua::Result<A> {
    match value {
        Value::String(_) => from_lua(value)
            .map_err(|invalid| mlua::Error::runtime(format!("invalid {table} action: {invalid}"))),
        other => Err(mlua::Error::runtime(format!(
            "the {table} table binds action names, not a {}",
            other.type_name()
        ))),
    }
}

pub(super) fn bound(lua: &Lua, constructors: &Table, value: Value) -> mlua::Result<Bound> {
    match value {
        Value::Function(callback) => match constructors.raw_get::<Option<String>>(&callback)? {
            Some(name) => parse_binding(name.into_lua(lua)?).map(Bound::Binding),
            None => Ok(Bound::Callback(callback)),
        },
        Value::String(_) | Value::Table(_) => parse_binding(value).map(Bound::Binding),
        other => Err(mlua::Error::runtime(format!(
            "a binding is an action or a function, not a {}",
            other.type_name()
        ))),
    }
}

fn parse_binding(value: Value) -> mlua::Result<Binding> {
    from_lua(value).map_err(|invalid| mlua::Error::runtime(format!("invalid action: {invalid}")))
}

fn actions(lua: &Lua) -> mlua::Result<(Table, Table)> {
    let names = variants::<Binding>();
    let action = lua.create_table()?;
    let constructors = lua.create_table()?;
    for &name in names {
        let takes_argument =
            Binding::deserialize(IntoDeserializer::<Error>::into_deserializer(name)).is_err();
        let constructor = function(lua, move |lua, argument: Value| {
            let value = match (takes_argument, argument) {
                (false, Value::Nil) => name.into_lua(lua)?,
                (true, Value::Nil) => {
                    return Err(mlua::Error::runtime(format!(
                        "amux.action.{name} needs an argument"
                    )))
                }
                (false, _) => {
                    return Err(mlua::Error::runtime(format!(
                        "amux.action.{name} takes no argument"
                    )))
                }
                (true, argument) => Value::Table(lua.create_table_from([(name, argument)])?),
            };
            lua.to_value(&parse_binding(value)?)
        })?;
        constructors.raw_set(&constructor, name)?;
        action.raw_set(name, constructor)?;
    }
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        function(
            lua,
            move |_, (_, name): (Table, String)| -> mlua::Result<()> {
                Err(mlua::Error::runtime(format!(
                    "unknown action {name}, expected one of {}",
                    names.join(", ")
                )))
            },
        )?,
    )?;
    meta.set("__metatable", "amux.action")?;
    action.set_metatable(Some(meta))?;
    Ok((action, constructors))
}

fn on(lua: &Lua, (event, handler): (String, Function)) -> mlua::Result<()> {
    if !EVENTS.contains(&event.as_str()) {
        return Err(mlua::Error::runtime(format!(
            "unknown event {event}, expected one of {}",
            EVENTS.join(", ")
        )));
    }
    registry(lua)?
        .hooks
        .0
        .entry(event)
        .or_default()
        .push(handler);
    Ok(())
}

fn hostname() -> mlua::Result<String> {
    let name = nix::unistd::gethostname()
        .map_err(|err| mlua::Error::runtime(format!("reading the hostname: {err}")))?;
    Ok(name.to_string_lossy().into_owned())
}

fn log(_: &Lua, values: Variadic<Value>) -> mlua::Result<()> {
    let words = values
        .iter()
        .map(Value::to_string)
        .collect::<mlua::Result<Vec<_>>>()?;
    info!("{}", words.join("\t"));
    Ok(())
}

fn variants<T: DeserializeOwned>() -> &'static [&'static str] {
    let found = Cell::new(&[][..]);
    T::deserialize(VariantProbe(&found)).ok();
    found.get()
}

struct VariantProbe<'a>(&'a Cell<&'static [&'static str]>);

impl<'de> Deserializer<'de> for VariantProbe<'_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, _: V) -> Result<V::Value, Error> {
        Err(de::Error::custom("expected an enum"))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        _: V,
    ) -> Result<V::Value, Error> {
        self.0.set(variants);
        Err(de::Error::custom("only the variants are read"))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf
        option unit unit_struct newtype_struct seq tuple tuple_struct map struct identifier
        ignored_any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua::runtime::load_init;
    use crate::lua::{Loaded, Process};
    use crate::protocol::{Direction, Split};

    fn loaded(source: &str) -> Loaded {
        load_init(source, Process::Client).unwrap()
    }

    fn failure(source: &str) -> String {
        load_init(source, Process::Client).unwrap_err().to_string()
    }

    fn key(notation: &str) -> Key {
        notation.parse().unwrap()
    }

    #[test]
    fn keymap_set_accepts_constructors_names_and_tables() {
        let keymap = loaded(
            "amux.keymap.set('prefix', '|', amux.action.split_pane('left-right'))\n\
             amux.keymap.set('root', 'M-h', amux.action.select_pane('left'))\n\
             amux.keymap.set('prefix', 'D', 'detach')\n\
             amux.keymap.set('prefix', 'N', { select_window = 3 })\n\
             amux.keymap.set('prefix', 'C', amux.action.new_window)\n\
             amux.keymap.set('prefix', 'r', amux.action.switch_table('resize'))\n\
             amux.keymap.set('resize', 'h', amux.action.select_pane('left'))\n\
             amux.keymap.set('prompt', 'C-w', 'delete_line')\n\
             amux.keymap.set('tree', 'x', 'cancel')\n\
             amux.keymap.set('picker', 'C-j', 'down')\n\
             amux.keymap.del('picker', 'C-u')\n\
             assert(amux.keymap.get('picker', 'Enter') == 'pick')\n\
             amux.keymap.set('copy', 'C-k', 'halfpage_up')\n\
             amux.keymap.del('copy', 'q')\n\
             assert(amux.keymap.get('copy', 'g') == 'history_top')",
        )
        .keymap;
        assert_eq!(keymap.picker.get(&key("C-j")), Some(&PickerAction::Down));
        assert_eq!(keymap.picker.get(&key("C-u")), None);
        assert_eq!(keymap.copy.get(&key("C-k")), Some(&CopyAction::HalfpageUp));
        assert_eq!(keymap.copy.get(&key("q")), None);
        for (notation, binding) in [
            ("|", Binding::SplitPane(Split::LeftRight)),
            ("D", Binding::Detach),
            ("N", Binding::SelectWindow(3)),
            ("C", Binding::NewWindow),
            ("r", Binding::SwitchTable("resize".into())),
            ("d", Binding::Detach),
        ] {
            assert_eq!(
                keymap.prefix.get(&key(notation)),
                Some(&binding),
                "{notation}"
            );
        }
        assert_eq!(
            keymap.root.get(&key("M-h")),
            Some(&Binding::SelectPane(Direction::Left))
        );
        assert_eq!(
            keymap
                .table("resize")
                .and_then(|table| table.get(&key("h"))),
            Some(&Binding::SelectPane(Direction::Left))
        );
        assert_eq!(
            keymap.prompt.get(&key("C-w")),
            Some(&PromptAction::DeleteLine)
        );
        assert_eq!(keymap.tree.get(&key("x")), Some(&TreeAction::Cancel));
    }

    #[test]
    fn keymap_get_del_and_clear() {
        let keymap = loaded(
            "local keymap = amux.keymap\n\
             assert(keymap.get('prefix', 'd') == 'detach')\n\
             assert(keymap.get('prefix', '%').split_pane == 'left-right')\n\
             assert(keymap.get('prefix', '3').select_window == 3)\n\
             assert(keymap.get('prompt', 'Enter') == 'submit')\n\
             assert(keymap.get('tree', 'q') == 'cancel')\n\
             assert(keymap.get('root', 'M-x') == nil)\n\
             assert(keymap.get('nowhere', 'x') == nil)\n\
             keymap.del('prefix', '&')\n\
             assert(keymap.get('prefix', '&') == nil)\n\
             keymap.del('prompt', 'C-u')\n\
             keymap.set('resize', 'h', 'next_pane')\n\
             keymap.del('resize', 'h')\n\
             keymap.set('gone', 'x', 'detach')\n\
             keymap.clear('gone')\n\
             keymap.clear('tree')\n\
             keymap.set('tree', 'j', 'down')\n\
             assert(keymap.get('copy', 'Escape') == 'cancel')\n\
             keymap.clear('copy')\n\
             assert(keymap.get('copy', 'q') == nil)",
        )
        .keymap;
        assert_eq!(keymap.prefix.get(&key("&")), None);
        assert_eq!(keymap.prefix.iter().count(), 28);
        assert_eq!(keymap.copy.iter().count(), 0);
        assert_eq!(keymap.prompt.get(&key("C-u")), None);
        assert_eq!(keymap.custom["resize"].iter().count(), 0);
        assert!(!keymap.custom.contains_key("gone"));
        assert_eq!(
            keymap.tree.iter().collect::<Vec<_>>(),
            [(&key("j"), &TreeAction::Down)]
        );
    }

    #[test]
    fn keymap_set_takes_a_description() {
        let keymap = loaded(
            "amux.keymap.set('prefix', 'g', function() end, { desc = 'show the diff' })\n\
             amux.keymap.set('prefix', 'r', amux.action.switch_table('resize'), { desc = 'resize' })\n\
             amux.keymap.set('resize', 'h', 'next_pane', {})\n\
             amux.keymap.set('prompt', 'C-w', 'delete_line', { desc = 'wipe' })\n\
             amux.keymap.set('prefix', 'c', 'new_window', { desc = 'shell' })\n\
             amux.keymap.set('prefix', 'c', 'new_window')",
        )
        .keymap;
        assert_eq!(keymap.prefix.description(&key("g")), Some("show the diff"));
        assert_eq!(keymap.prefix.description(&key("r")), Some("resize"));
        assert_eq!(keymap.prefix.description(&key("c")), None);
        assert_eq!(keymap.custom["resize"].description(&key("h")), None);
        assert_eq!(keymap.prompt.description(&key("C-w")), Some("wipe"));

        let error = failure("amux.keymap.set('prefix', 'g', 'detach', { descr = 'x' })");
        assert!(
            error.contains("init.lua:1: unknown amux.keymap.set option descr, expected desc"),
            "{error}"
        );
        let error = failure("amux.keymap.set('prefix', 'g', 'detach', { desc = {} })");
        assert!(error.contains("init.lua:1:"), "{error}");
    }

    #[test]
    fn a_key_sequence_binds_through_submaps() {
        let loaded = loaded(
            "amux.keymap.set('prefix', 'g', amux.action.switch_table('git'), { desc = 'git' })\n\
             amux.keymap.set('prefix', 'g s', 'new_window', { desc = 'status' })\n\
             amux.keymap.set('prefix', 'g l L', function() return 'log' end)\n\
             amux.keymap.set('root', 'M-s f', 'detach')\n\
             assert(amux.keymap.get('prefix', 'g s') == 'new_window')\n\
             assert(amux.keymap.get('git', 's') == 'new_window')\n\
             assert(amux.keymap.get('prefix', 'g l').switch_table == 'git l')\n\
             assert(amux.keymap.get('prefix', 'g l L')() == 'log')\n\
             assert(amux.keymap.get('prefix', 'g z') == nil)\n\
             assert(amux.keymap.get('prefix', 'd x') == nil)\n\
             amux.keymap.set('prefix', 'g d', 'detach')\n\
             amux.keymap.del('prefix', 'g d')",
        );
        let keymap = &loaded.keymap;
        assert_eq!(
            keymap.prefix.get(&key("g")),
            Some(&Binding::SwitchTable("git".into()))
        );
        assert_eq!(keymap.prefix.description(&key("g")), Some("git"));
        assert_eq!(
            keymap.custom["git"].get(&key("s")),
            Some(&Binding::NewWindow)
        );
        assert_eq!(keymap.custom["git"].description(&key("s")), Some("status"));
        assert_eq!(keymap.custom["git"].get(&key("d")), None);
        assert!(matches!(
            keymap.custom["git l"].get(&key("L")),
            Some(Binding::Callback(_))
        ));
        assert_eq!(
            keymap.root.get(&key("M-s")),
            Some(&Binding::SwitchTable("root M-s".into()))
        );
        assert_eq!(
            keymap.custom["root M-s"].get(&key("f")),
            Some(&Binding::Detach)
        );
        assert_eq!(loaded.callbacks.len(), 1);
    }

    #[test]
    fn a_key_sequence_needs_submaps_and_a_table_that_takes_one() {
        for (source, expected) in [
            (
                "amux.keymap.set('prefix', 'd x', 'detach')",
                "init.lua:1: d in the prefix table is bound to detach, not to a submap",
            ),
            (
                "amux.keymap.set('prompt', 'C-x C-w', 'delete_line')",
                "init.lua:1: the prompt table binds single keys, not the sequence C-x C-w",
            ),
            (
                "amux.keymap.get('tree', 'g g')",
                "init.lua:1: the tree table binds single keys, not the sequence g g",
            ),
            (
                "amux.keymap.set('copy', 'g g', 'history_top')",
                "init.lua:1: the copy table binds single keys, not the sequence g g",
            ),
            (
                "amux.keymap.del('prefix', 'z z')",
                "init.lua:1: z z is not bound in the prefix table",
            ),
            (
                "amux.keymap.set('prefix', 'g Bogus', 'detach')",
                "init.lua:1: invalid key \"Bogus\"",
            ),
        ] {
            let error = failure(source);
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn a_function_binding_becomes_a_callback() {
        let loaded = loaded(
            "local function greet() return 'hi' end\n\
             amux.keymap.set('prefix', 'g', greet)\n\
             amux.keymap.set('root', 'M-g', function() return 'root' end)\n\
             assert(amux.keymap.get('prefix', 'g') == greet)",
        );
        assert_eq!(loaded.callbacks.len(), 2);
        let Some(Binding::Callback(id)) = loaded.keymap.prefix.get(&key("g")).cloned() else {
            panic!("g is not a callback");
        };
        let greeting: String = loaded.callbacks.get(id).unwrap().call(()).unwrap();
        assert_eq!(greeting, "hi");
        let Some(Binding::Callback(root)) = loaded.keymap.root.get(&key("M-g")).cloned() else {
            panic!("M-g is not a callback");
        };
        assert_ne!(root, id);
        let answer: String = loaded.callbacks.get(root).unwrap().call(()).unwrap();
        assert_eq!(answer, "root");
    }

    #[test]
    fn replaced_and_deleted_callbacks_are_freed() {
        let loaded = loaded(
            "for i = 1, 10 do amux.keymap.set('prefix', 'g', function() return i end) end\n\
             amux.keymap.set('root', 'M-x', function() end)\n\
             amux.keymap.del('root', 'M-x')\n\
             amux.keymap.set('resize', 'h', function() end)\n\
             amux.keymap.set('resize', 'l', function() end)\n\
             amux.keymap.clear('resize')\n\
             amux.keymap.set('root', 'M-j', function() end)\n\
             amux.keymap.clear('root')\n\
             amux.keymap.set('prefix', 'j', function() end)\n\
             amux.keymap.set('prefix', 'j', 'detach')",
        );
        assert_eq!(loaded.callbacks.len(), 1);
        let Some(Binding::Callback(id)) = loaded.keymap.prefix.get(&key("g")).cloned() else {
            panic!("g is not a callback");
        };
        let last: i64 = loaded.callbacks.get(id).unwrap().call(()).unwrap();
        assert_eq!(last, 10);
    }

    #[test]
    fn an_invalid_action_lists_the_valid_ones() {
        let error = failure("\namux.keymap.set('prefix', 'z', 'zoom')");
        assert!(
            error.contains(
                "init.lua:2: invalid action: unknown variant `zoom`, \
                 expected one of `detach`, `send_prefix`, `new_window`"
            ),
            "{error}"
        );
        assert!(!error.contains("callback"), "{error}");

        let error = failure("amux.keymap.set('prefix', 'z', amux.action.zoom())");
        assert!(
            error.contains(
                "init.lua:1: unknown action zoom, expected one of detach, send_prefix, \
                 new_window, next_window, previous_window, select_window, split_pane, \
                 next_pane, select_pane, kill_pane, kill_window, rename_window, \
                 rename_session, cluster_tree, search_projects, search_worktrees, switch_table, \
                 reload_config, which_key, copy_mode, copy_mode_page_up"
            ),
            "{error}"
        );

        let error = failure("amux.action.split_pane('diagonal')");
        assert!(
            error.contains(
                "init.lua:1: invalid action: split_pane: unknown variant `diagonal`, \
                 expected `left-right` or `top-bottom`"
            ),
            "{error}"
        );
        let error = failure("amux.keymap.set('prompt', 'x', 'zoom')");
        assert!(
            error.contains("init.lua:1: invalid prompt action: unknown variant `zoom`, expected one of `submit`"),
            "{error}"
        );
        let error = failure("amux.keymap.set('copy', 'x', 'detach')");
        assert!(
            error.contains("init.lua:1: invalid copy action: unknown variant `detach`, expected one of `cursor_left`"),
            "{error}"
        );
    }

    #[test]
    fn keymap_misuse_is_reported_at_the_users_line() {
        for (source, expected) in [
            (
                "amux.action.split_pane()",
                "init.lua:1: amux.action.split_pane needs an argument",
            ),
            (
                "amux.action.detach(1)",
                "init.lua:1: amux.action.detach takes no argument",
            ),
            (
                "amux.keymap.set('prefix', 'C-', 'detach')",
                "init.lua:1: invalid key \"C-\"",
            ),
            (
                "amux.keymap.set('prefix', 'x', 5)",
                "init.lua:1: a binding is an action or a function, not a integer",
            ),
            (
                "amux.keymap.set('tree', 'x', function() end)",
                "init.lua:1: the tree table binds action names, not a function",
            ),
            (
                "amux.keymap.set('copy', 'x', function() end)",
                "init.lua:1: the copy table binds action names, not a function",
            ),
            (
                "amux.keymap.set('', 'x', 'detach')",
                "init.lua:1: a keymap table needs a name",
            ),
            (
                "amux.keymap.del('prefix', 'z')",
                "init.lua:1: z is not bound in the prefix table",
            ),
            (
                "amux.keymap.del('resize', 'h')",
                "init.lua:1: h is not bound in the resize table",
            ),
            (
                "amux.action.detach = 1",
                "init.lua:1: amux.action.detach cannot be replaced",
            ),
        ] {
            let error = failure(source);
            assert!(error.contains(expected), "{source}: {error}");
        }
    }

    #[test]
    fn hooks_are_stored_by_event() {
        let loaded = loaded(
            "amux.on('session_created', function(event) return 1 end)\n\
             amux.on('session_created', function(event) return 2 end)\n\
             amux.on('peer_offline', function(event) return 3 end)",
        );
        assert_eq!(
            loaded.hooks.events().collect::<Vec<_>>(),
            ["peer_offline", "session_created"]
        );
        let created: Vec<i64> = loaded
            .hooks
            .get("session_created")
            .iter()
            .map(|hook| hook.call(()).unwrap())
            .collect();
        assert_eq!(created, [1, 2]);
        assert!(loaded.hooks.get("window_closed").is_empty());

        let error = failure("amux.on('session_create', function() end)");
        assert!(
            error.contains(
                "init.lua:1: unknown event session_create, expected one of session_created, "
            ),
            "{error}"
        );
    }

    #[test]
    fn every_action_constructor_is_exposed() {
        let names = variants::<Binding>();
        assert_eq!(names.len(), 21);
        assert!(names.contains(&"switch_table"));
        assert!(!names.contains(&"callback"));
        let loaded = loaded("");
        let listed: Vec<String> = loaded
            .lua
            .load(
                "local names = {}\n\
                 for name in pairs(amux.action) do names[#names + 1] = name end\n\
                 table.sort(names)\n\
                 return names",
            )
            .eval()
            .unwrap();
        let mut expected: Vec<&str> = names.to_vec();
        expected.sort_unstable();
        assert_eq!(listed, expected);
    }
}
