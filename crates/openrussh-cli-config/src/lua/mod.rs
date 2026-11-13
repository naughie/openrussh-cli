use super::{Error, ResolveConfig, ResolveOptions, Resolved, Result};

#[cfg(test)]
use openrussh_client::config::Chain;

use mlua::serde::Deserializer;
use mlua::{DeserializeOptions, Lua, Table, Value};
use serde::Deserialize as _;

use std::path::Path;

mod cli;
mod jumps;
mod paths;
mod profiles;
mod values;

use profiles::Profiles;

pub(super) struct ParsedConfig {
    profiles: Profiles,
}

impl ParsedConfig {
    pub(super) fn execute(text: &str, path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .map_err(|source| Error::CanonicalizeLua {
                path: path.to_owned(),
                source,
            })?;
        let profiles = {
            let lua = Lua::new();
            let name = format!("@{}", path.display());
            add_module_path(&lua, root_dir(&path)?)?;
            cli::preload(&lua, path)?;
            let value = lua.load(text).set_name(name).eval::<Value>()?;
            let options = DeserializeOptions::new()
                .encode_empty_tables_as_array(true)
                .detect_mixed_tables(true);
            Profiles::deserialize(Deserializer::new_with_options(value, options))?
        };

        Ok(Self { profiles })
    }
}

impl ResolveConfig for ParsedConfig {
    fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        self.profiles.resolve(options)
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

fn add_module_path(lua: &Lua, root: &Path) -> mlua::Result<()> {
    let package = lua.globals().get::<Table>("package")?;
    let original = package.get::<mlua::String>("path")?;
    let module = root.join("?.lua");
    let init = root.join("?/init.lua");
    let search_path = [
        module.as_os_str().as_encoded_bytes(),
        b";",
        init.as_os_str().as_encoded_bytes(),
        b";",
        original.as_bytes().as_ref(),
    ]
    .concat();
    package.set("path", lua.create_string(search_path)?)
}

#[cfg(test)]
pub(super) fn execute(text: &str) -> Result<ParsedConfig> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lua/mod.rs");
    ParsedConfig::execute(text, &path)
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &ResolveOptions<'_>) -> Result<Chain> {
    execute(text)?
        .resolve(options)
        .map(|resolved| resolved.chain)
}

#[cfg(test)]
mod tests;
