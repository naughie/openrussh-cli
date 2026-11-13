use crate::{ResolveConfig, ResolveOptions, Resolved, Result};

mod profiles;
use profiles::Profiles;

mod jumps;
mod paths;
mod values;

#[cfg(test)]
use openrussh_client::config::Chain;

pub(super) struct ParsedConfig<'src> {
    profiles: Profiles<'src>,
}

impl<'src> ParsedConfig<'src> {
    pub(super) fn parse(text: &'src str) -> Result<Self> {
        let profiles: Profiles<'src> = ::toml::from_str(text)?;
        profiles
            .require_named_jumps()
            .map_err(|error| error.with_log_filter(profiles.log_filter()))?;

        Ok(Self { profiles })
    }
}

impl ResolveConfig for ParsedConfig<'_> {
    fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        self.profiles.resolve(options)
    }
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &ResolveOptions<'_>) -> Result<Chain> {
    ParsedConfig::parse(text)?
        .resolve(options)
        .map(|resolved| resolved.chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogFilter;

    #[test]
    fn toml_rejects_inline_jumps_in_defaults_and_unused_hosts() {
        for source in [
            "[default]\nproxy-jump = {host-name = 'jump'}",
            "[hosts.unused]\nproxy-jump = [{host-name = 'jump'}]",
            "[hosts.unused]\nproxy-jump = ['alias', {host-name = 'jump'}]",
            "[hosts.unused]\nproxy-jump = [[]]",
        ] {
            let source = format!("[meta]\nlog-level = 'none'\n{source}");
            let error = ParsedConfig::parse(&source)
                .err()
                .expect("Inline TOML jumps must be rejected");
            assert!(matches!(error.log_filter(), Some(LogFilter::Disabled)));
            assert_eq!(
                error.to_string(),
                "TOML proxy-jump must be a string or an array of strings"
            );
        }
    }

    #[test]
    fn malformed_inline_jumps_fail_before_logging_metadata_is_retained() {
        for value in [
            "{unknown = true}",
            "{port = -1}",
            "{user = ['alice']}",
            "{proxy-jump = [{unknown = true}]}",
        ] {
            let source =
                format!("[meta]\nlog-level = 'none'\n[hosts.unused]\nproxy-jump = {value}");
            let error = ParsedConfig::parse(&source)
                .err()
                .expect("Malformed inline TOML jumps must be rejected");

            assert!(matches!(&error, crate::Error::Toml(_)), "{value}");
            assert!(error.log_filter().is_none(), "{value}");
        }
    }
}

#[cfg(test)]
mod resolution_tests;
