# Testing Jiji Deployments

This guide covers how to test your Jiji deployments in different environments.

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

Is running on the same network as kamal-proxy (`jiji`) Has the correct health
check path configured Is actually responding to HTTP requests

### "No such object" errors

The container may not have been deployed yet. Run the deploy command to create
and start service containers before configuring the proxy.

## Testing Quiet Mode

Quiet mode suppresses host headers and reduces verbosity for clean output:

```bash
# Test quiet mode with logs
jiji services logs --services web --quiet

# Verify output has no host headers
# Expected: Only log lines, no [server1.example.com] headers

# Test quiet mode with grep
jiji services logs --services web --grep "ERROR" --quiet | wc -l

# Combine with other tools
jiji services logs --services web --quiet | grep -i "warning" | sort | uniq
```

**What to verify:**

- No host headers in output
- Clean log lines suitable for parsing
- Compatible with piping to other commands
- Log level set to "warn" (no debug/info messages)

## Testing Partial Service Removal

Partial service removal allows removing specific services while keeping others
running:

```bash
# Deploy multiple services first
jiji deploy --services "web,api,worker"

# Verify all services are running
jiji server exec "docker ps | grep myproject"

# Remove only one service
jiji services remove --services "worker"

# Verify worker is removed but web and api still running
jiji server exec "docker ps | grep myproject"
# Should show: web and api containers
# Should NOT show: worker container

# Verify project directory still exists
jiji server exec "ls -la .jiji/"
# Should still exist with audit logs, etc.
```

**What to verify:**

- Specified service containers are stopped and removed
- Other service containers remain running
- Named volumes for removed service are cleaned up
- Service is deregistered from proxy (if using proxy)
- Service is unregistered from network service discovery
- Project directory remains intact
- Other services continue to function normally

**Test with confirmation:**

```bash
# Without confirmation (prompts)
jiji services remove --services "worker"

# With confirmation flag (no prompt)
jiji services remove --services "worker" --confirmed
```

## Testing Port Forwarding for Local Registry

Port forwarding enables remote servers to pull from your local registry:

### Setup Local Registry

```bash
# Start local registry
docker run -d -p 31270:5000 --name test-registry registry:2

# Verify it's running
curl http://localhost:31270/v2/
# Expected: {}
```

### Test SSH Reverse Tunnel

```bash
# Manually create SSH reverse tunnel
ssh -R 31270:localhost:31270 user@server1.example.com

# On remote server, verify tunnel
curl http://localhost:31270/v2/
# Expected: {} (same as local)

# Exit SSH session to close tunnel
exit
```

### Test with Jiji Deployment

```yaml
# Configure local registry in .jiji/deploy.yml
builder:
  registry:
    type: local
    port: 31270
```

```bash
# Deploy (Jiji automatically creates tunnel)
jiji deploy --build

# Verify deployment creates tunnel
# Check Jiji output for port forwarding messages

# On remote server during deployment
ssh user@server1.example.com "netstat -tlnp | grep 31270"
# Expected: Listen on 127.0.0.1:31270
```

**What to verify:**

- Local registry is accessible
- SSH tunnel established automatically
- Remote server can pull from localhost:31270
- Tunnel torn down after deployment
- Deployment completes successfully

### Troubleshooting Port Forwarding

**Issue: Remote can't connect to localhost:31270**

```bash
# On remote server /etc/ssh/sshd_config
sudo grep -E "(AllowTcpForwarding|GatewayPorts)" /etc/ssh/sshd_config

# Should show:
# AllowTcpForwarding yes
# GatewayPorts yes (or clientspecified)

# If not, add and restart sshd
sudo systemctl restart sshd
```

**Issue: Permission denied for port forwarding**

```bash
# Use unprivileged port (>1024)
# Or configure SSH to allow port forwarding
```

## Testing with Multiple Environments

Test environment specific configurations:

```bash
# Create environment configs
cp .jiji/deploy.yml jiji.staging.yml
cp .jiji/deploy.yml jiji.production.yml

# Test staging
jiji --environment staging deploy --build

# Verify correct config is used
jiji --environment staging server exec "docker ps"

# Test production
jiji --environment production deploy --build
```

## Automated Testing

### Integration Test Example

```bash
#!/bin/bash
set -e

echo "Testing Jiji deployment..."

# 1. Initialize config
jiji init
cat > .jiji/deploy.yml << EOF
project: test-project
builder:
  engine: docker
  local: true
  registry:
    type: local
    port: 31270
ssh:
  user: deploy
servers:
  test-server:
    host: test-server.example.com
services:
  test-web:
    image: nginx:latest
    hosts:
      - test-server
    ports:
      - "80"
EOF

# 2. Test SSH connection
jiji server exec "whoami"

# 3. Test deployment
jiji deploy

# 4. Verify container running
jiji server exec "docker ps | grep test-web"

# 5. Test logs
jiji services logs --services test-web --lines 10

# 6. Test partial removal
jiji services remove --services test-web --confirmed

# 7. Verify removed
! jiji server exec "docker ps | grep test-web"

echo "All tests passed!"
```

## Performance Testing

### Measure Deployment Time

```bash
# Time a full deployment
time jiji deploy --build

# Time without build cache
time jiji deploy --build --no-cache

# Time with quiet mode
time jiji --quiet deploy --build
```

### Monitor Resource Usage

```bash
# During deployment, monitor server resources
jiji server exec "top -bn1 | head -20"
jiji server exec "free -h"
jiji server exec "df -h"
```
