fn main() {
    let mut rt = t::runtime();

    rt.register("unknown host confirmation and persistence", async || {
        let client = t::Client::new("")?;
        let _hosts = t::KnownHosts::replace(b"")?;

        let mut pty = client.pty(&["target", "true"])?;
        pty.read_until(b"(yes/no/[fingerprint])?").await?;
        pty.write(b"no\n").await?;
        pty.wait(255).await?;
        assert!(std::fs::read("/home/alice/.ssh/known_hosts")?.is_empty());

        let mut pty = client.pty(&["target", "true"])?;
        pty.read_until(b"(yes/no/[fingerprint])?").await?;
        pty.write(b"yes\n").await?;
        pty.wait(0).await?;
        assert!(!std::fs::read("/home/alice/.ssh/known_hosts")?.is_empty());

        let output = client.run(&["target", "printf", "remembered"], b"").await?;
        t::expect(&output, 0, b"remembered", b"");

        Ok(())
    });

    rt.run();
}
