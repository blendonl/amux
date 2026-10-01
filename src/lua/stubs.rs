pub const STUBS: &str = include_str!("amux.lua");

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use mlua::Lua;

    use super::STUBS;
    use crate::lua::api::{variants, EVENTS};
    use crate::lua::opt::{shape, Shape};
    use crate::lua::runtime::{load_init, Process};
    use crate::settings::{
        Binding, KeyTable, KeyboardLayer, PickerAction, ProjectConfig, PromptAction, ServerConfig,
        Settings, TreeAction,
    };

    #[derive(Default)]
    struct Stubs {
        classes: BTreeMap<&'static str, BTreeMap<&'static str, &'static str>>,
        aliases: BTreeMap<&'static str, BTreeSet<&'static str>>,
        members: BTreeMap<&'static str, BTreeSet<&'static str>>,
        overloaded_events: BTreeSet<&'static str>,
    }

    impl Stubs {
        fn parse() -> Self {
            let mut stubs = Self::default();
            let mut class = None;
            let mut alias = None;
            for line in STUBS.lines() {
                if let Some(rest) = line.strip_prefix("---@class ") {
                    let name = rest.split([' ', ':']).next().unwrap_or_default();
                    stubs.classes.entry(name).or_default();
                    class = Some(name);
                } else if let Some(rest) = line.strip_prefix("---@field ") {
                    let mut words = rest.split_whitespace();
                    let name = words.next().unwrap_or_default().trim_end_matches('?');
                    let kind = words.next().unwrap_or_default().trim_end_matches('?');
                    let owner = class.unwrap_or_else(|| panic!("{line} is outside a class"));
                    stubs.classes.entry(owner).or_default().insert(name, kind);
                } else if let Some(rest) = line.strip_prefix("---@alias ") {
                    let (name, values) = rest.split_once(' ').unwrap_or((rest, ""));
                    stubs
                        .aliases
                        .entry(name)
                        .or_default()
                        .extend(quoted(values));
                    alias = Some(name);
                } else if let Some(rest) = line.strip_prefix("---| ") {
                    let owner = alias.unwrap_or_else(|| panic!("{line} is outside an alias"));
                    stubs.aliases.entry(owner).or_default().extend(quoted(rest));
                } else if let Some(rest) = line.strip_prefix("---@overload fun(event: ") {
                    stubs.overloaded_events.extend(quoted(rest).take(1));
                } else if let Some(rest) = line.strip_prefix("function ") {
                    let path = rest.split('(').next().unwrap_or_default();
                    stubs.add_member(path);
                } else if let Some((path, _)) = line.split_once(" = ") {
                    stubs.add_member(path);
                }
            }
            stubs
        }

        fn add_member(&mut self, path: &'static str) {
            if let Some((table, name)) = path.rsplit_once('.') {
                self.members.entry(table).or_default().insert(name);
            }
        }

        fn alias(&self, name: &str) -> BTreeSet<&'static str> {
            self.aliases.get(name).cloned().unwrap_or_default()
        }

        fn fields(&self, class: &str) -> BTreeSet<&'static str> {
            self.classes
                .get(class)
                .map(|fields| fields.keys().copied().collect())
                .unwrap_or_default()
        }

        fn members(&self, table: &str) -> BTreeSet<&'static str> {
            self.members.get(table).cloned().unwrap_or_default()
        }

        fn compare(&self, class: &str, shape: &Shape, path: &str, problems: &mut Vec<String>) {
            let Shape::Struct(fields) = shape else {
                return;
            };
            let Some(declared) = self.classes.get(class) else {
                problems.push(format!("{path} has no ---@class {class}"));
                return;
            };
            for (name, field) in fields {
                match declared.get(name) {
                    Some(kind) => self.compare(kind, field, &format!("{path}.{name}"), problems),
                    None => problems.push(format!("{path}.{name} is missing from {class}")),
                }
            }
            for name in declared.keys().filter(|name| !fields.contains_key(*name)) {
                problems.push(format!("{class}.{name} is not a field of {path}"));
            }
        }
    }

    fn quoted(text: &'static str) -> impl Iterator<Item = &'static str> {
        text.split('"').skip(1).step_by(2)
    }

    fn set(names: &[&'static str]) -> BTreeSet<&'static str> {
        names.iter().copied().collect()
    }

    fn names(lua: &Lua, table: &str) -> BTreeSet<String> {
        lua.load(format!(
            "local names = {{}} for name in pairs({table}) do names[#names + 1] = name end \
             return names"
        ))
        .eval::<Vec<String>>()
        .unwrap()
        .into_iter()
        .collect()
    }

    #[test]
    fn every_option_has_a_stub_and_every_stub_an_option() {
        let stubs = Stubs::parse();
        let server = ServerConfig {
            address: String::new(),
            amux_path: None,
            socket: None,
        };
        let samples = [
            ("amux.opt", "amux.opt", shape(&Settings::default())),
            (
                "amux.opt.servers.<name>",
                "amux.ServerConfig",
                shape(&server),
            ),
            (
                "amux.opt.projects.<name>",
                "amux.ProjectConfig",
                shape(&ProjectConfig::default()),
            ),
            (
                "amux.opt.android.keyboard.layers.<name>",
                "amux.KeyboardLayer",
                shape(&KeyboardLayer::default()),
            ),
            ("a key table", "amux.KeyTable", shape(&KeyTable::default())),
        ];
        let mut problems = Vec::new();
        for (path, class, shape) in samples {
            stubs.compare(class, &shape.unwrap(), path, &mut problems);
        }
        assert!(
            problems.is_empty(),
            "src/lua/amux.lua is out of date:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn every_action_has_a_name_and_a_constructor_in_the_stubs() {
        let stubs = Stubs::parse();
        let actions = set(variants::<Binding>());
        assert_eq!(stubs.alias("amux.ActionName"), actions);
        assert_eq!(stubs.fields("amux.action"), actions);
        assert_eq!(
            stubs.alias("amux.PromptAction"),
            set(variants::<PromptAction>())
        );
        assert_eq!(
            stubs.alias("amux.TreeAction"),
            set(variants::<TreeAction>())
        );
        assert_eq!(
            stubs.alias("amux.PickerAction"),
            set(variants::<PickerAction>())
        );
    }

    #[test]
    fn every_event_has_a_name_and_an_overload_in_the_stubs() {
        let stubs = Stubs::parse();
        assert_eq!(stubs.alias("amux.EventName"), set(&EVENTS));
        assert_eq!(stubs.overloaded_events, set(&EVENTS));
    }

    #[test]
    fn every_function_the_client_or_server_has_is_in_the_stubs() {
        let stubs = Stubs::parse();
        let mut api = BTreeSet::new();
        let mut keymap = BTreeSet::new();
        for process in [Process::Client, Process::Server] {
            let loaded = load_init("", process).unwrap();
            api.extend(names(&loaded.lua, "amux"));
            keymap.extend(names(&loaded.lua, "amux.keymap"));
        }
        let mut declared: BTreeSet<String> = stubs
            .members("amux")
            .union(&stubs.fields("amux"))
            .map(|name| name.to_string())
            .collect();
        assert_eq!(declared, api);
        declared = stubs
            .members("amux.keymap")
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(declared, keymap);
    }
}
