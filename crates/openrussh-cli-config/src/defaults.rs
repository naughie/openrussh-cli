//! Built-in connection settings without a parsed configuration source.

use crate::algorithms::Policies;
use crate::{Meta, ResolveOptions, Resolved, Result};

use openrussh_client::config::{Auth, Chain, Dest, Host, Target};

use std::path::{Path, PathBuf};

pub(super) fn resolve(options: &ResolveOptions<'_>) -> Result<Resolved> {
    let target = host(
        Target {
            host: &options.destination.host,
            user: options.destination.user.as_deref(),
            port: options.destination.port,
        },
        options.identities,
        options.identity_agent.is_none(),
    )?;
    let bastions = options
        .proxy_jump
        .filter(|jumps| !jumps.is_empty())
        .map(|jumps| {
            jumps
                .iter()
                .rev()
                .map(|jump| host(Target::parse(jump), None, true))
                .collect::<Result<Vec<_>>>()
                .map(Vec::into_boxed_slice)
        })
        .transpose()?;

    Ok(Resolved {
        temp_dir: None,
        chain: Chain { target, bastions },
        algorithms: Policies::default(),
        meta: Meta::default(),
    })
}

fn host(target: Target<'_>, identities: Option<&[PathBuf]>, inherit_agent: bool) -> Result<Host> {
    let user = match target.user {
        Some(user) => user.to_owned(),
        None => whoami::username()?,
    };

    Ok(Host {
        dest: Dest {
            name: target.host.to_owned(),
            user,
            port: target.port.unwrap_or(22),
        },
        auth: Auth {
            identities: identities
                .map(|paths| paths.iter().filter_map(|path| cli_path(path)).collect()),
            agent: if inherit_agent { agent()? } else { None },
            ..Auth::default()
        },
        tty: Default::default(),
    })
}

fn agent() -> Result<Option<PathBuf>> {
    use crate::Error;

    let Some(value) = std::env::var_os("SSH_AUTH_SOCK") else {
        return Ok(None);
    };
    let path = Path::new(&value);
    if path.is_absolute() {
        return Ok(Some(path.to_owned()));
    }

    let tilde = matches!(path.components().next(), Some(std::path::Component::Normal(first))
        if first.as_encoded_bytes().first() == Some(&b'~'));
    if !tilde {
        return Err(Error::InvalidPath {
            field: "identity_agent",
            path: path.to_owned(),
        });
    }
    let expanded = cli_path(path).ok_or_else(|| Error::PathExpansion {
        field: "identity_agent",
        path: path.to_owned(),
    })?;
    if !expanded.is_absolute() {
        return Err(Error::InvalidPath {
            field: "identity_agent",
            path: path.to_owned(),
        });
    }
    Ok(Some(expanded))
}

fn cli_path(path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let mut parts = path.components();
    let Some(Component::Normal(first)) = parts.next() else {
        return (!path.as_os_str().is_empty()).then(|| path.to_owned());
    };
    if first != "~" {
        if first.as_encoded_bytes().first() != Some(&b'~') {
            return Some(path.to_owned());
        }
        let user = first.to_str()?.strip_prefix('~')?;
        if whoami::username().ok().as_deref() != Some(user) {
            return None;
        }
    }

    let mut expanded = std::env::home_dir()?;
    expanded.push(parts.as_path());
    Some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::TestConfig;

    #[test]
    fn explicit_agent_selection_does_not_read_the_unused_environment_default() -> Result<()> {
        const CHILD: &str = "OPENRUSSH_TEST_EXPLICIT_AGENT_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "defaults::tests::explicit_agent_selection_does_not_read_the_unused_environment_default",
                ])
                .env(CHILD, "1")
                .env("SSH_AUTH_SOCK", "invalid-relative-default")
                .output()?;
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }

        use crate::{Config, Destination, HostOverrides, IdentityAgent, Session};

        let destination = || Destination {
            host: "target.example".into(),
            user: Some("user".into()),
            port: None,
        };
        assert!(
            Config::builder()
                .config_file("none")
                .build(destination(), Session::None)
                .is_err()
        );
        for selection in [
            IdentityAgent::Disabled,
            IdentityAgent::Socket("/sockets/override".into()),
        ] {
            let expected = match &selection {
                IdentityAgent::Disabled => None,
                IdentityAgent::Socket(path) => Some(path.clone()),
            };
            let config = Config::builder()
                .config_file("none")
                .host_overrides(HostOverrides {
                    identity_agent: Some(selection),
                    ..HostOverrides::default()
                })
                .build(destination(), Session::None)?;
            assert_eq!(config.connection.chain().target.auth.agent, expected);
        }
        Ok(())
    }

    #[test]
    fn defaults_resolve_cli_jump_hosts_without_profiles() -> Result<()> {
        let mut config = TestConfig::new("destination");
        config.destination.user = Some("target-user".into());
        config.destination.port = Some(2222);
        config.identities = Some(vec!["relative-key".into()]);
        config.proxy_jump = Some(vec![
            "first-user@first:2200".into(),
            "second-user@second".into(),
        ]);
        let resolved = resolve(&config.options())?;

        assert_eq!(resolved.chain.target.dest.user, "target-user");
        assert_eq!(resolved.chain.target.dest.port, 2222);
        assert_eq!(
            resolved.chain.target.auth.identities,
            Some(vec!["relative-key".into()])
        );
        let jumps = resolved.chain.bastions.as_deref().unwrap();
        assert_eq!(jumps[0].dest.name, "second");
        assert_eq!(jumps[0].dest.user, "second-user");
        assert_eq!(jumps[0].dest.port, 22);
        assert_eq!(jumps[1].dest.name, "first");
        assert_eq!(jumps[1].dest.user, "first-user");
        assert_eq!(jumps[1].dest.port, 2200);
        assert!(jumps.iter().all(|jump| jump.auth.identities.is_none()));
        assert!(resolved.meta.known_hosts.is_none());
        assert!(resolved.meta.log_filter.is_none());
        Ok(())
    }

    #[test]
    fn cli_paths_preserve_native_bytes_and_expand_only_the_current_home() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let path = PathBuf::from(OsString::from_vec(b"relative-\xff-key".to_vec()));
        assert_eq!(cli_path(&path), Some(path));
        assert_eq!(
            cli_path(Path::new("relative-key")),
            Some("relative-key".into())
        );
        assert!(cli_path(Path::new("")).is_none());
        assert!(cli_path(Path::new("~definitely-not-the-current-user/key")).is_none());
        let home = std::env::home_dir().unwrap();
        assert_eq!(cli_path(Path::new("~/key")), Some(home.join("key")));
        let current_user = whoami::username().unwrap();
        assert_eq!(
            cli_path(Path::new(&format!("~{current_user}/key"))),
            Some(home.join("key"))
        );
    }
}
