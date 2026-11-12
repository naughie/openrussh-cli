# Configuration reference

This document specifies configuration discovery, the three input formats, and how a selected host becomes a connection configuration. It describes the behavior of this client and its locked `openrussh-client` 0.4.6 dependency. OpenSSH syntax support does not imply compatibility with every OpenSSH client option.

The [architecture reference](architecture.md) defines the types and conversion boundaries. The [runtime reference](runtime.md) describes command execution, terminal handling, forwarding, and host-key verification.

## Source selection

An explicit `-F PATH` selects one file. Its extension determines the format:

| Extension | Interpretation |
| --- | --- |
| `.lua` | Execute a Lua 5.4 chunk and deserialize its returned table. |
| `.toml` | Deserialize a TOML document. |
| Anything else, including no extension | Parse OpenSSH configuration syntax. |

Extension matching is case sensitive. The client does not detect the format from file contents. `-F none` skips file discovery and file parsing while retaining command-line options and built-in defaults.

Without `-F`, the client tries these paths in order:

1. `~/.ssh/config.lua`
2. `~/.ssh/config.toml`
3. `~/.ssh/config`

The first file that can be opened is selected. Only a `NotFound` error permits trying the next candidate. A permission error, read error, syntax error, Lua execution error, invalid value, or resolution error stops loading; it does not select a lower-priority file. The files are alternatives and are not merged. If all three are absent, built-in defaults and command-line options determine the connection. Automatic discovery fails if the home directory cannot be determined; an explicit `-F` bypasses that discovery. The client does not automatically read `/etc/ssh/ssh_config`.

TOML and Lua source files are read as UTF-8. File descriptors are closed after reading or parsing; they are not held for the SSH session.

For Lua, the selected source path is canonicalized to an absolute path,
resolving relative components and symbolic links. This path supplies the
`cli` module's file metadata and Lua's chunk name in error messages. A failure
to canonicalize the source stops loading with the selected filename in the
error context.

## OpenSSH format

OpenSSH files are parsed and queried through the library's `ssh2-config` integration using `ParseRule::ALLOW_UNSUPPORTED_FIELDS`. Host matching, negation, matching-block order, `Include`, parsing rules, and include scope follow that implementation. Native TOML/Lua validation rules do not apply to OpenSSH files.

The directives that affect the resolved connection are:

| Directive | Effect |
| --- | --- |
| `Host` | Select matching host blocks using the library's pattern rules. |
| `HostName` | Set the network destination name. |
| `User` | Set the SSH account name. |
| `Port` | Set the TCP destination port. |
| `IdentityFile` | Supply authentication identities in order. |
| `CertificateFile` | Supply an SSH user certificate for public-key authentication. |
| `IdentityAgent` | Select an authentication-agent socket or an environment reference. |
| `IdentitiesOnly` | Control the library's identity filtering behavior. |
| `RequestTTY` | Select the library's PTY allocation policy. |
| `ProxyJump` | Describe intermediate SSH servers. |

Recognized directives outside this effective set may be parsed or retained by `ssh2-config` without affecting the connection. The application does not implement their OpenSSH semantics. Completely unknown directives remain subject to the parser's errors and `IgnoreUnknown` behavior; `ALLOW_UNSUPPORTED_FIELDS` is not `ALLOW_UNKNOWN_FIELDS`.

For example, `Compression`, `Ciphers`, `MACs`, and `LocalForward` in an OpenSSH file do not configure this client's transport preferences or local listeners. Their corresponding supported command-line options are application settings. `KexAlgorithms` and `HostKeyAlgorithms` also retain their existing parser behavior without affecting negotiation; the native fields below provide that configuration. `UserKnownHostsFile` does not change this client's host-key database; see [host-key verification](runtime.md).

For scalar settings, the first value among matching host blocks wins. `IdentityFile` entries accumulate across matching blocks. CLI user, port, identity, and jump overrides are inserted before file settings; CLI identity entries therefore precede file entries. The final destination's CLI PTY override is applied after host resolution.

The library's path and value behavior is preserved. OpenSSH identity paths may be relative; the native absolute-path requirement is not imposed on them. Tilde expansion supports the current user's home directory, and paths that cannot be expanded are omitted rather than rejected. `IdentityAgent` supports the same whole-value environment references and `SSH_AUTH_SOCK` fallback described for TOML below, while retaining the library's permissive path behavior.

OpenSSH values do not pass through the native boolean conversions. `IdentitiesOnly` is enabled only by the literal string `yes`. `RequestTTY` recognizes `no`, `yes`, and `force` without regard to ASCII case; other values use the default shell-only policy. The application does not prevalidate identity-file contents or replace the library's authentication selection logic.

```sshconfig
Host github
    HostName github.com
    User git
    Port 22
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes
    RequestTTY no
```

## Native formats: TOML and Lua

TOML and Lua describe the same host fields. Lua additionally supports inline
host records in jump settings. Their canonical spellings differ:

| Concept | TOML | Lua |
| --- | --- | --- |
| Application setup | `[meta]` | `meta = { ... }` |
| Default host settings | `[default]` | `default = { ... }` |
| Named hosts | `[hosts.NAME]` or an inline entry in `[hosts]` | `hosts = { NAME = { ... } }` |
| Multiword field names | `kebab-case` | `snake_case` |
| Ordered list | TOML array | Lua sequence table |

Both `default` and `defaults` are accepted root names. The current schemas also
accept both kebab-case and snake_case spellings for the listed multiword fields
in either format. These are spelling aliases for one field, not independent
settings; supplying both aliases for the same field is an error. Unknown
sections and fields are errors, including `Include`/`include`.

Default and host tables are optional. Every native host key is an exact, case-sensitive lookup key. A key such as `"dev-*"`, `"server?"`, or `"!private"` is literal; it has no wildcard or negation meaning. Matching uses the requested alias before `host_name` replaces the network destination. The same rule applies when looking up jump-host aliases.

### Fields

The table gives canonical TOML names and their Lua equivalents. The fields are valid in both the defaults record and an individual host record.

| TOML / Lua | Accepted values | Meaning and omitted value |
| --- | --- | --- |
| `host-name` / `host_name` | String | Network destination name; falls back to the requested alias for named hosts. A Lua inline jump requires this field from its own record or defaults. |
| `user` | String | SSH account name; falls back to the operating-system username obtained through `whoami`. |
| `port` | Integer or decimal string | TCP destination port; defaults to `22`. |
| `identity-file` / `identity_file` | Path string or array of path strings | Ordered authentication identities; no list is configured when omitted everywhere. |
| `certificate-file` / `certificate_file` | Path string | One SSH user certificate; absent when omitted everywhere. |
| `identity-agent` / `identity_agent` | String, with format-specific rules below | Authentication-agent Unix-domain socket; TOML falls back to `SSH_AUTH_SOCK`, while Lua has no implicit agent. |
| `identities-only` / `identities_only` | Boolean or accepted boolean string | Identity-filtering policy; defaults to `false`. |
| `request-tty` / `request_tty` | Boolean or accepted TTY string | PTY allocation policy; defaults to `auto`. |
| `proxy-jump` / `proxy_jump` | String or list; Lua also accepts inline host records | Ordered jump-host specifications; absent when omitted everywhere. See [jump settings](#jump-settings). |
| `kex-algorithms` / `kex_algorithms` | Comma-separated string or array of algorithm-name strings | Ordered key-exchange preferences; transport defaults when omitted everywhere. |
| `host-key-algorithms` / `host_key_algorithms` | Comma-separated string or array of algorithm-name strings | Ordered server host-key preferences; existing known-host preference behavior when omitted everywhere. |

`port` is decoded as an unsigned 16-bit integer. The accepted numeric range is `0..=65535`; there is no additional nonzero-port validation. TOML integer notation, including separators and radix prefixes, is decoded by the TOML parser. String ports use decimal parsing. Negative integers, floating-point values, booleans, and out-of-range values are errors.

An identity is used for SSH user authentication. Depending on the library's authentication mode, an identity file may hold a private key or identify a public key whose private-key operation is performed by an authentication agent. `certificate_file` is an SSH user certificate, not a TLS/X.509 certificate and not a server host-key trust file. `identities_only = true` restricts the identities selected by the library; it does not disable the authentication agent. Authentication itself remains delegated to `Connection::connect`.

Omitting `identity_file` does not cause this client to add conventional private-key filenames automatically. An explicitly empty list remains distinguishable from an omitted list.

### Algorithm preferences

`kex_algorithms` selects key-exchange methods. `host_key_algorithms` selects
server host-key signature algorithms and host-certificate algorithms; it does
not select the client's user authentication identities. Each accepts an ordered
comma-separated string or an array of names. For example, these express the
same host-key preference:

```toml
[hosts.string-form]
host-key-algorithms = "ssh-ed25519,rsa-sha2-512"

[hosts.array-form]
host-key-algorithms = ["ssh-ed25519", "rsa-sha2-512"]
```

The equivalent Lua value is
`host_key_algorithms = { "ssh-ed25519", "rsa-sha2-512" }`. Prefix only the first
name to choose a list operation:

| Prefix | Operation |
| --- | --- |
| None | Replace the transport's default list. |
| `+` | Append the supplied names to the default list. |
| `-` | Remove the supplied names from the default list by exact name. |
| `^` | Put the supplied names before the remaining default names. |

For example, `kex-algorithms = "+ecdh-sha2-nistp256"` appends a method, while
`host_key_algorithms = { "^rsa-sha2-512", "ssh-ed25519" }` puts those algorithms
first. Modifiers use `russh::Preferred::default()` as their baseline, before any
known-host preference adjustments. That baseline contains no host-certificate
algorithms. A host's supplied value overrides the default table's entire value;
its modifier does not operate on an inherited list.

Replacement, append, and prepend names must be supported by the locked
transport. Removal compares exact names and ignores names absent from the
default list. Wildcard patterns, empty lists, empty entries, and operations
leaving no algorithms are errors. KEX extension signals such as `ext-info-c`
and the strict-KEX markers are preserved from the transport defaults by the
application adapter and are not configurable algorithm names.

Host-certificate names use their full SSH identifiers, for example
`ssh-ed25519-cert-v01@openssh.com` or
`rsa-sha2-512-cert-v01@openssh.com`. Certificate and plain-key choices are
independent: a certificate-only list does not also enable the plain key.
The transport advertises certificate algorithms before plain host-key
algorithms. A replacement list or modifier result that places any certificate
after a plain key is rejected because that order cannot be represented.

When `host_key_algorithms` is supplied, its resulting list is the complete
allowed preference list. Known-host data still verifies the server's identity,
but does not reorder the list or add keys or certificates. When omitted in both
host and defaults, the existing known-host preference behavior applies.
Each named or inline jump host resolves its own algorithm settings from its
record and defaults. CLI cipher, MAC, and compression options are unchanged.

### Application setup metadata

The optional top-level `meta` table supplies application setup for the entire
invocation. It is separate from the host/default records and is not inherited
through a jump chain. Its supported fields are `known-hosts` and `log-level`
in TOML, or `known_hosts` and `log_level` in Lua, with the same spelling aliases
as other native fields. Unknown metadata fields and a `meta` field inside a
host/default record are errors.

```toml
[meta]
known-hosts = "~/.ssh/known_hosts"
log-level = "debug,russh=info"
```

```lua
local cli = require("cli")
local home = assert(cli.home_dir())
return {
    meta = {
        known_hosts = home .. "/.ssh/known_hosts",
        log_level = "debug,russh=info",
    },
}
```

The configured path must be absolute after TOML's tilde expansion. TOML supports
the current-user `~user` form under the native path rules; it does not expand
environment variables in this field. Lua treats the value as a literal path:
use `cli.home_dir()` or Lua expressions explicitly to construct an absolute path.
Relative or empty values are errors. Neither form treats `SSH_AUTH_SOCK` or
`$VARIABLE` as a known-host path selector.

Omitting the path uses the dependency's default: `~/.ssh/known_hosts`, or
`/etc/ssh/ssh_known_hosts` when no home directory is available. Setup loads the
selected file once for all destination and jump-host checks. A missing file
starts with empty trust without creating a file; other read errors fail setup.
Accepted keys are saved to that same path. File locking and permission behavior
are described in the [runtime reference](runtime.md#server-host-key-policy).

`log-level` accepts a string using `tracing_subscriber::EnvFilter` directives,
similar to `RUST_LOG`. A directive can supply a global level or select a target
or module prefix, and commas combine directives. For example,
`"debug,russh=info"` sets a global debug level and limits matching `russh` targets
to info. The levels are `off`, `error`, `warn`, `info`, `debug`, and `trace`.
A bare target, such as `"russh"`, enables trace for that target prefix. With
only target directives, unmatched targets are disabled; `"error,russh=info"`
keeps error logging elsewhere. Invalid directives and non-string values are
errors. The exact string `"none"` disables all client log diagnostics. Use
`off` inside combined filters, such as `"off,russh=info"`.

Omitting the field or assigning `nil` in Lua defaults to `"error"` without
CLI `-v` and `"debug"` with `-v`. An explicit empty string remains equivalent
to `"error"` in either mode. TOML has no `nil` value. With CLI `-v`, explicit
directives determine which messages are logged. Without `-v`, those directives
still apply, with an additional error-only limit. For example,
`"debug,russh=info"` cannot enable
debug or info output without `-v`, while `"error,russh=off"` suppresses matching
`russh` errors in either mode. Repeating `-v` does not change the filter. This
setting does not read the `RUST_LOG` environment variable.

Log filtering does not suppress remote stdout/stderr, host-key questions, or
interactive escape help. See [runtime logging](runtime.md#logging) for output
formatting. OpenSSH-file `LogLevel` does not configure this setting.

### Boolean and TTY values

Boolean strings are case insensitive.

| Input | `identities_only` | `request_tty` |
| --- | --- | --- |
| `true`, `"true"`, `"yes"` | `true` | Request a PTY when local standard input is a terminal. |
| `false`, `"false"`, `"no"` | `false` | Never request a PTY. |
| `"auto"` | Invalid | Request a PTY for a remote shell. |
| `"force"` | Invalid | Request a PTY regardless of whether local standard input is a terminal. |

Other strings, numbers, arrays, and tables are errors for these fields. An explicit `false` or `"no"` in a host record overrides a true default; omission alone permits inheritance. Command-line `-t`/`-tt`/`-T` still takes precedence over the file's PTY policy. `-N` and `-W` select no PTY.

### Path rules

Native configuration paths must be absolute after the format's permitted expansion. This rule covers `identity_file`, `certificate_file`, `identity_agent`, and the top-level `meta.known_hosts` path.

| Input | TOML | Lua |
| --- | --- | --- |
| Absolute path | Used as specified. | Used as specified. |
| `~/...` or `~` | Expand using `std::env::home_dir()`. | Relative literal path: error. |
| `~user/...` | Expand only when `user` is the current username. A selected path that cannot be expanded is an error. | Relative literal path: error. |
| Relative or empty path | Error. | Error. |
| Agent environment reference | Supported as described below. | Not expanded; a relative literal is an error. |

TOML tilde expansion applies only to the first path component. It does not look up arbitrary users' home directories. Ordinary identity and certificate paths do not expand `$HOME` or other environment variables.

Lua must construct expanded values itself, for example:

```lua
local cli = require("cli")
local home = assert(cli.home_dir())
return {
    hosts = {
        work = {
            identity_file = home .. "/.ssh/id_ed25519",
            identity_agent = os.getenv("SSH_AUTH_SOCK"),
        },
    },
}
```

The client does not expand `~`, `$VARIABLE`, or a bare environment-variable name inside a configured Lua path. An absolute path containing a literal `$` or `~` remains an absolute path; those characters are not substituted.

The rule is syntactic absoluteness, not filesystem canonicalization. The loader does not require the referenced file or socket to exist, resolve symbolic links, inspect key formats, or verify permissions. Such failures occur when the connection/authentication implementation uses the resource.

Literal relative paths are rejected in every native defaults/host record,
including hosts not selected for this invocation and recursively nested Lua
inline jumps. Expansion results that are used by the selected connection or its
jump hosts must also be absolute. CLI `-i` values retain the library-compatible
CLI path behavior and are not subject to native-file absolute-path validation;
relative CLI paths remain possible.

### Agent environment resolution

After host/default inheritance, TOML interprets the effective `identity_agent` value as follows:

| TOML value | Interpretation |
| --- | --- |
| Absolute path or supported tilde path | Use that socket path after expansion. |
| `"SSH_AUTH_SOCK"` | Read the `SSH_AUTH_SOCK` environment variable. |
| `"$NAME"` | Read the environment variable named `NAME`. |
| `"none"` | No explicit socket is selected; continue to the fallback below. |

The environment reference occupies the entire value. This is not general shell substitution: `"$HOME/agent.sock"` is interpreted as an environment-variable name, not as `$HOME` followed by a suffix.

If TOML omits the effective setting, uses `"none"`, or names an unset variable, resolution falls back to `SSH_AUTH_SOCK`. This intentionally preserves `openrussh-client` 0.4.6 behavior: `"none"` does **not** guarantee that agent use is disabled. An environment value that is present but resolves to a non-absolute path is an error.

An explicit Lua `identity_agent` is always a literal absolute socket path. `"SSH_AUTH_SOCK"`, `"$SSH_AUTH_SOCK"`, `"none"`, and `"~/agent.sock"` are not Lua-side selector syntax. Use `os.getenv` or ordinary Lua string construction. If neither the selected host nor the defaults record supplies `identity_agent`, no authentication agent is configured. This applies to jump hosts as well. The loader never reads `SSH_AUTH_SOCK` implicitly for Lua. Assigning Lua `nil` removes the field and therefore behaves as omission; an inherited defaults value still applies.

### Inheritance and command-line overrides

For the final destination, scalar fields use this order:

1. Explicit command-line override, where that field has a CLI option.
2. The exactly matching host record.
3. The defaults record.
4. Built-in/library defaults.

The destination's embedded user and port, such as `alice@work:2222`, are command-line values. Explicit `-l` and `-p` replace their corresponding embedded values. The host alias itself remains the lookup key; `host_name` supplies the network address.

The list fields have different merge rules:

- `identity_file` accumulates in this order: CLI `-i` entries, host entries, then default entries. Order is preserved and entries are not deduplicated. An empty host list does not remove inherited default identities.
- `proxy_jump` takes the first present list: CLI `-J`, host, then defaults. Lists do not concatenate. An empty native array removes an inherited jump list. CLI `-J none` also supplies an explicit empty list; matching `none` for this CLI option is case insensitive.
- `kex_algorithms` and `host_key_algorithms` take the host value, then the default value. Lists do not concatenate across records. Their optional list modifiers operate on the transport's built-in defaults.

For jump hosts, CLI identity/user/port overrides for the final destination are
not propagated. Each jump alias gets its own host/default settings; an inline
Lua host uses its own record and defaults. A user or port embedded in a jump
string is a fallback below that jump host's configured/default user or port,
matching the library's resolution precedence. CLI `-J` replaces the final
destination's configured jump list, including any inline Lua records in it.

### Jump settings

TOML `proxy-jump` accepts a string or an array of strings. Lua `proxy_jump`
accepts an alias string, an inline host record, or a sequence of either:

```text
Jump       = string | Host
proxy_jump = Jump | sequence of Jump
```

Here `Host` is a native host record with the fields listed above, including
another `proxy_jump`. It has no `name` or `alias` field. The following Lua forms
are supported:

```lua
proxy_jump = "outer,inner"
proxy_jump = { "outer", "inner" }
proxy_jump = { host_name = "bastion.example.com", user = "jump-user" }
proxy_jump = {
    { host_name = "bastion.example.com", identity_file = "/keys/bastion" },
    "inner",
}
```

Each inline host inherits the defaults record using the same scalar,
identity-list, and jump-list rules as a named host. Its effective `host_name`
must come from the inline record or defaults; if neither supplies one,
resolution fails. Unlike a jump string, an inline `host_name` is a literal
network destination: it is not parsed for a user or port and is not looked up in
`hosts`. For example, `{ host_name = "gateway" }` connects to the network name
`gateway` even if `hosts.gateway` exists. Set `user` and `port` explicitly in the
inline record when needed.

The outer value `proxy_jump = {}` is an empty list and clears inherited jumps.
Inside a sequence, an empty record such as `proxy_jump = { {} }` is one inline
host; it still requires `host_name` from defaults. Use a record containing
`proxy_jump = {}` to clear a jump host's inherited ancestors.

Inline jump records are a Lua feature. TOML rejects them even when expressed
with TOML inline-table syntax, and OpenSSH `ProxyJump` retains its string syntax.
Named host records in TOML may still use inline-table syntax.

### Jump-chain traversal

A scalar native string value is split on commas without trimming individual
elements. A list supplies its elements directly; a string entry is one jump
specification and is not split again. Each resulting string is parsed by the
library's `Target::parse`, which is a separate parser from the application's
destination parser. Lua inline records bypass this string parser.

For TOML `proxy-jump = ["a", "b", "c"]` or Lua
`proxy_jump = { "a", "b", "c" }`, connection establishment proceeds through
`a`, then `b`, then `c`, then the destination. Resolution recursively follows
only the first entry's own `proxy_jump`; settings on later entries do not add
further ancestors. This also applies to inline Lua entries. The resolved library
`Chain` stores its bastions in the reverse order used to establish connections.

For example, if the destination names `["a", "b"]` and `a` itself names `["outer"]`, the connection route is:

```text
local client -> outer -> a -> b -> destination
```

Unlike CLI `-J none`, the native file value `proxy_jump = "none"` names a literal host called `none`; it does not clear the list. Use an empty array instead. The current resolver has no cycle detection for recursively inherited first-entry jumps. Configuration must describe an acyclic route; a cycle can prevent resolution from completing.

The same nested route can be expressed with inline Lua hosts:

```lua
proxy_jump = {
    {
        host_name = "a.example.com",
        proxy_jump = { host_name = "outer.example.com", proxy_jump = {} },
    },
    { host_name = "b.example.com" },
}
```

This establishes `outer.example.com`, then `a.example.com`, then
`b.example.com`, then the destination. The explicit empty list on `outer`
prevents inheritance of default ancestors.

### Lua execution and table shapes

Lua configuration is a synchronous Lua 5.4 chunk evaluated once for a load. It must return a table. Local variables, functions, table construction, and environment reads can compute the result. The client does not keep the Lua VM running for the SSH session and does not use `mlua`'s async feature.

The returned table is deserialized through Serde. Ordered lists use contiguous
Lua sequence tables. An empty table is accepted as an empty list where a list is
expected, and as an empty record/map where a record/map is expected. For
`proxy_jump`, a field-name table represents one inline host, a sequence table
represents a list, and the outer empty table represents an empty list. A
sequence may mix string entries and inline host records, but a single table
cannot mix field-name keys with sequence entries. Invalid element types,
non-table root results, and unsupported fields are errors. Syntax and execution
errors retain the selected configuration file's error context.

### Lua modules

Before executing the selected configuration, the client prepends
`CONFIG_DIR/?.lua` and `CONFIG_DIR/?/init.lua`, in that order, to `package.path`.
`CONFIG_DIR` is the canonical parent directory of the selected file, the same
directory exposed as `cli.__dir__`. Modules therefore resolve beside the actual
configuration file even when `-F` uses a relative path or symbolic link. The
existing `package.path` entries remain available after these two patterns.

For example, this layout supports both file and directory modules:

```text
CONFIG_DIR/config.lua
CONFIG_DIR/config/some/module/init.lua
CONFIG_DIR/another/module.lua
```

Inside `config.lua`, load them with dotted module names:

```lua
local some_module = require("config.some.module")
local another_module = require("another.module")
```

These modules follow Lua's normal `require` and `package.loaded` caching rules.
The built-in `cli` module remains registered through `package.preload` and takes
precedence over a local `cli.lua` file.

### Lua `cli` module

`local cli = require("cli")` loads a built-in module supplied by the client.
It provides three lookup functions and five string constants:

| Member | Type | Meaning |
| --- | --- | --- |
| `user_name()` | String or `nil` | Current OS username, obtained through `whoami::username`. |
| `home_dir()` | String or `nil` | Current user's home directory, obtained through `std::env::home_dir`. |
| `host_name()` | String or `nil` | Local machine hostname, obtained through `gethostname(2)`. |
| `__file__` | String | Absolute, canonical path of the selected Lua configuration file. |
| `__dir__` | String | Parent directory of `__file__`. |
| `os` | String | Binary's target operating system, from `std::env::consts::OS`, such as `"linux"` or `"macos"`. |
| `arch` | String | Binary's target CPU architecture, from `std::env::consts::ARCH`, such as `"x86_64"` or `"aarch64"`. |
| `os_family` | String | Binary's target operating-system family, from `std::env::consts::FAMILY`, such as `"unix"`. |

The constants are read-only and are accessed without parentheses, for example
`cli.os == "macos"`. Ordinary assignment, including assigning `nil`, raises a Lua
error. This uses Lua metatable protection; raw operations such as `rawset` bypass
it. Platform constants describe the target for which the client binary was
compiled; they do not query the remote SSH server.

The three functions perform their OS lookup on each call; module registration
and `require("cli")` do not perform those lookups. Results are not cached.
Lookup failure returns `nil`. Use `assert` when the configuration requires a
value, or supply a Lua fallback. The username is the local OS user, and the
hostname identifies the local machine; neither is the destination's SSH
account or network name.

File metadata is available as soon as the module is required. It describes the
actual selected file, including a file selected through `-F`, after resolving
relative components and symbolic links. The metadata and Lua error chunk names
therefore use the same canonical source path. The module follows Lua's normal
`package.loaded` caching behavior within this configuration execution; cached
module access does not cache the functions' return values.

Lua strings preserve native Unix bytes for the file metadata, home directory,
and hostname. Using such a value as a configuration string remains subject to
the normal UTF-8 requirements during deserialization. Diagnostic chunk names
use a display representation for non-UTF-8 paths.

For example, place the known-host database beside the configuration file:

```lua
local cli = require("cli")
return {
    meta = { known_hosts = cli.__dir__ .. "/known_hosts" },
}
```

The module does not implicitly select an authentication agent. Use
`identity_agent = os.getenv("SSH_AUTH_SOCK")` when the configuration should
read that environment variable.

### Examples

TOML:

```toml
[default]
identities-only = true
request-tty = "auto"

[hosts.github]
host-name = "github.com"
user = "git"
port = 22
identity-file = "~/.ssh/id_ed25519"
request-tty = false

[hosts.work]
host-name = "work.example.com"
user = "alice"
identity-file = ["~/.ssh/work_ed25519", "~/.ssh/work_rsa"]
```

Lua:

```lua
local cli = require("cli")
local home = assert(cli.home_dir())
return {
    default = {
        identities_only = true,
        request_tty = "auto",
    },
    hosts = {
        github = {
            host_name = "github.com",
            user = "git",
            port = 22,
            identity_file = home .. "/.ssh/id_ed25519",
            request_tty = false,
        },
        work = {
            host_name = "work.example.com",
            user = "alice",
            identity_file = {
                home .. "/.ssh/work_ed25519",
                home .. "/.ssh/work_rsa",
            },
        },
    },
}
```

## Validation and configuration scope

Loading separates syntax/type validation, source-specific path validation, host resolution, setup, and connection/authentication. A file can be syntactically valid yet fail later because a username cannot be obtained, a path cannot be expanded, a configured algorithm name is unknown, the known-host database cannot be read, a server cannot be reached, or an authentication resource cannot be used. A failure at any stage does not trigger fallback to another default config file.

Errors while opening or loading a selected file include its path in the application's error context. Parser and Lua error details are retained. Native path errors identify the semantic field using `identity_file`, `certificate_file`, `identity_agent`, or `known_hosts`, independently of its spelling in the source.

Native files configure the host fields and application setup metadata listed above. They do not configure the remote command, stdin policy, escape character, local listeners, stdio forwarding, compression preference, cipher list, or MAC list. Those remain application/CLI settings. Native `Include`, subsystem requests, remote forwarding, dynamic forwarding, agent forwarding, and X11 forwarding are not added by the schema.

All three formats preserve the library's `Auth::includes_none = true` default, so user authentication first attempts the SSH `none` method. The configuration formats do not expose a setting for changing that default.

Server host-key verification is a separate concern from user authentication. All three formats use the application's host-key handler. Native `meta.known_hosts` selects its database; otherwise the default path applies. Authentication identity and user-certificate settings must not be used as substitutes for server host-key verification.
