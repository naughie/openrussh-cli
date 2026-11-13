use mlua::{Function, Lua, Result, Table, Value};

use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub(super) fn preload(lua: &Lua, path: PathBuf) -> Result<()> {
    lua.preload_module(
        "cli",
        lua.create_function(move |lua, ()| module(lua, &path))?,
    )
}

fn module(lua: &Lua, path: &Path) -> Result<Table> {
    let parent = path
        .parent()
        .ok_or_else(|| mlua::Error::external("Lua configuration path has no parent"))?;
    let constants = lua.create_table_with_capacity(0, 6)?;
    constants.set("__file__", path.as_os_str())?;
    constants.set("__dir__", parent.as_os_str())?;
    constants.set("__root__", super::root_dir(path)?.as_os_str())?;
    constants.set("os", std::env::consts::OS)?;
    constants.set("arch", std::env::consts::ARCH)?;
    constants.set("os_family", std::env::consts::FAMILY)?;

    let module = lua.create_table_with_capacity(0, 5)?;
    module.set(
        "user_name",
        lookup(lua, || whoami::username().ok().map(OsString::from))?,
    )?;
    module.set(
        "home_dir",
        lookup(lua, || std::env::home_dir().map(PathBuf::into_os_string))?,
    )?;
    module.set(
        "host_name",
        lookup(lua, || nix::unistd::gethostname().ok())?,
    )?;
    module.set("debug", lua.create_function(debug)?)?;
    module.set("temp_file", lua.create_function(super::temp::file)?)?;

    // Constants stay outside the module so assignments to them invoke __newindex.
    let metatable = lua.create_table_with_capacity(0, 3)?;
    metatable.set("__index", constants.clone())?;
    metatable.set(
        "__newindex",
        lua.create_function(move |_, (module, key, value): (Table, Value, Value)| {
            if constants.contains_key(key.clone())? {
                return Err(mlua::Error::runtime(format!(
                    "cli.{} is a read-only constant",
                    key.to_string()?
                )));
            }
            module.raw_set(key, value)
        })?,
    )?;
    metatable.set("__metatable", false)?;
    module.set_metatable(Some(metatable))?;

    Ok(module)
}

fn lookup<F>(lua: &Lua, query: F) -> Result<Function>
where
    F: Fn() -> Option<OsString> + 'static,
{
    lua.create_function(move |_, ()| Ok(query()))
}

fn debug(lua: &Lua, (value, indent): (Value, Value)) -> Result<String> {
    let indent = match indent {
        Value::Nil => 2,
        Value::Integer(_) | Value::Number(_) => lua
            .coerce_integer(indent)?
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| {
                mlua::Error::runtime("cli.debug indent must be a non-negative integer")
            })?,
        _ => {
            return Err(mlua::Error::runtime(
                "cli.debug indent must be a non-negative integer",
            ));
        }
    };
    format_debug(&value, indent)
}

fn format_debug(value: &Value, indent: usize) -> Result<String> {
    let rendered = format!("{value:#?}");
    if indent == 2 {
        return Ok(rendered);
    }
    let mut output = String::with_capacity(rendered.len());
    // mlua's pretty formatter uses two spaces per nesting level.
    for (index, line) in rendered.split('\n').enumerate() {
        if index > 0 {
            output.push('\n');
        }
        let content = line.trim_start_matches(' ');
        let leading = line.len() - content.len();
        let width = (leading / 2)
            .checked_mul(indent)
            .and_then(|n| n.checked_add(leading % 2))
            .ok_or_else(|| mlua::Error::runtime("debug indentation overflow"))?;
        output.try_reserve(width).map_err(mlua::Error::external)?;
        output.extend(std::iter::repeat_n(' ', width));
        output.push_str(content);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups_are_lazy_and_evaluated_on_each_call() -> Result<()> {
        use std::cell::Cell;
        use std::rc::Rc;

        let lua = Lua::new();
        let calls = Rc::new(Cell::new(0));
        let count = calls.clone();
        let function = lookup(&lua, move || {
            count.set(count.get() + 1);
            Some(count.get().to_string().into())
        })?;

        assert_eq!(calls.get(), 0);
        assert_eq!(function.call::<String>(())?, "1");
        assert_eq!(function.call::<String>(())?, "2");
        assert_eq!(calls.get(), 2);

        Ok(())
    }

    #[test]
    fn unavailable_values_are_nil_and_native_bytes_are_preserved() -> Result<()> {
        use std::os::unix::ffi::OsStringExt as _;

        let lua = Lua::new();
        let missing = lookup(&lua, || None)?;
        assert!(matches!(missing.call::<mlua::Value>(())?, mlua::Value::Nil));

        let native = lookup(&lua, || Some(OsString::from_vec(vec![b'x', 0xff])))?;
        let value = native.call::<mlua::String>(())?;
        assert_eq!(value.as_bytes().as_ref(), &[b'x', 0xff]);

        Ok(())
    }

    #[test]
    fn module_exports_live_local_values_and_caches_its_table() -> Result<()> {
        let lua = Lua::new();
        let path = PathBuf::from("/configuration/config.lua");
        preload(&lua, path.clone())?;
        let loaded: Table = lua.globals().get::<Table>("package")?.get("loaded")?;
        assert!(matches!(
            loaded.get::<mlua::Value>("cli")?,
            mlua::Value::Nil
        ));

        let module: Table = lua.load("return require('cli')").eval()?;
        lua.load("assert(require('cli') == require('cli'))")
            .exec()?;
        assert_eq!(
            module.get::<mlua::String>("__file__")?.as_bytes().as_ref(),
            path.as_os_str().as_encoded_bytes()
        );
        assert_eq!(
            module.get::<mlua::String>("__dir__")?.as_bytes().as_ref(),
            b"/configuration"
        );
        assert_eq!(
            module.get::<mlua::String>("__root__")?.as_bytes().as_ref(),
            b"/configuration"
        );
        assert_eq!(module.get::<String>("os")?, std::env::consts::OS);
        assert_eq!(module.get::<String>("arch")?, std::env::consts::ARCH);
        assert_eq!(module.get::<String>("os_family")?, std::env::consts::FAMILY);

        assert_eq!(
            module
                .get::<Function>("user_name")?
                .call::<Option<String>>(())?,
            whoami::username().ok()
        );
        assert_eq!(
            module
                .get::<Function>("home_dir")?
                .call::<Option<OsString>>(())?,
            std::env::home_dir().map(PathBuf::into_os_string)
        );
        assert_eq!(
            module
                .get::<Function>("host_name")?
                .call::<Option<OsString>>(())?,
            nix::unistd::gethostname().ok()
        );

        Ok(())
    }

    #[test]
    fn constants_reject_assignment_and_deletion() -> Result<()> {
        let lua = Lua::new();
        preload(&lua, PathBuf::from("/configuration/config.lua"))?;
        lua.load(
            r#"
local cli = require('cli')
for _, name in ipairs({'__file__', '__dir__', '__root__', 'os', 'arch', 'os_family'}) do
    local original = cli[name]
    for _, replace in ipairs({true, false}) do
        local ok, err = pcall(function()
            if replace then cli[name] = 'replacement' else cli[name] = nil end
        end)
        assert(not ok)
        assert(tostring(err):find('cli.' .. name .. ' is a read-only constant', 1, true))
        assert(cli[name] == original)
        assert(require('cli')[name] == original)
    end
end
assert(getmetatable(cli) == false)
assert(not pcall(setmetatable, cli, {}))
"#,
        )
        .exec()
    }

    #[test]
    fn nonconstant_module_members_remain_mutable() -> Result<()> {
        let lua = Lua::new();
        preload(&lua, PathBuf::from("/configuration/config.lua"))?;
        lua.load(
            r#"
local cli = require('cli')
assert(cli.missing == nil)
local key = {}
for _, name in ipairs({'extra', 'user_name', 'home_dir', 'host_name', 'debug', 1, key, '\255'}) do
    cli[name] = 'first'
    assert(cli[name] == 'first')
    cli[name] = 'second'
    assert(cli[name] == 'second')
    cli[name] = nil
    assert(cli[name] == nil)
end
"#,
        )
        .exec()
    }

    #[test]
    fn file_metadata_preserves_non_utf8_paths() -> Result<()> {
        use std::os::unix::ffi::OsStringExt as _;

        for (filename, root) in [
            ("config.lua", b"/configuration/\xff".as_slice()),
            ("init.lua", b"/configuration".as_slice()),
        ] {
            let lua = Lua::new();
            let path =
                PathBuf::from(OsString::from_vec(b"/configuration/\xff".to_vec())).join(filename);
            preload(&lua, path.clone())?;
            let module: Table = lua.load("return require('cli')").eval()?;

            assert_eq!(
                module.get::<mlua::String>("__file__")?.as_bytes().as_ref(),
                path.as_os_str().as_encoded_bytes()
            );
            assert_eq!(
                module.get::<mlua::String>("__dir__")?.as_bytes().as_ref(),
                path.parent().unwrap().as_os_str().as_encoded_bytes()
            );
            assert_eq!(
                module.get::<mlua::String>("__root__")?.as_bytes().as_ref(),
                root
            );
        }

        Ok(())
    }

    #[test]
    fn root_metadata_uses_the_parent_only_for_init_lua() -> Result<()> {
        for (file, root) in [
            ("/configuration/config.lua", "/configuration"),
            ("/configuration/config/init.lua", "/configuration"),
            ("/configuration/config/Init.lua", "/configuration/config"),
            (
                "/configuration/config/not-init.lua",
                "/configuration/config",
            ),
            ("/init.lua", "/"),
        ] {
            let lua = Lua::new();
            preload(&lua, PathBuf::from(file))?;
            let module: Table = lua.load("return require('cli')").eval()?;

            assert_eq!(module.get::<String>("__root__")?, root, "{file}");
        }

        Ok(())
    }

    #[test]
    fn debug_formats_nested_values_with_custom_indentation() -> Result<()> {
        let lua = Lua::new();
        let value: Value = lua
            .load("return {z = true, outer = {42, 'line\\nquoted\\\"'}, [false] = {}}")
            .eval()?;
        for (indent, expected) in [
            (
                2,
                "{\n  [false] = {},\n  outer = {\n    42,\n    \"line\\nquoted\\\"\",\n  },\n  z = true,\n}",
            ),
            (
                4,
                "{\n    [false] = {},\n    outer = {\n        42,\n        \"line\\nquoted\\\"\",\n    },\n    z = true,\n}",
            ),
            (
                0,
                "{\n[false] = {},\nouter = {\n42,\n\"line\\nquoted\\\"\",\n},\nz = true,\n}",
            ),
        ] {
            assert_eq!(format_debug(&value, indent)?, expected);
        }

        Ok(())
    }

    #[test]
    fn debug_handles_cycles_shared_tables_and_other_lua_types() -> Result<()> {
        let lua = Lua::new();
        let value: Value = lua
            .load(
                r#"
local shared = {value = 1}
local root = {first = shared, second = shared, fn = function() end,
    co = coroutine.create(function() end), bytes = '\255\0'}
root.self = root
root[root] = root
return root
"#,
            )
            .eval()?;
        let output = format_debug(&value, 4)?;

        assert!(output.contains("self = table: "));
        assert!(output.contains("second = table: "));
        assert!(output.contains("[table: "));
        assert!(output.contains("fn = function: "));
        assert!(output.contains("co = thread: "));
        assert!(output.contains("bytes = b\""));
        assert!(output.contains("value = 1"));

        Ok(())
    }

    #[test]
    fn debug_rejects_invalid_indentation() -> Result<()> {
        let lua = Lua::new();
        preload(&lua, PathBuf::from("/configuration/config.lua"))?;
        lua.load(
            r#"
local cli = require('cli')
for _, indent in ipairs({-1, 1.5, '2', false, {}, math.huge, 0/0}) do
    local ok, err = pcall(cli.debug, {}, indent)
    assert(not ok)
    assert(tostring(err):find('cli.debug indent must be a non-negative integer', 1, true))
end
"#,
        )
        .exec()
    }

    #[test]
    fn debug_returns_a_single_string_without_an_added_newline() -> Result<()> {
        let lua = Lua::new();
        preload(&lua, PathBuf::from("/configuration/config.lua"))?;
        lua.load(
            r#"
local cli = require('cli')
assert(select('#', cli.debug(nil)) == 1)
assert(cli.debug(nil) == 'nil')
assert(cli.debug(true) == 'true')
assert(cli.debug('hello') == '"hello"')
assert(cli.debug({}) == '{}')
assert(cli.debug({key = true}) == '{\n  key = true,\n}')
assert(cli.debug({key = true}, nil) == cli.debug({key = true}, 2))
assert(cli.debug({key = true}, 2.0) == cli.debug({key = true}, 2))
"#,
        )
        .exec()
    }
}
