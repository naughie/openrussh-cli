use crate::{Error, Result};

use std::path::{Path, PathBuf};

pub(super) fn validate(path: &Path, field: &'static str) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::InvalidPath {
            field,
            path: path.to_owned(),
        });
    }
    Ok(())
}

pub(super) fn validate_agent(value: &str) -> Result<()> {
    validate(Path::new(value), "identity_agent")
}

pub(super) fn path(path: &Path, field: &'static str) -> Result<PathBuf> {
    validate(path, field)?;
    Ok(path.to_owned())
}

pub(super) fn agent(value: Option<&str>) -> Result<Option<PathBuf>> {
    value
        .map(|value| path(Path::new(value), "identity_agent"))
        .transpose()
}

pub(super) fn cli_path(path: &Path) -> Option<PathBuf> {
    use std::path::Component;

    let mut parts = path.components();
    let Some(Component::Normal(first)) = parts.next() else {
        return (!path.as_os_str().is_empty()).then(|| path.to_owned());
    };
    if first != "~" {
        if first.as_encoded_bytes().first() != Some(&b'~') {
            return Some(path.to_owned());
        }
        let user = first.to_str()?.strip_prefix('~')?;
        if whoami::username().ok().as_deref() != Some(user) {
            return None;
        }
    }

    let mut expanded = std::env::home_dir()?;
    expanded.push(parts.as_path());
    Some(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_paths_are_absolute_literals() -> Result<()> {
        for field in [
            "identity_file",
            "certificate_file",
            "identity_agent",
            "known_hosts",
        ] {
            for value in ["", "key", "./key", "../key", "~/key", "$HOME/key"] {
                assert!(path(Path::new(value), field).is_err(), "{field}: {value}");
            }
            assert_eq!(
                path(Path::new("/keys/$HOME/~key"), field)?,
                Path::new("/keys/$HOME/~key")
            );
        }
        Ok(())
    }

    #[test]
    fn agent_values_require_an_explicit_absolute_path() -> Result<()> {
        assert_eq!(agent(None)?, None);
        assert_eq!(
            agent(Some("/sockets/agent.sock"))?,
            Some("/sockets/agent.sock".into())
        );
        for value in [
            "none",
            "SSH_AUTH_SOCK",
            "$SSH_AUTH_SOCK",
            "~/agent.sock",
            "agent.sock",
        ] {
            assert!(agent(Some(value)).is_err(), "{value}");
        }
        Ok(())
    }
}
