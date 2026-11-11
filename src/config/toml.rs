use super::Options;
use super::formats::{PathMode, Profiles};

use openrussh_client::config::Chain;

use anyhow::Result;

pub(super) struct ParsedConfig<'src> {
    profiles: Profiles<'src>,
}

impl<'src> ParsedConfig<'src> {
    pub(super) fn parse(text: &'src str) -> Result<Self> {
        Ok(Self {
            profiles: ::toml::from_str(text)?,
        })
    }

    pub(super) fn resolve(&self, options: &Options) -> Result<Chain> {
        self.profiles.resolve(options, PathMode::Expand)
    }
}

#[cfg(test)]
pub(super) fn query(text: &str, options: &Options) -> Result<Chain> {
    ParsedConfig::parse(text)?.resolve(options)
}
