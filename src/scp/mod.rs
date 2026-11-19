use crate::connection::Connection;
use crate::session::selected_environment;
use openrussh_client::russh::client::Handler;

use anyhow::{Context as _, Result};
use russh_sftp::client::SftpSession;

use std::path::PathBuf;

mod transfer;

pub(crate) enum Transfer {
    Upload {
        sources: Vec<PathBuf>,
        destination: String,
        recursive: bool,
    },
    Download {
        sources: Vec<String>,
        destination: PathBuf,
        recursive: bool,
    },
}

impl Transfer {
    pub(crate) async fn run<H: Handler>(
        &self,
        conn: &Connection<H>,
        send_env: Option<&[String]>,
    ) -> Result<u8> {
        let channel = conn.handle().channel_open_session().await?;
        if let Some(patterns) = send_env.filter(|patterns| !patterns.is_empty()) {
            for (name, value) in selected_environment(patterns, std::env::vars_os()) {
                channel.set_env(false, name, value).await?;
            }
        }
        channel.request_subsystem(true, "sftp").await?;
        let session = SftpSession::new(channel.into_stream())
            .await
            .context("Start SFTP subsystem")?;

        let result = match self {
            Self::Upload {
                sources,
                destination,
                recursive,
            } => transfer::upload(&session, sources, destination, *recursive).await,
            Self::Download {
                sources,
                destination,
                recursive,
            } => transfer::download(&session, sources, destination, *recursive).await,
        };
        let close = session.close().await.context("Close SFTP subsystem");

        result?;
        close?;
        Ok(0)
    }
}
