use openrussh_cli_config::{BindHost, LocalForward};

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
use std::task::{Context, Poll};

pub(crate) struct Forwarding<'a, H: Handler> {
    conn: &'a Arc<Connection<H>>,
    listeners: Vec<Listener<'a>>,
    tasks: JoinSet<Result<()>>,
    next_listener: usize,
}

enum Listener<'a> {
    Tcp {
        listener: TcpListener,
        target_host: &'a String,
        target_port: u16,
    },
    Unix {
        listener: UnixListener,
        _path: SocketPath,
        target_path: &'a String,
    },
}

enum Incoming {
    Tcp {
        stream: TcpStream,
        peer: SocketAddr,
        target_host: String,
        target_port: u16,
    },
    Unix {
        stream: UnixStream,
        target_path: String,
    },
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

impl<'a, H> Forwarding<'a, H>
where
    H: Handler,
{
    pub(crate) async fn start(
        conn: &'a Arc<Connection<H>>,
        forwards: &'a [LocalForward],
    ) -> Result<Self> {
        let listeners = bind(forwards).await?;

        Ok(Self {
            conn,
            listeners,
            tasks: JoinSet::new(),
            next_listener: 0,
        })
    }

    pub(crate) async fn wait(&mut self) -> Result<Infallible>
    where
        H: 'static,
    {
        loop {
            tokio::select! {
                Some(res) = self.tasks.join_next() => {
                    match res {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => tracing::error!("local forward: {e:#}"),
                        Err(e) => return Err(e).context("local forwarding task failed"),
                    }
                }
                res = accept_next(&self.listeners, &mut self.next_listener) => {
                    let incoming = res?;
                    tracing::debug!("Local forwarding connection accepted.");
                    let conn = Arc::clone(self.conn);
                    self.tasks.spawn(async move { forward(&conn, incoming).await });
                }
            }
        }
    }

    pub(crate) async fn stop(&mut self) {
        self.listeners.clear();
        self.tasks.shutdown().await;
    }
}

async fn bind(forwards: &[LocalForward]) -> Result<Vec<Listener<'_>>> {
    let mut listeners = Vec::with_capacity(forwards.len());

    for conf in forwards {
        match conf {
            LocalForward::Tcp {
                bind_host: host,
                bind_port: port,
                target_host,
                target_port,
            } => {
                let res = match host {
                    BindHost::Loopback => bind_tcp(
                        [
                            SocketAddr::from((Ipv4Addr::LOCALHOST, *port)),
                            SocketAddr::from((Ipv6Addr::LOCALHOST, *port)),
                        ],
                        target_host,
                        *target_port,
                        &mut listeners,
                    ),
                    BindHost::All => bind_tcp(
                        [
                            SocketAddr::from((Ipv4Addr::UNSPECIFIED, *port)),
                            SocketAddr::from((Ipv6Addr::UNSPECIFIED, *port)),
                        ],
                        target_host,
                        *target_port,
                        &mut listeners,
                    ),
                    BindHost::Host(host) => {
                        let addrs = tokio::net::lookup_host((host.as_str(), *port))
                            .await
                            .with_context(|| format!("resolve local forward {host}:{port}"))?;
                        bind_tcp(addrs, target_host, *target_port, &mut listeners)
                    }
                };

                let name = match host {
                    BindHost::Loopback => "localhost",
                    BindHost::All => "*",
                    BindHost::Host(host) => host,
                };
                res.with_context(|| format!("bind local forward {name}:{port}"))?;
            }
            LocalForward::Unix { bind: path, target } => {
                let listener = UnixListener::bind(path)
                    .with_context(|| format!("bind local forward {}", path.display()))?;
                // On inspection failure, RAII closes the listener. Without its
                // identity, a later lookup cannot prove this path is ours.
                let meta = std::fs::symlink_metadata(path)
                    .with_context(|| format!("inspect local forward {}", path.display()))?;
                listeners.push(Listener::Unix {
                    listener,
                    _path: SocketPath {
                        path: path.clone(),
                        dev: meta.dev(),
                        ino: meta.ino(),
                    },
                    target_path: target,
                });
                tracing::debug!("Local forwarding listening on {}.", path.display());
            }
        }
    }

    Ok(listeners)
}

fn bind_tcp<'conf>(
    addrs: impl IntoIterator<Item = SocketAddr>,
    target_host: &'conf String,
    target_port: u16,
    listeners: &mut Vec<Listener<'conf>>,
) -> io::Result<()> {
    use socket2::SockRef;

    let before = listeners.len();
    let mut error = None;

    for addr in addrs {
        let bind = || {
            let socket = if addr.is_ipv6() {
                let socket = TcpSocket::new_v6()?;
                SockRef::from(&socket).set_only_v6(true)?;
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
                listeners.push(Listener::Tcp {
                    listener,
                    target_host,
                    target_port,
                });
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

async fn accept_next(listeners: &[Listener<'_>], next: &mut usize) -> Result<Incoming> {
    std::future::poll_fn(|cx| {
        for offset in 0..listeners.len() {
            let index = (*next + offset) % listeners.len();
            if let Poll::Ready(incoming) = listeners[index].poll_accept(cx) {
                *next = (index + 1) % listeners.len();
                return Poll::Ready(incoming);
            }
        }

        Poll::Pending
    })
    .await
}

impl Listener<'_> {
    fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<Result<Incoming>> {
        match self {
            Self::Tcp {
                listener,
                target_host,
                target_port,
            } => {
                let (stream, peer) = std::task::ready!(listener.poll_accept(cx))
                    .context("accept local TCP forward")?;
                Poll::Ready(Ok(Incoming::Tcp {
                    stream,
                    peer,
                    target_host: (**target_host).clone(),
                    target_port: *target_port,
                }))
            }
            Self::Unix {
                listener,
                target_path,
                ..
            } => {
                let (stream, _) = std::task::ready!(listener.poll_accept(cx))
                    .context("accept local Unix forward")?;
                Poll::Ready(Ok(Incoming::Unix {
                    stream,
                    target_path: (**target_path).clone(),
                }))
            }
        }
    }
}

async fn forward<H: Handler>(conn: &Connection<H>, incoming: Incoming) -> Result<()> {
    match incoming {
        Incoming::Tcp {
            stream,
            peer,
            target_host,
            target_port,
        } => {
            conn.forward_local_tcp((target_host, target_port).into(), Some(peer.into()), stream)
                .await?
                .wait()
                .await?;
        }
        Incoming::Unix {
            stream,
            target_path,
        } => {
            conn.forward_local_uds(target_path, stream)
                .await?
                .wait()
                .await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;
    use tokio::time::{Duration, timeout};

    fn tcp(bind_host: BindHost, bind_port: u16) -> LocalForward {
        LocalForward::Tcp {
            bind_host,
            bind_port,
            target_host: "127.0.0.1".to_owned(),
            target_port: 22,
        }
    }

    fn unix(bind: PathBuf) -> LocalForward {
        LocalForward::Unix {
            bind,
            target: "/tmp/remote.sock".to_owned(),
        }
    }

    async fn accept(listener: &Listener<'_>) -> Result<Incoming> {
        std::future::poll_fn(|cx| listener.poll_accept(cx)).await
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
                    || e.raw_os_error() == Some(libc::EAFNOSUPPORT) =>
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
    async fn ready_listeners_are_accepted_in_rotation() -> Result<()> {
        let forwards = [
            LocalForward::Tcp {
                bind_host: BindHost::Host("127.0.0.1".to_owned()),
                bind_port: 0,
                target_host: "first.example".to_owned(),
                target_port: 22,
            },
            LocalForward::Tcp {
                bind_host: BindHost::Host("127.0.0.1".to_owned()),
                bind_port: 0,
                target_host: "second.example".to_owned(),
                target_port: 2222,
            },
        ];
        let listeners = bind(&forwards).await?;
        let [
            Listener::Tcp {
                listener: first, ..
            },
            Listener::Tcp {
                listener: second, ..
            },
        ] = listeners.as_slice()
        else {
            panic!("Expected two TCP listeners");
        };
        let _first_client = TcpStream::connect(first.local_addr()?).await?;
        let _another_first_client = TcpStream::connect(first.local_addr()?).await?;
        let _second_client = TcpStream::connect(second.local_addr()?).await?;
        let mut next = 0;

        for (expected_host, expected_port) in [
            ("first.example", 22),
            ("second.example", 2222),
            ("first.example", 22),
        ] {
            let Incoming::Tcp {
                target_host,
                target_port,
                ..
            } = timeout(Duration::from_secs(3), accept_next(&listeners, &mut next)).await??
            else {
                panic!("Expected a TCP connection");
            };

            assert_eq!(target_host, expected_host);
            assert_eq!(target_port, expected_port);
        }

        Ok(())
    }

    #[tokio::test]
    async fn explicit_ipv4_bind_accepts_and_reports_the_peer() -> Result<()> {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let forwards = [tcp(BindHost::Host("127.0.0.1".to_owned()), 0)];

        let listeners = bind(&forwards).await?;
        let [listener @ Listener::Tcp { listener: tcp, .. }] = listeners.as_slice() else {
            panic!("Expected one TCP listener");
        };

        let mut client = TcpStream::connect(tcp.local_addr()?).await?;
        let local = client.local_addr()?;
        let Incoming::Tcp {
            stream: mut server,
            peer,
            target_host,
            target_port,
        } = timeout(Duration::from_secs(3), accept(listener)).await??
        else {
            panic!("Expected a TCP connection");
        };

        assert_eq!(peer, local);
        assert_eq!(target_host, "127.0.0.1");
        assert_eq!(target_port, 22);
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
        let forwards = [unix(path.clone())];
        let listeners = bind(&forwards).await?;

        assert!(std::fs::symlink_metadata(&path)?.file_type().is_socket());

        let mut client = UnixStream::connect(&path).await?;
        let Incoming::Unix {
            stream: mut server,
            target_path,
        } = timeout(Duration::from_secs(3), accept(&listeners[0])).await??
        else {
            panic!("Expected a Unix connection");
        };
        assert_eq!(target_path, "/tmp/remote.sock");
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
        let forwards = [tcp(BindHost::Host("127.0.0.1".to_owned()), 0)];

        let listeners = bind(&forwards).await?;
        let [Listener::Tcp { listener, .. }] = listeners.as_slice() else {
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
        let forwards = [tcp(BindHost::Loopback, 0)];

        let listeners = bind(&forwards).await?;
        let mut v4 = false;
        let mut v6 = false;

        for listener in &listeners {
            let Listener::Tcp { listener, .. } = listener else {
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
        use socket2::SockRef;

        let ipv6 = ipv6_available()?;
        let forwards = [tcp(BindHost::All, 0)];

        let listeners = bind(&forwards).await?;
        let mut v4 = false;
        let mut v6 = false;

        for listener in &listeners {
            let Listener::Tcp { listener, .. } = listener else {
                panic!("Expected a TCP listener");
            };

            let addr = listener.local_addr()?;

            assert!(addr.ip().is_unspecified());
            v4 |= addr.is_ipv4();
            v6 |= addr.is_ipv6();

            if addr.is_ipv6() {
                assert!(SockRef::from(listener).only_v6()?);
            }
        }

        assert!(v4);

        assert_eq!(v6, ipv6);

        Ok(())
    }

    #[tokio::test]
    async fn explicit_ipv6_does_not_create_an_ipv4_listener() -> Result<()> {
        use socket2::SockRef;

        if !ipv6_available()? {
            return Ok(());
        }

        let forwards = [tcp(BindHost::Host("::1".to_owned()), 0)];

        let listeners = bind(&forwards).await?;
        let [Listener::Tcp { listener, .. }] = listeners.as_slice() else {
            panic!("Expected one IPv6 listener");
        };

        assert_eq!(listener.local_addr()?.ip(), Ipv6Addr::LOCALHOST);

        assert!(SockRef::from(listener).only_v6()?);

        Ok(())
    }

    #[tokio::test]
    async fn occupied_explicit_port_returns_the_bind_error() -> Result<()> {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = occupied.local_addr()?;
        let forwards = [tcp(BindHost::Host(addr.ip().to_string()), addr.port())];

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
        let target_host = "127.0.0.1".to_owned();
        let mut listeners = Vec::new();
        bind_tcp(
            [occupied_addr, (Ipv4Addr::LOCALHOST, 0).into()],
            &target_host,
            22,
            &mut listeners,
        )?;

        let [Listener::Tcp { listener, .. }] = listeners.as_slice() else {
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
            unix(first.clone()),
            unix(second.clone()),
            tcp(BindHost::Host(addr.ip().to_string()), addr.port()),
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
        let forwards = [unix(path.clone())];

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

        let forwards = [unix(path.clone())];

        assert!(bind(&forwards).await.is_err());

        assert_eq!(std::fs::read_to_string(&path)?, "existing content");

        Ok(())
    }

    #[tokio::test]
    async fn cleanup_preserves_a_replacement_inode() -> Result<()> {
        let dir = dir()?;
        let path = dir.path().join("forward.sock");
        let replacement = dir.path().join("replacement");
        let forwards = [unix(path.clone())];
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
        let forwards = [unix(path.clone())];
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
