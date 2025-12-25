# Jiji Configuration Reference

Complete reference for Jiji configuration files (`.jiji/deploy.yml` or
`jiji.<environment>.yml`).

## Table of Contents

- [Configuration File Structure](#configuration-file-structure)
- [Project Configuration](#project-configuration)
- [Builder Configuration](#builder-configuration)
- [Registry Configuration](#registry-configuration)
- [SSH Configuration](#ssh-configuration)
- [Network Configuration](#network-configuration)
- [Service Configuration](#service-configuration)
- [Environment Variables](#environment-variables)
- [Complete Examples](#complete-examples)

## Configuration File Structure

Jiji uses YAML configuration files to define your infrastructure. The default
file is `.jiji/deploy.yml`, but you can create environment specific configs like
`jiji.staging.yml` or `jiji.production.yml`.

### Selecting Configuration

```bash
# Use default .jiji/deploy.yml
jiji deploy

# Use environment specific config
jiji --environment staging deploy  # Uses jiji.staging.yml
jiji --environment production deploy  # Uses jiji.production.yml

# Use custom config file
jiji --config-file /path/to/custom.yml deploy
```

### Minimal Configuration

```yaml
project: myapp

builder:
  engine: docker
  local: true
  registry:
    type: local
    port: 6767

ssh:
  user: deploy

services:
  web:
    image: nginx:latest
    servers:
      - host: server1.example.com
        arch: amd64
    ports:
      - "80:80"
```

## Project Configuration

### `project` (required)

Unique identifier for your application. Used for organizing services, audit
logs, and deployment locks.

```yaml
project: myapp
```

**Notes:**

- All audit logs stored in `.jiji/audit.txt` on remote hosts
- Deployment locks prevent concurrent deployments
- Project name used as namespace in container names

## Builder Configuration

Controls how container images are built and where they're stored.

### Basic Options

```yaml
builder:
  # Container engine (required)
  # Options: docker, podman
  engine: docker

  # Build location (required)
  # true = build on local machine
  # false = build on remote server
  local: true

  # Enable build cache (optional, default: true)
  # Set to false for clean builds (useful in CI/CD)
  cache: true

  # Remote builder SSH connection (required when local: false)
  # Format: ssh://[user@]hostname[:port]
  remote: ssh://builder@192.168.1.50:22
```

### Use Cases

**Local Development:**

```yaml
builder:
  engine: docker
  local: true
  cache: true
```

**Remote Building (offload to powerful build server):**

```yaml
builder:
  engine: docker
  local: false
  remote: ssh://builder@build-server.example.com
  cache: true
```

**CI/CD (always fresh builds):**

```yaml
builder:
  engine: docker
  local: true
  cache: false # Force clean builds
```

## Registry Configuration

Determines where built images are stored and retrieved from. Configure under
`builder.registry`.

### Local Registry

Creates a local registry with SSH port forwarding to deployment hosts. Perfect
for development.

```yaml
builder:
  registry:
    type: local
    port: 6767 # Optional, defaults to 6767
```

**How it works:**

1. Local registry runs on `localhost:6767`
2. Jiji automatically creates SSH reverse tunnels to remote servers
3. Remote servers pull from `localhost:6767` via the tunnel
4. Tunnel torn down after deployment

### Remote Registry

Use when deploying to production or sharing images across teams.

```yaml
builder:
  registry:
    type: remote
    server: ghcr.io # Registry server URL
    username: myuser
    password: "${GITHUB_TOKEN}" # Environment variable substitution
```

### Supported Registries

**GitHub Container Registry (GHCR):**

```yaml
builder:
  registry:
    type: remote
    server: ghcr.io
    username: your-github-username
    password: "${GITHUB_TOKEN}"
```

- Auto namespace: `username/project-name`
- Result: `ghcr.io/username/project-name/service:version`

**Docker Hub:**

```yaml
builder:
  registry:
    type: remote
    server: docker.io
    username: your-dockerhub-username
    password: "${DOCKER_PASSWORD}"
```

- Auto namespace: `username`
- Result: `docker.io/username/project-service:version`

**Custom Registry:**

```yaml
builder:
  registry:
    type: remote
    server: registry.example.com:5000
    username: myuser
    password: "${REGISTRY_PASSWORD}"
```

- No auto namespace
- Result: `registry.example.com:5000/project-service:version`

### Environment Variable Substitution

Registry passwords support environment variable substitution for security:

```yaml
registry:
  password: "${GITHUB_TOKEN}" # Substituted at runtime
```

Common environment variables:

- `${GITHUB_TOKEN}` - GitHub Personal Access Token
- `${DOCKER_PASSWORD}` - Docker Hub password or token
- `${REGISTRY_PASSWORD}` - Generic registry password

**Best practices:**

- Never commit passwords to configuration files
- Set environment variables before deployment
- Use secrets management in CI/CD (e.g., GitHub Actions secrets)

## SSH Configuration

Configure SSH connections to remote servers.

### Basic Options

```yaml
ssh:
  # User for SSH connections (required)
  user: deploy

  # SSH port (optional, default: 22)
  port: 22

  # Connection timeout in milliseconds (optional, default: 30000)
  timeout: 30000
```

### Authentication Methods

**SSH Agent (recommended):**

```yaml
ssh:
  user: deploy
  # Uses ssh-agent by default if no keys specified
```

**Private Keys:**

```yaml
ssh:
  user: deploy
  private_keys:
    - ~/.ssh/id_rsa
    - ~/.ssh/id_ed25519
    - /path/to/deploy_key
```

**Inline Key Data (for CI/CD):**

```yaml
ssh:
  user: deploy
  key_data:
    - SSH_PRIVATE_KEY_1 # Environment variable name
    - SSH_PRIVATE_KEY_2
```

**Keys Only Mode (disable ssh-agent):**

```yaml
ssh:
  user: deploy
  keys_only: true # Use only specified keys, ignore ssh-agent
  private_keys:
    - ~/.ssh/deploy_key
```

### Proxy/Jump Hosts

**ProxyJump (simple bastion host):**

```yaml
ssh:
  user: deploy
  proxy_jump: bastion.example.com
  # Or with user: deploy@bastion.example.com
  # Or with port: bastion.example.com:2222
```

**ProxyCommand (advanced):**

```yaml
ssh:
  user: deploy
  proxy_command: "ssh -W %h:%p user@proxy.example.com"
  # %h = target hostname
  # %p = target port
```

### SSH Config File Support

Load SSH configuration from `~/.ssh/config` to leverage existing setups:

```yaml
ssh:
  user: deploy
  config: true # Load default ~/.ssh/config
```

Or load specific config file:

```yaml
ssh:
  user: deploy
  config: ~/.ssh/custom_config
```

Or load multiple files:

```yaml
ssh:
  user: deploy
  config:
    - ~/.ssh/config
    - ~/.ssh/work_config
```

**What gets inherited:**

- Host specific settings (wildcards supported)
- ProxyJump/ProxyCommand
- IdentityFile (private keys)
- Connection timeouts
- SSH options

**Note:** Jiji configuration takes precedence over SSH config file settings.

### Advanced Options

```yaml
ssh:
  user: deploy

  # Connection pool settings
  max_concurrent_connections: 30 # Limit concurrent SSH connections
  pool_idle_timeout: 900 # Seconds before idle connections close

  # DNS retry attempts
  dns_retries: 3 # Retry DNS lookups with exponential backoff

  # Log level for SSH operations
  # Options: debug, info, warn, error, fatal
  log_level: error # Default: error
```

## Network Configuration

Enables secure, encrypted container to container communication across servers
using WireGuard VPN mesh network with automatic service discovery.

### Basic Configuration

```yaml
network:
  # Enable private networking (default: true)
  enabled: true

  # Cluster CIDR for container networking (default: 10.210.0.0/16)
  # Each server gets a /24 subnet (254 usable IPs per server)
  cluster_cidr: "10.210.0.0/16"
```

**Note:** The following are not configurable (hardcoded):

- **Service domain**: Always `jiji` (containers reach each other via
  `<service>.jiji`)
- **Service discovery method**: Always `corrosion` (distributed CRDT based)
- **WireGuard port**: Always `51820`
- **Corrosion gossip port**: Always `8787`
- **Corrosion API port**: Always `8080`

### Network Architecture

- **WireGuard**: Encrypted mesh VPN between all servers
- **Corrosion**: Distributed CRDT database for service registry (gossip
  protocol)
- **CoreDNS**: DNS server for service discovery (resolves `<service>.jiji` to
  container IPs)
- **Dual stack**: IPv4 (10.210.0.0/16) for containers, IPv6 (fdcc::/16) for
  management
- **Automatic**: Containers auto registered on deploy, auto unregistered on
  remove

### IP Allocation

```
Cluster CIDR: 10.210.0.0/16 (configurable)
├── Server 0: 10.210.0.0/24
│   ├── WireGuard IP: 10.210.0.1
│   ├── Management IP: fdcc:xxxx:... (IPv6, derived from pubkey)
│   └── Containers: 10.210.0.2 - 10.210.0.254
├── Server 1: 10.210.1.0/24
│   ├── WireGuard IP: 10.210.1.1
│   ├── Management IP: fdcc:xxxx:... (IPv6, derived from pubkey)
│   └── Containers: 10.210.1.2 - 10.210.1.254
└── Server N: 10.210.N.0/24 (up to 256 servers with /16)
```

### Service Discovery Example

Given these services:

```yaml
services:
  api:
    servers:
      - host: server1.example.com
        arch: amd64
      - host: server2.example.com
        arch: amd64
  database:
    servers:
      - host: server3.example.com
        arch: amd64
```

Containers can communicate via DNS:

```bash
# From api container, connect to database
DATABASE_URL: postgresql://user:pass@database.jiji:5432/myapp

# From database container, connect to api
API_URL: http://api.jiji:3000
```

DNS automatically resolves to all healthy container IPs for that service,
providing client side load balancing.

### Custom Network Configuration

**Avoid CIDR conflicts with existing networks:**

```yaml
network:
  enabled: true
  cluster_cidr: "172.20.0.0/16" # Use different CIDR
```

**Disable networking:**

```yaml
network:
  enabled: false
```

## Service Configuration

Services are the deployable units in Jiji - each service represents a
containerized application.

### Basic Service

```yaml
services:
  web:
    # Use pre built image
    image: nginx:latest

    # Target servers (required)
    servers:
      - host: server1.example.com
        arch: amd64
      - host: server2.example.com
        arch: amd64

    # Port mappings (optional)
    ports:
      - "80:80"
      - "443:443"
```

### Build Configuration

Instead of using a pre built image, build from source:

```yaml
services:
  web:
    build:
      context: . # Build context path
      dockerfile: Dockerfile # Dockerfile path (optional, default: Dockerfile)
      args: # Build arguments (optional)
        - NODE_ENV=production
        - VERSION=1.2.3

    servers:
      - host: server1.example.com
        arch: amd64
```

**Note:** Use either `image` or `build`, not both.

### Port Mapping Formats

Jiji supports multiple port mapping formats:

**Simple format:**

```yaml
ports:
  - "80:80"
  - "443:443"
```

**With protocol:**

```yaml
ports:
  - "80:80/tcp"
  - "53:53/udp"
```

**Host IP binding:**

```yaml
ports:
  - "127.0.0.1:8080:80" # Bind to localhost only
```

**Multi line array format:**

```yaml
ports:
  - "3000:3000"
  - "8080:80"
```

See [docs/port-mapping-examples.yaml](port-mapping-examples.yaml) for more
examples.

### Volume Mounts

**Bind mounts and Named volumes:**

```yaml
volumes:
  - "/data/web/logs:/var/log/nginx"
  - "web_storage:/opt/uploads"
  - "./data:/opt/extra_data:ro" # Read only
```

**File mounts:**

```yaml
# String format: local:remote[:options] where options can be ro, z, or Z
files:
  - "nginx.conf:/etc/nginx/nginx.conf:ro"
# Or use hash format for custom permissions and ownership:
# files:
#   - local: config/secret.key
#     remote: /etc/app/secret.key
#     mode: "0600"
#     owner: "nginx:nginx"
#     options: "ro"
```

**Directory mounts:**

```yaml
# String format: local:remote[:options] where options can be ro, z, or Z
directories:
  - "html:/usr/share/nginx/html:ro"
# Or use hash format for custom permissions and ownership:
# directories:
#   - local: logs
#     remote: /var/log/nginx
#     mode: "0755"
#     owner: "nginx:nginx"
#     options: "z"
```

### Environment Variables

**Shared environment variables:** Applied to all services at the project level

```yaml
environment:
  # Clear text environment variables
  clear:
    APP_ENV: production
    LOG_LEVEL: info
    DEBUG: false
  # Secrets loaded from host environment variables
  secrets:
    - API_KEY
    - DATABASE_PASSWORD
```

**Service specific environment variables:** Merged with shared environment

```yaml
services:
  web:
    # ... other config ...
    environment:
      # Clear text environment variables
      clear:
        NODE_ENV: production
        PORT: 3000
      # Secrets loaded from host environment variables
      secrets:
        - API_KEY
```

**Type conversion:** Numbers and booleans are automatically converted to strings
for container compatibility:

```yaml
environment:
  clear:
    DEBUG: true # Becomes "true"
    PORT: 3000 # Becomes "3000"
```

### Proxy Configuration

Enable kamal-proxy for HTTP/HTTPS routing:

**Basic proxy:**

```yaml
proxy:
  enabled: true
  host: myapp.example.com
  ssl: false
```

**Multiple hosts:**

```yaml
proxy:
  enabled: true
  hosts: # Array of hostnames
    - myapp.example.com
    - www.myapp.example.com
  ssl: true
```

**Health checks:**

```yaml
proxy:
  enabled: true
  host: myapp.example.com
  health_check:
    path: /health # Health check endpoint
    interval: "10s" # Check interval
    timeout: "5s" # Request timeout
    deploy_timeout: "60s" # Timeout during deployment
```

**Path prefix routing:**

```yaml
proxy:
  enabled: true
  host: myapp.example.com
  path: /api # Route only /api/* to this service
```

**SSL configuration:**

```yaml
proxy:
  enabled: true
  hosts:
    - myapp.example.com
  ssl: true
```

**Note:** SSL requires host configuration (kamal-proxy handles SSL termination).

### Complete Service Example

```yaml
services:
  api:
    # Build from source
    build:
      context: ./api
      dockerfile: Dockerfile.production
      args:
        - BUILD_ENV=production

    # Target servers
    servers:
      - host: server1.example.com
        arch: amd64
      - host: server2.example.com
        arch: amd64

    # Port mappings
    ports:
      - "3000:3000"

    # Volume mounts
    volumes:
      - "/data/api/logs:/app/logs"
      - "api-cache:/tmp/cache"

    # File mounts: String format: local:remote[:options] where options can be ro, z, or Z
    files:
      - "api-config.json:/app/config/config.json"

    # Directory mounts: String format: local:remote[:options] where options can be ro, z, or Z
    directories:
      - "uploads:/app/uploads"

    # Environment variables
    environment:
      clear:
        NODE_ENV: production
        PORT: 3000
        DATABASE_URL: "postgresql://user:pass@database.jiji:5432/myapp"
      secrets:
        - API_KEY

    # Proxy configuration
    proxy:
      enabled: true
      hosts:
        - api.example.com
        - www.api.example.com
      ssl: true
      health_check:
        path: /health
        interval: "10s"
        timeout: "5s"
        deploy_timeout: "60s"
```

## Environment Variables

Shared environment variables applied to all services. Can be overridden
per-service.

```yaml
environment:
  # Clear text environment variables
  clear:
    APP_ENV: production
    LOG_LEVEL: info
    DEBUG: false
  # Secrets loaded from host environment variables
  secrets:
    - DATABASE_PASSWORD
    - API_SECRET_KEY

  # All services inherit these variables
  # Services can override by defining their own environment section
```

## Complete Examples

### Minimal Development Setup

```yaml
project: myapp

builder:
  engine: docker
  local: true
  registry:
    type: local
    port: 6767

ssh:
  user: deploy

services:
  web:
    build:
      context: .
    servers:
      - host: localhost
        arch: amd64
    ports:
      - "3000:3000"
```

### Production Multi Service Deployment

```yaml
project: myapp-production

builder:
  engine: docker
  local: true
  cache: false
  registry:
    type: remote
    server: ghcr.io
    username: myorg
    password: "${GITHUB_TOKEN}"

ssh:
  user: deploy
  private_keys:
    - ~/.ssh/production_key
  proxy_jump: bastion.example.com

network:
  enabled: true
  cluster_cidr: "10.210.0.0/16"

environment:
  APP_ENV: production
  LOG_LEVEL: warn

services:
  web:
    build:
      context: ./web
      dockerfile: Dockerfile.production
    servers:
      - host: web1.example.com
        arch: amd64
      - host: web2.example.com
        arch: amd64
    ports:
      - "3000:3000"
    environment:
      clear:
        PORT: 3000
    proxy:
      enabled: true
      hosts:
        - app.example.com
        - www.app.example.com
      ssl: true
      health_check:
        path: /health
        interval: "10s"

  api:
    build:
      context: ./api
    servers:
      - host: api1.example.com
        arch: amd64
      - host: api2.example.com
        arch: amd64
    environment:
      clear:
        PORT: 4000
      secrets:
        - DB_PASSWORD
    proxy:
      enabled: true
      host: api.example.com
      ssl: true
      path: /api
      health_check:
        path: /api/health

  database:
    image: postgres:15
    servers:
      - host: db1.example.com
        arch: amd64
    volumes:
      - "/data/postgres:/var/lib/postgresql/data"
    environment:
      clear:
        POSTGRES_PASSWORD: "${DB_PASSWORD}"
```

### Staging with Remote Builder

```yaml
project: myapp-staging

builder:
  engine: docker
  local: false
  remote: ssh://builder@build-server.example.com
  registry:
    type: remote
    server: docker.io
    username: myuser
    password: "${DOCKER_PASSWORD}"

ssh:
  user: deploy
  config: true # Use ~/.ssh/config

network:
  enabled: true

services:
  web:
    build:
      context: .
    servers:
      - host: staging.example.com
        arch: amd64
    proxy:
      enabled: true
      host: staging.myapp.example.com
```

## Validation

Jiji validates configuration on load. Common validation errors:

**Missing required fields:**

```
Error: Missing required field 'project'
```

**Invalid values:**

```
Error: builder.engine must be 'docker' or 'podman'
```

**Conflicting options:**

```
Error: Cannot specify both 'image' and 'build' for service 'web'
```

Use `--verbose` flag for detailed validation messages:

```bash
jiji --verbose deploy
```
