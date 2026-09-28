use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use mlua::{Lua, LuaOptions, StdLib, Table, Value};
use serde::de::DeserializeOwned;

use super::{chunk_name, describe, from_lua, function, with_full_path};

pub fn load<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let source = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse(path, &source)
}

pub fn parse<T: DeserializeOwned>(path: &Path, source: &[u8]) -> Result<T> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::new())?;
    let value: Value = lua
        .load(source)
        .set_name(chunk_name(path))
        .set_environment(sealed_environment(&lua)?)
        .call(())
        .map_err(|error| anyhow::Error::msg(with_full_path(describe(&error), path)))?;
    if !value.is_table() {
        bail!(
            "{}: expected the file to return a table, not {}",
            path.display(),
            value.type_name()
        );
    }
    from_lua(value).map_err(|invalid| anyhow::anyhow!("{}: {invalid}", path.display()))
}

fn sealed_environment(lua: &Lua) -> mlua::Result<Table> {
    let guard = lua.create_table()?;
    guard.set(
        "__index",
        function(lua, |_, (_, name): (Table, Value)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!(
                "a data file cannot read the global {}",
                name.to_string()?
            )))
        })?,
    )?;
    guard.set(
        "__newindex",
        function(
            lua,
            |_, (_, name, _): (Table, Value, Value)| -> mlua::Result<()> {
                Err(mlua::Error::runtime(format!(
                    "a data file cannot set the global {}",
                    name.to_string()?
                )))
            },
        )?,
    )?;
    let environment = lua.create_table()?;
    environment.set_metatable(Some(guard))?;
    Ok(environment)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Server {
        address: String,
        port: Option<u16>,
    }

    fn parsed(source: &str) -> Result<BTreeMap<String, Server>> {
        parse(Path::new("/config/servers.lua"), source.as_bytes())
    }

    #[test]
    fn a_data_file_returns_a_typed_table() {
        let servers = parsed(
            "local port = 2200\n\
             return {\n\
               laptop = { address = \"ssh://laptop\", port = port + 22 },\n\
               [\"build-box\"] = { address = \"ssh://\" .. \"build\" },\n\
             }",
        )
        .unwrap();
        assert_eq!(
            servers,
            BTreeMap::from([
                (
                    "build-box".into(),
                    Server {
                        address: "ssh://build".into(),
                        port: None
                    }
                ),
                (
                    "laptop".into(),
                    Server {
                        address: "ssh://laptop".into(),
                        port: Some(2222)
                    }
                ),
            ])
        );
        assert_eq!(parsed("return {}").unwrap(), BTreeMap::new());
    }

    #[test]
    fn a_data_file_cannot_reach_globals() {
        for (source, expected) in [
            (
                "return { laptop = { address = os.getenv(\"HOME\") } }",
                "/config/servers.lua:1: a data file cannot read the global os",
            ),
            (
                "\nlocal p = print\nreturn {}",
                "/config/servers.lua:2: a data file cannot read the global print",
            ),
            (
                "x = 1\nreturn {}",
                "/config/servers.lua:1: a data file cannot set the global x",
            ),
            (
                "return { laptop = { address = _G } }",
                "/config/servers.lua:1: a data file cannot read the global _G",
            ),
            (
                "return { laptop = { address = require(\"x\") } }",
                "/config/servers.lua:1: a data file cannot read the global require",
            ),
        ] {
            let error = parsed(source).unwrap_err().to_string();
            assert_eq!(error, expected, "{source}");
        }
    }

    #[test]
    fn a_data_file_holds_data_only() {
        let error = parsed("return { laptop = { address = function() end } }")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("/config/servers.lua: laptop.address: "),
            "{error}"
        );

        let error = parsed("return 5").unwrap_err().to_string();
        assert_eq!(
            error,
            "/config/servers.lua: expected the file to return a table, not integer"
        );
        let error = parsed("").unwrap_err().to_string();
        assert!(error.ends_with("not nil"), "{error}");
    }

    #[test]
    fn errors_name_the_file_and_the_path() {
        let error = parsed("return { laptop = { address = 5 } }")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("/config/servers.lua: laptop.address: invalid type"),
            "{error}"
        );
        let error = parsed("return { laptop = { adress = \"x\" } }")
            .unwrap_err()
            .to_string();
        assert!(error.contains("laptop"), "{error}");
        assert!(error.contains("unknown field `adress`"), "{error}");

        let error = parsed("return {\n  laptop = {\n    address = \"x\",\n  }\n")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("/config/servers.lua:5:"), "{error}");
    }

    #[test]
    fn a_data_file_is_read_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("servers.lua");
        let missing = load::<BTreeMap<String, Server>>(&path).unwrap_err();
        assert!(missing.to_string().starts_with("reading "), "{missing}");

        fs::write(&path, "return { laptop = { address = \"ssh://laptop\" } }").unwrap();
        let servers: BTreeMap<String, Server> = load(&path).unwrap();
        assert_eq!(servers["laptop"].address, "ssh://laptop");
    }
}
