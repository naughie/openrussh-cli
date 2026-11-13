# Development reference

This document covers implementation boundaries, repository conventions, validation, and release automation. See [Runtime reference](runtime.md) for observable behavior and [Configuration architecture](architecture.md) for configuration semantics and types.

## Application boundaries

The root Cargo package is `openrussh-cli`, its library crate is `openrussh_cli`, and the executable is `ssh`. `main.rs` creates the Tokio runtime and calls `pub async fn run() -> ExitCode`.
The application keeps implementation modules private and owns error reporting,
exit status, and resource cleanup. The binary shuts the runtime down with
`shutdown_background()` and returns the code through Rust's `Termination`
mechanism. This prevents runtime teardown from waiting indefinitely when a
signal ends an invocation while a blocking host-key prompt is still reading.
The application library does not call `process::exit`; Clap retains its own
help, version, and argument-error exits.

The root manifest also defines a workspace containing the application and
`crates/*`. The configuration library lives in
`crates/openrussh-cli-config`, with the Rust crate name
`openrussh_cli_config`. Unix terminal resources live in
`crates/openrussh-cli-terminal`, with the Rust crate name
`openrussh_cli_terminal`. All three packages are workspace default members.
The application uses path dependencies on both libraries and retains its
remaining `src/` modules.

| Module | Responsibility |
| --- | --- |
| `cli` | Parse arguments into an `Invocation`, validate CLI syntax, and adapt its values to `ConfigBuilder`. |
| `connection` | Apply address-family filtering to direct TCP connections while retaining dependency-owned SSH/authentication and jump handling. |
| `openrussh_cli_config` | Resolve format-specific configuration and supplied inputs into the application's format-independent configuration. |
| `logging` | Initialize tracing, apply native filter directives and the CLI verbosity limit, and format client diagnostics for stderr. |
| `known_hosts` | Load the selected trust database once and retain its save path as a setup resource. |
| `handler` | Apply per-host transport policies and integrate the library's known-host checks with local trust prompts and persistence. |
| `session` | Select the library session operation and pump channel input/output, escapes, and terminal events. |
| `openrussh_cli_terminal` | Own Unix signal streams, terminal-mode guards, input flags, size queries, suspension, and asynchronous input reading. |
| `forwarding` | Bind local listeners, supervise accepted-stream tasks, and release listener resources. |
| `lib` | Coordinate connection, session, forwarding, shutdown, and final status. |

`openrussh-client` owns SSH transport, negotiation, user authentication, jump
connections, and supported channel operations. The application connection adapter
retains its `Connection` for unrestricted sockets and jump chains. A restricted
direct connection supplies a filtered TCP stream to the dependency's one-shot
connector, preserving the original hostname for host-key checks and policy lookup.
Session and forwarding code use the dependency's handle-based helpers.

The ordinary shell/exec path uses the dependency's safe process helpers. `SendEnv`
requires opening one session channel, requesting any PTY, and sending SSH
environment requests before the shell/exec request on that same channel. Keep
this path's PTY parameters aligned with the dependency helpers. Never implement
environment transfer with shell assignments or rewrite the raw remote command.
The `-W` path adapts a direct channel to the library's `Child` without session or
environment requests.

The CLI joins all command arguments after the destination with single ASCII spaces, without adding quotes or escaping, and constructs `Session::Exec { command }` for a nonempty result. An empty result selects `Session::Shell`. Session execution passes the command string directly to the library helper as one SSH `exec` command. The remote user's shell interprets its operators, quotes, and expansions. Preserve these OpenSSH CLI semantics across the parser, domain configuration, and session boundaries; do not preserve local argument boundaries through extra quoting or an explicit shell wrapper.

`ClientHandlerFactory` and `ClientHandler` are the application integration types. The outer connection setup instantiates them. Factory construction requires its resolved `ConnectionConfig`, so host membership is checked even without explicit algorithm overrides. Session, forwarding, and lifecycle code remains generic over the dependency's handler trait. Keep this boundary when introducing reusable operations. Application configuration is a concrete domain type, distinct from both parser output and `openrussh_client::config` intermediate types.

Each format's private `ParsedConfig` resolves to the configuration crate's
private `Resolved`, containing the connection chain, typed per-host algorithm
policies, and setup metadata. `ConfigBuilder::new()` or `Config::builder()`
starts construction; consuming fluent setters configure optional values.
`build(destination, session)` requires both invocation-specific values and
combines the resolved source with the supplied application settings into
`Config`. `HostOverrides` supplies a separate, format-independent invocation
stage after source resolution. `cli::parse()` remains free of configuration-file IO; the parsed
`Invocation::build()` performs the builder adaptation and finalization.
Setup metadata remains outside host/default records and is used when
initializing resources; handlers receive the known-host resource and its
save path rather than `Meta`. `ConnectionConfig` owns the chain and its policies
behind private fields, preventing independent replacement or host reordering.
Policy lookup uses resolved `Host` reference identity within that connection
configuration, preserving distinct policies even for hosts with identical
network destinations.

TOML and Lua own independent schemas and resolvers under their respective
modules. TOML profiles can borrow their source text; Lua profiles use owned
strings independent of the VM. Each format owns its host and metadata records,
scalar/list visitors, jump representation, path handling, and host resolution.
The root `defaults` module resolves the no-file path directly into owned
connection/authentication values without either profile type. TOML, Lua, and
this no-file resolver each implement CLI identity expansion independently.
Current overlap in observable behavior is documented and tested with selected
fixtures; it does not require future schema or semantic changes to affect both
formats.

The private `ResolveConfig` trait defines the resolution operation implemented
separately by all three `ParsedConfig` types. Builder loading uses a generic
helper with static dispatch to add selected-file error context. The private
`algorithms::AlgorithmList` trait shares transport-list compilation through
format-owned string iteration. Neither trait provides a shared native schema,
value/path parser, or host/jump resolver, and neither uses trait objects.

Lua may also contribute a temporary-directory owner through `cli.temp_file`.
Keep this owner in `ConnectionConfig` through connection teardown and release
it through types and scopes. OpenSSH, TOML, built-in defaults, and Lua
configurations that never call the helper must not allocate temporary-resource
state or an `Arc`.

## Dependencies and build contract

The project uses Rust edition 2024 and major/minor dependency requirements in manifests. The lockfile records the resolved dependency versions; the current SSH implementation is `openrussh-client` 0.4.6. Its enabled features select AWS-LC cryptography, RSA support, compression, the OpenSSH config parser, known hosts, process operations, and local TCP/Unix-domain forwarding. Disabling an unused feature is preferable to retaining an unnecessary implementation dependency.

Shared versions for `nix`, `openrussh-client`, `tokio`, `tracing-subscriber`,
and `tempfile` live in `[workspace.dependencies]`. Each member selects the
features it needs. The configuration package owns `mlua`, `serde`, `toml`, and
`whoami`; these are not direct application dependencies. Configuration uses
`nix` only for hostname lookup and `tracing-subscriber` for filter parsing.
The terminal package owns `terminal_size` and selects the `nix` filesystem,
process, signal, and terminal features. Its production Tokio features provide
files, asynchronous IO/readiness, signal streams, and selection macros;
terminal tests additionally enable the runtime and time utilities.

The application has no direct `nix` dependency. It uses `libc` for signal and
OS error constants and `socket2` for IPv6 listener options, while terminal
operations go through `openrussh_cli_terminal`. The application enables the SSH
dependency's process, forwarding, and known-host features and its own logging
formatting features. The application and configuration packages both select
the transport crypto backend, RSA, and compression support. The configuration
library needs the compression constants when compiling `Config::preferred()`
and builds independently.

`mlua` enables `vendored`, `lua54`, `serde`, and `error-send`, with default features disabled. Lua 5.4 is built from bundled source and linked into the executable, so release users do not install Lua separately. Configuration is evaluated synchronously once; the Lua async feature is deliberately absent. `error-send` allows errors to cross the application's error boundary without making the Lua state an asynchronous shared service. Serde performs native configuration deserialization, with `toml` handling TOML syntax. Do not introduce a manual TOML or Lua configuration parser.

`tempfile` supplies the lazily owned temporary directory for Lua-generated
resources, and `rand` supplies random filenames. Both are runtime dependencies
of the configuration package. The directory is created with private Unix permissions before its path is
exposed to Lua; callers create and write files themselves.

Client diagnostics use `tracing` with `tracing-subscriber` for initialization
and formatting, including its bridge for dependency `log` records. Keep the
effective filter controlled by the CLI verbosity flag and native metadata.
Use `tracing_subscriber::EnvFilter` for directive parsing and matching, without
reading `RUST_LOG` from the environment. Move the owned filter into subscriber
setup; after a configuration failure, `Error::take_log_filter()` transfers only
the policy while retaining the diagnostic and its causes. Retain the OpenSSH-style
stderr format
described in the [runtime reference](runtime.md#logging). Remote channel output and terminal
interaction are separate from log diagnostics.

Source builds require a Unix-like system, Rust/Cargo, and a C toolchain with a compiler and linker for the vendored Lua and AWS-LC dependencies. The configured non-FIPS AWS-LC build does not require CMake on the supported platforms. The code relies on Unix file descriptors, termios, signals, and Unix-domain sockets. Vendored cryptographic dependencies must remain compatible with static linking for musl builds.

The support tiers specified by the project are:

| Tier | Platforms |
| --- | --- |
| 1 | x86-64 GNU/Linux, x86-64 musl Linux, Apple Silicon macOS |
| 2 | AArch64 GNU/Linux, AArch64 musl Linux, Intel macOS |

Other platforms have no support contract. These are project support tiers, not Rust's target-tier assignments. The release matrix currently builds `aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `x86_64-unknown-linux-musl`. AArch64 musl is supported at tier 2 but currently has no configured release artifact.

## Implementation conventions

`instructions.md` defines the original application scope and `coding-styles.md` defines the author's Rust conventions. Later explicit requirements add tests, documentation, native configuration, and release automation; their earlier temporary exclusions no longer apply. The dependency remains the authority for OpenSSH configuration and SSH behavior delegated to it. Native configuration has its own documented semantics rather than an OpenSSH-compatibility requirement.

Apply these conventions during implementation and review:

- Divide modules and types by semantic responsibility and processing stage. A module with child files uses `foo/mod.rs` and `foo/bar.rs`, not `foo.rs` alongside `foo/bar.rs`.
- Preserve distinct concepts as distinct concrete types or enum variants. Do not convert unrelated parser output into an intermediate type solely to reuse an implementation path.
- Keep TOML and Lua schemas, value/jump visitors, path rules, and host resolution inside their respective modules. Share the resolved application types and transport-policy operations without introducing a common native source model or path-mode switch. Evaluate a format change against that format's documented contract.
- Borrow where ownership is unnecessary. Move or clone at task boundaries; use `Arc` to express task sharing or avoid copying a large value.
- Use generics or enums for abstraction; do not introduce trait objects. The standard `std::error::Error::source` signature remains the interoperability boundary for error causes. Keep helper visibility and import scope narrow; import traits anonymously unless their names are required for bounds or implementations.
- Changes must not increase heap allocations. Allocate only when a value is needed. Retain optional collections as `Option<Vec<_>>` where absence is meaningful, avoid materializing strings for formatting alone, and review each clone, collection, and task spawn for a concrete purpose.
- Use safe library process helpers appropriate to the command semantics. The ordinary command path uses `process::cmd::exec(conn.handle(), command, conn.request_tty())`; the `SendEnv` path sends the same raw command after environment requests on its session channel. Use the `Command::new().prog(...).arg(...)` builder when an operation requires literal program and argument values. Do not use `Command::new_unchecked()` for CLI execution. Keep any required unsafe operation local and satisfy its resource, initialization, and lifetime requirements.
- Release files, descriptors, terminal modes, listeners, and tasks promptly through scopes and guards. Preserve the original failure when cleanup also fails.
- Use blank lines to separate semantic stages and related definitions. Keep imports, names, and type paths readable without mechanically grouping everything or spelling every type as a full path.
- Consider LLVM optimization and allocation/context-switch costs, without CPU-specific or assembly-level optimization. Choose task spawning, async combinators, or a custom future according to ownership, cancellation, and actual cost.

Errors at the application boundary use `anyhow` with short English operation/context messages. The configuration crate has no `anyhow` dependency, including in its tests; it exposes its own `Error` and `Result` types. Its error source chain preserves parser, IO, and resolution causes, and `Error::log_filter()` exposes a parsed native filter when later validation fails. Convert these errors into `anyhow` only in the application adapter. Do not suppress input, persistence, or cleanup failures merely to make a path appear successful. Likewise, do not add validation to library-owned behavior unless the application's own format or syntax requires it.

## Test architecture

Unit tests live alongside the implementation in all three workspace packages. The configuration crate owns builder, format-local resolution, and selected cross-format outcome coverage; the terminal crate owns terminal attributes, size, asynchronous input, and shared input-flag restoration tests; the application owns README-example and handler-boundary regressions. Together they cover application parsing, native configuration resolution, listener ownership, known-host persistence, escape state, input flags, terminal guards, and exit policy. They should exercise meaningful behavior and edge cases rather than repeat the implementation as assertions.

Algorithm coverage should distinguish transport defaults from known-host
preference adjustments, preserve explicit allowlists and KEX extension signals,
and exercise named/inline jump policies independently. Known-host tests cover
both reading and saving the selected database, including missing-file setup and
existing locking/permission behavior. Each format's tests should assert its
own outcomes directly; cross-format comparisons are regression fixtures for
selected overlapping behavior rather than a semantic coupling requirement.
Native additions must retain the existing OpenSSH adapter behavior.

The root workspace and `tests/` are separate workspaces; `tests/` is explicitly excluded from the root workspace. Consequently, `cargo test` at the repository root runs unit tests for all three packages, public API tests, and doctests, but does not run the executable integration suites. The `tests/` workspace contains six application suites and a shared `tester` support crate:

| Suite | Application behavior |
| --- | --- |
| `cli` | CLI overrides, configuration selection/matching, native value types, path rules, and verbose logging policy. |
| `stdio` | Shell/exec input, remote command joining and shell interpretation, stdout/stderr, exit status, EOF, and full-duplex output. |
| `terminal` | PTY policy, resize, escapes, suspension, and terminal restoration. |
| `signals` | Termination and suspension across session modes and host-key confirmation. |
| `host-key-prompt` | Unknown-host acceptance/rejection and known-host persistence. |
| `listeners` | Local forwarding, binding behavior, rollback, and socket cleanup. |

Each suite is an executable using `libtest-mimic`; `tester` supplies a shared Tokio runtime, CLI subprocess/PTY operations, fixtures, and timeouts. Tests run serially by default because some cases temporarily mutate fixture configuration or known-host files. Keep fixture mutation scoped and restore it on failure. The helper checks the fixture home before making these changes. Spawned CLI processes use cancellation/kill guards so a failed assertion does not leave a long-running client behind.

The Docker fixture builds this project's actual `ssh` binary and the selected suite, launches a real OpenSSH server, and runs the suite as a separate non-root client user. The server listens on `127.0.0.1:2222`, uses an Ed25519 host key and public-key authentication for the fixture server user, and starts with a known host entry for ordinary tests. Prompt tests temporarily remove or replace that entry. The fixture pins its Debian/OpenSSH package inputs in `tests/x.sh` and `tests/Dockerfile`.

Integration coverage belongs to this application: CLI interpretation, configuration, process streams, Unix lifecycle handling, and forwarding ownership. Do not replace the OpenSSH fixture with a mock SSH server or add tests whose only assertion follows immediately from a dependency API's behavior.

Terminal restoration tests compare all relevant termios state. On macOS, their comparison helper masks only `PENDIN`, a kernel-maintained pending-input state flag that can change across canonical/raw transitions. This is a test normalization, not a relaxation of production restoration or a reason to ignore other terminal flags. Preserve the queued-input and shared-file-status-flag regression coverage.

## Local validation

Run unit tests and the real-server suites separately:

```sh
cargo test --locked --workspace
./tests/x.sh
```

To validate either library by itself, select its package:

```sh
cargo test --locked -p openrussh-cli-config
cargo test --locked -p openrussh-cli-terminal
```

Docker is required for the integration suites. The runner builds and runs all suites by default, reports failed suites, and works with or without a controlling terminal. Select a suite or separate image construction from execution when iterating:

```sh
./tests/x.sh cli
./tests/x.sh signals build
./tests/x.sh signals run
```

Formatting and lint checks cover both workspaces:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --manifest-path tests/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path tests/Cargo.toml --workspace --all-targets -- -D warnings
```

Select additional platform or artifact checks when a change affects termios, platform-specific code, native compilation, or linking. A Linux unit-test pass does not establish macOS terminal behavior, and a macOS release build does not establish that its tests ran.

## CI and releases

`dist-workspace.toml` configures cargo-dist 0.32.0, GitHub CI, archive artifacts without generated installers, and the release target matrix. The `dist` Cargo profile inherits `release`, uses thin LTO, and strips symbols. The Apple Silicon runner override is `macos-15`; keep runner selection in the dist configuration rather than relying on a retired default label.

`.github/workflows/release.yml` runs on pull requests and version-like tags. Its Ubuntu planning job installs stable Rust, runs `cargo +stable test --locked` for all default workspace members, and runs `./tests/x.sh` before invoking dist planning or release creation. Artifact build jobs depend on that job. The publishing job also explicitly requires successful planning, so the test gate cannot be bypassed through skipped artifact jobs. The default pull-request path plans the release without building all release artifacts unless dist's PR run mode requests uploads.

The test gate currently executes on Linux. macOS jobs build artifacts; they do not run the unit or integration suites. Preserve this distinction when interpreting a successful workflow.

The generated workflow contains intentional test-gate customizations. `allow-dirty = ["ci"]` permits those changes to survive dist's generated-file check. Preserve the unit/E2E steps and dependency gates when regenerating or upgrading the workflow. Inspect `dist plan` when changing the target/runner matrix, and review generated workflow changes alongside the dist configuration; the dirty-CI allowance itself does not verify that custom edits still enforce the intended gate.
