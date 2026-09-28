mod api;
mod client;
pub mod data;
pub mod emit;
mod opt;
mod runtime;
mod server;

use std::fmt;
use std::path::Path;
use std::time::{Duration, Instant};

use mlua::{
    DeserializeOptions, FromLuaMulti, Function, HookTriggers, IntoLuaMulti, Lua, MultiValue, Value,
    VmState,
};
use serde::de::DeserializeOwned;

pub use api::{Callbacks, Hooks, EVENTS};
pub use client::LuaScripting;
pub use runtime::{find_init, load, ConfigPaths, Loaded, Process, INIT_FILE, SYSTEM_INIT};
pub use server::{HookRun, LuaHooks};

const TRACEBACK: &str = "\nstack traceback:";
const BUDGET_CHECK_INSTRUCTIONS: u32 = 1000;

fn chunk_name(path: &Path) -> String {
    format!("@{}", path.display())
}

fn function<A, R, F>(lua: &Lua, body: F) -> mlua::Result<Function>
where
    A: FromLuaMulti,
    R: IntoLuaMulti,
    F: Fn(&Lua, A) -> mlua::Result<R> + 'static,
{
    lua.create_function(move |lua, args: MultiValue| {
        A::from_lua_multi(args, lua)
            .and_then(|args| body(lua, args))
            .map_err(|error| located(lua, describe(&error)))
    })
}

fn call_within<R: FromLuaMulti>(
    lua: &Lua,
    callback: &Function,
    arguments: impl IntoLuaMulti,
    budget: Duration,
    what: &'static str,
) -> mlua::Result<R> {
    let deadline = Instant::now() + budget;
    let triggers = HookTriggers::new().every_nth_instruction(BUDGET_CHECK_INSTRUCTIONS);
    lua.set_global_hook(triggers, move |_, debug| {
        if Instant::now() < deadline {
            return Ok(VmState::Continue);
        }
        let location = debug
            .current_line()
            .zip(debug.source().short_src)
            .map(|(line, source)| format!("{source}:{line}: "))
            .unwrap_or_default();
        Err(mlua::Error::runtime(format!(
            "{location}{what} ran past its {} ms budget",
            budget.as_millis()
        )))
    })?;
    let result = callback.call::<R>(arguments);
    lua.remove_global_hook();
    lua.remove_hook();
    result
}

fn located(lua: &Lua, message: impl fmt::Display) -> mlua::Error {
    let location = lua
        .inspect_stack(1, |frame| {
            let line = frame.current_line()?;
            let source = frame.source().short_src?.into_owned();
            Some(format!("{source}:{line}: "))
        })
        .flatten()
        .unwrap_or_default();
    mlua::Error::runtime(format!("{location}{message}"))
}

fn describe(error: &mlua::Error) -> String {
    match error {
        mlua::Error::CallbackError { cause, .. } => describe(cause),
        mlua::Error::WithContext { context, cause } => format!("{context}: {}", describe(cause)),
        mlua::Error::RuntimeError(message) => message
            .split(TRACEBACK)
            .next()
            .unwrap_or_default()
            .to_owned(),
        mlua::Error::SyntaxError { message, .. }
        | mlua::Error::SerializeError(message)
        | mlua::Error::DeserializeError(message) => message.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug)]
struct Invalid {
    path: String,
    message: String,
}

impl Invalid {
    fn within(&self, root: &str) -> String {
        match self.path.as_str() {
            "." => format!("{root}: {}", self.message),
            path if path.starts_with('[') => format!("{root}{path}: {}", self.message),
            path => format!("{root}.{path}: {}", self.message),
        }
    }
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.path.as_str() {
            "." => f.write_str(&self.message),
            path => write!(f, "{path}: {}", self.message),
        }
    }
}

fn from_lua<T: DeserializeOwned>(value: Value) -> Result<T, Invalid> {
    let options = DeserializeOptions::new().sort_keys(true);
    let deserializer = mlua::serde::Deserializer::new_with_options(value, options);
    serde_path_to_error::deserialize(deserializer).map_err(|error| Invalid {
        path: error.path().to_string(),
        message: describe(error.inner()),
    })
}
