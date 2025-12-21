# Testing Jiji Deployments

This guide covers how to test your Jiji deployments in different environments.

## Running Tests

```bash
# Run all tests
deno task test

# Run a specific test file
deno test --allow-all src/lib/configuration/tests/configuration_test.ts

# Run tests matching a pattern
deno test --allow-all --filter "Configuration" src/lib/configuration/tests/

# Run all checks (format, lint, test)
deno task check
```

## Testing Proxy Deployments

### Local Testing Without DNS

When testing kamal-proxy deployments without setting up DNS, use the `Host`
header to route requests:

```bash
# Test from your local machine
curl -H "Host: myproject.example.com" http://192.168.1.87

# Test from the remote host itself
curl -H "Host: myproject.example.com" http://localhost
```

### Using /etc/hosts for Local Testing

For easier testing in browsers, add an entry to your hosts file:

**Linux/Mac:**

```bash
sudo nano /etc/hosts

# Add this line:
192.168.1.87  myproject.example.com
```

**Windows:**

```powershell
# Edit C:\Windows\System32\drivers\etc\hosts as Administrator
# Add this line:
192.168.1.87  myproject.example.com
```

Then access directly:

```bash
curl http://myproject.example.com
# or visit http://myproject.example.com in your browser
```

### Testing Direct Container Access

Bypass the proxy and test containers directly using their host port mappings:

```bash
# Access nginx directly on mapped port
curl http://192.168.1.87:3000
```

### Verifying Proxy Configuration

Check what services are registered with kamal-proxy:

```bash
# SSH into the host
ssh root@192.168.1.87

# List running containers
podman ps

# Check kamal-proxy logs
podman logs kamal-proxy

# Inspect container networks
podman inspect jellyfin-web --format '{{.NetworkSettings.Networks}}'
```

## Testing SSH Connections

```bash
# Test SSH connectivity manually
ssh -o ConnectTimeout=10 root@192.168.1.87 "echo 'Connection successful'"

# Verify SSH agent is running
echo $SSH_AUTH_SOCK

# List SSH keys loaded in agent
ssh-add -l
```

## Testing Container Engine

```bash
# Verify podman/docker is available
podman version
# or
docker version

# Test pulling an image
podman pull docker.io/library/nginx:latest
```

## Common Issues

### "Permission denied" on port 80

For rootless Podman, kamal-proxy runs on high ports (8080/8443) internally and
maps to host ports 80/443. Ensure the container has the right configuration.

### Proxy health check timeouts

Ensure your service container:

Is running on the same network as kamal-proxy (`jiji`)
Has the correct health check path configured
Is actually responding to HTTP requests

### "No such object" errors

The container may not have been deployed yet. Run the deploy command to create
and start service containers before configuring the proxy.
