# Developer reference

This reference describes `openrussh-cli`, whose executable is named `ssh`. It
covers the application's configuration, runtime behavior, implementation
boundaries, and development workflow. The locked `openrussh-client` 0.4.6
dependency is the authority for the SSH operations delegated to it.

| Document | Contents |
| --- | --- |
| [Configuration](configuration.md) | File discovery, OpenSSH/TOML/Lua syntax, host matching, value types, paths, defaults, overrides, and jump chains. |
| [Configuration architecture](architecture.md) | The common configuration type, format-specific parse results, resolution stages, ownership, and dependency adapters. |
| [Runtime](runtime.md) | CLI arguments, remote commands, standard streams, PTYs, signals, forwarding, exit status, and server host-key verification. |
| [Development](development.md) | Module responsibilities, coding conventions, dependencies, tests, platform support, and release automation. |

For a user-facing installation guide and configuration examples, see the
[README](../README.md).

## Scope and terminology

The CLI resembles OpenSSH's interface but implements the subset exposed by this
application and its SSH dependency. OpenSSH-syntax configuration is handled by
the dependency; TOML and Lua have the native semantics specified here. A parsed
directive does not imply support for the corresponding OpenSSH feature.

An **identity** is used for SSH user authentication. A **server host key**
authenticates an SSH server. An **SSH user certificate** associates a user public
key with certificate principals and other authorization data; it is distinct
from a **host certificate** used to authenticate a server. Neither is an X.509
certificate. A **PTY** is a terminal allocated for a remote session, separate
from the local terminal whose modes the client manages.

These documents specify current behavior, including dependency behavior that
differs from OpenSSH. Implementation limitations are identified where they
affect configuration or operation; the reference does not promise unsupported
options or future features.
