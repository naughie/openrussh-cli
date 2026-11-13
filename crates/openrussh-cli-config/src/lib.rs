//! Load SSH configuration from OpenSSH, TOML, or Lua into owned runtime settings.
//!
//! Start with [`Config::builder()`] or [`ConfigBuilder::new()`], set optional sources
//! and overrides, then supply the required [`Destination`] and [`Session`] to
//! [`ConfigBuilder::build()`]. Builder methods consume and return the builder;
//! calling a setter again replaces that builder setting.
//!
//! Building configuration reads and resolves settings synchronously. It does not
//! connect to SSH servers, open authentication keys, bind listeners, load the
//! known-host database, or initialize logging. The caller uses the returned
//! [`Config`] to perform those operations. Failures use this crate's [`Error`]
//! and [`Result`] types.
//!
//! # Build without a configuration file
//!
//! The exact config path `none` bypasses file discovery. Explicit user and port
//! values override file settings when a file is selected instead.
//!
//! ```
//! use openrussh_cli_config::{Config, Destination, Session};
//!
//! let config = Config::builder()
//!     .config_file("none")
//!     .build(
//!         Destination {
//!             host: "example.com".into(),
//!             user: Some("alice".into()),
//!             port: Some(2222),
//!         },
//!         Session::Exec { command: "uname -a".into() },
//!     )?;
//!
//! assert_eq!(config.connection.chain().target.dest.name, "example.com");
//! assert_eq!(config.connection.chain().target.dest.port, 2222);
//! let preferred = config.preferred()?;
//! # let _ = preferred;
//! # Ok::<(), openrussh_cli_config::Error>(())
//! ```
//!
//! # Select a source
//!
//! [`ConfigBuilder::config_file()`] selects a file by its case-sensitive extension:
//!
//! | Extension | Format |
//! | --- | --- |
//! | `.toml` | Native TOML profiles. |
//! | `.lua` | A Lua 5.4 chunk returning native profiles. |
//! | Any other extension, or none | OpenSSH syntax through `openrussh-client`. |
//!
//! Without an explicit path, discovery tries `~/.ssh/config.lua`,
//! `~/.ssh/config.toml`, then `~/.ssh/config`. Only a missing file permits trying
//! the next one. Selected files are not merged, and a parsing or resolution
//! error does not cause fallback. No files means built-in defaults; automatic
//! discovery requires a home directory. `/etc/ssh/ssh_config` is not discovered.
//!
//! An OpenSSH-syntax file selected with `.config_file("connections")` can use
//! host blocks such as:
//!
//! ```text
//! Host work
//!     HostName work.example.com
//!     User alice
//!     Port 2222
//! ```
//!
//! # Native profiles
//!
//! TOML and Lua independently parse optional top-level sections named `meta`,
//! `default` (also `defaults`), and `hosts`. Each currently treats host aliases
//! as exact and case sensitive, accepts kebab-case and snake_case field
//! spellings, and rejects unknown fields. Each format owns its schema and
//! resolution rules; matching behavior does not require a shared implementation.
//!
//! For example, `connections.toml` can contain:
//!
//! ```toml
//! [meta]
//! log-level = "warn"
//!
//! [default]
//! identities-only = true
//!
//! [hosts.work]
//! host-name = "work.example.com"
//! user = "alice"
//! port = 2222
//! identity-file = "/keys/work"
//! proxy-jump = ["gateway"]
//!
//! [hosts.gateway]
//! host-name = "gateway.example.com"
//! user = "jump-user"
//! ```
//!
//! Select its alias at finalization:
//!
//! ```no_run
//! use openrussh_cli_config::{Config, Destination, Session};
//!
//! let config = Config::builder().config_file("connections.toml").build(
//!     Destination { host: "work".into(), user: None, port: None },
//!     Session::Shell,
//! )?;
//! assert_eq!(config.connection.chain().target.dest.name, "work.example.com");
//! # Ok::<(), openrussh_cli_config::Error>(())
//! ```
//!
//! A `.lua` file computes and returns its own profile table. For example,
//! this configuration constructs an identity path explicitly:
//!
//! ```lua
//! local cli = require("cli")
//! return {
//!     hosts = {
//!         work = {
//!             host_name = "work.example.com",
//!             user = "alice",
//!             identity_file = assert(cli.home_dir()) .. "/.ssh/work",
//!             identity_agent = os.getenv("SSH_AUTH_SOCK"),
//!         },
//!     },
//! }
//! ```
//!
//! Native scalar settings use supplied overrides, then host values, then defaults.
//! Identity lists accumulate in override/host/default order. Jump lists replace
//! inherited lists; an empty list clears them. TOML expands supported home paths
//! and agent environment selectors. Lua paths are literal, must be absolute, and
//! have no implicit agent environment lookup. Relative builder identity paths
//! remain allowed. OpenSSH files retain the dependency's own matching, path, and
//! merge behavior.
//!
//! Session, stdin, escape, forwarding, compression, cipher, and MAC settings come
//! from build arguments or builder setters, not from the file. Native profiles
//! can also select per-host KEX and host-key policies; see [`algorithms`]. For the
//! full file schema and Lua module reference, see the repository's
//! [configuration reference](https://github.com/naughie/openrussh-cli/blob/main/docs/configuration.md).
//!
//! # Consume the result
//!
//! [`Config::connection`] owns the dependency's connection chain together with
//! its per-host algorithm policies. [`ConnectionConfig::chain()`] borrows the
//! chain and [`ConnectionConfig::algorithm_policy()`] looks up each member's
//! policy by reference identity. [`Config::preferred()`] produces base transport
//! preferences. [`Config::meta`] provides setup metadata; errors can retain
//! parsed logging settings through [`Error::log_filter()`].

use openrussh_client::config::{Chain, Host};
use openrussh_client::russh::Preferred;

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;

mod defaults;
mod lua;
mod openssh;
mod toml;

pub mod algorithms;

mod builder;
pub use builder::ConfigBuilder;

mod overrides;
pub use overrides::{AddressFamily, HostOverrides, IdentityAgent};

pub mod error;
use algorithms::{Algorithms, Policies};
pub use error::{Error, Result};

/// Owned connection settings and application choices for one invocation.
///
/// Construct this through [`Config::builder()`] and [`ConfigBuilder::build()`]. The
/// result is independent of source text, parser state, and any Lua VM. Its public
/// fields describe work for the caller; constructing it performs no SSH or
/// application setup. See the [crate-level example](crate) for construction.
pub struct Config {
    /// Resolved connection chain and the policies associated with its hosts.
    pub connection: ConnectionConfig,
    /// Invocation-wide logging and known-host setup metadata.
    pub meta: Meta,
    /// Whether the application should enable its configured verbose diagnostics.
    ///
    /// Defaults to `false`. This crate does not initialize a logging subscriber.
    pub verbose: bool,
    /// Remote operation supplied to [`ConfigBuilder::build()`].
    pub session: Session,
    /// Whether session input should send EOF without reading local stdin.
    ///
    /// Defaults to `false`; remote output can still be read when this is `true`.
    pub stdin_null: bool,
    /// Interactive escape prefix, defaulting to `Some(b'~')`.
    ///
    /// `None` disables escape interpretation. The application uses this only
    /// for interactive sessions, excluding [`Session::Forward`].
    pub escape_char: Option<u8>,
    /// Ordered local listener specifications for the application to start.
    ///
    /// `None` and an empty vector both request no listeners. Building this
    /// configuration does not bind sockets.
    pub local_forwards: Option<Vec<LocalForward>>,
    /// Whether [`Self::preferred()`] should prefer compression; defaults to `false`.
    pub compression: bool,
    /// Optional comma-separated cipher list, validated by [`Self::preferred()`].
    pub ciphers: Option<String>,
    /// Optional comma-separated MAC list, validated by [`Self::preferred()`].
    pub macs: Option<String>,
    /// Address family for direct target sockets and local TCP listeners.
    ///
    /// Defaults to both families; jump connections keep their own policy.
    pub address_family: AddressFamily,
    /// Ordered environment-variable patterns requested for remote sessions.
    pub send_env: Option<Vec<String>>,
}

/// A resolved connection chain with its associated per-host algorithm policies.
///
/// Obtained through [`Config::connection`]. The chain and policy collection remain
/// owned together, and the chain can only be borrowed immutably. This preserves
/// their positional association even when hosts have equal network destinations.
/// Lua-created temporary files also remain alive until this value is dropped,
/// including when it is moved out of [`Config`]. Keep it through session cleanup
/// if those files are needed by the connection or session.
#[derive(Debug)]
pub struct ConnectionConfig {
    chain: Chain,
    policies: Policies,
    // Keep Lua-created files alive with the connection settings that use them.
    _temp_dir: Option<Arc<TempDir>>,
}

impl ConnectionConfig {
    /// Borrows the target and optional jump hosts for `Connection::connect()`.
    ///
    /// Bastions use the dependency's reverse connection order. The borrowed
    /// chain cannot be reordered or replaced independently of its policies.
    pub fn chain(&self) -> &Chain {
        &self.chain
    }

    /// Finds the policy associated with an actual host in this connection chain.
    ///
    /// The host must be borrowed from [`Self::chain()`]. Matching uses reference
    /// identity, so equal cloned hosts and hosts from another configuration are
    /// rejected. A host without configured algorithms receives an unconfigured
    /// policy that preserves the supplied transport preferences.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HostNotInChain`] for a host outside this chain, or
    /// [`Error::MissingAlgorithmPolicy`] for inconsistent internal policy data.
    ///
    /// ```
    /// use openrussh_cli_config::{Config, Destination, Error, Session};
    ///
    /// let config = Config::builder().config_file("none").build(
    ///     Destination { host: "server".into(), user: Some("demo".into()), port: None },
    ///     Session::None,
    /// )?;
    /// let connection = &config.connection;
    /// let target = &connection.chain().target;
    /// assert!(!connection.algorithm_policy(target)?.has_host_keys());
    /// let cloned_target = target.clone();
    /// assert!(matches!(
    ///     connection.algorithm_policy(&cloned_target),
    ///     Err(Error::HostNotInChain),
    /// ));
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    pub fn algorithm_policy(&self, host: &Host) -> Result<&Algorithms> {
        self.policies.get(&self.chain, host)
    }
}

/// Setup settings shared by the target and all jump hosts in an invocation.
///
/// Native configuration supplies these through its top-level `meta` record,
/// outside host/default inheritance. OpenSSH and no-file sources leave both
/// fields absent; invocation overrides may supply them for any source format.
/// [`Meta::default()`] also leaves both fields absent.
#[derive(Debug, Default)]
pub struct Meta {
    /// Selected known-host database path, already resolved under native path rules.
    ///
    /// `None` requests the application's default database. The CLI uses
    /// `~/.ssh/known_hosts`, or `/etc/ssh/ssh_known_hosts` without a home directory.
    /// This crate neither loads the database nor saves accepted host keys.
    pub known_hosts: Option<PathBuf>,
    /// Parsed native logging policy; omission differs from [`LogFilter::Disabled`].
    ///
    /// The CLI uses error-level diagnostics when omitted, or debug-level
    /// diagnostics when omitted with [`Config::verbose`] enabled.
    pub log_filter: Option<LogFilter>,
}

/// An explicit native logging choice, distinct from an omitted setting.
///
/// Parse a string with [`str::parse()`] or [`Self::try_from_filter()`].
///
/// [`Self::try_from_filter()`] and deserialization accept the exact string
/// `"none"` to disable logging.
/// Other strings use [`tracing_subscriber::EnvFilter`] syntax, with `error` as
/// the default directive for an empty string. Invalid directives are rejected;
/// the `RUST_LOG` environment variable is not read.
///
/// The CLI applies this filter together with [`Config::verbose`]: without
/// verbosity it caps emitted events at errors; with verbosity it uses the
/// configured directives. An absent `Option<LogFilter>` lets the CLI choose its
/// default, whereas `Some(LogFilter::Disabled)` explicitly disables diagnostics.
///
/// ```
/// use openrussh_cli_config::LogFilter;
///
/// let automatic: Option<LogFilter> = None;
/// let disabled = Some(LogFilter::try_from_filter("none")?);
/// let configured = LogFilter::try_from_filter("debug,russh=info")?;
/// assert!(automatic.is_none());
/// assert!(matches!(disabled, Some(LogFilter::Disabled)));
/// assert!(matches!(configured, LogFilter::Filter(_)));
/// # Ok::<(), openrussh_cli_config::Error>(())
/// ```
#[derive(Clone, Debug)]
pub enum LogFilter {
    /// Disable client log diagnostics, regardless of the verbosity flag.
    Disabled,
    /// Use explicit target/module filtering directives.
    Filter(
        /// The parsed filter; cloning the logging policy also clones this filter.
        Box<tracing_subscriber::EnvFilter>,
    ),
}

impl LogFilter {
    /// Parses a logging filter using the same rules as native configuration.
    ///
    /// The exact string `"none"` returns [`Self::Disabled`]. Other strings use
    /// [`tracing_subscriber::EnvFilter`] directives: an empty string defaults to
    /// `error`, and target directives may be combined with a global level, as in
    /// `"debug,russh=info"`. The `RUST_LOG` environment variable is not read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidLogFilter`] for invalid filter directives.
    ///
    /// ```
    /// use openrussh_cli_config::{Error, LogFilter};
    ///
    /// let filter = LogFilter::try_from_filter("debug,russh=info")?;
    /// assert!(matches!(filter, LogFilter::Filter(_)));
    /// assert!(matches!(LogFilter::try_from_filter("none")?, LogFilter::Disabled));
    /// assert!(matches!(
    ///     LogFilter::try_from_filter("russh=invalid-level"),
    ///     Err(Error::InvalidLogFilter(_)),
    /// ));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn try_from_filter<S>(filter: S) -> Result<Self>
    where
        S: AsRef<str>,
    {
        filter.as_ref().parse()
    }
}

impl std::str::FromStr for LogFilter {
    type Err = Error;

    /// Parses native logging directives without reading environment variables.
    fn from_str(filter: &str) -> Result<Self> {
        use tracing_subscriber::{EnvFilter, filter::LevelFilter};

        if filter == "none" {
            return Ok(Self::Disabled);
        }

        EnvFilter::builder()
            .with_default_directive(LevelFilter::ERROR.into())
            .parse(filter)
            .map(|filter| Self::Filter(Box::new(filter)))
            .map_err(Error::InvalidLogFilter)
    }
}

impl<'de> serde::Deserialize<'de> for LogFilter {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct LogFilterVisitor;

        impl serde::de::Visitor<'_> for LogFilterVisitor {
            type Value = LogFilter;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a logging filter string")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<LogFilter, E> {
                value.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(LogFilterVisitor)
    }
}

#[derive(Debug)]
struct Resolved {
    pub temp_dir: Option<Arc<TempDir>>,
    pub chain: Chain,
    pub algorithms: Policies,
    pub meta: Meta,
}

impl From<Chain> for Resolved {
    fn from(chain: Chain) -> Self {
        Self {
            temp_dir: None,
            chain,
            algorithms: Policies::default(),
            meta: Meta::default(),
        }
    }
}

struct ResolveOptions<'a> {
    destination: &'a Destination,
    identities: Option<&'a [PathBuf]>,
    proxy_jump: Option<&'a [String]>,
    identity_agent: Option<&'a IdentityAgent>,
}

/// The final resolution boundary, implemented independently by each source format.
trait ResolveConfig {
    fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved>;
}

/// A host alias or network name, with optional final-target user and port overrides.
///
/// The fields are already separated: `host` is not parsed as an SSH URI or as
/// `[user@]host[:port]`. Native files look up this exact, case-sensitive alias
/// before replacing the network destination with a configured `host_name`.
/// For instance, the crate-level file example selects `work` even though the
/// resulting connection uses `work.example.com`.
#[derive(Debug, PartialEq, Eq)]
pub struct Destination {
    /// Requested lookup alias, or the network hostname/address when no alias matches.
    pub host: String,
    /// Explicit final-target SSH username; `None` permits file settings or OS fallback.
    ///
    /// This override is not propagated to jump hosts.
    pub user: Option<String>,
    /// Explicit final-target TCP port; `None` permits file settings or the default `22`.
    ///
    /// This override is not propagated to jump hosts. The type permits port `0`.
    pub port: Option<u16>,
}

/// The remote operation requested when finalizing a [`ConfigBuilder`].
///
/// This is stored in [`Config::session`]; building does not start the operation.
/// Select the PTY policy independently with [`ConfigBuilder::tty()`]. The CLI
/// supplies `RequestTty::Never` for [`Self::None`] and [`Self::Forward`], but
/// the builder does not infer that override from the session variant.
///
/// ```
/// use openrussh_cli_config::Session;
///
/// // One command string is interpreted by the remote user's shell.
/// let command = Session::Exec { command: "printf '%s\\n' \"$HOME\"".into() };
/// // This endpoint is reached from the SSH server, without a local listener.
/// let tunnel = Session::Forward { host: "database.internal".into(), port: 5432 };
/// # let _ = (command, tunnel);
/// ```
#[derive(Debug, PartialEq, Eq)]
pub enum Session {
    /// Request the remote user's shell.
    Shell,
    /// Send one SSH exec command string, without adding quoting or a shell wrapper.
    ///
    /// The CLI joins command arguments with spaces before constructing this
    /// variant. The builder stores the supplied string unchanged, even if empty.
    Exec {
        /// Command text for interpretation by the remote user's shell.
        command: String,
    },
    /// Keep the connection and local forwards alive without opening a session channel.
    None,
    /// Bridge stdin/stdout to a TCP endpoint reached from the SSH server.
    Forward {
        /// Remote-side destination hostname or IP address.
        host: String,
        /// Remote-side destination TCP port.
        port: u16,
    },
}

/// One local listener and the matching endpoint reached through SSH.
///
/// Supply these through [`ConfigBuilder::local_forwards()`]. Each variant pairs
/// a local listener with a destination using the same transport. Unix socket
/// paths here do not have the absolute-path requirement of native identity-file
/// settings. Construction records the request without binding a listener.
///
/// ```
/// use openrussh_cli_config::{BindHost, LocalForward};
///
/// let database = LocalForward::Tcp {
///     bind_host: BindHost::Loopback,
///     bind_port: 15432,
///     target_host: "database.internal".into(),
///     target_port: 5432,
/// };
/// let socket = LocalForward::Unix {
///     bind: "./local.sock".into(),
///     target: "/run/service.sock".into(),
/// };
/// # let _ = (database, socket);
/// ```
#[derive(Debug, PartialEq, Eq)]
pub enum LocalForward {
    /// Listen on TCP and connect to a TCP endpoint from the SSH server.
    Tcp {
        /// Local interface/address selection.
        bind_host: BindHost,
        /// Local port; `0` lets the OS choose a port at bind time.
        bind_port: u16,
        /// Destination hostname or unbracketed IP address, resolved remotely.
        target_host: String,
        /// Destination TCP port on the remote side.
        target_port: u16,
    },
    /// Listen on a Unix-domain socket and connect to a remote Unix-domain socket.
    Unix {
        /// Native local filesystem path, which may be relative.
        bind: PathBuf,
        /// Socket path interpreted by the SSH server, rather than locally.
        target: String,
    },
}

/// Local interface selection for [`LocalForward::Tcp`].
///
/// The CLI normally chooses loopback for an omitted bind address, or all
/// interfaces with `-g`; explicit names remain explicit. See [`LocalForward`]
/// for usage.
#[derive(Debug, PartialEq, Eq)]
pub enum BindHost {
    /// Try IPv4 and IPv6 loopback addresses (`127.0.0.1` and `::1`).
    Loopback,
    /// Try IPv4 and IPv6 wildcard addresses (`0.0.0.0` and `::`).
    All,
    /// Resolve and bind an explicit local hostname or IP address.
    Host(
        /// Hostname or unbracketed address; the port is supplied separately.
        String,
    ),
}

impl Config {
    /// Creates a builder with the same defaults as [`ConfigBuilder::new()`].
    ///
    /// Destination and session are supplied to [`ConfigBuilder::build()`], not
    /// stored in the builder. See the [crate-level quick start](crate).
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::new()
    }

    /// Compiles invocation-wide compression, cipher, and MAC preferences.
    ///
    /// Starts with [`Preferred::default()`]. If [`Self::compression`] is enabled,
    /// compression order is `zlib`, `zlib@openssh.com`, then `none`. Supplied
    /// cipher/MAC strings replace their lists in order, splitting on commas
    /// without trimming whitespace. Omitted lists retain transport defaults.
    ///
    /// Per-host KEX and host-key settings are applied separately with
    /// [`algorithms::Algorithms::apply()`], after any permitted known-host preference
    /// adjustment. Calling this method neither connects nor negotiates algorithms.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownCipher`] or [`Error::UnknownMac`] for unsupported
    /// names, including empty names from an empty list or leading/trailing comma.
    /// These checks occur here, not in [`ConfigBuilder::build()`].
    ///
    /// ```
    /// use openrussh_cli_config::{Config, Destination, Error, Session};
    /// use openrussh_client::russh::{cipher, mac};
    ///
    /// let mut config = Config::builder()
    ///     .config_file("none")
    ///     .compression(true)
    ///     .ciphers("aes256-ctr,aes128-ctr")
    ///     .macs("hmac-sha2-256")
    ///     .build(
    ///         Destination { host: "server".into(), user: Some("alice".into()), port: None },
    ///         Session::Shell,
    ///     )?;
    /// let preferred = config.preferred()?;
    /// assert_eq!(preferred.cipher.as_ref(), &[cipher::AES_256_CTR, cipher::AES_128_CTR]);
    /// assert_eq!(preferred.mac.as_ref(), &[mac::HMAC_SHA256]);
    ///
    /// config.ciphers = Some("unknown-cipher".into());
    /// assert!(matches!(config.preferred(), Err(Error::UnknownCipher { .. })));
    /// # Ok::<(), Error>(())
    /// ```
    pub fn preferred(&self) -> Result<Preferred> {
        use openrussh_client::russh::{cipher, compression, mac};

        use std::borrow::Cow;

        let mut preferred = Preferred::default();

        if self.compression {
            preferred.compression = Cow::Borrowed(&[
                compression::ZLIB,
                compression::ZLIB_LEGACY,
                compression::NONE,
            ]);
        }

        if let Some(ciphers) = &self.ciphers {
            preferred.cipher = ciphers
                .split(',')
                .map(|s| {
                    cipher::Name::try_from(s)
                        .map_err(|()| Error::UnknownCipher { name: s.to_owned() })
                })
                .collect::<Result<Vec<_>>>()?
                .into();
        }

        if let Some(macs) = &self.macs {
            preferred.mac = macs
                .split(',')
                .map(|s| {
                    mac::Name::try_from(s).map_err(|()| Error::UnknownMac { name: s.to_owned() })
                })
                .collect::<Result<Vec<_>>>()?
                .into();
        }

        Ok(preferred)
    }
}

#[cfg(test)]
mod tests;
