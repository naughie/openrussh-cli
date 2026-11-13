use mlua::{Lua, Result, Value};
use tempfile::TempDir;

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Component, Path};
use std::sync::Arc;

pub(super) fn file(lua: &Lua, name: Value) -> Result<Option<OsString>> {
    // Absent means unused; a stored None caches the first creation failure.
    let initialized = lua.app_data_ref::<Option<Arc<TempDir>>>().is_some();
    if !initialized {
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .ok()
            .map(Arc::new);
        lua.set_app_data(directory);
    }
    let state = lua
        .app_data_ref::<Option<Arc<TempDir>>>()
        .expect("temporary directory state was initialized");
    let Some(directory) = state.as_ref() else {
        return Ok(None);
    };

    let path = match name {
        Value::Nil => directory
            .path()
            .join(format!("{:032x}", rand::random::<u128>())),
        Value::String(name) => {
            let bytes = name.as_bytes();
            let relative = Path::new(std::ffi::OsStr::from_bytes(&bytes));
            if !contained_file(relative) {
                return Ok(None);
            }
            directory.path().join(relative)
        }
        _ => return Ok(None),
    };
    Ok(Some(path.into_os_string()))
}

pub(super) fn take(lua: &Lua) -> Option<Arc<TempDir>> {
    lua.remove_app_data::<Option<Arc<TempDir>>>().flatten()
}

fn contained_file(path: &Path) -> bool {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return false;
    }
    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                let Some(parent) = depth.checked_sub(1) else {
                    return false;
                };
                depth = parent;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    depth > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn module_loading_does_not_initialize_temporary_storage() -> Result<()> {
        let lua = Lua::new();
        super::super::cli::preload(&lua, PathBuf::from("/configuration/config.lua"))?;
        lua.load("local cli = require('cli'); assert(type(cli.temp_file) == 'function')")
            .exec()?;

        assert!(lua.app_data_ref::<Option<Arc<TempDir>>>().is_none());
        assert!(take(&lua).is_none());
        Ok(())
    }

    #[test]
    fn paths_share_one_private_directory_and_do_not_create_files() -> Result<()> {
        let directory = {
            let lua = Lua::new();
            super::super::cli::preload(&lua, PathBuf::from("/configuration/config.lua"))?;
            let named: PathBuf = lua
                .load("return require('cli').temp_file('private-key')")
                .eval()?;
            let random: PathBuf = lua.load("return require('cli').temp_file()").eval()?;
            let another: PathBuf = lua.load("return require('cli').temp_file(nil)").eval()?;
            let repeated: PathBuf = lua
                .load("return require('cli').temp_file('private-key')")
                .eval()?;
            let directory = named.parent().unwrap().to_owned();

            assert!(named.is_absolute());
            assert_eq!(named.file_name().unwrap(), "private-key");
            assert_eq!(repeated, named);
            assert_eq!(random.parent(), Some(directory.as_path()));
            assert_eq!(another.parent(), Some(directory.as_path()));
            assert_ne!(random, another);
            assert_eq!(
                std::fs::metadata(&directory)?.permissions().mode() & 0o777,
                0o700
            );
            for path in [&named, &random, &another] {
                assert!(!path.exists());
            }
            std::fs::write(named, "secret")?;
            directory
        };

        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn names_preserve_native_bytes_and_reject_paths_outside_the_directory() -> Result<()> {
        let lua = Lua::new();
        super::super::cli::preload(&lua, PathBuf::from("/configuration/config.lua"))?;
        lua.load(
            r#"
local cli = require('cli')
local path = assert(cli.temp_file('key\255'))
assert(path:sub(-4) == 'key\255')
local prefix = path:sub(1, -5)
assert(cli.temp_file('nested/key') == prefix .. 'nested/key')
assert(cli.temp_file('a/../key') == prefix .. 'a/../key')
for _, name in ipairs({'/absolute', '../escape', 'a/../../escape', '', '.', 'a/..', 'key\0'}) do
    assert(cli.temp_file(name) == nil)
end
assert(cli.temp_file(false) == nil)
assert(cli.temp_file({}) == nil)
"#,
        )
        .exec()
    }

    #[test]
    fn a_cached_creation_failure_stays_nil() -> Result<()> {
        let lua = Lua::new();
        lua.set_app_data::<Option<Arc<TempDir>>>(None);

        assert!(file(&lua, Value::Nil)?.is_none());
        assert!(file(&lua, Value::String(lua.create_string("key")?))?.is_none());
        assert!(
            lua.app_data_ref::<Option<Arc<TempDir>>>()
                .unwrap()
                .is_none()
        );
        Ok(())
    }
}
