mod api;
pub mod data;
pub mod emit;
mod opt;
mod runtime;

use std::fmt;
use std::path::Path;

use mlua::{DeserializeOptions, FromLuaMulti, Function, IntoLuaMulti, Lua, MultiValue, Value};
use serde::de::DeserializeOwned;

pub use api::{Callbacks, Hooks, EVENTS};
pub use runtime::{find_init, load, ConfigPaths, Loaded, Process, INIT_FILE, SYSTEM_INIT};

const TRACEBACK: &str = "\nstack traceback:";

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
