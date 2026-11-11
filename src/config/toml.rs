use super::formats::{PathMode, Profiles};
use super::{LoadError, Options, Resolved};

#[cfg(test)]
use openrussh_client::config::Chain;

use anyhow::Result;

pub(super) struct ParsedConfig<'src> {
    profiles: Profiles<'src>,
}

impl<'src> ParsedConfig<'src> {
    pub(super) fn parse(text: &'src str) -> Result<Self> {
        let profiles: Profiles<'src> = ::toml::from_str(text)?;
        profiles.require_named_jumps().map_err(|error| LoadError {
            log_level: profiles.log_level(),
            error,
        })?;

        Ok(Self { profiles })
    }

    pub(super) fn resolve(&self, options: &Options) -> Result<Resolved> {
        self.profiles.resolve(options, PathMode::Expand)
    }
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &Options) -> Result<Chain> {
    ParsedConfig::parse(text)?
        .resolve(options)
        .map(|resolved| resolved.chain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_rejects_inline_jumps_in_defaults_and_unused_hosts() {
        for source in [
            "[default]\nproxy-jump = {host-name = 'jump'}",
            "[hosts.unused]\nproxy-jump = [{host-name = 'jump'}]",
            "[hosts.unused]\nproxy-jump = ['alias', {host-name = 'jump'}]",
            "[hosts.unused]\nproxy-jump = [[]]",
        ] {
            assert!(ParsedConfig::parse(source).is_err(), "{source}");
        }
    }
}
