# Jiji

Deploy containerized apps across servers, simple, fast, portable. No infra
vendor lock in, just run.

## Features

- **Server Bootstrap**: Bootstrap servers with curl and Podman or Docker
- **Remote Command Execution**: Execute custom commands across multiple servers
- **Configuration Management**: Create and manage infrastructure configurations
- **Server-Side Audit Trail**: Comprehensive logging of all operations directly
  on target servers
- **CLI Interface**: Easy-to-use command-line interface built with Cliffy

## Installation

### From NPM

```bash
npm install -g jiji
```

### From JSR

```bash
deno install --allow-all --name jiji jsr:@jiji/cli
```

### Linux/MacOS

```bash
curl -fsSL https://get.jiji.run/install.sh | sh
```

You can also install a specific version by setting the VERSION environment
variable:

```bash
curl -fsSL https://get.jiji.run/install.sh | VERSION=v0.1.5 sh
```

## Usage

### Initialize Configuration

Create a configuration stub in `.jiji/development.yml`:

```bash
jiji init
```

### Server Management

Bootstrap servers with container runtime:

```bash
jiji server bootstrap
```

Execute custom commands on remote hosts:

```bash
# Execute a command on all configured hosts
jiji server exec "docker ps"

# Execute on specific hosts only
jiji server exec "systemctl status docker" --hosts "server1.example.com,server2.example.com"

# Execute in parallel across all hosts
jiji server exec "df -h" --parallel

# Set custom timeout and continue on errors
jiji server exec "apt update && apt upgrade -y" --timeout 600 --continue-on-error
```

### Server-Side Audit Trail

View operations history and audit logs from your servers:

```bash
# View recent audit entries from all servers
jiji audit

# View entries from a specific server
jiji audit --host server1.example.com

# Filter by action type across all servers
jiji audit --filter bootstrap

# Filter by status across all servers
jiji audit --status failed

# Aggregate logs chronologically from all servers
jiji audit --aggregate

# View raw log format
jiji audit --raw
```

The audit trail tracks all Jiji operations including:

- Server bootstrapping (start, success, failure)
- Container engine installations on each server
- Service deployments per server
- Configuration changes
- SSH connections and errors

Audit logs are stored in `.jiji/audit.txt` on each target server and include:

- Timestamps (ISO 8601 format)
- Action types and status
- Server-specific operation context
- Detailed error messages and troubleshooting information
- Host identification for multi-server deployments

### Help

Get help for any command:

```bash
jiji --help
jiji server --help
jiji server bootstrap --help
jiji server exec --help
```

## Development

This project is built with Deno.

### Prerequisites

- [Deno](https://deno.land/) 2.5+

### Running Locally

```bash
# Clone the repository
git clone https://github.com/acidtib/jiji.git
cd jiji

# Run the CLI
deno task run

# Or run directly
deno run src/main.ts
```

## Contributing

1. Fork the repository
2. Create your feature branch (`git checkout -b feature/amazing-feature`)
3. Commit your changes (`git commit -m 'Add some amazing feature'`)
4. Push to the branch (`git push origin feature/amazing-feature`)
5. Open a Pull Request

## License

MIT License - see the [LICENSE](LICENSE) file for details.

## Roadmap

- [x] Server bootstrap functionality
- [x] Remote command execution across multiple servers
- [x] Support for multiple container runtimes (Podman/Docker)
- [x] Server-side audit trail and operation logging
- [ ] Service deployment and orchestration
- [ ] Configuration templates
- [ ] Server health checks
- [ ] Advanced monitoring integration
- [ ] Multi-environment support

## Documentation

For developers and contributors, additional documentation can be found in the
[`docs/`](docs/) directory, which contains detailed information that may be
useful for development and contribution workflows.

## Support

- 📖 [Documentation](https://github.com/acidtib/jiji)
- 🐛 [Issues](https://github.com/acidtib/jiji/issues)
- 💬 [Discussions](https://github.com/acidtib/jiji/discussions)
