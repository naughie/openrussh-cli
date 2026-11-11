use super::PathMode;

use anyhow::{Context as _, Result, bail, ensure};

use std::path::{Path, PathBuf};

impl PathMode {
    pub(super) fn validate(self, path: &Path, field: &str) -> Result<()> {
        let tilde = matches!(path.components().next(), Some(std::path::Component::Normal(s))
            if s.as_encoded_bytes().first() == Some(&b'~'));
        ensure!(
            path.is_absolute() || (matches!(self, Self::Expand) && tilde),
            "{field} must be an absolute path: {}",
            path.display()
        );
        Ok(())
    }

    pub(super) fn validate_agent(self, value: &str) -> Result<()> {
        if matches!(self, Self::Expand)
            && (matches!(value, "none" | "SSH_AUTH_SOCK") || value.starts_with('$'))
        {
            return Ok(());
        }
        self.validate(Path::new(value), "identity_agent")
    }

    pub(super) fn path(self, path: &Path, field: &str) -> Result<PathBuf> {
        self.validate(path, field)?;
        if path.is_absolute() {
            return Ok(path.to_owned());
        }

        if matches!(self, Self::Expand) {
            let first = path.components().next();
            if matches!(first, Some(std::path::Component::Normal(s)) if s.as_encoded_bytes().first() == Some(&b'~'))
            {
                let expanded = cli_path(path).with_context(|| {
                    format!(
                        "{field} must expand to an absolute path: {}",
                        path.display()
                    )
                })?;
                ensure!(
                    expanded.is_absolute(),
                    "{field} must be an absolute path: {}",
                    path.display()
                );
                return Ok(expanded);
            }
        }

        bail!("{field} must be an absolute path: {}", path.display())
    }

    pub(super) fn agent(self, value: Option<&str>) -> Result<Option<PathBuf>> {
        if matches!(self, Self::Literal) {
            return value
                .map(|s| self.path(Path::new(s), "identity_agent"))
                .transpose();
        }

        let env = |key: &str| -> Result<Option<PathBuf>> {
            std::env::var_os(key)
                .map(|p| self.path(Path::new(&p), "identity_agent"))
                .transpose()
        };

        if let Some(value) = value {
            let explicit = match value {
                "none" => None,
                "SSH_AUTH_SOCK" => env("SSH_AUTH_SOCK")?,
                s => match s.strip_prefix('$') {
                    Some(key) => env(key)?,
                    None => Some(self.path(Path::new(s), "identity_agent")?),
                },
            };

            if explicit.is_some() {
                return Ok(explicit);
            }
        }

        env("SSH_AUTH_SOCK")
    }
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
    fn only_toml_expands_home_paths() -> Result<()> {
        let home = std::env::home_dir().context("Missing home directory")?;
        let user = whoami::username()?;

        for path in ["~/key".to_owned(), format!("~{user}/key")] {
            assert_eq!(
                PathMode::Expand.path(Path::new(&path), "identity_file")?,
                home.join("key")
            );
            assert!(
                PathMode::Literal
                    .path(Path::new(&path), "identity_file")
                    .is_err()
            );
        }

        Ok(())
    }

    #[test]
    fn both_modes_reject_relative_paths_and_preserve_absolute_paths() -> Result<()> {
        for mode in [PathMode::Expand, PathMode::Literal] {
            for field in ["identity_file", "certificate_file", "identity_agent"] {
                for path in ["", "key", "./key", "../key", "$HOME/key"] {
                    assert!(
                        mode.path(Path::new(path), field).is_err(),
                        "{field}: {path}"
                    );
                }

                assert_eq!(
                    mode.path(Path::new("/keys/key"), field)?,
                    Path::new("/keys/key")
                );
            }
        }

        Ok(())
    }
}
