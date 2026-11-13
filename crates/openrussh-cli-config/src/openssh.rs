use super::{Error, ResolveConfig, ResolveOptions, Resolved, Result};
use openrussh_client::ssh2_config::{ParseRule, SshConfig};

use std::fs::File;
use std::io::BufReader;

pub(super) struct ParsedConfig {
    config: SshConfig,
}

impl ParsedConfig {
    pub(super) fn parse(f: File, options: &ResolveOptions<'_>) -> Result<Self> {
        let config = {
            let mut rdr = BufReader::new(f);
            overrides(options).parse(&mut rdr, ParseRule::ALLOW_UNSUPPORTED_FIELDS)?
        };

        Ok(Self { config })
    }
}

impl ResolveConfig for ParsedConfig {
    fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        use openrussh_client::config::Target;

        Target {
            host: &options.destination.host,
            user: options.destination.user.as_deref(),
            port: options.destination.port,
        }
        .query(&self.config)
        .map(Resolved::from)
        .map_err(Error::OpenSshResolve)
    }
}

fn overrides(options: &ResolveOptions<'_>) -> SshConfig {
    use openrussh_client::ssh2_config::{DefaultAlgorithms, Host, HostClause, HostParams};

    if options.destination.user.is_some()
        || options.destination.port.is_some()
        || options.identities.is_some()
        || options.proxy_jump.is_some()
    {
        let mut params = HostParams::new(&DefaultAlgorithms::empty());
        params.user.clone_from(&options.destination.user);
        params.port = options.destination.port;
        params.identity_file = options.identities.map(<[std::path::PathBuf]>::to_vec);
        params.proxy_jump = options.proxy_jump.map(<[String]>::to_vec);
        SshConfig::from_hosts(vec![Host::new(
            vec![HostClause::new(options.destination.host.clone(), false)],
            params,
        )])
    } else {
        SshConfig::default()
    }
}
