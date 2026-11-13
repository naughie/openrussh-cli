use openrussh_cli_config::{
    AddressFamily, BindHost, Config, Destination, HostOverrides, LocalForward, Session,
};
use openrussh_client::config::RequestTty;

use anyhow::{Context as _, Result, bail, ensure};

use std::path::PathBuf;

mod options;

#[derive(clap::Parser)]
#[command(
    name = "ssh",
    version,
    about = "SSH client powered by openrussh-client"
)]
struct Args {
    #[arg(short = '4', action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true", help = "Use IPv4 addresses only")]
    ipv4: Vec<bool>,

    #[arg(short = '6', action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true", help = "Use IPv6 addresses only")]
    ipv6: Vec<bool>,

    #[arg(
        short = 'o',
        value_name = "OPTION",
        help = "Set an OpenSSH-style option (repeatable, independent of config format)"
    )]
    options: Vec<String>,

    #[arg(
        short = 'v',
        overrides_with = "verbose",
        help = "Enable verbose logging with the configured filter"
    )]
    verbose: bool,

    #[arg(
        short = 'F',
        value_name = "CONFIG",
        overrides_with = "config",
        help = "Read a Lua (.lua), TOML (.toml), or OpenSSH config file (none to skip)"
    )]
    config: Option<PathBuf>,

    #[arg(short = 'p', value_name = "PORT", help = "Connect to this port")]
    port: Option<Vec<u16>>,

    #[arg(short = 'l', value_name = "USER", help = "Log in as this user")]
    user: Option<Vec<String>>,

    #[arg(
        short = 'i',
        value_name = "IDENTITY",
        help = "Use this identity file (repeatable)"
    )]
    identities: Option<Vec<PathBuf>>,

    #[arg(
        short = 'J',
        value_name = "JUMP",
        help = "Connect through comma-separated jump hosts"
    )]
    jump: Option<Vec<String>>,

    #[arg(short = 't', action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true", help = "Request a terminal (-tt forces allocation)")]
    tty: Vec<bool>,

    #[arg(
        short = 'T',
        action = clap::ArgAction::Append, num_args = 0, default_missing_value = "true",
        help = "Disable terminal allocation"
    )]
    no_tty: Vec<bool>,

    #[arg(short = 'n', help = "Read no input from stdin")]
    stdin_null: bool,

    #[arg(short = 'N', conflicts_with_all = ["stdio", "command"], help = "Open no remote shell or command")]
    no_session: bool,

    #[arg(
        short = 'W',
        value_name = "HOST:PORT",
        conflicts_with = "command",
        help = "Forward stdin and stdout to a remote TCP endpoint"
    )]
    stdio: Option<String>,

    #[arg(
        short = 'L',
        value_name = "FORWARD",
        help = "Forward a local TCP port or Unix socket (repeatable)"
    )]
    forwards: Option<Vec<String>>,

    #[arg(
        short = 'g',
        help = "Bind unspecified local forwarding addresses on all interfaces"
    )]
    gateway_ports: bool,

    #[arg(
        short = 'e',
        value_name = "CHAR",
        overrides_with = "escape",
        help = "Set the terminal escape character, or none (default: ~)"
    )]
    escape: Option<String>,

    #[arg(short = 'C', help = "Prefer compression")]
    compression: bool,

    #[arg(
        short = 'c',
        value_name = "CIPHERS",
        overrides_with = "ciphers",
        help = "Use these comma-separated ciphers"
    )]
    ciphers: Option<String>,

    #[arg(
        short = 'm',
        value_name = "MACS",
        overrides_with = "macs",
        help = "Use these comma-separated MAC algorithms"
    )]
    macs: Option<String>,

    #[arg(
        value_name = "DESTINATION",
        help = "[user@]host[:port] or ssh://[user@]host[:port]"
    )]
    destination: String,

    #[arg(value_name = "COMMAND", num_args = 0.., trailing_var_arg = true, allow_hyphen_values = true, help = "Remote shell command (arguments are joined with spaces)")]
    command: Option<Vec<String>>,
}

pub(crate) struct Invocation {
    destination: Destination,
    config_file: Option<PathBuf>,
    identities: Option<Vec<PathBuf>>,
    proxy_jump: Option<Vec<String>>,
    tty: Option<RequestTty>,
    verbose: bool,
    session: Session,
    stdin_null: bool,
    escape_char: Option<u8>,
    local_forwards: Option<Vec<LocalForward>>,
    compression: bool,
    ciphers: Option<String>,
    macs: Option<String>,
    host_overrides: HostOverrides,
    address_family: AddressFamily,
    send_env: Option<Vec<String>>,
    remote_command: Option<String>,
}

impl Invocation {
    pub(crate) fn build(mut self) -> Result<Config> {
        self.host_overrides.identity_files = self.identities;
        let mut builder = Config::builder()
            .verbose(self.verbose)
            .stdin_null(self.stdin_null)
            .escape_char(self.escape_char)
            .compression(self.compression)
            .host_overrides(self.host_overrides)
            .address_family(self.address_family);

        if let Some(path) = self.config_file {
            builder = builder.config_file(path);
        }
        if let Some(patterns) = self.send_env {
            builder = builder.send_env(patterns);
        }
        if let Some(command) = self.remote_command {
            builder = builder.remote_command(command);
        }
        if let Some(jumps) = self.proxy_jump {
            builder = builder.proxy_jump(jumps);
        }
        if let Some(tty) = self.tty {
            builder = builder.tty(tty);
        }
        if let Some(forwards) = self.local_forwards {
            builder = builder.local_forwards(forwards);
        }
        if let Some(ciphers) = self.ciphers {
            builder = builder.ciphers(ciphers);
        }
        if let Some(macs) = self.macs {
            builder = builder.macs(macs);
        }

        Ok(builder.build(self.destination, self.session)?)
    }
}

pub(crate) fn parse() -> Result<Invocation> {
    use clap::{CommandFactory as _, FromArgMatches as _};

    let matches = Args::command().get_matches();
    Args::from_arg_matches(&matches)?.into_invocation(&matches)
}

impl Args {
    fn into_invocation(self, matches: &clap::ArgMatches) -> Result<Invocation> {
        let mut args = self;
        let mut options = options::Options::from_args(&mut args, matches)?;
        if args.verbose && options.host.log_filter.is_some() {
            options.host.log_filter =
                Some(openrussh_cli_config::LogFilter::try_from_filter("debug")?);
        }

        let remote_command = options
            .remote_command
            .filter(|command| !command.eq_ignore_ascii_case("none"));
        let mut destination = parse_destination(args.destination)?;

        if let Some(user) = options.user {
            destination.user = Some(user);
        }

        if let Some(port) = options.port {
            destination.port = Some(port);
        }

        let session = if args.no_session {
            Session::None
        } else if let Some(s) = &args.stdio {
            let (host, port) = parse_tcp_endpoint(s)?;
            Session::Forward {
                host: host.to_owned(),
                port,
            }
        } else if options.no_session.unwrap_or(false) {
            Session::None
        } else if let Some(command) = args.command {
            ensure!(
                remote_command.is_none(),
                "Cannot execute a command-line command and RemoteCommand"
            );
            let mut words = command.into_iter();
            let mut command = words.next().unwrap_or_default();
            command.reserve(words.as_slice().iter().map(|word| word.len() + 1).sum());

            for word in words {
                command.push(' ');
                command.push_str(&word);
            }

            if command.is_empty() {
                Session::Shell
            } else {
                Session::Exec { command }
            }
        } else {
            Session::Shell
        };

        let tty = if matches!(session, Session::Forward { .. } | Session::None) {
            Some(RequestTty::Never)
        } else {
            options.tty
        };

        let local_forwards = options
            .forwards
            .map(|v| {
                v.iter()
                    .map(|s| {
                        parse_forward(
                            s,
                            args.gateway_ports || options.gateway_ports.unwrap_or(false),
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;

        Ok(Invocation {
            destination,
            config_file: args.config,
            identities: options.identities,
            proxy_jump: options.proxy_jump,
            tty,
            verbose: args.verbose || options.verbose,
            session,
            stdin_null: args.stdin_null || options.stdin_null.unwrap_or(false),
            escape_char: parse_escape(
                args.escape
                    .as_deref()
                    .or(options.escape.as_deref())
                    .unwrap_or("~"),
            )?,
            local_forwards,
            compression: args.compression || options.compression.unwrap_or(false),
            ciphers: args.ciphers.or(options.ciphers),
            macs: args.macs.or(options.macs),
            host_overrides: options.host,
            address_family: options.address_family.unwrap_or_default(),
            send_env: options.send_env,
            remote_command,
        })
    }
}

fn parse_destination(mut s: String) -> Result<Destination> {
    let query = s.strip_prefix("ssh://").unwrap_or(&s);
    let mut host_start = s.len() - query.len();
    let query = query.strip_suffix('/').unwrap_or(query);
    let (user, host) = if let Some((user, host)) = query.rsplit_once('@') {
        ensure!(!user.is_empty(), "The destination user is empty");
        host_start += user.len() + 1;
        (Some(user.to_owned()), host)
    } else {
        (None, query)
    };

    let (host, port) = if let Some(s) = host.strip_prefix('[') {
        host_start += 1;
        let (host, rest) = s.split_once(']').context("Unclosed destination address")?;
        let port = if rest.is_empty() {
            None
        } else {
            Some(
                rest.strip_prefix(':')
                    .context("Invalid destination address")?
                    .parse()
                    .context("Invalid destination port")?,
            )
        };
        (host, port)
    } else if host.bytes().filter(|&c| c == b':').count() == 1 {
        let (host, port) = host
            .split_once(':')
            .context("Invalid destination address")?;
        (
            host,
            Some(port.parse().context("Invalid destination port")?),
        )
    } else {
        (host, None)
    };
    ensure!(!host.is_empty(), "The destination host is empty");

    let host_end = host_start + host.len();
    s.truncate(host_end);
    // Keep the caller's buffer when removing the URI/user/address delimiters.
    drop(s.drain(..host_start));

    Ok(Destination {
        host: s,
        user,
        port,
    })
}

fn parse_escape(s: &str) -> Result<Option<u8>> {
    match s.as_bytes() {
        b"none" => Ok(None),
        &[c] if c.is_ascii() => Ok(Some(c)),
        &[b'^', b'?'] => Ok(Some(127)),
        &[b'^', c @ b'@'..=b'_'] => Ok(Some(c & 31)),
        _ => bail!("The escape character must be one ASCII character, ^X, or none"),
    }
}

fn parse_tcp_endpoint(s: &str) -> Result<(&str, u16)> {
    let (parts, n) = split_tcp_fields(s)?;
    ensure!(n == 2, "Expected host:port: {s}");

    Ok((
        forwarding_host(parts[0])?,
        parts[1].parse().context("Invalid forwarding port")?,
    ))
}

fn forwarding_host(s: &str) -> Result<&str> {
    let host = if let Some(host) = s.strip_prefix('[') {
        host.strip_suffix(']')
            .context("Invalid bracketed forwarding address")?
    } else {
        s
    };
    ensure!(!host.is_empty(), "The forwarding host is empty");
    ensure!(!host.contains('/'), "Invalid forwarding host: {s}");

    Ok(host)
}

fn parse_forward(s: &str, gateway_ports: bool) -> Result<LocalForward> {
    // Unix paths are literal filesystem names; brackets have no address meaning.
    if let Some((bind, target)) = s.split_once(':')
        && bind.contains('/')
        && target.contains('/')
        && !target.contains(':')
    {
        return Ok(LocalForward::Unix {
            bind: bind.into(),
            target: target.to_owned(),
        });
    }

    let (parts, n) = split_tcp_fields(s)?;
    ensure!(
        parts[..n].iter().all(|part| !part.contains('/')),
        "Invalid local forwarding specification: {s}"
    );
    let (bind, port, host, target_port) = match &parts[..n] {
        [port, host, target_port] => (None, *port, *host, *target_port),
        [bind, port, host, target_port] => (Some(*bind), *port, *host, *target_port),
        _ => bail!("Invalid local forwarding specification: {s}"),
    };

    let bind = match bind {
        Some("") => Some(""),
        Some(bind) => Some(forwarding_host(bind)?),
        None => None,
    };
    let bind_port = port.parse().context("Invalid local forwarding port")?;
    let host = forwarding_host(host)?;
    let target_port = target_port
        .parse()
        .context("Invalid forwarding target port")?;

    Ok(LocalForward::Tcp {
        bind_host: match bind {
            None if gateway_ports => BindHost::All,
            None => BindHost::Loopback,
            Some("" | "*") => BindHost::All,
            Some(bind) => BindHost::Host(bind.to_owned()),
        },
        bind_port,
        target_host: host.to_owned(),
        target_port,
    })
}

fn split_tcp_fields(s: &str) -> Result<([&str; 4], usize)> {
    let mut parts = [""; 4];
    let mut n = 0;
    let mut start = 0;
    let mut bracket = false;

    for (i, c) in s.bytes().enumerate() {
        match c {
            b'[' => {
                ensure!(!bracket && i == start, "Invalid forwarding address: {s}");
                bracket = true;
            }
            b']' => {
                ensure!(
                    bracket && (i + 1 == s.len() || s.as_bytes()[i + 1] == b':'),
                    "Invalid forwarding address: {s}"
                );
                bracket = false;
            }
            b':' if !bracket => {
                ensure!(n < parts.len() - 1, "Invalid forwarding specification: {s}");
                parts[n] = &s[start..i];
                n += 1;
                start = i + 1;
            }
            _ => {}
        }
    }
    ensure!(!bracket, "Unclosed forwarding address: {s}");
    parts[n] = &s[start..];
    n += 1;

    Ok((parts, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation> {
        use clap::{CommandFactory as _, FromArgMatches as _};

        let matches = Args::command()
            .try_get_matches_from(std::iter::once("ssh").chain(args.iter().copied()))?;
        Args::from_arg_matches(&matches)?.into_invocation(&matches)
    }

    #[test]
    fn readme_examples_resolve_documented_host_settings() -> Result<()> {
        use openrussh_cli_config::ConfigBuilder;

        use std::path::Path;

        fn example<'src>(readme: &'src str, fence: &str) -> Result<&'src str> {
            let (_, code) = readme.split_once(fence).context("Missing README example")?;
            let (code, _) = code
                .split_once("\n```")
                .context("Unclosed README example")?;

            Ok(code)
        }

        let readme = include_str!("../../README.md");
        let dir = tempfile::tempdir()?;
        let toml_path = dir.path().join("config.toml");
        let lua_path = dir.path().join("config.lua");
        std::fs::write(&toml_path, example(readme, "```toml\n")?)?;
        std::fs::write(&lua_path, example(readme, "```lua\n")?)?;

        let identity = std::env::home_dir()
            .context("Missing home directory")?
            .join(".ssh/id_ed25519");
        let load = |path: &Path, alias: &str| {
            ConfigBuilder::new().config_file(path).build(
                Destination {
                    host: alias.to_owned(),
                    user: None,
                    port: None,
                },
                Session::Shell,
            )
        };

        for (alias, host, user, port, tty) in [
            ("github", "github.com", "git", 22, RequestTty::ForShell),
            ("my-host", "example.com", "alice", 2222, RequestTty::IfTty),
        ] {
            let toml = load(&toml_path, alias)?.connection;
            let lua = load(&lua_path, alias)?.connection;

            assert_eq!(toml.chain().target.dest.name, host);
            assert_eq!(toml.chain().target.dest.user, user);
            assert_eq!(toml.chain().target.dest.port, port);
            assert_eq!(
                toml.chain().target.auth.identities.as_deref(),
                Some(std::slice::from_ref(&identity))
            );
            assert!(toml.chain().target.auth.identities_only);
            assert_eq!(toml.chain().target.tty, tty);
            assert!(toml.chain().bastions.is_none());
            assert!(lua.chain().bastions.is_none());

            let toml = &toml.chain().target;
            let lua = &lua.chain().target;
            assert_eq!(lua.dest, toml.dest);
            assert_eq!(lua.tty, toml.tty);
            assert_eq!(lua.auth.identities, toml.auth.identities);
            assert_eq!(lua.auth.cert, toml.auth.cert);
            assert_eq!(lua.auth.identities_only, toml.auth.identities_only);
            assert_eq!(lua.auth.includes_none, toml.auth.includes_none);
            assert!(lua.auth.agent.is_none());
        }

        Ok(())
    }

    #[test]
    fn building_preserves_cli_overrides_and_application_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[default]\nuser = 'configured'\nport = 2200\nproxy-jump = ['jump']\n[hosts.server]\nhost-name = 'target.example'\n[hosts.jump]\nhost-name = 'jump.example'\n",
        )?;

        let conf = parse(&[
            "-F",
            path.to_str().context("Configuration path is not UTF-8")?,
            "-l",
            "alice",
            "-p",
            "2222",
            "-i",
            "/keys/cli",
            "-J",
            "none",
            "-vttnC",
            "-e",
            "none",
            "-L",
            "8080:db:5432",
            "-c",
            "aes128-ctr",
            "-m",
            "hmac-sha2-256",
            "server",
            "printf",
            "hello",
        ])?
        .build()?;

        assert_eq!(conf.connection.chain().target.dest.name, "target.example");
        assert_eq!(conf.connection.chain().target.dest.user, "alice");
        assert_eq!(conf.connection.chain().target.dest.port, 2222);
        assert_eq!(
            conf.connection.chain().target.auth.identities,
            Some(vec![PathBuf::from("/keys/cli")])
        );
        assert!(conf.connection.chain().bastions.is_none());
        assert_eq!(conf.connection.chain().target.tty, RequestTty::Always);

        assert!(conf.verbose);
        assert!(conf.stdin_null);
        assert_eq!(conf.escape_char, None);
        assert_eq!(
            conf.session,
            Session::Exec {
                command: "printf hello".to_owned()
            }
        );
        assert_eq!(
            conf.local_forwards,
            Some(vec![parse_forward("8080:db:5432", false)?])
        );
        assert!(conf.compression);
        assert_eq!(conf.ciphers.as_deref(), Some("aes128-ctr"));
        assert_eq!(conf.macs.as_deref(), Some("hmac-sha2-256"));

        Ok(())
    }

    #[test]
    fn parsing_defers_loading_and_build_errors_retain_log_metadata() -> Result<()> {
        use openrussh_cli_config::{Error, LogFilter};

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("config.toml");
        let path_arg = path.to_str().context("Configuration path is not UTF-8")?;

        let invocation = parse(&["-F", path_arg, "server"])?;
        assert!(invocation.build().is_err());

        std::fs::write(
            &path,
            "[meta]\nlog-level = 'none'\n[default]\nidentity-file = 'relative'\n",
        )?;
        let error = match parse(&["-F", path_arg, "server"])?.build() {
            Ok(_) => panic!("A relative native identity path should fail"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<Error>().and_then(Error::log_filter),
            Some(LogFilter::Disabled)
        ));
        assert_eq!(
            format!("{error:#}"),
            format!(
                "Could not read SSH config {}: identity_file must be an absolute path: relative",
                path.display()
            )
        );

        Ok(())
    }

    #[test]
    fn destinations_preserve_user_host_and_port() -> Result<()> {
        for (s, user, host, port) in [
            ("server", None, "server", None),
            ("alice@server", Some("alice"), "server", None),
            ("server:2200", None, "server", Some(2200)),
            ("alice@server:2200", Some("alice"), "server", Some(2200)),
            ("2001:db8::1", None, "2001:db8::1", None),
            ("[2001:db8::1]", None, "2001:db8::1", None),
            (
                "alice@[2001:db8::1]:2200",
                Some("alice"),
                "2001:db8::1",
                Some(2200),
            ),
            (
                "ssh://alice@server:2200/",
                Some("alice"),
                "server",
                Some(2200),
            ),
            ("ssh://[2001:db8::1]:2200", None, "2001:db8::1", Some(2200)),
            (
                "ssh://名@例.example:2200/",
                Some("名"),
                "例.example",
                Some(2200),
            ),
        ] {
            let input = s.to_owned();
            let buffer = input.as_ptr();
            let destination = parse_destination(input)?;
            assert_eq!(destination.host.as_ptr(), buffer, "{s}");
            assert_eq!(
                destination,
                Destination {
                    host: host.to_owned(),
                    user: user.map(str::to_owned),
                    port,
                },
                "{s}"
            );
        }

        Ok(())
    }

    #[test]
    fn malformed_destinations_are_rejected() {
        for s in [
            "",
            "@server",
            "alice@",
            "ssh://",
            "[]",
            "[::1",
            "[::1]suffix",
            "server:",
            "server:not-a-port",
            "server:65536",
            "[::1]:65536",
        ] {
            assert!(parse_destination(s.to_owned()).is_err(), "{s}");
        }
    }

    #[test]
    fn command_arguments_are_joined_without_escaping() -> Result<()> {
        let cases: &[(&[&str], &str)] = &[
            (&["server", "echo hello"], "echo hello"),
            (&["server", "echo", "hello"], "echo hello"),
            (
                &[
                    "server", "printf", "'%s\\n'", "a b", "$PATH", "|", "cat", "-n",
                ],
                "printf '%s\\n' a b $PATH | cat -n",
            ),
            (
                &["server", "printf 'left", "", "right'"],
                "printf 'left  right'",
            ),
            (&["server", "", "echo", "hello", ""], " echo hello "),
            (
                &["server", "printf 'line\nbreak' # comment"],
                "printf 'line\nbreak' # comment",
            ),
            (&["server", "", ""], " "),
        ];

        for &(args, expected) in cases {
            assert_eq!(
                parse(args)?.session,
                Session::Exec {
                    command: expected.to_owned()
                },
                "{args:?}"
            );
        }

        Ok(())
    }

    #[test]
    fn an_empty_command_starts_a_shell() -> Result<()> {
        assert_eq!(parse(&["server", ""])?.session, Session::Shell);

        Ok(())
    }

    #[test]
    fn default_session_is_a_shell() -> Result<()> {
        let conf = parse(&["server"])?;

        assert_eq!(conf.session, Session::Shell);

        assert_eq!(conf.tty, None);

        assert_eq!(conf.escape_char, Some(b'~'));

        assert!(!conf.stdin_null);
        assert!(!conf.verbose);

        assert!(conf.local_forwards.is_none());

        Ok(())
    }

    #[test]
    fn verbose_flag_preserves_remote_command_arguments() -> Result<()> {
        for flags in ["-v", "-vv", "-vvv"] {
            let conf = parse(&[flags, "server", "printf", "-v"])?;

            assert!(conf.verbose);
            assert_eq!(
                conf.session,
                Session::Exec {
                    command: "printf -v".to_owned()
                }
            );
        }

        Ok(())
    }

    #[test]
    fn explicit_connection_options_override_the_destination() -> Result<()> {
        let conf = parse(&["-l", "bob", "-p", "2222", "alice@server:2200"])?;

        assert_eq!(
            conf.destination,
            Destination {
                host: "server".to_owned(),
                user: Some("bob".to_owned()),
                port: Some(2222),
            }
        );

        Ok(())
    }

    #[test]
    fn scalar_flags_follow_openssh_precedence() -> Result<()> {
        let conf = parse(&[
            "-F", "first", "-F", "second", "-l", "alice", "-l", "bob", "-p", "2200", "-p", "2222",
            "-J", "none", "-e", "!", "-e", "none", "server",
        ])?;

        assert_eq!(
            conf.config_file.as_deref(),
            Some(std::path::Path::new("second"))
        );

        assert_eq!(conf.destination.user.as_deref(), Some("alice"));

        assert_eq!(conf.destination.port, Some(2200));

        assert_eq!(conf.proxy_jump, Some(Vec::new()));

        assert_eq!(conf.escape_char, None);

        Ok(())
    }

    #[test]
    fn options_override_every_configuration_format_without_changing_alias_lookup() -> Result<()> {
        use openrussh_cli_config::LogFilter;

        let dir = tempfile::tempdir()?;
        for (name, source) in [
            (
                "config",
                "Host alias\n HostName file.example\n User file-user\n Port 2200\n IdentityFile /keys/file\n CertificateFile /keys/file-cert\n IdentityAgent /sockets/file\n IdentitiesOnly no\n RequestTTY force\n",
            ),
            (
                "config.toml",
                "[hosts.alias]\nhost-name='file.example'\nuser='file-user'\nport=2200\nidentity-file='/keys/file'\ncertificate-file='/keys/file-cert'\nidentity-agent='/sockets/file'\nidentities-only=false\nrequest-tty='force'\n",
            ),
            (
                "config.lua",
                "return {hosts={alias={host_name='file.example',user='file-user',port=2200,identity_file='/keys/file',certificate_file='/keys/file-cert',identity_agent='/sockets/file',identities_only=false,request_tty='force'}}}",
            ),
            ("none", ""),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, source)?;
            let config = if name == "none" {
                "none"
            } else {
                path.to_str().unwrap()
            };
            let conf = parse(&[
                "-F",
                config,
                "-o",
                "hOsTnAmE = cli.example",
                "-o",
                "User cli-user",
                "-o",
                "Port=2222",
                "-o",
                "IdentityFile='relative cli key'",
                "-o",
                "CertificateFile=relative-cert",
                "-o",
                "IdentityAgent=none",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "RequestTTY=no",
                "-o",
                "KexAlgorithms=curve25519-sha256",
                "-o",
                "HostKeyAlgorithms=ssh-ed25519",
                "-o",
                "UserKnownHostsFile=relative-known-hosts",
                "-o",
                "LogLevel=QUIET",
                "alias",
            ])?
            .build()?;
            let host = &conf.connection.chain().target;
            assert_eq!(host.dest.name, "cli.example", "{name}");
            assert_eq!(host.dest.user, "cli-user", "{name}");
            assert_eq!(host.dest.port, 2222, "{name}");
            let mut identities = vec![PathBuf::from("relative cli key")];
            if name != "none" {
                identities.push("/keys/file".into());
            }
            assert_eq!(host.auth.identities.as_ref(), Some(&identities), "{name}");
            assert_eq!(
                host.auth.cert.as_deref(),
                Some(std::path::Path::new("relative-cert"))
            );
            assert!(host.auth.agent.is_none());
            assert!(host.auth.identities_only);
            assert_eq!(host.tty, RequestTty::Never);
            assert!(conf.connection.algorithm_policy(host)?.has_host_keys());
            assert_eq!(
                conf.meta.known_hosts.as_deref(),
                Some(std::path::Path::new("relative-known-hosts"))
            );
            assert!(matches!(conf.meta.log_filter, Some(LogFilter::Disabled)));
        }
        Ok(())
    }

    #[test]
    fn options_and_dedicated_flags_obey_order_and_accumulation_rules() -> Result<()> {
        let conf = parse(&[
            "-o",
            "User=first",
            "-l",
            "second",
            "-o",
            "User=third",
            "-o",
            "Port=2201",
            "-p",
            "2202",
            "-o",
            "Port=2203",
            "-i",
            "first",
            "-o",
            "IdentityFile=second",
            "-i",
            "first",
            "-o",
            "IdentityFile=third",
            "-6",
            "-o",
            "AddressFamily=any",
            "-4",
            "-o",
            "Compression=no",
            "-C",
            "-o",
            "Compression=no",
            "-o",
            "Ciphers=aes128-ctr",
            "-c",
            "aes256-ctr",
            "-o",
            "MACs=hmac-sha2-512",
            "-m",
            "hmac-sha2-256",
            "-o",
            "EscapeChar=!",
            "-e",
            "none",
            "explicit@alias:2204",
        ])?;
        assert_eq!(conf.destination.user.as_deref(), Some("first"));
        assert_eq!(conf.destination.port, Some(2201));
        assert_eq!(
            conf.identities,
            Some(vec!["first".into(), "second".into(), "third".into()])
        );
        assert_eq!(conf.address_family, AddressFamily::Inet);
        assert!(conf.compression);
        assert_eq!(conf.ciphers.as_deref(), Some("aes256-ctr"));
        assert_eq!(conf.macs.as_deref(), Some("hmac-sha2-256"));
        assert_eq!(conf.escape_char, None);

        for (args, tty) in [
            (vec!["-oRequestTTY=yes", "-t", "alias"], RequestTty::Always),
            (vec!["-t", "-oRequestTTY=force", "alias"], RequestTty::IfTty),
            (vec!["-oRequestTTY=force", "-t", "alias"], RequestTty::IfTty),
            (
                vec!["-T", "-oRequestTTY=yes", "-t", "alias"],
                RequestTty::IfTty,
            ),
            (vec!["-oRequestTTY=force", "-T", "alias"], RequestTty::Never),
            (vec!["-oRequestTTY=force", "-N", "alias"], RequestTty::Never),
        ] {
            assert_eq!(parse(&args)?.tty, Some(tty), "{args:?}");
        }
        assert!(parse(&["-oProxyJump=first", "-J", "second", "alias"]).is_err());
        assert!(parse(&["-J", "first", "-J", "second", "alias"]).is_err());
        assert_eq!(
            parse(&["-J", "first", "-oProxyJump=second", "alias"])?.proxy_jump,
            Some(vec!["first".into()])
        );
        Ok(())
    }

    #[test]
    fn application_options_are_typed_and_remote_commands_keep_shell_syntax() -> Result<()> {
        let conf = parse(&[
            "-F",
            "none",
            "-oSendEnv=LANG LC_*",
            "-oSendEnv=-LC_PRIVATE* TEST_?",
            "-oStdinNull=yes",
            "-oGatewayPorts=yes",
            "-oLocalForward=8080 db:5432",
            "-oRemoteCommand=printf '%%s' 'a b' | cat",
            "alias",
        ])?;
        assert_eq!(
            conf.send_env,
            Some(vec![
                "LANG".into(),
                "LC_*".into(),
                "-LC_PRIVATE*".into(),
                "TEST_?".into()
            ])
        );
        assert!(conf.stdin_null);
        assert!(matches!(
            conf.local_forwards.as_ref().unwrap()[0],
            LocalForward::Tcp {
                bind_host: BindHost::All,
                ..
            }
        ));
        assert_eq!(
            conf.remote_command.as_deref(),
            Some("printf '%%s' 'a b' | cat")
        );
        assert_eq!(
            parse(&["-oSessionType=none", "alias"])?.session,
            Session::None
        );
        assert_eq!(
            parse(&["-oSessionType=none", "-W", "db:5432", "alias"])?.session,
            Session::Forward {
                host: "db".into(),
                port: 5432
            }
        );
        assert_eq!(
            parse(&["-oRemoteCommand=\"\"", "alias"])?
                .remote_command
                .as_deref(),
            Some("\"\"")
        );
        assert_eq!(
            parse(&["-oRemoteCommand=none", "alias", "true"])?.session,
            Session::Exec {
                command: "true".into()
            }
        );
        assert_eq!(
            parse(&["-oRemoteCommand=#comment", "alias"])?
                .remote_command
                .as_deref(),
            Some("#comment")
        );
        assert!(parse(&["-oSessionType=default", "-N", "alias"]).is_err());
        assert_eq!(
            parse(&["-N", "-oSessionType=default", "alias"])?.session,
            Session::None
        );
        assert!(parse(&["-oRemoteCommand=true", "alias", "false"]).is_err());
        for option in [
            "HostName",
            "User=one two",
            "Port=0",
            "Port=65536",
            "Port=no",
            "RequestTTY=bad",
            "AddressFamily=bad",
            "Compression=maybe",
            "Unknown=yes",
            "IdentityFile='unterminated",
        ] {
            assert!(parse(&["-o", option, "alias"]).is_err(), "{option}");
        }
        Ok(())
    }

    #[test]
    fn tty_flags_follow_command_line_order() -> Result<()> {
        for (flags, tty) in [
            ("-t", RequestTty::IfTty),
            ("-tt", RequestTty::Always),
            ("-ttT", RequestTty::Never),
            ("-Ttt", RequestTty::Always),
            ("-tTt", RequestTty::IfTty),
        ] {
            assert_eq!(parse(&[flags, "server"])?.tty, Some(tty), "{flags}");
        }

        assert_eq!(parse(&["-ttN", "server"])?.tty, Some(RequestTty::Never));

        assert_eq!(
            parse(&["-tt", "-W", "db:5432", "server"])?.tty,
            Some(RequestTty::Never)
        );

        Ok(())
    }

    #[test]
    fn session_modes_are_exclusive() -> Result<()> {
        assert_eq!(parse(&["-N", "server"])?.session, Session::None);

        assert_eq!(
            parse(&["-W", "[2001:db8::2]:5432", "server"])?.session,
            Session::Forward {
                host: "2001:db8::2".to_owned(),
                port: 5432
            }
        );

        for args in [
            &["-N", "server", "true"][..],
            &["-Ns", "server"][..],
            &["-N", "-W", "db:5432", "server"][..],
            &["-s", "-W", "db:5432", "server"][..],
            &["-W", "db:5432", "server", "true"][..],
            &["-s", "server"][..],
            &["-s", "server", "sftp", "extra"][..],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }

        Ok(())
    }

    #[test]
    fn repeatable_options_preserve_order() -> Result<()> {
        let conf = parse(&[
            "-ni",
            "first-key",
            "-i",
            "second-key",
            "-L",
            "8080:db:5432",
            "-L",
            "8081:db:5433",
            "server",
        ])?;

        assert!(conf.stdin_null);

        assert_eq!(
            conf.identities,
            Some(vec!["first-key".into(), "second-key".into()])
        );

        let forwards = conf.local_forwards.context("Missing forwards")?;

        assert_eq!(forwards.len(), 2);

        assert_eq!(
            forwards[0],
            LocalForward::Tcp {
                bind_host: BindHost::Loopback,
                bind_port: 8080,
                target_host: "db".to_owned(),
                target_port: 5432,
            }
        );

        assert_eq!(
            forwards[1],
            LocalForward::Tcp {
                bind_host: BindHost::Loopback,
                bind_port: 8081,
                target_host: "db".to_owned(),
                target_port: 5433,
            }
        );

        Ok(())
    }

    #[test]
    fn unsupported_options_are_rejected() {
        for option in ["-o", "-s", "-R", "-D", "-A", "-X"] {
            assert!(parse(&[option, "server"]).is_err(), "{option}");
        }
    }

    #[test]
    fn escapes_accept_ascii_and_control_notation() -> Result<()> {
        for (s, expected) in [
            ("~", Some(b'~')),
            ("!", Some(b'!')),
            ("^", Some(b'^')),
            ("^@", Some(0)),
            ("^Z", Some(26)),
            ("^_", Some(31)),
            ("^?", Some(127)),
            ("none", None),
        ] {
            assert_eq!(parse_escape(s)?, expected, "{s}");
        }

        for s in ["", "ab", "^a", "é", "NONE"] {
            assert!(parse_escape(s).is_err(), "{s}");
        }

        Ok(())
    }

    #[test]
    fn tcp_forwards_distinguish_default_wildcard_and_explicit_hosts() -> Result<()> {
        for (s, gateway, host) in [
            ("8080:db:5432", false, BindHost::Loopback),
            ("8080:db:5432", true, BindHost::All),
            (":8080:db:5432", false, BindHost::All),
            ("*:8080:db:5432", false, BindHost::All),
            (
                "127.0.0.1:8080:db:5432",
                false,
                BindHost::Host("127.0.0.1".to_owned()),
            ),
            (
                "0.0.0.0:8080:db:5432",
                false,
                BindHost::Host("0.0.0.0".to_owned()),
            ),
            (
                "localhost:8080:db:5432",
                true,
                BindHost::Host("localhost".to_owned()),
            ),
        ] {
            assert_eq!(
                parse_forward(s, gateway)?,
                LocalForward::Tcp {
                    bind_host: host,
                    bind_port: 8080,
                    target_host: "db".to_owned(),
                    target_port: 5432,
                },
                "{s}"
            );
        }

        assert_eq!(
            parse_forward("[::1]:8080:[2001:db8::2]:5432", false)?,
            LocalForward::Tcp {
                bind_host: BindHost::Host("::1".to_owned()),
                bind_port: 8080,
                target_host: "2001:db8::2".to_owned(),
                target_port: 5432,
            }
        );

        Ok(())
    }

    #[test]
    fn unix_socket_forwards_preserve_paths() -> Result<()> {
        assert_eq!(
            parse_forward("/tmp/local socket:/tmp/remote socket", false)?,
            LocalForward::Unix {
                bind: "/tmp/local socket".into(),
                target: "/tmp/remote socket".to_owned(),
            }
        );

        assert_eq!(
            parse_forward("./local[socket]:./remote[socket]", false)?,
            LocalForward::Unix {
                bind: "./local[socket]".into(),
                target: "./remote[socket]".to_owned(),
            }
        );
        assert_eq!(
            parse_forward("[./local]:[./remote]", false)?,
            LocalForward::Unix {
                bind: "[./local]".into(),
                target: "[./remote]".to_owned(),
            }
        );

        Ok(())
    }

    #[test]
    fn mixed_tcp_and_unix_forwards_are_rejected() {
        for spec in [
            "8080:/tmp/remote.sock",
            "127.0.0.1:8080:/tmp/remote.sock",
            "[::1]:8080:/tmp/remote.sock",
            "./local.sock:db:5432",
            "/tmp/local.sock:[::1]:5432",
        ] {
            assert!(parse(&["-L", spec, "server"]).is_err(), "{spec}");
        }
    }

    #[test]
    fn malformed_forwarding_specs_do_not_become_unix_socket_paths() {
        for s in [
            "",
            "8080",
            "8080:db",
            "65536:db:5432",
            "-1:db:5432",
            "port:db:5432",
            "[8080]:db:5432",
            "8080:db:[5432]",
            "[127.0.0.1]:[8080]:db:5432",
            "8080:[]:5432",
            "65536:/tmp/remote.sock",
            "localhost:65536:/tmp/remote.sock",
            "socket:db:5432",
            "8080::5432",
            "8080:db:65536",
            "127.0.0.1:8080:db:5432:extra",
            "[::1:8080:db:5432",
            "[::1]extra:8080:db:5432",
            "[::1]]:8080:db:5432",
        ] {
            assert!(parse_forward(s, false).is_err(), "{s}");
        }

        for s in [
            "db",
            ":5432",
            "db:65536",
            "::1:5432",
            "[::1]:",
            "db:5432:extra",
            "db:[5432]",
            "[]:5432",
            "/tmp/socket:5432",
        ] {
            assert!(parse_tcp_endpoint(s).is_err(), "{s}");
        }
    }
}
