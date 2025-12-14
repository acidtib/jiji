# Deployment Locks

## Overview

The lock system provides exclusive access control for deployment operations by creating locks across all target hosts. This prevents multiple deployments from running simultaneously and causing conflicts or inconsistent states.

## Key Features

1. **Exclusive Access**: Only one deployment can hold the lock at a time
2. **Multi-Host**: Locks are acquired across ALL target hosts
3. **Atomic Operation**: Either all hosts are locked or none are
4. **Process Tracking**: Locks track which process/user created them
5. **Automatic Cleanup**: Failed operations clean up partial locks

## Basic Usage

```bash
# Show available lock commands
jiji lock

# Check current lock status
jiji lock status

# Show detailed lock information
jiji lock show

# Acquire a deployment lock
jiji lock acquire "Deploying feature X"

# Release the deployment lock
jiji lock release
```

## Advanced Lock Management

```bash
# Acquire lock with timeout (in seconds)
jiji lock acquire "Emergency hotfix" --timeout 600

# Force acquire (use with extreme caution)
jiji lock acquire "Override existing lock" --force

# Target specific host for status check
jiji lock status --host production-1

# JSON output for scripting
jiji lock status --json

# Check lock status across all hosts
jiji lock status --all-hosts
```

## How Locks Work

### Lock Acquisition Process

1. **Validation**: Check if lock is already held
2. **Multi-Host Check**: Verify lock status on all target hosts
3. **Atomic Lock**: Acquire lock on all hosts simultaneously
4. **Verification**: Confirm all locks were acquired successfully
5. **Cleanup on Failure**: Remove partial locks if acquisition fails

### Lock Release Process

1. **Ownership Verification**: Confirm the requesting process owns the lock
2. **Multi-Host Release**: Release locks on all target hosts
3. **Cleanup**: Remove lock files and metadata
4. **Audit**: Log the lock release operation

## Lock File Structure

Locks are stored as `.jiji/deploy.lock` files on each host containing:

```json
{
  "locked": true,
  "message": "Deploying feature X",
  "acquiredAt": "2024-01-15T10:30:45.123Z",
  "acquiredBy": "alice",
  "host": "production-1",
  "pid": 12345,
  "version": "1.0",
  "timeout": 3600,
  "targets": [
    "web-1.example.com",
    "web-2.example.com"
  ]
}
```

### Lock File Fields

- **locked**: Boolean indicating lock status
- **message**: Human-readable description of the operation
- **acquiredAt**: ISO 8601 timestamp when lock was acquired
- **acquiredBy**: Username who acquired the lock
- **host**: Hostname where lock was created
- **pid**: Process ID of the locking process
- **version**: Lock format version
- **timeout**: Lock timeout in seconds (optional)
- **targets**: List of all hosts that should be locked

## Integration with Deployments

### Automatic Lock Management

When you run deployment commands, Jiji automatically manages locks:

1. **Pre-deployment**: 
   - Attempts to acquire deployment lock
   - Fails fast if lock cannot be acquired
   - Provides clear error message about who holds the lock

2. **During deployment**: 
   - Maintains lock throughout operation
   - Periodically refreshes lock to prevent timeout
   - Handles lock renewal on long-running operations

3. **Post-deployment**: 
   - Releases lock when deployment completes successfully
   - Logs lock release in audit trail

4. **On failure**: 
   - Cleans up lock even if deployment fails
   - Ensures no stale locks are left behind
   - Logs failure and cleanup actions

### Manual Lock Management

For complex operations or maintenance windows:

```bash
# Acquire lock before manual operations
jiji lock acquire "Database maintenance - Alice"

# Perform your operations
jiji server exec "systemctl stop myapp"
# ... run migration scripts ...
# ... perform manual fixes ...
jiji server exec "systemctl start myapp"

# Release when done
jiji lock release

# Verify release in audit log
jiji audit --filter lock -n 5
```

## Configuration

### SSH Requirements

Lock system requires SSH access to all target hosts:

```yaml
# .jiji/deploy.yml
ssh:
  user: deploy
  port: 22
  key_path: ~/.ssh/deploy_key

services:
  myapp:
    hosts:
      - web-1.example.com
      - web-2.example.com
```

### Lock Storage Location

Lock files are created in:
- **Remote hosts**: `.jiji/deploy.lock` on each target host
- **Local tracking**: `.jiji/local_locks.json` for local lock state

### Timeout Configuration

```yaml
# .jiji/deploy.yml
locks:
  default_timeout: 3600  # 1 hour in seconds
  max_timeout: 7200      # 2 hours maximum
  cleanup_interval: 300   # Check for stale locks every 5 minutes
```

## Safety Features

### Stale Lock Detection

Jiji automatically detects and handles stale locks:

- **Process Check**: Verifies if the locking process still exists
- **Timeout Check**: Respects configured timeout values
- **Host Connectivity**: Handles locks from unreachable processes
- **Safe Cleanup**: Only removes confirmed stale locks

### Partial Lock Protection

If lock acquisition fails partway through:

- **Immediate Cleanup**: Removes any successfully acquired locks
- **Clear Error Messages**: Explains which hosts failed and why
- **No Partial State**: Ensures either all hosts are locked or none are

### Ownership Verification

- **User Matching**: Locks can only be released by the acquiring user
- **Process Matching**: Additional verification using process ID
- **Host Verification**: Confirms lock operations from correct host

## Best Practices

### Lock Messages

Always provide clear, descriptive messages:

```bash
# Good examples
jiji lock acquire "Deploying v2.1.0 - critical security fix"
jiji lock acquire "Weekly maintenance - database migration"
jiji lock acquire "Hotfix deployment - fixing payment processor"

# Avoid vague messages
jiji lock acquire "deploying"  # Too vague
jiji lock acquire "stuff"      # Not descriptive
```

### Team Coordination

1. **Communication**: Notify team members before acquiring locks
2. **Descriptive Messages**: Include your name and purpose
3. **Time Limits**: Don't hold locks longer than necessary
4. **Documentation**: Document lock usage in team procedures

### Emergency Procedures

Document clear procedures for lock emergencies:

```bash
# Emergency override process
# 1. Verify the lock is actually stale
jiji lock show

# 2. Attempt to contact the lock holder
# 3. If confirmed safe, force release
jiji lock acquire "Emergency override - Alice authorized by Bob" --force

# 4. Document the override in team channels
# 5. Follow up with post-incident review
```

## Troubleshooting

### Common Lock Issues

#### Lock Already Held

```bash
# Check who has the lock
jiji lock show

# See detailed lock information
jiji lock status --json | jq '.lock_holder'

# Wait for lock release or coordinate with holder
```

#### Stale Lock Detection

```bash
# Check if process is still running
jiji lock show  # Shows PID and user

# Verify on the locking host
ssh user@host "ps aux | grep <PID>"

# If confirmed stale, use force flag carefully
jiji lock acquire "Cleaning up stale lock" --force
```

#### Partial Lock Failures

```bash
# Check which hosts have locks
jiji lock status --all-hosts

# Clean up manually if needed
jiji server exec --host failed-host "rm -f .jiji/deploy.lock"

# Re-attempt lock acquisition
jiji lock acquire "Retry after cleanup"
```

### Network Issues

```bash
# Test connectivity to all hosts
jiji server exec "echo 'connectivity test'" --all-hosts

# Check SSH key authentication
ssh -o ConnectTimeout=5 deploy@host "whoami"

# Verify lock directory permissions
jiji server exec "ls -la .jiji/"
```

## Examples

### Standard Deployment Flow

```bash
# Check current lock status
jiji lock status

# Deploy with automatic locking
jiji deploy myapp  # Locks automatically acquired and released

# Verify completion
jiji audit --filter lock -n 3
```

### Maintenance Window

```bash
# Start maintenance window
jiji lock acquire "Scheduled maintenance - 2024-01-15 - Alice"

# Perform maintenance tasks
jiji server exec "apt update && apt upgrade -y"
jiji deploy myapp --maintenance-mode
# ... additional maintenance tasks ...

# End maintenance window
jiji lock release

# Verify in logs
jiji audit --filter lock --since "1 hour ago"
```

### Emergency Deployment

```bash
# Check current status
jiji lock status

# If locked, evaluate urgency
jiji lock show

# For true emergencies only
jiji lock acquire "EMERGENCY: Critical security patch - Alice (approved by Bob)" --force

# Deploy emergency fix
jiji deploy myapp

# Lock automatically released after deployment
```

### Long-Running Operations

```bash
# For operations that might take hours
jiji lock acquire "Database migration - expected 3 hours" --timeout 14400

# Start long-running process
./long-migration-script.sh

# Release when complete
jiji lock release
```
