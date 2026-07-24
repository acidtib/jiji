# Jiji Private Network Reference

Jiji creates a coordinator-free private network between configured servers.
WireGuard carries encrypted host-to-host traffic, each host owns a routed
container subnet, dnsmasq serves compiled `.jiji` records, and nftables maps
stable service VIPs to active container backends.

Corrosion and the former reconciliation daemon are not part of this
architecture.

## Multiple projects on one server

Every piece of this network layer is **per-project isolated**, computed
purely from `config.project` -- there is no shared registry and no
coordination between projects. Two independent projects (different
`project:` names, different `deploy.yml` files) can run `jiji server setup`
against the same physical server, and each gets its own:

- WireGuard interface, port, and keypair
- container-engine bridge network and subnet
- dnsmasq resolver process
- compiled state tree under `/etc/jiji/network/{slug}/`
- systemd units

`{slug}` is a short, deterministic, hash-suffixed identifier derived from
`project:` (see `jiji network plan`'s output for the exact value for your
project). The one intentionally shared, multi-tenant component is
kamal-proxy: one container per host, attached to every project's bridge that
has active routes on that host, with routes namespaced per project.

Because identity is purely name-derived with no registry, **`project:` names
must be unique per host**, not just per config repository -- two projects
that happen to share a literal project name are indistinguishable to jiji.
There is also a small, real (not theoretical) chance of a hash collision
between two *different* project names that both rely on the default
`network.container_cidr`/`management_cidr` ranges, since both draw their
subnets from the same shared address space. If you expect several projects
on one host, set distinct `network.container_cidr`/`management_cidr` values
per project in `deploy.yml` -- distinct ranges make a collision impossible,
not just less likely. `jiji server setup`/`jiji network setup` prints an
advisory when a project is relying on the shared defaults, and rejects with
an actionable error (naming the colliding project's resource) if a real
collision is ever detected on a host.

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

These ranges are shared *by default* across every project that doesn't
override them -- see "Multiple projects on one server" above for why setting
distinct ranges matters once more than a handful of projects share a host.

Management addresses and per-host container subnets are deterministic. Each
service endpoint receives a stable VIP plus backend addresses A and B.
Containers use the inactive backend during replacement. After health checks
pass, jiji atomically moves the VIP to that backend.

## DNS

The aggregate name `{project}-{service}.jiji` resolves to the stable VIPs for
all configured replicas. The name `{project}-{service}-{server}.jiji` resolves
only to that server replica's stable VIP. Both forms are compiled from
`deploy.yml`. DNS contains desired topology, not runtime health state.

Containers are started with their own project's jiji resolver, the `jiji`
search domain, and `ndots:1`. Queries outside `.jiji` are forwarded through
the host resolver. kamal-proxy is the one exception: it's given no `--dns` at
all, since its routing targets are raw backend IP addresses (never a `.jiji`
hostname), and a single resolver can't reliably answer for every project's
`.jiji` records it might be attached to at once (see `jiji-cli/src/proxy.rs`
for the full reasoning).

## Installed files

Every path below is rooted under `/etc/jiji/network/{slug}/` -- one
independent tree per project, where `{slug}` is that project's derived
identifier (`jiji network plan` prints it). Interface/bridge/unit names
below are similarly derived per project, not fixed literals.

| Component | Location |
| --- | --- |
| Selected network generation | `/etc/jiji/network/{slug}/current` |
| Retained network generations | `/etc/jiji/network/{slug}/generations/` |
| WireGuard config | `/etc/wireguard/{wireguard_interface}.conf` |
| Selected DNS generation | `/etc/jiji/network/{slug}/dns-current` |
| Retained DNS generations | `/etc/jiji/network/{slug}/dns-generations/` |
| Selected service VIP mapping | `/etc/jiji/network/{slug}/service-nat-current` |
| WireGuard service | `wg-quick@{wireguard_interface}.service` |
| Bridge restoration service | `jiji-network-restore-{slug}.service` |
| VIP restoration service | `jiji-service-nat-{slug}.service` |
| Static DNS service | `jiji-dns-{slug}.service` |

`{wireguard_interface}` is a separate short (`jiji` + 8 hex chars) identifier
from `{slug}`, kept under Linux's 15-character interface name limit -- see
`jiji network plan` for the concrete value.

## Required firewall access

Allow UDP traffic between the configured server public IP addresses on each
project's WireGuard port (`jiji network plan` prints the exact port -- it's
derived per project, in the range `51820`-`55819`, not always `51820`).
Container subnets are routed only through WireGuard. Corrosion gossip and API
ports are not used.

## Verification

```bash
# Substitute this project's derived interface name and slug (see `jiji network plan`)
sudo wg show <wireguard_interface>
ip route show dev <wireguard_interface>
systemctl status jiji-network-restore-<slug> jiji-service-nat-<slug> jiji-dns-<slug>
readlink -f /etc/jiji/network/<slug>/current
readlink -f /etc/jiji/network/<slug>/dns-current
docker exec <container> getent hosts <project>-<service>.jiji
```

Use `podman exec` instead of `docker exec` when Podman is selected.

## Reboot behavior

WireGuard and the jiji systemd units restore the selected local generations.
Containers restart with their assigned backend addresses according to their
engine restart policy. No coordination service, SSH connection, jiji command,
or redeployment is required when topology has not changed.

Podman keeps its bridge only while a container is attached. Jiji installs a
minimal local `jiji-network-anchor-{slug}` container (one per project) at
reserved address `.3` and orders `podman-restart.service` after bridge
restoration. `podman-restart.service` itself is host-global, gated on every
project's restore unit via one drop-in file per project
(`/etc/systemd/system/podman-restart.service.d/jiji-network-{slug}.conf`).
The anchor uses a local static BusyBox rootfs and does not pull an image.

## Repair

Run `jiji network setup`. It stages and validates a complete generation on
every selected host before activation. If activation or verification fails,
all attempted hosts return to their previous retained generations. Repairing
one project's network never touches another project's tree, units, or
bridge on a shared host.

`jiji deploy` checks every configured host before making deployment changes.
When it detects a stale generation, it automatically applies the complete
network plan to the full cluster using the same transactional setup path.
The explicit command remains available for network-only maintenance.

## Upgrading from a pre-isolation jiji version

Per-project isolation changed the on-disk layout, WireGuard interface name,
and bridge name -- every already-provisioned host's installed generation
becomes incompatible with the new version, even for a host that only ever
ran one project. There is no in-place migration: run `jiji server teardown`
against the old layout (or manually remove `/etc/jiji/network`, the old
`jiji0` WireGuard interface, and the old `jiji` bridge network), then `jiji
server setup` again on the new version.
