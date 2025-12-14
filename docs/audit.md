# Audit System

## Overview

The audit system tracks all operations performed by Jiji, storing logs both locally and on remote servers. This provides transparency, accountability, and debugging capabilities for your deployment infrastructure.

## Basic Usage

```bash
# View recent audit entries
jiji audit

# View more entries
jiji audit -n 50

# Filter by action type
jiji audit --filter deploy

# Filter by status
jiji audit --status success

# View entries from specific host
jiji audit --host production-1

# Show raw log format
jiji audit --raw

# Display as formatted table
jiji audit --table

# Output as JSON
jiji audit --json
```

## Advanced Filtering

```bash
# Filter by date range
jiji audit --since 2024-01-01 --until 2024-01-31

# Follow logs in real-time
jiji audit --follow

# Combine logs from all servers chronologically
jiji audit --aggregate
```

## Audit Entry Format

Each audit entry contains:

- **Timestamp**: When the action occurred
- **Status**: `STARTED`, `SUCCESS`, `FAILED`, or `WARNING`
- **Action**: What operation was performed
- **Host**: Which server (for remote operations)
- **Message**: Human-readable description
- **Details**: Structured data about the operation

Example entry:

```
[2024-01-15T10:30:45.123Z] [SUCCESS ] SERVICE DEPLOY [web-1] - Service myapp deployment success
    service=myapp, version=v1.2.3, hosts=2
```

## What Gets Audited

The audit system tracks the following operations:

- **Deployments**: Service deployments, rollbacks, restarts
- **Lock Operations**: Lock acquisition and release
- **Server Bootstrap**: Initial server setup
- **Engine Installation**: Docker/Podman installation
- **Configuration Changes**: Config file updates
- **Custom Commands**: Manual command execution
- **Container Operations**: Start, stop, remove containers
- **Proxy Operations**: Load balancer changes

## Configuration

### SSH Configuration

The audit system requires SSH access to remote hosts for log collection:

```yaml
# .jiji/deploy.yml
ssh:
  user: deploy
  port: 22

services:
  myapp:
    hosts:
      - web-1.example.com
      - web-2.example.com
```

### Log Storage

Audit logs are automatically created in:

- **Local**: `.jiji/audit.txt`
- **Remote**: `.jiji/audit.txt` on each host

## Best Practices

### Regular Review
Check audit logs regularly for unusual activity or patterns that might indicate issues.

### Retention Policy
Archive old audit logs for compliance and historical analysis. Consider implementing log rotation for long-running systems.

### Filtering for Focus
Use specific filters to focus on relevant information:
- Filter by status to identify failures
- Filter by action type during troubleshooting
- Use date ranges for incident investigation

### Integration with Monitoring
Parse JSON output in monitoring and alerting systems:

```bash
jiji audit --json --status failed | jq '.[] | select(.action == "deploy")'
```

## Troubleshooting

### Viewing Local Audit Files

```bash
# Check local audit file directly
cat .jiji/audit.txt

# Get last 20 entries with timestamps
tail -n 20 .jiji/audit.txt
```

### Remote Audit Access

```bash
# Test SSH connectivity first
jiji server exec "whoami"

# View remote audit logs
jiji server exec "cat .jiji/audit.txt | tail -n 20"

# Check if remote audit file exists
jiji server exec "ls -la .jiji/audit.txt"
```

### Common Issues

#### Missing Audit Entries
- Verify SSH connectivity to target hosts
- Check file permissions on `.jiji/audit.txt`
- Ensure the `.jiji` directory exists on remote hosts

#### Performance with Large Logs
- Use date filtering to reduce data volume
- Consider implementing log rotation
- Use `--raw` format for faster parsing

#### Timestamp Issues
- Ensure server clocks are synchronized
- Use UTC timestamps for consistency across hosts

## Examples

### Daily Operations Review

```bash
# Review today's deployments
jiji audit --filter deploy --since $(date +%Y-%m-%d)

# Check for any failures in the last 24 hours
jiji audit --status failed --since "24 hours ago"

# Monitor real-time activity
jiji audit --follow
```

### Incident Investigation

```bash
# Find all actions around a specific time
jiji audit --since "2024-01-15T10:00:00Z" --until "2024-01-15T11:00:00Z"

# Get detailed information about failures
jiji audit --status failed --json | jq '.[] | {timestamp, action, host, message, details}'

# Track a specific service's deployment history
jiji audit --filter deploy | grep "myapp"
```

### Compliance Reporting

```bash
# Generate monthly deployment report
jiji audit --filter deploy --since 2024-01-01 --until 2024-01-31 --json > deployments_january.json

# Export all audit data for archival
jiji audit --json --since 2024-01-01 > audit_archive_2024.json
```

## Integration with Jiji Operations

The audit system automatically logs all Jiji operations without requiring manual intervention. Every command that modifies state or performs remote operations will generate audit entries.

### Automatic Logging

When you run any Jiji command, the system automatically:

1. **Pre-operation**: Logs the start of the operation
2. **During operation**: Logs significant steps and milestones
3. **Post-operation**: Logs completion status and results
4. **On failure**: Logs error details and cleanup actions

### Manual Audit Entries

For custom operations or manual interventions, you can add audit entries:

```bash
# Log custom operations (if supported by Jiji)
jiji audit log "Manual database backup completed"
```
