use crate::{Error, Result};

use std::path::{Path, PathBuf};

pub(super) fn validate(path: &Path, field: &'static str) -> Result<()> {
    let tilde = matches!(path.components().next(), Some(std::path::Component::Normal(value))
        if value.as_encoded_bytes().first() == Some(&b'~'));
    if !path.is_absolute() && !tilde {
        return Err(Error::InvalidPath {
            field,
            path: path.to_owned(),
        });
    }
    Ok(())
}

pub(super) fn validate_agent(value: &str) -> Result<()> {
    if matches!(value, "none" | "SSH_AUTH_SOCK") || value.starts_with('$') {
        return Ok(());
    }
    validate(Path::new(value), "identity_agent")
}

pub(super) fn path(path: &Path, field: &'static str) -> Result<PathBuf> {
    validate(path, field)?;
    if path.is_absolute() {
        return Ok(path.to_owned());
    }

    let expanded = cli_path(path).ok_or_else(|| Error::PathExpansion {
        field,
        path: path.to_owned(),
    })?;
    if !expanded.is_absolute() {
        return Err(Error::InvalidPath {
            field,
            path: path.to_owned(),
        });
    }
    Ok(expanded)
}

pub(super) fn agent(value: Option<&str>) -> Result<Option<PathBuf>> {
    let env = |key: &str| -> Result<Option<PathBuf>> {
        std::env::var_os(key)
            .map(|value| path(Path::new(&value), "identity_agent"))
            .transpose()
    };

    if let Some(value) = value {
        let explicit = match value {
            "none" => None,
            "SSH_AUTH_SOCK" => env("SSH_AUTH_SOCK")?,
            value => match value.strip_prefix('$') {
                Some(key) => env(key)?,
                None => Some(path(Path::new(value), "identity_agent")?),
            },
        };
        if explicit.is_some() {
            return Ok(explicit);
        }
    }

    env("SSH_AUTH_SOCK")
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
    fn expands_the_current_users_home() -> Result<()> {
        let home = std::env::home_dir().ok_or(Error::HomeDirectory)?;
        let user = whoami::username()?;

        for value in ["~/key".to_owned(), format!("~{user}/key")] {
            assert_eq!(path(Path::new(&value), "identity_file")?, home.join("key"));
        }

        Ok(())
    }

    #[test]
    fn rejects_relative_paths_and_preserves_absolute_paths() -> Result<()> {
        for field in ["identity_file", "certificate_file", "identity_agent"] {
            for value in ["", "key", "./key", "../key", "$HOME/key"] {
                assert!(path(Path::new(value), field).is_err(), "{field}: {value}");
            }
            assert_eq!(path(Path::new("/keys/key"), field)?, Path::new("/keys/key"));
        }

        Ok(())
    }
}
