//! Compiled key-exchange and server host-key preferences for a connection chain.
//!
//! [`crate::ConfigBuilder::build()`] produces [`Policies`] alongside
//! [`crate::Config::chain`]. Native TOML and Lua host/default records configure
//! them with `kex-algorithms` / `kex_algorithms` and `host-key-algorithms` /
//! `host_key_algorithms`. Each setting accepts a comma-separated string or an
//! array of names. A host's value replaces the default record's entire value.
//!
//! Prefix the first name with `+` to append, `-` to remove exact names, or `^` to
//! prepend; without a prefix, the list replaces the transport defaults. Modifiers
//! also use the transport defaults, rather than an inherited configuration list.
//! Names must be literal, supported algorithms; removal also permits unknown
//! literal names and ignores them. Empty lists/results and wildcard patterns
//! are rejected. Host-certificate algorithms must precede plain host-key
//! algorithms because the transport advertises those as separate lists.
//!
//! OpenSSH files, disabled file loading, and omitted native settings produce
//! unconfigured policies. An unconfigured policy preserves the preferences to
//! which it is applied; it does not replace them with transport defaults.
//!
//! # Applying a configured host policy
//!
//! Configuration compiles names into transport types before connection setup.
//! Obtain each host's policy using the actual host reference from its paired
//! chain, then apply it to that connection's preferences:
//!
//! ```
//! use openrussh_cli_config::{Config, Destination, Session};
//! use openrussh_client::russh::{Preferred, kex};
//! use openrussh_client::russh::keys::ssh_key::Algorithm;
//!
//! let directory = tempfile::tempdir()?;
//! let path = directory.path().join("config.toml");
//! std::fs::write(&path, r#"
//! [default]
//! user = "demo"
//! [hosts.server]
//! kex-algorithms = "curve25519-sha256"
//! host-key-algorithms = ["ssh-ed25519-cert-v01@openssh.com", "ssh-ed25519"]
//! "#)?;
//! let config = Config::builder().config_file(path).build(
//!     Destination { host: "server".into(), user: None, port: None },
//!     Session::None,
//! )?;
//! let policy = config.algorithms.get(&config.chain, &config.chain.target)?;
//! assert!(policy.has_host_keys());
//!
//! let mut preferred: Preferred = config.preferred()?;
//! policy.apply(&mut preferred);
//! assert_eq!(preferred.kex[0], kex::CURVE25519);
//! assert_eq!(preferred.key.as_ref(), &[Algorithm::Ed25519]);
//! assert_eq!(preferred.host_key_certificates.as_ref(), &[Algorithm::Ed25519]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use openrussh_client::config::{Chain, Host};
use openrussh_client::russh::keys::ssh_key::Algorithm;
use openrussh_client::russh::{Preferred, kex};

use crate::{Error, Result};

/// Static transport-policy operations for a format's own algorithm-list value.
///
/// Each source format supplies its own interpretation of string/list values.
/// The default operations compile the resulting names under transport rules.
pub(crate) trait AlgorithmList {
    fn algorithm_names(&self) -> impl Iterator<Item = &str> + Clone;

    fn validate_kex(&self) -> Result<()> {
        validate_kex(self.algorithm_names())
    }

    fn validate_host_keys(&self) -> Result<()> {
        validate_host_keys(self.algorithm_names())
    }

    fn kex(&self) -> Result<Box<[kex::Name]>> {
        kex(self.algorithm_names())
    }

    fn host_keys(&self) -> Result<HostKeys> {
        host_keys(self.algorithm_names())
    }
}

/// Opaque, compiled algorithm overrides for one destination or jump host.
///
/// Obtain configured values from [`Policies::get()`]. [`Self::default()`] represents
/// no overrides: [`Self::has_host_keys()`] returns `false`, and [`Self::apply()`]
/// leaves its argument untouched. This differs from an explicit native list,
/// even when that list happens to equal the transport's defaults.
#[derive(Debug, Default)]
pub struct Algorithms {
    pub(super) kex: Option<Box<[kex::Name]>>,
    pub(super) host_keys: Option<HostKeys>,
}

/// Per-host algorithm overrides paired with a resolved [`Chain`].
///
/// [`crate::Config::algorithms`] and [`crate::Config::chain`] are built together.
/// Keep them paired and preserve the chain's host order when using [`Self::get()`]:
/// target and bastion policies are associated with positions in that chain,
/// rather than network names. Hosts with identical connection settings can
/// therefore have distinct policies.
///
/// [`Self::default()`] supplies an unconfigured policy for every member of a
/// supplied chain. Its lookups still require actual references into that chain.
#[derive(Debug, Default)]
pub struct Policies {
    pub(super) target: Algorithms,
    pub(super) bastions: Option<Box<[Algorithms]>>,
}

impl Policies {
    /// Finds the policy for an actual host reference in the associated chain.
    ///
    /// `host` must refer to `chain.target` or an element of `chain.bastions`.
    /// Membership is checked by pointer identity, so a cloned or otherwise equal
    /// host is rejected. The returned policy borrows `self`.
    ///
    /// Pass the chain originally built alongside these policies, with its host
    /// order intact. This method checks membership in the supplied chain; it
    /// cannot establish that the chain belongs to this [`Policies`] instance.
    /// A bastion with no explicit policy receives an unconfigured policy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HostNotInChain`] if `host` is not an actual member of
    /// `chain`. Returns [`Error::MissingAlgorithmPolicy`] if a bastion policy
    /// array exists but lacks the matched host's index, indicating inconsistent
    /// chain/policy alignment.
    ///
    /// # Example
    ///
    /// Both destination and jump hosts must be borrowed from the paired chain:
    ///
    /// ```
    /// use openrussh_cli_config::{Config, Destination, Error, Session};
    ///
    /// let config = Config::builder()
    ///     .config_file("none")
    ///     .proxy_jump(["demo@jump.example"])
    ///     .build(
    ///         Destination {
    ///             host: "server.example".into(), user: Some("demo".into()), port: None,
    ///         },
    ///         Session::None,
    ///     )?;
    /// let target = &config.chain.target;
    /// let jump = &config.chain.bastions.as_deref().unwrap()[0];
    /// assert!(!config.algorithms.get(&config.chain, target)?.has_host_keys());
    /// assert!(!config.algorithms.get(&config.chain, jump)?.has_host_keys());
    ///
    /// let cloned_target = target.clone();
    /// assert!(matches!(
    ///     config.algorithms.get(&config.chain, &cloned_target),
    ///     Err(Error::HostNotInChain),
    /// ));
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    pub fn get(&self, chain: &Chain, host: &Host) -> Result<&Algorithms> {
        static EMPTY: Algorithms = Algorithms {
            kex: None,
            host_keys: None,
        };

        if std::ptr::eq(host, &chain.target) {
            return Ok(&self.target);
        }

        let index = chain
            .bastions
            .as_deref()
            .and_then(|hosts| {
                hosts
                    .iter()
                    .position(|candidate| std::ptr::eq(host, candidate))
            })
            .ok_or(Error::HostNotInChain)?;
        match self.bastions.as_deref() {
            Some(policies) => policies.get(index).ok_or(Error::MissingAlgorithmPolicy),
            None => Ok(&EMPTY),
        }
    }
}

impl Algorithms {
    pub(super) fn is_empty(&self) -> bool {
        self.kex.is_none() && self.host_keys.is_none()
    }

    /// Reports whether an explicit host-key list was configured or inherited.
    ///
    /// A `true` result means [`Self::apply()`] replaces both certificate and plain
    /// host-key preferences. Callers should skip known-host preference reordering
    /// or additions in this case, so the configured list remains authoritative.
    /// A `false` result permits the usual known-host preference adjustment before
    /// applying this policy, even when a KEX override is present.
    ///
    /// This flag concerns negotiation order and allowed algorithms. It does not
    /// verify the server's identity or disable the caller's host-key verification.
    pub fn has_host_keys(&self) -> bool {
        self.host_keys.is_some()
    }

    /// Applies configured overrides to one connection's transport preferences.
    ///
    /// A configured KEX list replaces `preferred.kex` with its compiled methods,
    /// followed by protocol extension signals from [`Preferred::default()`]. Signals
    /// from the incoming value are not retained separately. An omitted KEX policy
    /// leaves the incoming list untouched.
    ///
    /// A configured host-key list replaces both `preferred.host_key_certificates`
    /// and `preferred.key`, preserving the compiled order within each list. A
    /// certificate-only policy clears the plain-key list, and a plain-only policy
    /// clears the certificate list. An omitted host-key policy preserves both.
    /// Use [`Self::has_host_keys()`] to decide whether known-host preference
    /// adjustments should run before this method.
    ///
    /// Cipher, MAC, compression, and every other preference remain untouched.
    /// Application is infallible and performs no I/O; configured names and list
    /// ordering were validated during configuration building.
    ///
    /// # Example
    ///
    /// The default policy preserves an existing, customized preference list:
    ///
    /// ```
    /// use openrussh_cli_config::algorithms::Algorithms;
    /// use openrussh_client::russh::{Preferred, kex};
    ///
    /// let mut preferred = Preferred {
    ///     kex: vec![kex::CURVE25519].into(),
    ///     ..Preferred::default()
    /// };
    /// let before = preferred.clone();
    /// let policy = Algorithms::default();
    /// assert!(!policy.has_host_keys());
    /// policy.apply(&mut preferred);
    /// assert_eq!(preferred.kex, before.kex);
    /// assert_eq!(preferred.key, before.key);
    /// assert_eq!(preferred.host_key_certificates, before.host_key_certificates);
    /// ```
    pub fn apply(&self, preferred: &mut Preferred) {
        if let Some(kex) = &self.kex {
            let defaults = Preferred::default();
            let signals = defaults
                .kex
                .iter()
                .copied()
                .filter(|name| kex::Name::try_from(name.as_ref()).is_err());
            let mut values = Vec::with_capacity(kex.len() + signals.clone().count());
            values.extend_from_slice(kex);
            values.extend(signals);
            preferred.kex = values.into();
        }
        if let Some(keys) = &self.host_keys {
            let certificates = keys.0.partition_point(|key| key.certificate);
            preferred.host_key_certificates = keys.0[..certificates]
                .iter()
                .map(|key| key.algorithm.clone())
                .collect::<Vec<_>>()
                .into();
            preferred.key = keys.0[certificates..]
                .iter()
                .map(|key| key.algorithm.clone())
                .collect::<Vec<_>>()
                .into();
        }
    }
}

#[derive(Debug)]
pub(super) struct HostKeys(Box<[HostKey]>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct HostKey {
    algorithm: Algorithm,
    certificate: bool,
}

impl HostKey {
    fn matches(&self, name: &str) -> bool {
        let plain = self.algorithm.as_str();
        if self.certificate {
            name.strip_suffix("-cert-v01@openssh.com")
                == Some(plain.strip_suffix("@openssh.com").unwrap_or(plain))
        } else {
            name == plain
        }
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Replace,
    Append,
    Remove,
    Prepend,
}

fn list<'a, I>(mut names: I) -> Result<(Mode, impl Iterator<Item = &'a str> + Clone)>
where
    I: Iterator<Item = &'a str> + Clone,
{
    let first = names.next().ok_or(Error::EmptyAlgorithmList)?;
    let (mode, first) = match first.as_bytes().first() {
        Some(b'+') => (Mode::Append, &first[1..]),
        Some(b'-') => (Mode::Remove, &first[1..]),
        Some(b'^') => (Mode::Prepend, &first[1..]),
        _ => (Mode::Replace, first),
    };

    Ok((mode, std::iter::once(first).chain(names)))
}

fn validate<'a, T, I>(names: I, parse: fn(&str) -> Result<T>) -> Result<()>
where
    I: Iterator<Item = &'a str> + Clone,
{
    let (mode, names) = list(names)?;
    for name in names {
        literal_name(name)?;
        if !matches!(mode, Mode::Remove) {
            parse(name)?;
        }
    }
    Ok(())
}

fn compile<'a, T, I, B>(
    names: I,
    mut baseline: B,
    parse: fn(&str) -> Result<T>,
    matches_name: fn(&T, &str) -> bool,
) -> Result<Vec<T>>
where
    T: Clone + PartialEq,
    I: Iterator<Item = &'a str> + Clone,
    B: Iterator<Item = T>,
{
    let (mode, names) = list(names)?;
    let mut values = match mode {
        Mode::Replace | Mode::Prepend => Vec::with_capacity(names.size_hint().0),
        Mode::Append | Mode::Remove => baseline.by_ref().collect(),
    };

    for name in names {
        literal_name(name)?;
        match mode {
            Mode::Replace | Mode::Append | Mode::Prepend => {
                let value = parse(name)?;
                if !values.contains(&value) {
                    values.push(value);
                }
            }
            Mode::Remove => values.retain(|existing| !matches_name(existing, name)),
        }
    }

    if matches!(mode, Mode::Prepend) {
        for value in baseline {
            if !values.contains(&value) {
                values.push(value);
            }
        }
    }
    if values.is_empty() {
        return Err(Error::EmptyAlgorithmList);
    }

    Ok(values)
}

fn literal_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name.bytes().all(|byte| {
            byte.is_ascii_graphic() && !matches!(byte, b',' | b'*' | b'?' | b'[' | b']' | b'!')
        })
    {
        return Err(Error::InvalidAlgorithmName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

fn kex_name(name: &str) -> Result<kex::Name> {
    kex::Name::try_from(name).map_err(|_| Error::UnknownKeyExchangeAlgorithm {
        name: name.to_owned(),
    })
}

fn host_key_name(name: &str) -> Result<HostKey> {
    use openrussh_client::russh::keys::key::ALL_KEY_TYPES;

    if let Some(algorithm) = ALL_KEY_TYPES.iter().find(|a| a.as_str() == name) {
        return Ok(HostKey {
            algorithm: algorithm.clone(),
            certificate: false,
        });
    }

    if let Some(base) = name.strip_suffix("-cert-v01@openssh.com")
        && let Some(algorithm) = ALL_KEY_TYPES.iter().find(|algorithm| {
            let name = algorithm.as_str();
            name.strip_suffix("@openssh.com").unwrap_or(name) == base
        })
    {
        return Ok(HostKey {
            algorithm: algorithm.clone(),
            certificate: true,
        });
    }

    Err(Error::UnknownHostKeyAlgorithm {
        name: name.to_owned(),
    })
}

fn validate_kex<'a, I>(names: I) -> Result<()>
where
    I: Iterator<Item = &'a str> + Clone,
{
    validate(names, kex_name)
}

fn validate_host_keys<'a, I>(names: I) -> Result<()>
where
    I: Iterator<Item = &'a str> + Clone,
{
    validate(names, host_key_name)
}

fn kex<'a, I>(names: I) -> Result<Box<[kex::Name]>>
where
    I: Iterator<Item = &'a str> + Clone,
{
    let defaults = Preferred::default();
    let baseline = defaults
        .kex
        .iter()
        .copied()
        .filter(|name| kex::Name::try_from(name.as_ref()).is_ok());
    compile(names, baseline, kex_name, |value, name| {
        value.as_ref() == name
    })
    .map(Vec::into_boxed_slice)
}

fn host_keys<'a, I>(names: I) -> Result<HostKeys>
where
    I: Iterator<Item = &'a str> + Clone,
{
    let defaults = Preferred::default();
    let baseline = defaults
        .host_key_certificates
        .iter()
        .cloned()
        .map(|algorithm| HostKey {
            algorithm,
            certificate: true,
        })
        .chain(defaults.key.iter().cloned().map(|algorithm| HostKey {
            algorithm,
            certificate: false,
        }));
    let values = compile(names, baseline, host_key_name, HostKey::matches)?;
    let mut plain = false;
    for value in &values {
        if plain && value.certificate {
            return Err(Error::HostCertificateOrder);
        }
        plain |= !value.certificate;
    }

    Ok(HostKeys(values.into_boxed_slice()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kex_lists_replace_append_remove_and_prepend() -> Result<()> {
        let preferred = Preferred::default();
        let baseline: Vec<_> = preferred
            .kex
            .iter()
            .copied()
            .filter(|name| kex::Name::try_from(name.as_ref()).is_ok())
            .collect();
        let extra = kex::Name::try_from("diffie-hellman-group1-sha1").unwrap();

        assert_eq!(
            kex(["curve25519-sha256", "diffie-hellman-group14-sha256"].into_iter())?.as_ref(),
            &[kex::CURVE25519, kex::DH_G14_SHA256]
        );

        let appended = kex(["+diffie-hellman-group1-sha1"].into_iter())?;
        assert_eq!(&appended[..baseline.len()], baseline);
        assert_eq!(appended.last(), Some(&extra));

        let prepended = kex(["^diffie-hellman-group1-sha1"].into_iter())?;
        assert_eq!(prepended[0], extra);
        assert_eq!(&prepended[1..], baseline);

        assert_eq!(
            kex(["-unknown-but-literal"].into_iter())?.as_ref(),
            baseline
        );

        let removed = kex(["-curve25519-sha256"].into_iter())?;
        let expected: Vec<_> = baseline
            .into_iter()
            .filter(|name| *name != kex::CURVE25519)
            .collect();
        assert_eq!(removed.as_ref(), expected);

        Ok(())
    }

    #[test]
    fn explicit_host_keys_replace_both_certificate_and_plain_lists() -> Result<()> {
        for (names, plain, certificates) in [
            (&["ssh-ed25519"][..], &[Algorithm::Ed25519][..], &[][..]),
            (
                &["ssh-ed25519-cert-v01@openssh.com"][..],
                &[][..],
                &[Algorithm::Ed25519][..],
            ),
            (
                &["ssh-ed25519-cert-v01@openssh.com", "ssh-ed25519"][..],
                &[Algorithm::Ed25519][..],
                &[Algorithm::Ed25519][..],
            ),
        ] {
            let algorithms = Algorithms {
                host_keys: Some(host_keys(names.iter().copied())?),
                ..Algorithms::default()
            };
            let mut preferred = Preferred {
                host_key_certificates: vec![Algorithm::Ed25519].into(),
                ..Preferred::default()
            };
            algorithms.apply(&mut preferred);

            assert_eq!(preferred.key.as_ref(), plain);
            assert_eq!(preferred.host_key_certificates.as_ref(), certificates);
        }

        Ok(())
    }

    #[test]
    fn configured_kex_preserves_transport_extension_signals() -> Result<()> {
        let algorithms = Algorithms {
            kex: Some(kex(["curve25519-sha256"].into_iter())?),
            ..Algorithms::default()
        };
        let defaults = Preferred::default();
        let signals = defaults
            .kex
            .iter()
            .copied()
            .filter(|name| kex::Name::try_from(name.as_ref()).is_err());
        let mut preferred = Preferred::default();
        algorithms.apply(&mut preferred);

        assert!(
            preferred
                .kex
                .iter()
                .copied()
                .eq(std::iter::once(kex::CURVE25519).chain(signals))
        );

        Ok(())
    }

    #[test]
    fn host_key_modifiers_use_transport_defaults_and_preserve_order() -> Result<()> {
        let defaults = Preferred::default();
        let prepended = host_keys(["^rsa-sha2-512"].into_iter())?;
        let mut preferred = Preferred::default();
        Algorithms {
            host_keys: Some(prepended),
            ..Algorithms::default()
        }
        .apply(&mut preferred);

        assert_eq!(
            preferred.key[0],
            Algorithm::Rsa {
                hash: Some(openrussh_client::russh::keys::ssh_key::HashAlg::Sha512)
            }
        );
        assert_eq!(preferred.key.len(), defaults.key.len());
        assert!(preferred.host_key_certificates.is_empty());

        let removed = host_keys(["-ssh-ed25519"].into_iter())?;
        assert!(
            !removed
                .0
                .iter()
                .any(|key| key.algorithm == Algorithm::Ed25519)
        );

        let appended = host_keys(["+ssh-dss"].into_iter())?;
        assert_eq!(appended.0.last().unwrap().algorithm, Algorithm::Dsa);

        assert_eq!(
            host_keys(["-unknown-but-literal"].into_iter())?.0.len(),
            defaults.key.len()
        );

        Ok(())
    }

    #[test]
    fn supported_certificate_names_map_without_changing_algorithms() -> Result<()> {
        use openrussh_client::russh::keys::key::ALL_KEY_TYPES;

        for algorithm in ALL_KEY_TYPES {
            let plain = host_key_name(algorithm.as_str())?;
            let certificate = host_key_name(&algorithm.to_certificate_type())?;

            assert_eq!(&plain.algorithm, algorithm);
            assert!(!plain.certificate);
            assert_eq!(&certificate.algorithm, algorithm);
            assert!(certificate.certificate);
        }

        Ok(())
    }

    #[test]
    fn invalid_names_wildcards_empty_lists_and_unrepresentable_orders_fail() {
        for value in [
            "",
            "unknown",
            "+",
            "-curve*",
            "^curve?",
            "[curve25519-sha256]",
        ] {
            assert!(kex([value].into_iter()).is_err(), "{value}");
        }
        for value in ["", "unknown", "+", "-ssh-*", "^ssh-?", "[ssh-ed25519]"] {
            assert!(host_keys([value].into_iter()).is_err(), "{value}");
        }
        assert!(kex(std::iter::empty()).is_err());
        assert!(host_keys(std::iter::empty()).is_err());
        assert!(
            host_keys(["ssh-ed25519", "ssh-ed25519-cert-v01@openssh.com"].into_iter()).is_err()
        );
        assert!(host_keys(["+ssh-ed25519-cert-v01@openssh.com"].into_iter()).is_err());
    }
}
