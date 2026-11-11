use super::formats::{PathMode, Profiles};
use super::{Options, Resolved};

#[cfg(test)]
use openrussh_client::config::Chain;

use mlua::serde::Deserializer;
use mlua::{DeserializeOptions, Lua, Value};
use serde::Deserialize as _;

use anyhow::{Context as _, Result};

use std::path::Path;

mod cli;

pub(super) struct ParsedConfig {
    profiles: Profiles<'static>,
}

impl ParsedConfig {
    pub(super) fn execute(text: &str, path: &Path) -> Result<Self> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Could not canonicalize Lua config {}", path.display()))?;
        let profiles = {
            let lua = Lua::new();
            let name = format!("@{}", path.display());
            cli::preload(&lua, path)?;
            let value = lua.load(text).set_name(name).eval::<Value>()?;
            let options = DeserializeOptions::new()
                .encode_empty_tables_as_array(true)
                .detect_mixed_tables(true);
            Profiles::deserialize(Deserializer::new_with_options(value, options))?
        };

        Ok(Self { profiles })
    }

    pub(super) fn resolve(&self, options: &Options) -> Result<Resolved> {
        self.profiles.resolve(options, PathMode::Literal)
    }
}

#[cfg(test)]
pub(super) fn execute(text: &str) -> Result<ParsedConfig> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file!());
    ParsedConfig::execute(text, &path)
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &Options) -> Result<Chain> {
    execute(text)?
        .resolve(options)
        .map(|resolved| resolved.chain)
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
            verbose: false,
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
    fn source_paths_are_canonical_and_outlive_the_lua_state() -> Result<()> {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir()?;
        let real = dir.path().join("real");
        std::fs::create_dir(&real)?;
        let path = real.join("config.lua");
        let source = String::from(
            "local cli = require('cli'); return {hosts = {sample = {host_name = cli.__file__, user = cli.__dir__}}}",
        );
        std::fs::write(&path, &source)?;
        symlink(&path, dir.path().join("alias.lua"))?;
        let canonical = path.canonicalize()?;
        let parsed = ParsedConfig::execute(&source, &real.join("../alias.lua"))?;
        drop(source);
        let host = parsed.resolve(&options("sample"))?.chain.target;

        assert_eq!(host.dest.name, canonical.to_str().unwrap());
        assert_eq!(
            host.dest.user,
            canonical.parent().unwrap().to_str().unwrap()
        );

        Ok(())
    }

    #[test]
    fn source_path_failures_and_lua_errors_identify_the_file() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("actual-config.lua");
        let error = match ParsedConfig::execute("error('must not execute')", &path) {
            Ok(_) => panic!("missing configuration path should fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("canonicalize"));
        assert!(error.to_string().contains(path.to_str().unwrap()));

        std::fs::write(&path, "error('configuration failed')")?;
        let error = match ParsedConfig::execute("error('configuration failed')", &path) {
            Ok(_) => panic!("Lua execution should fail"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains(path.canonicalize()?.to_str().unwrap())
        );

        Ok(())
    }

    #[test]
    fn jump_values_accept_names_records_and_mixed_lists() -> Result<()> {
        for (jumps, expected) in [
            ("'a'", &["alias-a"][..]),
            ("'a,b'", &["alias-b", "alias-a"][..]),
            ("{'a', 'b'}", &["alias-b", "alias-a"][..]),
            ("{host_name = 'inline'}", &["inline"][..]),
            (
                "{{host_name = 'one'}, {host_name = 'two'}, 'c'}",
                &["alias-c", "two", "one"][..],
            ),
            ("{'a,b'}", &["literal-comma"][..]),
        ] {
            let source = format!(
                "return {{hosts = {{target = {{proxy_jump = {jumps}}}, a = {{host_name = 'alias-a'}}, b = {{host_name = 'alias-b'}}, c = {{host_name = 'alias-c'}}, ['a,b'] = {{host_name = 'literal-comma'}}}}}}"
            );
            let chain = query(&source, &options("target"))?;
            let actual: Vec<_> = chain
                .bastions
                .as_deref()
                .unwrap()
                .iter()
                .map(|host| host.dest.name.as_str())
                .collect();

            assert_eq!(actual, expected, "{jumps}");
        }

        Ok(())
    }

    #[test]
    fn inline_hosts_resolve_their_own_fields_without_alias_lookup() -> Result<()> {
        use openrussh_client::config::{Auth, Dest, Host, RequestTty};

        let source = r#"
return {
    default = {
        user = 'default-user', port = 2222, identity_file = '/keys/default',
        certificate_file = '/keys/default-cert', identity_agent = '/sockets/default',
        identities_only = true, request_tty = 'yes',
    },
    hosts = {
        target = {
            host_name = 'target.example',
            proxy_jump = {
                host_name = 'bastion', user = 'me', port = '2200',
                identity_file = '/keys/inline', certificate_file = '/keys/inline-cert',
                identity_agent = '/sockets/inline', identities_only = false, request_tty = 'force',
            },
        },
        bastion = { host_name = 'wrong.example', user = 'wrong', port = 1 },
    },
}
"#;
        let mut conf = options("target");
        conf.destination.user = Some("cli-user".to_owned());
        conf.destination.port = Some(9999);
        conf.identities = Some(vec!["/keys/cli".into()]);
        let chain = query(source, &conf)?;

        assert_eq!(chain.target.dest.user, "cli-user");
        assert_eq!(chain.target.dest.port, 9999);
        assert_eq!(
            chain.bastions.as_deref().unwrap(),
            &[Host {
                dest: Dest {
                    name: "bastion".to_owned(),
                    user: "me".to_owned(),
                    port: 2200
                },
                auth: Auth {
                    identities: Some(vec!["/keys/inline".into(), "/keys/default".into()]),
                    cert: Some("/keys/inline-cert".into()),
                    agent: Some("/sockets/inline".into()),
                    identities_only: false,
                    ..Auth::default()
                },
                tty: RequestTty::Always,
            }]
        );

        Ok(())
    }

    #[test]
    fn inline_hosts_inherit_defaults_and_empty_entries_are_records() -> Result<()> {
        for jump in ["{user = 'me'}", "{{}}"] {
            let source = format!(
                "return {{default = {{host_name = 'default.example', user = 'me', port = 2200, identity_file = '/keys/default'}}, hosts = {{target = {{host_name = 'target.example', proxy_jump = {jump}}}}}}}"
            );
            let chain = query(&source, &options("target"))?;
            let jump = &chain.bastions.as_deref().unwrap()[0];

            assert_eq!(jump.dest.name, "default.example");
            assert_eq!(jump.dest.user, "me");
            assert_eq!(jump.dest.port, 2200);
            assert_eq!(
                jump.auth.identities.as_deref(),
                Some(["/keys/default".into()].as_slice())
            );
            assert!(jump.auth.agent.is_none());
        }

        Ok(())
    }

    #[test]
    fn mixed_jumps_recurse_only_through_the_first_entry() -> Result<()> {
        let chain = query(
            r#"
return {
    hosts = {
        target = {
            proxy_jump = {
                {host_name = 'one', proxy_jump = {host_name = 'outer', proxy_jump = 'alias'}},
                {host_name = 'two', proxy_jump = 'ignored'},
                'last',
            },
        },
        alias = {host_name = 'alias.example'},
        last = {host_name = 'last.example'},
    },
}
"#,
            &options("target"),
        )?;
        let actual: Vec<_> = chain
            .bastions
            .as_deref()
            .unwrap()
            .iter()
            .map(|host| host.dest.name.as_str())
            .collect();

        assert_eq!(
            actual,
            ["last.example", "two", "one", "outer", "alias.example"]
        );

        Ok(())
    }

    #[test]
    fn cli_jumps_replace_inline_jumps() -> Result<()> {
        let source = "return {hosts = {target = {proxy_jump = {user = 'missing-host-name'}}, replacement = {host_name = 'replacement.example'}}}";
        let mut conf = options("target");
        conf.proxy_jump = Some(Vec::new());

        assert!(query(source, &conf)?.bastions.is_none());

        conf.proxy_jump = Some(vec!["replacement".to_owned()]);
        let chain = query(source, &conf)?;

        assert_eq!(
            chain.bastions.as_deref().unwrap()[0].dest.name,
            "replacement.example"
        );

        Ok(())
    }

    #[test]
    fn invalid_inline_hosts_are_rejected_including_nested_unused_records() {
        for source in [
            "return {hosts = {target = {proxy_jump = {user = 'missing-host-name'}}}}",
            "return {hosts = {target = {proxy_jump = {{}}}}}",
            "return {hosts = {target = {proxy_jump = {host_name = 'jump', unknown = true}}}}",
            "return {hosts = {target = {proxy_jump = {host_name = 'jump', port = 65536}}}}",
            "return {hosts = {target = {proxy_jump = {true}}}}",
            "return {hosts = {target = {proxy_jump = {{{host_name = 'jump'}}}}}}",
            "return {hosts = {target = {proxy_jump = {host_name = 'jump', [1] = 'mixed'}}}}",
            "return {hosts = {unused = {proxy_jump = {host_name = 'jump', identity_file = ''}}}}",
            "return {hosts = {target = {proxy_jump = {'first', {host_name = 'second', proxy_jump = {host_name = 'unused', certificate_file = 'relative'}}}}}}",
            "return {hosts = {unused = {proxy_jump = {host_name = 'jump', identity_agent = '$SSH_AUTH_SOCK'}}}}",
            "local jump = {host_name = 'jump'}; jump.proxy_jump = jump; return {hosts = {target = {proxy_jump = jump}}}",
        ] {
            assert!(query(source, &options("target")).is_err(), "{source}");
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
    default = {
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
            "return {default = {proxy_jump = 'jump'}, hosts = {sample = {proxy_jump = {}, identity_file = {}}}}",
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
            "return {default = {identity_agent = nil}, hosts = {sample = {}}}",
            "return {hosts = {sample = {proxy_jump = 'jump'}, jump = {}}}",
        ] {
            let chain = query(source, &conf)?;

            assert!(chain.target.auth.agent.is_none());
            for jump in chain.bastions.as_deref().unwrap_or_default() {
                assert!(jump.auth.agent.is_none());
            }
        }

        let explicit = query(
            "return {default = {identity_agent = os.getenv('SSH_AUTH_SOCK')}, hosts = {sample = {proxy_jump = 'jump'}}}",
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
            "return {default = {identity_agent = '/sockets/explicit.sock'}, hosts = {sample = {proxy_jump = 'jump'}}}",
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
                    &format!("return {{default = {{request_tty = '{value}'}}}}"),
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
            "return {default = {identities_only = 1}}",
            "return {default = {port = 65536}}",
            "return {default = {port = 22.5}}",
            "return {default = {identity_file = {1}}}",
            "return {default = {identity_file = {'key', extra = 'ignored'}}}",
            "return {hosts = {unused = {include = 'other'}}}",
        ] {
            assert!(query(text, &options("sample")).is_err(), "{text}");
        }
    }

    #[test]
    fn lua_syntax_and_runtime_errors_are_reported() {
        for (text, expected) in [
            ("return {", "mod.rs"),
            ("error('configuration failed')", "configuration failed"),
        ] {
            let error = query(text, &options("sample")).unwrap_err();

            assert!(format!("{error:#}").contains(expected));
        }
    }
}
