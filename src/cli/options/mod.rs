use super::{Args, HostOverrides};

use openrussh_cli_config::{AddressFamily, IdentityAgent, LogFilter};
use openrussh_client::config::RequestTty;

use anyhow::{Context as _, Result, bail, ensure};

use std::path::PathBuf;

mod words;

#[derive(Default)]
pub(super) struct Options {
    pub host: HostOverrides,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identities: Option<Vec<PathBuf>>,
    pub proxy_jump: Option<Vec<String>>,
    pub tty: Option<RequestTty>,
    pub address_family: Option<AddressFamily>,
    pub send_env: Option<Vec<String>>,
    pub forwards: Option<Vec<String>>,
    pub gateway_ports: Option<bool>,
    pub stdin_null: Option<bool>,
    pub compression: Option<bool>,
    pub ciphers: Option<String>,
    pub macs: Option<String>,
    pub escape: Option<String>,
    pub no_session: Option<bool>,
    pub remote_command: Option<String>,
    pub verbose: bool,
}

enum Argument {
    Option(String),
    User(String),
    Port(u16),
    Identity(PathBuf),
    Jump(String),
    NoSession,
    Tty(bool),
    Family(AddressFamily),
    Forward(String),
}

impl Options {
    pub(super) fn from_args(args: &mut Args, matches: &clap::ArgMatches) -> Result<Self> {
        fn append<T>(
            events: &mut Vec<(usize, Argument)>,
            matches: &clap::ArgMatches,
            id: &str,
            values: impl IntoIterator<Item = T>,
            wrap: impl Fn(T) -> Argument,
        ) {
            if let Some(indices) = matches.indices_of(id) {
                events.extend(indices.zip(values).map(|(i, v)| (i, wrap(v))));
            }
        }

        let count = args.options.len()
            + args.user.as_ref().map_or(0, Vec::len)
            + args.port.as_ref().map_or(0, Vec::len)
            + args.identities.as_ref().map_or(0, Vec::len)
            + args.jump.as_ref().map_or(0, Vec::len)
            + usize::from(args.no_session)
            + args.tty.len()
            + args.no_tty.len()
            + args.ipv4.len()
            + args.ipv6.len()
            + args.forwards.as_ref().map_or(0, Vec::len);
        let mut events = Vec::with_capacity(count);
        append(
            &mut events,
            matches,
            "options",
            std::mem::take(&mut args.options),
            Argument::Option,
        );
        append(
            &mut events,
            matches,
            "user",
            args.user.take().into_iter().flatten(),
            Argument::User,
        );
        append(
            &mut events,
            matches,
            "port",
            args.port.take().into_iter().flatten(),
            Argument::Port,
        );
        append(
            &mut events,
            matches,
            "identities",
            args.identities.take().into_iter().flatten(),
            Argument::Identity,
        );
        append(
            &mut events,
            matches,
            "jump",
            args.jump.take().into_iter().flatten(),
            Argument::Jump,
        );
        append(
            &mut events,
            matches,
            "no_session",
            args.no_session.then_some(()),
            |_| Argument::NoSession,
        );
        append(
            &mut events,
            matches,
            "tty",
            std::mem::take(&mut args.tty),
            |_| Argument::Tty(true),
        );
        append(
            &mut events,
            matches,
            "no_tty",
            std::mem::take(&mut args.no_tty),
            |_| Argument::Tty(false),
        );
        append(
            &mut events,
            matches,
            "ipv4",
            std::mem::take(&mut args.ipv4),
            |_| Argument::Family(AddressFamily::Inet),
        );
        append(
            &mut events,
            matches,
            "ipv6",
            std::mem::take(&mut args.ipv6),
            |_| Argument::Family(AddressFamily::Inet6),
        );
        append(
            &mut events,
            matches,
            "forwards",
            args.forwards.take().into_iter().flatten(),
            Argument::Forward,
        );
        events.sort_unstable_by_key(|(i, _)| *i);

        let mut options = Self::default();
        for (_, arg) in events {
            match arg {
                Argument::Option(line) => options.apply(&line)?,
                Argument::User(user) => {
                    options.user.get_or_insert(user);
                }
                Argument::Port(port) => {
                    options.port.get_or_insert(port);
                }
                Argument::Identity(path) => options.identity(path),
                Argument::Jump(jump) => {
                    ensure!(
                        options.proxy_jump.is_none(),
                        "Only one ProxyJump/-J option is permitted"
                    );
                    options.proxy_jump = Some(jumps(&jump));
                }
                Argument::NoSession => {
                    ensure!(
                        options.no_session.is_none(),
                        "Cannot specify -N with an earlier SessionType"
                    );
                    options.no_session = Some(true);
                }
                Argument::Tty(false) => options.tty = Some(RequestTty::Never),
                Argument::Tty(true) => {
                    options.tty = Some(if options.tty == Some(RequestTty::IfTty) {
                        RequestTty::Always
                    } else {
                        RequestTty::IfTty
                    })
                }
                Argument::Family(family) => options.address_family = Some(family),
                Argument::Forward(forward) => {
                    options.forwards.get_or_insert_with(Vec::new).push(forward)
                }
            }
        }
        Ok(options)
    }

    fn identity(&mut self, path: PathBuf) {
        let paths = self.identities.get_or_insert_with(Vec::new);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    fn apply(&mut self, line: &str) -> Result<()> {
        let line = line.trim_start();
        let end = line
            .find(|c: char| c == '=' || c.is_ascii_whitespace())
            .unwrap_or(line.len());
        let key = &line[..end];
        let rest = line[end..].trim_start();
        let rest = rest.strip_prefix('=').unwrap_or(rest).trim_start();
        ensure!(
            !key.is_empty() && !rest.is_empty(),
            "Missing value in -o option: {line}"
        );
        let mut values = words::split(rest)?;
        if key.eq_ignore_ascii_case("RemoteCommand") {
            self.remote_command.get_or_insert_with(|| rest.to_owned());
            return Ok(());
        }
        ensure!(
            !key.is_empty() && !values.is_empty(),
            "Missing value in -o option: {line}"
        );

        if key.eq_ignore_ascii_case("SendEnv") {
            ensure!(
                values.iter().all(|s| !s.is_empty() && !s.contains('=')),
                "Invalid SendEnv pattern"
            );
            self.send_env.get_or_insert_with(Vec::new).extend(values);
            return Ok(());
        }
        if key.eq_ignore_ascii_case("LocalForward") {
            ensure!(
                values.len() == 2,
                "LocalForward requires a listen endpoint and a target endpoint"
            );
            let target = values.pop().unwrap();
            let listen = values.pop().unwrap();
            self.forwards
                .get_or_insert_with(Vec::new)
                .push(format!("{listen}:{target}"));
            return Ok(());
        }
        ensure!(values.len() == 1, "Extra arguments in -o {key}");
        let value = values.pop().unwrap();
        ensure!(!value.is_empty(), "Missing value in -o {key}");

        if key.eq_ignore_ascii_case("HostName") {
            self.host.host_name.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("User") {
            self.user.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("Port") {
            let port = if value == "ssh" {
                22
            } else {
                value.parse().context("Invalid Port")?
            };
            ensure!(port != 0, "Port must be between 1 and 65535");
            self.port.get_or_insert(port);
        } else if key.eq_ignore_ascii_case("IdentityFile") {
            self.identity(value.into());
        } else if key.eq_ignore_ascii_case("CertificateFile") {
            let path = PathBuf::from(value);
            ensure!(
                self.host
                    .certificate_file
                    .as_ref()
                    .is_none_or(|old| *old == path),
                "Only one CertificateFile is supported"
            );
            self.host.certificate_file.get_or_insert(path);
        } else if key.eq_ignore_ascii_case("IdentityAgent") {
            self.host.identity_agent.get_or_insert(if value == "none" {
                IdentityAgent::Disabled
            } else {
                IdentityAgent::Socket(value.into())
            });
        } else if key.eq_ignore_ascii_case("IdentitiesOnly") {
            self.host.identities_only.get_or_insert(boolean(&value)?);
        } else if key.eq_ignore_ascii_case("RequestTTY") {
            let tty = if value.eq_ignore_ascii_case("force") {
                RequestTty::Always
            } else if value.eq_ignore_ascii_case("auto") {
                RequestTty::ForShell
            } else if boolean(&value)? {
                RequestTty::IfTty
            } else {
                RequestTty::Never
            };
            self.tty.get_or_insert(tty);
        } else if key.eq_ignore_ascii_case("ProxyJump") {
            self.proxy_jump.get_or_insert_with(|| jumps(&value));
        } else if key.eq_ignore_ascii_case("AddressFamily") {
            let family = if value.eq_ignore_ascii_case("any") {
                AddressFamily::Any
            } else if value.eq_ignore_ascii_case("inet") {
                AddressFamily::Inet
            } else if value.eq_ignore_ascii_case("inet6") {
                AddressFamily::Inet6
            } else {
                bail!("Invalid AddressFamily: {value}");
            };
            self.address_family.get_or_insert(family);
        } else if key.eq_ignore_ascii_case("Compression") {
            ensure!(
                value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("no"),
                "Compression must be yes or no"
            );
            self.compression.get_or_insert(boolean(&value)?);
        } else if key.eq_ignore_ascii_case("StdinNull") {
            self.stdin_null.get_or_insert(boolean(&value)?);
        } else if key.eq_ignore_ascii_case("GatewayPorts") {
            self.gateway_ports.get_or_insert(boolean(&value)?);
        } else if key.eq_ignore_ascii_case("Ciphers") {
            self.ciphers.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("MACs") {
            self.macs.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("EscapeChar") {
            super::parse_escape(&value)?;
            self.escape.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("KexAlgorithms") {
            self.host.kex_algorithms.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("HostKeyAlgorithms") {
            self.host.host_key_algorithms.get_or_insert(value);
        } else if key.eq_ignore_ascii_case("UserKnownHostsFile") {
            self.host.known_hosts.get_or_insert(value.into());
        } else if key.eq_ignore_ascii_case("LogLevel") {
            let (level, verbose) = log_level(&value)?;
            if self.host.log_filter.is_none() {
                self.host.log_filter = Some(level);
                self.verbose = verbose;
            }
        } else if key.eq_ignore_ascii_case("SessionType") {
            ensure!(
                value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("default"),
                "Unsupported SessionType: {value}"
            );
            self.no_session
                .get_or_insert(value.eq_ignore_ascii_case("none"));
        } else {
            bail!("Unsupported SSH option: {key}");
        }
        Ok(())
    }
}

fn boolean(value: &str) -> Result<bool> {
    if value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if value.eq_ignore_ascii_case("no") || value.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        bail!("Expected yes or no, got: {value}")
    }
}

fn jumps(value: &str) -> Vec<String> {
    if value.eq_ignore_ascii_case("none") {
        Vec::new()
    } else {
        value.split(',').map(str::to_owned).collect()
    }
}

fn log_level(value: &str) -> Result<(LogFilter, bool)> {
    let (filter, verbose) = if value.eq_ignore_ascii_case("quiet") {
        ("none", false)
    } else if value.eq_ignore_ascii_case("fatal") || value.eq_ignore_ascii_case("error") {
        ("error", false)
    } else if value.eq_ignore_ascii_case("info") {
        ("info", true)
    } else if ["verbose", "debug", "debug1", "debug2", "debug3"]
        .iter()
        .any(|s| value.eq_ignore_ascii_case(s))
    {
        ("debug", true)
    } else {
        bail!("Invalid LogLevel: {value}");
    };
    Ok((LogFilter::try_from_filter(filter)?, verbose))
}
