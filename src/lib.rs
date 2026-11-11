mod cli;
mod config;
mod forwarding;
mod handler;
mod session;
mod terminal;

use openrussh_client::connect::Connection;
use openrussh_client::russh::client::Handler;

use anyhow::Result;

pub async fn main_impl() {
    let status = match run().await {
        Ok(status) => status,
        Err(e) => {
            eprintln!("ssh: {e:#}");
            255
        }
    };
    std::process::exit(i32::from(status));
}

async fn run() -> Result<u8> {
    use crate::handler::MyMakeHandler;
    use crate::terminal::{Event, Signals};

    let conf = cli::parse()?.load()?;
    let make_handler = MyMakeHandler::new(conf.preferred()?)?;

    let mut signals = Signals::new()?;

    let conn = {
        let conn = Connection::connect(&conf.chain, &make_handler);
        tokio::pin!(conn);
        let (shutdown, events) = signals.split();

        loop {
            tokio::select! {
                res = &mut conn => break res.map_err(|e| anyhow::anyhow!("{e}"))?,
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

    run_connected(conn, conf, &mut signals).await
}

async fn run_connected<H>(
    conn: Connection<H>,
    conf: config::Config,
    signals: &mut terminal::Signals,
) -> Result<u8>
where
    H: Handler + Send + 'static,
{
    use crate::forwarding::Forwarding;

    use std::sync::Arc;

    let Some(forwards) = conf.local_forwards.as_deref().filter(|v| !v.is_empty()) else {
        let res = run_session(
            &conn,
            conf.session,
            conf.stdin_null,
            conf.escape_char,
            signals,
        )
        .await;

        return finish(&conn, res).await;
    };

    let conn = Arc::new(conn);

    let mut forwarding = {
        let (shutdown, _) = signals.split();

        tokio::select! {
            res = Forwarding::start(&conn, forwards) => res?,
            sig = shutdown.recv() => return finish(&conn, Ok(signal_status(&conf.session, sig))).await,
        }
    };

    let res = tokio::select! {
        res = run_session(&conn, conf.session, conf.stdin_null, conf.escape_char, signals) => res,
        res = forwarding.wait() => match res {
            Ok(never) => match never {},
            Err(e) => Err(e),
        },
    };

    forwarding.stop().await;
    finish(&conn, res).await
}

async fn run_session<H: Handler>(
    conn: &Connection<H>,
    session: config::Session,
    stdin_null: bool,
    escape_char: Option<u8>,
    signals: &mut terminal::Signals,
) -> Result<u8> {
    let (shutdown, events) = signals.split();
    let sigterm_status = signal_status(&session, nix::libc::SIGTERM);

    tokio::select! {
        res = session::run(conn, session, stdin_null, escape_char, events) => res,
        sig = shutdown.recv() => Ok(if sig == nix::libc::SIGTERM { sigterm_status } else { 255 }),
    }
}

fn signal_status(session: &config::Session, sig: i32) -> u8 {
    use crate::config::Session;

    if matches!(session, Session::None | Session::Forward { .. }) && sig == nix::libc::SIGTERM {
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

            Ok(status)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::signal_status;
    use crate::config::Session;

    #[test]
    fn terminating_forward_only_sessions_succeeds() {
        for session in [
            Session::None,
            Session::Forward {
                host: "localhost".to_owned(),
                port: 22,
            },
        ] {
            assert_eq!(signal_status(&session, nix::libc::SIGTERM), 0);

            assert_eq!(signal_status(&session, nix::libc::SIGINT), 255);

            assert_eq!(signal_status(&session, nix::libc::SIGHUP), 255);
        }
    }

    #[test]
    fn terminating_remote_sessions_fails() {
        for session in [
            Session::Shell,
            Session::Exec {
                prog: "sleep".to_owned(),
                args: Some(vec!["10".to_owned()]),
            },
        ] {
            for sig in [
                nix::libc::SIGTERM,
                nix::libc::SIGINT,
                nix::libc::SIGQUIT,
                nix::libc::SIGHUP,
            ] {
                assert_eq!(signal_status(&session, sig), 255);
            }
        }
    }
}
