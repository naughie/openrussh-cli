use openrussh_cli_config::{Config, ConfigBuilder, Destination, LocalForward, Session};
use openrussh_client::config::RequestTty;
use openrussh_client::russh::{Preferred, cipher, compression, mac};

use std::path::PathBuf;

use openrussh_cli_config::Result;

fn destination() -> Destination {
    Destination {
        host: "target.example".into(),
        user: Some("cli-user".into()),
        port: Some(2222),
    }
}

#[test]
fn constructors_share_defaults_and_build_with_required_invocation_inputs() -> Result<()> {
    for builder in [
        Config::builder(),
        ConfigBuilder::new(),
        ConfigBuilder::default(),
    ] {
        let config = builder
            .config_file("none")
            .build(destination(), Session::Shell)?;

        assert_eq!(config.connection.chain().target.dest.name, "target.example");
        assert_eq!(config.connection.chain().target.dest.user, "cli-user");
        assert_eq!(config.connection.chain().target.dest.port, 2222);
        assert_eq!(config.connection.chain().target.tty, RequestTty::ForShell);
        assert!(config.connection.chain().target.auth.identities.is_none());
        assert!(config.connection.chain().bastions.is_none());
        assert!(config.meta.known_hosts.is_none());
        assert!(config.meta.log_filter.is_none());
        assert_eq!(config.session, Session::Shell);
        assert!(!config.verbose);
        assert!(!config.stdin_null);
        assert_eq!(config.escape_char, Some(b'~'));
        assert!(config.local_forwards.is_none());
        assert!(!config.compression);
        assert!(config.ciphers.is_none());
        assert!(config.macs.is_none());
        let preferred = config.preferred()?;
        let default = Preferred::default();
        assert_eq!(preferred.cipher, default.cipher);
        assert_eq!(preferred.mac, default.mac);
        assert_eq!(preferred.compression, default.compression);
    }

    Ok(())
}

#[test]
fn consuming_setters_replace_values_and_preserve_file_override_rules() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[default]\nuser = 'file-user'\nport = 2200\nrequest-tty = 'force'\nidentity-file = '/keys/file'\nproxy-jump = 'jump'\n",
    )?;
    let session = Session::Exec {
        command: "printf hello".into(),
    };
    let config = Config::builder()
        .config_file(directory.path().join("missing"))
        .config_file(&path)
        .identities(["/keys/discarded"])
        .identities(["relative-cli-key"])
        .proxy_jump(["discarded-jump"])
        .proxy_jump(Vec::<String>::new())
        .tty(RequestTty::Always)
        .tty(RequestTty::Never)
        .verbose(false)
        .verbose(true)
        .stdin_null(false)
        .stdin_null(true)
        .escape_char(Some(b'!'))
        .escape_char(None)
        .local_forwards([LocalForward::Unix {
            bind: "discarded.sock".into(),
            target: "discarded-target.sock".into(),
        }])
        .local_forwards([LocalForward::Unix {
            bind: "listener.sock".into(),
            target: "target.sock".into(),
        }])
        .compression(false)
        .compression(true)
        .ciphers("discarded-cipher")
        .ciphers("aes256-ctr")
        .macs("discarded-mac")
        .macs("hmac-sha2-256")
        .build(destination(), session)?;

    assert_eq!(config.connection.chain().target.dest.user, "cli-user");
    assert_eq!(config.connection.chain().target.dest.port, 2222);
    assert_eq!(config.connection.chain().target.tty, RequestTty::Never);
    assert_eq!(
        config.connection.chain().target.auth.identities,
        Some(vec!["relative-cli-key".into(), "/keys/file".into()])
    );
    assert!(config.connection.chain().bastions.is_none());
    assert_eq!(
        config.session,
        Session::Exec {
            command: "printf hello".into()
        }
    );
    assert!(config.verbose);
    assert!(config.stdin_null);
    assert_eq!(config.escape_char, None);
    assert_eq!(
        config.local_forwards,
        Some(vec![LocalForward::Unix {
            bind: "listener.sock".into(),
            target: "target.sock".into(),
        }])
    );
    let preferred = config.preferred()?;
    assert_eq!(preferred.cipher.as_ref(), &[cipher::AES_256_CTR]);
    assert_eq!(preferred.mac.as_ref(), &[mac::HMAC_SHA256]);
    assert_eq!(
        preferred.compression.as_ref(),
        &[
            compression::ZLIB,
            compression::ZLIB_LEGACY,
            compression::NONE
        ]
    );

    Ok(())
}

#[test]
fn explicit_empty_lists_remain_distinct_from_omitted_lists() -> Result<()> {
    let config = ConfigBuilder::new()
        .config_file("none")
        .identities(["discarded-key"])
        .identities(Vec::<PathBuf>::new())
        .proxy_jump(["discarded-jump"])
        .proxy_jump(Vec::<String>::new())
        .local_forwards(Vec::<LocalForward>::new())
        .build(destination(), Session::None)?;

    assert_eq!(
        config.connection.chain().target.auth.identities,
        Some(Vec::new())
    );
    assert!(config.connection.chain().bastions.is_none());
    assert_eq!(config.local_forwards, Some(Vec::new()));
    assert_eq!(config.session, Session::None);

    Ok(())
}

#[test]
fn connection_policy_lookup_keeps_equal_hosts_bound_to_their_configuration() -> Result<()> {
    use openrussh_cli_config::Error;
    use openrussh_client::russh::kex;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("config.toml");
    std::fs::write(
        &path,
        "[hosts.target]\nhost-name = 'same.example'\nkex-algorithms = 'curve25519-sha256'\n[hosts.other]\nhost-name = 'same.example'\nkex-algorithms = 'diffie-hellman-group14-sha256'\n",
    )?;
    let build = |alias: &str| {
        let mut requested = destination();
        requested.host = alias.into();
        Config::builder()
            .config_file(&path)
            .build(requested, Session::None)
    };
    let target = build("target")?;
    let other = build("other")?;
    let target_host = &target.connection.chain().target;
    let other_host = &other.connection.chain().target;
    assert_eq!(target_host, other_host);

    for (connection, host, expected) in [
        (&target.connection, target_host, kex::CURVE25519),
        (&other.connection, other_host, kex::DH_G14_SHA256),
    ] {
        let mut preferred = Preferred::default();
        connection.algorithm_policy(host)?.apply(&mut preferred);
        assert_eq!(preferred.kex[0], expected);
    }
    assert!(matches!(
        target.connection.algorithm_policy(other_host),
        Err(Error::HostNotInChain)
    ));
    assert!(matches!(
        other.connection.algorithm_policy(target_host),
        Err(Error::HostNotInChain)
    ));
    Ok(())
}
