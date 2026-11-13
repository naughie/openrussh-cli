use openrussh_cli_config::{Config, Destination, HostOverrides, LogFilter, Result, Session};

fn destination() -> Destination {
    Destination {
        host: "target".into(),
        user: Some("demo".into()),
        port: Some(2222),
    }
}

#[test]
fn remote_commands_expand_after_resolution_and_preserve_shell_syntax() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("hosts.toml");
    std::fs::write(&path, "[hosts.target]\nhost-name = 'source.example'\n")?;
    let config = Config::builder()
        .config_file(path)
        .host_overrides(HostOverrides {
            host_name: Some("override.example".into()),
            ..HostOverrides::default()
        })
        .remote_command("discarded")
        .remote_command(r#""printf" '%%s' "%h:%p:%r:%n" "$HOME" '${MISSING}'"#)
        .build(destination(), Session::Shell)?;
    assert_eq!(
        config.session,
        Session::Exec {
            command: r#""printf" '%s' "override.example:2222:demo:target" "$HOME" '${MISSING}'"#
                .into(),
        }
    );
    Ok(())
}

#[test]
fn supplied_non_shell_sessions_keep_their_commands_and_ignore_remote_command_tokens() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("hosts.toml");
    std::fs::write(&path, "")?;
    let sessions: [fn() -> Session; 3] = [
        || Session::Exec {
            command: "printf '%s' '%Q' '$HOME'".into(),
        },
        || Session::None,
        || Session::Forward {
            host: "forward.example".into(),
            port: 80,
        },
    ];
    for make_session in sessions {
        let config = Config::builder()
            .config_file(&path)
            .remote_command("invalid %Q")
            .build(destination(), make_session())?;
        assert_eq!(config.session, make_session());
    }
    Ok(())
}

#[test]
fn unsupported_remote_command_tokens_retain_the_logging_policy() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("hosts.toml");
    std::fs::write(&path, "[meta]\nlog-level = 'none'\n")?;
    for command in ["printf '%s'", "trailing%"] {
        let error = Config::builder()
            .config_file(&path)
            .remote_command(command)
            .build(destination(), Session::Shell)
            .err()
            .expect("unsupported percent token must fail");
        assert!(matches!(error.log_filter(), Some(LogFilter::Disabled)));
        assert!(error.to_string().contains("remote_command"));
    }
    Ok(())
}
