use crate::algorithms::{AlgorithmList, Algorithms, Policies};
use crate::{Error, LogLevel, Meta, ResolveOptions, Resolved, Result};

use openrussh_client::config::{Auth, Chain, Dest, Host, RequestTty, Target};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::jumps::{Jump, Jumps};
use super::paths;
use super::values::{self, Strings};

#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Profiles {
    #[serde(rename = "default", alias = "defaults")]
    defaults: HostFields,
    hosts: BTreeMap<String, HostFields>,
    meta: MetaFields,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
struct MetaFields {
    #[serde(alias = "known_hosts")]
    known_hosts: Option<String>,
    #[serde(alias = "log_level")]
    log_level: Option<LogLevel>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct HostFields {
    #[serde(alias = "host_name")]
    host_name: Option<String>,
    user: Option<String>,
    #[serde(deserialize_with = "values::deserialize_port")]
    port: Option<u16>,
    #[serde(alias = "identity_file")]
    identity_file: Option<Strings>,
    #[serde(alias = "certificate_file")]
    certificate_file: Option<String>,
    #[serde(alias = "identity_agent")]
    identity_agent: Option<String>,
    #[serde(
        alias = "identities_only",
        deserialize_with = "values::deserialize_bool"
    )]
    identities_only: Option<bool>,
    #[serde(alias = "request_tty", deserialize_with = "values::deserialize_tty")]
    request_tty: Option<RequestTty>,
    #[serde(alias = "proxy_jump")]
    proxy_jump: Option<Jumps>,
    #[serde(alias = "kex_algorithms")]
    kex_algorithms: Option<Strings>,
    #[serde(alias = "host_key_algorithms")]
    host_key_algorithms: Option<Strings>,
}

impl Profiles {
    pub(super) fn resolve(&self, conf: &ResolveOptions<'_>) -> Result<Resolved> {
        self.resolve_inner(conf)
            .map_err(|error| error.with_log_level(self.meta.log_level.clone()))
    }

    fn resolve_inner(&self, conf: &ResolveOptions<'_>) -> Result<Resolved> {
        for host in std::iter::once(&self.defaults).chain(self.hosts.values()) {
            host.validate()?;
        }

        let doc = self;
        let target = HostRef::Named(Target {
            host: &conf.destination.host,
            user: conf.destination.user.as_deref(),
            port: conf.destination.port,
        });
        let (target_host, target_algorithms) = resolve_host(target, doc, Some(conf))?;
        let mut pending = jumps(target, doc, Some(conf));
        let mut bastions: Option<Vec<Host>> = None;
        let mut bastion_algorithms: Option<Vec<Algorithms>> = None;

        while let Some(first) = pending.next() {
            let chain = bastions.get_or_insert_with(Vec::new);
            chain.reserve(pending.size_hint().0 + 1);

            for jump in pending.rev() {
                push_host(jump, doc, chain, &mut bastion_algorithms)?;
            }

            push_host(first, doc, chain, &mut bastion_algorithms)?;
            pending = jumps(first, doc, None);
        }

        Ok(Resolved {
            chain: Chain {
                target: target_host,
                bastions: bastions.map(Vec::into_boxed_slice),
            },
            algorithms: Policies {
                target: target_algorithms,
                bastions: bastion_algorithms.map(Vec::into_boxed_slice),
            },
            meta: Meta {
                log_level: self.meta.log_level.clone(),
                known_hosts: self
                    .meta
                    .known_hosts
                    .as_ref()
                    .map(|value| paths::path(Path::new(value.as_str()), "known_hosts"))
                    .transpose()?,
            },
        })
    }
}

impl HostFields {
    fn validate(&self) -> Result<()> {
        if let Some(names) = &self.kex_algorithms {
            names.validate_kex()?;
        }
        if let Some(names) = &self.host_key_algorithms {
            names.validate_host_keys()?;
        }

        for path in self
            .identity_file
            .as_ref()
            .into_iter()
            .flat_map(Strings::as_slice)
        {
            paths::validate(Path::new(path.as_str()), "identity_file")?;
        }
        if let Some(path) = &self.certificate_file {
            paths::validate(Path::new(path.as_str()), "certificate_file")?;
        }
        if let Some(value) = &self.identity_agent {
            paths::validate_agent(value.as_str())?;
        }

        for jump in self
            .proxy_jump
            .as_ref()
            .into_iter()
            .flat_map(Jumps::as_slice)
        {
            if let Jump::Host(host) = jump {
                host.validate()?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy)]
enum HostRef<'a> {
    Named(Target<'a>),
    Inline(&'a HostFields),
}

impl<'a> HostRef<'a> {
    fn fields(self, doc: &'a Profiles) -> Option<&'a HostFields> {
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

fn push_host<'a>(
    target: HostRef<'a>,
    doc: &'a Profiles,
    hosts: &mut Vec<Host>,
    policies: &mut Option<Vec<Algorithms>>,
) -> Result<()> {
    let (host, algorithms) = resolve_host(target, doc, None)?;
    if !algorithms.is_empty() || policies.is_some() {
        let policies = policies.get_or_insert_with(|| {
            let mut values = Vec::with_capacity(hosts.capacity());
            values.resize_with(hosts.len(), Algorithms::default);
            values
        });
        policies.push(algorithms);
    }
    hosts.push(host);
    Ok(())
}

fn resolve_host<'a>(
    target: HostRef<'a>,
    doc: &'a Profiles,
    cli: Option<&ResolveOptions<'_>>,
) -> Result<(Host, Algorithms)> {
    let empty = HostFields::default();
    let host = target.fields(doc).unwrap_or(&empty);
    let defaults = &doc.defaults;
    let fallback = target.fallback();

    let name = host
        .host_name
        .as_ref()
        .or(defaults.host_name.as_ref())
        .map(String::as_str)
        .or_else(|| fallback.map(|t| t.host))
        .ok_or(Error::InlineJumpMissingHost)?
        .to_owned();
    let user = if let Some(user) = cli
        .and_then(|c| c.destination.user.as_deref())
        .or_else(|| {
            host.user
                .as_ref()
                .or(defaults.user.as_ref())
                .map(String::as_str)
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

    let cli_ids = cli.and_then(|c| c.identities);
    let host_ids = host.identity_file.as_ref().map(Strings::as_slice);
    let default_ids = defaults.identity_file.as_ref().map(Strings::as_slice);
    let identities = if cli_ids.is_some() || host_ids.is_some() || default_ids.is_some() {
        let mut ids = Vec::with_capacity(
            cli_ids.map_or(0, <[PathBuf]>::len)
                + host_ids.map_or(0, <[String]>::len)
                + default_ids.map_or(0, <[String]>::len),
        );
        ids.extend(
            cli_ids
                .into_iter()
                .flatten()
                .filter_map(|path| paths::cli_path(path)),
        );
        for path in [host_ids, default_ids].into_iter().flatten().flatten() {
            ids.push(paths::path(Path::new(path.as_str()), "identity_file")?);
        }
        Some(ids)
    } else {
        None
    };

    let cert = host
        .certificate_file
        .as_ref()
        .or(defaults.certificate_file.as_ref())
        .map(|s| paths::path(Path::new(s.as_str()), "certificate_file"))
        .transpose()?;
    let agent = paths::agent(
        host.identity_agent
            .as_ref()
            .or(defaults.identity_agent.as_ref())
            .map(String::as_str),
    )?;
    let identities_only = host
        .identities_only
        .or(defaults.identities_only)
        .unwrap_or_default();
    let tty = host
        .request_tty
        .or(defaults.request_tty)
        .unwrap_or_default();

    let algorithms = Algorithms {
        kex: host
            .kex_algorithms
            .as_ref()
            .or(defaults.kex_algorithms.as_ref())
            .map(|names| names.kex())
            .transpose()?,
        host_keys: host
            .host_key_algorithms
            .as_ref()
            .or(defaults.host_key_algorithms.as_ref())
            .map(|names| names.host_keys())
            .transpose()?,
    };

    Ok((
        Host {
            dest: Dest { name, user, port },
            auth: Auth {
                identities,
                cert,
                agent,
                identities_only,
                ..Auth::default()
            },
            tty,
        },
        algorithms,
    ))
}

fn jumps<'t>(
    target: HostRef<'t>,
    doc: &'t Profiles,
    cli: Option<&'t ResolveOptions<'_>>,
) -> impl DoubleEndedIterator<Item = HostRef<'t>> {
    let args = cli.and_then(|c| c.proxy_jump);
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
