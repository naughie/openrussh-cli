use openrussh_client::config::{Chain, RequestTty};
use openrussh_client::russh::Preferred;

use anyhow::{Context as _, Result};

use std::path::{Path, PathBuf};

mod formats;
mod lua;
mod openssh;
mod toml;

pub(crate) mod algorithms;
use algorithms::Policies;

pub(crate) struct Config {
    pub chain: Chain,
    pub algorithms: Policies,
    pub meta: Meta,
    pub verbose: bool,
    pub session: Session,
    pub stdin_null: bool,
    pub escape_char: Option<u8>,
    pub local_forwards: Option<Vec<LocalForward>>,
    pub compression: bool,
    pub ciphers: Option<String>,
    pub macs: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Meta {
    pub known_hosts: Option<PathBuf>,
    pub log_level: Option<LogLevel>,
}

#[derive(Clone, Debug)]
pub(crate) enum LogLevel {
    None,
    Filter(Box<tracing_subscriber::EnvFilter>),
}

impl<'de> serde::Deserialize<'de> for LogLevel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use tracing_subscriber::{EnvFilter, filter::LevelFilter};

        let value = String::deserialize(deserializer)?;
        if value == "none" {
            return Ok(Self::None);
        }

        EnvFilter::builder()
            .with_default_directive(LevelFilter::ERROR.into())
            .parse(value)
            .map(|filter| Self::Filter(Box::new(filter)))
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug)]
pub(crate) struct LoadError {
    pub log_level: Option<LogLevel>,
    error: anyhow::Error,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

#[derive(Debug)]
pub(super) struct Resolved {
    pub chain: Chain,
    pub algorithms: Policies,
    pub meta: Meta,
}

impl From<Chain> for Resolved {
    fn from(chain: Chain) -> Self {
        Self {
            chain,
            algorithms: Policies::default(),
            meta: Meta::default(),
        }
    }
}

pub(crate) struct Options {
    pub destination: Destination,
    pub config_file: Option<PathBuf>,
    pub identities: Option<Vec<PathBuf>>,
    pub proxy_jump: Option<Vec<String>>,
    pub tty: Option<RequestTty>,
    pub verbose: bool,
    pub session: Session,
    pub stdin_null: bool,
    pub escape_char: Option<u8>,
    pub local_forwards: Option<Vec<LocalForward>>,
    pub compression: bool,
    pub ciphers: Option<String>,
    pub macs: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Session {
    Shell,
    Exec {
        prog: String,
        args: Option<Vec<String>>,
    },
    None,
    Forward {
        host: String,
        port: u16,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LocalForward {
    pub bind: LocalBind,
    pub target: ForwardTarget,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LocalBind {
    Tcp { host: BindHost, port: u16 },
    Unix(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BindHost {
    Loopback,
    All,
    Host(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForwardTarget {
    Tcp { host: String, port: u16 },
    Unix(String),
}

impl Options {
    pub fn load(self) -> Result<Config> {
        let Resolved {
            mut chain,
            algorithms,
            meta,
        } = self.resolve()?;

        if let Some(tty) = self.tty {
            chain.target.tty = tty;
        }

        Ok(Config {
            chain,
            algorithms,
            meta,
            verbose: self.verbose,
            session: self.session,
            stdin_null: self.stdin_null,
            escape_char: self.escape_char,
            local_forwards: self.local_forwards,
            compression: self.compression,
            ciphers: self.ciphers,
            macs: self.macs,
        })
    }

    fn resolve(&self) -> Result<Resolved> {
        if let Some(path) = &self.config_file {
            if path == Path::new("none") {
                return formats::defaults(self);
            }

            return self.read_config(
                std::fs::File::open(path)
                    .with_context(|| format!("Could not open SSH config {}", path.display()))?,
                path,
            );
        }

        let dir = std::env::home_dir()
            .context("Could not find the home directory")?
            .join(".ssh");
        self.default_config(&dir)
    }

    fn default_config(&self, dir: &Path) -> Result<Resolved> {
        use std::fs::File;
        use std::io::ErrorKind;

        for name in ["config.lua", "config.toml", "config"] {
            let path = dir.join(name);
            match File::open(&path) {
                Ok(f) => return self.read_config(f, &path),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("Could not open SSH config {}", path.display()));
                }
            }
        }

        formats::defaults(self)
    }

    fn read_config(&self, mut f: std::fs::File, path: &Path) -> Result<Resolved> {
        if path
            .extension()
            .is_some_and(|ext| ext == "toml" || ext == "lua")
        {
            use std::io::Read as _;

            let text = {
                let mut text = String::new();
                f.read_to_string(&mut text)
                    .with_context(|| format!("Could not read SSH config {}", path.display()))?;
                drop(f);
                text
            };

            let res = if path.extension().is_some_and(|ext| ext == "lua") {
                lua::ParsedConfig::execute(&text).and_then(|parsed| parsed.resolve(self))
            } else {
                toml::ParsedConfig::parse(&text).and_then(|parsed| parsed.resolve(self))
            };

            return res.with_context(|| format!("Could not read SSH config {}", path.display()));
        }

        openssh::ParsedConfig::parse(f, self)
            .and_then(|parsed| parsed.resolve(self))
            .with_context(|| format!("Could not read SSH config {}", path.display()))
    }
}

impl Config {
    pub fn preferred(&self) -> Result<Preferred> {
        use openrussh_client::russh::{cipher, compression, mac};

        use anyhow::anyhow;

        use std::borrow::Cow;

        let mut preferred = Preferred::default();

        if self.compression {
            preferred.compression = Cow::Borrowed(&[
                compression::ZLIB,
                compression::ZLIB_LEGACY,
                compression::NONE,
            ]);
        }

        if let Some(ciphers) = &self.ciphers {
            preferred.cipher = ciphers
                .split(',')
                .map(|s| cipher::Name::try_from(s).map_err(|()| anyhow!("Unknown cipher: {s}")))
                .collect::<Result<Vec<_>>>()?
                .into();
        }

        if let Some(macs) = &self.macs {
            preferred.mac = macs
                .split(',')
                .map(|s| mac::Name::try_from(s).map_err(|()| anyhow!("Unknown MAC: {s}")))
                .collect::<Result<Vec<_>>>()?
                .into();
        }

        Ok(preferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn config(text: &str) -> Result<(TempDir, Options)> {
        let dir = TempDir::new()?;
        let path = dir.path().join("config");
        std::fs::write(&path, text)?;

        Ok((
            dir,
            Options {
                destination: Destination {
                    host: "sample".to_owned(),
                    user: None,
                    port: None,
                },
                config_file: Some(path),
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
            },
        ))
    }

    #[test]
    fn native_log_levels_accept_both_spellings_and_preserve_omission() -> Result<()> {
        fn check(level: Option<LogLevel>, expected: Option<&str>) {
            match (level, expected) {
                (Some(LogLevel::None), None) => {}
                (Some(LogLevel::Filter(filter)), Some(expected)) => {
                    assert_eq!(filter.to_string(), expected);
                }
                (actual, expected) => {
                    panic!("Unexpected log filter: {actual:?}, expected {expected:?}")
                }
            }
        }

        let (_dir, options) = config("")?;

        for (name, expected) in [
            ("none", None),
            ("error", Some("error")),
            ("warn", Some("warn")),
            ("info", Some("info")),
            ("debug", Some("debug")),
            ("trace", Some("trace")),
            ("off", Some("off")),
            ("ERROR", Some("error")),
            ("", Some("error")),
            ("debug,russh=info", Some("russh=info,debug")),
            ("info,russh::client=debug", Some("russh::client=debug,info")),
            ("debug,russh=off", Some("russh=off,debug")),
            ("russh", Some("russh=trace")),
            ("russh::client=info", Some("russh::client=info")),
        ] {
            for field in ["log-level", "log_level"] {
                let source = format!("[meta]\n{field} = '{name}'");
                let resolved = toml::ParsedConfig::parse(&source)?.resolve(&options)?;
                check(resolved.meta.log_level, expected);

                let source = format!("return {{meta = {{['{field}'] = '{name}'}}}}");
                let resolved = lua::ParsedConfig::execute(&source)?.resolve(&options)?;
                check(resolved.meta.log_level, expected);
            }
        }

        for source in ["", "[meta]"] {
            assert!(
                toml::ParsedConfig::parse(source)?
                    .resolve(&options)?
                    .meta
                    .log_level
                    .is_none()
            );
        }
        for source in ["return {}", "return {meta = {log_level = nil}}"] {
            assert!(
                lua::ParsedConfig::execute(source)?
                    .resolve(&options)?
                    .meta
                    .log_level
                    .is_none()
            );
        }

        assert!(options.resolve()?.meta.log_level.is_none());
        assert!(formats::defaults(&options)?.meta.log_level.is_none());

        Ok(())
    }

    #[test]
    fn native_log_levels_reject_invalid_values_and_host_fields() -> Result<()> {
        for value in [
            "'russh=verbose'",
            "'debug,russh=nope'",
            "'russh=info=debug'",
            "1",
            "false",
            "[]",
            "{}",
        ] {
            let source = format!("[meta]\nlog-level = {value}");
            assert!(toml::ParsedConfig::parse(&source).is_err(), "{source}");
        }
        for value in [
            "'russh=verbose'",
            "'debug,russh=nope'",
            "'russh=info=debug'",
            "1",
            "false",
            "{}",
        ] {
            let source = format!("return {{meta = {{log_level = {value}}}}}");
            assert!(lua::ParsedConfig::execute(&source).is_err(), "{source}");
        }
        for table in ["default", "hosts.sample"] {
            let source = format!("[{table}]\nlog-level = 'debug'");
            assert!(toml::ParsedConfig::parse(&source).is_err(), "{source}");
        }
        for source in [
            "return {default = {log_level = 'debug'}}",
            "return {hosts = {sample = {log_level = 'debug'}}}",
        ] {
            assert!(lua::ParsedConfig::execute(source).is_err(), "{source}");
        }

        Ok(())
    }

    #[test]
    fn native_resolution_errors_retain_log_level_filename_and_cause() -> Result<()> {
        for (extension, source, cause) in [
            (
                "toml",
                "[meta]\nlog-level = 'none'\nknown-hosts = 'relative'",
                "known_hosts must be an absolute path: relative",
            ),
            (
                "toml",
                "[meta]\nlog-level = 'none'\n[hosts.sample]\nkex-algorithms = 'unknown'",
                "Unknown key-exchange algorithm: unknown",
            ),
            (
                "toml",
                "[meta]\nlog-level = 'none'\n[hosts.sample]\nproxy-jump = {host-name = 'jump'}",
                "TOML proxy-jump must be a string or an array of strings",
            ),
            (
                "lua",
                "return {meta = {log_level = 'none', known_hosts = 'relative'}}",
                "known_hosts must be an absolute path: relative",
            ),
            (
                "lua",
                "return {meta = {log_level = 'none'}, hosts = {sample = {identity_file = 'relative'}}}",
                "identity_file must be an absolute path: relative",
            ),
        ] {
            let (dir, mut options) = config(source)?;
            let path = dir.path().join(format!("config.{extension}"));
            std::fs::rename(options.config_file.as_ref().unwrap(), &path)?;
            options.config_file = Some(path.clone());

            let error = options
                .load()
                .err()
                .context("Expected resolution failure")?;
            assert!(matches!(
                error
                    .downcast_ref::<LoadError>()
                    .context("Missing log level on resolution error")?
                    .log_level,
                Some(LogLevel::None)
            ));
            assert_eq!(
                format!("{error:#}"),
                format!("Could not read SSH config {}: {cause}", path.display())
            );
        }

        Ok(())
    }

    #[test]
    fn native_algorithms_and_meta_resolve_independently_for_each_hop() -> Result<()> {
        use openrussh_client::russh::kex;
        use openrussh_client::russh::keys::ssh_key::Algorithm;

        let (_dir, options) = config("")?;
        let toml = toml::ParsedConfig::parse(
            r#"
[default]
host-name = 'same.example'
user = 'same'
identity-agent = '/sockets/agent'
kex-algorithms = 'diffie-hellman-group14-sha256'
host-key-algorithms = ['ssh-ed25519']
[hosts.sample]
kex-algorithms = ['curve25519-sha256']
proxy-jump = 'jump'
[hosts.jump]
[meta]
known-hosts = '~/.ssh/custom_known_hosts'
"#,
        )?;
        let lua = lua::ParsedConfig::execute(
            r#"
return {
    default = {
        host_name = 'same.example', user = 'same', identity_agent = '/sockets/agent',
        kex_algorithms = 'diffie-hellman-group14-sha256', host_key_algorithms = {'ssh-ed25519'},
    },
    hosts = {sample = {kex_algorithms = {'curve25519-sha256'}, proxy_jump = 'jump'}, jump = {}},
    meta = {known_hosts = os.getenv('HOME') .. '/.ssh/custom_known_hosts'},
}
"#,
        )?;
        let expected_path = std::env::home_dir()
            .unwrap()
            .join(".ssh/custom_known_hosts");

        for resolved in [toml.resolve(&options)?, lua.resolve(&options)?] {
            let jump = &resolved.chain.bastions.as_deref().unwrap()[0];
            assert_eq!(&resolved.chain.target, jump);
            assert_eq!(
                resolved.meta.known_hosts.as_deref(),
                Some(expected_path.as_path())
            );

            for (host, expected) in [
                (&resolved.chain.target, kex::CURVE25519),
                (jump, kex::DH_G14_SHA256),
            ] {
                let mut preferred = Preferred::default();
                resolved
                    .algorithms
                    .get(&resolved.chain, host)
                    .unwrap()
                    .apply(&mut preferred);

                assert_eq!(preferred.kex[0], expected);
                assert_eq!(preferred.key.as_ref(), &[Algorithm::Ed25519]);
                assert!(preferred.host_key_certificates.is_empty());
            }
            assert!(
                resolved
                    .algorithms
                    .get(&resolved.chain, &jump.clone())
                    .is_err()
            );
        }

        Ok(())
    }

    #[test]
    fn known_hosts_does_not_widen_an_explicit_host_key_list() -> Result<()> {
        use crate::handler::MyMakeHandler;
        use crate::known_hosts::KnownHostsFile;
        use openrussh_client::connect::MakeHandler as _;
        use openrussh_client::russh::keys::ssh_key::HashAlg;
        use openrussh_client::russh::keys::{Algorithm, PrivateKey};

        let (dir, options) = config("")?;
        let resolved = toml::ParsedConfig::parse(
            "[default]\nhost-name = 'server.example'\nhost-key-algorithms = 'rsa-sha2-512'",
        )?
        .resolve(&options)?;
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
        let path = dir.path().join("known_hosts");
        std::fs::write(
            &path,
            format!("server.example {}\n", key.public_key().to_openssh()?),
        )?;
        let maker = MyMakeHandler::new(Preferred::default(), KnownHostsFile::load(Some(&path))?)
            .with_algorithms(&resolved.chain, &resolved.algorithms);
        let (_, client) = maker
            .make_handler(&resolved.chain.target)
            .unwrap()
            .into_inner();

        assert_eq!(
            client.preferred.key.as_ref(),
            &[Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            }]
        );
        assert!(client.preferred.host_key_certificates.is_empty());
        assert!(maker.make_handler(&resolved.chain.target.clone()).is_err());

        Ok(())
    }

    #[test]
    fn lua_inline_hops_keep_distinct_policies_for_identical_connections() -> Result<()> {
        use openrussh_client::russh::kex;

        let (_dir, options) = config("")?;
        let resolved = lua::ParsedConfig::execute(
            r#"
return {
    default = {host_name = 'same.example', user = 'same'},
    hosts = {sample = {kex_algorithms = 'curve25519-sha256', proxy_jump = {
        {host_name = 'same.example'},
        {host_name = 'same.example', kex_algorithms = 'diffie-hellman-group14-sha256'},
        {host_name = 'same.example'},
        {host_name = 'same.example', kex_algorithms = 'diffie-hellman-group16-sha512'},
        {host_name = 'same.example'},
    }}},
}
"#,
        )?
        .resolve(&options)?;
        let hops = resolved.chain.bastions.as_deref().unwrap();

        assert!(hops.iter().all(|host| *host == resolved.chain.target));

        for (host, expected) in [
            (&resolved.chain.target, Some(kex::CURVE25519)),
            (&hops[0], None),
            (
                &hops[1],
                Some(kex::Name::try_from("diffie-hellman-group16-sha512").unwrap()),
            ),
            (&hops[2], None),
            (&hops[3], Some(kex::DH_G14_SHA256)),
            (&hops[4], None),
        ] {
            let mut preferred = Preferred::default();
            let policy = resolved.algorithms.get(&resolved.chain, host).unwrap();
            policy.apply(&mut preferred);
            if let Some(expected) = expected {
                assert_eq!(preferred.kex[0], expected);
            } else {
                assert!(policy.is_empty());
                assert_eq!(preferred.kex.as_ref(), Preferred::default().kex.as_ref());
            }
        }

        Ok(())
    }

    #[test]
    fn metadata_and_algorithm_validation_do_not_extend_openssh() -> Result<()> {
        let (_dir, options) = config("")?;
        for source in [
            "[meta]\nknown-hosts = 'relative'",
            "[meta]\nknown-hosts = ''",
            "[meta]\nknown-hosts = 1",
            "[meta]\nlog-level = 'russh=invalid'",
            "[default]\nknown-hosts = '/tmp/hosts'",
            "[hosts.unused]\nkex-algorithms = 'unknown'",
            "[hosts.unused]\nhost-key-algorithms = '-ssh-*'",
            "[default]\nkex-algorithms = []",
        ] {
            assert!(
                toml::ParsedConfig::parse(source)
                    .and_then(|p| p.resolve(&options))
                    .is_err(),
                "{source}"
            );
        }
        for source in [
            "return {meta = {known_hosts = '~/hosts'}}",
            "return {meta = {known_hosts = '$HOME/hosts'}}",
            "return {meta = {known_hosts = ''}}",
            "return {meta = {known_hosts = false}}",
            "return {meta = {log_level = 'russh=invalid'}}",
            "return {hosts = {unused = {meta = {known_hosts = '/tmp/hosts'}}}}",
            "return {hosts = {unused = {proxy_jump = {host_name = 'jump', kex_algorithms = 'unknown'}}}}",
        ] {
            assert!(
                lua::ParsedConfig::execute(source)
                    .and_then(|p| p.resolve(&options))
                    .is_err(),
                "{source}"
            );
        }

        let (_dir, options) = config(
            "Host sample\n KexAlgorithms diffie-hellman-group1-sha1\n HostKeyAlgorithms rsa-sha2-512\n UserKnownHostsFile /ignored/hosts\n",
        )?;
        let resolved = options.resolve()?;

        assert!(resolved.algorithms.target.is_empty());
        assert!(resolved.algorithms.bastions.is_none());
        assert!(resolved.meta.known_hosts.is_none());

        Ok(())
    }

    #[test]
    fn readme_examples_resolve_documented_host_settings() -> Result<()> {
        fn example<'src>(readme: &'src str, fence: &str) -> Result<&'src str> {
            let (_, code) = readme.split_once(fence).context("Missing README example")?;
            let (code, _) = code
                .split_once("\n```")
                .context("Unclosed README example")?;

            Ok(code)
        }

        let readme = include_str!("../../README.md");
        let toml = toml::ParsedConfig::parse(example(readme, "```toml\n")?)?;
        let lua = {
            let source = example(readme, "```lua\n")?.to_owned();
            lua::ParsedConfig::execute(&source)?
        };
        let (_dir, mut options) = config("")?;
        let identity = std::env::home_dir()
            .context("Missing home directory")?
            .join(".ssh/id_ed25519");

        for (alias, host, user, port, tty) in [
            ("github", "github.com", "git", 22, RequestTty::ForShell),
            ("my-host", "example.com", "alice", 2222, RequestTty::IfTty),
        ] {
            options.destination.host = alias.to_owned();
            let toml = toml.resolve(&options)?.chain;
            let lua = lua.resolve(&options)?.chain;

            assert_eq!(toml.target.dest.name, host);
            assert_eq!(toml.target.dest.user, user);
            assert_eq!(toml.target.dest.port, port);
            assert_eq!(
                toml.target.auth.identities.as_deref(),
                Some(std::slice::from_ref(&identity))
            );
            assert!(toml.target.auth.identities_only);
            assert_eq!(toml.target.tty, tty);
            assert!(toml.bastions.is_none());
            assert!(lua.bastions.is_none());

            let mut expected_lua = toml.target;
            expected_lua.auth.agent = None;
            assert_eq!(lua.target, expected_lua);
        }

        Ok(())
    }

    #[test]
    fn parsed_results_preserve_profiles_until_resolution() -> Result<()> {
        let (_dir, mut options) = config(
            "Host first\n User first-user\nHost second\n User second-user\nHost *\n User default-user\n IdentityAgent /sockets/agent.sock\n",
        )?;
        let openssh = openssh::ParsedConfig::parse(
            std::fs::File::open(options.config_file.as_ref().unwrap())?,
            &options,
        )?;
        let toml = toml::ParsedConfig::parse(
            "[default]\nuser = 'default-user'\nidentity-agent = '/sockets/agent.sock'\n[hosts.first]\nuser = 'first-user'\n[hosts.second]\nuser = 'second-user'\n",
        )?;
        let lua = {
            let source = String::from(
                "return {default = {user = 'default-user', identity_agent = '/sockets/agent.sock'}, hosts = {first = {user = 'first-user'}, second = {user = 'second-user'}}}",
            );
            lua::ParsedConfig::execute(&source)?
        };

        for (name, user) in [
            ("first", "first-user"),
            ("second", "second-user"),
            ("unlisted", "default-user"),
        ] {
            options.destination.host = name.to_owned();
            let expected = openssh.resolve(&options)?.chain.target;

            assert_eq!(expected.dest.user, user);
            assert_eq!(toml.resolve(&options)?.chain.target, expected);
            assert_eq!(lua.resolve(&options)?.chain.target, expected);
        }

        Ok(())
    }

    #[test]
    fn both_formats_construct_equivalent_application_configs() -> Result<()> {
        let (_openssh_dir, mut openssh) = config(
            "Host sample\n HostName target.example\n User file\n Port 2200\n IdentityFile /keys/host-key\n RequestTTY force\n ProxyJump bad\nHost jump\n HostName jump.example\n User jumper\n IdentityFile /keys/jump-key\n",
        )?;
        let (toml_dir, mut toml) = config(
            "[hosts.sample]\nhost-name = 'target.example'\nuser = 'file'\nport = 2200\nidentity-file = '/keys/host-key'\nrequest-tty = 'force'\nproxy-jump = 'bad'\n[hosts.jump]\nhost-name = 'jump.example'\nuser = 'jumper'\nidentity-file = '/keys/jump-key'\n",
        )?;
        let path = toml_dir.path().join("config.toml");
        std::fs::rename(toml.config_file.as_ref().unwrap(), &path)?;
        toml.config_file = Some(path);

        for options in [&mut openssh, &mut toml] {
            options.destination.user = Some("cli-user".to_owned());
            options.destination.port = Some(2222);
            options.identities = Some(vec!["cli-key".into()]);
            options.proxy_jump = Some(vec!["jump".to_owned()]);
            options.tty = Some(RequestTty::Never);
            options.verbose = true;
            options.session = Session::Exec {
                prog: "printf".to_owned(),
                args: Some(vec!["hello".to_owned()]),
            };
            options.stdin_null = true;
            options.escape_char = Some(b'!');
            options.compression = true;
            options.ciphers = Some("aes256-ctr".to_owned());
            options.macs = Some("hmac-sha2-256".to_owned());
            options.local_forwards = Some(vec![LocalForward {
                bind: LocalBind::Unix("local.sock".into()),
                target: ForwardTarget::Unix("remote.sock".to_owned()),
            }]);
        }

        let openssh = openssh.load()?;
        let toml = toml.load()?;

        assert_eq!(openssh.chain.target, toml.chain.target);
        assert_eq!(openssh.chain.bastions, toml.chain.bastions);
        assert!(openssh.verbose);
        assert!(toml.verbose);
        assert_eq!(openssh.session, toml.session);
        assert_eq!(openssh.stdin_null, toml.stdin_null);
        assert_eq!(openssh.escape_char, toml.escape_char);
        assert_eq!(openssh.local_forwards, toml.local_forwards);
        assert_eq!(openssh.compression, toml.compression);
        assert_eq!(openssh.ciphers, toml.ciphers);
        assert_eq!(openssh.macs, toml.macs);

        Ok(())
    }

    #[test]
    fn toml_extension_selects_the_parser_and_preserves_cli_tty() -> Result<()> {
        let (dir, mut conf) = config(
            "[default]\nuser = 'default'\n[hosts.sample]\nuser = 'alice'\nrequest-tty = 'force'\n",
        )?;
        let path = dir.path().join("alternate.toml");
        std::fs::rename(conf.config_file.as_ref().unwrap(), &path)?;
        conf.config_file = Some(path);

        assert_eq!(
            toml::query(
                &std::fs::read_to_string(conf.config_file.as_ref().unwrap())?,
                &conf
            )?
            .target
            .tty,
            RequestTty::Always
        );
        conf.tty = Some(RequestTty::Never);

        let resolved = conf.load()?;

        assert_eq!(resolved.chain.target.dest.user, "alice");
        assert_eq!(resolved.chain.target.tty, RequestTty::Never);

        Ok(())
    }

    #[test]
    fn default_config_prefers_toml_and_falls_back_only_when_missing() -> Result<()> {
        let (dir, conf) = config("Host sample\n User openssh\n")?;
        let path = dir.path().join("config.toml");

        assert_eq!(
            conf.default_config(dir.path())?.chain.target.dest.user,
            "openssh"
        );
        std::fs::write(&path, "[default]\nuser = 'toml'\n")?;

        assert_eq!(
            conf.default_config(dir.path())?.chain.target.dest.user,
            "toml"
        );
        std::fs::write(&path, "[broken")?;

        assert!(conf.default_config(dir.path()).is_err());
        std::fs::remove_file(&path)?;
        std::fs::create_dir(&path)?;

        assert!(conf.default_config(dir.path()).is_err());
        std::fs::remove_dir(&path)?;

        assert_eq!(
            conf.default_config(dir.path())?.chain.target.dest.user,
            "openssh"
        );

        Ok(())
    }

    #[test]
    fn absent_default_configs_keep_command_line_settings() -> Result<()> {
        let (dir, mut conf) = config("")?;
        std::fs::remove_file(conf.config_file.as_ref().unwrap())?;
        conf.destination.user = Some("alice".to_owned());
        conf.destination.port = Some(2200);

        let chain = conf.default_config(dir.path())?.chain;

        assert_eq!(chain.target.dest.user, "alice");
        assert_eq!(chain.target.dest.port, 2200);

        Ok(())
    }

    #[test]
    fn command_line_values_precede_file_values() -> Result<()> {
        let (_dir, mut conf) = config(
            "Host sample\n User configured\n Port 2200\n IdentityFile /keys/configured\n RequestTTY force\n",
        )?;
        conf.destination.user = Some("alice".to_owned());
        conf.destination.port = Some(2222);
        conf.identities = Some(vec!["/keys/first".into(), "/keys/second".into()]);
        conf.tty = Some(RequestTty::Never);

        let chain = conf.load()?.chain;

        assert_eq!(chain.target.dest.user, "alice");

        assert_eq!(chain.target.dest.port, 2222);

        assert_eq!(
            chain.target.auth.identities,
            Some(vec![
                "/keys/first".into(),
                "/keys/second".into(),
                "/keys/configured".into()
            ])
        );

        assert_eq!(chain.target.tty, RequestTty::Never);

        Ok(())
    }

    #[test]
    fn includes_keep_their_host_scope_with_cli_overrides() -> Result<()> {
        let (dir, mut conf) = config("")?;
        let included = dir.path().join("included");
        std::fs::write(
            &included,
            "Port 2200\nIdentityFile /keys/included\nHost *\n User scoped\n",
        )?;
        std::fs::write(
            conf.config_file.as_ref().unwrap(),
            format!(
                "Host sample\n Include {}\nHost other\n Port 2300\nHost *\n User default\n",
                included.display()
            ),
        )?;
        conf.identities = Some(vec!["/keys/cli".into()]);

        let chain = conf.resolve()?.chain;

        assert_eq!(chain.target.dest.user, "scoped");

        assert_eq!(chain.target.dest.port, 2200);

        assert_eq!(
            chain.target.auth.identities,
            Some(vec!["/keys/cli".into(), "/keys/included".into()])
        );
        conf.destination.host = "other".to_owned();

        let chain = conf.resolve()?.chain;

        assert_eq!(chain.target.dest.user, "default");

        assert_eq!(chain.target.dest.port, 2300);

        assert_eq!(chain.target.auth.identities, Some(vec!["/keys/cli".into()]));

        Ok(())
    }

    #[test]
    fn explicit_jump_chain_replaces_the_file_chain() -> Result<()> {
        let (_dir, mut conf) = config(
            "Host sample\n User target\n ProxyJump first\nHost first\n User first-user\nHost second\n User second-user\n",
        )?;
        conf.proxy_jump = Some(vec!["second".to_owned()]);

        let chain = conf.resolve()?.chain;
        let bastions = chain.bastions.context("Missing jump host")?;

        assert_eq!(bastions.len(), 1);

        assert_eq!(bastions[0].dest.name, "second");

        assert_eq!(bastions[0].dest.user, "second-user");

        Ok(())
    }

    #[test]
    fn config_none_keeps_cli_settings_without_opening_a_file() -> Result<()> {
        let (_dir, mut conf) = config("Port invalid")?;
        conf.config_file = Some("none".into());
        conf.destination.user = Some("alice".to_owned());
        conf.destination.port = Some(2200);
        conf.identities = Some(vec!["/keys/not-created".into()]);

        let chain = conf.resolve()?.chain;

        assert_eq!(chain.target.dest.name, "sample");

        assert_eq!(chain.target.dest.user, "alice");

        assert_eq!(chain.target.dest.port, 2200);

        assert_eq!(
            chain.target.auth.identities,
            Some(vec!["/keys/not-created".into()])
        );

        Ok(())
    }

    #[test]
    fn explicit_missing_and_malformed_config_errors_are_preserved() -> Result<()> {
        let (dir, mut conf) = config("Host sample\n Port invalid\n")?;
        let e = conf.resolve().unwrap_err();

        assert!(format!("{e:#}").contains("Could not read SSH config"));
        conf.config_file = Some(dir.path().join("missing"));

        let e = conf.resolve().unwrap_err();

        assert!(format!("{e:#}").contains("Could not open SSH config"));

        Ok(())
    }

    #[test]
    fn transport_settings_in_the_file_do_not_extend_library_support() -> Result<()> {
        let (_dir, conf) = config(
            "Host sample\n User alice\n Compression yes\n Ciphers aes128-ctr\n MACs hmac-sha2-256\n LocalForward 8080 localhost:80\n",
        )?;
        let conf = conf.load()?;

        let preferred = conf.preferred()?;
        let default = Preferred::default();

        assert_eq!(preferred.compression, default.compression);

        assert_eq!(preferred.cipher, default.cipher);

        assert_eq!(preferred.mac, default.mac);

        assert!(conf.local_forwards.is_none());

        Ok(())
    }

    #[test]
    fn command_line_algorithms_keep_the_requested_order() -> Result<()> {
        use openrussh_client::russh::{cipher, compression, mac};

        let (_dir, conf) = config("")?;
        let mut conf = conf.load()?;
        conf.compression = true;
        conf.ciphers = Some("aes256-ctr,aes128-ctr".to_owned());
        conf.macs = Some("hmac-sha2-512,hmac-sha2-256".to_owned());

        let preferred = conf.preferred()?;

        assert_eq!(
            &*preferred.compression,
            &[
                compression::ZLIB,
                compression::ZLIB_LEGACY,
                compression::NONE
            ]
        );

        assert_eq!(
            &*preferred.cipher,
            &[cipher::AES_256_CTR, cipher::AES_128_CTR]
        );

        assert_eq!(&*preferred.mac, &[mac::HMAC_SHA512, mac::HMAC_SHA256]);

        Ok(())
    }

    #[test]
    fn invalid_algorithm_names_are_rejected() -> Result<()> {
        let (_dir, conf) = config("")?;
        let mut conf = conf.load()?;

        for s in ["", "unknown", "aes128-ctr,", ",aes128-ctr"] {
            conf.ciphers = Some(s.to_owned());

            assert!(conf.preferred().is_err(), "{s}");
        }
        conf.ciphers = None;

        for s in ["", "unknown", "hmac-sha2-256,", ",hmac-sha2-256"] {
            conf.macs = Some(s.to_owned());

            assert!(conf.preferred().is_err(), "{s}");
        }

        Ok(())
    }
}
