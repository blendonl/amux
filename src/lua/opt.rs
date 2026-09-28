use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::marker::PhantomData;
use std::ops::Bound;
use std::rc::Rc;

use mlua::{Function, IntoLua, Lua, LuaSerdeExt, SerializeOptions, Table, Value};
use serde::de::value::Error;
use serde::de::DeserializeOwned;
use serde::ser::{self, Serialize};

use super::{from_lua, function};

const ROOT: &str = "amux.opt";

const SERIALIZE: SerializeOptions = SerializeOptions::new()
    .set_array_metatable(false)
    .serialize_none_to_null(false)
    .serialize_unit_to_null(false);

pub struct Opt<T> {
    state: Rc<State>,
    proxy: Table,
    settings: PhantomData<T>,
}

impl<T: Serialize + DeserializeOwned + Default> Opt<T> {
    pub fn install(lua: &Lua) -> mlua::Result<Self> {
        let defaults = T::default();
        let Shape::Struct(fields) = defaults
            .serialize(Recorder)
            .map_err(mlua::Error::external)?
        else {
            return Err(mlua::Error::runtime(format!("{ROOT} must be a struct")));
        };
        let tree = |lua: &Lua| match lua.to_value_with(&defaults, SERIALIZE)? {
            Value::Table(table) => Ok(table),
            other => Err(mlua::Error::runtime(format!(
                "{ROOT} serialized to a {}",
                other.type_name()
            ))),
        };
        let state = Rc::new(State {
            data: tree(lua)?,
            defaults: tree(lua)?,
            views: lua.create_table()?,
            normalize: normalize::<T>,
        });
        let proxy = view(lua, &state, Vec::new(), fields)?;
        Ok(Self {
            state,
            proxy,
            settings: PhantomData,
        })
    }

    pub fn proxy(&self) -> &Table {
        &self.proxy
    }

    pub fn settings(&self, lua: &Lua) -> Result<T, String> {
        let data = Unwrapper::new(lua, &self.state)
            .unwrap(Value::Table(self.state.data.clone()))
            .map_err(|error| error.to_string())?;
        from_lua(data).map_err(|invalid| invalid.within(ROOT))
    }
}

struct State {
    data: Table,
    defaults: Table,
    views: Table,
    normalize: fn(&Lua, Table) -> Result<Table, String>,
}

impl State {
    fn node(&self, path: &[impl AsRef<str>]) -> mlua::Result<Table> {
        path.iter().try_fold(self.data.clone(), |table, name| {
            table
                .raw_get::<Option<Table>>(name.as_ref())?
                .ok_or_else(|| mlua::Error::runtime(format!("{} is not set", dotted(path))))
        })
    }
}

fn normalize<T: Serialize + DeserializeOwned>(lua: &Lua, trial: Table) -> Result<Table, String> {
    let settings: T = from_lua(Value::Table(trial)).map_err(|invalid| invalid.within(ROOT))?;
    match lua.to_value_with(&settings, SERIALIZE) {
        Ok(Value::Table(table)) => Ok(table),
        Ok(other) => Err(format!("{ROOT} serialized to a {}", other.type_name())),
        Err(error) => Err(error.to_string()),
    }
}

fn dotted(path: &[impl AsRef<str>]) -> String {
    path.iter().fold(ROOT.to_owned(), |mut dotted, name| {
        dotted.push('.');
        dotted.push_str(name.as_ref());
        dotted
    })
}

struct Node {
    path: Vec<&'static str>,
    fields: BTreeMap<&'static str, Shape>,
    children: BTreeMap<&'static str, Table>,
}

impl Node {
    fn field(&self, key: &str) -> mlua::Result<&'static str> {
        self.fields
            .get_key_value(key)
            .map(|(name, _)| *name)
            .ok_or_else(|| {
                let path: Vec<&str> = self.path.iter().copied().chain([key]).collect();
                let names: Vec<&str> = self.fields.keys().copied().collect();
                mlua::Error::runtime(format!(
                    "unknown option {}, expected one of {}",
                    dotted(&path),
                    names.join(", ")
                ))
            })
    }

    fn get(&self, state: &State, name: &'static str) -> mlua::Result<Value> {
        let value: Value = state.node(&self.path)?.raw_get(name)?;
        match self.children.get(name) {
            Some(child) if !value.is_nil() => Ok(Value::Table(child.clone())),
            _ => Ok(value),
        }
    }

    fn set(&self, lua: &Lua, state: &State, name: &'static str, value: Value) -> mlua::Result<()> {
        let value = Unwrapper::new(lua, state).unwrap(value)?;
        let root = copy(lua, &state.defaults)?;
        let parent = self.path.iter().try_fold(root.clone(), |table, step| {
            let child = copy(lua, &table.raw_get(*step)?)?;
            table.raw_set(*step, &child)?;
            Ok::<_, mlua::Error>(child)
        })?;
        parent.raw_set(name, value)?;
        let normalized = (state.normalize)(lua, root).map_err(mlua::Error::runtime)?;
        let stored: Value = self
            .path
            .iter()
            .try_fold(normalized, |table, step| table.raw_get::<Table>(*step))?
            .raw_get(name)?;
        state.node(&self.path)?.raw_set(name, stored)
    }

    fn next(&self, lua: &Lua, state: &State, after: Option<&str>) -> mlua::Result<(Value, Value)> {
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        for name in self
            .fields
            .range::<str, _>((start, Bound::Unbounded))
            .map(|(name, _)| *name)
        {
            let value = self.get(state, name)?;
            if !value.is_nil() {
                return Ok((name.into_lua(lua)?, value));
            }
        }
        Ok((Value::Nil, Value::Nil))
    }
}

fn view(
    lua: &Lua,
    state: &Rc<State>,
    path: Vec<&'static str>,
    fields: BTreeMap<&'static str, Shape>,
) -> mlua::Result<Table> {
    let mut children = BTreeMap::new();
    for (name, shape) in &fields {
        if let Shape::Struct(nested) = shape {
            let mut nested_path = path.clone();
            nested_path.push(name);
            children.insert(*name, view(lua, state, nested_path, nested.clone())?);
        }
    }
    let node = Rc::new(Node {
        path,
        fields,
        children,
    });
    let proxy = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__index", {
        let (state, node) = (state.clone(), node.clone());
        function(lua, move |_, (_, key): (Table, String)| {
            node.get(&state, node.field(&key)?)
        })?
    })?;
    meta.set("__newindex", {
        let (state, node) = (state.clone(), node.clone());
        function(lua, move |lua, (_, key, value): (Table, String, Value)| {
            node.set(lua, &state, node.field(&key)?, value)
        })?
    })?;
    let next: Function = {
        let (state, node) = (state.clone(), node.clone());
        function(lua, move |lua, (_, after): (Table, Option<String>)| {
            node.next(lua, &state, after.as_deref())
        })?
    };
    meta.set(
        "__pairs",
        function(lua, move |_, proxy: Table| {
            Ok((next.clone(), proxy, Value::Nil))
        })?,
    )?;
    meta.set("__metatable", ROOT)?;
    proxy.set_metatable(Some(meta))?;
    state.views.raw_set(&proxy, node.path.clone())?;
    Ok(proxy)
}

fn copy(lua: &Lua, table: &Table) -> mlua::Result<Table> {
    let copy = lua.create_table()?;
    table.for_each(|key: Value, value: Value| copy.raw_set(key, value))?;
    Ok(copy)
}

struct Unwrapper<'a> {
    lua: &'a Lua,
    state: &'a State,
    copies: HashMap<*const c_void, Table>,
}

impl<'a> Unwrapper<'a> {
    fn new(lua: &'a Lua, state: &'a State) -> Self {
        Self {
            lua,
            state,
            copies: HashMap::new(),
        }
    }

    fn unwrap(&mut self, value: Value) -> mlua::Result<Value> {
        let Value::Table(table) = value else {
            return Ok(value);
        };
        if let Some(path) = self.state.views.raw_get::<Option<Vec<String>>>(&table)? {
            return self.unwrap(Value::Table(self.state.node(&path)?));
        }
        if let Some(copy) = self.copies.get(&table.to_pointer()) {
            return Ok(Value::Table(copy.clone()));
        }
        let copy = self.lua.create_table()?;
        self.copies.insert(table.to_pointer(), copy.clone());
        for pair in table.pairs::<Value, Value>() {
            let (key, value) = pair?;
            copy.raw_set(key, self.unwrap(value)?)?;
        }
        Ok(Value::Table(copy))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Shape {
    Struct(BTreeMap<&'static str, Shape>),
    Map,
    Value,
}

struct Recorder;

impl ser::Serializer for Recorder {
    type Ok = Shape;
    type Error = Error;
    type SerializeSeq = Opaque;
    type SerializeTuple = Opaque;
    type SerializeTupleStruct = Opaque;
    type SerializeTupleVariant = Opaque;
    type SerializeMap = Opaque;
    type SerializeStruct = Fields;
    type SerializeStructVariant = Opaque;

    fn serialize_bool(self, _: bool) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_i8(self, _: i8) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_i16(self, _: i16) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_i32(self, _: i32) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_i64(self, _: i64) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_u8(self, _: u8) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_u16(self, _: u16) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_u32(self, _: u32) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_u64(self, _: u64) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_f32(self, _: f32) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_f64(self, _: f64) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_char(self, _: char) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_str(self, _: &str) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_bytes(self, _: &[u8]) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_none(self) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Shape, Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_unit_struct(self, _: &'static str) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Shape, Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Shape, Error> {
        Ok(Shape::Value)
    }

    fn serialize_seq(self, _: Option<usize>) -> Result<Opaque, Error> {
        Ok(Opaque(Shape::Value))
    }

    fn serialize_tuple(self, _: usize) -> Result<Opaque, Error> {
        Ok(Opaque(Shape::Value))
    }

    fn serialize_tuple_struct(self, _: &'static str, _: usize) -> Result<Opaque, Error> {
        Ok(Opaque(Shape::Value))
    }

    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Opaque, Error> {
        Ok(Opaque(Shape::Value))
    }

    fn serialize_map(self, _: Option<usize>) -> Result<Opaque, Error> {
        Ok(Opaque(Shape::Map))
    }

    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Fields, Error> {
        Ok(Fields(BTreeMap::new()))
    }

    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Opaque, Error> {
        Ok(Opaque(Shape::Value))
    }
}

struct Opaque(Shape);

impl ser::SerializeSeq for Opaque {
    type Ok = Shape;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Error> {
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(self.0)
    }
}

impl ser::SerializeTuple for Opaque {
    type Ok = Shape;
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Error> {
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(self.0)
    }
}

impl ser::SerializeTupleStruct for Opaque {
    type Ok = Shape;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Error> {
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(self.0)
    }
}

impl ser::SerializeTupleVariant for Opaque {
    type Ok = Shape;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Error> {
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(self.0)
    }
}

impl ser::SerializeMap for Opaque {
    type Ok = Shape;
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Error> {
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, _: &T) -> Result<(), Error> {
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(self.0)
    }
}

impl ser::SerializeStructVariant for Opaque {
    type Ok = Shape;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _: &'static str,
        _: &T,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(self.0)
    }
}

struct Fields(BTreeMap<&'static str, Shape>);

impl ser::SerializeStruct for Fields {
    type Ok = Shape;
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.0.insert(name, value.serialize(Recorder)?);
        Ok(())
    }

    fn skip_field(&mut self, name: &'static str) -> Result<(), Error> {
        self.0.insert(name, Shape::Value);
        Ok(())
    }

    fn end(self) -> Result<Shape, Error> {
        Ok(Shape::Struct(self.0))
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::lua::describe;
    use crate::settings::Color;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Sample {
        name: String,
        retries: u32,
        inner: Inner,
        servers: BTreeMap<String, Server>,
        shell: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hidden: Option<u8>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Inner {
        label: String,
        flag: bool,
        color: Option<Color>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Server {
        address: String,
    }

    impl Default for Sample {
        fn default() -> Self {
            Self {
                name: "box".into(),
                retries: 3,
                inner: Inner::default(),
                servers: BTreeMap::new(),
                shell: None,
                hidden: None,
            }
        }
    }

    impl Default for Inner {
        fn default() -> Self {
            Self {
                label: "x".into(),
                flag: true,
                color: None,
            }
        }
    }

    fn run(source: &str) -> Result<Sample, String> {
        let lua = Lua::new();
        let opt = Opt::<Sample>::install(&lua).unwrap();
        lua.globals().set("opt", opt.proxy()).unwrap();
        lua.load(source)
            .set_name("@/config/init.lua")
            .exec()
            .map_err(|error| describe(&error))?;
        opt.settings(&lua)
    }

    #[test]
    fn the_shape_follows_the_serde_data_model() {
        let Shape::Struct(fields) = Sample::default().serialize(Recorder).unwrap() else {
            panic!("a struct records as a struct");
        };
        assert_eq!(
            fields,
            BTreeMap::from([
                ("name", Shape::Value),
                ("retries", Shape::Value),
                (
                    "inner",
                    Shape::Struct(BTreeMap::from([
                        ("label", Shape::Value),
                        ("flag", Shape::Value),
                        ("color", Shape::Value),
                    ]))
                ),
                ("servers", Shape::Map),
                ("shell", Shape::Value),
                ("hidden", Shape::Value),
            ])
        );
    }

    #[test]
    fn the_defaults_are_readable_and_round_trip() {
        let settings = run("assert(opt.name == 'box')\n\
             assert(opt.retries == 3 and math.type(opt.retries) == 'integer')\n\
             assert(opt.inner.label == 'x' and opt.inner.flag == true)\n\
             assert(opt.inner.color == nil and opt.shell == nil and opt.hidden == nil)\n\
             assert(next(opt.servers) == nil)\n\
             assert(getmetatable(opt) == 'amux.opt')")
        .unwrap();
        assert_eq!(settings, Sample::default());
    }

    #[test]
    fn assignments_are_normalized_and_read_back() {
        let settings = run("opt.name = 'desk'\n\
             opt.inner.color = 12\n\
             assert(opt.inner.color == 'bright-blue')\n\
             opt.inner.color = 200\n\
             assert(opt.inner.color == 200)\n\
             opt.shell = { '/bin/fish', '-l' }\n\
             opt.hidden = 7\n\
             assert(opt.hidden == 7)")
        .unwrap();
        assert_eq!(
            settings,
            Sample {
                name: "desk".into(),
                inner: Inner {
                    color: Some(Color::Indexed(200)),
                    ..Inner::default()
                },
                shell: Some(vec!["/bin/fish".into(), "-l".into()]),
                hidden: Some(7),
                ..Sample::default()
            }
        );
    }

    #[test]
    fn an_unknown_option_is_reported_at_the_users_line() {
        assert_eq!(
            run("opt.name = 'desk'\nopt.bogus = 1").unwrap_err(),
            "/config/init.lua:2: unknown option amux.opt.bogus, \
             expected one of hidden, inner, name, retries, servers, shell"
        );
        assert_eq!(
            run("\n\nlocal label = opt.inner.lable").unwrap_err(),
            "/config/init.lua:3: unknown option amux.opt.inner.lable, \
             expected one of color, flag, label"
        );
        assert_eq!(
            run("local inner = opt.inner\ninner.bogus = true").unwrap_err(),
            "/config/init.lua:2: unknown option amux.opt.inner.bogus, \
             expected one of color, flag, label"
        );
    }

    #[test]
    fn a_wrong_type_is_reported_at_the_users_line() {
        let error = run("opt.retries = 'many'").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:1: amux.opt.retries: invalid type: string"),
            "{error}"
        );
        let error = run("\nopt.inner.flag = 1").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:2: amux.opt.inner.flag: invalid type"),
            "{error}"
        );
        let error = run("opt.inner.color = 'purple'").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:1: amux.opt.inner.color: invalid color"),
            "{error}"
        );
        let error = run("opt.inner = { lable = 'y' }").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:1: amux.opt.inner"),
            "{error}"
        );
        assert!(error.contains("unknown field `lable`"), "{error}");
        let error = run("opt.inner = 5").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:1: amux.opt.inner: invalid type"),
            "{error}"
        );
        let error = run("opt.name = function() end").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:1: amux.opt.name: "),
            "{error}"
        );
    }

    #[test]
    fn a_table_replaces_a_struct_and_nil_restores_the_default() {
        let settings = run("opt.inner.flag = false\n\
             opt.inner = { label = 'y' }\n\
             assert(opt.inner.flag == true and opt.inner.label == 'y')\n\
             opt.retries = 9\n\
             opt.retries = nil\n\
             assert(opt.retries == 3)")
        .unwrap();
        assert_eq!(
            settings,
            Sample {
                inner: Inner {
                    label: "y".into(),
                    ..Inner::default()
                },
                ..Sample::default()
            }
        );
    }

    #[test]
    fn views_follow_the_path_and_copy_by_value() {
        let settings = run("local inner = opt.inner\n\
             opt.inner = { label = 'y' }\n\
             inner.flag = false\n\
             assert(opt.inner.flag == false and inner.label == 'y')\n\
             opt.inner = opt.inner\n\
             opt.inner = { label = opt.name, color = 'red' }")
        .unwrap();
        assert_eq!(
            settings.inner,
            Inner {
                label: "box".into(),
                flag: true,
                color: Some(Color::Indexed(1)),
            }
        );
    }

    #[test]
    fn map_options_stay_open() {
        let settings = run("opt.servers.laptop = { address = 'ssh://laptop' }\n\
             opt.servers['build-box'] = {}\n\
             opt.servers['build-box'].address = 'ssh://build'")
        .unwrap();
        assert_eq!(
            settings.servers,
            BTreeMap::from([
                (
                    "build-box".into(),
                    Server {
                        address: "ssh://build".into()
                    }
                ),
                (
                    "laptop".into(),
                    Server {
                        address: "ssh://laptop".into()
                    }
                ),
            ])
        );

        let settings =
            run("opt.servers = { a = { address = 'x' } }\nopt.servers.b = { address = 'y' }")
                .unwrap();
        assert_eq!(settings.servers.len(), 2);
    }

    #[test]
    fn map_entries_are_checked_when_the_settings_are_read() {
        let error = run("opt.servers.laptop = { adress = 'ssh://laptop' }").unwrap_err();
        assert!(error.starts_with("amux.opt.servers.laptop"), "{error}");
        assert!(error.contains("unknown field `adress`"), "{error}");

        let error = run("opt.servers.laptop = { address = 5 }").unwrap_err();
        assert!(
            error.starts_with("amux.opt.servers.laptop.address: invalid type"),
            "{error}"
        );

        let error = run("\nopt.servers = { laptop = { address = 5 } }").unwrap_err();
        assert!(
            error.starts_with("/config/init.lua:2: amux.opt.servers.laptop.address: invalid type"),
            "{error}"
        );
    }

    #[test]
    fn pairs_lists_the_options_that_are_set() {
        let lua = Lua::new();
        let opt = Opt::<Sample>::install(&lua).unwrap();
        lua.globals().set("opt", opt.proxy()).unwrap();
        let keys: String = lua
            .load(
                "local keys = {}\n\
                 for key, value in pairs(opt) do keys[#keys + 1] = key end\n\
                 for key in pairs(opt.inner) do keys[#keys + 1] = 'inner.' .. key end\n\
                 return table.concat(keys, ',')",
            )
            .eval()
            .unwrap();
        assert_eq!(keys, "inner,name,retries,servers,inner.flag,inner.label");
    }
}
