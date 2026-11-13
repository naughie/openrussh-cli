use openrussh_cli_config::{
    AddressFamily, Config, Destination, Error, HostOverrides, IdentityAgent, LogFilter, Result,
    Session,
};
use openrussh_client::russh::{Preferred, kex};

use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;

fn destination() -> Destination {
    Destination {
        host: "target".into(),
        user: Some("cli-user".into()),
        port: Some(2222),
    }
}

#[test]
fn explicit_host_overrides_preserve_alias_selection_and_jump_settings_in_all_formats() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    for (extension, source) in [
        (
            "config",
            "Host target\n HostName file.example\n IdentityFile /keys/file\n CertificateFile /keys/file-cert\n IdentityAgent /sockets/file\n ProxyJump jump\nHost jump\n HostName jump.example\n IdentityFile /keys/jump\n",
        ),
        (
            "toml",
            "[hosts.target]\nhost-name = 'file.example'\nidentity-file = '/keys/file'\ncertificate-file = '/keys/file-cert'\nidentity-agent = '/sockets/file'\nproxy-jump = 'jump'\n[hosts.jump]\nhost-name = 'jump.example'\nidentity-file = '/keys/jump'\n",
        ),
        (
            "lua",
            "return {hosts = {target = {host_name = 'file.example', identity_file = '/keys/file', certificate_file = '/keys/file-cert', identity_agent = '/sockets/file', proxy_jump = 'jump'}, jump = {host_name = 'jump.example', identity_file = '/keys/jump'}}}",
        ),
    ] {
        let path = directory.path().join(format!("hosts.{extension}"));
        std::fs::write(&path, source)?;
        let config = Config::builder()
            .config_file(path)
            .host_overrides(HostOverrides {
                host_name: Some("override-%h".into()),
                identity_files: Some(vec!["keys/%h/%n/%p/%r".into()]),
                certificate_file: Some("relative-cert".into()),
                identity_agent: Some(IdentityAgent::Disabled),
                identities_only: Some(true),
                kex_algorithms: Some("diffie-hellman-group14-sha256".into()),
                host_key_algorithms: Some("ssh-ed25519".into()),
                known_hosts: Some("none".into()),
                log_filter: Some(LogFilter::Disabled),
            })
            .address_family(AddressFamily::Inet6)
            .send_env(["LANG", "LC_*", "-LC_PRIVATE"])
            .build(destination(), Session::None)?;
        let connection = &config.connection;
        let host = &connection.chain().target;
        assert_eq!(host.dest.name, "override-target", "{extension}");
        assert_eq!(host.dest.user, "cli-user");
        assert_eq!(host.dest.port, 2222);
        assert_eq!(
            host.auth.identities,
            Some(vec![
                "keys/override-target/target/2222/cli-user".into(),
                "/keys/file".into()
            ])
        );
        assert_eq!(host.auth.cert, Some("relative-cert".into()));
        assert!(host.auth.agent.is_none());
        assert!(host.auth.identities_only);
        let jump = &connection.chain().bastions.as_ref().unwrap()[0];
        assert_eq!(jump.dest.name, "jump.example");
        assert_eq!(jump.auth.identities, Some(vec!["/keys/jump".into()]));
        let mut preferred = Preferred::default();
        connection.algorithm_policy(host)?.apply(&mut preferred);
        assert_eq!(preferred.kex[0], kex::DH_G14_SHA256);
        assert!(connection.algorithm_policy(host)?.has_host_keys());
        assert_eq!(config.meta.known_hosts, Some("/dev/null".into()));
        assert!(matches!(config.meta.log_filter, Some(LogFilter::Disabled)));
        assert_eq!(config.address_family, AddressFamily::Inet6);
        assert_eq!(
            config.send_env,
            Some(vec!["LANG".into(), "LC_*".into(), "-LC_PRIVATE".into()])
        );
    }
    Ok(())
}

#[test]
fn omitted_algorithm_overrides_keep_native_policy_and_paths_keep_native_bytes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("hosts.toml");
    std::fs::write(
        &path,
        "[hosts.target]\nkex-algorithms = 'curve25519-sha256'\nhost-key-algorithms = 'ssh-ed25519'\n",
    )?;
    let identity = PathBuf::from(std::ffi::OsString::from_vec(
        b"relative/identity\xff".to_vec(),
    ));
    let config = Config::builder()
        .config_file(path)
        .host_overrides(HostOverrides {
            identity_files: Some(vec![identity.clone()]),
            ..HostOverrides::default()
        })
        .build(destination(), Session::None)?;
    let host = &config.connection.chain().target;
    assert_eq!(host.auth.identities, Some(vec![identity]));
    let policy = config.connection.algorithm_policy(host)?;
    assert!(policy.has_host_keys());
    let mut preferred = Preferred::default();
    policy.apply(&mut preferred);
    assert_eq!(preferred.kex[0], kex::CURVE25519);
    Ok(())
}

#[test]
fn explicit_none_identity_and_certificate_values_keep_their_meaning() -> Result<()> {
    let config = Config::builder()
        .config_file("none")
        .host_overrides(HostOverrides {
            identity_files: Some(vec!["none".into()]),
            certificate_file: Some("none".into()),
            identity_agent: Some(IdentityAgent::Disabled),
            ..HostOverrides::default()
        })
        .build(destination(), Session::None)?;
    let auth = &config.connection.chain().target.auth;
    assert_eq!(auth.identities, Some(Vec::new()));
    assert!(auth.cert.is_none());
    assert!(auth.agent.is_none());
    Ok(())
}

#[test]
fn override_failures_retain_native_logging_metadata() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("hosts.toml");
    std::fs::write(&path, "[meta]\nlog-level = 'none'\n")?;
    for overrides in [
        HostOverrides {
            certificate_file: Some("%Q".into()),
            ..HostOverrides::default()
        },
        HostOverrides {
            kex_algorithms: Some("unknown-algorithm".into()),
            ..HostOverrides::default()
        },
    ] {
        let mut error = Config::builder()
            .config_file(&path)
            .host_overrides(overrides)
            .build(destination(), Session::None)
            .err()
            .expect("invalid override should fail");
        assert!(matches!(error.log_filter(), Some(LogFilter::Disabled)));
        assert!(matches!(error.take_log_filter(), Some(LogFilter::Disabled)));
        assert!(error.log_filter().is_none());
        assert!(matches!(error, Error::Native { .. }));
    }
    Ok(())
}

#[test]
fn token_values_are_not_reinterpreted_as_tilde_or_percent_expressions() -> Result<()> {
    let config = Config::builder()
        .config_file("none")
        .host_overrides(HostOverrides {
            host_name: Some("~".into()),
            identity_files: Some(vec!["%h/key".into()]),
            certificate_file: Some("%%h/cert".into()),
            ..HostOverrides::default()
        })
        .build(destination(), Session::None)?;
    let auth = &config.connection.chain().target.auth;
    assert_eq!(auth.identities, Some(vec!["~/key".into()]));
    assert_eq!(auth.cert, Some("%h/cert".into()));
    Ok(())
}

#[test]
fn explicit_logging_policy_survives_source_opening_and_parsing_errors() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, source) in [
        ("missing.toml", None),
        ("malformed.toml", Some("[unterminated")),
        ("malformed.lua", Some("return {")),
        ("malformed.config", Some("Port invalid-port\n")),
    ] {
        let path = directory.path().join(name);
        if let Some(source) = source {
            std::fs::write(&path, source)?;
        }
        let mut error = Config::builder()
            .config_file(&path)
            .host_overrides(HostOverrides {
                log_filter: Some(LogFilter::Disabled),
                ..HostOverrides::default()
            })
            .build(destination(), Session::None)
            .err()
            .expect("opening or parsing the selected source must fail");
        assert!(matches!(error.log_filter(), Some(LogFilter::Disabled)));
        assert!(error.to_string().contains(path.to_str().unwrap()));
        assert!(matches!(error.take_log_filter(), Some(LogFilter::Disabled)));
        assert!(error.log_filter().is_none());
    }
    Ok(())
}
