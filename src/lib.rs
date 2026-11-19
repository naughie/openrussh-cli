mod cli;
mod connection;
mod forwarding;
mod handler;
mod known_hosts;
mod logging;
mod scp;
mod session;

use crate::connection::Connection;
use openrussh_cli_config as config;
use openrussh_cli_terminal as terminal;
use openrussh_client::russh::client::Handler;

use anyhow::Result;

use std::process::ExitCode;

/// Runs one SSH CLI invocation and returns its process exit code after cleanup.
///
/// The caller owns the Tokio runtime and its shutdown policy. Host-key prompts
/// use blocking tasks, so runtime shutdown must not wait for an interrupted
/// prompt to receive more terminal input.
pub async fn run_ssh() -> ExitCode {
    run_invocation(
        "ssh",
        cli::parse_ssh().and_then(|invocation| Ok((invocation.build()?, None))),
    )
    .await
}

/// Copies files using SCP command-line arguments and returns the exit code after cleanup.
///
/// Uses the same configuration and connection lifecycle as [`run_ssh()`]. The caller
/// owns the Tokio runtime and must use the same shutdown policy described there.
pub async fn run_scp() -> ExitCode {
    run_invocation(
        "scp",
        cli::parse_scp()
            .and_then(|(invocation, transfer)| Ok((invocation.build()?, Some(transfer)))),
    )
    .await
}

async fn run_invocation(
    program: &'static str,
    conf: Result<(config::Config, Option<scp::Transfer>)>,
) -> ExitCode {
    let mut conf = conf;
    let (verbose, log_filter) = match &mut conf {
        Ok((conf, _)) => (conf.verbose, conf.meta.log_filter.take()),
        Err(error) => (
            false,
            error
                .downcast_mut::<config::Error>()
                .and_then(config::Error::take_log_filter),
        ),
    };
    logging::init(program, verbose, log_filter);

    let result = match conf {
        Ok((conf, transfer)) => run_with_config(conf, transfer).await,
        Err(e) => Err(e),
    };
    let status = match result {
        Ok(status) => status,
        Err(e) => {
            tracing::error!("{e:#}");
            255
        }
    };
    ExitCode::from(status)
}

async fn run_with_config(conf: config::Config, transfer: Option<scp::Transfer>) -> Result<u8> {
    use crate::handler::ClientHandlerFactory;
    use crate::known_hosts::KnownHostsFile;
    use openrussh_cli_terminal::{Event, Signals};

    let preferred = conf.preferred()?;
    let known_hosts = KnownHostsFile::load(conf.meta.known_hosts.as_deref())?;
    let make_handler = ClientHandlerFactory::new(preferred, known_hosts, &conf.connection);

    let mut signals = Signals::new()?;

    let conn = {
        let conn = Connection::connect(conf.connection.chain(), &make_handler, conf.address_family);
        tokio::pin!(conn);
        let (shutdown, events) = signals.split();

        loop {
            tokio::select! {
                res = &mut conn => break res.map_err(|e| anyhow::anyhow!(e))?,
                _ = shutdown.recv() => return Ok(255),
                event = events.recv() => {
                    match event {
                        Event::Suspend => terminal::suspend()?,
                        Event::Resize | Event::Continue => {}
                    }
                }
            }
        }
    };

    tracing::info!(
        "Authenticated to {} (port {}) as '{}'.",
        conf.connection.chain().target.dest.name,
        conf.connection.chain().target.dest.port,
        conf.connection.chain().target.dest.user
    );

    drop(make_handler);
    run_connected(conn, conf, transfer, &mut signals).await
}

async fn run_connected<H>(
    conn: Connection<H>,
    conf: config::Config,
    transfer: Option<scp::Transfer>,
    signals: &mut terminal::Signals,
) -> Result<u8>
where
    H: Handler + Send + 'static,
{
    use crate::forwarding::Forwarding;

    use std::sync::Arc;

    let operation = match transfer {
        Some(transfer) => Operation::Copy {
            transfer,
            send_env: conf.send_env,
        },
        None => Operation::Session {
            session: conf.session,
            stdin_null: conf.stdin_null,
            escape_char: conf.escape_char,
            send_env: conf.send_env,
        },
    };

    let Some(forwards) = conf.local_forwards.as_deref().filter(|v| !v.is_empty()) else {
        let res = operation.run(&conn, signals).await;

        return finish(&conn, res).await;
    };

    let conn = Arc::new(conn);

    let mut forwarding = {
        let (shutdown, _) = signals.split();

        tokio::select! {
            res = Forwarding::start(&conn, forwards, conf.address_family) => match res {
                Ok(forwarding) => forwarding,
                Err(error) => return finish(&conn, Err(error)).await,
            },
            sig = shutdown.recv() => return finish(&conn, Ok(operation.signal_status(sig))).await,
        }
    };

    let res = tokio::select! {
        res = operation.run(&conn, signals) => res,
        res = forwarding.wait() => match res {
            Ok(never) => match never {},
            Err(e) => Err(e),
        },
    };

    forwarding.stop().await;
    finish(&conn, res).await
}

enum Operation {
    Session {
        session: config::Session,
        stdin_null: bool,
        escape_char: Option<u8>,
        send_env: Option<Vec<String>>,
    },
    Copy {
        transfer: scp::Transfer,
        send_env: Option<Vec<String>>,
    },
}

impl Operation {
    fn signal_status(&self, sig: i32) -> u8 {
        match self {
            Self::Session { session, .. } => signal_status(session, sig),
            Self::Copy { .. } => 255,
        }
    }

    async fn run<H: Handler>(
        self,
        conn: &Connection<H>,
        signals: &mut terminal::Signals,
    ) -> Result<u8> {
        match self {
            Self::Session {
                session,
                stdin_null,
                escape_char,
                send_env,
            } => {
                run_session(
                    conn,
                    session,
                    stdin_null,
                    escape_char,
                    send_env.as_deref(),
                    signals,
                )
                .await
            }
            Self::Copy { transfer, send_env } => {
                use terminal::Event;

                let (shutdown, events) = signals.split();
                let copy = transfer.run(conn, send_env.as_deref());
                tokio::pin!(copy);

                loop {
                    tokio::select! {
                        res = &mut copy => return res,
                        _ = shutdown.recv() => return Ok(255),
                        event = events.recv() => match event {
                            Event::Suspend => terminal::suspend()?,
                            Event::Resize | Event::Continue => {},
                        }
                    }
                }
            }
        }
    }
}

async fn run_session<H: Handler>(
    conn: &Connection<H>,
    session: config::Session,
    stdin_null: bool,
    escape_char: Option<u8>,
    send_env: Option<&[String]>,
    signals: &mut terminal::Signals,
) -> Result<u8> {
    let (shutdown, events) = signals.split();
    let sigterm_status = signal_status(&session, libc::SIGTERM);

    tokio::select! {
        res = session::run(conn, session, stdin_null, escape_char, send_env, events) => res,
        sig = shutdown.recv() => Ok(if sig == libc::SIGTERM { sigterm_status } else { 255 }),
    }
}

fn signal_status(session: &config::Session, sig: i32) -> u8 {
    use openrussh_cli_config::Session;

    if matches!(session, Session::None | Session::Forward { .. }) && sig == libc::SIGTERM {
        0
    } else {
        255
    }
}

async fn finish<H>(conn: &Connection<H>, res: Result<u8>) -> Result<u8>
where
    H: Handler,
{
    use openrussh_client::connect::Disconnect;

    let disconnect = conn.disconnect(Disconnect::ByApplication, "", "").await;

    match res {
        Ok(status) => {
            if !conn.handle().is_closed() {
                disconnect?;
            }

            tracing::debug!("Exit status {status}");
            Ok(status)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::signal_status;
    use openrussh_cli_config::Session;

    #[test]
    fn terminating_forward_only_sessions_succeeds() {
        for session in [
            Session::None,
            Session::Forward {
                host: "localhost".to_owned(),
                port: 22,
            },
        ] {
            assert_eq!(signal_status(&session, libc::SIGTERM), 0);

            assert_eq!(signal_status(&session, libc::SIGINT), 255);

            assert_eq!(signal_status(&session, libc::SIGHUP), 255);
        }
    }

    #[test]
    fn terminating_remote_sessions_fails() {
        for session in [
            Session::Shell,
            Session::Exec {
                command: "sleep 10".to_owned(),
            },
        ] {
            for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT, libc::SIGHUP] {
                assert_eq!(signal_status(&session, sig), 255);
            }
        }
    }
}
