//! Typed failures from configuration loading, resolution, and transport preferences.
//!
//! [`Error`] distinguishes source IO and format errors from native path or
//! algorithm validation. [`Error::OpenConfig`], [`Error::ReadConfig`], and
//! [`Error::CanonicalizeLua`] add the selected path and operation; [`Error::Native`]
//! carries a successfully parsed native logging policy. [`Error::log_filter()`]
//! retrieves that policy through the surrounding file context.
//!
//! Ordinary [`std::fmt::Display`] reports the current operation or validation
//! message. Alternate display (`{error:#}`) also includes the causal messages,
//! separated by `: `. Use [`std::error::Error::source()`] to inspect causes without
//! parsing that text. The transparent format/IO variants and native-policy
//! wrapper delegate display and source handling to their contained error.
//!
//! ```
//! use openrussh_cli_config::Error;
//! use std::error::Error as _;
//! use std::io;
//!
//! let error = Error::OpenConfig {
//!     path: "missing.toml".into(),
//!     source: io::Error::new(io::ErrorKind::NotFound, "file absent"),
//! };
//! assert_eq!(error.to_string(), "Could not open SSH config missing.toml");
//! assert_eq!(error.source().unwrap().to_string(), "file absent");
//! assert_eq!(
//!     format!("{error:#}"),
//!     "Could not open SSH config missing.toml: file absent",
//! );
//! ```

use crate::LogFilter;

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::PathBuf;

/// The result of a configuration operation, with [`Error`] as its failure type.
///
/// This alias is also re-exported at the crate root. It covers configuration
/// building, [`crate::Config::preferred()`], [`LogFilter::try_from_filter()`], and
/// algorithm-policy lookup.
pub type Result<T> = std::result::Result<T, Error>;

/// A failure while building configuration or applying its transport settings.
///
/// File loading usually adds [`Self::OpenConfig`] or [`Self::ReadConfig`] context
/// around the underlying cause. Native validation adds [`Self::Native`] context
/// so a caller can honor [`Self::log_filter()`] even when configuration fails.
/// The enum is non-exhaustive: downstream matches must include a wildcard arm.
///
/// Match the typed context when an explicitly selected file does not exist:
///
/// ```
/// use openrussh_cli_config::{Config, Destination, Error, Session};
/// use std::io::ErrorKind;
///
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("missing.toml");
/// let error = Config::builder()
///     .config_file(&path)
///     .build(
///         Destination { host: "server.example".into(), user: None, port: None },
///         Session::Shell,
///     )
///     .err()
///     .expect("the selected file does not exist");
/// match error {
///     Error::OpenConfig { path: failed_path, source } => {
///         assert_eq!(failed_path, path);
///         assert_eq!(source.kind(), ErrorKind::NotFound);
///     }
///     _ => panic!("expected an explicit-file opening failure"),
/// }
/// # Ok::<(), openrussh_cli_config::Error>(())
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Automatic configuration discovery could not determine the home directory.
    HomeDirectory,
    /// An IO failure without a more specific operation/path wrapper.
    Io(
        /// The underlying IO error.
        io::Error,
    ),
    /// TOML syntax or native-schema deserialization failed.
    Toml(
        /// The TOML parser/deserializer's diagnostic.
        toml::de::Error,
    ),
    /// Lua setup, syntax, evaluation, or native-schema deserialization failed.
    Lua(
        /// The Lua runtime/deserializer's diagnostic.
        mlua::Error,
    ),
    /// The dependency rejected an OpenSSH-syntax configuration.
    OpenSshParse(
        /// The OpenSSH configuration parser's diagnostic.
        openrussh_client::ssh2_config::SshParserError,
    ),
    /// The dependency could not resolve an OpenSSH destination or jump chain.
    OpenSshResolve(
        /// The dependency's connection-configuration error.
        openrussh_client::config::Error,
    ),
    /// The OS username required for the default SSH account could not be obtained.
    Username(
        /// The username lookup failure.
        whoami::Error,
    ),
    /// [`LogFilter::try_from_filter()`] rejected logging filter directives.
    ///
    /// Direct calls to that helper return this variant. Invalid filters in
    /// configuration files fail during deserialization and instead surface as
    /// [`Self::Toml`] or [`Self::Lua`], usually inside [`Self::ReadConfig`]. None
    /// of those invalid-filter failures retains native log metadata.
    InvalidLogFilter(
        /// The tracing filter parser's diagnostic.
        tracing_subscriber::filter::ParseError,
    ),
    /// Opening an explicitly selected or discovered configuration file failed.
    ///
    /// Missing automatic-discovery candidates are skipped; a missing explicit
    /// file produces this error.
    OpenConfig {
        /// The selected file path, before any Lua canonicalization.
        path: PathBuf,
        /// The failure returned when opening the file.
        source: io::Error,
    },
    /// Reading, parsing, executing, or resolving a selected configuration failed.
    ///
    /// This wrapper retains file context and forwards [`Self::log_filter()`] to
    /// the contained error.
    ReadConfig {
        /// The selected file path.
        path: PathBuf,
        /// The underlying IO, format, or resolution failure.
        source: Box<Error>,
    },
    /// The selected Lua source path could not be canonicalized before execution.
    CanonicalizeLua {
        /// The source path supplied to canonicalization.
        path: PathBuf,
        /// The filesystem failure.
        source: io::Error,
    },
    /// Native validation/resolution failed after the profiles were deserialized.
    ///
    /// This transparent context preserves log metadata while delegating display
    /// and causal traversal to the wrapped failure. It also covers TOML's
    /// rejection of inline jump records after deserialization.
    Native {
        /// The parsed log policy, or `None` when the native file omitted it.
        log_filter: Option<LogFilter>,
        /// The validation or resolution failure.
        source: Box<Error>,
    },
    /// A path required to be absolute is empty or relative.
    InvalidPath {
        /// The semantic field name, such as `identity_file` or `known_hosts`.
        field: &'static str,
        /// The rejected path value.
        path: PathBuf,
    },
    /// A permitted current-user tilde path could not be expanded to an absolute path.
    PathExpansion {
        /// The semantic field name, independent of its spelling in the source.
        field: &'static str,
        /// The supplied path whose expansion failed.
        path: PathBuf,
    },
    /// A TOML jump setting contains an inline host record instead of names.
    TomlInlineJump,
    /// A selected inline Lua jump has no `host_name` of its own or in defaults.
    InlineJumpMissingHost,
    /// Native jump-host inheritance revisits the same configured route.
    ///
    /// Such a route cannot resolve to a finite connection chain.
    JumpCycle,
    /// A native KEX/host-key list is empty, or its operation removed every choice.
    EmptyAlgorithmList,
    /// A native algorithm name is empty or uses whitespace, wildcards, or other
    /// unsupported list syntax.
    InvalidAlgorithmName {
        /// The rejected name after removing the first entry's list modifier.
        name: String,
    },
    /// A native key-exchange name is unsupported by the enabled transport.
    UnknownKeyExchangeAlgorithm {
        /// The unsupported key-exchange method name.
        name: String,
    },
    /// A native plain-key or host-certificate name is unsupported.
    UnknownHostKeyAlgorithm {
        /// The unsupported host-key or certificate algorithm name.
        name: String,
    },
    /// [`crate::Config::preferred()`] encountered an unsupported cipher name.
    UnknownCipher {
        /// The rejected comma-separated entry, which may be empty.
        name: String,
    },
    /// [`crate::Config::preferred()`] encountered an unsupported MAC name.
    UnknownMac {
        /// The rejected comma-separated entry, which may be empty.
        name: String,
    },
    /// A native host-key preference places a certificate after a plain key.
    ///
    /// The transport advertises certificates before plain keys and cannot
    /// represent this ordering.
    HostCertificateOrder,
    /// Policy lookup received a host reference outside the supplied chain.
    ///
    /// [`crate::ConnectionConfig::algorithm_policy()`] checks reference identity, so an equal
    /// cloned host also produces this error.
    HostNotInChain,
    /// A chain's bastion index has no corresponding entry in its policy array.
    MissingAlgorithmPolicy,
}

impl Error {
    /// Returns a native log policy retained on a later configuration failure.
    ///
    /// This traverses [`Self::ReadConfig`] context to [`Self::Native`]. A supplied
    /// `LogFilter::Disabled` remains `Some(&LogFilter::Disabled)`, so callers can distinguish
    /// explicit disabling from an omitted policy. Reading, format execution,
    /// deserialization, and invalid-filter failures have no parsed policy;
    /// successful native deserialization preserves it on later path, algorithm,
    /// jump-shape, or host-resolution errors. OpenSSH errors have no native policy.
    ///
    /// ```
    /// use openrussh_cli_config::{Config, Destination, LogFilter, Session};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("config.toml");
    /// std::fs::write(
    ///     &path,
    ///     "[meta]\nlog-level = 'none'\n[hosts.demo]\nidentity-file = 'relative'\n",
    /// )?;
    /// let error = Config::builder()
    ///     .config_file(&path)
    ///     .build(
    ///         Destination { host: "demo".into(), user: None, port: None },
    ///         Session::Shell,
    ///     )
    ///     .err()
    ///     .expect("relative native identity paths must fail");
    /// assert!(matches!(error.log_filter(), Some(LogFilter::Disabled)));
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    pub fn log_filter(&self) -> Option<&LogFilter> {
        match self {
            Self::Native { log_filter, .. } => log_filter.as_ref(),
            Self::ReadConfig { source, .. } => source.log_filter(),
            _ => None,
        }
    }

    /// Takes a retained native log policy, leaving its slot empty.
    ///
    /// Like [`Self::log_filter()`], this traverses selected-file context to the
    /// native policy. The error, filename, and source chain remain intact. A
    /// second call returns `None`; errors without a native policy also return
    /// `None`. This transfers ownership without cloning the parsed filter.
    pub fn take_log_filter(&mut self) -> Option<LogFilter> {
        match self {
            Self::Native { log_filter, .. } => log_filter.take(),
            Self::ReadConfig { source, .. } => source.take_log_filter(),
            _ => None,
        }
    }

    pub(crate) fn with_log_filter(self, log_filter: Option<LogFilter>) -> Self {
        Self::Native {
            log_filter,
            source: Box::new(self),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HomeDirectory => f.write_str("Could not find the home directory"),
            Self::Io(error) => fmt::Display::fmt(error, f),
            Self::Toml(error) => fmt::Display::fmt(error, f),
            Self::Lua(error) => fmt::Display::fmt(error, f),
            Self::OpenSshParse(error) => fmt::Display::fmt(error, f),
            Self::OpenSshResolve(_) => f.write_str("Could not resolve the SSH connection settings"),
            Self::Username(error) => fmt::Display::fmt(error, f),
            Self::InvalidLogFilter(error) => fmt::Display::fmt(error, f),
            Self::OpenConfig { path, .. } => {
                write!(f, "Could not open SSH config {}", path.display())
            }
            Self::ReadConfig { path, .. } => {
                write!(f, "Could not read SSH config {}", path.display())
            }
            Self::CanonicalizeLua { path, .. } => {
                write!(f, "Could not canonicalize Lua config {}", path.display())
            }
            Self::Native { source, .. } => return fmt::Display::fmt(source, f),
            Self::InvalidPath { field, path } => {
                write!(f, "{field} must be an absolute path: {}", path.display())
            }
            Self::PathExpansion { field, path } => write!(
                f,
                "{field} must expand to an absolute path: {}",
                path.display()
            ),
            Self::TomlInlineJump => {
                f.write_str("TOML proxy-jump must be a string or an array of strings")
            }
            Self::InlineJumpMissingHost => f.write_str("Inline proxy_jump host requires host_name"),
            Self::JumpCycle => f.write_str("Proxy jump route contains an inheritance cycle"),
            Self::EmptyAlgorithmList => f.write_str("Algorithm list must not be empty"),
            Self::InvalidAlgorithmName { name } => write!(
                f,
                "Invalid algorithm name (wildcards are not supported): {name}"
            ),
            Self::UnknownKeyExchangeAlgorithm { name } => {
                write!(f, "Unknown key-exchange algorithm: {name}")
            }
            Self::UnknownHostKeyAlgorithm { name } => {
                write!(f, "Unknown host-key algorithm: {name}")
            }
            Self::UnknownCipher { name } => write!(f, "Unknown cipher: {name}"),
            Self::UnknownMac { name } => write!(f, "Unknown MAC: {name}"),
            Self::HostCertificateOrder => {
                f.write_str("Host certificate algorithms must precede plain host-key algorithms")
            }
            Self::HostNotInChain => {
                f.write_str("SSH host does not belong to the configured connection chain")
            }
            Self::MissingAlgorithmPolicy => f.write_str("Missing algorithm policy for SSH hop"),
        }?;

        if f.alternate() {
            let mut source = self.source();
            while let Some(error) = source {
                write!(f, ": {error}")?;
                source = error.source();
            }
        }
        Ok(())
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Io(error) => error.source(),
            Self::Toml(error) => error.source(),
            Self::Lua(error) => error.source(),
            Self::OpenSshParse(error) => error.source(),
            Self::OpenSshResolve(error) => Some(error),
            Self::Username(error) => error.source(),
            Self::InvalidLogFilter(error) => error.source(),
            Self::OpenConfig { source, .. } | Self::CanonicalizeLua { source, .. } => Some(source),
            Self::ReadConfig { source, .. } => Some(source.as_ref()),
            Self::Native { source, .. } => source.source(),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<toml::de::Error> for Error {
    fn from(error: toml::de::Error) -> Self {
        Self::Toml(error)
    }
}

impl From<mlua::Error> for Error {
    fn from(error: mlua::Error) -> Self {
        Self::Lua(error)
    }
}

impl From<openrussh_client::ssh2_config::SshParserError> for Error {
    fn from(error: openrussh_client::ssh2_config::SshParserError) -> Self {
        Self::OpenSshParse(error)
    }
}

impl From<whoami::Error> for Error {
    fn from(error: whoami::Error) -> Self {
        Self::Username(error)
    }
}
