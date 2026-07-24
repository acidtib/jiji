# Jiji Private Network Reference

Jiji creates a coordinator-free private network between configured servers.
WireGuard carries encrypted host-to-host traffic, each host owns a routed
container subnet, dnsmasq serves compiled `.jiji` records, and nftables maps
stable service VIPs to active container backends.

Corrosion and the former reconciliation daemon are not part of this
architecture.

## Commands

```bash
# Print the deterministic plan without changing hosts
jiji network plan

# Install, update, or repair the complete network
jiji network setup

# The full server setup includes the same network workflow
jiji server setup
```

`--hosts` limits which hosts are changed, but planning always uses the complete
configured topology.

## Default address ranges

```yaml
network:
  enabled: true
  management_cidr: 198.18.0.0/16
  container_cidr: 100.64.0.0/10
  host_prefix: 21
```

Management addresses and per-host container subnets are deterministic. Each
service endpoint receives a stable VIP plus backend addresses A and B.
Containers use the inactive backend during replacement. After health checks
pass, jiji atomically moves the VIP to that backend.

## DNS

The aggregate name `{project}-{service}.jiji` resolves to the stable VIPs for
all configured replicas. The name `{project}-{service}-{server}.jiji` resolves
only to that server replica's stable VIP. Both forms are compiled from
`deploy.yml`. DNS contains desired topology, not runtime health state.

Containers are started with the local jiji resolver, the `jiji` search domain,
and `ndots:1`. Queries outside `.jiji` are forwarded through the host resolver.

## Installed files

| Component | Location |
| --- | --- |
| Selected network generation | `/etc/jiji/network/current` |
| Retained network generations | `/etc/jiji/network/generations/` |
| WireGuard config | `/etc/wireguard/jiji0.conf` |
| Selected DNS generation | `/etc/jiji/network/dns-current` |
| Retained DNS generations | `/etc/jiji/network/dns-generations/` |
| Selected service VIP mapping | `/etc/jiji/network/service-nat-current` |
| WireGuard service | `wg-quick@jiji0.service` |
| Bridge restoration service | `jiji-network-restore.service` |
| VIP restoration service | `jiji-service-nat.service` |
| Static DNS service | `jiji-dns.service` |

## Required firewall access

Allow UDP port `51820` between the configured server public IP addresses.
Container subnets are routed only through WireGuard. Corrosion gossip and API
ports are not used.

## Verification

```bash
sudo wg show jiji0
ip route show dev jiji0
systemctl status jiji-network-restore jiji-service-nat jiji-dns
readlink -f /etc/jiji/network/current
readlink -f /etc/jiji/network/dns-current
docker exec <container> getent hosts <project>-<service>.jiji
```

Use `podman exec` instead of `docker exec` when Podman is selected.

## Reboot behavior

WireGuard and the jiji systemd units restore the selected local generations.
Containers restart with their assigned backend addresses according to their
engine restart policy. No coordination service, SSH connection, jiji command,
or redeployment is required when topology has not changed.

Podman keeps its bridge only while a container is attached. Jiji installs a
minimal local `jiji-network-anchor` container at reserved address `.3` and
orders `podman-restart.service` after bridge restoration. The anchor uses a
local static BusyBox rootfs and does not pull an image.

## Repair

Run `jiji network setup`. It stages and validates a complete generation on
every selected host before activation. If activation or verification fails,
all attempted hosts return to their previous retained generations.

`jiji deploy` checks every configured host before making deployment changes.
When it detects a stale generation, it automatically applies the complete
network plan to the full cluster using the same transactional setup path.
The explicit command remains available for network-only maintenance.
