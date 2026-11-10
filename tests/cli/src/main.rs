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
            let client = t::Client::settings("Port 1\nUser wrong\nIdentityFile /missing/identity")?;
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

    rt.run();
}
