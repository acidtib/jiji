# Registry Reference

Jiji automatically detects namespace requirements for supported container
registries, providing a simple configuration experience.

## Supported Registries

### GitHub Container Registry (GHCR)

Server: `ghcr.io` Auto namespace: `username/project-name`

```yaml
builder:
  registry:
    type: remote
    server: ghcr.io
    username: your-github-username
    password: GITHUB_TOKEN
```

Result: Images are pushed to
`ghcr.io/your-github-username/project-name/service:version`

### Docker Hub

Server: `docker.io`, `registry-1.docker.io`, `index.docker.io` Auto namespace:
`username`

```yaml
builder:
  registry:
    type: remote
    server: docker.io
    username: your-dockerhub-username
    password: DOCKER_PASSWORD
```

Result: Images are pushed to
`docker.io/your-dockerhub-username/project-service:version`

### Local Registry

Type: `local` No namespace required

```yaml
builder:
  registry:
    type: local
    port: 9270 # optional, defaults to 9270
```

Result: Images are stored at `localhost:9270/project-service:version`

## Environment Setup

### GitHub Container Registry

1. Create a Personal Access Token at https://github.com/settings/tokens
2. Grant `write:packages` and `read:packages` permissions
3. Set environment variable: `export GITHUB_TOKEN=ghp_your_token_here`

### Docker Hub

1. Use your Docker Hub password or create an access token at
   https://hub.docker.com/settings/security
2. Set environment variable: `export DOCKER_PASSWORD=your_password_or_token`

## Registry Configuration

Jiji uses a configuration driven approach for registry management. Registry
settings are defined in your `.jiji/deploy.yml` file and used across all
deployments.

### Configuration Location

Registry configuration is part of your project configuration in:

```yaml
# .jiji/deploy.yml
builder:
  registry:
    type: remote # or "local"
    server: ghcr.io
    username: your-github-username
    password: GITHUB_TOKEN
```

### Registry Commands

```bash
# Login to registry (locally and on remote servers)
jiji registry login

# Login with flags
jiji registry login --skip-remote  # Skip remote server authentication
jiji registry login --skip-local   # Skip local authentication

# Logout from registry
jiji registry logout

# Logout with flags
jiji registry logout --skip-remote  # Skip remote server logout
jiji registry logout --skip-local   # Skip local logout

# Remove local registry container or logout
jiji registry remove
```

### Local vs Remote Operations

Jiji distinguishes between local and remote registry operations:

- **Local**: Authentication on your development machine (where you run `jiji`
  commands)
- **Remote**: Authentication on deployment servers (where containers run)

Use `--skip-local` or `--skip-remote` flags to control where authentication
happens.

### Benefits

- **Configuration as code**: Registry settings versioned with your project
- **Environment specific configs**: Use different registries per environment
  (staging, production)
- **Secure secrets**: Passwords can reference secret names for secure handling
- **Consistent deployments**: Same registry configuration across all team
  members

## Registry Password

Registry passwords can use a secret name:

```yaml
builder:
  registry:
    server: ghcr.io
    username: myuser
    password: GITHUB_TOKEN
```

When the password is an ALL_CAPS name like `GITHUB_TOKEN`, it will be resolved
from the secrets system. See the Environment Configuration documentation for
details on how secrets and `.env` files work.

### Best Practices

1. **Never commit `.env` files**: Add `.env*` to your `.gitignore`
2. **Use environment-specific files**: `.env.staging`, `.env.production`
3. **CI/CD integration**: Use secrets management:
   ```yaml
   # GitHub Actions example
   - name: Deploy
     env:
       GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
     run: jiji deploy
   ```

### Security Notes

- Environment variables are only substituted at runtime
- Passwords are never written to configuration files in plain text
- Use separate tokens for each environment (staging, production)

## Port Forwarding for Local Registries

When using a local registry, Jiji can automatically set up SSH port forwarding
to allow remote servers to access the registry on your local machine.

### How It Works

```
Local Machine (localhost:9270)
  |
  | SSH Reverse Tunnel
  |
Remote Server (localhost:9270)
  |
  | Pull image
  V
Container Deployment
```

### Automatic Setup

Jiji automatically creates SSH reverse tunnels when:

1. Registry type is `local`
2. Deploying to remote servers
3. SSH connection is established

```bash
# Deploy with local registry
jiji deploy

# Jiji automatically:
# 1. Establishes SSH connection to remote servers
# 2. Creates reverse tunnel: remote:9270 -> local:9270
# 3. Remote servers pull from localhost:9270
# 4. Tunnel is torn down after deployment
```

### Manual Port Forward

You can also manually set up port forwarding:

```bash
# Forward local registry to remote server
ssh -R 9270:localhost:9270 user@server1.example.com

# On remote server, pull from localhost:9270
docker pull localhost:9270/myapp/service:latest
```

### Configuration

Local registry configuration:

```yaml
builder:
  local: true # Build locally, push to local registry
  registry:
    type: local
    port: 9270 # Default port (customizable)
```

### Troubleshooting Port Forwarding

**Issue**: Remote server can't connect to localhost:9270

**Solutions**:

1. Verify local registry is running:
   ```bash
   curl http://localhost:9270/v2/
   ```
2. Check SSH allows port forwarding:
   ```bash
   # On remote server /etc/ssh/sshd_config
   GatewayPorts yes
   AllowTcpForwarding yes
   ```
3. Verify tunnel is established:
   ```bash
   # On remote server
   netstat -tlnp | grep 9270
   ```

**Issue**: Permission denied for port forwarding

**Solution**: Use unprivileged port (>1024) or configure SSH permissions

### Benefits

- **No external registry needed**: Test deployments without GHCR/Docker Hub
- **Faster iteration**: No push/pull from remote registry
- **Offline development**: Works without internet connection
- **Secure**: Traffic encrypted via SSH tunnel

## Troubleshooting

### GHCR 403 Forbidden Error

If you see a 403 Forbidden error when pushing to GHCR, ensure:

1. Your GitHub token has `write:packages` permission
2. The username matches your GitHub username or organization
3. The repository exists or the token has permission to create packages

### Username Required Error

For GHCR and Docker Hub, the username is required for automatic namespace
detection:

```
GHCR requires username to be configured
```

**Solution:** Add the `username` field to your registry configuration.
