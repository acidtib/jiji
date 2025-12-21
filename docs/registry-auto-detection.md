# Registry Auto-Detection

Jiji automatically detects namespace requirements for supported container
registries, providing a simple configuration experience.

## Supported Registries

### GitHub Container Registry (GHCR)

Server: `ghcr.io`
Auto-namespace: `username/project-name`

```yaml
builder:
  registry:
    type: remote
    server: ghcr.io
    username: your-github-username
    password: "${GITHUB_TOKEN}"
```

Result: Images are pushed to
`ghcr.io/your-github-username/project-name/service:version`

### Docker Hub

Server: `docker.io`, `registry-1.docker.io`, `index.docker.io`
Auto-namespace: `username`

```yaml
builder:
  registry:
    type: remote
    server: docker.io
    username: your-dockerhub-username
    password: "${DOCKER_PASSWORD}"
```

Result: Images are pushed to
`docker.io/your-dockerhub-username/project-service:version`

### Local Registry

Type: `local`
No namespace required

```yaml
builder:
  registry:
    type: local
    port: 6767 # optional, defaults to 6767
```

Result: Images are stored at `localhost:6767/project-service:version`

## Environment Setup

### GitHub Container Registry

1. Create a Personal Access Token at https://github.com/settings/tokens
2. Grant `write:packages` and `read:packages` permissions
3. Set environment variable: `export GITHUB_TOKEN=ghp_your_token_here`

### Docker Hub

1. Use your Docker Hub password or create an access token at
   https://hub.docker.com/settings/security
2. Set environment variable: `export DOCKER_PASSWORD=your_password_or_token`

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

## Examples

See `examples/ghcr-config.yml` and `examples/registry-examples.yml` for
configuration examples.
