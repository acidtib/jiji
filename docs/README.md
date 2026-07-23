# Jiji Documentation

Documentation for developers and contributors working with Jiji.

## Guides

| Document                                              | Description                             |
| ----------------------------------------------------- | --------------------------------------- |
| [Deployment Guide](deployment-guide.md)               | Complete deployment workflows and CI/CD |
| [Configuration Reference](configuration-reference.md) | All configuration options               |
| [Network Reference](network-reference.md)             | Private networking, WireGuard, and DNS  |
| [Troubleshooting](troubleshooting.md)                 | Common issues and solutions             |

## Reference

| Document                                                                                       | Description                                           |
| ---------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
| [Architecture](architecture.md)                                                                | System design and components                          |
| [Logs Reference](logs-reference.md)                                                            | Viewing and filtering logs                            |
| [Registry Reference](registry-reference.md)                                                    | Container registry setup                              |
| [Rust Deploy Command Follow-Up](rust-deploy-command-follow-up.md)                               | `jiji deploy` implementation plan (implemented)       |
| [Rust Server Teardown Command Follow-Up](rust-server-teardown-command-follow-up.md)             | `jiji server teardown` implementation plan (implemented) |
| [Testing Guide](testing.md)                                                                    | Testing deployments                                   |
| [Version Script](version.md)                                                                   | Version management                                    |

## Configuration Example

See [crates/jiji-config/src/jiji.yml](../crates/jiji-config/src/jiji.yml) for
a complete configuration example with all available options (also the
template `jiji init` writes).
