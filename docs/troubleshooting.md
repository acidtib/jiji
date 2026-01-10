# Jiji Troubleshooting Guide

Troubleshooting guide for common Jiji issues and their solutions.

## Table of Contents

- [SSH Connection Issues](#ssh-connection-issues)
- [Registry Issues](#registry-issues)
- [Deployment Failures](#deployment-failures)
- [Network Issues](#network-issues)
- [Container Issues](#container-issues)
- [Build Failures](#build-failures)
- [Proxy Issues](#proxy-issues)
- [Performance Issues](#performance-issues)
- [Debugging Tools](#debugging-tools)
- [Common Error Messages](#common-error-messages)

## SSH Connection Issues

### Permission Denied (publickey)

**Symptoms:**

```
Error: Permission denied (publickey)
Could not connect to server1.example.com
```

**Solutions:**

1. **Verify SSH key is added to server:**
   ```bash
   # From your local machine
   ssh-copy-id deploy@server1.example.com

   # Or manually add to server's authorized_keys
   cat ~/.ssh/id_rsa.pub | ssh deploy@server1.example.com "mkdir -p ~/.ssh && cat >> ~/.ssh/authorized_keys"
   ```

2. **Check SSH agent:**
   ```bash
   # Start SSH agent
   eval $(ssh-agent)

   # Add your key
   ssh-add ~/.ssh/id_rsa

   # Verify keys are loaded
   ssh-add -l
   ```

3. **Specify key in configuration:**
   ```yaml
   ssh:
     user: deploy
     private_keys:
       - ~/.ssh/id_rsa
   ```

4. **Check file permissions on server:**
   ```bash
   ssh deploy@server1.example.com
   chmod 700 ~/.ssh
   chmod 600 ~/.ssh/authorized_keys
   ```

### Connection Timeout

**Symptoms:**

```
Error: Connection timed out after 30000ms
Could not connect to server1.example.com:22
```

**Solutions:**

1. **Verify server is reachable:**
   ```bash
   ping server1.example.com
   telnet server1.example.com 22
   ```

2. **Check firewall allows SSH:**
   ```bash
   # On server
   sudo ufw status
   sudo ufw allow 22/tcp
   ```

3. **Increase timeout:**
   ```yaml
   ssh:
     user: deploy
     timeout: 60000 # 60 seconds
   ```

4. **Check SSH service on server:**
   ```bash
   ssh deploy@server1.example.com
   sudo systemctl status sshd
   sudo systemctl restart sshd
   ```

### ProxyJump/Bastion Issues

**Symptoms:**

```
Error: Could not establish proxy connection through bastion.example.com
```

**Solutions:**

1. **Test bastion connection:**
   ```bash
   ssh deploy@bastion.example.com
   ```

2. **Test through bastion:**
   ```bash
   ssh -J deploy@bastion.example.com deploy@server1.example.com
   ```

3. **Verify proxy configuration:**
   ```yaml
   ssh:
     user: deploy
     proxy: deploy@bastion.example.com
   ```

4. **Use ProxyCommand for advanced scenarios:**
   ```yaml
   ssh:
     user: deploy
     proxy_command: "ssh -W %h:%p deploy@bastion.example.com"
   ```

### Too Many Authentication Failures

**Symptoms:**

```
Error: Received too many authentication failures
```

**Solutions:**

1. **Use keys only mode:**
   ```yaml
   ssh:
     user: deploy
     keys_only: true
     private_keys:
       - ~/.ssh/deploy_key
   ```

2. **Limit SSH agent keys:**
   ```bash
   # Remove all keys from agent
   ssh-add -D

   # Add only the key you need
   ssh-add ~/.ssh/deploy_key
   ```

## Registry Issues

### 403 Forbidden (GHCR)

**Symptoms:**

```
Error: Failed to push image to ghcr.io: 403 Forbidden
```

**Solutions:**

1. **Verify GitHub token has correct permissions:**
   - Go to https://github.com/settings/tokens
   - Ensure token has `write:packages` and `read:packages` permissions
   - Create new token if needed

2. **Verify username matches:**
   ```yaml
   registry:
     server: ghcr.io
     username: your-github-username # Must match GitHub username
     password: GITHUB_TOKEN
   ```

3. **Re login to registry:**
   ```bash
   # Add to your .env file:
   # GITHUB_TOKEN=ghp_new_token
   jiji registry login
   ```

### 401 Unauthorized

**Symptoms:**

```
Error: Failed to authenticate with registry: 401 Unauthorized
```

**Solutions:**

1. **Verify credentials:**
   ```bash
   # Test credentials manually
   echo $GITHUB_TOKEN | docker login ghcr.io -u your-username --password-stdin
   ```

2. **Re login:**
   ```bash
   jiji registry logout
   jiji registry login
   ```

3. **Check environment variable:**
   ```bash
   echo $GITHUB_TOKEN  # Should output your token
   ```

### Push Failed - Image Not Found

**Symptoms:**

```
Error: Failed to push image: requested access to the resource is denied
```

**Solutions:**

1. **Verify image was built:**
   ```bash
   docker images | grep myproject
   ```

2. **Check build completed:**
   ```bash
   jiji build --verbose
   ```

3. **Verify registry namespace:**
   - GHCR: `ghcr.io/username/project-name/service:version`
   - Docker Hub: `docker.io/username/project-service:version`

### Local Registry Not Accessible

**Symptoms:**

```
Error: Failed to connect to localhost:9270
```

**Solutions:**

1. **Verify local registry is running:**
   ```bash
   curl http://localhost:9270/v2/
   ```

2. **Start local registry:**
   ```bash
   docker run -d -p 9270:5000 --name jiji-registry registry:2
   ```

3. **Check port forwarding:**
   ```bash
   # On remote server during deployment
   netstat -tlnp | grep 9270
   ```

4. **Verify SSH allows port forwarding:**
   ```bash
   # On remote server /etc/ssh/sshd_config
   AllowTcpForwarding yes
   GatewayPorts yes
   ```

## Deployment Failures

### Health Check Timeout

**Symptoms:**

```
Error: Health check failed after 60s
Container did not become healthy in time
```

**Solutions:**

1. **Increase deploy timeout:**
   ```yaml
   proxy:
     health_check:
       deploy_timeout: "120s" # Increase timeout
   ```

2. **Check health endpoint:**
   ```bash
   # From server, test health endpoint
   jiji server exec "curl -I http://localhost:3000/health"
   ```

3. **View container logs:**
   ```bash
   jiji services logs --services web --lines 200
   ```

4. **Check container is running:**
   ```bash
   jiji server exec "docker ps | grep web"
   ```

5. **Verify health endpoint implementation:**
   ```javascript
   // Ensure endpoint returns 200 when ready
   app.get("/health", (req, res) => {
     res.status(200).json({ status: "ok" });
   });
   ```

### Container Fails to Start

**Symptoms:**

```
Error: Container exited with code 1
```

**Solutions:**

1. **Check container logs:**
   ```bash
   jiji services logs --services web --lines 100

   # Or check directly on server
   jiji server exec "docker logs <container-id>"
   ```

2. **Common causes:**
   - **Missing environment variables:**
     ```bash
     # Verify env vars are set
     jiji services logs --services web | grep "environment"
     ```

   - **Port already in use:**
     ```bash
     # Check what's using the port
     jiji server exec "sudo lsof -i :3000"
     ```

   - **Volume mount errors:**
     ```bash
     # Verify mount paths exist
     jiji server exec "ls -la /data/web"
     ```

3. **Test container locally:**
   ```bash
   # Run container locally first
   docker run -p 3000:3000 myapp/web:latest
   ```

### Deployment Hangs

**Symptoms:**

```
Deployment appears stuck, no progress for several minutes
```

**Solutions:**

1. **Enable verbose logging:**
   ```bash
   jiji --verbose deploy
   ```

2. **Check SSH connection:**
   ```bash
   # Test SSH connection
   ssh deploy@server1.example.com
   ```

3. **Check server resources:**
   ```bash
   # Check CPU/memory
   jiji server exec "top -bn1 | head -20"

   # Check disk space
   jiji server exec "df -h"
   ```

4. **Check for stuck containers:**
   ```bash
   jiji server exec "docker ps -a"
   ```

5. **Kill and retry:**
   ```bash
   # Ctrl+C to cancel
   # Clean up and retry
   jiji deploy
   ```

## Network Issues

### WireGuard Not Connecting

**Symptoms:**

```
Error: WireGuard peers not establishing handshake
No recent handshake times
```

**Solutions:**

1. **Check WireGuard status:**
   ```bash
   jiji server exec "sudo wg show jiji0"

   # Look for "latest handshake" times
   # Should be recent (< 2 minutes)
   ```

2. **Verify firewall allows WireGuard:**
   ```bash
   # Allow UDP 51820
   jiji server exec "sudo ufw allow 51820/udp"

   # Verify rule
   jiji server exec "sudo ufw status | grep 51820"
   ```

3. **Check network status:**
   ```bash
   jiji network status
   ```

4. **Restart WireGuard:**
   ```bash
   jiji server exec "sudo systemctl restart wg-quick@jiji0"
   ```

5. **Check routing:**
   ```bash
   jiji server exec "ip route show | grep jiji0"
   ```

### DNS Resolution Failures

**Symptoms:**

```
Error: Could not resolve api.jiji
ping: api.jiji: Name or service not known
```

**Solutions:**

1. **Check jiji-dns status:**
   ```bash
   jiji server exec "sudo systemctl status jiji-dns"
   ```

2. **Restart jiji-dns:**
   ```bash
   jiji server exec "sudo systemctl restart jiji-dns"
   ```

3. **Verify DNS configuration:**
   ```bash
   # Check daemon DNS config
   jiji server exec "cat /etc/docker/daemon.json"

   # Should include DNS servers
   ```

4. **Test DNS resolution:**
   ```bash
   # From container
   jiji server exec "docker exec <container> nslookup api.jiji"
   ```

5. **Check Corrosion:**
   ```bash
   jiji server exec "sudo systemctl status jiji-corrosion"
   ```

### Container Can't Reach Other Containers

**Symptoms:**

```
Error: Connection refused when trying to connect to api.jiji
```

**Solutions:**

1. **Verify routing:**
   ```bash
   # Check IP forwarding is enabled
   jiji server exec "sysctl net.ipv4.ip_forward"
   # Should output: net.ipv4.ip_forward = 1

   # Check routes exist
   jiji server exec "ip route show | grep jiji0"
   ```

2. **Check iptables rules:**
   ```bash
   jiji server exec "sudo iptables -L FORWARD -n -v | grep jiji0"
   ```

3. **Test connectivity:**
   ```bash
   # From one container, ping another
   jiji server exec "docker exec web-container ping 10.210.1.10"
   ```

4. **Verify containers are on network:**
   ```bash
   jiji server exec "docker network inspect jiji"
   ```

5. **Check firewall isn't blocking:**
   ```bash
   jiji server exec "sudo ufw status"
   ```

## Container Issues

### Container Crashes Repeatedly

**Symptoms:**

```
Container keeps restarting
Docker shows container in restart loop
```

**Solutions:**

1. **Check logs for errors:**
   ```bash
   jiji services logs --services web --lines 500
   ```

2. **Check resource limits:**
   ```bash
   # Check memory usage
   jiji server exec "docker stats --no-stream"
   ```

3. **Disable restart policy temporarily:**
   ```bash
   jiji server exec "docker update --restart=no <container-id>"
   ```

4. **Run container interactively:**
   ```bash
   jiji server exec "docker run -it myapp/web:latest /bin/bash"
   ```

### Volume Mount Permission Denied

**Symptoms:**

```
Error: Permission denied: /data/app
Cannot write to mounted volume
```

**Solutions:**

1. **Check directory permissions:**
   ```bash
   jiji server exec "ls -la /data/app"
   ```

2. **Fix ownership:**
   ```bash
   # Find container user ID
   jiji server exec "docker exec <container> id"

   # Change ownership
   jiji server exec "sudo chown -R 1000:1000 /data/app"
   ```

3. **Create directory with correct permissions:**
   ```bash
   jiji server exec "sudo mkdir -p /data/app && sudo chmod 755 /data/app"
   ```

### Port Already in Use

**Symptoms:**

```
Error: Bind for 0.0.0.0:3000 failed: port is already allocated
```

**Solutions:**

1. **Find what's using the port:**
   ```bash
   jiji server exec "sudo lsof -i :3000"
   jiji server exec "sudo netstat -tlnp | grep :3000"
   ```

2. **Stop conflicting container:**
   ```bash
   jiji server exec "docker ps | grep 3000"
   jiji server exec "docker stop <container-id>"
   ```

3. **Use different port:**
   ```yaml
   ports:
     - "3001:3000"
   ```

## Build Failures

### Dockerfile Not Found

**Symptoms:**

```
Error: Cannot find Dockerfile at ./Dockerfile
```

**Solutions:**

1. **Verify Dockerfile exists:**
   ```bash
   ls -la Dockerfile
   ```

2. **Specify Dockerfile path:**
   ```yaml
   build:
     context: .
     dockerfile: docker/Dockerfile.production
   ```

3. **Check build context:**
   ```yaml
   build:
     context: ./backend # Dockerfile should be in ./backend/
   ```

### Build Context Too Large

**Symptoms:**

```
Error: Build context is too large (>500MB)
Sending build context to Docker daemon...
```

**Solutions:**

1. **Create .dockerignore:**
   ```
   node_modules
   .git
   *.log
   dist
   coverage
   ```

2. **Use smaller build context:**
   ```yaml
   build:
     context: ./src # Only send src directory
   ```

### Build Arguments Not Working

**Symptoms:**

```
Error: ARG values not being substituted in Dockerfile
```

**Solutions:**

1. **Verify ARG in Dockerfile:**
   ```dockerfile
   ARG NODE_ENV=production
   ENV NODE_ENV=$NODE_ENV
   ```

2. **Pass build args in config:**
   ```yaml
   build:
     context: .
     args:
       - NODE_ENV=production
       - VERSION=1.2.3
   ```

## Proxy Issues

### Proxy Not Routing Traffic

**Symptoms:**

```
502 Bad Gateway when accessing https://myapp.example.com
```

**Solutions:**

1. **Check proxy logs:**
   ```bash
   jiji proxy logs --lines 100
   ```

2. **Verify proxy is running:**
   ```bash
   jiji server exec "docker ps | grep kamal-proxy"
   ```

3. **Check proxy configuration:**
   ```bash
   jiji server exec "docker exec kamal-proxy cat /config/routes.json"
   ```

4. **Restart proxy:**
   ```bash
   jiji server exec "docker restart kamal-proxy"
   ```

5. **Verify health endpoint:**
   ```bash
   curl -I http://server1.example.com:3000/health
   ```

### SSL/TLS Issues

**Symptoms:**

```
Error: SSL handshake failed
Certificate verification failed
```

**Solutions:**

1. **Verify SSL is enabled:**
   ```yaml
   proxy:
     enabled: true
     hosts:
       - myapp.example.com
     ssl: true # Must be true for SSL
   ```

2. **Check certificate:**
   ```bash
   curl -vI https://myapp.example.com 2>&1 | grep -i certificate
   ```

3. **Verify host configuration:**
   - Ensure DNS points to server
   - Ensure firewall allows 80/443

## Performance Issues

### Slow Deployments

**Symptoms:**

```
Deployments taking 10+ minutes
```

**Solutions:**

1. **Enable build cache:**
   ```yaml
   builder:
     cache: true
   ```

2. **Use .dockerignore:**
   ```
   node_modules
   .git
   ```

3. **Optimize Dockerfile:**
   ```dockerfile
   # Copy package files first (better caching)
   COPY package*.json ./
   RUN npm install

   # Then copy source
   COPY . .
   ```

4. **Use remote builder:**
   ```yaml
   builder:
     local: false
     remote: ssh://powerful-server.example.com
   ```

### High Memory Usage

**Symptoms:**

```
Server running out of memory
OOM (Out of Memory) errors
```

**Solutions:**

1. **Check container memory:**
   ```bash
   jiji server exec "docker stats --no-stream"
   ```

2. **Add memory limits:**
   ```yaml
   services:
     web:
       memory: "512m"
       cpus: 1
   ```

3. **Clean up old images:**
   ```bash
   jiji services prune
   jiji server exec "docker system prune -f"
   ```

4. **Monitor system resources:**
   ```bash
   jiji server exec "free -h"
   jiji server exec "df -h"
   ```

## Debugging Tools

### Enable Verbose Logging

```bash
# Global verbose flag
jiji --verbose deploy
jiji --verbose build
jiji --verbose network status

# Shows detailed SSH operations, command execution, etc.
```

### Check Audit Logs

```bash
# View all audit entries
jiji audit

# Filter by action
jiji audit --filter deploy
jiji audit --filter init

# Filter by status
jiji audit --status failed
jiji audit --status success

# Specific host
jiji audit --host server1.example.com

# Aggregate chronologically
jiji audit --aggregate
```

### Interactive Server Access

```bash
# Execute commands on servers
jiji server exec "docker ps"
jiji server exec "systemctl status docker"

# Interactive bash session
jiji server exec "bash" --interactive --hosts server1.example.com
```

### Network Diagnostics

```bash
# Check network status
jiji network status

# WireGuard status
jiji server exec "sudo wg show jiji0"

# DNS test
jiji server exec "dig @10.210.0.1 api.jiji"

# Routing table
jiji server exec "ip route show | grep jiji0"

# iptables rules
jiji server exec "sudo iptables -L -n -v"
```

## Common Error Messages

### "Configuration validation failed"

**Cause:** Invalid configuration file syntax or missing required fields.

**Solution:**

```bash
# Check YAML syntax
yamllint .jiji/deploy.yml

# Use verbose mode to see specific validation errors
jiji --verbose deploy
```

### "Project already exists"

**Cause:** Project directory already exists on server.

**Solution:**

```bash
# Remove existing project
jiji services remove --confirmed

# Or use different project name
```

### "Deployment lock is held"

**Cause:** Another deployment is in progress or previous deployment didn't
release lock.

**Solution:**

```bash
# Check lock status
jiji lock status

# Force release (if safe)
jiji lock release
```

### "Image not found in registry"

**Cause:** Image wasn't pushed to registry or wrong registry configured.

**Solution:**

```bash
# Verify image was built
docker images | grep myproject

# Push manually
docker push ghcr.io/username/project/service:version

# Or rebuild and push
jiji build
```

### "Container name conflict"

**Cause:** Container with same name already exists.

**Solution:**

```bash
# Remove old container
jiji server exec "docker rm -f <container-name>"

# Or remove service
jiji services remove --services <service-name>
```

## Getting Help

If you're still experiencing issues:

1. **Enable verbose logging:**
   ```bash
   jiji --verbose <command>
   ```

2. **Check audit logs:**
   ```bash
   jiji audit --status failed
   ```

3. **Search existing issues:**
   - https://github.com/acidtib/jiji/issues

4. **Ask in Discord:**
   - https://discord.gg/BMdKJzkknE

5. **Report a bug:**
   - https://github.com/acidtib/jiji/issues/new

**Include in bug reports:**

- Jiji version (`jiji version`)
- Operating system
- Error messages
- Configuration (sanitized)
- Steps to reproduce
