use openrussh_client::config::{Chain, RequestTty};
use openrussh_client::russh::Preferred;
use openrussh_client::ssh2_config::SshConfig;

use anyhow::{Context as _, Result};

use std::path::{Path, PathBuf};

pub(crate) struct Config {
    pub destination: Destination,
    pub config_file: Option<PathBuf>,
    pub identities: Option<Vec<PathBuf>>,
    pub proxy_jump: Option<Vec<String>>,
    pub tty: Option<RequestTty>,
    pub session: Session,
    pub stdin_null: bool,
    pub escape_char: Option<u8>,
    pub local_forwards: Option<Vec<LocalForward>>,
    pub compression: bool,
    pub ciphers: Option<String>,
    pub macs: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Destination {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Session {
    Shell,
    Exec {
        prog: String,
        args: Option<Vec<String>>,
    },
    None,
    Forward {
        host: String,
        port: u16,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct LocalForward {
    pub bind: LocalBind,
    pub target: ForwardTarget,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LocalBind {
    Tcp { host: BindHost, port: u16 },
    Unix(PathBuf),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BindHost {
    Loopback,
    All,
    Host(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForwardTarget {
    Tcp { host: String, port: u16 },
    Unix(String),
}

impl Config {
    pub fn chain(&self) -> Result<Chain> {
        use openrussh_client::ssh2_config::{
            DefaultAlgorithms, Host, HostClause, HostParams, ParseRule,
        };

        use std::fs::File;
        use std::io::{BufReader, ErrorKind};

        let conf = if self.destination.user.is_some()
            || self.destination.port.is_some()
            || self.identities.is_some()
            || self.proxy_jump.is_some()
        {
            let mut params = HostParams::new(&DefaultAlgorithms::empty());
            params.user.clone_from(&self.destination.user);
            params.port = self.destination.port;
            params.identity_file.clone_from(&self.identities);
            params.proxy_jump.clone_from(&self.proxy_jump);
            SshConfig::from_hosts(vec![Host::new(
                vec![HostClause::new(self.destination.host.clone(), false)],
                params,
            )])
        } else {
            SshConfig::default()
        };

        let default_path;
        let path = if let Some(path) = &self.config_file {
            if path == Path::new("none") {
                return self.query(&conf);
            }
            path
        } else {
            default_path = std::env::home_dir()
                .context("Could not find the home directory")?
                .join(".ssh/config");
            &default_path
        };

        let conf = match File::open(path) {
            Ok(f) => {
                let mut rdr = BufReader::new(f);
                conf.parse(&mut rdr, ParseRule::ALLOW_UNSUPPORTED_FIELDS)
                    .with_context(|| format!("Could not read SSH config {}", path.display()))?
            }
            Err(e) if self.config_file.is_none() && e.kind() == ErrorKind::NotFound => conf,
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("Could not open SSH config {}", path.display()));
            }
        };
        self.query(&conf)
    }

    fn query(&self, conf: &SshConfig) -> Result<Chain> {
        use openrussh_client::config::Target;

        let mut chain = Target {
            host: &self.destination.host,
            user: self.destination.user.as_deref(),
            port: self.destination.port,
        }
        .query(conf)
        .context("Could not resolve the SSH connection settings")?;

        if let Some(tty) = self.tty {
            chain.target.tty = tty;
        }

        Ok(chain)
    }

    pub fn preferred(&self) -> Result<Preferred> {
        use openrussh_client::russh::{cipher, compression, mac};

        use anyhow::anyhow;

        use std::borrow::Cow;

        let mut preferred = Preferred::default();

        if self.compression {
            preferred.compression = Cow::Borrowed(&[
                compression::ZLIB,
                compression::ZLIB_LEGACY,
                compression::NONE,
            ]);
        }

        if let Some(ciphers) = &self.ciphers {
            preferred.cipher = ciphers
                .split(',')
                .map(|s| cipher::Name::try_from(s).map_err(|()| anyhow!("Unknown cipher: {s}")))
                .collect::<Result<Vec<_>>>()?
                .into();
        }

        if let Some(macs) = &self.macs {
            preferred.mac = macs
                .split(',')
                .map(|s| mac::Name::try_from(s).map_err(|()| anyhow!("Unknown MAC: {s}")))
                .collect::<Result<Vec<_>>>()?
                .into();
        }

        Ok(preferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn config(text: &str) -> Result<(TempDir, Config)> {
        let dir = TempDir::new()?;
        let path = dir.path().join("config");
        std::fs::write(&path, text)?;

        Ok((
            dir,
            Config {
                destination: Destination {
                    host: "sample".to_owned(),
                    user: None,
                    port: None,
                },
                config_file: Some(path),
                identities: None,
                proxy_jump: None,
                tty: None,
                session: Session::Shell,
                stdin_null: false,
                escape_char: Some(b'~'),
                local_forwards: None,
                compression: false,
                ciphers: None,
                macs: None,
            },
        ))
    }

    #[test]
    fn command_line_values_precede_file_values() -> Result<()> {
        let (_dir, mut conf) = config(
            "Host sample\n User configured\n Port 2200\n IdentityFile /keys/configured\n RequestTTY force\n",
        )?;
        conf.destination.user = Some("alice".to_owned());
        conf.destination.port = Some(2222);
        conf.identities = Some(vec!["/keys/first".into(), "/keys/second".into()]);
        conf.tty = Some(RequestTty::Never);

        let chain = conf.chain()?;

        assert_eq!(chain.target.dest.user, "alice");

        assert_eq!(chain.target.dest.port, 2222);

        assert_eq!(
            chain.target.auth.identities,
            Some(vec![
                "/keys/first".into(),
                "/keys/second".into(),
                "/keys/configured".into()
            ])
        );

        assert_eq!(chain.target.tty, RequestTty::Never);

        Ok(())
    }

    #[test]
    fn includes_keep_their_host_scope_with_cli_overrides() -> Result<()> {
        let (dir, mut conf) = config("")?;
        let included = dir.path().join("included");
        std::fs::write(
            &included,
            "Port 2200\nIdentityFile /keys/included\nHost *\n User scoped\n",
        )?;
        std::fs::write(
            conf.config_file.as_ref().unwrap(),
            format!(
                "Host sample\n Include {}\nHost other\n Port 2300\nHost *\n User default\n",
                included.display()
            ),
        )?;
        conf.identities = Some(vec!["/keys/cli".into()]);

        let chain = conf.chain()?;

        assert_eq!(chain.target.dest.user, "scoped");

        assert_eq!(chain.target.dest.port, 2200);

        assert_eq!(
            chain.target.auth.identities,
            Some(vec!["/keys/cli".into(), "/keys/included".into()])
        );
        conf.destination.host = "other".to_owned();

        let chain = conf.chain()?;

        assert_eq!(chain.target.dest.user, "default");

        assert_eq!(chain.target.dest.port, 2300);

        assert_eq!(chain.target.auth.identities, Some(vec!["/keys/cli".into()]));

        Ok(())
    }

    #[test]
    fn explicit_jump_chain_replaces_the_file_chain() -> Result<()> {
        let (_dir, mut conf) = config(
            "Host sample\n User target\n ProxyJump first\nHost first\n User first-user\nHost second\n User second-user\n",
        )?;
        conf.proxy_jump = Some(vec!["second".to_owned()]);

        let chain = conf.chain()?;
        let bastions = chain.bastions.context("Missing jump host")?;

        assert_eq!(bastions.len(), 1);

        assert_eq!(bastions[0].dest.name, "second");

        assert_eq!(bastions[0].dest.user, "second-user");

        Ok(())
    }

    #[test]
    fn config_none_keeps_cli_settings_without_opening_a_file() -> Result<()> {
        let (_dir, mut conf) = config("Port invalid")?;
        conf.config_file = Some("none".into());
        conf.destination.user = Some("alice".to_owned());
        conf.destination.port = Some(2200);
        conf.identities = Some(vec!["/keys/not-created".into()]);

        let chain = conf.chain()?;

        assert_eq!(chain.target.dest.name, "sample");

        assert_eq!(chain.target.dest.user, "alice");

        assert_eq!(chain.target.dest.port, 2200);

        assert_eq!(
            chain.target.auth.identities,
            Some(vec!["/keys/not-created".into()])
        );

        Ok(())
    }

    #[test]
    fn explicit_missing_and_malformed_config_errors_are_preserved() -> Result<()> {
        let (dir, mut conf) = config("Host sample\n Port invalid\n")?;
        let e = conf.chain().unwrap_err();

        assert!(format!("{e:#}").contains("Could not read SSH config"));
        conf.config_file = Some(dir.path().join("missing"));

        let e = conf.chain().unwrap_err();

        assert!(format!("{e:#}").contains("Could not open SSH config"));

        Ok(())
    }

    #[test]
    fn transport_settings_in_the_file_do_not_extend_library_support() -> Result<()> {
        let (_dir, conf) = config(
            "Host sample\n User alice\n Compression yes\n Ciphers aes128-ctr\n MACs hmac-sha2-256\n LocalForward 8080 localhost:80\n",
        )?;
        conf.chain()?;

        let preferred = conf.preferred()?;
        let default = Preferred::default();

        assert_eq!(preferred.compression, default.compression);

        assert_eq!(preferred.cipher, default.cipher);

        assert_eq!(preferred.mac, default.mac);

        assert!(conf.local_forwards.is_none());

        Ok(())
    }

    #[test]
    fn command_line_algorithms_keep_the_requested_order() -> Result<()> {
        use openrussh_client::russh::{cipher, compression, mac};

        let (_dir, mut conf) = config("")?;
        conf.compression = true;
        conf.ciphers = Some("aes256-ctr,aes128-ctr".to_owned());
        conf.macs = Some("hmac-sha2-512,hmac-sha2-256".to_owned());

        let preferred = conf.preferred()?;

        assert_eq!(
            &*preferred.compression,
            &[
                compression::ZLIB,
                compression::ZLIB_LEGACY,
                compression::NONE
            ]
        );

        assert_eq!(
            &*preferred.cipher,
            &[cipher::AES_256_CTR, cipher::AES_128_CTR]
        );

        assert_eq!(&*preferred.mac, &[mac::HMAC_SHA512, mac::HMAC_SHA256]);

        Ok(())
    }

    #[test]
    fn invalid_algorithm_names_are_rejected() -> Result<()> {
        let (_dir, mut conf) = config("")?;

        for s in ["", "unknown", "aes128-ctr,", ",aes128-ctr"] {
            conf.ciphers = Some(s.to_owned());

            assert!(conf.preferred().is_err(), "{s}");
        }
        conf.ciphers = None;

        for s in ["", "unknown", "hmac-sha2-256,", ",hmac-sha2-256"] {
            conf.macs = Some(s.to_owned());

            assert!(conf.preferred().is_err(), "{s}");
        }

        Ok(())
    }
}
