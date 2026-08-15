# Architecture Notes

This document records current architectural invariants and the reasoning that
is easy to lose when changing Jiji. Use the source files linked in each
section for implementation detail. User-facing behavior belongs in the docs
site at `~/Code/jiji-website/app/docs/`.

Do not add release history, phase-by-phase migration notes, or closed incident
reports here. Preserve a past bug only when it establishes a constraint that
current code still depends on.

## System Boundaries

Jiji has three runtime components:

- `jiji`, the operator-side CLI, loads configuration, plans mutations,
  connects over SSH, acquires locks, and coordinates transactions.
- `jiji-agent`, one systemd service per project per host, owns durable local
  state, WireGuard and bridge repair, DNS, catalog replication, leases,
  container reconciliation, and scheduled jobs.
- `jiji-proxy`, one shared container per physical host, provides HTTP, HTTPS,
  and raw TCP ingress for every Jiji project on that host.

The CLI is not a persistent controller. Once bootstrap or a mutation
finishes, agents maintain the runtime state that must survive an operator
disconnect or host restart.

Primary sources:

- `crates/jiji-cli/src/commands/`
- `crates/jiji-agent/src/runtime.rs`
- `crates/jiji-agent/src/local_reconcile.rs`
- `crates/jiji-proxy/src/main.rs`

## Distributed Control Plane

Every project agent has a durable SQLite store. Membership, catalog records,
desired placement, address leases, cron specifications, and cron run history
have different ownership and replication rules.

### Membership

Membership is derived by the CLI from `jiji.yml` and observed host identity,
then pushed directly over SSH. It is not peer-to-peer replicated and contains
no signing key. A host trusts membership because root installed it.

Changing a host endpoint increments its membership revision. Changing its
WireGuard key advances its owner epoch, fencing records from the old identity.
Removing a configured host creates an explicit tombstone. An unreachable host
is never interpreted as removed.

Sources:

- `crates/jiji-cli/src/commands/network/membership.rs`
- `crates/jiji-agent/src/membership.rs`

### Catalog and desired placement

Catalog and desired-placement records are node-originated and replicate by
direct, one-hop anti-entropy over the WireGuard mesh. A node sends only records
it owns. Receivers authenticate provenance by mapping the connection's mesh
source address to membership. WireGuard authenticates that source address, so
records do not need a second signature layer.

Records from a third node are never relayed. Protocol and schema mismatches are
rejected before state is applied.

Sources:

- `crates/jiji-agent/src/catalog.rs`
- `crates/jiji-agent/src/desired.rs`
- `crates/jiji-agent/src/catalog_replication.rs`

### Control-plane invariants

- Deploying a service never rewrites WireGuard configuration.
- Temporary absence suppresses reachability but never deletes durable state.
- Permanent removal requires an explicit tombstone.
- DNS publishes only Active and Healthy catalog records that are currently
  reachable from the answering node.
- A targeted mutation connects to and locks only its mutation owners, except
  when a documented reconciliation sweep needs a wider host set.
- Configuration limits are 32 nodes, 500 services, and 2,000 logical replicas
  per project.

## Health-Gated Deployment Transaction

A logical replica has a stable `replica_id`. Each replacement creates a new
deployment with a unique `deployment_id`, container name, and dynamically
leased address. Jiji does not use fixed A/B slots or a service VIP.

For a rolling service, `deploy_dynamic_endpoint` performs this transaction:

1. Read the current Active and Healthy deployment, if one exists.
2. Allocate an address for a fresh deployment ID.
3. Stage mounts and an environment file. Secrets are not placed in command
   arguments.
4. Commit the new deployment as Candidate and Unknown before starting it, so
   failed startup remains recoverable state.
5. Create and start the candidate at its leased address.
6. Health-check the candidate directly, never through DNS or jiji-proxy.
7. Commit it Active and Healthy, making it visible through project DNS.
8. Reapply and verify each proxy route until the candidate appears as a
   healthy backend.
9. Mark the previous deployment Draining, remove its container, release its
   lease, and tombstone its catalog record.

Failure before route verification completes removes the candidate and leaves
the previous deployment serving. There is no route restoration step because
routes discover backends from DNS rather than storing a single pushed target.

`service.stop_first: true` deliberately stops the previous container before
starting its replacement. Direct host-port bindings also prevent old and new
containers from binding the same port concurrently. Neither case guarantees
zero downtime.

Sources:

- `crates/jiji-cli/src/deploy_transaction.rs`
- `crates/jiji-cli/src/health_check.rs`
- `crates/jiji-cli/src/placement.rs`
- `crates/jiji-agent/src/leases.rs`

## Project Networking

Each project derives its own WireGuard interface, routed bridge, agent unit,
state directory, DNS address, proxy attachment address, and replication port.
Projects on one host do not share project network or control-plane state.
The host-global jiji-proxy is the intentional exception.

When `network:` or either CIDR override is absent, the network planner derives
stable project-specific `/24` management and `/16` container ranges from
`config.project`. The result is independent of the checkout directory and is
identical on every operator machine. The `/16` contains exactly 32 `/21`
server subnets. Explicit CIDRs remain the escape hatch for overlap with a LAN,
VPN, cloud VPC, or another project that selected the same finite default slot.
Each active compiled generation records its full reserved ranges. Network
preflight compares those markers across co-located projects before mutation,
in addition to inspecting host routes, container networks, interface
addresses, and WireGuard ports. This catches a shared project-range collision
before two projects happen to allocate the same server subnet.

`jiji server setup` bootstraps the host. After bootstrap, the agent owns
continuous repair of its WireGuard interface, bridge, DNS binding, proxy
attachment, and local containers. `jiji deploy` reconciles networking only
when a selected host is missing or stale.

### Address ownership

Docker and Podman IPAM do not know about Jiji's reserved infrastructure
addresses or agent leases. Every Jiji-managed container attached to a project
bridge must receive an explicit address allocated by the agent. New code must
not start an unaddressed container on a Jiji bridge and rely on engine IPAM.

### DNS

Each agent serves the project's `.jiji` zone from its replicated catalog:

- `{project}-{service}.jiji` returns all reachable Active and Healthy replicas.
- `{project}-{service}-{server}.jiji` returns the matching reachable replica.
- Queries outside the project zone are forwarded to configured DNS
  forwarders.

Reachability is a reversible filter. It does not mutate or tombstone catalog
records.

Sources:

- `crates/jiji-network/src/naming.rs`
- `crates/jiji-agent/src/wireguard.rs`
- `crates/jiji-agent/src/bridge_bringup.rs`
- `crates/jiji-agent/src/dns.rs`

## Shared Ingress Proxy

jiji-proxy is shared across projects on a host and attaches to every project
bridge with an active route. Adding a project must preserve existing project
attachments and routes.

Routes contain discovery information, not concrete backend addresses. Each
route resolves the aggregate service DNS name and load-balances across healthy
backends mesh-wide. Reapplying a route forces immediate resolution during a
deployment; the proxy also refreshes and health-checks backends continuously.

The agent uses setup-time route specs only to repair missing routes. It does
not overwrite an existing route because a later deploy can change its port or
policy. The deploy-time configuration is authoritative for an existing route.

### HTTP and HTTPS routes

HTTP routes are keyed by `(host, path_prefix)`. Exact hosts take precedence
over single-label wildcard hosts. Automatic TLS uses ACME HTTP-01, so wildcard
hosts require a supplied certificate. Static certificates take precedence
over ACME-managed certificates.

### Raw TCP routes

`listen_port` selects raw TCP relay mode and is the public port. `port` remains
the backend container port. Raw TCP routes are keyed by `listen_port` and have
no Host header, path routing, or TLS termination.

Constraints:

- `listen_port` cannot be combined with `path_prefix` or `ssl`.
- Ports 0, 80, and 443 are reserved.
- A public TCP port must be unique across every project sharing a host.
- Config validation catches same-project conflicts. jiji-proxy catches
  cross-project conflicts when applying a route.
- `hosts` is optional metadata for a TCP target, not a routing key.

### Docker ingress

Jiji project bridges use routed, NAT-disabled networking. Docker can therefore
drop the IPv4 side of normal published-port bindings for jiji-proxy. Jiji uses
a host-global nftables ingress table to forward public HTTP, HTTPS, and raw TCP
ports to the proxy's project attachment.

Ingress rules must match the intended public destination address as well as
the destination port. Port-only rules can capture unrelated host traffic or
cross-host mesh traffic. One co-resident agent owns ingress reconciliation
through the host-global lease in `host_lease.rs`.

The lease guard explicitly unlocks before it closes its descriptor. This
prevents Podman helpers from retaining an inherited flock after proxy creation.
The CLI also uses `flock --close` so the proxy command never receives its lock
descriptor.
During an upgrade from an older agent, a helper can already hold the old lock.
The agent replaces that lock inode only when every other process with the inode
open is a known container helper. It never replaces a lock held by an agent,
the CLI, or an unknown process.

Sources:

- `crates/jiji-cli/src/proxy_routes.rs`
- `crates/jiji-cli/src/proxy_ingress.rs`
- `crates/jiji-proxy/src/route_manager.rs`
- `crates/jiji-proxy/src/tcp_relay.rs`
- `crates/jiji-proxy/src/acme.rs`
- `crates/jiji-agent/src/host_lease.rs`
- `crates/jiji-agent/src/proxy_bringup.rs`

## Container Namespace Sharing

`network_mode: service:<upstream>` makes a dependent container share the
upstream container's network namespace. It is intended for VPN killswitch
patterns.

The upstream name is the dependency declaration. Validation rejects missing,
self-referencing, or chained upstreams; server placement outside the
upstream's server set; replicas above one; and a proxy or health check on the
dependent. The dependent receives no address, DNS settings, published ports,
or proxy route of its own.

Selecting an upstream cascades its direct dependents into the deployment plan.
Deployment runs in two waves: upstreams first, then dependents. Waiting inside
one bounded SSH-pool wave can deadlock when the pool has a single permit.
Proxy activation for an upstream whose target is served by a dependent must be
deferred until the dependent wave finishes.

Namespace-sharing updates are not zero-downtime. There is a real interval
between removal of the old upstream and attachment of the new dependent.

### Host networking (`network_mode: host`)

A service can instead share the host's own network namespace via
`network_mode: host`. Unlike `service:<upstream>` sharing, it still goes
through the full candidate/active/draining/tombstone catalog lifecycle and a
container-readiness health check, exactly like a bridge-networked service
does; it just never leases an address from the agent. The catalog record's
address is the server's own management (WireGuard mesh) address, since the
container shares that interface directly. `ports:` accepts at most one bare
container-side port number as routing metadata only, never rendered as
`-p`: combining `-p` with `--network host` is a discarded, warning-spamming
no-op on current Docker/Podman and a hard error on some older Podman
releases. Two `host`-mode services whose `servers` overlap cannot declare
the same port; the check is project-scoped, so a different project's
`host`-mode service on a shared machine can still collide, surfacing as a
failed container start rather than a validation error. `proxy:` and
`replicas > 1` are rejected by the same generic non-bridge checks that
already apply to `service:<name>` sharing.

`network_mode: none` is rejected by validation outright: every service
needs a reachable address for DNS and health checks, the catalog's address
field is not optional, and the one legitimate use of `none` (an isolated
one-off with no network needs) is already served by `crons:`.

Sources:

- `crates/jiji-config/src/validation.rs`
- `crates/jiji-cli/src/cascade.rs`
- `crates/jiji-cli/src/deploy_transaction.rs`
- `crates/jiji-network/src/service_runtime.rs`
- `crates/jiji-cli/src/container_runtime.rs`

## Scheduled Jobs

A service cron runs in a fresh one-off container using the service image and
runtime context. It does not run inside the serving container, a host crontab,
or a systemd timer.

### Ownership and reconciliation

The lowest-ordinal Active and Healthy replica owns a job. Specs and run history
are local to that owner's agent and are not replicated. Because the CLI cannot
infer stale specs from current configuration, reconciliation must inspect
every eligible server and remove installed specs absent from the desired set,
including when `crons:` is empty or an entry was renamed.

A cron reconciliation failure does not roll back a successful service deploy.

### Execution and recovery

Each run has a durable claim and its own leased address. On agent startup,
claimed or running jobs are reconciled against real containers before the
scheduler starts. Existing containers resume monitoring, missing containers
become failed runs, and unclaimed cron containers are removed.

Agent-spawned container paths must be absolute. Relative staging paths work in
an SSH login's home directory but resolve from `/` when the systemd agent
starts a process. Cron addresses must come from the host's `container_subnet`,
not the project-wide `container_cidr`.

The agent systemd unit uses `KillMode=process`. With Podman and cgroupfs,
`KillMode=control-group` can kill container monitor processes when the agent
restarts.

### Scheduler semantics

- `overlap: forbid` atomically skips a due run while the previous run remains
  active.
- `missed_runs: skip` advances to the next future occurrence instead of
  replaying missed ticks.
- Owner outage has no automatic failover. Ownership changes when a later CLI
  reconciliation installs the spec on a new owner.
- Completed metadata is retained for 30 days or the latest 100 runs per job,
  whichever retains more. Completed containers are retained for 24 hours so
  logs remain readable.

Sources:

- `crates/jiji-cli/src/cron_reconcile.rs`
- `crates/jiji-agent/src/cron.rs`
- `crates/jiji-agent/src/cron_exec.rs`
- `crates/jiji-agent/src/scheduler.rs`

## Teardown Ordering

`jiji server teardown` must remove the project agent first. Leaving it running
allows continuous reconciliation to recreate routes, proxy attachments, or
containers while teardown removes them.

After removing the agent, teardown removes project proxy routes, service and
cron containers, optional named volumes, images, staging data, proxy
attachments, and project networking. The shared proxy container and
host-global ingress state are removed only when no project still uses them.
Exact names and ownership labels determine scope; teardown must not use broad
globs or remove another project's resources.

Removing WireGuard includes both configuration files and the live kernel
interface. No `wg-quick` unit exists to provide that cleanup implicitly.

Sources:

- `crates/jiji-cli/src/commands/server/teardown.rs`
- `crates/jiji-cli/src/proxy_teardown.rs`
- `crates/jiji-cli/src/network_teardown.rs`
- `crates/jiji-cli/src/teardown_plan.rs`

## SSH Connection Semantics

`jiji-ssh` uses russh directly. `SshPool` bounds concurrency and reuses
sessions across command work and lock operations. ProxyJump uses a
stream-backed nested session. Registry forwarding binds the remote listener
to `127.0.0.1`, so `GatewayPorts` is not required.

During server setup, Jiji reads target membership through the SSH sessions
that hold host-runtime locks. Jiji pushes membership through the agent-install
sessions. It opens new membership sessions only for non-target hosts. This
connection reuse reduces pressure on SSH firewall rate limits. If a setup
connection is refused, Jiji waits 31 seconds and retries once. The wait covers
the 30-second window of `ufw limit ssh`. Jiji does not retry during this
window because each rejected connection can refresh the limit.

SSH can report a signal without an exit status. An absent exit status must be
treated as failure, never as success. Interactive stdin uses a dedicated
reader thread because cancelling a Tokio blocking stdin read does not cancel
the underlying read and can hang process shutdown.

Sources:

- `crates/jiji-ssh/src/session.rs`
- `crates/jiji-ssh/src/pool.rs`
- `crates/jiji-cli/src/ssh_adapter.rs`
- `crates/jiji-cli/src/commands/server/exec.rs`

## Deployment Locks

Locks use atomic remote directory creation and a fixed rank order:

```text
ProjectMaintenance < HostRuntime < ServiceScale < LogicalReplica < HostGlobalProxy
```

A command computes its complete lock set before mutation, sorts by rank, host,
and path, then acquires concurrently within a rank and sequentially between
ranks. Every lock set is a subset of this total order, preventing deadlock.

Scopes reflect ownership:

- project maintenance for control-plane-wide maintenance;
- host runtime for server setup and teardown;
- service scale for desired replica-count changes;
- logical replica for deploy, restart, rollback, and removal;
- host-global proxy for the shared proxy container and ingress state.

Source: `crates/jiji-cli/src/lock.rs`.

## Naming and Ownership

Stable identities and runtime instances are different:

- Logical replica: deterministic from project, service, and ordinal.
- Deployment: a fresh ID for every container start.
- Service container: `{project}-{service}-{deployment-id-prefix}`.
- Aggregate DNS: `{project}-{service}.jiji`.
- Per-server DNS: `{project}-{service}-{server}.jiji`.
- HTTP route: `(host, path_prefix)`.
- Raw TCP route: `listen_port`.

Managed resources use `jiji.managed=true` plus project, service, server, and
resource labels. Exact project-derived names come from
`crates/jiji-network/src/naming.rs`; do not duplicate their derivation in
command code.

## Container Engine Provisioning

`engine::ensure_engine` is shared by server setup and remote-builder
preflight. Debian and Ubuntu use the pinned, checksum-verified static Podman
bundle. Fedora and RHEL use `dnf`. An installed Podman below the required
version is upgraded.

The managed static Podman configuration uses crun and cgroupfs. Podman exec
commands pass `--no-session` to avoid a PAM session for every health probe or
proxy command. Network bootstrap enables linger for the SSH user so systemd
does not terminate rootful containers when the SSH session closes.

Agent file writes use `install -D`. This command creates a missing destination
directory before it writes the file.

Ubuntu 26.04 confines `wg` and `wg-quick` with AppArmor. Network bootstrap
adds idempotent local rules for Jiji's private key and immutable WireGuard
configuration. These rules keep Jiji's project-scoped, root-only state layout.
Systems without the packaged profiles take a no-op path.

Sources:

- `crates/jiji-cli/src/engine.rs`
- `crates/jiji-cli/src/commands/network/setup.rs`

## Testing Boundaries

Mock-SSH tests verify orchestration, rendered commands, output, and ordering.
They do not validate container-engine formatting differences, systemd cgroup
behavior, nftables packet matching, kernel networking, or real terminal
lifecycle.

Changes in these areas require live validation in addition to unit and
integration tests:

- Docker or Podman command rendering and label inspection;
- routed bridge, WireGuard, DNS, and nftables behavior;
- agent restart and systemd ownership;
- shared proxy attachment, HTTP/TCP ingress, and teardown;
- interactive PTY and signal handling.

Tests should preserve these known constraints:

- Docker and Podman expose labels through different formatting expressions.
- Local loopback registries require `--tls-verify=false` with Podman.
- nftables ingress must match the public destination address, not only a port.
- teardown tests must verify that the agent is removed before reconciled
  resources.
- interactive execution must cover remote exit without local stdin activity.

Source examples:

- `crates/jiji-cli/tests/deploy_test.rs`
- `crates/jiji-cli/tests/server_setup_test.rs`
- `crates/jiji-cli/tests/server_teardown_test.rs`
- `crates/jiji-ssh/tests/session_test.rs`
