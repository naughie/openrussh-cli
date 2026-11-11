use mlua::{Function, Lua, Result, Table};

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
    let module = lua.create_table_with_capacity(0, 5)?;
    module.set("__file__", path.as_os_str())?;
    module.set("__dir__", parent.as_os_str())?;

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

    Ok(module)
}

fn lookup<F>(lua: &Lua, query: F) -> Result<Function>
where
    F: Fn() -> Option<OsString> + 'static,
{
    lua.create_function(move |_, ()| Ok(query()))
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
    fn file_metadata_preserves_non_utf8_paths() -> Result<()> {
        use std::os::unix::ffi::OsStringExt as _;

        let lua = Lua::new();
        let path = PathBuf::from(OsString::from_vec(
            b"/configuration/\xff/config.lua".to_vec(),
        ));
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

        Ok(())
    }
}
