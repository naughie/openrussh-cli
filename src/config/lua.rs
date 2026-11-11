use super::Options;
use super::formats::{PathMode, Profiles};

use openrussh_client::config::Chain;

use mlua::serde::Deserializer;
use mlua::{DeserializeOptions, Lua, Value};
use serde::Deserialize as _;

use anyhow::Result;

pub(super) struct ParsedConfig {
    profiles: Profiles<'static>,
}

impl ParsedConfig {
    pub(super) fn execute(text: &str) -> Result<Self> {
        let profiles = {
            let lua = Lua::new();
            let value = lua.load(text).set_name("config.lua").eval::<Value>()?;
            let options = DeserializeOptions::new()
                .encode_empty_tables_as_array(true)
                .detect_mixed_tables(true);
            Profiles::deserialize(Deserializer::new_with_options(value, options))?
        };

        Ok(Self { profiles })
    }

    pub(super) fn resolve(&self, options: &Options) -> Result<Chain> {
        self.profiles.resolve(options, PathMode::Literal)
    }
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &Options) -> Result<Chain> {
    ParsedConfig::execute(text)?.resolve(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Destination, Session};

    fn options(host: &str) -> Options {
        Options {
            destination: Destination {
                host: host.to_owned(),
                user: None,
                port: None,
            },
            config_file: None,
            identities: None,
            proxy_jump: None,
            tty: None,
            session: Session::Shell,
            stdin_null: false,
            escape_char: Some(b'~'),
            local_forwards: None,
            compression: false,
            ciphers: None,
            macs: None,
        }
    }

    #[test]
    fn lua_and_toml_resolve_equivalent_native_configs() -> Result<()> {
        let lua = r#"
assert(_VERSION == "Lua 5.4")
local home = assert(os.getenv("HOME"))
local prefix = home .. "/.ssh/"
local function identity(name) return prefix .. name end
return {
    defaults = {
        user = "default", identities_only = "yes", identity_agent = home .. "/agent.sock",
        identity_file = identity("default"), request_tty = "yes",
    },
    hosts = {
        ["my-host"] = {
            host_name = "example.com", port = "2222", user = "alice",
            identity_file = {identity("first"), identity("second")},
            certificate_file = identity("certificate"), proxy_jump = {"Jump"},
        },
        Jump = { host_name = "jump.example", port = 2200 },
    },
}
"#;
        let toml = r#"
[default]
user = "default"
identities-only = "yes"
identity-agent = "~/agent.sock"
identity-file = "~/.ssh/default"
request-tty = "yes"
[hosts.my-host]
host-name = "example.com"
port = "2222"
user = "alice"
identity-file = ["~/.ssh/first", "~/.ssh/second"]
certificate-file = "~/.ssh/certificate"
proxy-jump = ["Jump"]
[hosts.Jump]
host-name = "jump.example"
port = 2200
"#;
        let conf = options("my-host");
        let lua = query(lua, &conf)?;
        let toml = super::super::toml::query(toml, &conf)?;

        assert_eq!(lua.target, toml.target);
        assert_eq!(lua.bastions, toml.bastions);

        Ok(())
    }

    #[test]
    fn empty_tables_preserve_defaults_and_can_clear_jump_lists() -> Result<()> {
        use openrussh_client::config::{Auth, Dest, Host, RequestTty};

        let conf = options("sample");
        let empty = query("return {}", &conf)?;

        assert_eq!(
            empty.target,
            Host {
                dest: Dest {
                    name: "sample".to_owned(),
                    user: whoami::username()?,
                    port: 22,
                },
                auth: Auth::default(),
                tty: RequestTty::default(),
            }
        );
        assert!(empty.bastions.is_none());

        let chain = query(
            "return {defaults = {proxy_jump = 'jump'}, hosts = {sample = {proxy_jump = {}, identity_file = {}}}}",
            &conf,
        )?;

        assert!(chain.bastions.is_none());
        assert_eq!(chain.target.auth.identities, Some(Vec::new()));

        Ok(())
    }

    #[test]
    fn agents_require_explicit_lua_values() -> Result<()> {
        use anyhow::ensure;

        use std::path::Path;
        use std::process::Command;

        const CASE: &str = "OPENRUSSH_LUA_AGENT_TEST";

        if std::env::var_os(CASE).is_none() {
            for agent in [
                None,
                Some("/sockets/agent.sock"),
                Some("relative-agent.sock"),
            ] {
                let mut cmd = Command::new(std::env::current_exe()?);
                cmd.args([
                    "--exact",
                    "config::lua::tests::agents_require_explicit_lua_values",
                    "--nocapture",
                ])
                .env(CASE, "1");

                if let Some(agent) = agent {
                    cmd.env("SSH_AUTH_SOCK", agent);
                } else {
                    cmd.env_remove("SSH_AUTH_SOCK");
                }

                let output = cmd.output()?;
                ensure!(
                    output.status.success(),
                    "Lua agent test failed with SSH_AUTH_SOCK={agent:?}:\n{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            return Ok(());
        }

        let conf = options("sample");
        for source in [
            "return {}",
            "return {defaults = {identity_agent = nil}, hosts = {sample = {}}}",
            "return {hosts = {sample = {proxy_jump = 'jump'}, jump = {}}}",
        ] {
            let chain = query(source, &conf)?;

            assert!(chain.target.auth.agent.is_none());
            for jump in chain.bastions.as_deref().unwrap_or_default() {
                assert!(jump.auth.agent.is_none());
            }
        }

        let explicit = query(
            "return {defaults = {identity_agent = os.getenv('SSH_AUTH_SOCK')}, hosts = {sample = {proxy_jump = 'jump'}}}",
            &conf,
        );
        let agent = std::env::var_os("SSH_AUTH_SOCK");

        if agent
            .as_deref()
            .is_some_and(|s| !Path::new(s).is_absolute())
        {
            assert!(explicit.is_err());
        } else {
            let chain = explicit?;
            let expected = agent.as_deref().map(Path::new);

            assert_eq!(chain.target.auth.agent.as_deref(), expected);
            assert_eq!(chain.bastions.unwrap()[0].auth.agent.as_deref(), expected);
        }

        let chain = query(
            "return {defaults = {identity_agent = '/sockets/explicit.sock'}, hosts = {sample = {proxy_jump = 'jump'}}}",
            &conf,
        )?;
        let expected = Some(Path::new("/sockets/explicit.sock"));

        assert_eq!(chain.target.auth.agent.as_deref(), expected);
        assert_eq!(chain.bastions.unwrap()[0].auth.agent.as_deref(), expected);

        Ok(())
    }

    #[test]
    fn lua_and_toml_accept_the_same_boolean_and_tty_values() -> Result<()> {
        use openrussh_client::config::RequestTty::{Always, ForShell, IfTty, Never};

        for (value, enabled, tty) in [
            ("true", true, IfTty),
            ("false", false, Never),
            ("'true'", true, IfTty),
            ("'false'", false, Never),
            ("'yes'", true, IfTty),
            ("'no'", false, Never),
        ] {
            let conf = options("sample");
            let lua = query(
                &format!(
                    "return {{default = {{identity_agent = '/sockets/agent.sock', identities_only = {value}, request_tty = {value}}}}}"
                ),
                &conf,
            )?;
            let toml = super::super::toml::query(
                &format!(
                    "[default]\nidentity-agent = '/sockets/agent.sock'\nidentities-only = {value}\nrequest-tty = {value}"
                ),
                &conf,
            )?;

            assert!(lua.target.auth.identities_only == enabled);
            assert_eq!(lua.target.tty, tty);
            assert_eq!(lua.target, toml.target);
        }

        for (value, expected) in [("auto", ForShell), ("force", Always)] {
            assert_eq!(
                query(
                    &format!("return {{defaults = {{request_tty = '{value}'}}}}"),
                    &options("sample")
                )?
                .target
                .tty,
                expected
            );
        }

        Ok(())
    }

    #[test]
    fn invalid_return_values_and_fields_are_rejected() {
        for text in [
            "return",
            "return false",
            "return 22",
            "return 'text'",
            "return function() end",
            "return {defaults = {identities_only = 1}}",
            "return {defaults = {port = 65536}}",
            "return {defaults = {port = 22.5}}",
            "return {defaults = {identity_file = {1}}}",
            "return {defaults = {identity_file = {'key', extra = 'ignored'}}}",
            "return {hosts = {unused = {include = 'other'}}}",
        ] {
            assert!(query(text, &options("sample")).is_err(), "{text}");
        }
    }

    #[test]
    fn lua_syntax_and_runtime_errors_are_reported() {
        for (text, expected) in [
            ("return {", "config.lua"),
            ("error('configuration failed')", "configuration failed"),
        ] {
            let error = query(text, &options("sample")).unwrap_err();

            assert!(format!("{error:#}").contains(expected));
        }
    }
}
