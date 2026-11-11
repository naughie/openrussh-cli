use super::Options;

use openrussh_client::config::{Auth, Chain, Dest, Host, RequestTty, Target};

use anyhow::{Result, ensure};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod values;
use values::{Strings, Text};

mod paths;

mod jumps;
use jumps::{Jump, Jumps};

#[derive(Clone, Copy)]
pub(super) enum PathMode {
    Expand,
    Literal,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Profiles<'src> {
    #[serde(rename = "default", alias = "defaults", borrow)]
    defaults: HostFields<'src>,
    #[serde(borrow)]
    hosts: BTreeMap<Text<'src>, HostFields<'src>>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
struct HostFields<'src> {
    #[serde(borrow, alias = "host_name")]
    host_name: Option<Text<'src>>,
    #[serde(borrow)]
    user: Option<Text<'src>>,
    #[serde(deserialize_with = "values::deserialize_port")]
    port: Option<u16>,
    #[serde(borrow, alias = "identity_file")]
    identity_file: Option<Strings<'src>>,
    #[serde(borrow, alias = "certificate_file")]
    certificate_file: Option<Text<'src>>,
    #[serde(borrow, alias = "identity_agent")]
    identity_agent: Option<Text<'src>>,
    #[serde(
        alias = "identities_only",
        deserialize_with = "values::deserialize_bool"
    )]
    identities_only: Option<bool>,
    #[serde(alias = "request_tty", deserialize_with = "values::deserialize_tty")]
    request_tty: Option<RequestTty>,
    #[serde(borrow, alias = "proxy_jump")]
    proxy_jump: Option<Jumps<'src>>,
}

pub(super) fn defaults(conf: &Options) -> Result<Chain> {
    Profiles::default().resolve(conf, PathMode::Expand)
}

impl Profiles<'_> {
    pub(super) fn require_named_jumps(&self) -> Result<()> {
        for host in std::iter::once(&self.defaults).chain(self.hosts.values()) {
            ensure!(
                host.proxy_jump.as_ref().is_none_or(|jumps| jumps
                    .as_slice()
                    .iter()
                    .all(|j| matches!(j, Jump::Name(_)))),
                "TOML proxy-jump must be a string or an array of strings"
            );
        }

        Ok(())
    }

    pub(super) fn resolve(&self, conf: &Options, paths: PathMode) -> Result<Chain> {
        for host in std::iter::once(&self.defaults).chain(self.hosts.values()) {
            host.validate_paths(paths)?;
        }

        let doc = self;
        let target = HostRef::Named(Target {
            host: &conf.destination.host,
            user: conf.destination.user.as_deref(),
            port: conf.destination.port,
        });
        let target_host = resolve_host(target, doc, Some(conf), paths)?;
        let mut pending = jumps(target, doc, Some(conf));
        let mut bastions: Option<Vec<Host>> = None;

        while let Some(first) = pending.next() {
            let chain = bastions.get_or_insert_with(Vec::new);
            chain.reserve(pending.size_hint().0 + 1);

            for jump in pending.rev() {
                chain.push(resolve_host(jump, doc, None, paths)?);
            }

            chain.push(resolve_host(first, doc, None, paths)?);
            pending = jumps(first, doc, None);
        }

        Ok(Chain {
            target: target_host,
            bastions: bastions.map(Vec::into_boxed_slice),
        })
    }
}

impl HostFields<'_> {
    fn validate_paths(&self, paths: PathMode) -> Result<()> {
        for path in self
            .identity_file
            .as_ref()
            .into_iter()
            .flat_map(Strings::as_slice)
        {
            paths.validate(Path::new(path.as_str()), "identity_file")?;
        }
        if let Some(path) = &self.certificate_file {
            paths.validate(Path::new(path.as_str()), "certificate_file")?;
        }
        if let Some(value) = &self.identity_agent {
            paths.validate_agent(value.as_str())?;
        }

        for jump in self
            .proxy_jump
            .as_ref()
            .into_iter()
            .flat_map(Jumps::as_slice)
        {
            if let Jump::Host(host) = jump {
                host.validate_paths(paths)?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum HostRef<'a, 'src> {
    Named(Target<'a>),
    Inline(&'a HostFields<'src>),
}

impl<'a, 'src> HostRef<'a, 'src> {
    fn fields(self, doc: &'a Profiles<'src>) -> Option<&'a HostFields<'src>> {
        match self {
            Self::Named(target) => doc.hosts.get(target.host),
            Self::Inline(host) => Some(host),
        }
    }

    fn fallback(self) -> Option<Target<'a>> {
        match self {
            Self::Named(target) => Some(target),
            Self::Inline(_) => None,
        }
    }
}

fn resolve_host<'a, 'src>(
    target: HostRef<'a, 'src>,
    doc: &'a Profiles<'src>,
    cli: Option<&Options>,
    paths: PathMode,
) -> Result<Host> {
    use anyhow::Context as _;

    let empty = HostFields::default();
    let host = target.fields(doc).unwrap_or(&empty);
    let defaults = &doc.defaults;
    let fallback = target.fallback();

    let name = host
        .host_name
        .as_ref()
        .or(defaults.host_name.as_ref())
        .map(Text::as_str)
        .or_else(|| fallback.map(|t| t.host))
        .context("Inline proxy_jump host requires host_name")?
        .to_owned();
    let user = if let Some(user) = cli
        .and_then(|c| c.destination.user.as_deref())
        .or_else(|| {
            host.user
                .as_ref()
                .or(defaults.user.as_ref())
                .map(Text::as_str)
        })
        .or_else(|| fallback.and_then(|t| t.user))
    {
        user.to_owned()
    } else {
        whoami::username()?
    };
    let port = cli
        .and_then(|c| c.destination.port)
        .or(host.port)
        .or(defaults.port)
        .or_else(|| fallback.and_then(|t| t.port))
        .unwrap_or(22);

    let cli_ids = cli.and_then(|c| c.identities.as_deref());
    let host_ids = host.identity_file.as_ref().map(Strings::as_slice);
    let default_ids = defaults.identity_file.as_ref().map(Strings::as_slice);
    let identities = if cli_ids.is_some() || host_ids.is_some() || default_ids.is_some() {
        let mut ids = Vec::with_capacity(
            cli_ids.map_or(0, <[PathBuf]>::len)
                + host_ids.map_or(0, <[Text<'_>]>::len)
                + default_ids.map_or(0, <[Text<'_>]>::len),
        );
        ids.extend(
            cli_ids
                .into_iter()
                .flatten()
                .filter_map(|path| paths::cli_path(path)),
        );
        for path in [host_ids, default_ids].into_iter().flatten().flatten() {
            ids.push(paths.path(Path::new(path.as_str()), "identity_file")?);
        }
        Some(ids)
    } else {
        None
    };

    let cert = host
        .certificate_file
        .as_ref()
        .or(defaults.certificate_file.as_ref())
        .map(|s| paths.path(Path::new(s.as_str()), "certificate_file"))
        .transpose()?;
    let agent = paths.agent(
        host.identity_agent
            .as_ref()
            .or(defaults.identity_agent.as_ref())
            .map(Text::as_str),
    )?;
    let identities_only = host
        .identities_only
        .or(defaults.identities_only)
        .unwrap_or_default();
    let tty = host
        .request_tty
        .or(defaults.request_tty)
        .unwrap_or_default();

    Ok(Host {
        dest: Dest { name, user, port },
        auth: Auth {
            identities,
            cert,
            agent,
            identities_only,
            ..Auth::default()
        },
        tty,
    })
}

fn jumps<'t, 'src>(
    target: HostRef<'t, 'src>,
    doc: &'t Profiles<'src>,
    cli: Option<&'t Options>,
) -> impl DoubleEndedIterator<Item = HostRef<'t, 'src>> {
    let args = cli.and_then(|c| c.proxy_jump.as_deref());
    let selected = if args.is_some() {
        None
    } else {
        target
            .fields(doc)
            .and_then(|h| h.proxy_jump.as_ref())
            .or(doc.defaults.proxy_jump.as_ref())
    };

    let (string, array, inline) = match selected {
        Some(Jumps::One(Jump::Name(s))) => (Some(s.as_str()), None, None),
        Some(Jumps::One(Jump::Host(host))) => (None, None, Some(host.as_ref())),
        Some(Jumps::Many(v)) => (None, Some(v.as_slice()), None),
        None => (None, None, None),
    };

    args.into_iter()
        .flatten()
        .map(String::as_str)
        .chain(string.into_iter().flat_map(|s| s.split(',')))
        .map(|s| HostRef::Named(Target::parse(s)))
        .chain(array.into_iter().flatten().map(|jump| match jump {
            Jump::Name(s) => HostRef::Named(Target::parse(s.as_str())),
            Jump::Host(host) => HostRef::Inline(host),
        }))
        .chain(inline.into_iter().map(HostRef::Inline))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::toml::query;
    use crate::config::{Destination, Session};
    use anyhow::Context as _;

    fn config(host: &str) -> Options {
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
    fn sample_tables_resolve_host_settings_and_defaults() -> Result<()> {
        use openrussh_client::config::RequestTty;

        let text = r#"
[default]
identities-only = "yes"
[hosts]
github = { user = "git", port = 22, identity-file = "~/.ssh/id_ed25519" }
[hosts.my-host]
host-name = "example.com"
identity-file = ["/keys/id1", "/keys/id2"]
identity-agent = "~/.ssh/agent.sock"
request-tty = "yes"
"#;
        let github = query(text, &config("github"))?.target;

        assert_eq!(github.dest.user, "git");
        assert_eq!(github.dest.port, 22);
        assert!(github.auth.identities_only);

        let host = query(text, &config("my-host"))?.target;

        assert_eq!(host.dest.name, "example.com");
        assert_eq!(
            host.auth.identities,
            Some(vec!["/keys/id1".into(), "/keys/id2".into()])
        );
        assert_eq!(
            host.auth.agent,
            Some(std::env::home_dir().unwrap().join(".ssh/agent.sock"))
        );
        assert_eq!(host.tty, RequestTty::IfTty);
        assert!(host.auth.identities_only);

        Ok(())
    }

    #[test]
    fn host_names_are_literal_and_case_sensitive() -> Result<()> {
        let text = r#"
[default]
user = "default"
[hosts]
Sample = { user = "capital" }
"s*" = { user = "star" }
"s?" = { user = "question" }
"!sample" = { user = "bang" }
"#;

        for (name, user) in [
            ("sample", "default"),
            ("Sample", "capital"),
            ("s*", "star"),
            ("s?", "question"),
            ("!sample", "bang"),
            ("sa", "default"),
        ] {
            assert_eq!(query(text, &config(name))?.target.dest.user, user);
        }

        Ok(())
    }

    #[test]
    fn identities_accumulate_in_cli_host_default_order() -> Result<()> {
        let mut conf = config("sample");
        conf.identities = Some(vec!["cli".into()]);
        conf.destination.user = Some("cli-user".to_owned());
        conf.destination.port = Some(2200);

        let host = query(
            r#"
[default]
user = "default"
port = 22
identity-file = ["/keys/default1", "/keys/default2"]
[hosts.sample]
user = "host"
port = "2222"
identity-file = "/keys/host key"
certificate-file = "/keys/certificate"
"#,
            &conf,
        )?
        .target;

        assert_eq!(host.dest.user, "cli-user");
        assert_eq!(host.dest.port, 2200);
        assert_eq!(
            host.auth.identities,
            Some(
                ["cli", "/keys/host key", "/keys/default1", "/keys/default2"]
                    .map(PathBuf::from)
                    .into()
            )
        );
        assert_eq!(host.auth.cert, Some("/keys/certificate".into()));

        Ok(())
    }

    #[test]
    fn omitted_and_agent_directives_keep_library_behavior() -> Result<()> {
        use openrussh_client::ssh2_config::{ParseRule, SshConfig};

        for (toml, openssh) in [
            ("", ""),
            (
                "[default]\nidentity-agent = 'SSH_AUTH_SOCK'",
                "IdentityAgent SSH_AUTH_SOCK",
            ),
            (
                "[default]\nidentity-agent = '$SSH_AUTH_SOCK'",
                "IdentityAgent $SSH_AUTH_SOCK",
            ),
            ("[default]\nidentity-agent = 'none'", "IdentityAgent none"),
            (
                "[default]\nidentity-agent = '~/agent'",
                "IdentityAgent ~/agent",
            ),
        ] {
            let openssh = SshConfig::default()
                .parse(&mut openssh.as_bytes(), ParseRule::ALLOW_UNSUPPORTED_FIELDS)?;
            let expected = Target::parse("sample").query(&openssh)?;

            assert_eq!(query(toml, &config("sample"))?.target, expected.target);
        }

        Ok(())
    }

    #[test]
    fn cli_paths_expand_only_the_current_users_home() -> Result<()> {
        let home = std::env::home_dir().context("Missing home directory")?;
        let user = whoami::username()?;

        for path in ["", "~", "~/key", "relative/key", "/absolute/key", "."] {
            let expected = match path {
                "" => None,
                "~" => Some(home.clone()),
                "~/key" => Some(home.join("key")),
                path => Some(PathBuf::from(path)),
            };

            assert_eq!(paths::cli_path(Path::new(path)), expected, "{path}");
        }

        assert_eq!(
            paths::cli_path(Path::new(&format!("~{user}/key"))),
            Some(home.join("key"))
        );
        assert_eq!(
            paths::cli_path(Path::new(&format!("~{user}-other/key"))),
            None
        );

        Ok(())
    }

    #[test]
    fn native_cli_identity_paths_preserve_non_utf8_bytes() -> Result<()> {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(OsString::from_vec(vec![b'k', 0xff]));
        let mut conf = config("sample");
        conf.identities = Some(vec![path.clone()]);

        assert_eq!(defaults(&conf)?.target.auth.identities, Some(vec![path]));

        Ok(())
    }

    #[test]
    fn jump_lists_recurse_only_through_the_first_exact_host() -> Result<()> {
        for jumps in ["'Jump,last'", "['Jump', 'last']"] {
            let text = format!(
                r#"
[default]
user = "default"
[hosts.sample]
proxy-jump = {jumps}
[hosts.Jump]
host-name = "first.example"
proxy-jump = "s*"
[hosts.jump]
proxy-jump = "wrong"
[hosts.last]
proxy-jump = "ignored"
[hosts."s*"]
host-name = "literal.example"
[hosts.star]
host-name = "wrong.example"
"#
            );
            let chain = query(&text, &config("sample"))?;
            let bastions = chain.bastions.context("Missing jumps")?;

            assert_eq!(
                bastions
                    .iter()
                    .map(|h| h.dest.name.as_str())
                    .collect::<Vec<_>>(),
                ["last", "first.example", "literal.example"]
            );
        }

        Ok(())
    }

    #[test]
    fn cli_and_empty_jump_lists_override_inherited_jumps() -> Result<()> {
        let text = "[default]\nproxy-jump = 'jump'\n[hosts.sample]\nproxy-jump = []";

        assert!(query(text, &config("sample"))?.bastions.is_none());

        let mut conf = config("sample");
        conf.proxy_jump = Some(Vec::new());

        assert!(
            query("[hosts.sample]\nproxy-jump = 'jump'", &conf)?
                .bastions
                .is_none()
        );
        conf.proxy_jump = Some(vec!["alice@Jump:2222".to_owned()]);

        let chain = query("[hosts.Jump]\nhost-name = 'jump.example'", &conf)?;
        let bastions = chain.bastions.context("Missing CLI jump")?;

        assert_eq!(bastions.len(), 1);
        assert_eq!(bastions[0].dest.name, "jump.example");
        assert_eq!(bastions[0].dest.user, "alice");
        assert_eq!(bastions[0].dest.port, 2222);

        Ok(())
    }

    #[test]
    fn ports_accept_strings_and_toml_integer_notation() -> Result<()> {
        for port in ["'2222'", "2222", "2_222", "0x8ae"] {
            assert_eq!(
                query(&format!("[default]\nport = {port}"), &config("sample"))?
                    .target
                    .dest
                    .port,
                2222
            );
        }

        Ok(())
    }

    #[test]
    fn invalid_sections_directives_and_value_types_are_rejected() {
        for text in [
            "[unknown]",
            "default = 1",
            "[hosts]\nsample = 'host'",
            "[default]\ninclude = 'other.toml'",
            "[hosts.unused]\ninclude = ['other']",
            "[default]\nidentities-only = 1",
            "[default]\nport = 65536",
            "[default]\nport = -1",
            "[default]\nport = 22.0",
            "[default]\nuser = ['alice']",
            "[default]\nidentity-file = [1]",
            "[hosts.unused]\nproxy-jump = ['jump', 1]",
            "[hosts.sample",
        ] {
            assert!(query(text, &config("sample")).is_err(), "{text}");
        }
    }
}
