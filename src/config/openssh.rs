use super::Options;

use openrussh_client::config::Chain;
use openrussh_client::ssh2_config::{ParseRule, SshConfig};

use anyhow::{Context as _, Result};

use std::fs::File;
use std::io::BufReader;

pub(super) struct ParsedConfig {
    config: SshConfig,
}

impl ParsedConfig {
    pub(super) fn parse(f: File, options: &Options) -> Result<Self> {
        let config = {
            let mut rdr = BufReader::new(f);
            overrides(options).parse(&mut rdr, ParseRule::ALLOW_UNSUPPORTED_FIELDS)?
        };

        Ok(Self { config })
    }

    pub(super) fn resolve(&self, options: &Options) -> Result<Chain> {
        use openrussh_client::config::Target;

        Target {
            host: &options.destination.host,
            user: options.destination.user.as_deref(),
            port: options.destination.port,
        }
        .query(&self.config)
        .context("Could not resolve the SSH connection settings")
    }
}

fn overrides(options: &Options) -> SshConfig {
    use openrussh_client::ssh2_config::{DefaultAlgorithms, Host, HostClause, HostParams};

    if options.destination.user.is_some()
        || options.destination.port.is_some()
        || options.identities.is_some()
        || options.proxy_jump.is_some()
    {
        let mut params = HostParams::new(&DefaultAlgorithms::empty());
        params.user.clone_from(&options.destination.user);
        params.port = options.destination.port;
        params.identity_file.clone_from(&options.identities);
        params.proxy_jump.clone_from(&options.proxy_jump);
        SshConfig::from_hosts(vec![Host::new(
            vec![HostClause::new(options.destination.host.clone(), false)],
            params,
        )])
    } else {
        SshConfig::default()
    }
}
