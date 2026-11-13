use openrussh_cli_config::{
    Config, ConfigBuilder, Destination, ForwardTarget, LocalBind, LocalForward, Session,
};
use openrussh_client::config::RequestTty;
use openrussh_client::russh::{Preferred, cipher, compression, mac};

use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

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

        assert_eq!(config.chain.target.dest.name, "target.example");
        assert_eq!(config.chain.target.dest.user, "cli-user");
        assert_eq!(config.chain.target.dest.port, 2222);
        assert_eq!(config.chain.target.tty, RequestTty::ForShell);
        assert!(config.chain.target.auth.identities.is_none());
        assert!(config.chain.bastions.is_none());
        assert!(config.meta.known_hosts.is_none());
        assert!(config.meta.log_level.is_none());
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
        .local_forwards([LocalForward {
            bind: LocalBind::Unix("discarded.sock".into()),
            target: ForwardTarget::Unix("discarded-target.sock".into()),
        }])
        .local_forwards([LocalForward {
            bind: LocalBind::Unix("listener.sock".into()),
            target: ForwardTarget::Unix("target.sock".into()),
        }])
        .compression(false)
        .compression(true)
        .ciphers("discarded-cipher")
        .ciphers("aes256-ctr")
        .macs("discarded-mac")
        .macs("hmac-sha2-256")
        .build(destination(), session)?;

    assert_eq!(config.chain.target.dest.user, "cli-user");
    assert_eq!(config.chain.target.dest.port, 2222);
    assert_eq!(config.chain.target.tty, RequestTty::Never);
    assert_eq!(
        config.chain.target.auth.identities,
        Some(vec!["relative-cli-key".into(), "/keys/file".into()])
    );
    assert!(config.chain.bastions.is_none());
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
        Some(vec![LocalForward {
            bind: LocalBind::Unix("listener.sock".into()),
            target: ForwardTarget::Unix("target.sock".into()),
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

    assert_eq!(config.chain.target.auth.identities, Some(Vec::new()));
    assert!(config.chain.bastions.is_none());
    assert_eq!(config.local_forwards, Some(Vec::new()));
    assert_eq!(config.session, Session::None);

    Ok(())
}
