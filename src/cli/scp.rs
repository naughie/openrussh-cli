use super::{ConnectionArgs, Invocation, options, parse_destination};
use crate::scp::Transfer;
use openrussh_cli_config::{Destination, Session};
use openrussh_client::config::RequestTty;

use anyhow::{Context as _, Result, bail, ensure};

use std::path::PathBuf;

#[derive(clap::Parser)]
#[command(name = "scp", version, about = "Copies files over SSH using SFTP")]
struct ArgsScp {
    #[command(flatten)]
    connection: ConnectionArgs,

    #[arg(short = 'r', help = "Copy directories recursively")]
    recursive: bool,

    #[arg(value_name = "PATH", num_args = 2.., help = "Source paths followed by a target; use [user@]host:path for remote paths")]
    paths: Vec<PathBuf>,
}

pub(crate) fn parse_scp() -> Result<(Invocation, Transfer)> {
    use clap::FromArgMatches as _;

    let matches = ArgsScp::command().get_matches();
    ArgsScp::from_arg_matches(&matches)?.into_invocation(&matches)
}

impl ArgsScp {
    fn command() -> clap::Command {
        use clap::CommandFactory;

        <Self as CommandFactory>::command().mut_arg("port", |arg| arg.visible_short_alias('P'))
    }

    fn into_invocation(self, matches: &clap::ArgMatches) -> Result<(Invocation, Transfer)> {
        let Self {
            mut connection,
            recursive,
            mut paths,
        } = self;
        let options = options::Options::from_args(&mut connection, None, matches)?;
        ensure!(
            !options.no_session.unwrap_or(false),
            "SessionType=none conflicts with copying files"
        );
        ensure!(
            options
                .remote_command
                .as_deref()
                .is_none_or(|s| s.eq_ignore_ascii_case("none")),
            "RemoteCommand conflicts with copying files"
        );
        let target = paths.pop().context("Missing copy destination")?;
        ensure!(!paths.is_empty(), "Missing copy source");
        let (destination, transfer) = match CopyPath::parse(target)? {
            CopyPath::Remote { destination, path } => {
                let sources = paths
                    .into_iter()
                    .map(|path| match CopyPath::parse(path)? {
                        CopyPath::Local(path) => Ok(path),
                        CopyPath::Remote { .. } => {
                            bail!("Copying between remote hosts is not supported")
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                (
                    destination,
                    Transfer::Upload {
                        sources,
                        destination: path,
                        recursive,
                    },
                )
            }
            CopyPath::Local(target) => {
                let mut destination: Option<Destination> = None;
                let sources = paths
                    .into_iter()
                    .map(|path| {
                        let CopyPath::Remote {
                            destination: src,
                            path,
                        } = CopyPath::parse(path)?
                        else {
                            bail!("Copying requires a remote source or destination");
                        };
                        if let Some(destination) = &destination {
                            ensure!(
                                destination.host == src.host
                                    && (options.user.is_some() || destination.user == src.user)
                                    && (options.port.is_some() || destination.port == src.port),
                                "Remote sources must use the same host, user, and port"
                            );
                        } else {
                            destination = Some(src);
                        }
                        Ok(path)
                    })
                    .collect::<Result<Vec<_>>>()?;
                (
                    destination.context("Missing remote source")?,
                    Transfer::Download {
                        sources,
                        destination: target,
                        recursive,
                    },
                )
            }
        };
        let mut invocation = connection.into_invocation(destination, options)?;
        invocation.session = Session::None;
        invocation.tty = Some(RequestTty::Never);
        invocation.stdin_null = true;
        invocation.escape_char = None;
        Ok((invocation, transfer))
    }
}

enum CopyPath {
    Local(PathBuf),
    Remote {
        destination: Destination,
        path: String,
    },
}

impl CopyPath {
    fn parse(path: PathBuf) -> Result<Self> {
        use std::os::unix::ffi::OsStrExt as _;

        let mut bracket = false;
        let mut colon = false;
        let mut separator = None;
        for (idx, &c) in path.as_os_str().as_bytes().iter().enumerate() {
            match c {
                b'/' => return Ok(Self::Local(path)),
                b'[' => bracket = true,
                b']' => bracket = false,
                b':' => {
                    colon = true;
                    if !bracket {
                        separator = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(separator) = separator else {
            ensure!(!bracket || !colon, "Unclosed remote address");
            return Ok(Self::Local(path));
        };
        let s = path.to_str().context("Remote paths must be UTF-8")?;
        let (address, path) = s
            .split_at_checked(separator)
            .context("Invalid remote address")?;
        let path = path
            .strip_prefix(':')
            .context("Missing remote path separator")?;
        let destination = parse_destination(address.to_owned())?;
        Ok(Self::Remote {
            destination,
            path: if path.is_empty() {
                ".".to_owned()
            } else {
                path.to_owned()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<(Invocation, Transfer)> {
        use clap::FromArgMatches as _;

        let matches = ArgsScp::command()
            .try_get_matches_from(std::iter::once("scp").chain(args.iter().copied()))?;
        ArgsScp::from_arg_matches(&matches)?.into_invocation(&matches)
    }

    #[test]
    fn uploads_reuse_connection_options_and_force_no_terminal() -> Result<()> {
        let (invocation, transfer) = parse(&[
            "-F",
            "none",
            "-oUser=first",
            "-l",
            "second",
            "-p",
            "2222",
            "-P",
            "3333",
            "-oHostName=server.example",
            "-oRequestTTY=force",
            "-i",
            "/keys/first",
            "-oIdentityFile=/keys/second",
            "-6",
            "-4",
            "-Cvr",
            "file one",
            "./file:two",
            "named@alias:",
        ])?;
        let Transfer::Upload {
            sources,
            destination,
            recursive,
        } = transfer
        else {
            panic!("Expected upload");
        };
        assert_eq!(sources, [PathBuf::from("file one"), "./file:two".into()]);
        assert_eq!(destination, ".");
        assert!(recursive);

        let conf = invocation.build()?;
        let host = &conf.connection.chain().target;
        assert_eq!(host.dest.name, "server.example");
        assert_eq!(host.dest.user, "first");
        assert_eq!(host.dest.port, 2222);
        assert_eq!(
            host.auth.identities,
            Some(vec!["/keys/first".into(), "/keys/second".into()])
        );
        assert_eq!(host.tty, RequestTty::Never);
        assert_eq!(
            conf.address_family,
            openrussh_cli_config::AddressFamily::Inet
        );
        assert_eq!(conf.session, Session::None);
        assert!(conf.stdin_null);
        assert_eq!(conf.escape_char, None);
        assert!(conf.compression);
        assert!(conf.verbose);
        Ok(())
    }

    #[test]
    fn downloads_retain_literal_remote_paths_and_bracketed_ipv6() -> Result<()> {
        let (invocation, transfer) = parse(&[
            "-r",
            "alice@[2001:db8::1]:directory/file:one@two",
            "alice@[2001:db8::1]:名 with 'quotes'",
            "local",
        ])?;
        assert_eq!(invocation.destination.host, "2001:db8::1");
        assert_eq!(invocation.destination.user.as_deref(), Some("alice"));
        assert_eq!(invocation.destination.port, None);
        let Transfer::Download {
            sources,
            destination,
            recursive,
        } = transfer
        else {
            panic!("Expected download");
        };
        assert_eq!(sources, ["directory/file:one@two", "名 with 'quotes'"]);
        assert_eq!(destination, PathBuf::from("local"));
        assert!(recursive);
        Ok(())
    }

    #[test]
    fn download_sources_compare_users_after_connection_overrides() -> Result<()> {
        for (args, user) in [
            (
                vec!["-l", "bob", "alice@host:a", "bob@host:b", "localdir"],
                "bob",
            ),
            (
                vec![
                    "-oUser=first",
                    "-l",
                    "bob",
                    "alice@host:a",
                    "bob@host:b",
                    "localdir",
                ],
                "first",
            ),
        ] {
            let (invocation, transfer) = parse(&args)?;
            assert_eq!(invocation.destination.host, "host");
            assert_eq!(invocation.destination.user.as_deref(), Some(user));
            let Transfer::Download { sources, .. } = transfer else {
                panic!("Expected download");
            };
            assert_eq!(sources, ["a", "b"]);
        }
        assert!(parse(&["alice@host:a", "bob@host:b", "localdir"]).is_err());
        assert!(parse(&["-l", "bob", "alice@one:a", "bob@two:b", "localdir"]).is_err());
        Ok(())
    }

    #[test]
    fn slash_before_colon_keeps_a_path_local() -> Result<()> {
        for src in ["./name:part", "directory/name:part", "/tmp/name:part"] {
            let (_, transfer) = parse(&[src, "host:target"])?;
            let Transfer::Upload { sources, .. } = transfer else {
                panic!("Expected upload");
            };
            assert_eq!(sources, [PathBuf::from(src)]);
        }
        Ok(())
    }

    #[test]
    fn invalid_copy_endpoints_are_rejected() {
        for args in [
            vec!["local"],
            vec!["one", "two"],
            vec!["host:one", "other:two"],
            vec!["host:one", "other:two", "local"],
            vec!["alice@host:one", "bob@host:two", "local"],
            vec!["host:one", "local", "target"],
            vec!["local", ":file"],
            vec!["local", "@host:file"],
            vec!["local", "[]:file"],
            vec!["local", "[::1:file"],
            vec!["local", "[::1]extra:file"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn conflicting_session_options_are_rejected() -> Result<()> {
        for option in ["SessionType=none", "RemoteCommand=echo unexpected"] {
            let error = match parse(&["-o", option, "local", "host:file"]) {
                Ok(_) => panic!("Expected incompatible session option to fail"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("conflicts with copying files"));
        }
        assert!(
            parse(&[
                "-oSessionType=default",
                "-oRemoteCommand=none",
                "local",
                "host:file",
            ])
            .is_ok()
        );
        Ok(())
    }

    #[test]
    fn local_paths_preserve_non_utf8_names() -> Result<()> {
        use clap::FromArgMatches as _;
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;

        let src = OsString::from_vec(b"local-\xff".to_vec());
        let matches = ArgsScp::command().try_get_matches_from([
            OsString::from("scp"),
            src.clone(),
            OsString::from("host:file"),
        ])?;
        let (_, transfer) = ArgsScp::from_arg_matches(&matches)?.into_invocation(&matches)?;
        let Transfer::Upload { sources, .. } = transfer else {
            panic!("Expected upload");
        };
        assert_eq!(sources, [PathBuf::from(src)]);

        let error = match CopyPath::parse(PathBuf::from(OsString::from_vec(b"host:\xff".to_vec())))
        {
            Ok(_) => panic!("Expected invalid remote UTF-8 to fail"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), "Remote paths must be UTF-8");
        Ok(())
    }
}
