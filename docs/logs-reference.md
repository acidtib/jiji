# Logs Reference

Jiji provides comprehensive logging capabilities for both services and the
kamal-proxy reverse proxy. This guide covers all available logging features and
common use cases.

## Overview

Jiji offers two main logging commands:

- **`jiji services logs`** - View logs from your deployed services
- **`jiji proxy logs`** - View logs from kamal-proxy

Both commands support filtering, searching, and real-time log following across
multiple servers.

## Service Logs

The `jiji services logs` command fetches logs from containerized services
deployed with Jiji.

### Basic Usage

```bash
# View logs from a specific service
jiji services logs --services web

# View logs from multiple services
jiji services logs --services "web,api,worker"

# View logs from services matching a pattern
jiji services logs --services "web*"
```

### Options

#### `--services` (or `-S`)

Target specific services. Supports comma-separated values and wildcards.

```bash
# Single service
jiji services logs --services web

# Multiple services
jiji services logs --services "web,api"

# Wildcard pattern
jiji services logs --services "api-*"
```

**Required**: You must specify either `--services` or `--container-id`.

#### `--lines` (or `-n`)

Number of lines to show from each server. Default is 100 lines when no
`--since` or `--grep` is specified.

```bash
# Show last 50 lines
jiji services logs --services web --lines 50

# Show last 200 lines
jiji services logs --services api --lines 200
```

#### `--since` (or `-s`)

Show logs since a timestamp or relative time.

```bash
# Absolute timestamp (ISO 8601)
jiji services logs --services web --since "2023-01-01T00:00:00Z"

# Relative time
jiji services logs --services web --since "30m"  # Last 30 minutes
jiji services logs --services web --since "2h"   # Last 2 hours
jiji services logs --services web --since "1d"   # Last 1 day
```

#### `--grep` (or `-g`)

Filter log lines using grep pattern matching.

```bash
# Show only error lines
jiji services logs --services web --grep "ERROR"

# Filter for specific request IDs
jiji services logs --services api --grep "request_id=abc123"

# Case-insensitive search
jiji services logs --services web --grep "warning" --grep-options "-i"
```

#### `--grep-options`

Additional options to pass to grep.

```bash
# Case-insensitive search
jiji services logs --services web --grep "error" --grep-options "-i"

# Invert match (show lines NOT matching pattern)
jiji services logs --services web --grep "health" --grep-options "-v"

# Extended regex
jiji services logs --services web --grep "ERROR|WARN" --grep-options "-E"
```

#### `--follow` (or `-f`)

Follow logs in real-time from the primary server (or specific host set by
`--hosts`).

```bash
# Follow logs from web service
jiji services logs --services web --follow

# Follow logs from a specific host
jiji services logs --services api --follow --hosts "server1.example.com"
```

**Note**: Press `Ctrl+C`, `Ctrl+D`, or `Ctrl+\` to exit follow mode.

#### `--container-id`

Fetch logs from a specific container by ID, bypassing service configuration.

```bash
# View logs from a specific container
jiji services logs --container-id abc123def456
```

Useful for debugging containers that aren't part of your managed services.

#### `--hosts` (or `-H`)

Target specific hosts. Supports comma-separated values and wildcards.

```bash
# Specific host
jiji services logs --services web --hosts "server1.example.com"

# Multiple hosts
jiji services logs --services web --hosts "server1.example.com,server2.example.com"

# Wildcard pattern
jiji services logs --services web --hosts "server*.example.com"
```

### Common Use Cases

#### Debugging Errors

```bash
# Find all errors in the last hour
jiji services logs --services web --since "1h" --grep "ERROR"

# Find errors with context (5 lines before/after)
jiji services logs --services api --grep "ERROR" --grep-options "-C 5"
```

#### Monitoring Specific Requests

```bash
# Track a request by ID
jiji services logs --services api --grep "request_id=abc123"

# Monitor authentication attempts
jiji services logs --services auth --grep "authentication"
```

#### Checking Recent Deployments

```bash
# View logs since last deployment
jiji services logs --services web --since "10m"

# Follow logs during deployment
jiji services logs --services web --follow
```

#### Comparing Logs Across Servers

```bash
# View logs from all servers
jiji services logs --services web

# View logs from specific server
jiji services logs --services web --hosts "server1.example.com"
```

## Proxy Logs

The `jiji proxy logs` command fetches logs from the kamal-proxy container,
which handles HTTP/HTTPS routing to your services.

### Basic Usage

```bash
# View proxy logs
jiji proxy logs

# View last 50 lines
jiji proxy logs --lines 50

# Follow proxy logs in real-time
jiji proxy logs --follow
```

### Options

The proxy logs command supports the same filtering and searching options as
service logs:

- `--lines` (or `-n`) - Number of lines to show
- `--since` (or `-s`) - Show logs since timestamp or relative time
- `--grep` (or `-g`) - Filter lines with grep
- `--follow` (or `-f`) - Follow logs in real-time
- `--hosts` (or `-H`) - Target specific hosts

### Common Use Cases

#### Debugging Routing Issues

```bash
# Find 404 errors
jiji proxy logs --grep "404"

# Find 500 errors
jiji proxy logs --grep "500"

# Check all HTTP error codes
jiji proxy logs --grep "HTTP/[0-9]\\.[0-9]\" [45][0-9][0-9]" --grep-options "-E"
```

#### Monitoring Traffic

```bash
# Follow all incoming requests
jiji proxy logs --follow

# Monitor requests to a specific domain
jiji proxy logs --grep "myapp.example.com"

# Track requests to a specific service
jiji proxy logs --grep "web-app"
```

#### SSL/TLS Debugging

```bash
# Check SSL certificate issues
jiji proxy logs --grep "SSL\|TLS\|certificate" --grep-options "-E"

# Monitor HTTPS traffic
jiji proxy logs --grep "https"
```

#### Performance Analysis

```bash
# Find slow requests (adjust timing as needed)
jiji proxy logs --grep "upstream_response_time"

# Monitor proxy health
jiji proxy logs --grep "health_check"
```

## Log Format

All logs include timestamps from Docker/Podman in ISO 8601 format:

```
2023-12-22T15:30:45.123456789Z [INFO] Application started
2023-12-22T15:30:46.234567890Z [ERROR] Connection failed
```

## Best Practices

### 1. Use Relative Time for Recent Logs

```bash
# Instead of calculating exact timestamps
jiji services logs --services web --since "30m"

# Not this
jiji services logs --services web --since "2023-12-22T14:00:00Z"
```

### 2. Combine Filters for Precision

```bash
# Find errors in the last hour from a specific service
jiji services logs --services api --since "1h" --grep "ERROR"
```

### 3. Use Follow Mode During Debugging

```bash
# Watch logs in real-time while reproducing an issue
jiji services logs --services web --follow
```

### 4. Check Proxy Logs for Routing Issues

If a service isn't receiving traffic, check proxy logs first:

```bash
# See if proxy is routing to your service
jiji proxy logs --grep "your-service-name"
```

### 5. Limit Lines for Large Log Volumes

```bash
# Get just the most recent entries
jiji services logs --services web --lines 20
```

## Troubleshooting

### Container Not Found

If you see "Container not found", verify:

1. The service is deployed: `jiji deploy --services web`
2. The container is running on the target host
3. You're targeting the correct hosts with `--hosts`

### No Logs Displayed

If no logs appear:

1. Check if the container is producing logs
2. Verify the time range with `--since`
3. Check if `--grep` is filtering out all lines
4. Ensure the service has been deployed and started

### Follow Mode Not Working

If follow mode doesn't work:

1. Verify SSH connection to the target host
2. Check that the container is running
3. Ensure you have proper permissions

### Permission Denied

If you get permission errors:

1. Verify your SSH user has Docker/Podman access
2. Check if the user is in the `docker` group (Docker) or has Podman socket
   access
3. Try running with elevated permissions if needed

## Examples

### Complete Debugging Workflow

```bash
# 1. Check recent deployment logs
jiji services logs --services web --since "5m"

# 2. Look for errors
jiji services logs --services web --grep "ERROR"

# 3. Check proxy routing
jiji proxy logs --grep "web"

# 4. Follow logs in real-time
jiji services logs --services web --follow
```

### Multi-Service Debugging

```bash
# Check all api services for errors
jiji services logs --services "api*" --grep "ERROR"

# Compare logs across different servers
jiji services logs --services web --hosts "server1.example.com"
jiji services logs --services web --hosts "server2.example.com"
```

### Production Monitoring

```bash
# Monitor critical errors
jiji services logs --services "web,api,worker" --grep "CRITICAL\|FATAL" --grep-options "-E" --follow

# Track specific feature usage
jiji services logs --services api --grep "feature_flag=new_checkout" --since "1h"
```

## See Also

- [Main README](../README.md) - General Jiji usage
- [Network Reference](network-reference.md) - Private networking and service
  discovery
- [Configuration Reference](../src/jiji.yml) - Service configuration options
