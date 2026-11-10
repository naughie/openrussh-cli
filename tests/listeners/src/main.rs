use t::tokio;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::time::{Duration, sleep, timeout};

fn main() {
    let mut rt = t::runtime();

    rt.register(
        "-L endpoint selection, -N lifetime, -W stdio and socket cleanup",
        async || {
            let client = t::Client::new("")?;
            use std::os::unix::fs::PermissionsExt as _;

            let dir = t::tempdir()?;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))?;

            let tcp = TcpListener::bind("127.0.0.1:0").await?;
            let remote = tcp.local_addr()?;
            let remote_path = dir.path().join("remote.sock");
            let unix = UnixListener::bind(&remote_path)?;
            std::fs::set_permissions(&remote_path, std::fs::Permissions::from_mode(0o777))?;

            let tcp_task = tokio::spawn(async move {
                for _ in 0..2 {
                    let (mut stream, _) = tcp.accept().await?;
                    let mut data = Vec::new();
                    stream.read_to_end(&mut data).await?;
                    stream.write_all(&data).await?;
                }
                std::io::Result::Ok(())
            });

            let unix_task = tokio::spawn(async move {
                let (mut stream, _) = unix.accept().await?;
                let mut data = Vec::new();
                stream.read_to_end(&mut data).await?;
                stream.write_all(&data).await?;

                std::io::Result::Ok(())
            });

            for unix in [false, true] {
                let path = dir.path().join("local.sock");
                let reserve = TcpListener::bind("127.0.0.1:0").await?;
                let addr = reserve.local_addr()?;
                drop(reserve);

                let bind = if unix {
                    path.display().to_string()
                } else {
                    addr.to_string()
                };

                let target = if unix {
                    remote_path.display().to_string()
                } else {
                    remote.to_string()
                };

                let spec = format!("{bind}:{target}");
                let child = client.command(&["-N", "-L", &spec, "target"]).spawn()?;

                let transfer = async {
                    if unix {
                        let mut stream = loop {
                            match UnixStream::connect(&path).await {
                                Ok(stream) => break stream,
                                Err(_) => sleep(Duration::from_millis(20)).await,
                            }
                        };
                        stream.write_all(b"payload").await?;
                        stream.shutdown().await?;

                        let mut data = Vec::new();
                        stream.read_to_end(&mut data).await?;

                        assert_eq!(data, b"payload");
                    } else {
                        let mut stream = loop {
                            match TcpStream::connect(addr).await {
                                Ok(stream) => break stream,
                                Err(_) => sleep(Duration::from_millis(20)).await,
                            }
                        };
                        stream.write_all(b"payload").await?;
                        stream.shutdown().await?;

                        let mut data = Vec::new();
                        stream.read_to_end(&mut data).await?;

                        assert_eq!(data, b"payload");
                    }
                    std::io::Result::Ok(())
                };

                timeout(Duration::from_secs(10), transfer).await??;
                t::terminate(child).await?;

                assert!(!path.exists());
            }

            let output = client
                .run(&["-W", &remote.to_string(), "target"], b"stdio")
                .await?;
            t::expect(&output, 0, b"stdio", b"");

            timeout(Duration::from_secs(5), tcp_task).await???;
            timeout(Duration::from_secs(5), unix_task).await???;

            Ok(())
        },
    );

    rt.register("-L failure rolls back earlier CLI listeners", async || {
        let client = t::Client::new("")?;
        let dir = t::tempdir()?;
        let path = dir.path().join("local.sock");
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let occupied = listener.local_addr()?;
        let first = format!("{}:/tmp/remote.sock", path.display());
        let second = format!("{occupied}:127.0.0.1:2222");

        let output = client
            .run(&["-N", "-L", &first, "-L", &second, "target"], b"")
            .await?;
        assert_eq!(output.status.code(), Some(255));

        assert!(!path.exists());

        Ok(())
    });

    rt.run();
}
