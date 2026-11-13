use crate::{
    Config, ConnectionConfig, Destination, Error, LocalForward, ResolveConfig, ResolveOptions,
    Resolved, Result, Session,
};
use crate::{defaults, lua, openssh, toml};

use openrussh_client::config::RequestTty;

use std::path::{Path, PathBuf};

/// Builds resolved SSH settings and application options for one invocation.
///
/// Create a builder with [`Config::builder()`], [`Self::new()`], or [`Default::default()`].
/// Every setter consumes and returns the builder. Calling a setter again replaces
/// its previous builder value, including an entire supplied collection; collection
/// merging with a configuration file follows each setter's documented rules.
/// Setters store options without reading files. [`Self::build()`] performs loading
/// and resolution, taking the required [`Destination`] and [`Session`] separately.
///
/// # Defaults
///
/// | Setting | Default |
/// | --- | --- |
/// | [`config_file()`](Self::config_file) | Discover the first available file under `~/.ssh`. |
/// | [`identities()`](Self::identities) | No builder-supplied identities; retain file settings. |
/// | [`proxy_jump()`](Self::proxy_jump) | No builder override; retain the configured route. |
/// | [`tty()`](Self::tty) | No override; use the file policy, or [`RequestTty::ForShell`](openrussh_client::config::RequestTty::ForShell). |
/// | [`verbose()`](Self::verbose) | `false`. |
/// | [`stdin_null()`](Self::stdin_null) | `false`. |
/// | [`escape_char()`](Self::escape_char) | `Some(b'~')`. |
/// | [`local_forwards()`](Self::local_forwards) | `None`. |
/// | [`compression()`](Self::compression) | `false`; retain transport defaults. |
/// | [`ciphers()`](Self::ciphers) | No replacement; retain transport defaults. |
/// | [`macs()`](Self::macs) | No replacement; retain transport defaults. |
///
/// The [`build()` example](Self::build) combines application options and an explicit
/// PTY policy. Application options are returned in [`Config`] for the caller to
/// implement; building does not start a session, initialize logging, or bind
/// forwarding listeners.
#[derive(Debug)]
#[must_use]
pub struct ConfigBuilder {
    pub(super) config_file: Option<PathBuf>,
    pub(super) identities: Option<Vec<PathBuf>>,
    pub(super) proxy_jump: Option<Vec<String>>,
    pub(super) tty: Option<RequestTty>,
    pub(super) verbose: bool,
    pub(super) stdin_null: bool,
    pub(super) escape_char: Option<u8>,
    pub(super) local_forwards: Option<Vec<LocalForward>>,
    pub(super) compression: bool,
    pub(super) ciphers: Option<String>,
    pub(super) macs: Option<String>,
}

impl Default for ConfigBuilder {
    /// Creates the same default builder as [`ConfigBuilder::new()`].
    fn default() -> Self {
        Self {
            config_file: None,
            identities: None,
            proxy_jump: None,
            tty: None,
            verbose: false,
            stdin_null: false,
            escape_char: Some(b'~'),
            local_forwards: None,
            compression: false,
            ciphers: None,
            macs: None,
        }
    }
}

impl ConfigBuilder {
    /// Creates a builder with the [default settings](ConfigBuilder#defaults).
    ///
    /// This is equivalent to [`Config::builder()`] and [`Self::default()`]. File
    /// discovery is deferred until [`Self::build()`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects one configuration file, replacing any previous selection.
    ///
    /// Passing `"none"` skips file discovery and parsing, using built-in defaults
    /// together with the supplied overrides. Otherwise, the path's case-sensitive
    /// extension selects the format: `.lua` executes Lua, `.toml` parses TOML, and
    /// every other extension or absent extension selects OpenSSH syntax. File
    /// contents are not used to detect the format.
    ///
    /// If this setter is omitted, [`Self::build()`] checks `~/.ssh/config.lua`,
    /// `~/.ssh/config.toml`, then `~/.ssh/config`. The first file successfully
    /// opened is selected. Only a not-found error permits trying the next path;
    /// other open errors and all loading or resolution errors stop the build.
    /// If all three files are absent, built-in defaults apply. Files are never
    /// combined. An explicitly selected missing or invalid file is an error and
    /// does not trigger discovery or fallback.
    ///
    /// Relative paths are accepted relative to the process's working directory.
    /// This setter does not expand `~` in the selected filename. The file is
    /// opened only when [`Self::build()`] is called.
    pub fn config_file<P>(mut self, path: P) -> Self
    where
        P: Into<PathBuf>,
    {
        self.config_file = Some(path.into());
        self
    }

    /// Replaces the ordered builder-supplied authentication identity paths.
    ///
    /// These paths apply only to the final destination, preceding identities
    /// accumulated from the selected file. Jump hosts use their own file settings.
    /// Order and duplicates are preserved. An empty iterator records an explicitly
    /// empty builder list; it does not remove identities supplied by the file.
    /// Omitting this setter leaves the builder list absent instead.
    ///
    /// Builder paths use the CLI path rules for every format: relative paths are
    /// accepted, `~` and `~user` expand for the current user, and paths that cannot
    /// be expanded are omitted. Files are not opened or checked for key material
    /// during building. These rules differ from native-file identity paths, which
    /// must resolve to absolute paths.
    ///
    /// Repeated calls replace the builder list rather than append to it:
    ///
    /// ```
    /// use openrussh_cli_config::{ConfigBuilder, Destination, Session};
    /// use std::path::PathBuf;
    ///
    /// let config = ConfigBuilder::new()
    ///     .config_file("none")
    ///     .identities(["/keys/replaced"])
    ///     .identities(["/keys/primary", "relative-key"])
    ///     .build(
    ///         Destination {
    ///             host: "server.example".into(),
    ///             user: Some("alice".into()),
    ///             port: None,
    ///         },
    ///         Session::Shell,
    ///     )?;
    /// assert_eq!(
    ///     config.connection.chain().target.auth.identities,
    ///     Some(vec![PathBuf::from("/keys/primary"), PathBuf::from("relative-key")]),
    /// );
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    pub fn identities<I, P>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.identities = Some(paths.into_iter().map(Into::into).collect());
        self
    }

    /// Replaces the final destination's configured jump-host list.
    ///
    /// Supply one host specification per item, in connection order from the local
    /// machine toward the destination, for example
    /// `["alice@outer.example:2222", "inner.example"]`. This setter does not split
    /// comma-separated strings or translate the CLI spelling `"none"`; use an
    /// empty iterator to clear the route. If the setter is omitted, the selected
    /// file's route remains eligible. Repeated calls replace the entire builder
    /// list rather than concatenate it.
    ///
    /// Each jump is resolved through the selected format's own host/default rules.
    /// The final destination's user, port, and builder identity overrides do not
    /// propagate to jumps. In native formats, a jump specification's embedded
    /// user or port is a fallback below that jump's file settings. The first jump
    /// can inherit additional ancestors from its own configuration. Native
    /// resolution rejects cycles in those inherited routes.
    ///
    /// An explicitly empty iterator suppresses a route inherited from a file:
    ///
    /// ```
    /// use openrussh_cli_config::{ConfigBuilder, Destination, Session};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("config.toml");
    /// std::fs::write(
    ///     &path,
    ///     "[hosts.work]\nproxy-jump = ['gateway']\n[hosts.gateway]\nuser = 'relay'\n",
    /// )?;
    /// let destination = || Destination {
    ///     host: "work".into(), user: Some("alice".into()), port: None,
    /// };
    /// let inherited = ConfigBuilder::new()
    ///     .config_file(&path)
    ///     .build(destination(), Session::Shell)?;
    /// assert!(inherited.connection.chain().bastions.is_some());
    ///
    /// let direct = ConfigBuilder::new()
    ///     .config_file(&path)
    ///     .proxy_jump(std::iter::empty::<&str>())
    ///     .build(destination(), Session::Shell)?;
    /// assert!(direct.connection.chain().bastions.is_none());
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    pub fn proxy_jump<I, S>(mut self, jumps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.proxy_jump = Some(jumps.into_iter().map(Into::into).collect());
        self
    }

    /// Overrides the final destination's pseudo-terminal policy after resolution.
    ///
    /// This takes precedence over the file's policy and leaves jump-host policies
    /// unchanged. Omitting it retains the source policy, whose built-in default is
    /// [`RequestTty::ForShell`](openrussh_client::config::RequestTty::ForShell).
    /// Repeated calls retain the last supplied policy.
    ///
    /// [`Self::build()`] does not infer a policy from [`Session`]: callers selecting
    /// [`Session::None`] or [`Session::Forward`] should supply
    /// [`RequestTty::Never`](openrussh_client::config::RequestTty::Never) when they
    /// want no PTY. The CLI adapter supplies that override itself. See the
    /// [`build()` example](Self::build).
    pub fn tty(mut self, tty: RequestTty) -> Self {
        self.tty = Some(tty);
        self
    }

    /// Sets [`Config::verbose`], replacing its previous value; the default is `false`.
    ///
    /// The consuming application uses this flag to permit verbose diagnostics
    /// alongside [`Config::meta`]'s log filter. This setter does not initialize
    /// logging or replace that filter. See the [`build()` example](Self::build).
    pub fn verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Sets [`Config::stdin_null`], replacing its previous value; the default is `false`.
    ///
    /// `true` requests that the application send end-of-input instead of reading
    /// local standard input for the selected session. Building only stores this
    /// policy. See the [`build()` example](Self::build).
    pub fn stdin_null(mut self, stdin_null: bool) -> Self {
        self.stdin_null = stdin_null;
        self
    }

    /// Replaces the terminal escape byte, or disables escapes with `None`.
    ///
    /// The default is `Some(b'~')`. The byte is stored verbatim: this setter does
    /// not parse CLI control notation such as `"^Z"` or impose ASCII validation.
    /// The application decides when to interpret escapes; the CLI uses them only
    /// for interactive PTY sessions, outside stdio forwarding. See the
    /// [`build()` example](Self::build).
    pub fn escape_char(mut self, escape_char: Option<u8>) -> Self {
        self.escape_char = escape_char;
        self
    }

    /// Replaces the ordered collection stored in [`Config::local_forwards`].
    ///
    /// The iterator supplies owned [`LocalForward`] values. Omitting this setter
    /// produces `None`; supplying an empty iterator produces `Some(Vec::new())`.
    /// Neither requests a listener in the CLI. Repeated calls replace the entire
    /// collection rather than append to it.
    ///
    /// Specifications are stored without binding listeners or validating their
    /// endpoint values. The application owns binding, forwarding tasks, and
    /// cleanup. See [`LocalForward`] for constructing TCP and Unix-domain pairs.
    ///
    /// Use a typed empty iterator when an explicitly empty collection is needed:
    ///
    /// ```
    /// use openrussh_cli_config::{ConfigBuilder, Destination, LocalForward, Session};
    ///
    /// let config = ConfigBuilder::new()
    ///     .config_file("none")
    ///     .local_forwards(std::iter::empty::<LocalForward>())
    ///     .build(
    ///         Destination {
    ///             host: "server.example".into(),
    ///             user: Some("alice".into()),
    ///             port: None,
    ///         },
    ///         Session::None,
    ///     )?;
    /// assert_eq!(config.local_forwards, Some(Vec::new()));
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    pub fn local_forwards<I>(mut self, forwards: I) -> Self
    where
        I: IntoIterator<Item = LocalForward>,
    {
        self.local_forwards = Some(forwards.into_iter().collect());
        self
    }

    /// Sets the compression preference; the default is `false`.
    ///
    /// Repeated calls replace the previous value. With `true`,
    /// [`Config::preferred()`] orders compression as `zlib`, `zlib@openssh.com`, then
    /// `none`. With `false`, it keeps the transport's default order. Actual
    /// compression depends on SSH negotiation; this setter only stores the flag.
    pub fn compression(mut self, compression: bool) -> Self {
        self.compression = compression;
        self
    }

    /// Replaces the base transport's ordered comma-separated cipher list.
    ///
    /// Omitting this setter retains transport defaults. The string is stored
    /// unchanged; [`Self::build()`] does not validate it. [`Config::preferred()`]
    /// performs exact name lookup and returns [`Error::UnknownCipher`] for an
    /// unknown name, including an empty entry. Whitespace is not trimmed and
    /// native algorithm-list modifiers such as `+`, `-`, or `^` are not supported.
    /// Order and duplicates are preserved, and repeated calls replace the whole
    /// string. See [`Config::preferred()`] for preference and error examples.
    pub fn ciphers<S>(mut self, ciphers: S) -> Self
    where
        S: Into<String>,
    {
        self.ciphers = Some(ciphers.into());
        self
    }

    /// Replaces the base transport's ordered comma-separated MAC algorithm list.
    ///
    /// Omitting this setter retains transport defaults. As with [`Self::ciphers()`],
    /// the string is stored unchanged and validated only by [`Config::preferred()`],
    /// which returns [`Error::UnknownMac`] for unknown names or empty entries.
    /// Whitespace is not trimmed, list modifiers are not supported, and order and
    /// duplicates are preserved. Repeated calls replace the entire string.
    pub fn macs<S>(mut self, macs: S) -> Self
    where
        S: Into<String>,
    {
        self.macs = Some(macs.into());
        self
    }

    /// Consumes the builder and resolves configuration for a destination and session.
    ///
    /// `destination` supplies the lookup alias and optional final-host user/port
    /// overrides. `session` supplies the application's requested operation. Both
    /// are required arguments, rather than optional builder state. For native
    /// files, final-host scalar precedence is explicit destination override,
    /// matching host record, defaults record, then built-in defaults; OpenSSH
    /// resolution follows the dependency's host-matching rules. Identity lists,
    /// jump routes, and PTY overrides follow [`Self::identities()`],
    /// [`Self::proxy_jump()`], and [`Self::tty()`] respectively.
    ///
    /// Loading is synchronous. This method selects one file using
    /// [`Self::config_file()`]'s rules, parses it or executes its Lua chunk, resolves
    /// the destination and jump hosts, and applies any final-host PTY override.
    /// The returned [`Config`] owns its values and retains no source text, Lua VM,
    /// or open configuration file. Application settings move into the result;
    /// no SSH connection or session is started. Identity/key contents and agent
    /// connections are left to the SSH client. Cipher and MAC name validation is
    /// deferred to [`Config::preferred()`].
    ///
    /// Selecting [`Session::None`] or [`Session::Forward`] does not automatically
    /// change the resolved PTY policy. Supply [`Self::tty()`] explicitly when needed:
    ///
    /// ```
    /// use openrussh_cli_config::{ConfigBuilder, Destination, Session};
    /// use openrussh_client::config::RequestTty;
    ///
    /// let config = ConfigBuilder::new()
    ///     .config_file("none")
    ///     .verbose(true)
    ///     .stdin_null(true)
    ///     .escape_char(None)
    ///     .tty(RequestTty::Never)
    ///     .build(
    ///         Destination {
    ///             host: "example.com".into(),
    ///             user: Some("alice".into()),
    ///             port: Some(2222),
    ///         },
    ///         Session::None,
    ///     )?;
    /// assert_eq!(config.connection.chain().target.dest.port, 2222);
    /// assert_eq!(config.connection.chain().target.tty, RequestTty::Never);
    /// assert!(config.verbose && config.stdin_null);
    /// assert_eq!(config.escape_char, None);
    /// assert_eq!(config.session, Session::None);
    /// # Ok::<(), openrussh_cli_config::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::HomeDirectory`] if automatic discovery cannot locate a
    /// home directory, [`Error::OpenConfig`] for a selected-file open failure, or
    /// [`Error::ReadConfig`] with the filename and underlying parse, Lua, or
    /// resolution error for a selected file. Defaults-only resolution can also
    /// fail, for example if a username cannot be obtained or an authentication
    /// agent path is invalid. Native resolution errors retain any successfully
    /// parsed logging metadata through [`Error::log_filter()`]. Loading errors do
    /// not fall back to another format or configuration file.
    pub fn build(self, destination: Destination, session: Session) -> Result<Config> {
        let Resolved {
            mut chain,
            algorithms,
            meta,
        } = self.resolve(&self.options(&destination))?;

        if let Some(tty) = self.tty {
            chain.target.tty = tty;
        }

        Ok(Config {
            connection: ConnectionConfig {
                chain,
                policies: algorithms,
            },
            meta,
            verbose: self.verbose,
            session,
            stdin_null: self.stdin_null,
            escape_char: self.escape_char,
            local_forwards: self.local_forwards,
            compression: self.compression,
            ciphers: self.ciphers,
            macs: self.macs,
        })
    }

    pub(super) fn options<'a>(&'a self, destination: &'a Destination) -> ResolveOptions<'a> {
        ResolveOptions {
            destination,
            identities: self.identities.as_deref(),
            proxy_jump: self.proxy_jump.as_deref(),
        }
    }

    pub(super) fn resolve(&self, options: &ResolveOptions<'_>) -> Result<Resolved> {
        if let Some(path) = &self.config_file {
            if path == Path::new("none") {
                return defaults::resolve(options);
            }

            let file = std::fs::File::open(path).map_err(|source| Error::OpenConfig {
                path: path.to_owned(),
                source,
            })?;
            return Self::read_config(file, path, options);
        }

        let dir = std::env::home_dir()
            .ok_or(Error::HomeDirectory)?
            .join(".ssh");
        Self::default_config(&dir, options)
    }

    pub(super) fn default_config(dir: &Path, options: &ResolveOptions<'_>) -> Result<Resolved> {
        use std::fs::File;
        use std::io::ErrorKind;

        for name in ["config.lua", "config.toml", "config"] {
            let path = dir.join(name);
            match File::open(&path) {
                Ok(file) => return Self::read_config(file, &path, options),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(source) => return Err(Error::OpenConfig { path, source }),
            }
        }

        defaults::resolve(options)
    }

    fn read_config(
        mut file: std::fs::File,
        path: &Path,
        options: &ResolveOptions<'_>,
    ) -> Result<Resolved> {
        let read_error = |source| Error::ReadConfig {
            path: path.to_owned(),
            source: Box::new(source),
        };
        if path
            .extension()
            .is_some_and(|extension| extension == "toml" || extension == "lua")
        {
            use std::io::Read as _;

            let mut text = String::new();
            file.read_to_string(&mut text)
                .map_err(|error| read_error(error.into()))?;
            drop(file);

            return if path.extension().is_some_and(|extension| extension == "lua") {
                let parsed = lua::ParsedConfig::execute(&text, path).map_err(read_error)?;
                Self::resolve_source(&parsed, options, path)
            } else {
                let parsed = toml::ParsedConfig::parse(&text).map_err(read_error)?;
                Self::resolve_source(&parsed, options, path)
            };
        }

        let parsed = openssh::ParsedConfig::parse(file, options).map_err(read_error)?;
        Self::resolve_source(&parsed, options, path)
    }

    fn resolve_source<S>(source: &S, options: &ResolveOptions<'_>, path: &Path) -> Result<Resolved>
    where
        S: ResolveConfig,
    {
        source.resolve(options).map_err(|source| Error::ReadConfig {
            path: path.to_owned(),
            source: Box::new(source),
        })
    }
}
