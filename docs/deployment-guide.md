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

## Initial Setup

### Prerequisites

- jiji installed
- SSH access to target servers
- Servers running Ubuntu 24.04+
- Docker or Podman installed on servers (or will be installed by Jiji)

### 1. Install Jiji

```bash
# Linux/MacOS
curl -fsSL https://get.jiji.run/install.sh | sh

# Or with JSR (Deno)
deno install --allow-all --name jiji jsr:@jiji/cli
```

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
    port: 6767

ssh:
  user: root

services:
  web:
    build:
      context: .
      dockerfile: Dockerfile
    servers:
      - host: server1.example.com
        arch: amd64
    ports:
      - "3000:3000"
    proxy:
      enabled: true
      hosts:
        - myapp.example.com
      health_check:
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
  proxy_jump: bastion.example.com
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
    password: "${GITHUB_TOKEN}"
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
    port: 6767 # Jiji handles port forwarding automatically
```

### 6. Initialize Servers

```bash
# Install container runtime and setup infrastructure
jiji server init

# This will:
# - Install Docker/Podman
# - Setup private networking (WireGuard)
# - Install service discovery (Corrosion, CoreDNS)
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
jiji services logs --services web --follow

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
      enabled: true
      hosts:
        - myapp.example.com
      health_check:
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
jiji services logs --services web --follow

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
    password: "${GITHUB_TOKEN}"

ssh:
  user: deploy
  config: true

services:
  web:
    build:
      context: .
      args:
        - BUILD_ENV=staging
    servers:
      - host: staging.example.com
        arch: amd64
    environment:
      clear:
        APP_ENV: staging
        LOG_LEVEL: debug
    proxy:
      enabled: true
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
    password: "${GITHUB_TOKEN}"

ssh:
  user: deploy
  private_keys:
    - ~/.ssh/production_key
  proxy_jump: bastion.example.com

services:
  web:
    build:
      context: .
      args:
        - BUILD_ENV=production
    servers:
      - host: web1.example.com
        arch: amd64
      - host: web2.example.com
        arch: amd64
    environment:
      clear:
        APP_ENV: production
        LOG_LEVEL: warn
    proxy:
      enabled: true
      hosts:
        - myapp.example.com
        - www.myapp.example.com
      ssl: true
      health_check:
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
# Redeploy previous version
jiji deploy --version v1.2.2

# Or rebuild from previous git commit
git checkout v1.2.2
jiji deploy --build
```

### Restart Services

```bash
# Restart all instances of a service
jiji services restart --services web

# Restart on specific host
jiji services restart --services web --hosts server1.example.com
```

### View Logs

```bash
# View recent logs
jiji services logs --services web --lines 100

# Follow logs in real-time
jiji services logs --services web --follow

# Filter for errors
jiji services logs --services web --grep "ERROR" --since "1h"
```

### Clean Up Old Images

```bash
# Remove old image versions (keeps last 5)
jiji services prune

# Keep more versions
jiji services prune --retain 10

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
  health_check:
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
jiji services logs --services web --follow
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

# Filter by service
jiji audit --filter deploy

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
jiji services logs --services web --lines 200
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
jiji services logs --services web --follow
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
4. **Deploy previous version**:
   ```bash
   jiji deploy --version v1.2.2
   ```

### Network Issues

**Test connectivity:**

```bash
# Check WireGuard status
jiji server exec "sudo wg show jiji0"

# Test DNS resolution
jiji server exec "ping api.jiji"

# Verify routing
jiji server exec "ip route show | grep jiji0"
```

**Check network status:**

```bash
jiji network status
```

### Get Help

- **Verbose logging**: `jiji --verbose <command>`
- **Audit trail**: `jiji audit`
- **GitHub Issues**: https://github.com/acidtib/jiji/issues
- **Discussions**: https://github.com/acidtib/jiji/discussions
