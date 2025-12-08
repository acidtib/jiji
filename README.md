# Jiji

Deploy containerized apps across servers, simple, fast, portable. No infra vendor lock in, just run.

## Features

- **Server Bootstrap**: Bootstrap servers with curl and Podman or Docker
- **Configuration Management**: Create and manage infrastructure configurations
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

You can also install a specific version by setting the VERSION environment variable:

```bash
curl -fsSL https://get.jiji.run/install.sh | VERSION=v0.1.5 sh
```


## Usage

### Initialize Configuration

Create a configuration stub in `config/jiji.yml`:

```bash
jiji init
```

### Server Management

Bootstrap servers with container runtime:

```bash
jiji server bootstrap
```

### Help

Get help for any command:

```bash
jiji --help
jiji server --help
jiji server bootstrap --help
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

### Building for npm

```bash
# Build npm package
deno task build:npm

# Or with version
deno task build:npm 1.0.0
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

- [ ] Implement configuration management
- [ ] Add server bootstrap functionality
- [ ] Support for multiple container runtimes
- [ ] Configuration templates
- [ ] Server health checks
- [ ] Logging and monitoring integration

## Support

- 📖 [Documentation](https://github.com/acidtib/jiji)
- 🐛 [Issues](https://github.com/acidtib/jiji/issues)
- 💬 [Discussions](https://github.com/acidtib/jiji/discussions)
