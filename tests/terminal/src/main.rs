fn main() {
    let mut rt = t::runtime();

    rt.register("PTY resize and signal restoration", async || {
        let client = t::Client::new("")?;
        let mut pty = client.pty(&[
            "-tt",
            "target",
            "sh",
            "-c",
            "printf 'ready\\n'; trap 'stty size' WINCH; while :; do sleep 1; done",
        ])?;
        pty.read_until(b"ready").await?;
        pty.resize(100, 42)?;
        pty.read_until(b"42 100").await?;
        pty.signal(t::Signal::SIGTERM)?;
        pty.wait(255).await?;

        let mut pty = client.pty(&["-tt", "target", "sh", "-c", "printf ready; cat"])?;
        pty.read_until(b"ready").await?;
        pty.write(b"~\x1a").await?;
        pty.wait_stopped().await?;
        pty.assert_restored()?;
        pty.signal(t::Signal::SIGCONT)?;
        pty.wait_raw().await?;
        pty.write(b"~.").await?;
        pty.wait(255).await?;

        Ok(())
    });

    rt.register(
        "CLI terminal flags override config with piped stdin",
        async || {
            let client = t::Client::new("")?;

            for (flag, expected) in [
                ("-T", b"pipe".as_slice()),
                ("-t", b"pipe".as_slice()),
                ("-tt", b"tty".as_slice()),
            ] {
                let output = client
                    .run(
                        &[
                            flag,
                            "target",
                            "sh",
                            "-c",
                            "if test -t 0; then printf tty; else printf pipe; fi",
                        ],
                        b"",
                    )
                    .await?;
                t::expect(&output, 0, expected, b"");
            }

            Ok(())
        },
    );

    rt.run();
}
