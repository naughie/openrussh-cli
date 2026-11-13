use super::jumps::{Jump, Jumps};
use super::paths;
use super::values::{Strings, Text};

use crate::algorithms::{AlgorithmList, Algorithms, Policies};
use crate::{Error, LogFilter, Meta, ResolveOptions, Resolved, Result};

use openrussh_client::config::{Auth, Chain, Dest, Host, RequestTty, Target};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Profiles<'src> {
    #[serde(rename = "default", alias = "defaults", borrow)]
    defaults: HostFields<'src>,
    #[serde(borrow)]
    hosts: BTreeMap<Text<'src>, HostFields<'src>>,
    #[serde(borrow)]
    meta: MetaFields<'src>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
struct MetaFields<'src> {
    #[serde(borrow, alias = "known_hosts")]
    known_hosts: Option<Text<'src>>,
    #[serde(rename = "log-level", alias = "log_level")]
    log_filter: Option<LogFilter>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct HostFields<'src> {
    #[serde(borrow, alias = "host_name")]
    host_name: Option<Text<'src>>,
    #[serde(borrow)]
    user: Option<Text<'src>>,
    #[serde(deserialize_with = "super::values::deserialize_port")]
    port: Option<u16>,
    #[serde(borrow, alias = "identity_file")]
    identity_file: Option<Strings<'src>>,
    #[serde(borrow, alias = "certificate_file")]
    certificate_file: Option<Text<'src>>,
    #[serde(borrow, alias = "identity_agent")]
    identity_agent: Option<Text<'src>>,
    #[serde(
        alias = "identities_only",
        deserialize_with = "super::values::deserialize_bool"
    )]
    identities_only: Option<bool>,
    #[serde(
        alias = "request_tty",
        deserialize_with = "super::values::deserialize_tty"
    )]
    request_tty: Option<RequestTty>,
    #[serde(borrow, alias = "proxy_jump")]
    proxy_jump: Option<Jumps<'src>>,
    #[serde(borrow, alias = "kex_algorithms")]
    kex_algorithms: Option<Strings<'src>>,
    #[serde(borrow, alias = "host_key_algorithms")]
    host_key_algorithms: Option<Strings<'src>>,
}

impl Profiles<'_> {
    pub(super) fn log_filter(&self) -> Option<LogFilter> {
        self.meta.log_filter.clone()
    }

    pub(super) fn require_named_jumps(&self) -> Result<()> {
        for host in std::iter::once(&self.defaults).chain(self.hosts.values()) {
            if !host.proxy_jump.as_ref().is_none_or(|jumps| {
                jumps
                    .as_slice()
                    .iter()
                    .all(|jump| matches!(jump, Jump::Name(_)))
            }) {
                return Err(Error::TomlInlineJump);
            }
        }

        Ok(())
    }

    pub(super) fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        self.resolve_inner(options)
            .map_err(|error| error.with_log_filter(self.meta.log_filter.clone()))
    }

    fn resolve_inner(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        for host in std::iter::once(&self.defaults).chain(self.hosts.values()) {
            host.validate()?;
        }

        let target = Target {
            host: &options.destination.host,
            user: options.destination.user.as_deref(),
            port: options.destination.port,
        };
        require_acyclic_jump_route(target, self, options)?;
        let (target_host, target_algorithms) = resolve_host(target, self, Some(options))?;
        let mut pending = jumps(target, self, Some(options));
        let mut bastions: Option<Vec<Host>> = None;
        let mut bastion_algorithms: Option<Vec<Algorithms>> = None;

        while let Some(first) = pending.next() {
            let chain = bastions.get_or_insert_with(Vec::new);
            chain.reserve(pending.size_hint().0 + 1);

            for jump in pending.rev() {
                push_host(jump, self, chain, &mut bastion_algorithms)?;
            }

            push_host(first, self, chain, &mut bastion_algorithms)?;
            pending = jumps(first, self, None);
        }

        Ok(Resolved {
            temp_dir: None,
            chain: Chain {
                target: target_host,
                bastions: bastions.map(Vec::into_boxed_slice),
            },
            algorithms: Policies {
                target: target_algorithms,
                bastions: bastion_algorithms.map(Vec::into_boxed_slice),
            },
            meta: Meta {
                log_filter: self.meta.log_filter.clone(),
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

fn require_acyclic_jump_route<'a>(
    target: Target<'a>,
    profiles: &'a Profiles<'_>,
    overrides: &'a ResolveOptions<'_>,
) -> Result<()> {
    // Only the first jump contributes ancestors. Its successor depends on the
    // lookup alias, not on embedded user/port values or the resolved host_name.
    let next = |target| jumps(target, profiles, None).next();
    let first = jumps(target, profiles, Some(overrides)).next();
    let mut slow = first;
    let mut fast = first.and_then(next);

    while let (Some(left), Some(right)) = (slow, fast) {
        if left.host == right.host {
            return Err(Error::JumpCycle);
        }
        slow = next(left);
        fast = next(right).and_then(next);
    }

    Ok(())
}

impl HostFields<'_> {
    fn validate(&self) -> Result<()> {
        if let Some(names) = &self.kex_algorithms {
            names.validate_kex_names()?;
        }
        if let Some(names) = &self.host_key_algorithms {
            names.validate_host_key_names()?;
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

        Ok(())
    }
}

fn push_host(
    target: Target<'_>,
    profiles: &Profiles<'_>,
    hosts: &mut Vec<Host>,
    policies: &mut Option<Vec<Algorithms>>,
) -> Result<()> {
    let (host, algorithms) = resolve_host(target, profiles, None)?;
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

fn resolve_host(
    target: Target<'_>,
    profiles: &Profiles<'_>,
    overrides: Option<&ResolveOptions<'_>>,
) -> Result<(Host, Algorithms)> {
    let empty = HostFields::default();
    let host = profiles.hosts.get(target.host).unwrap_or(&empty);
    let defaults = &profiles.defaults;

    let name = host
        .host_name
        .as_ref()
        .or(defaults.host_name.as_ref())
        .map_or(target.host, Text::as_str)
        .to_owned();
    let user = if let Some(user) = overrides
        .and_then(|options| options.destination.user.as_deref())
        .or_else(|| {
            host.user
                .as_ref()
                .or(defaults.user.as_ref())
                .map(Text::as_str)
        })
        .or(target.user)
    {
        user.to_owned()
    } else {
        whoami::username()?
    };
    let port = overrides
        .and_then(|options| options.destination.port)
        .or(host.port)
        .or(defaults.port)
        .or(target.port)
        .unwrap_or(22);

    let supplied_ids = overrides.and_then(|options| options.identities);
    let host_ids = host.identity_file.as_ref().map(Strings::as_slice);
    let default_ids = defaults.identity_file.as_ref().map(Strings::as_slice);
    let identities = if supplied_ids.is_some() || host_ids.is_some() || default_ids.is_some() {
        let mut ids = Vec::with_capacity(
            supplied_ids.map_or(0, <[PathBuf]>::len)
                + host_ids.map_or(0, <[Text<'_>]>::len)
                + default_ids.map_or(0, <[Text<'_>]>::len),
        );
        ids.extend(
            supplied_ids
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
        .map(|value| paths::path(Path::new(value.as_str()), "certificate_file"))
        .transpose()?;
    let agent = paths::agent(
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
    let algorithms = Algorithms {
        kex: host
            .kex_algorithms
            .as_ref()
            .or(defaults.kex_algorithms.as_ref())
            .map(AlgorithmList::kex)
            .transpose()?,
        host_keys: host
            .host_key_algorithms
            .as_ref()
            .or(defaults.host_key_algorithms.as_ref())
            .map(AlgorithmList::host_keys)
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

fn jumps<'a, 'src>(
    target: Target<'a>,
    profiles: &'a Profiles<'src>,
    overrides: Option<&'a ResolveOptions<'_>>,
) -> impl DoubleEndedIterator<Item = Target<'a>> {
    let supplied = overrides.and_then(|options| options.proxy_jump);
    let selected = if supplied.is_some() {
        None
    } else {
        profiles
            .hosts
            .get(target.host)
            .and_then(|host| host.proxy_jump.as_ref())
            .or(profiles.defaults.proxy_jump.as_ref())
    };
    let (single, array) = match selected {
        Some(Jumps::One(Jump::Name(name))) => (Some(name.as_str()), None),
        Some(Jumps::Many(names)) => (None, Some(names.as_slice())),
        Some(Jumps::One(Jump::InvalidInline)) | None => (None, None),
    };

    supplied
        .into_iter()
        .flatten()
        .map(String::as_str)
        .chain(single.into_iter().flat_map(|names| names.split(',')))
        .chain(array.into_iter().flatten().filter_map(|jump| match jump {
            Jump::Name(name) => Some(name.as_str()),
            Jump::InvalidInline => None,
        }))
        .map(Target::parse)
}
