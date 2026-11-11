use crate::config::{BindHost, ForwardTarget, LocalBind, LocalForward};

use openrussh_client::connect::Connection;
use openrussh_client::russh::client::Handler;

use anyhow::{Context as _, Result};
use tokio::net::{TcpListener, TcpSocket, TcpStream, UnixListener, UnixStream};
use tokio::task::JoinSet;

use std::convert::Infallible;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) struct Forwarding {
    tasks: JoinSet<Result<Infallible>>,
}

enum Listener {
    Tcp(TcpListener),
    Unix {
        listener: UnixListener,
        _path: SocketPath,
    },
}

enum Incoming {
    Tcp(TcpStream, SocketAddr),
    Unix(UnixStream),
}

struct SocketPath {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        if let Ok(meta) = std::fs::symlink_metadata(&self.path)
            && meta.dev() == self.dev
            && meta.ino() == self.ino
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Forwarding {
    pub(crate) async fn start<H>(
        conn: &Arc<Connection<H>>,
        forwards: &[LocalForward],
    ) -> Result<Self>
    where
        H: Handler + Send + 'static,
    {
        let listeners = bind(forwards).await?;

        let mut tasks = JoinSet::new();

        for (listener, target) in listeners {
            let conn = Arc::clone(conn);
            let target = target.clone();
            tasks.spawn(async move { listen(&conn, listener, &target).await });
        }

        Ok(Self { tasks })
    }

    pub(crate) async fn wait(&mut self) -> Result<Infallible> {
        match self.tasks.join_next().await {
            Some(res) => res.context("local forwarding task stopped")?,
            None => std::future::pending().await,
        }
    }

    pub(crate) async fn stop(&mut self) {
        self.tasks.shutdown().await;
    }
}

async fn bind(forwards: &[LocalForward]) -> Result<Vec<(Listener, &ForwardTarget)>> {
    let mut listeners = Vec::with_capacity(forwards.len());

    for conf in forwards {
        match &conf.bind {
            LocalBind::Tcp { host, port } => {
                let res = match host {
                    BindHost::Loopback => bind_tcp(
                        [
                            SocketAddr::from((Ipv4Addr::LOCALHOST, *port)),
                            SocketAddr::from((Ipv6Addr::LOCALHOST, *port)),
                        ],
                        &conf.target,
                        &mut listeners,
                    ),
                    BindHost::All => bind_tcp(
                        [
                            SocketAddr::from((Ipv4Addr::UNSPECIFIED, *port)),
                            SocketAddr::from((Ipv6Addr::UNSPECIFIED, *port)),
                        ],
                        &conf.target,
                        &mut listeners,
                    ),
                    BindHost::Host(host) => {
                        let addrs = tokio::net::lookup_host((host.as_str(), *port))
                            .await
                            .with_context(|| format!("resolve local forward {host}:{port}"))?;
                        bind_tcp(addrs, &conf.target, &mut listeners)
                    }
                };

                let name = match host {
                    BindHost::Loopback => "localhost",
                    BindHost::All => "*",
                    BindHost::Host(host) => host,
                };
                res.with_context(|| format!("bind local forward {name}:{port}"))?;
            }
            LocalBind::Unix(path) => {
                let listener = UnixListener::bind(path)
                    .with_context(|| format!("bind local forward {}", path.display()))?;
                let meta = match std::fs::symlink_metadata(path) {
                    Ok(meta) => meta,
                    Err(e) => {
                        drop(listener);

                        if let Ok(meta) = std::fs::symlink_metadata(path) {
                            use std::os::unix::fs::FileTypeExt as _;

                            if meta.file_type().is_socket() {
                                drop(SocketPath {
                                    path: path.clone(),
                                    dev: meta.dev(),
                                    ino: meta.ino(),
                                });
                            }
                        }

                        return Err(e)
                            .with_context(|| format!("inspect local forward {}", path.display()));
                    }
                };
                listeners.push((
                    Listener::Unix {
                        listener,
                        _path: SocketPath {
                            path: path.clone(),
                            dev: meta.dev(),
                            ino: meta.ino(),
                        },
                    },
                    &conf.target,
                ));
                tracing::debug!("Local forwarding listening on {}.", path.display());
            }
        }
    }

    Ok(listeners)
}

fn bind_tcp<'conf>(
    addrs: impl IntoIterator<Item = SocketAddr>,
    target: &'conf ForwardTarget,
    listeners: &mut Vec<(Listener, &'conf ForwardTarget)>,
) -> io::Result<()> {
    use nix::sys::socket::{setsockopt, sockopt::Ipv6V6Only};

    let before = listeners.len();
    let mut error = None;

    for addr in addrs {
        let bind = || {
            let socket = if addr.is_ipv6() {
                let socket = TcpSocket::new_v6()?;
                setsockopt(&socket, Ipv6V6Only, &true)?;
                socket
            } else {
                TcpSocket::new_v4()?
            };
            socket.set_reuseaddr(true)?;
            socket.bind(addr)?;
            socket.listen(1024)
        };

        match bind() {
            Ok(listener) => {
                tracing::debug!(
                    "Local forwarding listening on {}.",
                    listener.local_addr().unwrap_or(addr)
                );
                listeners.push((Listener::Tcp(listener), target));
            }
            Err(e) => error = Some(e),
        }
    }

    if listeners.len() == before {
        return Err(error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AddrNotAvailable, "no local address found")
        }));
    }

    Ok(())
}

async fn listen<H>(
    conn: &Arc<Connection<H>>,
    listener: Listener,
    target: &ForwardTarget,
) -> Result<Infallible>
where
    H: Handler + Send + 'static,
{
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            res = tasks.join_next(), if !tasks.is_empty() => {
                match res {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(e))) => tracing::error!("local forward: {e:#}"),
                    Some(Err(e)) => return Err(e).context("local forwarding task failed"),
                    None => {}
                }
            }
            res = accept(&listener) => {
                let incoming = res?;
                tracing::debug!("Local forwarding connection accepted.");
                let conn = Arc::clone(conn);
                let target = target.clone();
                tasks.spawn(async move { forward(&conn, incoming, &target).await });
            }
        }
    }
}

async fn accept(listener: &Listener) -> Result<Incoming> {
    match listener {
        Listener::Tcp(listener) => {
            let (stream, peer) = listener
                .accept()
                .await
                .context("accept local TCP forward")?;
            Ok(Incoming::Tcp(stream, peer))
        }
        Listener::Unix { listener, .. } => {
            let (stream, _) = listener
                .accept()
                .await
                .context("accept local Unix forward")?;
            Ok(Incoming::Unix(stream))
        }
    }
}

async fn forward<H: Handler>(
    conn: &Connection<H>,
    incoming: Incoming,
    target: &ForwardTarget,
) -> Result<()> {
    match (incoming, target) {
        (Incoming::Tcp(stream, peer), ForwardTarget::Tcp { host, port }) => {
            conn.forward_local_tcp((host.as_str(), *port).into(), Some(peer.into()), stream)
                .await?
                .wait()
                .await?;
        }
        (Incoming::Unix(stream), ForwardTarget::Unix(path)) => {
            conn.forward_local_uds(path.clone(), stream)
                .await?
                .wait()
                .await?;
        }
        (Incoming::Tcp(..), ForwardTarget::Unix(_))
        | (Incoming::Unix(_), ForwardTarget::Tcp { .. }) => {
            anyhow::bail!("TCP and Unix forwarding endpoints must use the same transport");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;
    use tokio::time::{Duration, timeout};

    fn conf(bind: LocalBind) -> LocalForward {
        let target = match &bind {
            LocalBind::Tcp { .. } => ForwardTarget::Tcp {
                host: "127.0.0.1".to_owned(),
                port: 22,
            },
            LocalBind::Unix(_) => ForwardTarget::Unix("/tmp/remote.sock".to_owned()),
        };

        LocalForward { bind, target }
    }

    fn dir() -> io::Result<TempDir> {
        tempfile::Builder::new()
            .prefix("ssh-bind-")
            .tempdir_in("/tmp")
    }

    fn ipv6_available() -> io::Result<bool> {
        match TcpSocket::new_v6().and_then(|s| s.bind((Ipv6Addr::LOCALHOST, 0).into())) {
            Ok(()) => Ok(true),
            Err(e)
                if e.kind() == io::ErrorKind::AddrNotAvailable
                    || e.kind() == io::ErrorKind::Unsupported
                    || e.raw_os_error() == Some(nix::libc::EAFNOSUPPORT) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    #[tokio::test]
    async fn no_forwards_create_no_listeners() -> Result<()> {
        assert!(bind(&[]).await?.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn explicit_ipv4_bind_accepts_and_reports_the_peer() -> Result<()> {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let forwards = [conf(LocalBind::Tcp {
            host: BindHost::Host("127.0.0.1".to_owned()),
            port: 0,
        })];

        let listeners = bind(&forwards).await?;
        let [(listener @ Listener::Tcp(tcp), _)] = listeners.as_slice() else {
            panic!("Expected one TCP listener");
        };

        let mut client = TcpStream::connect(tcp.local_addr()?).await?;
        let local = client.local_addr()?;
        let Incoming::Tcp(mut server, peer) =
            timeout(Duration::from_secs(3), accept(listener)).await??
        else {
            panic!("Expected a TCP connection");
        };

        assert_eq!(peer, local);
        client.write_all(b"forward payload").await?;

        let mut buf = [0; 15];
        timeout(Duration::from_secs(3), server.read_exact(&mut buf)).await??;

        assert_eq!(&buf, b"forward payload");

        Ok(())
    }

    #[tokio::test]
    async fn unix_bind_accepts_and_removes_its_socket() -> Result<()> {
        use std::os::unix::fs::FileTypeExt as _;
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let dir = dir()?;
        let path = dir.path().join("forward.sock");
        let forwards = [conf(LocalBind::Unix(path.clone()))];
        let listeners = bind(&forwards).await?;

        assert!(std::fs::symlink_metadata(&path)?.file_type().is_socket());

        let mut client = UnixStream::connect(&path).await?;
        let Incoming::Unix(mut server) =
            timeout(Duration::from_secs(3), accept(&listeners[0].0)).await??
        else {
            panic!("Expected a Unix connection");
        };
        client.write_all(b"unix payload").await?;

        let mut buf = [0; 12];
        timeout(Duration::from_secs(3), server.read_exact(&mut buf)).await??;

        assert_eq!(&buf, b"unix payload");
        drop(server);
        drop(client);
        drop(listeners);

        assert!(!path.exists());

        Ok(())
    }

    #[tokio::test]
    async fn dropping_a_tcp_binding_releases_its_address() -> Result<()> {
        let forwards = [conf(LocalBind::Tcp {
            host: BindHost::Host("127.0.0.1".to_owned()),
            port: 0,
        })];

        let listeners = bind(&forwards).await?;
        let [(Listener::Tcp(listener), _)] = listeners.as_slice() else {
            panic!("Expected one TCP listener");
        };

        let addr = listener.local_addr()?;
        drop(listeners);

        let replacement = TcpListener::bind(addr).await?;

        assert_eq!(replacement.local_addr()?, addr);

        Ok(())
    }

    #[tokio::test]
    async fn loopback_binds_each_available_address_family() -> Result<()> {
        let ipv6 = ipv6_available()?;
        let forwards = [conf(LocalBind::Tcp {
            host: BindHost::Loopback,
            port: 0,
        })];

        let listeners = bind(&forwards).await?;
        let mut v4 = false;
        let mut v6 = false;

        for (listener, _) in &listeners {
            let Listener::Tcp(listener) = listener else {
                panic!("Expected a TCP listener");
            };

            let addr = listener.local_addr()?;

            assert!(addr.ip().is_loopback());
            v4 |= addr.is_ipv4();
            v6 |= addr.is_ipv6();
        }

        assert!(v4);

        assert_eq!(v6, ipv6);

        Ok(())
    }

    #[tokio::test]
    async fn wildcard_binds_each_available_address_family() -> Result<()> {
        use nix::sys::socket::{getsockopt, sockopt::Ipv6V6Only};

        let ipv6 = ipv6_available()?;
        let forwards = [conf(LocalBind::Tcp {
            host: BindHost::All,
            port: 0,
        })];

        let listeners = bind(&forwards).await?;
        let mut v4 = false;
        let mut v6 = false;

        for (listener, _) in &listeners {
            let Listener::Tcp(listener) = listener else {
                panic!("Expected a TCP listener");
            };

            let addr = listener.local_addr()?;

            assert!(addr.ip().is_unspecified());
            v4 |= addr.is_ipv4();
            v6 |= addr.is_ipv6();

            if addr.is_ipv6() {
                assert!(getsockopt(listener, Ipv6V6Only)?);
            }
        }

        assert!(v4);

        assert_eq!(v6, ipv6);

        Ok(())
    }

    #[tokio::test]
    async fn explicit_ipv6_does_not_create_an_ipv4_listener() -> Result<()> {
        use nix::sys::socket::{getsockopt, sockopt::Ipv6V6Only};

        if !ipv6_available()? {
            return Ok(());
        }

        let forwards = [conf(LocalBind::Tcp {
            host: BindHost::Host("::1".to_owned()),
            port: 0,
        })];

        let listeners = bind(&forwards).await?;
        let [(Listener::Tcp(listener), _)] = listeners.as_slice() else {
            panic!("Expected one IPv6 listener");
        };

        assert_eq!(listener.local_addr()?.ip(), Ipv6Addr::LOCALHOST);

        assert!(getsockopt(listener, Ipv6V6Only)?);

        Ok(())
    }

    #[tokio::test]
    async fn occupied_explicit_port_returns_the_bind_error() -> Result<()> {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = occupied.local_addr()?;
        let forwards = [conf(LocalBind::Tcp {
            host: BindHost::Host(addr.ip().to_string()),
            port: addr.port(),
        })];

        let e = match bind(&forwards).await {
            Ok(_) => panic!("An occupied port was bound twice"),
            Err(e) => e,
        };

        assert_eq!(
            e.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::AddrInUse)
        );

        assert!(format!("{e:#}").contains(&addr.to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn an_unavailable_address_does_not_discard_available_addresses() -> Result<()> {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let occupied_addr = occupied.local_addr()?;
        let target = ForwardTarget::Tcp {
            host: "127.0.0.1".to_owned(),
            port: 22,
        };

        let mut listeners = Vec::new();
        bind_tcp(
            [occupied_addr, (Ipv4Addr::LOCALHOST, 0).into()],
            &target,
            &mut listeners,
        )?;

        let [(Listener::Tcp(listener), _)] = listeners.as_slice() else {
            panic!("Expected the available address to remain bound");
        };

        assert_ne!(listener.local_addr()?, occupied_addr);

        Ok(())
    }

    #[tokio::test]
    async fn a_later_failure_rolls_back_all_new_unix_bindings() -> Result<()> {
        let dir = dir()?;
        let first = dir.path().join("first.sock");
        let second = dir.path().join("second.sock");
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = occupied.local_addr()?;
        let forwards = [
            conf(LocalBind::Unix(first.clone())),
            conf(LocalBind::Unix(second.clone())),
            conf(LocalBind::Tcp {
                host: BindHost::Host(addr.ip().to_string()),
                port: addr.port(),
            }),
        ];

        assert!(bind(&forwards).await.is_err());

        assert!(!first.exists());

        assert!(!second.exists());

        assert_eq!(occupied.local_addr()?, addr);

        Ok(())
    }

    #[tokio::test]
    async fn an_existing_unix_socket_is_not_unlinked() -> Result<()> {
        let dir = dir()?;
        let path = dir.path().join("existing.sock");
        let _existing = UnixListener::bind(&path)?;
        let before = std::fs::symlink_metadata(&path)?;
        let forwards = [conf(LocalBind::Unix(path.clone()))];

        assert!(bind(&forwards).await.is_err());

        let after = std::fs::symlink_metadata(&path)?;

        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));

        Ok(())
    }

    #[tokio::test]
    async fn an_existing_regular_file_is_not_replaced() -> Result<()> {
        let dir = dir()?;
        let path = dir.path().join("existing");
        std::fs::write(&path, "existing content")?;

        let forwards = [conf(LocalBind::Unix(path.clone()))];

        assert!(bind(&forwards).await.is_err());

        assert_eq!(std::fs::read_to_string(&path)?, "existing content");

        Ok(())
    }

    #[tokio::test]
    async fn cleanup_preserves_a_replacement_inode() -> Result<()> {
        let dir = dir()?;
        let path = dir.path().join("forward.sock");
        let replacement = dir.path().join("replacement");
        let forwards = [conf(LocalBind::Unix(path.clone()))];
        let listeners = bind(&forwards).await?;
        let original = std::fs::symlink_metadata(&path)?;
        std::fs::write(&replacement, "replacement content")?;

        let meta = std::fs::symlink_metadata(&replacement)?;

        assert_ne!((original.dev(), original.ino()), (meta.dev(), meta.ino()));
        std::fs::rename(&replacement, &path)?;
        drop(listeners);

        assert_eq!(std::fs::read_to_string(&path)?, "replacement content");

        Ok(())
    }

    #[tokio::test]
    async fn cleanup_does_not_follow_a_replacement_symlink() -> Result<()> {
        use std::os::unix::fs::symlink;

        let dir = dir()?;
        let path = dir.path().join("forward.sock");
        let target = dir.path().join("target");
        let forwards = [conf(LocalBind::Unix(path.clone()))];
        let listeners = bind(&forwards).await?;
        std::fs::write(&target, "preserved")?;
        std::fs::remove_file(&path)?;
        symlink(&target, &path)?;
        drop(listeners);

        assert!(std::fs::symlink_metadata(&path)?.file_type().is_symlink());

        assert_eq!(std::fs::read_to_string(&target)?, "preserved");

        Ok(())
    }
}
