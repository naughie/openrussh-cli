# Runtime reference

This document describes the CLI's behavior and its boundary with `openrussh-client`. Configuration formats and their resolution are described in [Configuration architecture](architecture.md). The executable is named `ssh`; its implementation is an application around `openrussh-client`, rather than a replacement implementation of SSH transport, authentication, or channels.

## Invocation and argument semantics

```text
ssh [OPTIONS] DESTINATION [COMMAND [ARGUMENT ...]]
```

`DESTINATION` accepts `[user@]host[:port]` and `ssh://[user@]host[:port]`, with an optional trailing `/`. An IPv6 address may be unbracketed when it has no port; use brackets when specifying a port, as in `user@[::1]:2222`. An empty user, empty host, malformed bracketed address, or invalid `u16` port is rejected. `-l` and `-p` override the user and port embedded in the destination.

| Option | Meaning |
| --- | --- |
| `-F CONFIG` | Select a configuration file by its filename extension; `none` disables configuration files. |
| `-l USER`, `-p PORT` | Override the destination user and port. |
| `-i IDENTITY` | Supply a user identity file; repeatable, preserving order. |
| `-J JUMP` | Supply a comma-separated jump-host chain. `none`, compared without regard to ASCII case, clears the chain. |
| `-t`, `-tt`, `-T` | Request a remote PTY when local stdin is a terminal, force a remote PTY, or disable the remote PTY. |
| `-n` | Send channel EOF without reading local stdin. |
| `-N` | Keep the connection and local forwards alive without starting a remote shell or command. |
| `-W HOST:PORT` | Connect local stdin/stdout to a TCP endpoint reached from the SSH server. |
| `-L FORWARD` | Create a local TCP or Unix-domain listener; repeatable, preserving order. |
| `-g` | Bind unspecified local TCP listener addresses on all interfaces. |
| `-e CHAR` | Set the interactive escape prefix; the default is `~`, and `none` disables escapes. |
| `-C` | Prefer compression. |
| `-c CIPHERS` | Set an ordered, comma-separated cipher preference list. |
| `-m MACS` | Set an ordered, comma-separated message authentication code preference list. |
| `-V`, `-h`, `--help` | Print the version or usage. |

Repeated scalar options use the last occurrence. `-t` and `-T` follow command-line order: a later `-T` cancels earlier `-t` occurrences, and a later `-t` cancels `-T`. More than one active `-t` forces allocation. `-N` and `-W` select distinct session modes, conflict with each other and a remote command, and disable the remote PTY.

Omitting `COMMAND` starts a remote shell. A command supplies a program and an ordered argument list. Tokens following the program are its arguments, including tokens beginning with `-`. CLI connection settings take precedence over file settings; details of file selection, host matching, defaults, and list merging belong to the configuration reference.

The CLI does not expose arbitrary `-o` settings, verbose `-v` logging, subsystem requests, remote forwarding (`-R`), dynamic SOCKS forwarding (`-D`), agent forwarding (`-A`), or X11 forwarding (`-X`). Parsing a directive in an OpenSSH configuration file does not itself add an application feature or extend the dependency's implementation.

## Connection, authentication, and commands

The application calls `Connection::connect()` with the resolved destination/jump chain and its handler factory. The dependency owns connection establishment, SSH algorithm negotiation, and user authentication. The configuration layer supplies identity paths, user certificates, and agent sockets according to the selected format's rules. The dependency uses those resolved settings for authentication; the CLI does not reproduce that state machine or add authentication methods.

The application uses `Connection::shell()` for shells and `Connection::exec()` for commands. It builds commands with `Command::new().prog(...).arg(...).complete(false)`. The dependency quotes each program and argument for the server's shell, preserving the boundaries supplied to the CLI. There is no unchecked command-string construction.

An SSH `exec` request carries a command string, rather than an operating-system argument vector. The quoting step preserves literal arguments through the remote shell; it does not make a positional CLI argument into shell syntax. To request a shell expression, invoke the shell explicitly:

```sh
ssh my-host sh -c 'printf "%s\n" "$HOME"; printf "%s\n" done'
```

User authentication keys, server host keys, key-exchange algorithms, ciphers, and message authentication codes have distinct roles. The handler's host-key check authenticates the server; it does not select or verify the client's user authentication identity. Cipher and MAC preferences modify the dependency's transport configuration, while known-host information may adjust server host-key algorithm preferences.

## Standard streams and exit status

Input and output are pumped concurrently. Local stdin is sent as channel input; ordinary channel data goes to stdout and extended stderr data goes to stderr. Each output chunk is written and flushed. This concurrency is necessary when both endpoints write enough data to fill their respective buffers.

Local input EOF sends SSH channel EOF after forwarding an unfinished escape prefix. The output side continues to drain, including a queued remote exit status. A peer closing its input early (`SendError` or `BrokenPipe`) also allows output and status collection to finish. Other local input and SSH errors propagate as failures. `-n` takes the EOF path without opening a local input reader.

| Completion | Local exit status |
| --- | --- |
| Remote command or shell reports `exit-status` | The dependency's Unix status conversion retains the low eight bits. |
| Channel closes without `exit-status` | `0`, following the current dependency's initial status. |
| Local runtime/configuration/connection error | `255`, with `ssh: ...` on stderr. |
| Interactive disconnect escape | `255`. |
| Connection closes during `-N` | `255`. |

The current dependency does not translate an SSH `exit-signal` message into the local process being terminated by that signal. Its absence of an `exit-status` therefore follows the no-status behavior above. Clap handles its own help, version, and argument-parsing exits before the runtime path.

When a connected operation ends, the application sends an SSH disconnect with reason `ByApplication`. An existing operation error takes precedence over a disconnect error. A disconnect error can turn an otherwise successful operation into a failure while the connection handle remains open.

## PTYs, local terminal state, and escapes

The remote PTY policy comes from the resolved configuration: `Never` disables it, `Always` requests it, `IfTty` checks local stdin, and `ForShell` requests it for a shell session. The dependency makes the actual PTY request. A remote PTY and a local terminal are separate resources: local raw mode is entered only for an interactive session whose stdin is a terminal and whose input is enabled.

The local terminal guard saves termios attributes, enters raw mode with `TCSADRAIN`, and restores the saved attributes on drop. Terminal size comes from the borrowed terminal descriptor, with `80 × 24` as the fallback. The input reader duplicates stdin's descriptor while preserving the original file-status flags. Its nonblocking guard restores those shared flags when reading ends or the operation is cancelled. Regular files use a Tokio file reader when the asynchronous descriptor backend cannot register them.

Escape processing is enabled for sessions requesting a PTY, except `-W`. A prefix is recognized at the start of input and immediately after carriage return or newline. Prefix state survives input-buffer boundaries.

| Default escape | Action |
| --- | --- |
| `~.` | Close the channel and exit with status `255`. |
| `~` followed by Ctrl-Z | Suspend the local client after restoring terminal modes and input flags. |
| `~~` | Send one literal `~`. |
| `~?` | Print escape help to stderr. |

Unrecognized sequences are sent literally. A lone prefix at input EOF is also sent literally. `-e` accepts one ASCII byte, caret notation `^@` through `^_`, or `^?` for DEL. A custom prefix changes the prefix for all four actions. `-e none` forwards all input bytes without escape interpretation.

## Signals and suspension

The application registers Unix signal streams for `SIGHUP`, `SIGINT`, `SIGQUIT`, `SIGTERM`, `SIGWINCH`, `SIGTSTP`, and `SIGCONT`. These are local process signals. They are handled by the client lifecycle rather than encoded as SSH signal requests. In a remote PTY session, control characters written through the terminal remain subject to the remote terminal's line discipline.

| State | `SIGHUP`, `SIGINT`, `SIGQUIT` | `SIGTERM` |
| --- | --- | --- |
| Connecting, including host-key confirmation | `255` | `255` |
| Connected shell or command | `255` | `255` |
| Connected `-N` or `-W` | `255` | `0` |

These statuses are subject to the normal cleanup/error policy. Local forwarding tasks are stopped before the connection is disconnected.

`SIGTSTP` restores terminal attributes and shared input flags where applicable, then stops the local process with `SIGSTOP`. After continuation, the client restores its reading/raw-mode state and sends the current window size for an interactive channel. The suspend escape uses the same restoration sequence. `SIGWINCH` and `SIGCONT` refresh raw mode and send a window-change request for an interactive channel. While connecting, resize and continue events are ignored; suspension still stops and resumes the local process. `-N` has no terminal channel to resize.

## Local and standard-stream forwarding

Supported `-L` forms are:

```text
port:host:hostport
bind_address:port:host:hostport
local_socket_path:remote_socket_path
```

TCP address parsing supports bracketed IPv6 addresses. Ports must fit `u16`, and the target host cannot be empty. Both paths in the Unix-domain form must contain `/`; the forwarding syntax does not impose the absolute-path rule used for native configuration identity paths. TCP-to-Unix and Unix-to-TCP combinations are rejected.

An unspecified TCP bind address means loopback, or all interfaces with `-g`. An explicit empty bind address or `*` means all interfaces. An explicit address remains explicit regardless of `-g`. Loopback and wildcard bindings try both IPv4 and IPv6; IPv6 sockets use `IPV6_V6ONLY` so each listener has a distinct address family. An explicit hostname is resolved to its socket addresses. A forwarding specification succeeds if at least one of its addresses can be bound, and fails if none can be bound.

All requested local listeners are bound before accepting connections. If a later specification fails, earlier bindings are dropped. A Unix-domain listener never removes an existing path to make room. Its cleanup guard records the created socket's device and inode and removes the path only if that same filesystem object is still present, preserving replacements and symlinks.

Each listener accepts streams and starts a task for each accepted connection. TCP-to-TCP forwarding uses `Connection::forward_local_tcp()` and supplies the local peer address as the originator. Unix-to-Unix forwarding uses `Connection::forward_local_uds()`. Both wait for the library forwarding handle. Failure of one accepted stream is reported on stderr and leaves the listener running; listener failure or a task panic ends the forwarding operation. Ending the session shuts down listener and stream tasks and releases bindings.

`-W HOST:PORT` has no local listener. It opens an SSH `direct-tcpip` channel with originator `127.0.0.1:0`, wraps that channel in the dependency's `Child`, and uses the standard-stream pump. This lower-level channel opening is necessary because the dependency's local forwarding helpers take a network stream. It does not add a subsystem or a second forwarding implementation.

## Server host-key policy

The application loads known-host data through `KnownHosts::parse_default_path()`. A missing file starts with an empty set; other file-read failures propagate. Entry parsing follows the dependency, which ignores malformed entries. The known-host path follows the dependency's default-path behavior, and application handling does not honor `UserKnownHostsFile` as an override.

The handler delegates known-host matching and host-certificate validation to `KnownHosts`. `Found` accepts the key; `NotFound` starts a trust prompt. Key mismatch, revocation, certificate principal mismatch, critical certificate options, and the dependency's `CertNotVerified` result fail without prompting. The dependency checks host-certificate type, principals, revocation, and CA trust. Its current policy falls back to ordinary public-key matching if a certificate from a trusted CA fails signature or validity-period validation; the application preserves that result.

The prompt opens `/dev/tty`, independently of redirected stdin/stdout. It displays the public-key algorithm and SHA-256 fingerprint and accepts `yes` or the complete displayed fingerprint; `no` or terminal EOF rejects it. Other answers repeat the prompt. Without an accessible controlling terminal, confirmation fails.

Acceptance appends an unhashed host-key entry and caches that accepted key for the handler instance. A later different key is not covered by the cached acceptance. Hostnames are normalized to lowercase; the known-host token is the hostname for port `22` and `[hostname]:port` otherwise. The save path is `~/.ssh/known_hosts`, with `/etc/ssh/ssh_known_hosts` as the no-home fallback. Newly created parent directories use mode `0700` and newly created files use mode `0600`; existing permissions are retained. Appends take a file lock and preserve existing content, inserting a missing final newline. Save errors fail the connection rather than silently discarding the user's acceptance.
