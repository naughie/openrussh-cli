use openrussh_client::config::{Chain, Host};
use openrussh_client::russh::keys::ssh_key::Algorithm;
use openrussh_client::russh::{Preferred, kex};

use anyhow::{Context as _, Result, anyhow, ensure};

#[derive(Debug, Default)]
pub(crate) struct Algorithms {
    pub(super) kex: Option<Box<[kex::Name]>>,
    pub(super) host_keys: Option<HostKeys>,
}

#[derive(Debug, Default)]
pub(crate) struct Policies {
    pub(super) target: Algorithms,
    pub(super) bastions: Option<Box<[Algorithms]>>,
}

impl Policies {
    pub(crate) fn get(&self, chain: &Chain, host: &Host) -> Result<&Algorithms> {
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
            .context("SSH host does not belong to the configured connection chain")?;
        match self.bastions.as_deref() {
            Some(policies) => policies
                .get(index)
                .context("Missing algorithm policy for SSH hop"),
            None => Ok(&EMPTY),
        }
    }
}

impl Algorithms {
    pub(super) fn is_empty(&self) -> bool {
        self.kex.is_none() && self.host_keys.is_none()
    }

    pub(crate) fn has_host_keys(&self) -> bool {
        self.host_keys.is_some()
    }

    pub(crate) fn apply(&self, preferred: &mut Preferred) {
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

fn list<'a>(
    mut names: impl Iterator<Item = &'a str> + Clone,
) -> Result<(Mode, impl Iterator<Item = &'a str> + Clone)> {
    let first = names.next().context("Algorithm list must not be empty")?;
    let (mode, first) = match first.as_bytes().first() {
        Some(b'+') => (Mode::Append, &first[1..]),
        Some(b'-') => (Mode::Remove, &first[1..]),
        Some(b'^') => (Mode::Prepend, &first[1..]),
        _ => (Mode::Replace, first),
    };

    Ok((mode, std::iter::once(first).chain(names)))
}

fn validate<'a, T>(
    names: impl Iterator<Item = &'a str> + Clone,
    parse: fn(&str) -> Result<T>,
) -> Result<()> {
    let (mode, names) = list(names)?;
    for name in names {
        literal_name(name)?;
        if !matches!(mode, Mode::Remove) {
            parse(name)?;
        }
    }
    Ok(())
}

fn compile<'a, T: Clone + PartialEq>(
    names: impl Iterator<Item = &'a str> + Clone,
    mut baseline: impl Iterator<Item = T>,
    parse: fn(&str) -> Result<T>,
    matches_name: fn(&T, &str) -> bool,
) -> Result<Vec<T>> {
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
    ensure!(!values.is_empty(), "Algorithm list must not be empty");

    Ok(values)
}

fn literal_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.bytes().all(|byte| byte.is_ascii_graphic()
                && !matches!(byte, b',' | b'*' | b'?' | b'[' | b']' | b'!')),
        "Invalid algorithm name (wildcards are not supported): {name}"
    );
    Ok(())
}

fn kex_name(name: &str) -> Result<kex::Name> {
    kex::Name::try_from(name).map_err(|_| anyhow!("Unknown key-exchange algorithm: {name}"))
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

    Err(anyhow!("Unknown host-key algorithm: {name}"))
}

pub(super) fn validate_kex<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Result<()> {
    validate(names, kex_name)
}

pub(super) fn validate_host_keys<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Result<()> {
    validate(names, host_key_name)
}

pub(super) fn kex<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Result<Box<[kex::Name]>> {
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

pub(super) fn host_keys<'a>(names: impl Iterator<Item = &'a str> + Clone) -> Result<HostKeys> {
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
        ensure!(
            !plain || !value.certificate,
            "Host certificate algorithms must precede plain host-key algorithms"
        );
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
