# Development reference

This document covers implementation boundaries, repository conventions, validation, and release automation. See [Runtime reference](runtime.md) for observable behavior and [Configuration architecture](architecture.md) for configuration semantics and types.

## Application boundaries

The Cargo package is `openrussh-cli`, the library crate is `openrussh_cli`, and the executable is `ssh`. `main.rs` creates the Tokio runtime and calls `pub async fn main_impl()`. The library keeps implementation modules private and owns error reporting, exit status, and resource cleanup before the final process exit.

| Module | Responsibility |
| --- | --- |
| `cli` | Parse arguments, validate CLI syntax, and construct configuration inputs. |
| `config` | Resolve format-specific configuration and CLI inputs into the application's format-independent configuration. |
| `handler` | Integrate the library's known-host checks with local trust prompts and persistence. |
| `session` | Select the library session operation and pump channel input/output, escapes, and terminal events. |
| `terminal` | Own Unix signal streams, terminal-mode guards, input flags, and asynchronous stdin reading. |
| `forwarding` | Bind local listeners, supervise accepted-stream tasks, and release listener resources. |
| `lib` | Coordinate connection, session, forwarding, shutdown, and final status. |

`openrussh-client` owns SSH transport, negotiation, user authentication, jump connections, and supported channel operations. Use its `Connection` methods for shell, exec, and local forwarding. The application's direct-channel use for `-W` adapts standard streams to a library `Child`; it does not justify duplicating helpers that already accept the required resource.

`MyMakeHandler` and `MyHandler` are the application integration types. The outer connection setup instantiates them; session, forwarding, and lifecycle code remains generic over the dependency's handler trait. Keep this boundary when introducing reusable operations. Application configuration is a concrete domain type, distinct from both parser output and `openrussh_client::config` intermediate types.

## Dependencies and build contract

The project uses Rust edition 2024 and major/minor dependency requirements in manifests. The lockfile records the resolved dependency versions; the current SSH implementation is `openrussh-client` 0.4.6. Its enabled features select AWS-LC cryptography, RSA support, compression, the OpenSSH config parser, known hosts, process operations, and local TCP/Unix-domain forwarding. Disabling an unused feature is preferable to retaining an unnecessary implementation dependency.

`mlua` enables `vendored`, `lua54`, `serde`, and `error-send`, with default features disabled. Lua 5.4 is built from bundled source and linked into the executable, so release users do not install Lua separately. Configuration is evaluated synchronously once; the Lua async feature is deliberately absent. `error-send` allows errors to cross the application's error boundary without making the Lua state an asynchronous shared service. Serde performs native configuration deserialization, with `toml` handling TOML syntax. Do not introduce a manual TOML or Lua configuration parser.

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
- Borrow where ownership is unnecessary. Move or clone at task boundaries; use `Arc` to express task sharing or avoid copying a large value.
- Prefer generics or enums to trait objects, except where a dependency requires dynamic dispatch. Keep helper visibility and import scope narrow; import traits anonymously unless their names are required for bounds or implementations.
- Allocate only when a value is needed. Retain optional collections as `Option<Vec<_>>` where absence is meaningful, avoid materializing strings for formatting alone, and review each clone, collection, and task spawn for a concrete purpose.
- Use the safe `Command::new().prog(...).arg(...)` builder for remote commands. Keep any required unsafe operation local and satisfy its resource, initialization, and lifetime requirements.
- Release files, descriptors, terminal modes, listeners, and tasks promptly through scopes and guards. Preserve the original failure when cleanup also fails.
- Use blank lines to separate semantic stages and related definitions. Keep imports, names, and type paths readable without mechanically grouping everything or spelling every type as a full path.
- Consider LLVM optimization and allocation/context-switch costs, without CPU-specific or assembly-level optimization. Choose task spawning, async combinators, or a custom future according to ownership, cancellation, and actual cost.

Errors at the application boundary use `anyhow` with short English operation/context messages. Do not suppress input, persistence, or cleanup failures merely to make a path appear successful. Likewise, do not add validation to library-owned behavior unless the application's own format or syntax requires it.

## Test architecture

Unit tests live inside the individual implementation files. They cover application parsing, native configuration resolution, listener ownership, known-host persistence, escape state, input flags, terminal guards, and exit policy. They should exercise meaningful behavior and edge cases rather than repeat the implementation as assertions.

The root Cargo project and `tests/` are separate workspaces. Consequently, `cargo test` at the repository root runs inline unit tests but does not run the executable integration suites. The `tests/` workspace contains six application suites and a shared `tester` support crate:

| Suite | Application behavior |
| --- | --- |
| `cli` | CLI overrides, configuration selection/matching, native value types, and path rules. |
| `stdio` | Shell/exec input, literal arguments, stdout/stderr, exit status, EOF, and full-duplex output. |
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
cargo test --locked
./tests/x.sh
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
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --manifest-path tests/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path tests/Cargo.toml --workspace --all-targets -- -D warnings
```

Select additional platform or artifact checks when a change affects termios, platform-specific code, native compilation, or linking. A Linux unit-test pass does not establish macOS terminal behavior, and a macOS release build does not establish that its tests ran.

## CI and releases

`dist-workspace.toml` configures cargo-dist 0.32.0, GitHub CI, archive artifacts without generated installers, and the release target matrix. The `dist` Cargo profile inherits `release`, uses thin LTO, and strips symbols. The Apple Silicon runner override is `macos-15`; keep runner selection in the dist configuration rather than relying on a retired default label.

`.github/workflows/release.yml` runs on pull requests and version-like tags. Its Ubuntu planning job installs stable Rust, runs `cargo +stable test --locked`, and runs `./tests/x.sh` before invoking dist planning or release creation. Artifact build jobs depend on that job. The publishing job also explicitly requires successful planning, so the test gate cannot be bypassed through skipped artifact jobs. The default pull-request path plans the release without building all release artifacts unless dist's PR run mode requests uploads.

The test gate currently executes on Linux. macOS jobs build artifacts; they do not run the unit or integration suites. Preserve this distinction when interpreting a successful workflow.

The generated workflow contains intentional test-gate customizations. `allow-dirty = ["ci"]` permits those changes to survive dist's generated-file check. Preserve the unit/E2E steps and dependency gates when regenerating or upgrading the workflow. Inspect `dist plan` when changing the target/runner matrix, and review generated workflow changes alongside the dist configuration; the dirty-CI allowance itself does not verify that custom edits still enforce the intended gate.
