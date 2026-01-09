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
    port: 9270

ssh:
  user: deploy

servers:
  server1:
    host: server1.example.com

services:
  web:
    image: nginx:latest
    hosts: [server1]
    ports:
      - "80"
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
    port: 9270 # Optional, defaults to 9270
```

**How it works:**

1. Local registry runs on `localhost:9270`
2. Jiji automatically creates SSH reverse tunnels to remote servers
3. Remote servers pull from `localhost:9270` via the tunnel
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
builder:
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
  proxy: bastion.example.com
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
- **Corrosion gossip port**: Always `9280`
- **Corrosion API port**: Always `9220`

### Network Architecture

- **WireGuard**: Encrypted mesh VPN between all servers
- **Corrosion**: Distributed CRDT database for service registry (gossip
  protocol)
- **jiji-dns**: DNS server for service discovery (resolves
  `<project>-<service>.jiji` to container IPs via real-time Corrosion
  subscriptions)
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
servers:
  server1:
    host: server1.example.com
  server2:
    host: server2.example.com
  server3:
    host: server3.example.com

services:
  api:
    hosts: [server1, server2]
  database:
    hosts: [server3]
```

Containers can communicate via DNS (format: `{project}-{service}.jiji`):

```bash
# From api container, connect to database
DATABASE_URL: postgresql://user:pass@myapp-database.jiji:5432/myapp

# From database container, connect to api
API_URL: http://myapp-api.jiji:3000
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
servers:
  server1:
    host: server1.example.com
  server2:
    host: server2.example.com

services:
  web:
    # Use pre built image
    image: nginx:latest

    # Target servers (required)
    hosts: [server1, server2]

    # Port mappings (optional)
    ports:
      - "80"
      - "443"
```

### Build Configuration

Instead of using a pre built image, build from source:

```yaml
servers:
  server1:
    host: server1.example.com

services:
  web:
    build:
      context: . # Build context path
      dockerfile: Dockerfile # Dockerfile path (optional, default: Dockerfile)
      args: # Build arguments (optional)
        - NODE_ENV=production
        - VERSION=1.2.3

    hosts: [server1]
```

**Note:** Use either `image` or `build`, not both.

### Port Mapping Formats

Jiji supports multiple port mapping formats.

**Recommended format for zero-downtime deployments:**

For services using the proxy, specify only the container port. This allows
multiple container instances to run simultaneously during deployment (old and
new containers can coexist without port conflicts):

```yaml
ports:
  - "80"
  - "3000"
```

The proxy handles external traffic routing, so host port binding is not needed.

**Host:Container format (not recommended for proxy services):**

Binding to host ports prevents zero-downtime deployments because only one
container can bind to a host port at a time:

```yaml
ports:
  - "80:80" # Binds host port 80 to container port 80
  - "3000:3000" # Binds host port 3000 to container port 3000
```

Use this format only for services that need direct host port access without a
proxy (e.g., databases, non-HTTP services).

**With protocol:**

```yaml
ports:
  - "53/udp" # Container port only
  - "53:53/udp" # Host:Container
```

**Host IP binding:**

```yaml
ports:
  - "127.0.0.1:8080:80" # Bind to localhost only
```

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
  app_port: 3000
  host: myapp.example.com
  ssl: false
```

**Multiple hosts:**

```yaml
proxy:
  app_port: 3000
  hosts: # Array of hostnames
    - myapp.example.com
    - www.myapp.example.com
  ssl: true
```

**HTTP health checks:**

```yaml
proxy:
  app_port: 3000
  host: myapp.example.com
  healthcheck:
    path: /health # HTTP health check endpoint
    interval: 10s # Check interval
    timeout: 5s # Request timeout
    deploy_timeout: 60s # Timeout during deployment
```

**Command-based health checks:**

```yaml
proxy:
  app_port: 3000
  host: myapp.example.com
  healthcheck:
    cmd: "test -f /app/ready" # Command to execute (exit 0 = healthy)
    cmd_runtime: docker # Optional: Runtime (docker/podman, auto-detects from builder.engine)
    interval: 10s # Check interval
    timeout: 5s # Command timeout
    deploy_timeout: 60s # Timeout during deployment
```

Command health check examples:

- File-based readiness: `cmd: "test -f /app/ready"`
- Process check: `cmd: "pgrep -f myapp"`
- Custom script: `cmd: "/app/healthcheck.sh"`
- Internal HTTP: `cmd: "curl -f http://localhost:3000/health"`
- Complex: `cmd: '/app/healthcheck --config "/etc/app.conf"'`

**Note:**

- HTTP (`path`) and command (`cmd`) health checks are mutually exclusive
- `cmd_runtime` is optional and defaults to the container engine configured in
  `builder.engine` (docker or podman)
- You only need to specify `cmd_runtime` if you want to override the builder
  engine for health checks

**Path prefix routing:**

```yaml
proxy:
  app_port: 3000
  host: myapp.example.com
  path_prefix: /api # Route only /api/* to this service
```

**SSL configuration:**

```yaml
proxy:
  app_port: 3000
  hosts:
    - myapp.example.com
  ssl: true
```

**Multi-target proxy (multiple ports):**

```yaml
proxy:
  targets:
    - app_port: 3900
      host: s3.example.com
      ssl: false
      healthcheck:
        path: /health
        interval: 30s
    - app_port: 3903
      host: admin.example.com
      ssl: true
      healthcheck:
        cmd: "test -f /ready"
        cmd_runtime: docker
        interval: 15s
```

**Note:** SSL requires host configuration (kamal-proxy handles SSL termination).

### Complete Service Example

```yaml
servers:
  server1:
    host: server1.example.com
  server2:
    host: server2.example.com

services:
  api:
    # Build from source
    build:
      context: ./api
      dockerfile: Dockerfile.production
      args:
        - BUILD_ENV=production

    # Target servers
    hosts: [server1, server2]

    # Port mappings
    ports:
      - "3000"

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
        DATABASE_URL: "postgresql://user:pass@myapp-database.jiji:5432/myapp"
      secrets:
        - API_KEY

    # Proxy configuration
    proxy:
      app_port: 3000
      hosts:
        - api.example.com
        - www.api.example.com
      ssl: true
      healthcheck:
        path: /health
        interval: 10s
        timeout: 5s
        deploy_timeout: 60s
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
    port: 9270

ssh:
  user: deploy

servers:
  local:
    host: localhost

services:
  web:
    build:
      context: .
    hosts: [local]
    ports:
      - "3000"
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
  proxy: bastion.example.com

network:
  enabled: true
  cluster_cidr: "10.210.0.0/16"

environment:
  APP_ENV: production
  LOG_LEVEL: warn

servers:
  web1:
    host: web1.example.com
  web2:
    host: web2.example.com
  api1:
    host: api1.example.com
  api2:
    host: api2.example.com
  db1:
    host: db1.example.com

services:
  web:
    build:
      context: ./web
      dockerfile: Dockerfile.production
    hosts: [web1, web2]
    ports:
      - "3000"
    environment:
      clear:
        PORT: 3000
    proxy:
      app_port: 3000
      hosts:
        - app.example.com
        - www.app.example.com
      ssl: true
      healthcheck:
        path: /health
        interval: 10s

  api:
    build:
      context: ./api
    hosts: [api1, api2]
    ports:
      - "4000"
    environment:
      clear:
        PORT: 4000
      secrets:
        - DB_PASSWORD
    proxy:
      app_port: 4000
      host: api.example.com
      ssl: true
      path_prefix: /api
      healthcheck:
        path: /api/health

  database:
    image: postgres:15
    hosts: [db1]
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

servers:
  staging:
    host: staging.example.com

services:
  web:
    build:
      context: .
    hosts: [staging]
    ports:
      - "3000"
    proxy:
      app_port: 3000
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
