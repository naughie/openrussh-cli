fn main() {
    let mut rt = t::runtime();

    rt.register("CLI help and unsupported options", async || {
        let client = t::Client::new("")?;
        let output = client.run(&["--help"], b"").await?;
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: ssh"));

        for args in [
            &["-o", "BatchMode=yes", "target"][..],
            &["-v", "target", "true"][..],
            &["-s", "target", "sftp"][..],
        ] {
            let output = client.run(args, b"").await?;
            assert_eq!(output.status.code(), Some(2));
            assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
        }

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
                &[
                    "target",
                    "sh",
                    "-c",
                    "if test -t 0; then exit 1; else printf toml; fi",
                ],
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
                    "sh",
                    "-c",
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
                    &[
                        "target",
                        "sh",
                        "-c",
                        "if test -t 0; then exit 1; else printf lua; fi",
                    ],
                    b"",
                )
                .await?;
            t::expect(&output, 0, b"lua", b"");
        }

        Ok(())
    });

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
                    "sh",
                    "-c",
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
                let output = client.run(&["target", "sh", "-c", command], b"").await?;
                t::expect(&output, 0, expected, b"");

                let output = client.run(&["-tt", "target", "sh", "-c", command], b"").await?;
                t::expect(&output, 0, b"tty", b"");
            }
        }

        Ok(())
    });

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
            "sh",
            "-c",
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
            "target", "sh", "-c",
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
