use crate::config::algorithms::Policies;
use crate::known_hosts::KnownHostsFile;

use openrussh_client::config::{Chain, Host};
use openrussh_client::connect::{Context, MakeHandler};
use openrussh_client::known_hosts::{KnownHostsHandler, MatchResult};
use openrussh_client::russh::Channel;
use openrussh_client::russh::Preferred;
use openrussh_client::russh::client::{ChannelOpenHandle, Config, Handler, Msg, Session};
use openrussh_client::russh::keys::{PublicKey, PublicKeyOrCertificate};

use anyhow::{Context as _, Result, bail};

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;

pub(crate) struct MyMakeHandler<'a> {
    known_hosts: KnownHostsFile,
    preferred: Preferred,
    algorithms: Option<(&'a Chain, &'a Policies)>,
}

impl<'a> MyMakeHandler<'a> {
    pub(crate) fn new(preferred: Preferred, known_hosts: KnownHostsFile) -> Self {
        Self {
            known_hosts,
            preferred,
            algorithms: None,
        }
    }

    pub(crate) fn with_algorithms(mut self, chain: &'a Chain, algorithms: &'a Policies) -> Self {
        self.algorithms = Some((chain, algorithms));
        self
    }
}

impl MakeHandler for MyMakeHandler<'_> {
    type Handler = MyHandler;
    type Error = anyhow::Error;

    fn make_handler(&self, host: &Host) -> Result<Context<Self::Handler>, Self::Error> {
        tracing::debug!("Connecting to {} port {}.", host.dest.name, host.dest.port);
        let known_hosts = self.known_hosts.handler(&host.dest.name, host.dest.port);
        let algorithms = self
            .algorithms
            .map(|(chain, policies)| policies.get(chain, host))
            .transpose()?;

        let mut conf = Config {
            preferred: self.preferred.clone(),
            ..Default::default()
        };
        if !algorithms.is_some_and(|algorithms| algorithms.has_host_keys()) {
            known_hosts.update_preferred_config(&mut conf.preferred);
        }
        if let Some(algorithms) = algorithms {
            algorithms.apply(&mut conf.preferred);
        }

        let handler = MyHandler {
            known_hosts,
            save_path: self.known_hosts.save_path().clone(),
            accepted: None,
            host: host.dest.name.to_ascii_lowercase(),
            port: host.dest.port,
        };

        Ok(Context::new(handler, Arc::new(conf)))
    }
}

pub(crate) struct MyHandler {
    known_hosts: KnownHostsHandler,
    save_path: Arc<Path>,
    accepted: Option<PublicKey>,
    host: String,
    port: u16,
}

impl Handler for MyHandler {
    type Error = anyhow::Error;

    async fn auth_banner(&mut self, banner: &str, _session: &mut Session) -> Result<()> {
        tracing::info!("{}", banner.trim_end_matches(['\r', '\n']));

        Ok(())
    }

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        let mut matched = self.known_hosts.check_server_key(key);

        if tracing::enabled!(tracing::Level::DEBUG) {
            use openrussh_client::russh::keys::ssh_key::HashAlg;

            let key = key.public_key();
            tracing::debug!(
                "Server host key: {} {}",
                key.algorithm(),
                key.fingerprint(HashAlg::Sha256)
            );
        }

        if matched == MatchResult::NotFound
            && let Some(accepted) = &self.accepted
        {
            let data = match key {
                PublicKeyOrCertificate::PublicKey { key, .. } => key.key_data(),
                PublicKeyOrCertificate::Certificate(cert) => cert.public_key(),
            };
            matched = if data == accepted.key_data() {
                MatchResult::Found
            } else {
                MatchResult::KeyMismatch
            };
        }

        match matched {
            MatchResult::Found => {
                tracing::debug!(
                    "Host '{}' is known and matches the server host key.",
                    host_port(&self.host, self.port)
                );
                Ok(true)
            }
            MatchResult::NotFound => {
                let host = self.host.clone();
                let port = self.port;
                let save_path = self.save_path.clone();
                let key = key.public_key();
                let accepted = tokio::task::spawn_blocking(move || {
                    let confirmed = confirm_key(&save_path, &host, port, &key)?;
                    Result::<_, anyhow::Error>::Ok(confirmed.then_some(key))
                })
                .await
                .context("Host key prompt failed")??;

                let confirmed = accepted.is_some();
                self.accepted = accepted;

                Ok(confirmed)
            }
            MatchResult::KeyMismatch => bail!(
                "WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\nHost key verification failed for {}",
                host_port(&self.host, self.port)
            ),
            MatchResult::Revoked => bail!(
                "WARNING: REVOKED HOST KEY DETECTED!\nHost key verification failed for {}",
                host_port(&self.host, self.port)
            ),
            MatchResult::PrincipalMismatch => bail!(
                "Host certificate does not match {}",
                host_port(&self.host, self.port)
            ),
            MatchResult::UnknownCriticalOptions => {
                bail!("Host certificate has unsupported critical options")
            }
            MatchResult::CertNotVerified => bail!(
                "Host certificate verification failed for {}",
                host_port(&self.host, self.port)
            ),
        }
    }

    async fn server_channel_open_agent_forward(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_agent_forward(channel, reply, session)
            .await?;
        Ok(())
    }

    async fn server_channel_open_x11(
        &mut self,
        channel: Channel<Msg>,
        originator_address: &str,
        originator_port: u32,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_x11(channel, originator_address, originator_port, reply, session)
            .await?;
        Ok(())
    }

    async fn server_channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_session(channel, reply, session)
            .await?;
        Ok(())
    }

    async fn server_channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        originator_address: &str,
        originator_port: u32,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_direct_tcpip(
                channel,
                host_to_connect,
                port_to_connect,
                originator_address,
                originator_port,
                reply,
                session,
            )
            .await?;
        Ok(())
    }

    async fn server_channel_open_direct_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_direct_streamlocal(channel, socket_path, reply, session)
            .await?;
        Ok(())
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_forwarded_tcpip(
                channel,
                connected_address,
                connected_port,
                originator_address,
                originator_port,
                reply,
                session,
            )
            .await?;
        Ok(())
    }

    async fn server_channel_open_forwarded_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<()> {
        self.known_hosts
            .server_channel_open_forwarded_streamlocal(channel, socket_path, reply, session)
            .await?;
        Ok(())
    }
}

fn host_port(host: &str, port: u16) -> impl std::fmt::Display + '_ {
    use std::fmt;

    struct HostPort<'h>(&'h str, u16);

    impl fmt::Display for HostPort<'_> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            if self.1 == 22 {
                f.write_str(self.0)
            } else {
                write!(f, "[{}]:{}", self.0, self.1)
            }
        }
    }

    HostPort(host, port)
}

fn confirm_key(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<bool> {
    use std::fs::OpenOptions;
    use std::io::BufReader;

    {
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("Host key verification requires a controlling terminal")?;
        if !prompt_key(&mut BufReader::new(&tty), &mut &tty, host, port, key)? {
            return Ok(false);
        }
    }
    save_key_at(path, host, port, key)
        .with_context(|| format!("Failed to save host key to {}", path.display()))?;
    tracing::warn!(
        "Warning: Permanently added '{}' ({}) to the list of known hosts.",
        host_port(host, port),
        key.algorithm()
    );

    Ok(true)
}

fn prompt_key<R: BufRead, W: Write>(
    rdr: &mut R,
    wtr: &mut W,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> io::Result<bool> {
    use openrussh_client::russh::keys::ssh_key::HashAlg;

    let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    writeln!(
        wtr,
        "The authenticity of host '{}' can't be established.\n{} key fingerprint is {}.",
        host_port(host, port),
        key.algorithm(),
        fingerprint
    )?;
    write!(
        wtr,
        "Are you sure you want to continue connecting (yes/no/[fingerprint])? "
    )?;

    let mut line = String::new();

    loop {
        wtr.flush()?;
        line.clear();

        if rdr.read_line(&mut line)? == 0 {
            return Ok(false);
        }

        match line.trim() {
            "yes" => return Ok(true),
            "no" => return Ok(false),
            s if s == fingerprint => return Ok(true),
            _ => write!(wtr, "Please type 'yes', 'no' or the fingerprint: ")?,
        }
    }
}

fn save_key_at(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<()> {
    use std::fs::{DirBuilder, OpenOptions};
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }

    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    f.lock()?;

    if f.metadata()?.len() != 0 {
        let mut last = [0];
        f.seek(SeekFrom::End(-1))?;
        f.read_exact(&mut last)?;

        if last[0] != b'\n' {
            f.write_all(b"\n")?;
        }
    }
    writeln!(f, "{} {}", host_port(host, port), key.to_openssh()?)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use openrussh_client::known_hosts::KnownHosts;
    use openrussh_client::russh::keys::PrivateKey;

    fn key() -> PrivateKey {
        use openrussh_client::russh::keys::Algorithm;

        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
    }

    fn public_key(key: &PrivateKey) -> PublicKeyOrCertificate {
        PublicKeyOrCertificate::PublicKey {
            key: key.public_key().clone(),
            hash_alg: None,
        }
    }

    fn handler(hosts: &str) -> MyHandler {
        MyHandler {
            known_hosts: KnownHosts::parse(hosts).handler("server.example", 22),
            save_path: Path::new("/unused-known-hosts").into(),
            accepted: None,
            host: "server.example".to_owned(),
            port: 22,
        }
    }

    #[test]
    fn host_names_include_only_nonstandard_ports() {
        for (host, port, expected) in [
            ("server.example", 22, "server.example"),
            ("server.example", 2222, "[server.example]:2222"),
            ("::1", 22, "::1"),
            ("::1", 2222, "[::1]:2222"),
        ] {
            assert_eq!(host_port(host, port).to_string(), expected);
        }
    }

    #[tokio::test]
    async fn revoked_key_takes_precedence_over_trust_and_confirmation() {
        let trusted = key();
        let public = trusted.public_key().to_openssh().unwrap();
        let hosts = format!("server.example {public}\n@revoked server.example {public}\n");
        let mut handler = handler(&hosts);
        handler.accepted = Some(trusted.public_key().clone());

        let e = handler
            .check_server_key(&public_key(&trusted))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("REVOKED HOST KEY"));
    }

    #[tokio::test]
    async fn confirmed_key_is_reused_without_accepting_a_different_key() {
        let trusted = key();
        let changed = key();
        let mut handler = handler("");
        handler.accepted = Some(trusted.public_key().clone());

        assert!(
            handler
                .check_server_key(&public_key(&trusted))
                .await
                .unwrap()
        );

        assert!(
            handler
                .check_server_key(&public_key(&trusted))
                .await
                .unwrap()
        );

        let e = handler
            .check_server_key(&public_key(&changed))
            .await
            .unwrap_err();
        assert!(
            e.to_string()
                .contains("REMOTE HOST IDENTIFICATION HAS CHANGED")
        );

        assert!(
            handler
                .check_server_key(&public_key(&trusted))
                .await
                .unwrap()
        );
    }

    #[test]
    fn factory_keeps_client_preferences_and_normalizes_the_host() {
        use openrussh_client::config::{Dest, Host};
        use openrussh_client::russh::cipher;

        use std::borrow::Cow;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected_hosts");
        let maker = MyMakeHandler::new(
            Preferred {
                cipher: Cow::Borrowed(&[cipher::AES_256_CTR]),
                ..Default::default()
            },
            KnownHostsFile::load(Some(&path)).unwrap(),
        );

        let host = Host {
            dest: Dest {
                name: "SERVER.Example".to_owned(),
                port: 2222,
                user: "alice".to_owned(),
            },
            auth: Default::default(),
            tty: Default::default(),
        };

        let (handler, conf) = maker.make_handler(&host).unwrap().into_inner();

        assert_eq!(handler.host, "server.example");

        assert_eq!(handler.port, 2222);

        assert_eq!(handler.save_path.as_ref(), path);

        assert!(handler.accepted.is_none());

        assert_eq!(conf.preferred.cipher.as_ref(), &[cipher::AES_256_CTR]);

        assert!(!conf.preferred.host_key_certificates.is_empty());
    }

    #[test]
    fn prompt_accepts_only_yes_or_the_full_fingerprint() {
        use openrussh_client::russh::keys::ssh_key::HashAlg;

        use std::io::Cursor;

        let key = key();
        let key = key.public_key();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();

        for (answer, expected, retries) in [
            ("yes\n".to_owned(), true, 0),
            (" yes \n".to_owned(), true, 0),
            (format!("{fingerprint}\n"), true, 0),
            (
                format!("{}\nno\n", &fingerprint[..fingerprint.len() - 1]),
                false,
                1,
            ),
            ("no\n".to_owned(), false, 0),
            (String::new(), false, 0),
            ("y\nYES\nno\n".to_owned(), false, 2),
            ("invalid\nyes\n".to_owned(), true, 1),
        ] {
            let mut input = Cursor::new(answer.as_bytes());
            let mut output = Vec::new();
            let accepted =
                prompt_key(&mut input, &mut output, "server.example", 2222, key).unwrap();
            let output = String::from_utf8(output).unwrap();

            assert_eq!(accepted, expected, "{answer:?}");

            assert!(output.contains("'[server.example]:2222'"));

            assert!(output.contains(&fingerprint));

            assert!(output.contains("(yes/no/[fingerprint])?"));

            assert_eq!(
                output
                    .matches("Please type 'yes', 'no' or the fingerprint:")
                    .count(),
                retries
            );
        }
    }

    #[test]
    fn prompt_propagates_io_errors() {
        use std::io::{Cursor, ErrorKind};

        struct BrokenInput;
        impl io::Read for BrokenInput {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::from(ErrorKind::BrokenPipe))
            }
        }
        impl BufRead for BrokenInput {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Err(io::Error::from(ErrorKind::BrokenPipe))
            }

            fn consume(&mut self, _: usize) {}
        }

        struct BrokenOutput;
        impl Write for BrokenOutput {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(ErrorKind::PermissionDenied))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let key = key();
        let e = prompt_key(
            &mut BrokenInput,
            &mut Vec::new(),
            "host",
            22,
            key.public_key(),
        )
        .unwrap_err();

        assert_eq!(e.kind(), ErrorKind::BrokenPipe);

        let e = prompt_key(
            &mut Cursor::new(b"yes\n"),
            &mut BrokenOutput,
            "host",
            22,
            key.public_key(),
        )
        .unwrap_err();

        assert_eq!(e.kind(), ErrorKind::PermissionDenied);
    }

    #[test]
    fn saved_keys_round_trip_and_keep_existing_content() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let key = key();
        let key = key.public_key();
        let public = key.to_openssh().unwrap();

        for existing in ["", "existing", "existing\n"] {
            let path = dir.path().join("known_hosts");
            fs::write(&path, existing).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            save_key_at(&path, "server.example", 2222, key).unwrap();

            let separator = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };

            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                format!("{existing}{separator}[server.example]:2222 {public}\n")
            );

            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );

            assert_eq!(
                KnownHosts::parse_path(&path)
                    .unwrap()
                    .handler("server.example", 2222)
                    .check_public_key(key),
                MatchResult::Found
            );
        }
    }

    #[test]
    fn new_known_hosts_paths_have_private_permissions() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh/known_hosts");
        let key = key();
        save_key_at(&path, "server.example", 22, key.public_key()).unwrap();

        assert_eq!(
            fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("server.example ssh-ed25519 ")
        );
    }

    #[test]
    fn concurrent_appends_do_not_interleave_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let key = key();
        let key = key.public_key();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..8 {
                        save_key_at(path, "server.example", 22, key).unwrap();
                    }
                });
            }
        });

        let contents = std::fs::read_to_string(path).unwrap();
        let expected = format!("server.example {}", key.to_openssh().unwrap());

        assert_eq!(contents.lines().count(), 64);

        assert!(contents.lines().all(|line| line == expected));
    }

    #[test]
    fn save_failures_are_not_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let key = key();

        assert!(save_key_at(dir.path(), "host", 22, key.public_key()).is_err());

        let parent = dir.path().join("file");
        std::fs::write(&parent, "").unwrap();

        assert!(save_key_at(&parent.join("known_hosts"), "host", 22, key.public_key()).is_err());
    }
}
