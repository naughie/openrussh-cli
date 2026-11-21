use t::tokio;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::task::JoinSet;
use tokio::time::{Duration, sleep, timeout};

use std::io::{self, ErrorKind};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

pub(super) async fn check() -> t::Result<()> {
    let dir = t::tempdir()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))?;

    // A relay lets the test drop one SSH connection and count new transports.
    let transport = TcpListener::bind("127.0.0.1:0").await?;
    let port = transport.local_addr()?.port();
    let known_hosts = dir.path().join("known_hosts");
    let key = std::fs::read_to_string("/etc/ssh/ssh_host_ed25519_key.pub")?;
    std::fs::write(&known_hosts, format!("[127.0.0.1]:{port} {key}"))?;
    let client = t::Client::toml(&format!(
        r#"[meta]
known-hosts = '{}'
[hosts.target]
host-name = '127.0.0.1'
port = {port}
user = 'bob'
identity-file = '~/.ssh/id_ed25519'
"#,
        known_hosts.display(),
    ))?;

    let reserve = TcpListener::bind("127.0.0.1:0").await?;
    let local_tcp = reserve.local_addr()?;
    drop(reserve);
    let remote_tcp = TcpListener::bind("127.0.0.1:0").await?;
    let tcp_spec = format!("{local_tcp}:{}", remote_tcp.local_addr()?);

    let local_unix = dir.path().join("local.sock");
    let remote_unix = dir.path().join("remote.sock");
    let unix_spec = format!("{}:{}", local_unix.display(), remote_unix.display());

    let child = client
        .command_ssh(&["-N", "-L", &tcp_spec, "-L", &unix_spec, "target"])
        .env_remove("SSH_AUTH_SOCK")
        .spawn()?;
    let mut relays = JoinSet::new();
    start_relay(&transport, &mut relays).await?;

    // Opening a channel to a missing Unix socket fails while SSH stays live.
    let mut stream = loop {
        match UnixStream::connect(&local_unix).await {
            Ok(stream) => break stream,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) => {
                sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };
    let ino = std::fs::metadata(&local_unix)?.ino();
    assert_eq!(stream.read(&mut [0]).await?, 0);

    let mut tcp = TcpStream::connect(local_tcp).await?;
    let (mut upstream, _) = remote_tcp.accept().await?;
    exchange(&mut tcp, &mut upstream).await?;

    // End the existing SSH transport while its forwarded TCP stream is open.
    relays.abort_all();
    let cancelled = relays.join_next().await.unwrap().unwrap_err();
    assert!(cancelled.is_cancelled());
    assert_eq!(tcp.read(&mut [0]).await?, 0);
    assert!(
        timeout(Duration::from_millis(250), transport.accept())
            .await
            .is_err(),
        "SSH reconnected without a new local client"
    );

    // The SSH server is unavailable for this attempt. A second client arrives
    // before the failed handshake finishes and must close during the backoff.
    let mut failed = TcpStream::connect(local_tcp).await?;
    let (attempt, _) = transport.accept().await?;
    let mut waiting = UnixStream::connect(&local_unix).await?;
    drop(attempt);
    assert_eq!(failed.read(&mut [0]).await?, 0);
    assert_eq!(waiting.read(&mut [0]).await?, 0);
    assert!(
        timeout(Duration::from_millis(250), transport.accept())
            .await
            .is_err(),
        "a client opened another SSH connection during the backoff"
    );

    let remote = UnixListener::bind(&remote_unix)?;
    std::fs::set_permissions(&remote_unix, std::fs::Permissions::from_mode(0o777))?;
    assert_eq!(std::fs::metadata(&local_unix)?.ino(), ino);

    // Both clients arrive before the replacement transport can authenticate.
    let (tcp, unix) = tokio::try_join!(
        TcpStream::connect(local_tcp),
        UnixStream::connect(&local_unix)
    )?;
    start_relay(&transport, &mut relays).await?;
    let ((upstream_tcp, _), (upstream_unix, _)) =
        tokio::try_join!(remote_tcp.accept(), remote.accept())?;
    tokio::try_join!(exchange(tcp, upstream_tcp), exchange(unix, upstream_unix))?;
    assert!(
        timeout(Duration::from_millis(250), transport.accept())
            .await
            .is_err(),
        "concurrent local clients opened more than one SSH connection"
    );

    t::terminate(child).await?;
    assert!(!local_unix.exists());

    Ok(())
}

async fn start_relay(
    listener: &TcpListener,
    relays: &mut JoinSet<io::Result<()>>,
) -> io::Result<()> {
    let (mut client, _) = listener.accept().await?;
    let mut server = TcpStream::connect("127.0.0.1:2222").await?;
    relays.spawn(async move {
        copy_bidirectional(&mut client, &mut server).await?;
        Ok(())
    });

    Ok(())
}

async fn exchange(
    mut local: impl AsyncRead + AsyncWrite + Unpin,
    mut remote: impl AsyncRead + AsyncWrite + Unpin,
) -> io::Result<()> {
    let mut data = [0; 7];
    local.write_all(b"request").await?;
    remote.read_exact(&mut data).await?;
    assert_eq!(&data, b"request");

    remote.write_all(b"response").await?;
    let mut data = [0; 8];
    local.read_exact(&mut data).await?;
    assert_eq!(&data, b"response");

    Ok(())
}
