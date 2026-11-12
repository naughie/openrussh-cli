use crate::config::{
    BindHost, Destination, ForwardTarget, LocalBind, LocalForward, Options, Session,
};

use anyhow::{Context as _, Result, bail, ensure};

use std::path::PathBuf;

#[derive(clap::Parser)]
#[command(
    name = "ssh",
    version,
    about = "SSH client powered by openrussh-client"
)]
struct Args {
    #[arg(
        short = 'v',
        overrides_with = "verbose",
        help = "Enable verbose logging at the configured log level"
    )]
    verbose: bool,

    #[arg(
        short = 'F',
        value_name = "CONFIG",
        overrides_with = "config",
        help = "Read a Lua (.lua), TOML (.toml), or OpenSSH config file (none to skip)"
    )]
    config: Option<PathBuf>,

    #[arg(
        short = 'p',
        value_name = "PORT",
        overrides_with = "port",
        help = "Connect to this port"
    )]
    port: Option<u16>,

    #[arg(
        short = 'l',
        value_name = "USER",
        overrides_with = "user",
        help = "Log in as this user"
    )]
    user: Option<String>,

    #[arg(
        short = 'i',
        value_name = "IDENTITY",
        help = "Use this identity file (repeatable)"
    )]
    identities: Option<Vec<PathBuf>>,

    #[arg(
        short = 'J',
        value_name = "JUMP",
        overrides_with = "jump",
        help = "Connect through comma-separated jump hosts"
    )]
    jump: Option<String>,

    #[arg(short = 't', action = clap::ArgAction::Count, overrides_with = "no_tty", help = "Request a terminal (-tt forces allocation)")]
    tty: u8,

    #[arg(
        short = 'T',
        overrides_with = "tty",
        help = "Disable terminal allocation"
    )]
    no_tty: bool,

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

pub(crate) fn parse() -> Result<Options> {
    use clap::Parser as _;

    Args::parse().into_options()
}

impl Args {
    fn into_options(self) -> Result<Options> {
        use openrussh_client::config::RequestTty;

        let args = self;

        let mut destination = parse_destination(args.destination)?;

        if let Some(user) = args.user {
            destination.user = Some(user);
        }

        if let Some(port) = args.port {
            destination.port = Some(port);
        }

        let session = if args.no_session {
            Session::None
        } else if let Some(s) = &args.stdio {
            let (host, port) = parse_address(s)?;
            Session::Forward {
                host: host.to_owned(),
                port,
            }
        } else if let Some(command) = args.command {
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
                Session::Exec(command)
            }
        } else {
            Session::Shell
        };

        let tty = if args.no_tty || matches!(session, Session::Forward { .. } | Session::None) {
            Some(RequestTty::Never)
        } else {
            match args.tty {
                0 => None,
                1 => Some(RequestTty::IfTty),
                _ => Some(RequestTty::Always),
            }
        };

        let local_forwards = args
            .forwards
            .map(|v| {
                v.iter()
                    .map(|s| parse_forward(s, args.gateway_ports))
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?;

        let proxy_jump = args.jump.map(|s| {
            if s.eq_ignore_ascii_case("none") {
                Vec::new()
            } else {
                s.split(',').map(str::to_owned).collect()
            }
        });

        Ok(Options {
            destination,
            config_file: args.config,
            identities: args.identities,
            proxy_jump,
            tty,
            verbose: args.verbose,
            session,
            stdin_null: args.stdin_null,
            escape_char: parse_escape(args.escape.as_deref().unwrap_or("~"))?,
            local_forwards,
            compression: args.compression,
            ciphers: args.ciphers,
            macs: args.macs,
        })
    }
}

fn parse_destination(s: String) -> Result<Destination> {
    let query = s.strip_prefix("ssh://").unwrap_or(&s);
    let query = query.strip_suffix('/').unwrap_or(query);
    let (user, host) = if let Some((user, host)) = query.rsplit_once('@') {
        ensure!(!user.is_empty(), "The destination user is empty");
        (Some(user.to_owned()), host)
    } else {
        (None, query)
    };

    let (host, port) = if let Some(s) = host.strip_prefix('[') {
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

    let host = if host.len() == s.len() {
        s
    } else {
        host.to_owned()
    };

    Ok(Destination { host, user, port })
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

fn parse_address(s: &str) -> Result<(&str, u16)> {
    let (parts, n) = split_forward(s)?;
    ensure!(n == 2 && !parts[0].is_empty(), "Expected host:port: {s}");

    Ok((
        parts[0],
        parts[1].parse().context("Invalid forwarding port")?,
    ))
}

fn parse_forward(s: &str, gateway_ports: bool) -> Result<LocalForward> {
    let (parts, n) = split_forward(s)?;
    let tcp_bind = |host: Option<&str>, port| LocalBind::Tcp {
        host: match host {
            None if gateway_ports => BindHost::All,
            None => BindHost::Loopback,
            Some("" | "*") => BindHost::All,
            Some(host) => BindHost::Host(host.to_owned()),
        },
        port,
    };

    let tcp_target = |host: &str, port: &str| -> Result<_> {
        ensure!(!host.is_empty(), "The forwarding host is empty");

        Ok(ForwardTarget::Tcp {
            host: host.to_owned(),
            port: port.parse().context("Invalid forwarding target port")?,
        })
    };

    let (bind, target) = match &parts[..n] {
        [port, host, target_port]
            if !port.contains('/') && !host.contains('/') && !target_port.contains('/') =>
        {
            (
                tcp_bind(None, port.parse().context("Invalid local forwarding port")?),
                tcp_target(host, target_port)?,
            )
        }
        [bind, port, host, target_port]
            if !bind.contains('/') && !host.contains('/') && !target_port.contains('/') =>
        {
            (
                tcp_bind(
                    Some(bind),
                    port.parse().context("Invalid local forwarding port")?,
                ),
                tcp_target(host, target_port)?,
            )
        }
        [socket, target] if socket.contains('/') && target.contains('/') => (
            LocalBind::Unix((*socket).into()),
            ForwardTarget::Unix((*target).to_owned()),
        ),
        _ => bail!("Invalid local forwarding specification: {s}"),
    };

    if let LocalBind::Unix(path) = &bind {
        ensure!(
            !path.as_os_str().is_empty(),
            "The local socket path is empty"
        );
    }

    if let ForwardTarget::Unix(path) = &target {
        ensure!(!path.is_empty(), "The remote socket path is empty");
    }

    Ok(LocalForward { bind, target })
}

fn split_forward(s: &str) -> Result<([&str; 4], usize)> {
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
                ensure!(bracket, "Invalid forwarding address: {s}");
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

    for part in &mut parts[..n] {
        if let Some(s) = part.strip_prefix('[') {
            *part = s
                .strip_suffix(']')
                .context("Invalid bracketed forwarding address")?;
        }
    }

    Ok((parts, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    use openrussh_client::config::RequestTty;

    fn parse(args: &[&str]) -> Result<Options> {
        use clap::Parser as _;

        Args::try_parse_from(std::iter::once("ssh").chain(args.iter().copied()))?.into_options()
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
        ] {
            assert_eq!(
                parse_destination(s.to_owned())?,
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
                Session::Exec(expected.to_owned()),
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
            assert_eq!(conf.session, Session::Exec("printf -v".to_owned()));
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
    fn last_scalar_option_wins() -> Result<()> {
        let conf = parse(&[
            "-F", "first", "-F", "second", "-l", "alice", "-l", "bob", "-p", "2200", "-p", "2222",
            "-J", "first", "-J", "none", "-e", "!", "-e", "none", "server",
        ])?;

        assert_eq!(
            conf.config_file.as_deref(),
            Some(std::path::Path::new("second"))
        );

        assert_eq!(conf.destination.user.as_deref(), Some("bob"));

        assert_eq!(conf.destination.port, Some(2222));

        assert_eq!(conf.proxy_jump, Some(Vec::new()));

        assert_eq!(conf.escape_char, None);

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
            forwards[0].target,
            ForwardTarget::Tcp {
                host: "db".to_owned(),
                port: 5432
            }
        );

        assert_eq!(
            forwards[1].target,
            ForwardTarget::Tcp {
                host: "db".to_owned(),
                port: 5433
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
                LocalForward {
                    bind: LocalBind::Tcp { host, port: 8080 },
                    target: ForwardTarget::Tcp {
                        host: "db".to_owned(),
                        port: 5432
                    },
                },
                "{s}"
            );
        }

        assert_eq!(
            parse_forward("[::1]:8080:[2001:db8::2]:5432", false)?,
            LocalForward {
                bind: LocalBind::Tcp {
                    host: BindHost::Host("::1".to_owned()),
                    port: 8080
                },
                target: ForwardTarget::Tcp {
                    host: "2001:db8::2".to_owned(),
                    port: 5432
                },
            }
        );

        Ok(())
    }

    #[test]
    fn unix_socket_forwards_preserve_paths() -> Result<()> {
        assert_eq!(
            parse_forward("/tmp/local socket:/tmp/remote socket", false)?,
            LocalForward {
                bind: LocalBind::Unix("/tmp/local socket".into()),
                target: ForwardTarget::Unix("/tmp/remote socket".to_owned()),
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
        ] {
            assert!(parse_address(s).is_err(), "{s}");
        }
    }
}
