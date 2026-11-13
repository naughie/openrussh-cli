use super::*;
use crate::tests::TestConfig;
use openrussh_client::config::Target;

use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn config(host: &str) -> TestConfig {
    TestConfig::new(host)
}

#[test]
fn sample_tables_resolve_host_settings_and_defaults() -> Result<()> {
    use openrussh_client::config::RequestTty;

    let text = r#"
[default]
identities-only = "yes"
[hosts]
github = { user = "git", port = 22, identity-file = "~/.ssh/id_ed25519" }
[hosts.my-host]
host-name = "example.com"
identity-file = ["/keys/id1", "/keys/id2"]
identity-agent = "~/.ssh/agent.sock"
request-tty = "yes"
"#;
    let github = query(text, &config("github").options())?.target;

    assert_eq!(github.dest.user, "git");
    assert_eq!(github.dest.port, 22);
    assert!(github.auth.identities_only);

    let host = query(text, &config("my-host").options())?.target;

    assert_eq!(host.dest.name, "example.com");
    assert_eq!(
        host.auth.identities,
        Some(vec!["/keys/id1".into(), "/keys/id2".into()])
    );
    assert_eq!(
        host.auth.agent,
        Some(std::env::home_dir().unwrap().join(".ssh/agent.sock"))
    );
    assert_eq!(host.tty, RequestTty::IfTty);
    assert!(host.auth.identities_only);

    Ok(())
}

#[test]
fn host_names_are_literal_and_case_sensitive() -> Result<()> {
    let text = r#"
[default]
user = "default"
[hosts]
Sample = { user = "capital" }
"s*" = { user = "star" }
"s?" = { user = "question" }
"!sample" = { user = "bang" }
"#;

    for (name, user) in [
        ("sample", "default"),
        ("Sample", "capital"),
        ("s*", "star"),
        ("s?", "question"),
        ("!sample", "bang"),
        ("sa", "default"),
    ] {
        assert_eq!(query(text, &config(name).options())?.target.dest.user, user);
    }

    Ok(())
}

#[test]
fn identities_accumulate_in_cli_host_default_order() -> Result<()> {
    let mut conf = config("sample");
    conf.identities = Some(vec!["cli".into()]);
    conf.destination.user = Some("cli-user".to_owned());
    conf.destination.port = Some(2200);

    let host = query(
        r#"
[default]
user = "default"
port = 22
identity-file = ["/keys/default1", "/keys/default2"]
[hosts.sample]
user = "host"
port = "2222"
identity-file = "/keys/host key"
certificate-file = "/keys/certificate"
"#,
        &conf.options(),
    )?
    .target;

    assert_eq!(host.dest.user, "cli-user");
    assert_eq!(host.dest.port, 2200);
    assert_eq!(
        host.auth.identities,
        Some(
            ["cli", "/keys/host key", "/keys/default1", "/keys/default2"]
                .map(PathBuf::from)
                .into()
        )
    );
    assert_eq!(host.auth.cert, Some("/keys/certificate".into()));

    Ok(())
}

#[test]
fn omitted_and_agent_directives_keep_library_behavior() -> Result<()> {
    use openrussh_client::ssh2_config::{ParseRule, SshConfig};

    for (toml, openssh) in [
        ("", ""),
        (
            "[default]\nidentity-agent = 'SSH_AUTH_SOCK'",
            "IdentityAgent SSH_AUTH_SOCK",
        ),
        (
            "[default]\nidentity-agent = '$SSH_AUTH_SOCK'",
            "IdentityAgent $SSH_AUTH_SOCK",
        ),
        ("[default]\nidentity-agent = 'none'", "IdentityAgent none"),
        (
            "[default]\nidentity-agent = '~/agent'",
            "IdentityAgent ~/agent",
        ),
    ] {
        let openssh = SshConfig::default()
            .parse(&mut openssh.as_bytes(), ParseRule::ALLOW_UNSUPPORTED_FIELDS)?;
        let expected = Target::parse("sample").query(&openssh)?;

        assert_eq!(
            query(toml, &config("sample").options())?.target,
            expected.target
        );
    }

    Ok(())
}

#[test]
fn cli_paths_expand_only_the_current_users_home() -> Result<()> {
    let home = std::env::home_dir().expect("Missing home directory");
    let user = whoami::username()?;

    for path in ["", "~", "~/key", "relative/key", "/absolute/key", "."] {
        let expected = match path {
            "" => None,
            "~" => Some(home.clone()),
            "~/key" => Some(home.join("key")),
            path => Some(PathBuf::from(path)),
        };

        assert_eq!(paths::cli_path(Path::new(path)), expected, "{path}");
    }

    assert_eq!(
        paths::cli_path(Path::new(&format!("~{user}/key"))),
        Some(home.join("key"))
    );
    assert_eq!(
        paths::cli_path(Path::new(&format!("~{user}-other/key"))),
        None
    );

    Ok(())
}

#[test]
fn native_cli_identity_paths_preserve_non_utf8_bytes() -> Result<()> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let path = PathBuf::from(OsString::from_vec(vec![b'k', 0xff]));
    let mut conf = config("sample");
    conf.identities = Some(vec![path.clone()]);

    assert_eq!(
        query("", &conf.options())?.target.auth.identities,
        Some(vec![path])
    );

    Ok(())
}

#[test]
fn jump_lists_recurse_only_through_the_first_exact_host() -> Result<()> {
    for jumps in ["'Jump,last'", "['Jump', 'last']"] {
        let text = format!(
            r#"
[default]
user = "default"
[hosts.sample]
proxy-jump = {jumps}
[hosts.Jump]
host-name = "first.example"
proxy-jump = "s*"
[hosts.jump]
proxy-jump = "wrong"
[hosts.last]
proxy-jump = "ignored"
[hosts."s*"]
host-name = "literal.example"
[hosts.star]
host-name = "wrong.example"
"#
        );
        let chain = query(&text, &config("sample").options())?;
        let bastions = chain.bastions.expect("Missing jumps");

        assert_eq!(
            bastions
                .iter()
                .map(|h| h.dest.name.as_str())
                .collect::<Vec<_>>(),
            ["last", "first.example", "literal.example"]
        );
    }

    Ok(())
}

#[test]
fn cli_and_empty_jump_lists_override_inherited_jumps() -> Result<()> {
    let text = "[default]\nproxy-jump = 'jump'\n[hosts.sample]\nproxy-jump = []";

    assert!(query(text, &config("sample").options())?.bastions.is_none());

    let mut conf = config("sample");
    conf.proxy_jump = Some(Vec::new());

    assert!(
        query("[hosts.sample]\nproxy-jump = 'jump'", &conf.options())?
            .bastions
            .is_none()
    );
    conf.proxy_jump = Some(vec!["alice@Jump:2222".to_owned()]);

    let chain = query("[hosts.Jump]\nhost-name = 'jump.example'", &conf.options())?;
    let bastions = chain.bastions.expect("Missing CLI jump");

    assert_eq!(bastions.len(), 1);
    assert_eq!(bastions[0].dest.name, "jump.example");
    assert_eq!(bastions[0].dest.user, "alice");
    assert_eq!(bastions[0].dest.port, 2222);

    Ok(())
}

#[test]
fn ports_accept_strings_and_toml_integer_notation() -> Result<()> {
    for port in ["'2222'", "2222", "2_222", "0x8ae"] {
        assert_eq!(
            query(
                &format!("[default]\nport = {port}"),
                &config("sample").options()
            )?
            .target
            .dest
            .port,
            2222
        );
    }

    Ok(())
}

#[test]
fn invalid_sections_directives_and_value_types_are_rejected() {
    for text in [
        "[unknown]",
        "default = 1",
        "[hosts]\nsample = 'host'",
        "[default]\ninclude = 'other.toml'",
        "[hosts.unused]\ninclude = ['other']",
        "[default]\nidentities-only = 1",
        "[default]\nport = 65536",
        "[default]\nport = -1",
        "[default]\nport = 22.0",
        "[default]\nuser = ['alice']",
        "[default]\nidentity-file = [1]",
        "[hosts.unused]\nproxy-jump = ['jump', 1]",
        "[hosts.sample",
    ] {
        assert!(query(text, &config("sample").options()).is_err(), "{text}");
    }
}
