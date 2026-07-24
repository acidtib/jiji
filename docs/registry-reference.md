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
    port: 31270 # optional, defaults to 31270
```

Result: Images are stored at `localhost:31270/project-service:version`

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

`jiji build` and `jiji deploy --build` perform registry setup automatically.
Remote registries are authenticated locally before pushing and on selected
deployment hosts before pulling. Local registries require no credentials.

Authenticate or clear credentials explicitly with:

```bash
jiji registry login
jiji registry login --skip-local
jiji registry login --skip-remote

jiji registry logout
jiji registry logout --skip-local
jiji registry logout --skip-remote
```

By default both commands act on two targets, in order: the local development
machine, then every server selected by `-H`/`--hosts` (all configured servers
when `-H` is omitted). `--skip-local` and `--skip-remote` each remove one side;
passing both is rejected before anything runs. `-S`/`--services` is rejected:
registry credentials belong to a host's container engine, not an individual
service.

`jiji registry login` requires `builder.registry.server`, `username`, and
`password` to be configured; the password is resolved the same way as for
`build`/`deploy` (a literal value, or an ALL_CAPS secret name looked up in the
selected `.env` file, with host-environment fallback only via `--host-env`).
The password is sent to the container engine over stdin only -- it is never
placed in a command string, logged, or printed. `jiji registry logout` only
requires `builder.registry.server` and is idempotent: an engine reporting the
target was already logged out (for example Podman's `not logged into ...`) is
treated as success, not a failure.

Both commands attempt every requested target even if one fails, then exit
nonzero if any target failed, with a per-target error above the summary line.
SSH connections are only opened when at least one remote target is requested;
`--skip-remote` never requires an `ssh:` section.

A configured local registry needs no authentication: both commands report
that immediately and exit successfully without starting the registry, opening
tunnels, or running any container-engine command.

Remove the local registry container with:

```bash
jiji registry teardown
jiji registry teardown --dry-run
jiji registry teardown --yes
```

Teardown verifies the Jiji ownership label and configured port before removing
the exact `jiji-registry` container. It refuses to remove a conflicting
container.

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
Local Machine (localhost:31270)
  |
  | SSH Reverse Tunnel
  |
Remote Server (localhost:31270)
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
# 1. Starts or reuses the loopback-bound jiji-registry container
# 2. Builds and pushes versioned images to localhost:31270
# 3. Establishes SSH connections to selected deployment servers
# 4. Creates reverse tunnels: remote 127.0.0.1:31270 -> local 127.0.0.1:31270
# 5. Forces each deployment server to pull the newly built image
# 6. Tears the tunnels down after deployment
```

The registry container is named `jiji-registry` and labeled as Jiji-managed.
If that name belongs to another container, or its recorded port differs from
the configured port, Jiji stops and asks you to resolve the conflict. The
registry remains running for later builds, while SSH tunnels exist only for
the deployment session.

### Manual Port Forward

You can also manually set up port forwarding:

```bash
# Forward local registry to remote server
ssh -R 31270:localhost:31270 user@server1.example.com

# On remote server, pull from localhost:31270
docker pull localhost:31270/myapp/service:latest
```

### Configuration

Local registry configuration:

```yaml
builder:
  local: true # Build locally, push to local registry
  registry:
    type: local
    port: 31270 # Default port (customizable)
```

### Troubleshooting Port Forwarding

**Issue**: Remote server can't connect to localhost:31270

**Solutions**:

1. Verify local registry is running:
   ```bash
   curl http://localhost:31270/v2/
   ```
2. Check SSH allows port forwarding:
   ```bash
   # On remote server /etc/ssh/sshd_config
   AllowTcpForwarding yes
   ```

Jiji binds the forwarded registry port to remote `127.0.0.1`. `GatewayPorts`
is not required and should not be enabled solely for Jiji because it can expose
forwarded services on non-loopback interfaces.
3. Verify tunnel is established:
   ```bash
   # On remote server
   netstat -tlnp | grep 31270
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
