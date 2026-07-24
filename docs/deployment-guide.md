# Jiji Deployment Guide

Complete guide to deploying applications with Jiji, from initial setup to
advanced deployment patterns.

## Table of Contents

- [Initial Setup](#initial-setup)
- [First Deployment](#first-deployment)
- [Zero-Downtime Deployments](#zero-downtime-deployments)
- [Multi-Environment Deployments](#multi-environment-deployments)
- [CI/CD Integration](#cicd-integration)
- [Common Workflows](#common-workflows)
- [Best Practices](#best-practices)
- [Troubleshooting Deployments](#troubleshooting-deployments)
- [Audit Trail](#audit-trail)
- [Deployment Locks](#deployment-locks)

## Initial Setup

### Prerequisites

- jiji installed
- SSH access to target servers
- Servers running Ubuntu 24.04+
- Docker or Podman installed on servers (or will be installed by Jiji)

### 1. Install Jiji

Prebuilt binaries and an install script are not published yet for the Rust
rewrite; build from source:

```bash
git clone https://github.com/acidtib/jiji.git
cd jiji
mise install         # cargo build --release --bin jiji -> ~/.local/bin/jiji
```

(or `cargo build --release --bin jiji` directly, then put
`target/release/jiji` on `PATH`, if you don't use `mise`.)

### 2. Initialize Configuration

```bash
# Create .jiji/deploy.yml stub
jiji init

# Edit configuration
vim .jiji/deploy.yml
```

### 3. Configure Your Application

**Minimal configuration:**

```yaml
project: myapp

builder:
  engine: podman
  local: true
  registry:
    type: local
    port: 31270

ssh:
  user: root

servers:
  server1:
    host: server1.example.com

services:
  web:
    build:
      context: .
      dockerfile: Dockerfile
    hosts:
      - server1
    ports:
      - "3000"
    proxy:
      app_port: 3000
      host: myapp.example.com
      healthcheck:
        path: /health
```

### 4. Setup SSH Access

Ensure you have SSH access to your servers:

```bash
# Test SSH connection
ssh deploy@server1.example.com

# If using SSH keys, add to agent
ssh-add ~/.ssh/id_rsa
```

**Bastion/jump host setup:**

```yaml
ssh:
  user: deploy
  proxy: bastion.example.com
```

### 5. Setup Registry

**For GitHub Container Registry:**

```bash
# Create GitHub Personal Access Token with write:packages permission
# https://github.com/settings/tokens

# Set environment variable
export GITHUB_TOKEN=ghp_your_token_here

# Update config
```

```yaml
builder:
  registry:
    type: remote
    server: ghcr.io
    username: your-github-username
    password: GITHUB_TOKEN
```

```bash
# Login to registry
jiji registry login
```

**For local registry (development):**

```yaml
builder:
  registry:
    type: local
    port: 31270 # Jiji handles port forwarding automatically
```

### 6. Initialize Servers

```bash
# Install container runtime and setup infrastructure
jiji server init

# This will:
# - Install Docker/Podman
# - Setup private networking (WireGuard)
# - Install WireGuard, routed container networking, compiled DNS, and service VIPs
# - Configure firewall rules
```

## First Deployment

### 1. Build Images

```bash
# Build all services
jiji build

# This will:
# - Build container images from your Dockerfiles
# - Tag with git SHA (or --version if specified)
# - Push to configured registry
```

### 2. Deploy Services

```bash
# Deploy with confirmation prompt
jiji deploy

# Review deployment plan showing:
# - Services to be deployed
# - Target hosts
# - Image versions
# - Build configurations

# Confirm to proceed
```

**Skip confirmation (for CI/CD):**

```bash
jiji deploy --yes
```

**Build and deploy in one command:**

```bash
jiji deploy --build
```

### 3. Verify Deployment

```bash
# Check container status
jiji server exec "podman ps"

# Follow logs
jiji service logs --services web --follow

# Check health endpoint
curl https://myapp.example.com/health

# View deployment audit trail
jiji audit
```

## Zero-Downtime Deployments

### How It Works

1. **New Container Deployment**
   - Deploy new container alongside existing one
   - Container gets unique name with version tag
   - Connected to private network

2. **Health Check Verification**
   - Wait for health checks to pass
   - Verify via proxy health endpoint (if using proxy)
   - Or verify container readiness

3. **Traffic Routing**
   - Configure proxy to route traffic to new container
   - Old container continues handling in flight requests

4. **Graceful Shutdown**
   - Stop routing new traffic to old container
   - Wait for in flight requests to complete
   - Stop and remove old container

5. **Cleanup**
   - Remove old images (keeping configured number of versions)
   - Update service registry

### Configuration

**Health check configuration:**

```yaml
services:
  web:
    proxy:
      app_port: 3000
      host: myapp.example.com
      healthcheck:
        path: /health # Health endpoint
        interval: "10s" # Check every 10 seconds
        timeout: "5s" # Timeout after 5 seconds
        deploy_timeout: "60s" # Total deployment timeout
```

**Your application must:**

- Respond to health check endpoint with 200 OK when ready
- Handle graceful shutdown (respond to SIGTERM)
- Complete in flight requests before exiting

**Example health endpoint (Node.js/Express):**

```javascript
app.get("/health", (req, res) => {
  // Check database connection, dependencies, etc.
  if (database.isConnected()) {
    res.status(200).json({ status: "healthy" });
  } else {
    res.status(503).json({ status: "unhealthy" });
  }
});
```

### Monitoring During Deployment

```bash
# Follow logs during deployment
jiji service logs --services web --follow

# In another terminal, watch containers
watch -n 1 'jiji server exec "podman ps | grep web"'

# Monitor proxy status
jiji proxy logs --follow
```

## Multi-Environment Deployments

Manage multiple environments (staging, production) with environment-specific
configurations.

### Setup

Create environment-specific config files:

```bash
# Development
.jiji/deploy.yml

# Staging
jiji.staging.yml

# Production
jiji.production.yml
```

### Example Configurations

**Staging (jiji.staging.yml):**

```yaml
project: myapp-staging

builder:
  engine: docker
  local: true
  registry:
    type: remote
    server: ghcr.io
    username: myorg
    password: GITHUB_TOKEN
ssh:
  user: deploy
  config: true

servers:
  staging:
    host: staging.example.com

services:
  web:
    build:
      context: .
      args:
        - BUILD_ENV=staging
    hosts:
      - staging
    ports:
      - "3000"
    environment:
      clear:
        APP_ENV: staging
        LOG_LEVEL: debug
    proxy:
      app_port: 3000
      host: staging.myapp.example.com
```

**Production (jiji.production.yml):**

```yaml
project: myapp-production

builder:
  engine: docker
  local: true
  cache: false # Always fresh builds
  registry:
    type: remote
    server: ghcr.io
    username: myorg
    password: GITHUB_TOKEN
ssh:
  user: deploy
  private_keys:
    - ~/.ssh/production_key
  proxy: bastion.example.com

servers:
  web1:
    host: web1.example.com
  web2:
    host: web2.example.com

services:
  web:
    build:
      context: .
      args:
        - BUILD_ENV=production
    hosts:
      - web1
      - web2
    ports:
      - "3000"
    environment:
      clear:
        APP_ENV: production
        LOG_LEVEL: warn
    proxy:
      app_port: 3000
      hosts:
        - myapp.example.com
        - www.myapp.example.com
      ssl: true
      healthcheck:
        path: /health
        interval: "10s"
```

### Deploy to Specific Environment

```bash
# Deploy to staging
jiji --environment staging deploy

# Deploy to production
jiji --environment production deploy --yes

# Build specific version for production
jiji --environment production deploy --build --version v1.2.3
```

### Environment Variables

Use environment variables for secrets:

```bash
# Staging
export GITHUB_TOKEN=ghp_staging_token
export DATABASE_PASSWORD=staging_db_pass

# Production
export GITHUB_TOKEN=ghp_production_token
export DATABASE_PASSWORD=production_db_pass
```

### Debugging Secrets

Use `jiji secrets print` to verify secrets are correctly configured before
deployment:

```bash
# Show which secrets are configured (values hidden)
jiji secrets print

# Show actual secret values (use with caution)
jiji secrets print --show-values

# Check secrets for specific services
jiji secrets print --services api,worker

# With environment flag
jiji --environment production secrets print
```

The command shows:

- Which secrets are `[SET]` vs `[MISSING]`
- Source of secrets (`.env` file location)
- Registry password status
- Warnings about missing values

### Restarting kamal-proxy

Pull the current kamal-proxy image and recreate the shared proxy container on
every configured server:

```bash
jiji proxy restart

# Restart only matching hosts
jiji -H 'web-*' proxy restart
```

The named proxy configuration volume is preserved, so active routes remain
configured. Each selected host has a brief interruption while its proxy
container is recreated.

### Proxy Logs

Show the latest 100 kamal-proxy log lines from every configured server:

```bash
jiji proxy logs
jiji -H web-1 proxy logs --lines 200
jiji proxy logs --since 1h --grep ERROR
```

Following logs requires exactly one selected host:

```bash
jiji -H web-1 proxy logs --follow
```

Both commands operate on host-level proxy infrastructure and reject
`-S`/`--services`.

**Using host environment fallback:**

If secrets are set as environment variables on your machine (common in CI/CD),
use the `--host-env` flag:

```bash
# Check secrets using host environment variables as fallback
jiji --host-env secrets print

# Deploy with host env fallback
jiji --host-env deploy
```

This is useful when:

- Running in CI/CD where secrets are injected as environment variables
- Testing locally without a `.env` file
- Debugging secret resolution issues

## CI/CD Integration

Integrate Jiji with your CI/CD pipeline for automated deployments.

### GitHub Actions

**.github/workflows/deploy.yml:**

```yaml
name: Deploy

on:
  push:
    branches: [main]

jobs:
  deploy:
    runs-on: ubuntu-latest

    steps:
      - uses: actions/checkout@v4

      - name: Install Jiji
        run: |
          curl -fsSL https://get.jiji.run/install.sh | sh
          echo "$HOME/.jiji/bin" >> $GITHUB_PATH

      - name: Setup SSH Key
        run: |
          mkdir -p ~/.ssh
          echo "${{ secrets.SSH_PRIVATE_KEY }}" > ~/.ssh/deploy_key
          chmod 600 ~/.ssh/deploy_key
          ssh-keyscan ${{ secrets.DEPLOY_HOST }} >> ~/.ssh/known_hosts

      - name: Deploy to Production
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          DATABASE_PASSWORD: ${{ secrets.DATABASE_PASSWORD }}
        run: |
          jiji --environment production deploy --build --yes

      - name: Verify Deployment
        run: |
          sleep 10
          curl -f https://myapp.example.com/health || exit 1
```

**Required Secrets:**

- `SSH_PRIVATE_KEY` - SSH private key for server access
- `DEPLOY_HOST` - Deployment server hostname
- `DATABASE_PASSWORD` - Database password
- `GITHUB_TOKEN` - Automatically provided by GitHub Actions

### GitLab CI

**.gitlab-ci.yml:**

```yaml
stages:
  - build
  - deploy

variables:
  JIJI_VERSION: "0.1.13"

before_script:
  - apt-get update && apt-get install -y curl
  - curl -fsSL https://get.jiji.run/install.sh | sh
  - export PATH="$HOME/.jiji/bin:$PATH"
  - mkdir -p ~/.ssh
  - echo "$SSH_PRIVATE_KEY" > ~/.ssh/deploy_key
  - chmod 600 ~/.ssh/deploy_key

deploy_staging:
  stage: deploy
  script:
    - jiji --environment staging deploy --build --yes
  only:
    - develop
  environment:
    name: staging
    url: https://staging.myapp.example.com

deploy_production:
  stage: deploy
  script:
    - jiji --environment production deploy --build --yes
    - curl -f https://myapp.example.com/health
  only:
    - main
  environment:
    name: production
    url: https://myapp.example.com
  when: manual # Require manual approval
```

**Required Variables (GitLab CI/CD Settings):**

- `SSH_PRIVATE_KEY` - SSH private key
- `GITHUB_TOKEN` - Registry access token
- `DATABASE_PASSWORD` - Database password

## Common Workflows

### Update Single Service

```bash
# Deploy only the API service
jiji deploy --services api

# Build and deploy specific service
jiji deploy --build --services api
```

### Deploy to Specific Hosts

```bash
# Deploy to specific server
jiji deploy --hosts server1.example.com

# Deploy specific service to specific host
jiji deploy --services web --hosts server1.example.com
```

### Version-Tagged Deployments

```bash
# Deploy with custom version tag
jiji deploy --build --version v1.2.3

# Images will be tagged as:
# registry/project/service:v1.2.3
```

### Rollback Deployment

Jiji keeps previous container versions running until new ones are healthy. If
deployment fails, the old container continues serving traffic.

**Manual rollback:**

```bash
# Roll back to a previously built and pushed version (does not rebuild)
jiji service rollback --version v1.2.2

# Roll back a specific service, or only on specific hosts
jiji service rollback --services web --version v1.2.2
jiji service rollback --services web --hosts server1.example.com --version v1.2.2
```

`jiji service rollback` runs the same zero-downtime slot cycle as `jiji
deploy`/`jiji service restart` (health check, VIP cutover, old-slot cleanup),
but targets the exact image tag you pass with `--version` instead of building
a new one. For a service with `build:` configured, it resolves that tag
straight from `builder.registry` (no rebuild, trusting the tag was already
pushed by an earlier `jiji build`/`jiji deploy --build`); for a service with a
static, untagged `image:`, `--version` is appended the same way `jiji deploy
--version` applies it.

You can also redeploy an older version through `jiji deploy --version
<tag>`, or rebuild from an older commit entirely:

```bash
# Redeploy previous version through the normal deploy path
jiji deploy --version v1.2.2

# Or rebuild from previous git commit
git checkout v1.2.2
jiji deploy --build
```

### Restart Services

```bash
# Restart all instances of a service
jiji service restart --services web

# Restart on specific host
jiji service restart --services web --hosts server1.example.com
```

### View Logs

```bash
# View recent logs
jiji service logs --services web --lines 100

# Follow logs in real-time
jiji service logs --services web --follow

# Filter for errors
jiji service logs --services web --grep "ERROR" --since "1h"
```

### Clean Up Old Images

```bash
# Remove old image versions (keeps last 5)
jiji service prune

# Keep more versions
jiji service prune --retain 10

# Auto pruning runs after successful deployments
```

## Best Practices

### 1. Use Version Tags

Always tag releases with semantic versioning:

```bash
# Tag in git
git tag v1.2.3
git push --tags

# Deploy with version
jiji deploy --build --version v1.2.3
```

### 2. Implement Health Checks

Always implement health check endpoints:

```yaml
proxy:
  app_port: 3000
  host: myapp.example.com
  healthcheck:
    path: /health
    interval: "10s"
    timeout: "5s"
    deploy_timeout: "60s"
```

### 3. Use Environment Variables for Secrets

Never commit secrets to configuration files:

```yaml
environment:
  secrets:
    - DATABASE_PASSWORD
    - API_KEY
```

### 4. Test in Staging First

Always deploy to staging before production:

```bash
# Deploy to staging
jiji --environment staging deploy

# Test thoroughly
curl https://staging.myapp.example.com/health

# Deploy to production
jiji --environment production deploy
```

### 5. Monitor Deployments

Watch logs during deployment:

```bash
# Terminal 1: Deploy
jiji deploy --build

# Terminal 2: Follow logs
jiji service logs --services web --follow
```

### 6. Use Deployment Locks

Prevent concurrent deployments:

```bash
# Acquire lock before deployment
jiji lock acquire "Deploying v1.2.3"

# Deploy
jiji deploy

# Release lock
jiji lock release
```

### 7. Keep Audit Trail

Review audit logs regularly:

```bash
# View recent deployments
jiji audit

# Filter by action or message
jiji audit --grep deploy

# View failures
jiji audit --status failed
```

### 8. Backup Before Major Updates

```bash
# Backup volumes before deployment
jiji server exec "tar -czf /backup/data-$(date +%Y%m%d).tar.gz /data"

# Deploy
jiji deploy

# If issues, restore from backup
```

## Troubleshooting Deployments

### Deployment Fails

**Check deployment logs:**

```bash
jiji --verbose deploy
```

**Common issues:**

- **Build failures**: Check Dockerfile syntax and dependencies
- **Registry authentication**: Verify credentials with `jiji registry login`
- **SSH connection**: Test with `ssh user@server.example.com`
- **Health check failures**: Verify health endpoint returns 200

### Container Won't Start

**Check logs:**

```bash
jiji service logs --services web --lines 200
```

**Check container status:**

```bash
jiji server exec "docker ps -a | grep web"
jiji server exec "docker logs <container-id>"
```

**Common issues:**

- **Port conflicts**: Check if port is already in use
- **Volume mount errors**: Ensure directories exist on server
- **Environment variable errors**: Verify all required variables are set

### Health Check Failures

**Debug health check:**

```bash
# From server, test health endpoint
jiji server exec "curl -I http://localhost:3000/health"

# Check container logs during health check
jiji service logs --services web --follow
```

**Common issues:**

- **Slow startup**: Increase deploy_timeout
- **Endpoint not implemented**: Verify health endpoint exists
- **Dependencies not ready**: Ensure database/services are available

### Rollback Procedure

If deployment fails:

1. **Old container keeps running** (zero downtime)
2. **Check logs** to identify issue
3. **Fix and redeploy**, or
4. **Roll back to the previous version**:
   ```bash
   jiji service rollback --version v1.2.2
   ```

### Network Issues

**Test connectivity:**

```bash
# Check WireGuard status (substitute this project's derived interface
# name, see `jiji network plan`)
jiji server exec "sudo wg show <wireguard_interface>"

# Test DNS resolution
jiji server exec "ping api.jiji"

# Verify routing
jiji server exec "ip route show dev <wireguard_interface>"
```

**Inspect the compiled network plan:**

```bash
jiji network plan
```

## Audit Trail

Jiji keeps an append-only audit log per project on each server, at
`.jiji/{project}/audit.log`. Every state-changing command writes to it:
`jiji deploy`, `service restart`/`rollback`/`remove`/`prune` (one entry per
server, summarizing every endpoint touched on it that run), `jiji lock
acquire`/`release`, and `jiji server setup`/`teardown`. A `server teardown`
entry survives the teardown that produced it: the project's staging
directory (which the audit log lives under) is removed early since it also
holds plaintext secrets, and the final teardown entry recreates that
directory containing nothing but itself.

### View Recent Entries

```bash
# Show last 20 entries per server (default)
jiji audit

# Show more entries
jiji audit --lines 50

# Follow as new entries are appended (requires exactly one host)
jiji audit --follow
```

### Filter Entries

```bash
# Filter by action or message substring
jiji audit --grep deploy
jiji audit --grep web

# Filter by status
jiji audit --status success
jiji audit --status failed
```

### Output Formats

```bash
# JSON output for scripts (one object per line, with a `host` field)
jiji audit --json

# Target specific hosts
jiji audit -H server1.example.com
```

### Audit Entry Fields

Each entry is `{timestamp, action, status, actor, message}`. Actions
currently written: `deploy`, `lock_acquire`, `lock_release`.

## Deployment Locks

Prevent concurrent deployments with deployment locks. Useful for CI/CD pipelines
and team coordination.

### Acquire Lock

```bash
# Acquire lock with message
jiji lock acquire "Deploying v1.2.3 - @username"

# Force acquire (override existing lock)
jiji lock acquire "Emergency fix" --force
```

### Release Lock

```bash
jiji lock release
```

### Check Lock Status

```bash
# Quick status
jiji lock status

# Detailed info
jiji lock show

# JSON output
jiji lock status --json
```

### CI/CD Usage

```bash
# In CI pipeline
jiji lock acquire "CI deploy: $CI_COMMIT_SHA"
jiji deploy
jiji lock release
```

Locks are stored in `.jiji/deploy.lock` on each server and contain:

- Lock message
- Timestamp
- User who acquired it
- Process ID
