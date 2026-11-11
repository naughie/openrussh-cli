use openrussh_client::known_hosts::{KnownHosts, KnownHostsHandler};

use anyhow::{Context as _, Result};

use std::path::Path;
use std::sync::Arc;

pub(crate) struct KnownHostsFile {
    known_hosts: KnownHosts,
    path: Arc<Path>,
}

impl KnownHostsFile {
    pub(crate) fn load(path: Option<&Path>) -> Result<Self> {
        use std::io::ErrorKind;

        let selected = path;
        let path = path.map(Path::to_path_buf).unwrap_or_else(|| {
            if let Some(mut home) = std::env::home_dir() {
                home.push(".ssh/known_hosts");
                home
            } else {
                "/etc/ssh/ssh_known_hosts".into()
            }
        });
        let loaded = match selected {
            Some(path) => KnownHosts::parse_path(path),
            None => KnownHosts::parse_default_path(),
        };
        let known_hosts = match loaded {
            Ok(known_hosts) => known_hosts,
            Err(e) if e.kind() == ErrorKind::NotFound => KnownHosts::parse(""),
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("Failed to read known_hosts {}", path.display()));
            }
        };

        Ok(Self {
            known_hosts,
            path: path.into(),
        })
    }

    pub(crate) fn handler(&self, host: &str, port: u16) -> KnownHostsHandler {
        self.known_hosts.handler(host, port)
    }

    pub(crate) fn save_path(&self) -> &Arc<Path> {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use openrussh_client::known_hosts::MatchResult;
    use openrussh_client::russh::keys::{Algorithm, PrivateKey};

    #[test]
    fn loads_keys_from_the_selected_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selected_hosts");
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        std::fs::write(
            &path,
            format!(
                "[server.example]:2222 {}\n",
                key.public_key().to_openssh().unwrap()
            ),
        )
        .unwrap();
        let file = KnownHostsFile::load(Some(&path)).unwrap();

        assert_eq!(file.save_path().as_ref(), path);

        assert_eq!(
            file.handler("server.example", 2222)
                .check_public_key(key.public_key()),
            MatchResult::Found
        );

        assert_eq!(
            file.handler("server.example", 22)
                .check_public_key(key.public_key()),
            MatchResult::NotFound
        );
    }

    #[test]
    fn missing_file_starts_empty_without_creating_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ssh/selected_hosts");
        let file = KnownHostsFile::load(Some(&path)).unwrap();
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();

        assert_eq!(file.save_path().as_ref(), path);

        assert_eq!(
            file.handler("server.example", 22)
                .check_public_key(key.public_key()),
            MatchResult::NotFound
        );

        assert!(!path.exists());
    }

    #[test]
    fn load_errors_identify_the_selected_path() {
        let dir = tempfile::tempdir().unwrap();
        let e = match KnownHostsFile::load(Some(dir.path())) {
            Ok(_) => panic!("directory should not load as known_hosts"),
            Err(e) => e,
        };

        assert!(e.to_string().contains("Failed to read known_hosts"));

        assert!(e.to_string().contains(&dir.path().display().to_string()));
    }
}
