# Jiji Private Network - Quick Reference

## Network Components

### WireGuard Mesh Network

Interface: `jiji0` Port: `51820` Protocol: UDP Encryption: ChaCha20-Poly1305 Key
Exchange: Curve25519

### IP Allocation

```
Cluster CIDR: 10.210.0.0/16 (IPv4 - configurable)
├── Server 0: 10.210.0.0/24
│   ├── WireGuard IP: 10.210.0.1
│   ├── Management IP: fdcc:xxxx:xxxx:xxxx:xxxx:xxxx:xxxx:xxxx (IPv6, derived from pubkey)
│   └── Containers: 10.210.0.2 - 10.210.0.254
├── Server 1: 10.210.1.0/24
│   ├── WireGuard IP: 10.210.1.1
│   ├── Management IP: fdcc:xxxx:xxxx:xxxx:xxxx:xxxx:xxxx:xxxx (IPv6, derived from pubkey)
│   └── Containers: 10.210.1.2 - 10.210.1.254
└── Server N: 10.210.N.0/24 (up to 255 servers with /16 cluster CIDR)
```

**Note**: The network uses IPv4 for WireGuard tunnels and container networking.
IPv6 addresses (fdcc::/16 prefix) are used only for Corrosion gossip protocol
management communication.

## Commands

### Setup Network

```bash
# During initial server initialization
jiji server init -H server1.example.com,server2.example.com

# Network is automatically configured in phases:
# 1. Install dependencies (WireGuard, Corrosion, CoreDNS)
# 2. Generate keys and allocate IPs
# 3. Configure WireGuard mesh
# 4. Setup Corrosion (if enabled)
# 5. Create Docker networks with subnets
# 6. Configure routing
# 7. Setup DNS
# 8. Enable peer monitoring
```

### Verify Network

```bash
# WireGuard status
sudo wg show jiji0

# Routing table
ip route show | grep jiji0

# iptables rules
sudo iptables -L FORWARD -n -v | grep jiji0

# Docker network
docker network inspect jiji

# Peer monitoring
sudo systemctl status jiji-peer-monitor.service

# DNS
dig @10.210.0.1 <service-name>.jiji
```

### Debug Container Networking

```bash
# Check container IP
docker inspect <container> | grep IPAddress

# Test connectivity from container
docker exec <container> ping 10.210.1.10
docker exec <container> ping api.jiji

# View container DNS config
docker exec <container> cat /etc/resolv.conf

# Trace route
docker exec <container> traceroute 10.210.1.10
```

### Monitor Peers

```bash
# View monitoring logs
sudo journalctl -u jiji-peer-monitor.service -f

# Manual peer check
sudo wg show jiji0 dump

# Check last handshake times
sudo wg show jiji0 | grep "latest handshake"

# View peer endpoints
sudo wg show jiji0 endpoints
```

## Service Files

### Network State

File: `.jiji/network.json` Contents: Server topology, IPs, WireGuard keys
(public only)

### WireGuard Config

File: `/etc/wireguard/jiji0.conf` Service: `wg-quick@jiji0.service`

### Corrosion (Discovery)

Config: `/opt/jiji/corrosion/config.toml` Database:
`/opt/jiji/corrosion/state.db` Service: `jiji-corrosion.service`

### CoreDNS

Config: `/opt/jiji/coredns/Corefile` Hosts: `/opt/jiji/coredns/hosts` Service:
`jiji-coredns.service`

### Daemon DNS Configuration

Docker: `/etc/docker/daemon.json` Podman: `/etc/containers/containers.conf`
Search domain: `jiji` (configurable)

### Peer Monitor

Script: `/opt/jiji/bin/monitor-wireguard-peers.sh` Service:
`jiji-peer-monitor.service`

## Firewall Rules

### iptables Forward Rules

```bash
# Docker to WireGuard
iptables -A FORWARD -i docker0 -o jiji0 -j ACCEPT

# WireGuard to Docker
iptables -A FORWARD -i jiji0 -o docker0 -j ACCEPT

# Established connections
iptables -A FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
```

### NAT Rules

```bash
# Container traffic through WireGuard
iptables -t nat -A POSTROUTING -s 10.210.0.0/24 -o jiji0 -j MASQUERADE
```

### Host Firewall

```bash
# Allow WireGuard (IPv4 tunnel traffic)
ufw allow 51820/udp

# Allow Corrosion gossip (uses IPv6 management addresses)
ufw allow 8787/tcp
```

## DNS Resolution

### Service Names

Format: `<service-name>.<domain>` Domain: `jiji` (configurable in jiji.yml)
Example: `api.jiji`, `postgres.jiji`

### Resolution Flow

```
Container DNS query for "api.jiji"
  ↓
Container daemon forwards to CoreDNS (10.210.0.1:53)
  ↓
CoreDNS queries Corrosion for service "api"
  ↓
Returns container IPs across cluster
  ↓
Container connects to 10.210.1.15
```

**Note**: DNS configuration is applied at both daemon and container levels:

- **Daemon level**: All new containers inherit DNS settings automatically
- **Container level**: Existing containers get DNS servers passed explicitly

## Performance

### Latency

WireGuard overhead: ~0.1-0.5ms Typical ping times: 1-10ms (LAN), 20-100ms (WAN)

### Throughput

WireGuard: Near line-rate (10Gbps+) Container networking: Limited by Docker
bridge (~5-9Gbps)

### Resource Usage

WireGuard: Minimal (~1-2% CPU) Corrosion: ~50MB RAM, <1% CPU CoreDNS: ~20MB RAM,
<1% CPU Peer Monitor: Negligible

## Security

### Encryption

All inter-machine traffic encrypted via WireGuard No plaintext container traffic
on network

### Key Management

Private keys never leave servers Public keys stored in `.jiji/network.json` Keys
generated on each server

### Network Isolation

Containers isolated in WireGuard network Not directly accessible from internet
Expose via proxy/load balancer only

## Troubleshooting Quick Tips

| Issue                         | Quick Fix                                                     |
| ----------------------------- | ------------------------------------------------------------- |
| No peer handshakes            | Check firewall allows UDP 51820                               |
| Containers can't ping peers   | Verify routes: `ip route \| grep jiji0`                       |
| DNS not resolving             | Check CoreDNS: `systemctl status jiji-coredns`                |
| Service names not resolving   | Check daemon DNS config: `cat /etc/docker/daemon.json`        |
| Endpoint rotation not working | Check peer monitor logs                                       |
| Slow cross-machine traffic    | Check WireGuard MTU settings                                  |
| Container wrong subnet        | Recreate network: `docker network rm jiji` then re-initialize |

## Environment Variables

Set these before initialization if needed:

```bash
# Custom cluster CIDR (IPv4 only, must be /24 or larger)
export JIJI_CLUSTER_CIDR="10.220.0.0/16"

# Disable networking
export JIJI_NETWORK_ENABLED="false"

# Use static discovery instead of Corrosion
export JIJI_NETWORK_DISCOVERY="static"
```

## Example Deployments

### Simple Multi-Server App

```yaml
# docker-compose.yml
services:
  api:
    image: myapp/api:latest
    networks:
      - jiji
    deploy:
      replicas: 3

  postgres:
    image: postgres:15
    networks:
      - jiji
    volumes:
      - pgdata:/var/lib/postgresql/data

networks:
  jiji:
    external: true

volumes:
  pgdata:
```

Deploy:

```bash
jiji deploy -f docker-compose.yml
```

Access from any container:

```bash
# API containers can connect to postgres
psql -h postgres.jiji -U myuser
```

### Service with Load Balancing

```yaml
services:
  web:
    image: nginx:latest
    networks:
      - jiji
    deploy:
      replicas: 5 # Distributed across servers
```

DNS automatically returns all 5 IPs for `web.jiji`, providing client-side load
balancing.

## Network Limits

Max servers: 256 (with /24 subnets in /16 cluster CIDR) Max containers per
server: 253 (with /24 subnet, .1 reserved for WireGuard interface) Max total
containers: ~64,768 (256 servers × 253 containers) WireGuard peers: Unlimited
(mesh scales to hundreds)

**CIDR Requirements**: Cluster CIDR must be /24 or larger (smaller prefix
numbers) to support multiple servers. Each server receives a /24 subnet
allocation.

## Best Practices

1. **Use consistent CIDR** - Don't change cluster_cidr after setup (default:
   10.210.0.0/16)
2. **Monitor peer health** - Check monitoring logs regularly
3. **Keep WireGuard updated** - Security patches are important
4. **Use DNS names** - Avoid hardcoding container IPs (use service.jiji format)
5. **Plan subnet allocation** - Each server gets consecutive /24 subnet (0, 1,
   2, etc.)
6. **Test failover** - Regularly test endpoint rotation and peer connectivity
7. **Document endpoints** - Keep track of server public IPs for WireGuard peers
8. **Backup network.json** - Critical for cluster state and topology
9. **IPv4 only** - Don't configure IPv6 on WireGuard interface (used internally
   for management)
