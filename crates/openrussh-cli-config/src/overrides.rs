//! Explicit invocation overrides, independent of the selected file format.

use crate::algorithms::AlgorithmList;
use crate::{Destination, Error, LogFilter, Resolved, Result};
use openrussh_client::config::Dest;

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Component, Path, PathBuf};

/// Address family for direct target sockets and local TCP listeners.
///
/// Jump connections and server-side forwarding retain their own resolution policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AddressFamily {
    /// Permit IPv4 and IPv6 addresses.
    #[default]
    Any,
    /// Permit IPv4 addresses only.
    Inet,
    /// Permit IPv6 addresses only.
    Inet6,
}

/// Explicit authentication-agent selection for the final destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IdentityAgent {
    /// Disable agent authentication, including any inherited socket.
    Disabled,
    /// Select a socket path, `SSH_AUTH_SOCK`, or a `$VARIABLE` selector.
    Socket(PathBuf),
}

/// Overrides applied to the final target after resolving the selected source.
///
/// Omitted fields retain the source's values. The original destination alias
/// continues to select file records and jump hosts. Paths use invocation rules:
/// relative paths are accepted, current-user tildes and `${VARIABLE}` expand,
/// and `%%`, `%d`, `%h`, `%k`, `%L`, `%l`, `%n`, `%p`, `%r`, and `%u` expand.
/// Unsupported percent tokens are rejected. These
/// rules apply equally to OpenSSH, TOML, Lua, and defaults-only configuration.
#[derive(Debug, Default)]
pub struct HostOverrides {
    /// Network hostname; `%%` and `%h` expand, with `%h` naming the original alias.
    pub host_name: Option<String>,
    /// Identity paths prepended to file identities; `none` suppresses built-in defaults.
    pub identity_files: Option<Vec<PathBuf>>,
    /// Authentication certificate; the exact path `none` disables certificates.
    pub certificate_file: Option<PathBuf>,
    /// Explicit agent selection, distinct from inheriting the source policy.
    pub identity_agent: Option<IdentityAgent>,
    /// Restrict authentication to the configured identity files.
    pub identities_only: Option<bool>,
    /// Comma-separated KEX preference, using the native algorithm-list rules.
    pub kex_algorithms: Option<String>,
    /// Comma-separated host-key preference, using the native algorithm-list rules.
    pub host_key_algorithms: Option<String>,
    /// Known-host database path; the exact path `none` selects `/dev/null`.
    pub known_hosts: Option<PathBuf>,
    /// Invocation logging policy, replacing native setup metadata.
    pub log_filter: Option<LogFilter>,
}

impl HostOverrides {
    pub(super) fn apply(self, resolved: &mut Resolved, destination: &Destination) -> Result<()> {
        if let Some(filter) = self.log_filter {
            resolved.meta.log_filter = Some(filter);
        }
        if let Some(name) = self.host_name {
            resolved.chain.target.dest.name = expand_hostname(name, &destination.host)?;
        }

        if let Some(mut identities) = self.identity_files {
            identities.retain(|path| path != Path::new("none"));
            for path in &mut identities {
                *path = expand_path(std::mem::take(path), "identity_file", resolved, destination)?;
            }
            identities.extend(
                resolved
                    .chain
                    .target
                    .auth
                    .identities
                    .take()
                    .into_iter()
                    .flatten(),
            );
            resolved.chain.target.auth.identities = Some(identities);
        }
        if let Some(path) = self.certificate_file {
            let path = if path == Path::new("none") {
                None
            } else {
                Some(expand_path(
                    path,
                    "certificate_file",
                    resolved,
                    destination,
                )?)
            };
            resolved.chain.target.auth.cert = path;
        }
        if let Some(agent) = self.identity_agent {
            resolved.chain.target.auth.agent = match agent {
                IdentityAgent::Disabled => None,
                IdentityAgent::Socket(path) => {
                    let bytes = path.as_os_str().as_bytes();
                    let variable = if bytes == b"SSH_AUTH_SOCK" {
                        Some(OsStr::new("SSH_AUTH_SOCK"))
                    } else if bytes.starts_with(b"$") && !bytes.starts_with(b"${") {
                        Some(OsStr::from_bytes(&bytes[1..]))
                    } else {
                        None
                    };
                    match variable {
                        Some(variable) => {
                            let name = variable.as_bytes();
                            if name.is_empty()
                                || !name
                                    .iter()
                                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                            {
                                return Err(invalid(
                                    "identity_agent",
                                    "invalid environment-variable selector",
                                ));
                            }
                            std::env::var_os(variable)
                                .filter(|value| !value.is_empty())
                                .map(PathBuf::from)
                        }
                        None => Some(expand_path(path, "identity_agent", resolved, destination)?),
                    }
                }
            };
        }
        if let Some(value) = self.identities_only {
            resolved.chain.target.auth.identities_only = value;
        }
        if let Some(names) = self.kex_algorithms {
            resolved.algorithms.target.kex = Some(Names(&names).kex()?);
        }
        if let Some(names) = self.host_key_algorithms {
            resolved.algorithms.target.host_keys = Some(Names(&names).host_keys()?);
        }
        if let Some(path) = self.known_hosts {
            resolved.meta.known_hosts = Some(if path == Path::new("none") {
                PathBuf::from("/dev/null")
            } else {
                expand_path(path, "known_hosts", resolved, destination)?
            });
        }
        Ok(())
    }
}

struct Names<'a>(&'a str);

impl AlgorithmList for Names<'_> {
    fn algorithm_names(&self) -> impl Iterator<Item = &str> + Clone {
        self.0.split(',')
    }
}

fn invalid(field: &'static str, reason: &str) -> Error {
    Error::InvalidOverride {
        field,
        reason: reason.to_owned(),
    }
}

fn expand_hostname(name: String, original_host: &str) -> Result<String> {
    if name.is_empty() || name.as_bytes().contains(&0) {
        return Err(invalid(
            "host_name",
            "hostname is empty or contains a NUL byte",
        ));
    }
    if !name.contains('%') {
        return Ok(name);
    }
    let mut expanded = String::with_capacity(name.len());
    let mut chars = name.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            expanded.push(ch);
            continue;
        }
        match chars.next() {
            Some('%') => expanded.push('%'),
            Some('h') => expanded.push_str(original_host),
            _ => {
                return Err(invalid(
                    "host_name",
                    "unsupported or incomplete percent token",
                ));
            }
        }
    }
    if expanded.is_empty() || expanded.as_bytes().contains(&0) {
        return Err(invalid(
            "host_name",
            "expanded hostname is empty or contains a NUL byte",
        ));
    }
    Ok(expanded)
}

fn expand_path(
    path: PathBuf,
    field: &'static str,
    resolved: &Resolved,
    destination: &Destination,
) -> Result<PathBuf> {
    let path = expand_tilde(path, field)?;
    let input = path.as_os_str().as_bytes();
    if input.is_empty() || input.contains(&0) {
        return Err(invalid(field, "path is empty or contains a NUL byte"));
    }
    if !input.contains(&b'%') && !input.windows(2).any(|bytes| bytes == b"${") {
        return Ok(path);
    }
    let mut output = Vec::with_capacity(input.len());
    let mut position = 0;
    while position < input.len() {
        match input[position] {
            b'$' if input.get(position + 1) == Some(&b'{') => {
                let rest = &input[position + 2..];
                let end = rest
                    .iter()
                    .position(|byte| *byte == b'}')
                    .ok_or_else(|| invalid(field, "unterminated environment variable"))?;
                let name = OsStr::from_bytes(&rest[..end]);
                let value = std::env::var_os(name)
                    .ok_or_else(|| invalid(field, "referenced environment variable is not set"))?;
                output.extend_from_slice(value.as_bytes());
                position += end + 3;
            }
            b'%' => {
                let token = input
                    .get(position + 1)
                    .ok_or_else(|| invalid(field, "incomplete percent token"))?;
                append_token(
                    &mut output,
                    *token,
                    field,
                    &resolved.chain.target.dest,
                    &destination.host,
                )?;
                position += 2;
            }
            byte => {
                output.push(byte);
                position += 1;
            }
        }
    }
    if output.is_empty() {
        return Err(invalid(field, "expanded path is empty"));
    }
    Ok(PathBuf::from(OsString::from_vec(output)))
}

pub(super) fn expand_command(command: String, host: &Dest, original_host: &str) -> Result<String> {
    if !command.contains('%') {
        return Ok(command);
    }
    let mut output = Vec::with_capacity(command.len());
    let mut bytes = command.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let token = bytes
                .next()
                .ok_or_else(|| invalid("remote_command", "incomplete percent token"))?;
            append_token(&mut output, token, "remote_command", host, original_host)?;
        } else {
            output.push(byte);
        }
    }
    String::from_utf8(output)
        .map_err(|_| invalid("remote_command", "expanded command is not UTF-8"))
}

fn append_token(
    output: &mut Vec<u8>,
    token: u8,
    field: &'static str,
    host: &Dest,
    original_host: &str,
) -> Result<()> {
    match token {
        b'%' => output.push(b'%'),
        b'h' => output.extend_from_slice(host.name.as_bytes()),
        b'n' | b'k' => output.extend_from_slice(original_host.as_bytes()),
        b'p' => write!(output, "{}", host.port)?,
        b'r' => output.extend_from_slice(host.user.as_bytes()),
        b'd' => output.extend_from_slice(
            std::env::home_dir()
                .ok_or(Error::HomeDirectory)?
                .as_os_str()
                .as_bytes(),
        ),
        b'u' => output.extend_from_slice(whoami::username()?.as_bytes()),
        b'l' | b'L' => {
            let name = nix::unistd::gethostname().map_err(std::io::Error::from)?;
            if token == b'L' {
                output.extend_from_slice(
                    name.as_bytes()
                        .split(|byte| *byte == b'.')
                        .next()
                        .unwrap_or_default(),
                );
            } else {
                output.extend_from_slice(name.as_bytes());
            }
        }
        _ => return Err(invalid(field, "unsupported percent token")),
    }
    Ok(())
}

fn expand_tilde(path: PathBuf, field: &'static str) -> Result<PathBuf> {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return Ok(path);
    };
    if !first.as_bytes().starts_with(b"~") {
        return Ok(path);
    }
    if first != "~" {
        let user = first.to_str().and_then(|value| value.strip_prefix('~'));
        if user != Some(whoami::username()?.as_str()) {
            return Err(Error::PathExpansion { field, path });
        }
    }
    let mut home = std::env::home_dir().ok_or(Error::HomeDirectory)?;
    home.push(components.as_path());
    Ok(home)
}
