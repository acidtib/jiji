# Jiji Private Network Reference

Quick reference for Jiji's private networking features.

## Overview

Jiji creates a secure mesh network between servers using:

- **WireGuard** - Encrypted VPN tunnels between servers
- **Corrosion** - Distributed CRDT database for service discovery
- **jiji-dns** - DNS server with real-time Corrosion subscriptions

## Network Commands

```bash
# View network topology and status
jiji network status

# Show DNS records for all services
jiji network dns

# Inspect a specific container
jiji network inspect --container <id>

# Garbage collect stale records
jiji network gc              # Dry run (default)
jiji network gc --force      # Actually delete

# Database operations
jiji network db stats        # Show database statistics
jiji network db query "SQL"  # Execute raw SQL query

# Tear down network infrastructure
jiji network teardown
```

## IP Allocation

```
Cluster CIDR: 10.210.0.0/16 (configurable)
├── Server 0: 10.210.0.0/24
│   ├── WireGuard IP: 10.210.0.1
│   └── Containers: 10.210.0.2 - 10.210.0.254
├── Server 1: 10.210.1.0/24
│   ├── WireGuard IP: 10.210.1.1
│   └── Containers: 10.210.1.2 - 10.210.1.254
└── Server N: 10.210.N.0/24 (up to 255 servers)
```

## DNS Resolution

### Service Names

Format: `<project>-<service>.<domain>`

| Pattern                               | Example                  | Description                        |
| ------------------------------------- | ------------------------ | ---------------------------------- |
| `{project}-{service}.jiji`            | `myapp-api.jiji`         | All healthy containers for service |
| `{project}-{service}-{instance}.jiji` | `myapp-api-primary.jiji` | Specific instance                  |

### Resolution Flow

```
Container queries "myapp-api.jiji"
  -> jiji-dns (port 53 on WireGuard IP)
  -> Corrosion subscription (real-time container data)
  -> Returns healthy container IPs
  -> Client-side load balancing
```

Non-`.jiji` queries are forwarded to system resolvers.

## Service Files

| Component          | Location                          |
| ------------------ | --------------------------------- |
| WireGuard config   | `/etc/wireguard/jiji0.conf`       |
| WireGuard service  | `wg-quick@jiji0.service`          |
| Corrosion config   | `/opt/jiji/corrosion/config.toml` |
| Corrosion database | `/opt/jiji/corrosion/state.db`    |
| Corrosion service  | `jiji-corrosion.service`          |
| jiji-dns binary    | `/opt/jiji/dns/jiji-dns`          |
| jiji-dns service   | `jiji-dns.service`                |

### Daemon DNS Configuration

Docker: `/etc/docker/daemon.json`

```json
{
  "dns": ["10.210.0.1"],
  "dns-search": ["jiji"]
}
```

Podman: `/etc/containers/containers.conf`

```ini
[network]
dns_servers = ["10.210.0.1"]
dns_searches = ["jiji"]
```

## Verification Commands

```bash
# WireGuard status
sudo wg show jiji0

# Check peer connectivity
sudo wg show jiji0 | grep "latest handshake"

# Test DNS resolution
dig @10.210.0.1 myapp-api.jiji

# View jiji-dns logs
journalctl -u jiji-dns -f

# View Corrosion logs
journalctl -u jiji-corrosion -f

# Check container DNS config
docker exec <container> cat /etc/resolv.conf

# Test cross-server connectivity
docker exec <container> ping 10.210.1.10
```

## Firewall Rules

Required ports:

| Port  | Protocol | Purpose          |
| ----- | -------- | ---------------- |
| 31820 | UDP      | WireGuard tunnel |
| 31280 | TCP      | Corrosion gossip |

```bash
# UFW examples
ufw allow 31820/udp
ufw allow 31280/tcp
```

## Troubleshooting

| Issue                      | Solution                                |
| -------------------------- | --------------------------------------- |
| No peer handshakes         | Check firewall allows UDP 31820         |
| DNS not resolving          | Check `systemctl status jiji-dns`       |
| Service names not working  | Check daemon DNS config                 |
| Stale DNS records          | Run `jiji network gc --force`           |
| Container can't ping peers | Verify routes: `ip route \| grep jiji0` |

## Configuration

In `.jiji/deploy.yml`:

```yaml
network:
  enabled: true
  cluster_cidr: "10.210.0.0/16" # Optional, this is default
```

**Note:** The service domain is always `jiji` and cannot be changed. All
services are accessible via `{project}-{service}.jiji` DNS names.

## Limits

- Max servers: 255 (with /16 cluster CIDR)
- Max containers per server: 253
- DNS TTL: 60 seconds (configurable in jiji-dns)
