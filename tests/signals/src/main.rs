use t::Signal;
use t::tokio;
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;
use tokio::time::{Duration, sleep, timeout};

fn main() {
    let mut rt = t::runtime();

    for sig in [
        Signal::SIGHUP,
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTERM,
    ] {
        rt.register(
            &format!("{sig}: remote command and terminal restoration"),
            async move || {
                let client = t::Client::new("")?;

                for flag in ["-T", "-tt"] {
                    let mut pty = client.pty(&[flag, "target", "sh", "-c", "printf ready; cat"])?;
                    pty.read_until(b"ready").await?;
                    pty.signal(sig)?;
                    pty.wait(255).await?;
                }

                Ok(())
            },
        );

        rt.register(
            &format!("{sig}: interactive shell and terminal restoration"),
            async move || {
                let client = t::Client::new("")?;
                let mut pty = client.pty(&["-tt", "target"])?;
                pty.write(b"printf 'shell-%s' ready\n").await?;
                pty.read_until(b"shell-ready").await?;
                pty.signal(sig)?;
                pty.wait(255).await?;

                Ok(())
            },
        );

        rt.register(
            &format!("{sig}: -N exit status and listener cleanup"),
            async move || {
                let client = t::Client::new("")?;
                let dir = t::tempdir()?;
                let path = dir.path().join("local.sock");
                let spec = format!("{}:/tmp/remote.sock", path.display());
                let pty = client.pty(&["-N", "-L", &spec, "target"])?;

                timeout(Duration::from_secs(15), async {
                    while !path.exists() {
                        sleep(Duration::from_millis(10)).await;
                    }
                })
                .await?;

                pty.signal(sig)?;
                pty.wait(if sig == Signal::SIGTERM { 0 } else { 255 })
                    .await?;
                assert!(!path.exists());

                Ok(())
            },
        );

        rt.register(&format!("{sig}: -W exit status"), async move || {
            let client = t::Client::new("")?;
            let listener = TcpListener::bind("127.0.0.1:0").await?;
            let addr = listener.local_addr()?;
            let mut pty = client.pty(&["-W", &addr.to_string(), "target"])?;
            let (mut stream, _) = timeout(Duration::from_secs(15), listener.accept()).await??;
            stream.write_all(b"ready").await?;
            pty.read_until(b"ready").await?;
            pty.signal(sig)?;
            pty.wait(if sig == Signal::SIGTERM { 0 } else { 255 })
                .await?;

            Ok(())
        });

        rt.register(
            &format!("{sig}: cancel host-key confirmation"),
            async move || {
                let client = t::Client::new("")?;
                let _hosts = t::KnownHosts::replace(b"")?;
                let mut pty = client.pty(&["target", "true"])?;
                pty.read_until(b"(yes/no/[fingerprint])?").await?;
                pty.signal(sig)?;
                pty.wait(255).await?;
                assert!(std::fs::read("/home/alice/.ssh/known_hosts")?.is_empty());

                Ok(())
            },
        );
    }

    rt.register(
        "SIGTSTP and SIGCONT restore and resume the terminal",
        async || {
            let client = t::Client::new("")?;
            let mut pty = client.pty(&[
                "-tt",
                "target",
                "sh",
                "-c",
                "stty -echo; printf ready; IFS= read -r line; printf '<%s>' \"$line\"; cat",
            ])?;
            pty.read_until(b"ready").await?;
            pty.wait_raw().await?;
            pty.signal(Signal::SIGTSTP)?;
            pty.wait_stopped().await?;
            pty.assert_restored()?;
            pty.signal(Signal::SIGCONT)?;
            pty.wait_raw().await?;
            pty.write(b"resumed\n").await?;
            pty.read_until(b"<resumed>").await?;
            pty.write(b"~.").await?;
            pty.wait(255).await?;

            Ok(())
        },
    );

    rt.run();
}
