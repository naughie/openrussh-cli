use super::{Error, ResolveConfig, ResolveOptions, Resolved, Result};

#[cfg(test)]
use openrussh_client::config::Chain;

use mlua::serde::Deserializer;
use mlua::{DeserializeOptions, Lua, Table, Value};
use serde::Deserialize as _;

use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

use tempfile::TempDir;

mod cli;
mod jumps;
mod paths;
mod profiles;
mod temp;
mod values;

use profiles::Profiles;

pub(super) struct ParsedConfig {
    profiles: Profiles,
    temp_dir: Option<Arc<TempDir>>,
}

impl ParsedConfig {
    pub(super) fn execute(text: &str, path: &Path) -> Result<Self> {
        let package_path = std::env::var_os("SSH_LUA_PACKAGE_PATH");
        Self::execute_with_package_path(text, path, package_path.as_deref())
    }

    fn execute_with_package_path(
        text: &str,
        path: &Path,
        package_path: Option<&OsStr>,
    ) -> Result<Self> {
        let path = path
            .canonicalize()
            .map_err(|source| Error::CanonicalizeLua {
                path: path.to_owned(),
                source,
            })?;
        let (profiles, temp_dir) = {
            let lua = Lua::new();
            let name = format!("@{}", path.display());
            add_module_path(&lua, root_dir(&path)?, package_path)?;
            cli::preload(&lua, path)?;
            let value = lua.load(text).set_name(name).eval::<Value>()?;
            let options = DeserializeOptions::new()
                .encode_empty_tables_as_array(true)
                .detect_mixed_tables(true);
            let profiles = Profiles::deserialize(Deserializer::new_with_options(value, options))?;
            (profiles, temp::owner(&lua))
        };

        Ok(Self { profiles, temp_dir })
    }
}

impl ResolveConfig for ParsedConfig {
    fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        let mut resolved = self.profiles.resolve(options)?;
        resolved.temp_dir = self.temp_dir.clone();
        Ok(resolved)
    }
}

fn root_dir(path: &Path) -> mlua::Result<&Path> {
    let directory = path
        .parent()
        .ok_or_else(|| mlua::Error::external("Lua configuration path has no parent"))?;
    if path.file_name().is_some_and(|name| name == "init.lua") {
        Ok(directory.parent().unwrap_or(directory))
    } else {
        Ok(directory)
    }
}

fn add_module_path(lua: &Lua, root: &Path, configured: Option<&OsStr>) -> mlua::Result<()> {
    let package = lua.globals().get::<Table>("package")?;
    let configured = configured.map(OsStr::as_encoded_bytes);
    let insertion = configured.and_then(|path| path.windows(2).position(|part| part == b";;"));
    if let (Some(configured), None) = (configured, insertion) {
        return package.set("path", lua.create_string(configured)?);
    }

    let original = package.get::<mlua::String>("path")?;
    let module = root.join("?.lua");
    let init = root.join("?/init.lua");
    let default = [
        module.as_os_str().as_encoded_bytes(),
        b";",
        init.as_os_str().as_encoded_bytes(),
        b";",
        original.as_bytes().as_ref(),
    ]
    .concat();
    let search_path = if let (Some(configured), Some(insertion)) = (configured, insertion) {
        let prefix = &configured[..insertion];
        let suffix = &configured[insertion + 2..];
        // Lua expands only the first double separator in its path environment variables.
        [
            prefix,
            if prefix.is_empty() { b"" } else { b";" },
            &default,
            if suffix.is_empty() { b"" } else { b";" },
            suffix,
        ]
        .concat()
    } else {
        default
    };
    package.set("path", lua.create_string(search_path)?)
}

#[cfg(test)]
pub(super) fn execute(text: &str) -> Result<ParsedConfig> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lua/mod.rs");
    ParsedConfig::execute_with_package_path(text, &path, None)
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &ResolveOptions<'_>) -> Result<Chain> {
    execute(text)?
        .resolve(options)
        .map(|resolved| resolved.chain)
}

#[cfg(test)]
mod tests;
