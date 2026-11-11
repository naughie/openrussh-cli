# openrussh-cli

# Prerequisites

Building from source requires a C toolchain, including a compiler and linker.
Lua 5.4 is bundled; release binaries do not require a separate Lua installation.

# Install

## Release binaries

Download and extract an archive from [Releases](https://github.com/naughie/openrussh-cli/releases).
From the extracted directory, install `ssh` into a directory on your `PATH`:

```sh
mkdir -p "$HOME/.local/bin"
install -m 755 ssh "$HOME/.local/bin/ssh"
```

Add the directory to your shell configuration if needed:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## From source

Source builds require a Unix-like operating system and Rust with Cargo.

```sh
git clone https://github.com/naughie/openrussh-cli.git
cd openrussh-cli
cargo install --locked --path .
```

Ensure `$HOME/.cargo/bin` is on your `PATH`.

# Configuration

Without `-F`, the first existing file is used:

1. `~/.ssh/config.lua`
2. `~/.ssh/config.toml`
3. `~/.ssh/config`

Files are not combined. A read, syntax, execution, or configuration error stops
loading; it does not cause fallback to another format. If none exists, defaults
apply.

`-F PATH` selects one file: `.lua` selects Lua, `.toml` selects TOML, and other
extensions select OpenSSH syntax. Extensions are case-sensitive. `-F none` skips
configuration files.

## Examples

`~/.ssh/config.toml`:

```toml
[default]
identities-only = true
identity-file = "~/.ssh/id_ed25519"

[hosts.github]
host-name = "github.com"
user = "git"

[hosts.my-host]
host-name = "example.com"
user = "alice"
port = 2222
request-tty = "yes"
```

`~/.ssh/config.lua`:

```lua
local home = assert(os.getenv("HOME"))

return {
    default = {
        identities_only = true,
        identity_file = home .. "/.ssh/id_ed25519",
    },
    hosts = {
        github = { host_name = "github.com", user = "git" },
        ["my-host"] = {
            host_name = "example.com",
            user = "alice",
            port = 2222,
            request_tty = "yes",
        },
    },
}
```

## TOML and Lua

TOML uses `[default]` and `[hosts.NAME]`, with kebab-case field names. Inline
host tables are also accepted. Lua returns a table with `default` and `hosts`,
using snake_case field names. Lua is evaluated synchronously once per load and
may use local variables, functions, and standard libraries.

Host names match exactly and case-sensitively. `*`, `?`, and `!` in host names
are literal characters. Neither format supports an `Include` directive.
Unknown fields and invalid types are errors, including in unused hosts.

| TOML field | Lua field | Type | Default |
| --- | --- | --- | --- |
| `host-name` | `host_name` | String | Supplied host name |
| `user` | `user` | String | Current OS user |
| `port` | `port` | Integer or decimal string, 0–65535 | `22` |
| `identity-file` | `identity_file` | Path string or array of path strings | No configured identity files |
| `certificate-file` | `certificate_file` | Path string for an SSH user certificate | None |
| `identity-agent` | `identity_agent` | Authentication-agent socket path; see below | TOML: `SSH_AUTH_SOCK`; Lua: none |
| `identities-only` | `identities_only` | Boolean or boolean string | `false` |
| `request-tty` | `request_tty` | Boolean or TTY-mode string | `"auto"` |
| `proxy-jump` | `proxy_jump` | Comma-separated string or array of strings | No jump hosts |

For compatibility, both native formats also accept `default`/`defaults` and the
other format's field-name spelling. Host names are never case-folded.

### Boolean and TTY values

`identities-only` accepts `true`, `false`, and the case-insensitive strings
`"true"`, `"false"`, `"yes"`, and `"no"`.
It filters identities used for user authentication; it does not disable the agent.

`request-tty` accepts the same values, plus `"auto"` and `"force"`:

- `true`, `"true"`, or `"yes"`: request a PTY when local stdin is a terminal.
- `false`, `"false"`, or `"no"`: do not request a PTY.
- `"auto"`: request a PTY for a remote shell session.
- `"force"`: request a PTY even when local stdin is not a terminal.

### Paths and authentication agents

Configured identity files, SSH user certificate files, and agent sockets must
resolve to absolute paths. Relative and empty paths are errors. Files are not
checked for existence or key format during configuration loading.

TOML expands `~` and `~user`; `~user` must name the current OS user. Agent values
may also be `"SSH_AUTH_SOCK"` or `"$VARIABLE"`, referring to the whole environment
variable's value. The result must be absolute. An unset variable, or the special
agent value `"none"`, falls back to `SSH_AUTH_SOCK`.

Lua does not expand tildes or environment-variable references in path strings.
Use Lua expressions such as `os.getenv("HOME") .. "/.ssh/id_ed25519"` or
`identity_agent = os.getenv("SSH_AUTH_SOCK")`. A configured `"none"` agent value
is not a path and is rejected. If neither the host nor the default table sets
`identity_agent`, no authentication agent is used. Lua configuration never reads
`SSH_AUTH_SOCK` implicitly.

### Defaults and overrides

Scalar settings use command-line overrides, then the selected host, then the
default table. Omitted settings use the defaults listed above. `-l`, `-p`, and
`-t`/`-T` override the corresponding final-host settings.

Identity files accumulate in this order: repeated `-i` options, host identities,
then default identities. An empty identity array adds no files; it does not
remove inherited files. `certificate-file` specifies one certificate.

`-J` replaces the final host's jump list. Otherwise a host's `proxy-jump` replaces
the default list. Use `[]` in TOML, `{}` in Lua, or `-J none` to clear jumps.
The native string `"none"` is a literal host name. Jump-host aliases use the same
exact matching and defaults as the destination. Avoid cyclic jump definitions.

## OpenSSH format

OpenSSH-syntax files use the parser and configuration semantics of
`openrussh-client` 0.4.6, including host patterns, `Include`, defaults, path
expansion, and authentication-agent selection. The native-format boolean and
absolute-path rules do not apply to these files.

| Directive | Setting |
| --- | --- |
| `Host` | Host patterns, including negated patterns |
| `HostName`, `User`, `Port` | Network destination, SSH account, and TCP port |
| `IdentityFile` | Authentication identity path; repeatable |
| `CertificateFile` | One SSH user certificate path |
| `IdentityAgent` | Authentication-agent socket path or environment reference |
| `IdentitiesOnly` | Identity filtering; enabled by the literal `yes` |
| `RequestTTY` | `no`, `yes`, `force`, or the default shell-only policy |
| `ProxyJump` | Comma-separated jump hosts |

Matching blocks use the first scalar value; identity files accumulate. CLI
user, port, identity, and jump settings precede file settings. The CLI PTY
override applies to the final destination after resolution.

`RequestTTY` recognizes `no`, `yes`, and `force` case-insensitively; other values
use the shell-only policy. Identity and certificate paths may be relative.
Tilde expansion supports the current user; paths that cannot be expanded are
omitted. `IdentityAgent` uses the same environment-reference syntax and
`SSH_AUTH_SOCK` fallback as TOML, with the library's permissive path handling.

Parsing another directive does not enable that feature. Forwarding and
algorithm preferences use CLI options; `UserKnownHostsFile` does not change the
default host-key database. Unsupported and unknown directives follow the
library parser's rules.
