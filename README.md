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

TOML uses `[default]` and `[hosts.NAME]`, with kebab-case field names. Named hosts
can also use TOML inline-table syntax. Lua returns a table with `default` and `hosts`,
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
| `proxy-jump` | `proxy_jump` | String or list; Lua also accepts inline jump-host tables | No jump hosts |
| `kex-algorithms` | `kex_algorithms` | Ordered comma-separated string or array of algorithm names | Transport defaults |
| `host-key-algorithms` | `host_key_algorithms` | Ordered comma-separated string or array of host-key algorithm names | Transport defaults with known-host preferences |

Both native formats also accept `defaults` and the other format's field-name
spelling. Host names are never case-folded.

### Boolean and TTY values

`identities-only` accepts `true`, `false`, and the case-insensitive strings
`"true"`, `"false"`, `"yes"`, and `"no"`.
It filters identities used for user authentication; it does not disable the agent.

`request-tty` accepts the same values, plus `"auto"` and `"force"`:

- `true`, `"true"`, or `"yes"`: request a PTY when local stdin is a terminal.
- `false`, `"false"`, or `"no"`: do not request a PTY.
- `"auto"`: request a PTY for a remote shell session.
- `"force"`: request a PTY even when local stdin is not a terminal.

### Algorithm preferences

Native host and default tables can select key-exchange and server host-key
algorithms:

```toml
[default]
kex-algorithms = "curve25519-sha256,mlkem768x25519-sha256"

[hosts.work]
host-key-algorithms = ["ssh-ed25519", "rsa-sha2-512"]
```

Use `kex_algorithms` and `host_key_algorithms` in Lua. A host value replaces the
default table's value. Prefix the first algorithm with `+` to append, `-` to
remove, or `^` to prepend relative to the transport's built-in defaults;
without a prefix, the list replaces those defaults. For example,
`host_key_algorithms = { "^rsa-sha2-512", "ssh-ed25519" }` puts those keys first.
Modifiers use the transport defaults, whose certificate list is empty.
Unknown names in replacement, append, or prepend lists are errors; removal
ignores names absent from the defaults. Wildcards, empty lists, and empty
results are errors.

Host-certificate names such as `ssh-ed25519-cert-v01@openssh.com` are supported.
The transport requires all certificate algorithms before plain host-key
algorithms; a list or modifier result that puts a certificate after a plain key
is rejected. An explicit host-key list is used as supplied without known-host
reordering or added algorithms/certificates. Omitting the setting preserves the
known-host preference behavior. Each jump host has its own policy, including
Lua inline hosts. KEX extension signals are managed by the client, not supplied
as algorithm names. CLI `-C`, `-c`, and `-m` retain their existing behavior.

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

### Application setup

The optional top-level `meta` table configures resources for the invocation,
separately from host settings. Its only field is the known-host database path:

```toml
[meta]
known-hosts = "~/.ssh/known_hosts"
```

```lua
local home = assert(os.getenv("HOME"))
return { meta = { known_hosts = home .. "/.ssh/known_hosts" } }
```

TOML expands tildes using the path rules above; Lua requires a literal absolute
path or an expression producing one. This path has no environment-reference
syntax. `meta` is not a host/default field, and unknown setup fields are errors.
If omitted, the library's default known-host path is used. The selected file is
used for checks and for saving accepted host keys across all jump hosts and the
destination. A missing file starts with empty trust; other read failures stop
setup.

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
exact matching and defaults as the destination.

### Lua jump hosts

Lua `proxy_jump` accepts a string, an inline host table, or a sequence containing
either:

```lua
proxy_jump = {
    { host_name = "bastion1.example.com", user = "me" },
    { host_name = "bastion2.example.com" },
    "bastion3",
}
```

Inline hosts inherit defaults and accept the same fields as named hosts,
including nested `proxy_jump`. Their `host_name` is a literal network destination
and must be supplied by the table or defaults. String jumps use host aliases.
Only the first jump's nested `proxy_jump` adds ancestors. Inline jump tables are
Lua-only. Avoid cyclic jump definitions.

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

Parsing another directive does not enable that feature. OpenSSH-file
`KexAlgorithms` and `HostKeyAlgorithms` remain ineffective; native fields above
provide those settings. Forwarding, cipher, MAC, and compression preferences
use CLI options. `UserKnownHostsFile` does not change the host-key database.
Unsupported and unknown directives follow the library parser's rules.

See the [configuration reference](docs/configuration.md) for complete field,
validation, inheritance, and jump-chain rules.
