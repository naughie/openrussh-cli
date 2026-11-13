fn main() {
    let mut rt = t::runtime();

    rt.register("CLI help and unsupported options", async || {
        let client = t::Client::new("")?;
        let output = client.run(&["--help"], b"").await?;
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: ssh"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("-v"));

        let output = client.run(&["-s", "target", "sftp"], b"").await?;
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));

        let output = client.run(&["-o", "BatchMode=yes", "target"], b"").await?;
        assert_eq!(output.status.code(), Some(255));
        assert!(String::from_utf8_lossy(&output.stderr).contains("Unsupported SSH option"));

        for args in [
            &["-L", "8080:/tmp/remote.sock", "target"][..],
            &["-L", "/tmp/local.sock:localhost:80", "target"][..],
        ] {
            let output = client.run(args, b"").await?;
            assert_eq!(output.status.code(), Some(255));
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("Invalid local forwarding specification")
            );
        }

        Ok(())
    });

    rt.register("-o overrides the final host in every configuration format", async || {
        let dir = t::tempdir()?;
        let identity = dir.path().join("identity with spaces");
        let known_hosts = dir.path().join("trusted hosts");
        std::fs::copy("/home/alice/.ssh/id_ed25519", &identity)?;
        std::fs::copy("/home/alice/.ssh/known_hosts", &known_hosts)?;
        let identity = format!("IdentityFile='{}'", identity.display());
        let known_hosts = format!("UserKnownHostsFile='{}'", known_hosts.display());

        let clients = [
            t::Client::settings(" HostName 127.0.0.2\n User nobody\n Port 1\n CertificateFile /missing-certificate\n IdentityAgent /missing-agent\n")?,
            t::Client::toml("[meta]\nknown-hosts = '/missing-known-hosts'\n[hosts.target]\nhost-name = '127.0.0.2'\nuser = 'nobody'\nport = 1\ncertificate-file = '/missing-certificate'\nidentity-agent = '/missing-agent'\n")?,
            t::Client::lua("return {meta = {known_hosts = '/missing-known-hosts'}, hosts = {target = {host_name = '127.0.0.2', user = 'nobody', port = 1, certificate_file = '/missing-certificate', identity_agent = '/missing-agent'}}}")?,
        ];
        for client in clients {
            let output = client.run(&[
                "-o", "hOsTnAmE 127.0.0.1", "-o", "User=bob", "-o", "Port=2222",
                "-o", &identity, "-o", &known_hosts, "-o", "IdentityAgent=none",
                "-o", "CertificateFile=none", "-o", "IdentitiesOnly=yes",
                "-o", "RequestTTY=no", "-o", "KexAlgorithms=curve25519-sha256",
                "-o", "HostKeyAlgorithms=ssh-ed25519", "-o", "LogLevel=ERROR",
                "target", "printf options",
            ], b"").await?;
            t::expect(&output, 0, b"options", b"");
        }

        let client = t::Client::default_config()?;
        let output = client.run(&[
            "-F", "none", "-o", "HostName=127.0.0.1", "-o", "User=bob",
            "-o", "Port=2222", "-o", &identity, "-o", &known_hosts,
            "-o", "RequestTTY=no", "target", "printf defaults",
        ], b"").await?;
        t::expect(&output, 0, b"defaults", b"");
        Ok(())
    });

    rt.register(
        "-o and dedicated user and port options keep the first value",
        async || {
            let client = t::Client::new("")?;
            for options in [
                vec![
                    "-o",
                    "User=bob",
                    "-l",
                    "nobody",
                    "-o",
                    "Port=2222",
                    "-p",
                    "1",
                ],
                vec![
                    "-l",
                    "bob",
                    "-o",
                    "User=nobody",
                    "-p",
                    "2222",
                    "-o",
                    "Port=1",
                ],
                vec![
                    "-o",
                    "User=bob",
                    "-o",
                    "User=nobody",
                    "-o",
                    "Port=2222",
                    "-o",
                    "Port=1",
                ],
            ] {
                let mut args = options;
                args.extend(["target", "printf first"]);
                let output = client.run(&args, b"").await?;
                t::expect(&output, 0, b"first", b"");
            }
            Ok(())
        },
    );

    rt.register(
        "address-family options filter direct connections",
        async || {
            let client = t::Client::new("")?;
            for options in [
                vec!["-4"],
                vec!["-o", "AddressFamily=inet"],
                vec!["-o", "AddressFamily=inet6", "-4"],
                vec!["-4", "-o", "AddressFamily=inet6"],
            ] {
                let mut args = options;
                args.extend(["target", "printf ipv4"]);
                let output = client.run(&args, b"").await?;
                t::expect(&output, 0, b"ipv4", b"");
            }
            for flag in ["-6", "-oAddressFamily=inet6"] {
                let output = client.run(&[flag, "target", "true"], b"").await?;
                assert_eq!(output.status.code(), Some(255));
                assert!(output.stdout.is_empty());
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("requested address family")
                );
            }
            Ok(())
        },
    );

    rt.register(
        "-o RequestTTY participates in terminal flag ordering",
        async || {
            let client = t::Client::new("")?;
            for (options, expected) in [
                (vec!["-o", "RequestTTY=force"], b"tty".as_slice()),
                (vec!["-o", "RequestTTY=no", "-tt"], b"tty".as_slice()),
                (vec!["-o", "RequestTTY=force", "-T"], b"pipe".as_slice()),
                (vec!["-T", "-o", "RequestTTY=force"], b"pipe".as_slice()),
            ] {
                let mut args = options;
                args.extend([
                    "target",
                    "if test -t 0; then printf tty; else printf pipe; fi",
                ]);
                let output = client.run(&args, b"").await?;
                t::expect(&output, 0, expected, b"");
            }
            Ok(())
        },
    );

    rt.register("SendEnv selects local values and removes earlier patterns", async || {
        use t::tokio::time::{Duration, timeout};

        let client = t::Client::new("")?;
        let command = "printf '%s|%s|%s' \"${OPENRUSSH_TEST_KEEP1-unset}\" \"${OPENRUSSH_TEST_REMOVE-unset}\" \"${OPENRUSSH_TEST_SPACE-unset}\"";
        for (options, expected) in [
            (Vec::new(), b"unset|unset|unset".as_slice()),
            (vec!["-o", "SendEnv=OPENRUSSH_TEST_*", "-o", "SendEnv=-OPENRUSSH_TEST_*", "-o", "SendEnv=OPENRUSSH_TEST_KEEP? OPENRUSSH_TEST_SPACE"], b"kept|unset|value with spaces".as_slice()),
        ] {
            let mut args = options;
            args.extend(["target", command]);
            let output = timeout(Duration::from_secs(15), client.command(&args)
                .env("OPENRUSSH_TEST_KEEP1", "kept")
                .env("OPENRUSSH_TEST_REMOVE", "removed")
                .env("OPENRUSSH_TEST_SPACE", "value with spaces")
                .output()).await??;
            t::expect(&output, 0, expected, b"");
        }
        Ok(())
    });

    rt.register(
        "RemoteCommand and StdinNull use the normal session path",
        async || {
            let client = t::Client::new("")?;
            let output = client
                .run(
                    &[
                        "-o",
                        "RemoteCommand=cat; printf done",
                        "-o",
                        "StdinNull=yes",
                        "target",
                    ],
                    b"ignored input",
                )
                .await?;
            t::expect(&output, 0, b"done", b"");

            for options in [
                vec!["-o", "RemoteCommand=true", "target", "true"],
                vec!["-o", "SessionType=default", "-N", "target"],
                vec![
                    "-o",
                    "CertificateFile=one",
                    "-o",
                    "CertificateFile=two",
                    "target",
                ],
                vec!["-o", "UserKnownHostsFile=one two", "target"],
                vec!["-o", "Compression=maybe", "target"],
                vec!["-o", "User='unterminated", "target"],
            ] {
                let output = client.run(&options, b"").await?;
                assert_eq!(output.status.code(), Some(255));
                assert!(output.stdout.is_empty());
                assert!(!output.stderr.is_empty());
            }
            Ok(())
        },
    );

    rt.register("logging defaults follow verbose mode", async || {
        let openssh = t::Client::new("")?;
        let [toml, lua] = logging_clients(None)?;

        for client in [&openssh, &toml, &lua] {
            let output = client.run(&["target", "printf", "default"], b"").await?;
            t::expect(&output, 0, b"default", b"");

            let output = client
                .run(&["-v", "target", "printf", "default"], b"")
                .await?;
            assert_eq!(output.status.code(), Some(0));
            assert_eq!(output.stdout, b"default");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("debug1: Connecting to "), "{stderr}");
            assert!(stderr.contains("debug1: Entering session."), "{stderr}");
            assert!(!stderr.contains("debug2: "), "{stderr}");

            let output = client.run(&["-p", "1", "target", "true"], b"").await?;
            assert_eq!(output.status.code(), Some(255));
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.starts_with("ssh: "), "{stderr}");
            assert!(!stderr.contains("debug1: "), "{stderr}");

            let output = client
                .run(&["-v", "-p", "1", "target", "true"], b"")
                .await?;
            assert_eq!(output.status.code(), Some(255));
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("debug1: Connecting to "), "{stderr}");
            assert!(
                stderr.lines().any(|line| line.starts_with("ssh: ")),
                "{stderr}"
            );
        }

        Ok(())
    });

    rt.register("native log levels require verbose mode", async || {
        for level in ["none", "error", "warn", "info", "debug"] {
            for client in logging_clients(Some(level))? {
                let output = client.run(&["target", "printf", "quiet"], b"").await?;
                t::expect(&output, 0, b"quiet", b"");

                let output = client
                    .run(&["-v", "target", "printf", "verbose"], b"")
                    .await?;
                assert_eq!(
                    output.status.code(),
                    Some(0),
                    "{level}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(output.stdout, b"verbose");

                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(!stderr.contains('\x1b'), "{level}: {stderr}");
                assert!(!stderr.contains("openrussh_cli::"), "{level}: {stderr}");

                match level {
                    "info" => {
                        assert!(!stderr.is_empty());
                        assert!(!stderr.contains("debug1: "), "{stderr}");
                    }
                    "debug" => assert!(stderr.contains("debug1: "), "{stderr}"),
                    _ => assert!(stderr.is_empty(), "{level}: {stderr}"),
                }
            }
        }

        Ok(())
    });

    rt.register(
        "native log filters select application and dependency targets",
        async || {
            for filter in ["debug", "debug,russh=info", "error,openrussh_cli=debug"] {
                for client in logging_clients(Some(filter))? {
                    let output = client
                        .run(&["-v", "target", "printf", "filtered"], b"")
                        .await?;
                    assert_eq!(
                        output.status.code(),
                        Some(0),
                        "{}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    assert_eq!(output.stdout, b"filtered");

                    let stderr = String::from_utf8_lossy(&output.stderr);
                    assert!(
                        stderr.contains("debug1: Connecting to "),
                        "{filter}: {stderr}"
                    );
                    assert!(
                        stderr.contains("debug1: Entering session."),
                        "{filter}: {stderr}"
                    );
                    // russh emits this for every handshake, before key exchange.
                    assert_eq!(
                        stderr.contains("ssh id = "),
                        filter == "debug",
                        "{filter}: {stderr}"
                    );
                }
            }

            Ok(())
        },
    );

    rt.register(
        "non-verbose mode caps target filters at errors",
        async || {
            for filter in [
                "debug,russh=info",
                "error,openrussh_cli=debug",
                "trace,russh=trace,openrussh_cli=trace",
            ] {
                for client in logging_clients(Some(filter))? {
                    let output = client.run(&["target", "printf", "quiet"], b"").await?;
                    t::expect(&output, 0, b"quiet", b"");

                    let output = client.run(&["-p", "1", "target", "true"], b"").await?;
                    assert_eq!(output.status.code(), Some(255));
                    assert!(output.stdout.is_empty());

                    let stderr = String::from_utf8_lossy(&output.stderr);
                    assert!(!stderr.is_empty());
                    assert!(
                        stderr.lines().all(|line| line.starts_with("ssh: ")),
                        "{filter}: {stderr}"
                    );
                }
            }

            Ok(())
        },
    );

    rt.register(
        "none suppresses connection errors in both modes",
        async || {
            for level in ["none", "error", "warn", "info", "debug"] {
                for client in logging_clients(Some(level))? {
                    for verbose in [false, true] {
                        let args = if verbose {
                            &["-v", "-p", "1", "target", "true"][..]
                        } else {
                            &["-p", "1", "target", "true"][..]
                        };
                        let output = client.run(args, b"").await?;
                        assert_eq!(output.status.code(), Some(255));
                        assert!(output.stdout.is_empty());

                        let stderr = String::from_utf8_lossy(&output.stderr);
                        if level == "none" {
                            assert!(stderr.is_empty(), "{stderr}");
                        } else {
                            assert!(
                                stderr.lines().any(|line| line.starts_with("ssh: ")),
                                "{level}: {stderr}"
                            );
                            if !verbose || level != "debug" {
                                assert!(!stderr.contains("debug1: "), "{level}: {stderr}");
                            }
                        }
                    }
                }
            }

            Ok(())
        },
    );

    rt.register("native log levels apply to resolution errors", async || {
        for level in ["none", "error"] {
            let toml = t::Client::toml(&format!(
                "[meta]\nlog-level = '{level}'\nknown-hosts = 'relative/hosts'\n"
            ))?;
            let lua = t::Client::lua(&format!(
                "return {{meta = {{log_level = '{level}', known_hosts = 'relative/hosts'}}}}"
            ))?;

            for (client, extension) in [(&toml, ".toml"), (&lua, ".lua")] {
                for args in [&["target", "true"][..], &["-v", "target", "true"][..]] {
                    let output = client.run(args, b"").await?;
                    assert_eq!(output.status.code(), Some(255));
                    assert!(output.stdout.is_empty());

                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if level == "none" {
                        assert!(stderr.is_empty(), "{stderr}");
                    } else {
                        assert!(stderr.starts_with("ssh: "), "{stderr}");
                        assert!(stderr.contains(extension), "{stderr}");
                        assert!(
                            stderr.contains("known_hosts must be an absolute path"),
                            "{stderr}"
                        );
                        assert!(stderr.contains("relative/hosts"), "{stderr}");
                    }
                }
            }
        }

        Ok(())
    });

    rt.register("none preserves remote output and exit status", async || {
        for client in logging_clients(Some("none"))? {
            let output = client
                .run(
                    &["-v", "target", "printf stdout; printf stderr >&2; exit 7"],
                    b"",
                )
                .await?;
            t::expect(&output, 7, b"stdout", b"stderr");
        }

        Ok(())
    });

    rt.register(
        "CLI connection options override the selected config",
        async || {
            let client = t::Client::settings("Port 1\nUser wrong\nIdentityFile relative/missing")?;
            let output = client
                .run(
                    &[
                        "-p",
                        "2222",
                        "-l",
                        "bob",
                        "-i",
                        "/home/alice/.ssh/id_ed25519",
                        "target",
                        "printf",
                        "overrides",
                    ],
                    b"",
                )
                .await?;
            t::expect(&output, 0, b"overrides", b"");

            Ok(())
        },
    );

    rt.register("-F none replaces the selected config", async || {
        let client = t::Client::settings(
            "Host *\nHostName invalid.invalid\nPort 1\nIdentityFile /missing/identity",
        )?;
        let output = client
            .run(
                &[
                    "-F",
                    "none",
                    "-p",
                    "2222",
                    "-l",
                    "bob",
                    "-i",
                    "/home/alice/.ssh/id_ed25519",
                    "127.0.0.1",
                    "printf",
                    "explicit",
                ],
                b"",
            )
            .await?;
        t::expect(&output, 0, b"explicit", b"");

        Ok(())
    });

    rt.register("-J none clears a configured jump", async || {
        let client = t::Client::new("ProxyJump invalid.invalid")?;
        let output = client
            .run(&["-J", "none", "target", "printf", "direct"], b"")
            .await?;
        t::expect(&output, 0, b"direct", b"");

        Ok(())
    });

    rt.register("-F TOML resolves defaults and host settings", async || {
        let client = t::Client::toml(
            r#"
[default]
host-name = "invalid.invalid"
user = "wrong"
port = 1
request-tty = "force"
identities-only = true

[hosts.target]
host-name = "127.0.0.1"
user = "bob"
port = 2222
identity-file = ["~/.ssh/id_ed25519"]
request-tty = false
"#,
        )?;
        let output = client
            .run(
                &["target", "if test -t 0; then exit 1; else printf toml; fi"],
                b"",
            )
            .await?;
        t::expect(&output, 0, b"toml", b"");

        Ok(())
    });

    rt.register("CLI options override TOML settings", async || {
        let client = t::Client::toml(
            r#"
[hosts.target]
host-name = "127.0.0.1"
user = "wrong"
port = 1
identity-file = "/missing/identity"
proxy-jump = "invalid.invalid"
request-tty = "force"
"#,
        )?;
        let output = client
            .run(
                &[
                    "-p",
                    "2222",
                    "-l",
                    "bob",
                    "-i",
                    "/home/alice/.ssh/id_ed25519",
                    "-J",
                    "none",
                    "-T",
                    "target",
                    "if test -t 0; then exit 1; else printf overrides; fi",
                ],
                b"",
            )
            .await?;
        t::expect(&output, 0, b"overrides", b"");

        Ok(())
    });

    rt.register("TOML hosts use exact case-sensitive names", async || {
        let client = t::Client::toml(
            r#"
[default]
host-name = "127.0.0.1"
user = "bob"
port = 2222
identity-file = "~/.ssh/id_ed25519"
request-tty = "no"

[hosts]
"t*" = { port = 1 }
Target = { port = 1 }
"#,
        )?;
        let output = client.run(&["target", "printf", "exact"], b"").await?;
        t::expect(&output, 0, b"exact", b"");

        for host in ["t*", "Target"] {
            let output = client.run(&[host, "true"], b"").await?;
            assert_eq!(output.status.code(), Some(255));
        }

        Ok(())
    });

    rt.register(
        "TOML jump hosts use exact names and inherited defaults",
        async || {
            for jumps in ["'Jump'", "['Jump']"] {
                let client = t::Client::toml(&format!(
                    "[default]\nhost-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
                 identity-file = '~/.ssh/id_ed25519'\nrequest-tty = 'no'\n\
                 [hosts.target]\nproxy-jump = {jumps}\n\
                 [hosts.Jump]\n[hosts.jump]\nport = 1\n"
                ))?;
                let output = client.run(&["target", "printf", "jumped"], b"").await?;
                t::expect(&output, 0, b"jumped", b"");
            }

            Ok(())
        },
    );

    rt.register("-F Lua evaluates defaults and host settings", async || {
        for identities in ["home .. '/.ssh/id_ed25519'", "{home .. '/.ssh/id_ed25519'}"] {
            let client = t::Client::lua(&format!(
                r#"
local home = os.getenv('HOME')
local function target()
    return {{
        host_name = '127.0.0.1',
        user = 'bob',
        port = 2200 + 22,
        identity_file = {identities},
        request_tty = false,
    }}
end
return {{
    default = {{
        host_name = 'invalid.invalid',
        user = 'wrong',
        port = 1,
        identities_only = true,
        request_tty = 'force',
    }},
    hosts = {{ target = target() }},
}}
"#
            ))?;
            let output = client
                .run(
                    &["target", "if test -t 0; then exit 1; else printf lua; fi"],
                    b"",
                )
                .await?;
            t::expect(&output, 0, b"lua", b"");
        }

        Ok(())
    });

    rt.register(
        "Lua cli module exposes local information and constants",
        async || {
            let client = t::Client::lua(
                r#"
local cli = require('cli')
assert(require('cli') == cli)
assert(type(cli.user_name) == 'function')
assert(type(cli.home_dir) == 'function')
assert(type(cli.host_name) == 'function')
assert(cli.os == 'linux')
assert(cli.os_family == 'unix')
assert(type(cli.arch) == 'string' and cli.arch ~= '')
assert(cli.__root__ == cli.__dir__)
for _, name in ipairs({'__file__', '__dir__', '__root__', 'os', 'arch', 'os_family'}) do
    local original = cli[name]
    assert(type(original) == 'string')
    local replaced = pcall(function() cli[name] = 'replacement' end)
    assert(not replaced, name .. ' must reject reassignment')
    assert(cli[name] == original)
    local deleted = pcall(function() cli[name] = nil end)
    assert(not deleted, name .. ' must reject deletion')
    assert(cli[name] == original)
end
assert(cli.user_name() == 'alice')
local home = cli.home_dir()
assert(home == '/home/alice')
local hostname = cli.host_name()
assert(type(hostname) == 'string' and hostname ~= '')
local hostname_file = assert(io.open('/proc/sys/kernel/hostname', 'r'))
assert(hostname == hostname_file:read('*l'))
hostname_file:close()
return {
    hosts = {
        target = {
            host_name = '127.0.0.1', user = 'bob', port = 2222,
            identity_file = home .. '/.ssh/id_ed25519', request_tty = false,
        },
    },
}
"#,
            )?;
            let output = client.run(&["target", "printf", "local"], b"").await?;
            t::expect(&output, 0, b"local", b"");

            Ok(())
        },
    );

    rt.register(
        "Lua temporary identities survive authentication and session cleanup",
        async || {
            use std::os::unix::fs::PermissionsExt as _;
            use std::path::PathBuf;
            use t::tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            use t::tokio::time::{Duration, timeout};

            let dir = t::tempdir()?;
            let manifest = dir.path().join("paths");
            let client = t::Client::lua(&format!(
                r#"
local cli = require('cli')
local named = assert(cli.temp_file('identity'))
assert(cli.temp_file('identity') == named)
local random = assert(cli.temp_file())
local other = assert(cli.temp_file(nil))
assert(random ~= other and random ~= named and other ~= named)
local source = assert(io.open('/home/alice/.ssh/id_ed25519', 'r'))
local key = assert(source:read('*a'))
source:close()
local identity = assert(io.open(named, 'w'))
assert(identity:write(key))
identity:close()
local manifest = assert(io.open('{manifest}', 'w'))
assert(manifest:write(named, '\n', random, '\n', other, '\n'))
manifest:close()
return {{hosts = {{target = {{
    host_name = '127.0.0.1', user = 'bob', port = 2222,
    identity_file = named, request_tty = false,
}}}}}}
"#,
                manifest = manifest.to_str().unwrap(),
            ))?;
            let mut child = client
                .command(&["target", "printf 'ready\\n'; read continue; printf done"])
                .spawn()?;
            let mut ready = [0_u8; 6];
            timeout(
                Duration::from_secs(15),
                child.stdout.as_mut().unwrap().read_exact(&mut ready),
            )
            .await??;
            assert_eq!(&ready, b"ready\n");
            assert!(child.try_wait()?.is_none());

            let paths = std::fs::read_to_string(&manifest)?
                .lines()
                .map(PathBuf::from)
                .collect::<Vec<_>>();
            assert_eq!(paths.len(), 3);
            let parent = paths[0].parent().unwrap();
            assert_eq!(paths[0].file_name().unwrap(), "identity");
            assert!(paths.iter().all(|path| path.parent() == Some(parent)));
            assert_eq!(
                std::fs::metadata(parent)?.permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::read(&paths[0])?,
                std::fs::read("/home/alice/.ssh/id_ed25519")?
            );

            child
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"continue\n")
                .await?;
            let output = timeout(Duration::from_secs(15), child.wait_with_output()).await??;
            t::expect(&output, 0, b"done", b"");
            assert!(!parent.exists());
            assert!(paths.iter().all(|path| !path.exists()));

            Ok(())
        },
    );

    rt.register(
        "Lua temp_file is unavailable once the VM owner has been destroyed",
        async || {
            let dir = t::tempdir()?;
            let manifest = dir.path().join("finalizer-result");
            let client = t::Client::lua(&format!(
                r#"
local cli = require('cli')
local identity = assert(cli.temp_file('identity'))
local source = assert(io.open('/home/alice/.ssh/id_ed25519', 'r'))
local key = assert(source:read('*a'))
assert(source:close())
local file = assert(io.open(identity, 'w'))
assert(file:write(key))
assert(file:close())
_G.finalizer = setmetatable({{}}, {{__gc = function()
    local result = assert(io.open('{manifest}', 'w'))
    assert(result:write(tostring(cli.temp_file('identity')), '\n',
        tostring(cli.temp_file())))
    assert(result:close())
end}})
return {{hosts = {{target = {{
    host_name = '127.0.0.1', user = 'bob', port = 2222,
    identity_file = identity, request_tty = false,
}}}}}}
"#,
                manifest = manifest.to_str().unwrap(),
            ))?;
            let output = client.run(&["target", "printf finalizer"], b"").await?;
            t::expect(&output, 0, b"finalizer", b"");
            assert_eq!(std::fs::read_to_string(manifest)?, "nil\nnil");
            Ok(())
        },
    );

    rt.register(
        "Lua temporary files are removed after configuration and connection failures",
        async || {
            use std::path::PathBuf;

            let dir = t::tempdir()?;
            for (index, ending) in [
                "error('intentional failure after temp_file')",
                "return {unknown = true}",
                "return {hosts = {target = {identity_file = path, kex_algorithms = 'invalid-algorithm'}}}",
                "return {hosts = {target = {host_name = '127.0.0.1', port = 1, identity_file = path}}}",
            ]
            .into_iter()
            .enumerate()
            {
                let manifest = dir.path().join(format!("path-{index}"));
                let client = t::Client::lua(&format!(
                    r#"
local path = assert(require('cli').temp_file('secret'))
local secret = assert(io.open(path, 'w'))
assert(secret:write('private contents'))
secret:close()
local manifest = assert(io.open('{manifest}', 'w'))
assert(manifest:write(path))
manifest:close()
{ending}
"#,
                    manifest = manifest.to_str().unwrap(),
                ))?;
                let output = client.run(&["target", "true"], b"").await?;
                assert_eq!(output.status.code(), Some(255));
                assert!(output.stdout.is_empty());
                assert!(!output.stderr.is_empty());
                let path = PathBuf::from(std::fs::read_to_string(&manifest)?);
                assert!(!path.exists(), "{ending}");
                assert!(!path.parent().unwrap().exists(), "{ending}");
            }

            Ok(())
        },
    );

    rt.register(
        "Lua temp_file caches failure even after its temporary directory becomes usable",
        async || {
            use t::tokio::time::{Duration, timeout};

            let dir = t::tempdir()?;
            let ready = dir.path().join("ready");
            std::fs::create_dir(&ready)?;
            let client = t::Client::lua(
                r#"
local cli = require('cli')
assert(cli.temp_file() == nil)
local temp_dir = assert(os.getenv('TMPDIR'))
assert(os.rename(assert(os.getenv('OPENRUSSH_TEMP_FILE_READY')), temp_dir))
local probe = assert(io.open(temp_dir .. '/probe', 'w'))
probe:close()
assert(cli.temp_file('after-failure') == nil)
return {hosts = {target = {
    host_name = '127.0.0.1', user = 'bob', port = 2222,
    identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
}}}
"#,
            )?;
            let output = timeout(
                Duration::from_secs(15),
                client
                    .command(&["target", "printf", "nil"])
                    .env("TMPDIR", dir.path().join("missing"))
                    .env("OPENRUSSH_TEMP_FILE_READY", &ready)
                    .output(),
            )
            .await??;
            t::expect(&output, 0, b"nil", b"");

            Ok(())
        },
    );

    rt.register(
        "Lua cli debug returns formatted strings for explicit printing",
        async || {
            let client = t::Client::lua(
                r#"
local cli = require('cli')
local value = {outer = {answer = 42}}
assert(select('#', cli.debug(value)) == 1)
local formatted = cli.debug(value)
assert(type(formatted) == 'string')
assert(formatted == '{\n  outer = {\n    answer = 42,\n  },\n}')
print(formatted)
print(cli.debug(value, 4))
print(cli.debug(value, 0))
print(cli.debug(value, nil))
print(cli.debug(nil))
print(cli.debug('line\n"quoted"'))
print(cli.debug(true))
return {
    meta = {log_level = 'none'},
    hosts = {
        target = {
            host_name = '127.0.0.1', user = 'bob', port = 2222,
            identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
        },
    },
}
"#,
            )?;
            let output = client.run(&["target", "printf", "remote"], b"").await?;
            let expected = concat!(
                "{\n  outer = {\n    answer = 42,\n  },\n}\n",
                "{\n    outer = {\n        answer = 42,\n    },\n}\n",
                "{\nouter = {\nanswer = 42,\n},\n}\n",
                "{\n  outer = {\n    answer = 42,\n  },\n}\n",
                "nil\n",
                "\"line\\n\\\"quoted\\\"\"\n",
                "true\n",
                "remote",
            );
            t::expect(&output, 0, expected.as_bytes(), b"");

            Ok(())
        },
    );

    rt.register("Lua cli debug rejects invalid indentation", async || {
        for indent in ["-1", "1.5", "true", "'bad'"] {
            let client = t::Client::lua(&format!(
                "require('cli').debug({{}}, {indent}); return {{}}",
            ))?;
            let output = client.run(&["target", "true"], b"").await?;
            let stderr = String::from_utf8_lossy(&output.stderr);

            assert_eq!(output.status.code(), Some(255));
            assert!(output.stdout.is_empty());
            assert!(stderr.contains(".lua"), "{indent}: {stderr}");
            assert!(stderr.contains("indent"), "{indent}: {stderr}");
        }

        Ok(())
    });

    rt.register(
        "Lua cli paths locate the actual config, modules, and adjacent files",
        async || {
            use std::os::unix::fs::symlink;
            use std::path::Path;
            use std::process::Stdio;
            use t::tokio::time::{Duration, timeout};

            let dir = t::tempdir()?;
            let actual = dir.path().join("actual");
            let work = dir.path().join("work");
            std::fs::create_dir_all(actual.join("nested"))?;
            std::fs::create_dir_all(actual.join("config/some/module"))?;
            std::fs::create_dir_all(actual.join("another"))?;
            std::fs::create_dir(&work)?;
            let actual = actual.canonicalize()?;
            std::fs::write(
                actual.join("config/some/module/init.lua"),
                b"return {host_name = '127.0.0.1', user = 'bob', port = 2222, request_tty = false}",
            )?;
            std::fs::write(
                actual.join("another/module.lua"),
                b"return '/home/alice/.ssh/id_ed25519'",
            )?;
            std::fs::write(
                actual.join("cli.lua"),
                b"error('local module must not replace the built-in cli module')",
            )?;
            let known_hosts = std::fs::read("/home/alice/.ssh/known_hosts")?;
            std::fs::write(actual.join("known_hosts"), &known_hosts)?;
            let _hosts = t::KnownHosts::replace(b"")?;
            let client = t::Client::default_config()?;

            for (filename, alias) in [
                ("config.lua", "link.lua"),
                ("config/init.lua", "init-link.lua"),
            ] {
                let config = actual.join(filename);
                let link = dir.path().join(alias);
                std::fs::write(
                    &config,
                    format!(
                        r#"
local cli = require('cli')
assert(cli.__file__ == '{file}')
assert(cli.__dir__ == '{dir}')
assert(cli.__root__ == '{root}')
local target = require('config.some.module')
target.identity_file = require('another.module')
return {{
    meta = {{known_hosts = cli.__root__ .. '/known_hosts'}},
    hosts = {{target = target}},
}}
"#,
                        file = config.to_str().unwrap(),
                        dir = config.parent().unwrap().to_str().unwrap(),
                        root = actual.to_str().unwrap(),
                    ),
                )?;
                symlink(&config, &link)?;

                for path in [
                    config,
                    link,
                    Path::new("..").join(alias),
                    Path::new("../actual/nested/..").join(filename),
                ] {
                    let output = timeout(
                        Duration::from_secs(15),
                        client
                            .command(&[
                                "-F",
                                path.to_str().unwrap(),
                                "target",
                                "printf",
                                "adjacent",
                            ])
                            .current_dir(&work)
                            .stdin(Stdio::null())
                            .output(),
                    )
                    .await??;
                    t::expect(&output, 0, b"adjacent", b"");
                }
            }

            assert_eq!(std::fs::read(actual.join("known_hosts"))?, known_hosts);
            assert!(std::fs::read("/home/alice/.ssh/known_hosts")?.is_empty());

            Ok(())
        },
    );

    rt.register(
        "SSH_LUA_PACKAGE_PATH replaces paths and expands default search paths",
        async || {
            use std::process::Stdio;
            use t::tokio::time::{Duration, timeout};

            let dir = t::tempdir()?;
            let root = dir.path().join("config");
            let external_files = dir.path().join("external-files");
            let external_dirs = dir.path().join("external-directories");
            let work = dir.path().join("work");
            std::fs::create_dir(&root)?;
            std::fs::create_dir(&external_files)?;
            std::fs::create_dir_all(external_dirs.join("identity"))?;
            std::fs::create_dir(&work)?;
            std::fs::write(root.join("shared.lua"), "return 'root'")?;
            std::fs::write(root.join("local_only.lua"), "return 'local'")?;
            std::fs::write(external_files.join("shared.lua"), "return 'external'")?;
            std::fs::write(
                external_files.join("target.lua"),
                "return {host_name = '127.0.0.1', user = 'bob', port = 2222, request_tty = false}",
            )?;
            std::fs::write(
                external_dirs.join("identity/init.lua"),
                "return '/home/alice/.ssh/id_ed25519'",
            )?;
            let custom = format!(
                "{};{}",
                external_files.join("?.lua").display(),
                external_dirs.join("?/init.lua").display()
            );
            let client = t::Client::default_config()?;
            let config = root.join("config.lua");

            for (path, expected, local, external) in [
                (None, "root", true, false),
                (Some(custom.clone()), "external", false, true),
                (Some(format!("{custom};;")), "external", true, true),
                (Some(format!(";;{custom}")), "root", true, true),
                (Some(String::new()), "", false, false),
            ] {
                std::fs::write(
                    &config,
                    format!(
                        r#"
local cli = require('cli')
assert(type(cli.home_dir) == 'function')
if '{expected}' == '' then
    assert(package.path == '')
    assert(not pcall(require, 'shared'))
else
    assert(require('shared') == '{expected}')
end
if {local} then
    assert(require('local_only') == 'local')
else
    assert(not pcall(require, 'local_only'))
end
local target
if {external} then
    target = require('target')
    target.identity_file = require('identity')
else
    assert(not pcall(require, 'target'))
    target = {{host_name = '127.0.0.1', user = 'bob', port = 2222,
        identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false}}
end
return {{hosts = {{target = target}}}}
"#,
                    ),
                )?;
                let mut command = client.command(&[
                    "-F",
                    config.to_str().unwrap(),
                    "target",
                    "printf",
                    "modules",
                ]);
                command
                    .current_dir(&work)
                    .stdin(Stdio::null())
                    .env_remove("LUA_PATH")
                    .env_remove("LUA_PATH_5_4");
                if let Some(path) = path {
                    command.env("SSH_LUA_PACKAGE_PATH", path);
                } else {
                    command.env_remove("SSH_LUA_PACKAGE_PATH");
                }
                let output = timeout(Duration::from_secs(15), command.output()).await??;
                t::expect(&output, 0, b"modules", b"");
            }

            Ok(())
        },
    );

    rt.register("CLI options override Lua settings", async || {
        let client = t::Client::lua(
            r#"
return {
    hosts = {
        target = {
            host_name = '127.0.0.1', user = 'wrong', port = 1,
            identity_file = '/missing/identity', proxy_jump = 'invalid.invalid',
            request_tty = 'force',
        },
    },
}
"#,
        )?;
        let output = client
            .run(
                &[
                    "-p",
                    "2222",
                    "-l",
                    "bob",
                    "-i",
                    "/home/alice/.ssh/id_ed25519",
                    "-J",
                    "none",
                    "-T",
                    "target",
                    "if test -t 0; then exit 1; else printf overrides; fi",
                ],
                b"",
            )
            .await?;
        t::expect(&output, 0, b"overrides", b"");

        Ok(())
    });

    rt.register(
        "Lua hosts and jumps use exact case-sensitive names",
        async || {
            for jumps in ["'Jump'", "{'Jump'}"] {
                let client = t::Client::lua(&format!(
                    r#"
local home = os.getenv('HOME')
return {{
    default = {{
        host_name = '127.0.0.1', user = 'bob', port = 2222,
        identity_file = home .. '/.ssh/id_ed25519', request_tty = false,
    }},
    hosts = {{
        target = {{ proxy_jump = {jumps} }},
        Jump = {{}},
        jump = {{ port = 1 }},
        Target = {{ port = 1 }},
        ['t*'] = {{ port = 1 }},
    }},
}}
"#
                ))?;
                let output = client.run(&["target", "printf", "jumped"], b"").await?;
                t::expect(&output, 0, b"jumped", b"");

                for host in ["jump", "Target", "t*"] {
                    let output = client.run(&[host, "true"], b"").await?;
                    assert_eq!(output.status.code(), Some(255));
                }
            }

            Ok(())
        },
    );

    rt.register(
        "Lua inline jumps inherit defaults and bypass alias lookup",
        async || {
            for jump in ["{host_name = '127.0.0.1'}", "{user = 'bob'}"] {
                let client = t::Client::lua(&format!(
                    r#"
return {{
    default = {{
        host_name = '127.0.0.1', user = 'bob', port = 2222,
        identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
    }},
    hosts = {{
        target = {{proxy_jump = {jump}}},
        ['127.0.0.1'] = {{host_name = 'invalid.invalid', port = 1}},
    }},
}}
"#,
                ))?;
                let output = client.run(&["target", "printf", "inline"], b"").await?;
                t::expect(&output, 0, b"inline", b"");
            }

            Ok(())
        },
    );

    rt.register(
        "Lua inline and named jump chains traverse only the first entry",
        async || {
            for (jumps, success) in [
                (
                    "{{host_name = '127.0.0.1'}, {host_name = '127.0.0.1'}}",
                    true,
                ),
                (
                    "{{host_name = '127.0.0.1', proxy_jump = {host_name = '127.0.0.1'}}, 'Later'}",
                    true,
                ),
                (
                    "{'First', {host_name = '127.0.0.1', proxy_jump = 'Invalid'}}",
                    true,
                ),
                (
                    "{{host_name = '127.0.0.1', proxy_jump = 'Invalid'}, 'Later'}",
                    false,
                ),
                ("{'BrokenFirst', {host_name = '127.0.0.1'}}", false),
            ] {
                let client = t::Client::lua(&format!(
                    r#"
return {{
    default = {{
        host_name = '127.0.0.1', user = 'bob', port = 2222,
        identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
    }},
    hosts = {{
        target = {{proxy_jump = {jumps}}},
        First = {{proxy_jump = {{host_name = '127.0.0.1'}}}},
        BrokenFirst = {{proxy_jump = 'Invalid'}},
        Later = {{proxy_jump = 'Invalid'}},
        Invalid = {{port = 1}},
    }},
}}
"#,
                ))?;
                let output = client.run(&["target", "printf", "chained"], b"").await?;

                if success {
                    t::expect(&output, 0, b"chained", b"");
                } else {
                    assert_eq!(output.status.code(), Some(255));
                    assert!(output.stdout.is_empty());
                }
            }

            Ok(())
        },
    );

    rt.register(
        "Lua inline jumps use their own connection settings",
        async || {
            for (user, port, identity, success) in [
                ("bob", 2222, "/home/alice/.ssh/id_ed25519", true),
                ("wrong", 2222, "/home/alice/.ssh/id_ed25519", false),
                ("bob", 1, "/home/alice/.ssh/id_ed25519", false),
                ("bob", 2222, "/missing/identity", false),
            ] {
                let client = t::Client::lua(&format!(
                    r#"
return {{
    default = {{user = 'wrong', port = 1, request_tty = false}},
    hosts = {{
        target = {{
            host_name = '127.0.0.1', user = 'bob', port = 2222,
            identity_file = '/home/alice/.ssh/id_ed25519',
            proxy_jump = {{
                host_name = '127.0.0.1', user = '{user}', port = {port},
                identity_file = '{identity}',
            }},
        }},
    }},
}}
"#,
                ))?;
                let output = client
                    .run(&["target", "printf", "independent"], b"")
                    .await?;

                if success {
                    t::expect(&output, 0, b"independent", b"");
                } else {
                    assert_eq!(output.status.code(), Some(255));
                    assert!(output.stdout.is_empty());
                }
            }

            Ok(())
        },
    );

    rt.register("-J overrides Lua inline jump chains", async || {
        let client = t::Client::lua(
            r#"
return {
    default = {
        host_name = '127.0.0.1', user = 'bob', port = 2222,
        identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
    },
    hosts = {
        target = {proxy_jump = {{host_name = '127.0.0.1', port = 1}, 'Invalid'}},
        Jump = {},
        Invalid = {port = 1},
    },
}
"#,
        )?;

        for jump in ["none", "Jump"] {
            let output = client
                .run(&["-J", jump, "target", "printf", "overridden"], b"")
                .await?;
            t::expect(&output, 0, b"overridden", b"");
        }

        Ok(())
    });

    rt.register(
        "Lua requires explicit agent environment access",
        async || {
            use std::path::Path;
            use std::process::{Output, Stdio};
            use t::tokio::time::{Duration, timeout};

            async fn run(client: &t::Client, args: &[&str], socket: &Path) -> t::Result<Output> {
                let output = timeout(
                    Duration::from_secs(15),
                    client
                        .command(args)
                        .env("SSH_AUTH_SOCK", socket)
                        .stdin(Stdio::null())
                        .output(),
                )
                .await??;

                Ok(output)
            }

            let client = t::Client::lua(
                r#"
return {
    default = {
        host_name = '127.0.0.1', user = 'bob', port = 2222,
        identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
    },
    hosts = {
        target = {}, Jump = {},
        Inline = {proxy_jump = {host_name = '127.0.0.1'}},
    },
}
"#,
            )?;
            let dir = t::tempdir()?;
            let missing = dir.path().join("missing-agent.sock");

            for socket in [Path::new("relative-agent.sock"), missing.as_path()] {
                for args in [
                    &["target", "printf", "local-key"][..],
                    &["-J", "Jump", "target", "printf", "local-key"][..],
                    &["Inline", "printf", "local-key"][..],
                ] {
                    let output = run(&client, args, socket).await?;
                    t::expect(&output, 0, b"local-key", b"");
                }
            }

            let client = t::Client::lua(
                "return {hosts = {target = {identity_agent = os.getenv('SSH_AUTH_SOCK')}}}",
            )?;
            let output = run(
                &client,
                &["target", "true"],
                Path::new("relative-agent.sock"),
            )
            .await?;

            assert_eq!(output.status.code(), Some(255));
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains("identity_agent must be an absolute path: relative-agent.sock")
            );

            Ok(())
        },
    );

    rt.register("native config terminal booleans and CLI overrides", async || {
        for (tty, expected) in [("true", b"pipe".as_slice()), ("false", b"pipe".as_slice()), ("'force'", b"tty".as_slice())] {
            let toml = t::Client::toml(&format!(
                "[hosts.target]\nhost-name = '127.0.0.1'\nuser = 'bob'\nport = '2222'\nidentity-file = '~/.ssh/id_ed25519'\nrequest-tty = {tty}\n",
            ))?;
            let lua = t::Client::lua(&format!(
                "return {{hosts = {{target = {{host_name = '127.0.0.1', user = 'bob', port = '2222', identity_file = os.getenv('HOME') .. '/.ssh/id_ed25519', request_tty = {tty}}}}}}}",
            ))?;

            for client in [&toml, &lua] {
                let command = "if test -t 0; then printf tty; else printf pipe; fi";
                let output = client.run(&["target", command], b"").await?;
                t::expect(&output, 0, expected, b"");

                let output = client.run(&["-tt", "target", command], b"").await?;
                t::expect(&output, 0, b"tty", b"");
            }
        }

        Ok(())
    });

    rt.register(
        "native algorithm lists inherit defaults and replace host settings",
        async || {
            for (toml_kex, lua_kex, toml_keys, lua_keys) in [
                (
                    "'diffie-hellman-group1-sha1,curve25519-sha256'",
                    "'diffie-hellman-group1-sha1,curve25519-sha256'",
                    "'rsa-sha2-512,ssh-ed25519'",
                    "'rsa-sha2-512,ssh-ed25519'",
                ),
                (
                    "['diffie-hellman-group1-sha1', 'curve25519-sha256']",
                    "{'diffie-hellman-group1-sha1', 'curve25519-sha256'}",
                    "['rsa-sha2-512', 'ssh-ed25519']",
                    "{'rsa-sha2-512', 'ssh-ed25519'}",
                ),
            ] {
                for inherit in [true, false] {
                    let (toml_default, toml_host, lua_default, lua_host) = if inherit {
                        (
                            format!("kex-algorithms = {toml_kex}\nhost-key-algorithms = {toml_keys}"),
                            String::new(),
                            format!("kex_algorithms = {lua_kex}, host_key_algorithms = {lua_keys},"),
                            String::new(),
                        )
                    } else {
                        (
                            "kex-algorithms = 'diffie-hellman-group1-sha1'\nhost-key-algorithms = 'rsa-sha2-512'".to_owned(),
                            format!("kex-algorithms = {toml_kex}\nhost-key-algorithms = {toml_keys}"),
                            "kex_algorithms = 'diffie-hellman-group1-sha1', host_key_algorithms = 'rsa-sha2-512',".to_owned(),
                            format!("kex_algorithms = {lua_kex}, host_key_algorithms = {lua_keys},"),
                        )
                    };
                    let toml = t::Client::toml(&format!(
                        "[default]\nhost-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
                         identity-file = '~/.ssh/id_ed25519'\nrequest-tty = false\n\
                         {toml_default}\n[hosts.target]\n{toml_host}\n",
                    ))?;
                    let lua = t::Client::lua(&format!(
                        "return {{default = {{host_name = '127.0.0.1', user = 'bob', port = 2222, \
                         identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false, \
                         {lua_default}}}, hosts = {{target = {{{lua_host}}}}}}}",
                    ))?;

                    for client in [&toml, &lua] {
                        let output = client.run(&["target", "printf", "algorithms"], b"").await?;
                        t::expect(&output, 0, b"algorithms", b"");
                    }
                }
            }

            Ok(())
        },
    );

    rt.register(
        "native host algorithms restrict negotiation despite trusted keys",
        async || {
            for (toml_override, lua_override) in [
                (
                    "kex-algorithms = 'diffie-hellman-group1-sha1'",
                    "kex_algorithms = 'diffie-hellman-group1-sha1'",
                ),
                (
                    "host-key-algorithms = ['rsa-sha2-512']",
                    "host_key_algorithms = {'rsa-sha2-512'}",
                ),
            ] {
                let toml = t::Client::toml(&format!(
                    "[default]\nhost-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
                     identity-file = '~/.ssh/id_ed25519'\nrequest-tty = false\n\
                     kex-algorithms = 'curve25519-sha256'\nhost-key-algorithms = 'ssh-ed25519'\n\
                     [hosts.target]\n{toml_override}\n",
                ))?;
                let lua = t::Client::lua(&format!(
                    "return {{default = {{host_name = '127.0.0.1', user = 'bob', port = 2222, \
                     identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false, \
                     kex_algorithms = 'curve25519-sha256', host_key_algorithms = 'ssh-ed25519'}}, \
                     hosts = {{target = {{{lua_override}}}}}}}",
                ))?;

                for client in [&toml, &lua] {
                    let output = client
                        .run(&["target", "printf", "unreachable"], b"")
                        .await?;
                    assert_eq!(output.status.code(), Some(255));
                    assert!(output.stdout.is_empty());
                    assert!(!output.stderr.is_empty());
                }
            }

            Ok(())
        },
    );

    rt.register("native algorithms apply to individual jump hosts", async || {
        for (kex, keys, success) in [
            ("curve25519-sha256", "ssh-ed25519", true),
            ("diffie-hellman-group1-sha1", "ssh-ed25519", false),
            ("curve25519-sha256", "rsa-sha2-512", false),
        ] {
            let toml = t::Client::toml(&format!(
                "[default]\nhost-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
                 identity-file = '~/.ssh/id_ed25519'\nrequest-tty = false\n\
                 kex-algorithms = 'diffie-hellman-group1-sha1'\nhost-key-algorithms = 'rsa-sha2-512'\n\
                 [hosts.target]\nproxy-jump = 'Jump'\nkex-algorithms = 'curve25519-sha256'\n\
                 host-key-algorithms = 'ssh-ed25519'\n[hosts.Jump]\n\
                 kex-algorithms = '{kex}'\nhost-key-algorithms = '{keys}'\n",
            ))?;
            let lua = t::Client::lua(&format!(
                "return {{default = {{host_name = '127.0.0.1', user = 'bob', port = 2222, \
                 identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false, \
                 kex_algorithms = 'diffie-hellman-group1-sha1', host_key_algorithms = 'rsa-sha2-512'}}, \
                 hosts = {{target = {{kex_algorithms = 'curve25519-sha256', host_key_algorithms = 'ssh-ed25519', \
                 proxy_jump = {{host_name = '127.0.0.1', kex_algorithms = '{kex}', host_key_algorithms = '{keys}'}}}}}}}}",
            ))?;

            for client in [&toml, &lua] {
                let output = client.run(&["target", "printf", "jumped"], b"").await?;
                if success {
                    t::expect(&output, 0, b"jumped", b"");
                } else {
                    assert_eq!(output.status.code(), Some(255));
                    assert!(output.stdout.is_empty());
                }
            }
        }

        Ok(())
    });

    rt.register("OpenSSH transport directives remain ignored", async || {
        let client = t::Client::new(
            "KexAlgorithms diffie-hellman-group1-sha1\n\
             HostKeyAlgorithms rsa-sha2-512\nUserKnownHostsFile /missing/known_hosts",
        )?;
        let output = client.run(&["target", "printf", "ignored"], b"").await?;
        t::expect(&output, 0, b"ignored", b"");

        Ok(())
    });

    rt.register(
        "native known-hosts metadata selects the loaded database",
        async || {
            let dir = t::tempdir()?;
            let selected = dir.path().join("known_hosts");
            let original = std::fs::read("/home/alice/.ssh/known_hosts")?;
            std::fs::write(&selected, &original)?;
            let path = selected.to_str().unwrap();
            let toml = t::Client::toml(&format!(
                "[meta]\nknown-hosts = '{path}'\n[hosts.target]\n\
             host-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
             identity-file = '~/.ssh/id_ed25519'\nrequest-tty = false\n",
            ))?;
            let lua = t::Client::lua(&format!(
                "return {{meta = {{known_hosts = '{path}'}}, hosts = {{target = {{\
             host_name = '127.0.0.1', user = 'bob', port = 2222, \
             identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false}}}}}}",
            ))?;
            let _hosts = t::KnownHosts::replace(b"")?;

            for client in [&toml, &lua] {
                let output = client.run(&["target", "printf", "selected"], b"").await?;
                t::expect(&output, 0, b"selected", b"");
            }
            assert_eq!(std::fs::read(&selected)?, original);
            assert!(std::fs::read("/home/alice/.ssh/known_hosts")?.is_empty());

            Ok(())
        },
    );

    rt.register(
        "native known-hosts metadata persists accepted keys",
        async || {
            let dir = t::tempdir()?;
            let original = std::fs::read("/home/alice/.ssh/known_hosts")?;

            for format in ["toml", "lua"] {
                let selected = dir.path().join(format).join("known_hosts");
                let path = selected.to_str().unwrap();
                let client = if format == "toml" {
                    t::Client::toml(&format!(
                        "[meta]\nknown-hosts = '{path}'\n[hosts.target]\n\
                     host-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
                     identity-file = '~/.ssh/id_ed25519'\nrequest-tty = false\n",
                    ))?
                } else {
                    t::Client::lua(&format!(
                        "return {{meta = {{known_hosts = '{path}'}}, hosts = {{target = {{\
                     host_name = '127.0.0.1', user = 'bob', port = 2222, \
                     identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false}}}}}}",
                    ))?
                };

                let mut pty = client.pty(&["target", "true"])?;
                pty.read_until(b"(yes/no/[fingerprint])?").await?;
                pty.write(b"no\n").await?;
                pty.wait(255).await?;
                assert!(!selected.exists());

                let mut pty = client.pty(&["target", "true"])?;
                pty.read_until(b"(yes/no/[fingerprint])?").await?;
                pty.write(b"yes\n").await?;
                pty.wait(0).await?;
                assert!(!std::fs::read(&selected)?.is_empty());
                assert_eq!(std::fs::read("/home/alice/.ssh/known_hosts")?, original);

                let output = client.run(&["target", "printf", "remembered"], b"").await?;
                t::expect(&output, 0, b"remembered", b"");
            }

            Ok(())
        },
    );

    rt.register(
        "native known-hosts metadata uses format-specific path handling",
        async || {
            let toml = t::Client::toml(
                "[meta]\nknown-hosts = '~/.ssh/known_hosts'\n[hosts.target]\n\
                 host-name = '127.0.0.1'\nuser = 'bob'\nport = 2222\n\
                 identity-file = '~/.ssh/id_ed25519'\nrequest-tty = false\n",
            )?;
            let lua = t::Client::lua(
                "return {meta = {known_hosts = os.getenv('HOME') .. '/.ssh/known_hosts'}, \
                 hosts = {target = {host_name = '127.0.0.1', user = 'bob', port = 2222, \
                 identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false}}}",
            )?;

            for client in [&toml, &lua] {
                let output = client.run(&["target", "printf", "expanded"], b"").await?;
                t::expect(&output, 0, b"expanded", b"");
            }

            for path in ["relative/hosts", "../hosts", ""] {
                let client = t::Client::toml(&format!("[meta]\nknown-hosts = '{path}'\n"))?;
                let output = client.run(&["target", "true"], b"").await?;
                assert_eq!(output.status.code(), Some(255));
                assert!(String::from_utf8_lossy(&output.stderr).contains("absolute"));
            }

            for path in [
                "relative/hosts",
                "../hosts",
                "~/.ssh/known_hosts",
                "$HOME/.ssh/known_hosts",
                "",
            ] {
                let client =
                    t::Client::lua(&format!("return {{meta = {{known_hosts = '{path}'}}}}",))?;
                let output = client.run(&["target", "true"], b"").await?;
                assert_eq!(output.status.code(), Some(255));
                assert!(String::from_utf8_lossy(&output.stderr).contains("absolute"));
            }

            Ok(())
        },
    );

    rt.register(
        "native config paths must resolve to absolute paths",
        async || {
            for field in ["identity-file", "certificate-file", "identity-agent"] {
                for path in ["relative/key", "../key", ""] {
                    let client = t::Client::toml(&format!("[hosts.target]\n{field} = '{path}'\n"))?;
                    let output = client.run(&["target", "true"], b"").await?;

                    assert_eq!(output.status.code(), Some(255));
                    assert!(String::from_utf8_lossy(&output.stderr).contains("absolute"));
                }
            }

            for field in ["identity_file", "certificate_file", "identity_agent"] {
                for path in ["relative/key", "../key", "~/.ssh/key", ""] {
                    let client = t::Client::lua(&format!(
                        "return {{hosts = {{target = {{{field} = '{path}'}}}}}}",
                    ))?;
                    let output = client.run(&["target", "true"], b"").await?;

                    assert_eq!(output.status.code(), Some(255));
                    assert!(String::from_utf8_lossy(&output.stderr).contains("absolute"));
                }
            }

            for path in ["$SSH_AUTH_SOCK", "SSH_AUTH_SOCK"] {
                let client = t::Client::lua(&format!(
                    "return {{hosts = {{target = {{identity_agent = '{path}'}}}}}}",
                ))?;
                let output = client.run(&["target", "true"], b"").await?;

                assert_eq!(output.status.code(), Some(255));
                assert!(String::from_utf8_lossy(&output.stderr).contains("absolute"));
            }

            Ok(())
        },
    );

    rt.register("invalid Lua inline jumps report the selected file", async || {
        for (jump, expected) in [
            ("true", "jump"),
            ("{true}", "jump"),
            ("{host_name = true}", "string"),
            ("{host_name = '127.0.0.1', port = false}", "port"),
            (
                "{host_name = '127.0.0.1', proxy_jump = {{host_name = '127.0.0.1', unexpected = true}}}",
                "unknown field",
            ),
            ("{user = 'bob'}", "requires host_name"),
        ] {
            let client = t::Client::lua(&format!(
                "return {{hosts = {{target = {{host_name = '127.0.0.1', proxy_jump = {jump}}}}}}}",
            ))?;
            let output = client.run(&["target", "true"], b"").await?;
            let stderr = String::from_utf8_lossy(&output.stderr);

            assert_eq!(output.status.code(), Some(255));
            assert!(stderr.contains(".lua"), "{stderr}");
            assert!(stderr.contains(expected), "{stderr}");
        }

        for field in ["identity_file", "certificate_file", "identity_agent"] {
            for path in ["relative/key", "../key", "~/.ssh/key", ""] {
                let client = t::Client::lua(&format!(
                    r#"
return {{
    hosts = {{
        target = {{
            host_name = '127.0.0.1', user = 'bob', port = 2222,
            identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false,
        }},
        Unselected = {{
            proxy_jump = {{
                host_name = '127.0.0.1',
                proxy_jump = {{{{host_name = '127.0.0.1', {field} = '{path}'}}}},
            }},
        }},
    }},
}}
"#,
                ))?;
                let output = client.run(&["-J", "none", "target", "true"], b"").await?;
                let stderr = String::from_utf8_lossy(&output.stderr);

                assert_eq!(output.status.code(), Some(255));
                assert!(stderr.contains(".lua"), "{stderr}");
                assert!(stderr.contains("absolute"), "{stderr}");
                assert!(stderr.contains(field), "{stderr}");
            }
        }

        Ok(())
    });

    rt.register("invalid Lua configs report the selected file", async || {
        for text in [
            "return {",
            "error('config failure')",
            "return 3",
            "return 'config'",
            "return function() end",
        ] {
            let client = t::Client::lua(text)?;
            let output = client.run(&["target", "true"], b"").await?;

            assert_eq!(output.status.code(), Some(255));
            assert!(String::from_utf8_lossy(&output.stderr).contains(".lua"));
        }

        Ok(())
    });

    rt.register("default config discovery and explicit -F precedence", async || {
        let openssh = FixtureConfig::new("config")?;
        let toml = FixtureConfig::new("config.toml")?;
        let lua = FixtureConfig::new("config.lua")?;
        openssh.write(
            b"Host target\n HostName 127.0.0.1\n User bob\n Port 2222\n IdentityFile ~/.ssh/id_ed25519\n RequestTTY force\n",
        )?;
        toml.write(
            br#"
[hosts.target]
host-name = "127.0.0.1"
user = "bob"
port = 2222
identity-file = "~/.ssh/id_ed25519"
request-tty = "no"
"#,
        )?;

        let client = t::Client::default_config()?;
        let command = [
            "target",
            "if test -t 0; then printf openssh; else printf toml; fi",
        ];
        lua.write(br#"
local home = os.getenv('HOME')
return {
    hosts = {
        target = {
            host_name = '127.0.0.1', user = 'bob', port = 2222,
            identity_file = home .. '/.ssh/id_ed25519', request_tty = 'force',
        },
    },
}
"#)?;
        let output = client.run(&[
            "target",
            "if test -t 0; then printf lua; else printf toml; fi",
        ], b"").await?;
        t::expect(&output, 0, b"lua", b"");

        lua.write(b"error('selected Lua config')")?;

        let output = client.run(&["target", "true"], b"").await?;
        assert_eq!(output.status.code(), Some(255));
        assert!(String::from_utf8_lossy(&output.stderr).contains("config.lua"));

        let output = client.run(&["-F", toml.path.to_str().unwrap(), "target", "printf", "toml"], b"").await?;
        t::expect(&output, 0, b"toml", b"");

        lua.write(b"return {")?;

        let output = client.run(&["target", "true"], b"").await?;
        assert_eq!(output.status.code(), Some(255));
        assert!(String::from_utf8_lossy(&output.stderr).contains("config.lua"));

        let explicit = t::Client::new("")?;
        let output = explicit.run(&["target", "printf", "explicit"], b"").await?;
        t::expect(&output, 0, b"explicit", b"");

        let output = client.run(&[
            "-F", "none", "-p", "2222", "-l", "bob",
            "-i", "/home/alice/.ssh/id_ed25519", "127.0.0.1", "printf", "none",
        ], b"").await?;
        t::expect(&output, 0, b"none", b"");

        lua.remove()?;

        let output = client.run(&command, b"").await?;
        t::expect(&output, 0, b"toml", b"");

        toml.remove()?;

        let output = client.run(&command, b"").await?;
        t::expect(&output, 0, b"openssh", b"");

        toml.write(b"[hosts.target\n")?;

        let output = client.run(&["target", "true"], b"").await?;
        assert_eq!(output.status.code(), Some(255));
        assert!(String::from_utf8_lossy(&output.stderr).contains("config.toml"));

        let explicit = t::Client::new("")?;
        let output = explicit.run(&["target", "printf", "explicit"], b"").await?;
        t::expect(&output, 0, b"explicit", b"");

        let output = client.run(&[
            "-F", "none", "-p", "2222", "-l", "bob",
            "-i", "/home/alice/.ssh/id_ed25519", "127.0.0.1", "printf", "none",
        ], b"").await?;
        t::expect(&output, 0, b"none", b"");

        Ok(())
    });

    rt.run();
}

fn logging_clients(level: Option<&str>) -> t::Result<[t::Client; 2]> {
    let toml_meta = level.map_or_else(String::new, |level| format!("log-level = '{level}'"));
    let lua_level = level.map_or_else(|| "nil".to_owned(), |level| format!("'{level}'"));

    Ok([
        t::Client::toml(&format!(
            "[meta]\n{toml_meta}\n[hosts.target]\nhost-name = '127.0.0.1'\n\
             user = 'bob'\nport = 2222\nidentity-file = '~/.ssh/id_ed25519'\n\
             request-tty = false\n"
        ))?,
        t::Client::lua(&format!(
            "return {{meta = {{log_level = {lua_level}}}, hosts = {{target = {{\
             host_name = '127.0.0.1', user = 'bob', port = 2222, \
             identity_file = '/home/alice/.ssh/id_ed25519', request_tty = false}}}}}}"
        ))?,
    ])
}

struct FixtureConfig {
    path: std::path::PathBuf,
    original: Option<Vec<u8>>,
}

impl FixtureConfig {
    fn new(name: &str) -> t::Result<Self> {
        assert!(matches!(name, "config" | "config.toml" | "config.lua"));

        let home = std::env::home_dir().expect("Find fixture home");
        assert_eq!(home, std::path::Path::new("/home/alice"));

        let path = home.join(".ssh").join(name);
        let original = match std::fs::read(&path) {
            Ok(data) => Some(data),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };

        Ok(Self { path, original })
    }

    fn write(&self, text: &[u8]) -> t::Result<()> {
        std::fs::write(&self.path, text)?;

        Ok(())
    }

    fn remove(&self) -> t::Result<()> {
        std::fs::remove_file(&self.path)?;

        Ok(())
    }
}

impl Drop for FixtureConfig {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            std::fs::write(&self.path, original).expect("Restore fixture config");
        } else if let Err(e) = std::fs::remove_file(&self.path) {
            assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
        }
    }
}
