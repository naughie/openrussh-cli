//! Application socket policy around the dependency's SSH connection helpers.

use openrussh_cli_config::AddressFamily;
use openrussh_client::auth::AuthMethods;
use openrussh_client::config::{Chain, RequestTty};
use openrussh_client::connect::{self, Context, MakeHandler};
use openrussh_client::russh;
use russh::client::{Handle, Handler};

use std::io;
use std::net::SocketAddr;

use tokio::net::TcpStream;

pub(crate) enum Connection<H: Handler> {
    Managed(connect::Connection<H>),
    Direct { handle: Handle<H>, tty: RequestTty },
}

impl<H: Handler> Connection<H> {
    pub(crate) fn handle(&self) -> &Handle<H> {
        match self {
            Self::Managed(connection) => connection.handle(),
            Self::Direct { handle, .. } => handle,
        }
    }

    pub(crate) fn request_tty(&self) -> RequestTty {
        match self {
            Self::Managed(connection) => connection.request_tty(),
            Self::Direct { tty, .. } => *tty,
        }
    }

    pub(crate) async fn disconnect(
        &self,
        reason: russh::Disconnect,
        description: &str,
        language: &str,
    ) -> Result<(), russh::Error> {
        self.handle()
            .disconnect(reason, description, language)
            .await
    }
}

impl<H: Handler + 'static> Connection<H> {
    pub(crate) async fn connect<M: MakeHandler<Handler = H>>(
        chain: &Chain,
        make_handler: M,
        family: AddressFamily,
    ) -> Result<Self, connect::Error<H, M::Error>> {
        // ProxyJump owns its own connection policy in OpenSSH. Keep those
        // hostnames for the forwarding server to resolve, without local DNS.
        if family == AddressFamily::Any
            || chain
                .bastions
                .as_ref()
                .is_some_and(|jumps| !jumps.is_empty())
        {
            return connect::Connection::connect(chain, make_handler)
                .await
                .map(Self::Managed);
        }

        let target = &chain.target;
        let context = make_handler
            .make_handler(target)
            .map_err(connect::Error::MakeHandler)?;
        let socket = connect_tcp(&target.dest.name, target.dest.port, family)
            .await
            .map_err(|error| connect::Error::Establish(H::Error::from(russh::Error::IO(error))))?;
        let (handler, config) = context.into_inner();
        if config.nodelay
            && let Err(error) = socket.set_nodelay(true)
        {
            tracing::warn!("set_nodelay() failed: {error:?}");
        }
        let handle = connect::connect_stream(
            &target.dest,
            AuthMethods::from_config(&target.auth),
            socket,
            Context::new(handler, config),
        )
        .await
        .map_err(connect::Error::cast::<M::Error>)?;

        Ok(Self::Direct {
            handle,
            tty: target.tty,
        })
    }
}

async fn connect_tcp(host: &str, port: u16, family: AddressFamily) -> io::Result<TcpStream> {
    let addresses = tokio::net::lookup_host((host, port)).await?;
    let mut last_error = None;
    for address in addresses.filter(|address| matches_family(family, address)) {
        match TcpStream::connect(address).await {
            Ok(socket) => return Ok(socket),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("no address for {host}:{port} in the requested address family"),
        )
    }))
}

pub(crate) fn matches_family(family: AddressFamily, address: &SocketAddr) -> bool {
    match family {
        AddressFamily::Any => true,
        AddressFamily::Inet => address.is_ipv4(),
        AddressFamily::Inet6 => address.is_ipv6(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn direct_connections_obey_the_family_without_changing_the_hostname() -> io::Result<()> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let socket = connect_tcp("localhost", port, AddressFamily::Inet).await?;
        assert!(socket.peer_addr()?.is_ipv4());
        let error = connect_tcp("127.0.0.1", port, AddressFamily::Inet6)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrNotAvailable);
        Ok(())
    }

    #[tokio::test]
    async fn direct_ipv6_connections_reject_ipv4_only_policy() -> io::Result<()> {
        let listener = match TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)).await {
            Ok(listener) => listener,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
                ) || error.raw_os_error() == Some(libc::EAFNOSUPPORT) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let port = listener.local_addr()?.port();
        let socket = connect_tcp("::1", port, AddressFamily::Inet6).await?;
        assert!(socket.peer_addr()?.is_ipv6());
        let error = connect_tcp("::1", port, AddressFamily::Inet)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrNotAvailable);
        Ok(())
    }
}
