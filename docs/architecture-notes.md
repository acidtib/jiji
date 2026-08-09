# Architecture Notes

Deep implementation rationale, full mechanism detail, and the historical
incident record for jiji's architecture. `CLAUDE.md` keeps a tight,
always-loaded summary of each area with a pointer here; read this file when
you need the full "why" and "exactly how," not just the "what." Update this
file (not just CLAUDE.md) when the underlying mechanism changes. CLAUDE.md's
summaries are only useful if this detail stays in sync with them.

## Distributed Control Plane

Jiji replaced its original compiled, all-host `NetworkPlan` design with a
**distributed, per-project control plane**: every server runs a project-scoped
`jiji-agent` (`crates/jiji-agent/`) that owns a durable local store,
incrementally repairs its own WireGuard peers, serves project DNS from its
local catalog, and reconciles its own containers/leases/proxy routes on
restart. Membership has no key material and no peer-to-peer relay: `jiji-cli`
computes it locally from `jiji.yml` and pushes it directly over SSH to every
reachable host (`jiji-agent membership-import`), so a host's own trust
boundary is "this file was installed by root," the same as every other
agent-managed file (see `jiji-agent/src/membership.rs`'s module doc comment).
The service catalog and desired-state placement, being genuinely
node-originated at runtime (crash-restart reconciliation, health flips), keep
continuous peer-to-peer anti-entropy (`catalog_replication.rs`) -- but
direct-only, one hop, never relayed through a third node: a node's outbound
exchange contains only records it owns, and a receiver authenticates an
inbound record by resolving the TCP connection's actual source address
against its local membership view rather than a signature, since WireGuard's
own peer authentication already makes that source address unspoofable within
the mesh (see `catalog.rs`'s `RecordProvenance` doc comment). There is no
legacy runtime or mixed-version cluster to support: protocol and schema
versions are checked before any state exchange (`jiji-agent/src/
membership.rs`, `catalog_replication.rs`), and a mismatch is rejected outright
rather than partially applied.

Non-negotiable invariants: a service deployment never rewrites WireGuard; a
targeted deploy normally connects to and locks only its affected host and
logical replica; temporary absence of a host or peer never means permanent
deletion, only an explicit tombstone does; DNS only ever publishes an
`active`+`healthy` catalog record.

Supported capacity per project (`crates/jiji-config/src/validation.rs`):
32 nodes in a full WireGuard mesh, 500 services, 2,000 logical replicas.
Configuration validation rejects node/service/replica cardinality above these
limits outright.

## Zero-Downtime Deployment Strategy

Jiji uses **dynamically leased deployment addresses**, not fixed dual-address
(A/B) backend slots or a stable service VIP. A **logical replica** (stable
`replica_id`, survives restart/redeploy) points at exactly one **deployment**
(the actual container underneath it, replaceable). `jiji-cli/src/placement.rs`
derives `replica_id` deterministically from `(project, service, ordinal)`; the
agent allocates a fresh address per `deployment_id` from the host's
container subnet on every deploy (`AllocateAddress`), so there is no
rename step and no fixed per-service address to collide with.

Flow (`crates/jiji-cli/src/deploy_transaction.rs::deploy_dynamic_endpoint`,
orchestrated by `commands/deploy.rs` through `crate::agent_client`'s calls
into the target host's own agent socket):

1. Read the replica's current `active`+`healthy` catalog record, if any
   (`agent_client::catalog`), and allocate a new address for a fresh
   `deployment_id` (`RequestBody::AllocateAddress`).
2. Stage mounts (`mounts.rs`) and environment (`env_resolution.rs`, via a
   remote `--env-file`, never inline `-e KEY=VALUE`, so secrets never appear
   in a logged command).
3. Commit the candidate to the catalog as `Candidate`/`Unknown`
   (`RequestBody::CatalogCommit`) *before* starting it, so its existence is
   durable even if the container start fails.
4. Create and start the candidate container
   (`container_runtime::build_dynamic_run` + `container_ops::create_and_start`)
   at its own newly leased address, named
   `container_runtime::dynamic_container_name` (`{project}-{service}-{first
   12 hex chars of deployment_id}`). The previous deployment keeps serving
   traffic throughout.
5. Health-check the candidate directly at its own address (`health_check.rs`),
   never through the proxy.
6. Commit the candidate as `Active`/`Healthy` in the catalog *before* touching
   jiji-proxy at all: jiji-agent's DNS resolver answers directly from this
   catalog, so the commit itself is what makes the candidate's address
   resolvable -- unlike kamal-proxy, jiji-proxy's route was never pushed an
   explicit target address to begin with (see "jiji-proxy" below).
7. "Activate" by re-applying the route's static definition
   (`proxy_routes::deploy_route`, a cheap idempotent upsert that always runs a
   synchronous DNS re-resolution) and then polling `jiji-proxy route status`
   (`proxy_routes::verify_route_address`) until the candidate's specific
   address shows up as a healthy backend, or the same timeout the
   pre-activation health check used elapses. If verification fails, roll the
   candidate back (tombstone it, release its lease) rather than leave an
   unverified record live for jiji-proxy's mesh-wide DNS discovery to
   potentially pick up on some other host; `previous` was never touched, so
   there is nothing to "restore" the way kamal-proxy's single-target route
   needed.
8. Commit the previous deployment (if any) as `Draining`, in a separate
   catalog transaction.
9. Stop and remove the previous deployment's container, release its address
   lease (`RequestBody::ReleaseAddress`), then commit it `Tombstoned`.

If any step through health-checking or proxy activation fails, the previous
container and route are never touched and keep serving traffic; only the
candidate is torn down. `service.stop_first: true` is a distinct transaction
that stops the previous container before starting the candidate (for
services that cannot tolerate two running instances) and attempts to restart
it if the candidate fails to come up.

## Private Networking (WireGuard mesh + agent-served DNS)

`jiji network setup` (`crates/jiji-cli/src/commands/network/setup.rs`) still
writes each host's WireGuard bootstrap material to a symlink-swapped
"generation" tree under `/etc/jiji/network/{slug}/`, but that generation now
covers **mesh bootstrap only** (the WireGuard interface, its initial peer set,
the bridge/engine network). It is a one-time/repair step, not something a
service deploy ever touches: `jiji deploy` never re-runs it except to reconcile
a genuinely stale host before continuing. Everything that changes on every
deploy or scale (membership updates after the first tunnel, the service
catalog, DNS answers, container reconciliation) is owned continuously by the
long-running `jiji-agent-{slug}.service` (`crates/jiji-agent/`, installed by
`jiji server setup`, one process per project per host), not recomputed and
reapplied by the CLI. `network setup`'s own `remove_legacy_service_runtime`
step additionally cleans up any pre-agent installation's `jiji-dns-{slug}`/
`jiji-service-nat-{slug}` systemd units and nftables table it finds on a host,
since those belonged to the old compiled-DNS/VIP-NAT design this control plane
replaced (Phase 8) and are never reinstalled.

**Per-project isolated, not a host-global singleton.** Every name and path
below is derived purely from `config.project` (`crates/jiji-network/src/
naming.rs`). Two independent projects can run `jiji server setup` against
the same physical host and get two fully independent sets of the following,
with zero shared/persisted state between them (see "Naming Conventions"
below for the exact derivation and the jiji-website repo's Network Reference
page, `~/Code/jiji-website/app/docs/reference/network/page.mdx`, for the
operator-facing explanation, including the residual hash-collision risk when
projects share default CIDR ranges):

- **WireGuard**: interface name `jiji{8 hex}`, one per project. Config is
  still written to `/etc/jiji/network/{slug}/generations/{gen}/wireguard.conf`
  (symlinked to `/etc/wireguard/{wireguard_interface}.conf`) for manual
  `wg-quick`/`wg` inspection and recovery, but as of Phase 9 no
  `wg-quick@{iface}.service` unit is installed or enabled: the agent brings
  the interface up itself at startup (`local_reconcile.rs::ensure_link`,
  equivalent to what `wg-quick` did, since jiji's rendered config never used
  `PostUp`/`PostDown` hooks) and repairs it if torn down externally.
  WireGuard port is per-project (`51820..=55819`), not the fixed `51820`.
  Once the first tunnel is up, `jiji-agent` takes over incremental peer
  repair (`jiji-agent/src/wireguard.rs::plan_reconciliation`). New hosts
  join and endpoints roam without the CLI re-touching this generation.
- **Bridge/engine network** (`commands/network/bridge.rs`): a
  `jiji-{slug}` docker/podman network per project (kernel device name
  `jijib{7 hex}`, distinct from the logical name because of Linux's 15-char
  interface limit). As of Phase 9 the agent brings this up itself at
  startup (`local_reconcile.rs::ensure_bridge_and_dns`/`bridge_bringup.rs`)
  instead of depending on a separate `jiji-network-restore-{slug}.service`
  oneshot unit, and the Podman-only `podman-restart.service.d` drop-in
  (which used to restart `unless-stopped` containers on boot) is retired
  entirely: the agent's own post-restart container reconciliation
  (`reconcile_containers`/`recover_startup_candidates`) was already
  authoritative, so the drop-in was a straight duplicate.
- **Membership**: plain (unsigned) records, computed locally by `jiji-cli`
  from `jiji.yml` and pushed directly over SSH to every reachable host
  (`jiji-agent membership-import`, see `jiji-agent/src/membership.rs`'s
  module doc comment): no peer-to-peer relay, so nothing beyond "installed
  by root" authenticates a change. There is no operator-facing
  membership-editing command: every `jiji server setup` run reconciles
  membership from config and observed host state on its own
  (`commands/network/membership.rs::reconcile_record`/
  `compute_decommissions`, invoked from `commands/server/setup.rs::
  setup_agents`). Per targeted host, it re-reads the current on-disk
  WireGuard public key and the config-derived endpoint and compares them
  against the last known record: unchanged is a no-op; an endpoint-only
  change just bumps `revision` at the same `owner_epoch`; a changed public
  key (organic drift, a re-provisioned host under the same server name, or
  `--rotate-key` forcing a fresh keypair before this reconcile pass) fences
  a new `owner_epoch`: no separate tombstone message is transmitted for
  this, since `MembershipView::apply`'s ordering rule already makes a
  strictly higher `owner_epoch` win outright on every peer and reject any
  future record still asserting the old one. Separately, any server still
  `Active` in the gathered mesh view but no longer present in `servers:` is
  tombstoned. A host unreachable at push time keeps its last-known
  membership until the next `jiji server setup` reaches it; that command
  additionally pushes the complete merged set to every already-configured
  server, not just the ones it's targeting, so an existing peer's own
  WireGuard reconciliation learns about a newly enrolled or changed host
  without waiting for that later contact.
- **Service catalog**: `jiji-agent` replicates a node-owned, append-only
  operation log peer-to-peer over WireGuard (`jiji-agent/src/catalog.rs`,
  `catalog_replication.rs`), authoritative for each logical replica's current
  deployment, address, image, and `Candidate`/`Active`/`Draining`/`Stopped`/
  `Tombstoned` state. Unlike membership, this genuinely is node-originated at
  runtime (crash-restart reconciliation, health flips), so it keeps
  continuous anti-entropy sync, but direct-only: a node's outbound exchange
  contains only records it owns, never a relayed third party's, and a
  receiver authenticates an inbound record by resolving the TCP connection's
  source address against its local membership view (`RecordProvenance`,
  `MembershipView::find_by_management_address`) instead of a signature:
  sufficient because WireGuard's own peer authentication already makes that
  source address unspoofable within the mesh, and because nothing is ever
  relayed there's only the one hop to authenticate. There is no CLI-driven
  VIP/NAT cutover and no `service-nat` nftables table for live traffic;
  `jiji-cli/src/deploy_transaction.rs` is the only thing that ever commits
  new records in the normal deploy path.
- **DNS**: each `jiji-agent` process serves the `.jiji` zone directly from its
  local replicated catalog (`jiji-agent/src/dns.rs`, a hand-rolled minimal
  authoritative resolver on the project's management address, UDP with TCP
  fallback for large answer sets). There is no `dnsmasq` process and no
  compiled `dns.conf` in the running system anymore. Only `active`+`healthy`
  records are ever answered with; a peer the local agent currently considers
  unreachable is suppressed reversibly, never deleted, from both the
  aggregate (`{project}-{service}.jiji`) and per-server
  (`{project}-{service}-{server}.jiji`) names.
- **jiji-proxy** (`crates/jiji-proxy/`, driven from the CLI side by
  `crates/jiji-cli/src/proxy.rs`/`proxy_routes.rs`): a Pingora-based (Rust)
  reverse proxy container (`ghcr.io/acidtib/jiji-proxy:v{version}`, pinned to
  the exact `jiji-proxy` version this build was compiled against --
  `jiji_network::image()` -- since `jiji-proxy` is versioned and released
  independently of `jiji`/`jiji-agent`), provisioned
  per-server by `jiji server setup`, that fully replaced the earlier
  kamal-proxy Go fork. Deliberately the **one genuinely shared, multi-tenant**
  component: one container per host, **multi-homed** across every project's
  bridge that has active routes on that host (`network connect --ip
  <ServerPlan::proxy_address> <bridge_name> jiji-proxy`, idempotent/additive,
  see `ensure_attached`), routes namespaced per project already.

  jiji-proxy's routing model is fundamentally different from kamal-proxy's:
  jiji-cli/jiji-agent push a **static route definition** once per
  `(host, path_prefix)` (`jiji-proxy route apply --host --dns-server --name
  --port [--path-prefix] [--tls] [--health-check ...]`, see
  `proxy_routes::RouteTarget`/`targets_for_service`) instead of an explicit
  target IP:PORT on every deploy. jiji-proxy itself then continuously
  re-resolves the **aggregate** `{project}-{service}.jiji` DNS name (served
  mesh-wide by every host's `jiji-agent` from its replicated catalog, not
  filtered to local replicas) on its own `refresh_interval_secs`
  (`crates/jiji-proxy/src/discovery.rs`, `route_manager.rs`) and
  load-balances (`pingora::lb::LoadBalancer<RoundRobin>`) across whatever it
  discovers. This is what gives jiji-proxy genuine **cross-host load
  balancing** (confirmed live: each host's jiji-proxy independently
  discovers and routes to every healthy replica of a service, not just the
  ones running on that same host), something kamal-proxy could never do.
  jiji-proxy also runs its own **active health-checking**
  (`route_manager.rs::build_health_check`, HTTP or TCP, translated from
  `service.proxy.healthcheck.{path,interval,timeout}`: `healthcheck.cmd` is
  never translated, since execing into a container only makes sense when the
  backend happens to be on the same host as jiji-proxy, an assumption
  mesh-wide routing can no longer make; `cmd` remains meaningful only for
  jiji's own separate pre-activation gate, `health_check.rs`), giving a
  backend that starts failing mid-interval fast eviction from `select()`
  rather than waiting out `refresh_interval_secs`.

  A route's `hosts:` entry may be a single-label wildcard (`*.example.com`),
  matched by `RouteManager::lookup` falling back to
  `wildcard::parent_wildcard_key` (`crates/jiji-proxy/src/wildcard.rs`) when
  no exact-host route exists: it strips exactly the left-most DNS label off
  the incoming host and checks for a route registered under `*.` plus the
  remainder. This is what gives the single-level semantics (`foo.example.com`
  matches `*.example.com`; `deep.foo.example.com` does not, since stripping
  its own left-most label yields `foo.example.com`, which only matches a
  route configured for the more specific `*.foo.example.com`; the bare
  `example.com` never matches its own wildcard sibling either) with no
  special-casing beyond that one string operation. An exact-host route
  always wins over a wildcard for the same request, since the exact lookup
  is tried first. `CertStore::get` (`cert_store.rs`) uses the identical
  fallback for TLS SNI resolution, since it's also an exact-match table.

  A deploy's "proxy activation" step (see "Zero-Downtime Deployment
  Strategy" above) therefore isn't pushing a new target address (the route
  never carried one); it's re-applying the (unchanged) route definition to
  force an immediate re-resolution, then polling `jiji-proxy route status`
  (`AdminRequest::RouteStatus`, backed by `Backends::get_backend`/`ready`)
  until the candidate's specific address shows up healthy, restoring the
  same kind of synchronous "did the cutover actually happen" barrier
  kamal-proxy's own blocking `deploy` command used to provide.
  ACME/TLS automation (`crates/jiji-proxy/src/acme.rs`, `cert_store.rs`,
  `instant-acme`, HTTP-01 only) and static PEM certs
  (`proxy_routes::upload_static_certs_if_configured`, written straight into
  `CERTS_DIR` so `CertStore` picks them up without ACME ever touching them)
  are both handled by jiji-proxy itself now, not a separate cert-management
  path. A wildcard host can never get an ACME certificate (HTTP-01 cannot
  issue one; that needs DNS-01, not implemented). `jiji-config` rejects
  `ssl: true` on a wildcard host outright (`PROXY_WILDCARD_REQUIRES_STATIC_CERT`
  in `validation.rs`), and `RouteManager::tls_hosts` independently excludes
  any wildcard-pattern host from `AcmeManager`'s worklist regardless of how
  it got a `tls` flag, since the admin socket itself has no validation of
  its own. TLS for a wildcard host is only possible via a user-supplied
  static certificate. `commands/server/teardown.rs` disconnects jiji-proxy from a
  project's bridge (`proxy::disconnect_bridge_if_attached`) before that
  bridge can be removed, independent of whether jiji-proxy is still running
  for other projects, and, since jiji-agent's own continuous reconcile loop
  would otherwise recreate/reapply anything this teardown removes out from
  under it (confirmed live), `jiji-agent` itself is stopped as the very
  first teardown step, before jiji-proxy or any other network-layer
  resource is touched.

  Confirmed live on Docker: jiji-proxy's own `--publish 80:8080 --publish
  443:8443` silently drops its IPv4 binding, because its primary network is
  always one of the bridges above and those are created with
  `enable_ip_masquerade=false` + `gateway_mode_ipv4=routed` (needed for
  routable backend addresses across the WireGuard mesh). dockerd logs "Host
  port ignored, because NAT is disabled" and the IPv6 publish keeps working
  while IPv4 silently doesn't. `crates/jiji-cli/src/proxy_ingress.rs` works
  around this Docker-only (Podman's bridge creation doesn't set either
  option, so it's unaffected): a host-global (not per-project, since
  jiji-proxy itself is host-global) nftables table
  (`jiji_proxy_ingress`, `/etc/jiji/proxy-ingress/`) DNATs the public ports
  straight to jiji-proxy's bridge address, bypassing Docker's own
  port-publish path entirely. As of Phase 9, ingress reconciliation is
  owned by whichever co-resident project's agent currently holds a
  same-host, non-blocking `flock` lease
  (`crates/jiji-agent/src/host_lease.rs`,
  `/etc/jiji/proxy-ingress/agent.lock`). No separate
  `jiji-proxy-ingress-restore.service` boot-persistence unit exists
  anymore; the lease holder's agent reapplies the nftables table on every
  reconcile tick (`proxy_bringup.rs`). `ensure_proxy` (CLI) still
  re-applies it idempotently on first install;
  `proxy_teardown::teardown_proxy_container_if_unused`
  removes it only when jiji-proxy's own container is finally removed (no
  project has routes left).

  **Control surface** (`crates/jiji-proxy/src/admin.rs`): a length-prefixed
  JSON request/response protocol over a container-local Unix socket, the
  same framing shape as `jiji-agent`'s own API (`jiji-agent/src/api.rs`).
  Reached via `docker exec jiji-proxy jiji-proxy route ...` from
  `jiji-cli`/`jiji-agent`, mirroring how `docker exec kamal-proxy
  kamal-proxy deploy ...` reached kamal-proxy's own admin API -- this
  replaces that call, not the exec pattern itself. `RouteManager`
  (`route_manager.rs`) is the single struct registered with the Pingora
  `Server` for both admin-socket handling and per-route DNS refresh, since
  Pingora only accepts new services before `run_forever()` consumes it:
  routes added later at runtime through the admin socket could never be
  individually registered as their own background service otherwise.
  Driving one shared tick loop over a table `RouteManager` owns is what
  makes route apply/remove dynamic without a restart. `config.rs`'s
  `routes:`/TCP-route seed (mainly for standalone testing) is applied once
  at startup, before the admin socket accepts any requests; routes pushed
  later through the admin socket (the normal production path) are layered
  on top of that seed, never replacing it.

  **TLS certificates**: `CertStore` (`cert_store.rs`) resolves a
  terminated host's certificate by SNI at handshake time
  (`certificate_callback`, Pingora's `TlsAccept` hook), populated either by
  a static file pair an operator drops into `cert_dir`
  (`{host}.crt`/`{host}.key`, loaded once at startup and never touched
  again) or by `acme.rs`'s issuance/renewal loop. A static file always
  wins: `acme.rs` only ever issues/renews a host whose current entry is
  missing or itself ACME-sourced. ACME automation
  (`crates/jiji-proxy/src/acme.rs`, `instant-acme`) implements HTTP-01
  only, deliberately never DNS-01: DNS-01 is the right challenge type once
  more than one `jiji-proxy` instance can answer for the same hostname
  (HTTP-01's challenge response must be served by whichever instance
  ACME's validator happens to hit), but that needs a specific DNS
  provider's API wired in, which nobody has chosen yet -- HTTP-01 is
  correct and sufficient for today's single-ingress-host-per-hostname
  model. Pebble (the local ACME test CA used to validate this integration)
  strictly enforces RFC 8555 section 6.1's `User-Agent` requirement and
  rejects any request missing one with a 400 `malformed` problem document;
  `instant-acme`'s own `DefaultClient` never sets one, so `Account::
  builder()`/`from_credentials` against Pebble fail with a confusing
  "missing field newNonce" JSON error (parsing that 400 problem body as if
  it were the directory) unless wrapped with a client that adds it.

Docker/Podman's own IPAM has no knowledge of jiji's reserved infrastructure
addresses (`ServerPlan::dns_address`, `proxy_address`, `bridge_gateway`) or of
whatever deployment addresses the agent has currently leased: `jiji-agent`
runs as a host-level systemd process, not a container, so the engine can and
will hand out `dns_address` to an ad-hoc container started on a jiji bridge
without an explicit `--ip` (confirmed live, pre-isolation, against the old
shared `jiji` bridge: `docker run --network jiji nginx:alpine` got assigned
the DNS address and silently broke resolution for that container; the same
risk applies to any project's `jiji-{slug}` bridge today). Every jiji-managed
container avoids this because `container_runtime`/`proxy.rs` always pin
`--ip` explicitly to an address `jiji-agent` itself leased
(`leases.rs::AddressAllocator`). Any new code that runs a container on a
jiji bridge (debug tooling, health-check sidecars, etc.) must do the same.

## Container Namespace Sharing (`network_mode: service:<name>`)

A service can share another ("upstream") service's container network
namespace instead of getting its own dynamically-leased bridge address, via
`network_mode: "service:<upstream-name>"` (Compose's shorthand for what
Docker/Podman render as `--network container:<name>`): the standard "VPN
killswitch" pattern, where a torrent client shares a VPN gateway container's
network stack so all its traffic is forced through the tunnel. Naming the
upstream this way is itself the dependency declaration; there is no separate
`depends_on` field. `jiji_config::Service::network_mode_dependency()` parses
it; `validation.rs` rejects an undefined/self-referencing upstream, a
chained dependency (the upstream must itself use `network_mode: bridge`,
v1 supports exactly one level, not chains), a `servers:` list that isn't a
subset of the upstream's, and (via the pre-existing `NON_BRIDGE_SCALE`/
`NON_BRIDGE_PROXY` rules, which already generalize to any non-`"bridge"`
value) `replicas` above 1 or a `proxy:` block of its own. A dependent is
reached through the upstream's own route, at the upstream's own address,
never a route of its own.

A dependent has no address to lease: `deploy_transaction.rs::
deploy_shared_endpoint` (a sibling of the normal `deploy_dynamic_endpoint`,
dispatched from `deploy_endpoint` based on `network_mode_dependency()`)
skips `AllocateAddress`/`ReleaseAddress` entirely, resolves the upstream's
current Active/Healthy catalog record by filtering on `service` + `owner_
node_id` (not by recomputing a replica_id through placement arithmetic,
the upstream may use a different placement policy, but at most one of its
replicas can ever be Active/Healthy on a given server), and runs
`container_runtime::build_shared_run` /
`NetworkedContainerRun::shared` (`--network container:<upstream's current
container name>`, no `--ip`/`--dns*`/`-p`, all owned by the upstream).
Since a dependent can't configure `healthcheck:` (no `proxy:` allowed),
it gets `health_check::plan_for_candidate`'s existing no-config fallback:
an engine-native container-readiness check, the same one any bridge
service without an explicit `healthcheck:` already gets.

`commands/deploy.rs::add_cascaded_dependents` automatically adds every
direct dependent of a selected upstream to the deployment (visible in the
printed plan/confirmation prompt), using `placement::endpoint_replica_id`
(sorted-position-in-`servers` ordinal) rather than `placement::place` for
the dependent's own replica_id: a dependent's real cardinality is "one
instance per shared-namespace server," not an independently round-robined
replica count. Selecting a dependent alone never cascades the other
direction: it just attaches to the upstream's already-existing deployment,
failing actionably if the upstream has none.

The upstream's own redeploy must complete (its old container is only torn
down as part of that same transaction) before any cascaded dependent can
attach to its new one, so `commands/deploy.rs` deploys in two sequential
waves (an upstream-with-dependents wave, then a dependents wave) rather than
the usual single `SshPool::execute_concurrent` call: an in-closure wait on
the upstream's completion would deadlock, since `execute_concurrent`
acquires its semaphore permit *before* running a task, and the pool is
bounded to 1 whenever any selected service configures `proxy:` (true for a
VPN-gateway-shaped upstream). If the upstream's own proxy route targets a
port only a dependent actually serves, its inline `activate_proxy_routes`
call is forced to defer (`skip_proxy: true`) whenever it has a dependent in
the second wave; the route is instead verified by the already-existing
`reconcile_catalog_routes` pass, which already runs once after every
selected endpoint (upstream and dependents alike) has finished.

This is not itself zero-downtime with respect to upstream churn: there is a
real gap between the upstream's old container being removed and each
dependent's own redeploy completing, during which an old dependent
container may be degraded, matching Compose's own `depends_on: {condition:
service_healthy, restart: true}` semantics for this exact pattern (two
containers can't simultaneously share one upstream's single network
namespace during a bridge-swap-style cutover).

## Scheduled Cron Execution (`crons:`)

A service's `crons:` map (name -> schedule/timezone/command/timeout/
overlap/missed_runs) runs each job in a fresh one-off container built from
the service's own image and runtime context (environment, mounts,
resources, project network) -- never inside the running service container,
and never through host crontabs or systemd timers, so a job keeps running
even if the CLI operator disconnects. `jiji-agent`'s own scheduler
(`scheduler.rs`) owns the whole lifecycle locally; `jiji-cli` only ever
installs/removes a job's specification.

### Ownership and reconciliation

After every `jiji deploy`/`service restart`/`rollback`/`scale` that could
move ownership, `cron_reconcile::reconcile_after_deploy` picks the
lowest-ordinal Active/Healthy replica as the job's *owner*
(`select_cron_owner`) and pushes an idempotent `CronSpecApply` to that
replica's agent; `service remove` unconditionally removes every installed
spec for the service (`remove_all_cron_specs`). Idempotency is a content
hash, not a version number a caller has to track: `CronJobSpec::
canonical_hash` hashes a `CronSpecContent` view that deliberately excludes
identity/ownership bookkeeping (`project`/`service`/`cron_name`, `revision`/
the hash itself, `owner_node_id`/`owner_epoch`/`server`), computed as a
separate struct rather than hashed off `CronJobSpec` directly so a future
bookkeeping field never silently changes every installed job's hash. The
receiving agent stamps `owner_node_id`/`owner_epoch`/`server` from its own
local membership record, never trusting the caller for them (mirrors
`CatalogCommit`'s handling in `api.rs`).

Cron specs and run history are agent-local and **deliberately never
replicated** between hosts, unlike the catalog (`cron.rs`'s module doc: "only
a job's assigned owner ever needs it, so there is no `RecordProvenance`/
anti-entropy machinery here"). The practical consequence: finding a stale
spec -- left behind on a *former* owner after an ownership transfer, or left
behind entirely because its `crons:` entry was renamed or deleted -- requires
connecting to every eligible agent and asking what it actually has
installed; the CLI has no memory of what used to be configured, so there is
no other way to discover one. `cron_reconcile.rs` therefore connects to
every server in a service's `servers:` list, not just whatever `-H`/`-S` the
triggering command selected, extending the caller's SSH session pool in
place and closing only the sessions it newly opened, and always sweeps every
one of them (`remove_specs_absent_from`: list installed specs, remove
whatever isn't in the current desired set) after installing on the owner --
even when `service.crons` is now empty, and even when installation itself
failed (a broken owner shouldn't block cleanup on hosts that are still
reachable). `remove_all_cron_specs` (`service remove`) uses the same sweep
with an empty desired set, removing every installed spec for the service
regardless of `crons:`'s current content. (Found by code review, not a
real-host incident: an earlier version of this reconciliation only removed
specs whose names were still present in the current config, so a renamed or
deleted `crons:` entry -- or a service whose `crons:` block became empty --
left its old installation running forever, undiscoverable by `service
remove` either, since it had the same current-config dependency.)

A cron-only failure (installation, not the deploy itself) is reported as a
human-readable problem, never a rolled-back deploy -- a service deployment
that already
succeeded must not be undone for a cron-only failure.

### Execution model

The owning agent names each run's container `{project}-{service}-cron-
{cron_name}-{first 12 hex chars of run_id}` (falling back to a hash suffix
if that would exceed the engine's name-length limit, `cron_exec::
cron_container_name`), labeled `jiji.resource=cron`, `jiji.cron=<name>`,
`jiji.cron-run=<run_id>` alongside the usual `jiji.managed`/`jiji.project`/
`jiji.service`/`jiji.server` labels every jiji-managed container carries.
Network/address/DNS argv rendering reuses `jiji-network`'s
`NetworkedContainerRun` directly (the same renderer a normal service
deploy uses) rather than a second cron-specific renderer. Address leasing
reuses the durable `AddressAllocator`/`address_leases` machinery
generically, keyed by `cron_replica_id(service, cron_name)` ->
`"cron/{service}/{cron_name}"` (`leases.rs`) so a cron job's lease can never
collide with a real replica's.

On agent startup, `cron_exec::recover_claimed_runs` runs before the
scheduler starts (`main.rs`), so a still-`claimed`/`running` run from before
a restart is never raced by a fresh scheduler tick: it matches each active
run against actually-running containers (by the `jiji.cron-run` label),
resumes monitoring anything still running with whatever timeout remains
(not the full configured duration again), finalizes anything that already
exited while the agent was down, marks a run `Failed` if its container is
simply gone, and stops/removes any *unclaimed* cron container found on the
host (a container with no matching active run in the store at all).

### Scheduler rules

`scheduler.rs::tick` evaluates every installed spec once per pass: a
freshly installed job is only initialized forward from `now` (never claims
retroactively, since there is no prior schedule to have missed). A due job
is claimed via `AgentStore::claim_cron_run`, a single SQLite transaction
that handles three outcomes atomically -- a fresh claim, an idempotent
replay of the same due time (`DuplicateScheduledClaim`, e.g. two overlapping
ticks), or `OverlapForbidden` (the only supported `overlap` value: skips a
due run while the prior run for this job is still active, incrementing the
durable `skipped_overlap` counter `service cron status` reports). Claim
atomicity is entirely local to this transaction -- unlike `deploy`/`restart`/
`rollback`, a cron claim never goes through the CLI's `LogicalReplica` SSH
lock scope (`lock.rs`); there is no distributed coordination to speak of,
since only one agent (the owner) ever runs a given job's ticks.

`missed_runs: skip` (the only supported value) advances the schedule from
the *natural* next occurrence after the due time just claimed, but only if
that occurrence is still in the future; if it has also already passed (the
tick fell behind by more than one interval, whether from a long agent
outage or just a slow tick), it skips straight to the next occurrence after
`now` instead of claiming every missed tick one by one -- collapsing any
pile-up of missed ticks without a separate startup-recovery pass.

A second periodic pass (hourly, `SCHEDULER_CLEANUP_INTERVAL_SECS` in
`main.rs`) enforces retention: completed run *metadata* is kept 30 days or
the latest 100 runs per job, whichever is more (`METADATA_RETAIN_SECS`/
`METADATA_RETAIN_LATEST`, `AgentStore::retain_cron_runs`, and never removes
a still-active run); a completed run's *container* is kept 24 hours past its
own `finished_at` so `cron logs` can still read its output
(`CONTAINER_RETAIN_SECS`, `cron_exec::cleanup_old_cron_containers`) --
these are fixed constants today, not per-service configuration.

### Failure semantics

- **Container creation failure**: finished as `Failed` immediately (no
  retry in this release), with its address lease released back to the
  pool.
- **Timeout**: the container is `stop`ped, given a short grace period to
  exit on its own, then force-removed if it hasn't; recorded as
  `TimedOut` with whatever exit code was observed, or none if it never
  produced one.
- **Owner outage**: no automatic failover. The old owner simply stops
  scheduling once it's offline; a new owner only starts once the CLI
  installs a spec there (naturally, on the next deploy/restart/rollback/
  scale that changes ownership).
- **Specification removal while a run is still active**: the plan's
  intended semantics are that an in-flight run keeps completing on its own
  already-claimed context even though its spec is gone. Since that context
  no longer includes a timeout once the spec is removed, this path falls
  back to a fixed 1-hour timeout (`cron_exec::FALLBACK_TIMEOUT_SECS`,
  matching `jiji_config`'s own default) rather than failing the run outright.

### Three gotchas confirmed live

All three surfaced only against real hosts during Phase 6+ live validation
(two droplets, real `podman`, real `systemd`) -- none reproducible against
the mock-SSH test suite, since they all depend on `jiji-agent` actually
spawning a container process itself rather than a canned SSH response:

1. **Env/mount paths must be absolutized for the agent, not the CLI's own
   convention.** `jiji-agent` spawns cron containers directly via
   `tokio::process::Command`, never over SSH -- but `stage_env_file`'s and
   `mounts::remote_mount_base`'s `.jiji/{project}/...` paths are
   deliberately *relative*, resolving against an SSH login's home
   directory (see "SSH Connection Management" -- this works for a normal
   deploy because that render happens inside an SSH-executed command).
   `jiji-agent`'s own working directory is `/` (no SSH login at all), so a
   cron run consistently failed with "no such file or directory" until
   `cron_reconcile.rs` started resolving the owner's home directory once
   per reconciliation (`remote_home_dir`, a plain `pwd` over the already-open
   session) and sending `CronSpecApply` absolute paths (`absolutize`/
   `absolutize_mount_args`) instead.
2. **Lease cron addresses from `container_subnet`, never `container_cidr`.**
   `MeshConfig::local_runtime` carries both: `container_subnet` is this
   host's actual bridge subnet (what a container can really be given an
   address from); `container_cidr` is the whole-mesh reserved range
   spanning every project and server, used elsewhere for firewall/NAT
   rules, never for picking a real address. `lease_and_spawn` originally
   allocated from `container_cidr`, and podman rejected every cron
   container start outright ("requested static ip ... not in any subnet
   on network ...") since the leased address was never actually inside the
   bridge's subnet.
3. **`jiji-agent`'s systemd unit needs `KillMode=process`.** Podman here
   runs `cgroup_manager = cgroupfs` (see "Container Engine Provisioning"
   below), so a container's conmon/crun process stays in the launching
   unit's own cgroup rather than escaping into a systemd-delegated scope.
   Under the default `KillMode=control-group`, stopping/restarting the
   agent's own unit (an upgrade, `Restart=on-failure`) `SIGKILL`s that
   whole cgroup -- silently killing every container the agent manages
   along with it, `jiji-proxy` included. `podman ps` kept reporting the
   dead `jiji-proxy` container "Up" since conmon never got the chance to
   record an orderly exit; only a resolver-level probe (`podman exec`, the
   admin socket) caught the drift. `process` mode kills only the tracked
   main PID; already-running containers are untouched and get re-adopted
   by `recover_claimed_runs`/`local_reconcile.rs` on the next start, same
   as any other agent restart.

## `jiji server teardown` (inverse of `server setup`)

`crates/jiji-cli/src/commands/server/teardown.rs` orchestrates the inverse of
everything above, holding a `HostRuntime` lock per targeted host
(`crate::lock::LockScope::HostRuntime`, see "Deployment Locking" below) for
the duration: `jiji-agent` itself first and unconditionally
(`agent_install::remove_agent`) -> proxy routes -> application containers ->
volumes (with `--volumes`) -> images -> the shared jiji-proxy container (only
when no project still has routes) -> disconnecting jiji-proxy from this
project's bridge (`proxy::disconnect_bridge_if_attached`, independent of
whether jiji-proxy is still running for other projects) -> the per-project
staging directory (`env_resolution::project_staging_dir`, holds staged
`.env` files with resolved secrets and uploaded mount content) -> (only once
every application-layer step above succeeded) this project's own network
layer: WireGuard (config/key files *and* the live kernel interface, `ip link
delete` -- see below), any legacy pre-agent nftables table, bridge network,
compiled `/etc/jiji/network/{slug}` subtree only (never a sibling project's
subtree on a shared host). Ownership discovery is by `jiji.managed`/
`jiji.project` labels for containers, and config-computed exact names (never
a glob) for volumes/images/proxy routes. `-S`/`--services` is explicitly
rejected rather than silently ignored. Another project's containers being
present on the same host is surfaced as an informational notice
(`teardown_plan::render_other_project_notices`), not a blocker: teardown
only ever acts on this project's own labeled resources.

`jiji-agent` is removed **first**, before anything it reconciles, rather
than last: confirmed live, leaving it running through the later steps let
its own continuous reconcile loop (`local_reconcile.rs`,
`proxy_bringup.rs`) silently reapply a proxy route and recreate the
jiji-proxy container moments after this same teardown had already reported
both removed: the agent provides no interactive help investigating a
stuck container removal, it only fights every other step's changes, so
there's no reason to keep it alive even if a later step fails. Separately,
`network_teardown::remove_wireguard` deletes the live interface
(`ip link delete`) as well as its config/key files: before Phase 9 the
interface was owned by a `wg-quick@{iface}.service` unit, and stopping that
unit tore the interface down as a side effect; Phase 9 replaced it with the
agent bringing the interface up directly (no systemd unit), which dropped
that implicit cleanup path until this fix -- every `jiji server teardown`
between Phase 9 and this fix leaked a WireGuard interface on every host it
touched.

## SSH Connection Management (detail)

`jiji-ssh` is built on **russh** (pure-Rust async SSH client, no subprocess,
no libssh FFI). `SshSession::execute`/`execute_with_input` enforce
`connect_timeout`/`command_timeout`. `SshPool` (semaphore-based) provides
`execute_concurrent`/`execute_batched`/`execute_with_error_collection` for
running independent SSH operations across many hosts without overloading any
one server. `SshSession` supports stream-backed nested sessions for ProxyJump
and loopback-bound reverse TCP forwarding with explicit cancellation.
`crates/jiji-cli/src/ssh_adapter.rs` resolves supported OpenSSH config fields
and adapts `jiji_config::{NamedServer, Ssh}` into `jiji_ssh::ConnectOptions`.

A remote command killed by a signal never sends `ChannelMsg::ExitStatus` (SSH
sends `exit-signal` instead, which `classify_channel_msg` in `session.rs`
ignores), so `Option<u32>` exit codes must treat `None` the same as a
nonzero exit, never as success. `run_command`'s `success: code == Some(0)`
gets this right; `commands/server/exec.rs`'s streaming/PTY paths originally
didn't (`Some(0) | None => Ok(())`), silently reporting success on a killed
remote command, fixed to bail on `None` with an actionable message.

## Deployment Locking (detail)

`crates/jiji-cli/src/lock.rs` generalizes CLI coordination to match mutation
scope (Phase 7): a mkdir-atomic SSH lock primitive
(pending dir -> `install` an `info.json` -> `mv -T` into place) taken over
`LockScope`, a fixed, deadlock-free rank order:

```
ProjectMaintenance (0) < HostRuntime (1) < ServiceScale (2)
  < LogicalReplica (3) < HostGlobalProxy (4)
```

A command computes its full lock set up front, sorts by `(rank, host, path)`,
and acquires concurrently within a rank, sequentially rank-to-rank -- since
every command's lock set is a subset of one fixed total order, two commands
can never deadlock against each other. `ProjectMaintenance` covers `network
setup`/`backup`/`restore`/`import`/`compact`; `HostRuntime` covers `server
setup`/`teardown`; `ServiceScale` is keyed by service name so unrelated
services scale independently; `LogicalReplica` (keyed by `replica_id`) is
what `deploy`/`service restart`/`rollback`/`remove` actually lock, so an
unrelated offline host or a different replica never blocks a targeted
operation; `HostGlobalProxy` is the one host-global (no project segment)
lock, since jiji-proxy itself is host-global and shared across projects.
`commands/lock/mod.rs::with_locks` acquires/releases over a caller's
already-connected session map instead of opening a dedicated connection
episode per command, and `crate::agent_client`/deploy sessions are reused for
the lock the same way. See the `jiji lock` entry in CLAUDE.md's "Command
Reference" for the CLI surface this backs.

## Naming Conventions (detail)

- **Images**: explicit `image:` references, or versioned references produced by
  `jiji build` / `jiji deploy --build` from each service's `build:` config.
  Static short names are normalized before engine operations:
  `nginx:latest` becomes `docker.io/library/nginx:latest`, and
  `owner/image:tag` becomes `docker.io/owner/image:tag`; references whose
  first path component is `localhost`, contains a dot, or contains a port
  remain unchanged.
- **Logical replicas**: `placement::replica_id(project, service, ordinal)`
  (`crates/jiji-cli/src/placement.rs`), a stable ID surviving redeploy,
  independent of which host currently owns it; deterministic per
  `(project, service, ordinal)`, not derived from a container name.
- **Deployments**: a fresh, random `deployment_id` per container start
  (`deploy_transaction.rs::deploy_dynamic_endpoint`, hashed from project,
  replica ID, a nonce, and the CLI process ID).
- **Containers**: `{project}-{service}-{first 12 hex chars of deployment_id}`
  (`container_runtime::dynamic_container_name`), a new name every deploy, no
  permanent A/B slot names, no rename step.
- **Proxy routes**: identified by `(host, path_prefix)`, not a generated
  name: jiji-proxy's route table is keyed by the config's own `hosts:`/
  `path_prefix:` values (`proxy_routes::RouteTarget`), applied independently
  to every selected server's own jiji-proxy (routes are not shared/synced
  across servers, but jiji-proxy's DNS-driven discovery makes their
  *backends* mesh-wide regardless).
- **DNS records**: `{project}-{service}.jiji` (aggregate) and
  `{project}-{service}-{server}.jiji` (per-replica), served live by each
  host's own `jiji-agent` from its local catalog, not compiled ahead of time.
- **Ownership labels**: `jiji.managed=true jiji.project=<p> jiji.service=<s>
  jiji.server=<srv> jiji.resource=service` on every service container.
- **Per-project network identifiers** (`crates/jiji-network/src/naming.rs`,
  all pure functions of `project:` alone, computed the same way independent
  of whether any other project shares the host, see the jiji-website repo's
  Network Reference page's "Multiple projects on one server" section):
  WireGuard interface `jiji{8 hex}` (`wireguard_interface_name`), kernel
  bridge device `jijib{7 hex}` (`bridge_interface_name`, distinct from the
  logical bridge name below because Linux interface names are capped at 15
  characters), Docker/Podman logical network `jiji-{slug}`
  (`bridge_network_name`), the sole per-project systemd unit
  `jiji-agent-{slug}.service` (`systemd_unit_slug`,
  `jiji-agent/src/paths.rs::unit_name`), WireGuard port `51820..=55819`
  (`wireguard_port`), the agent's catalog/desired-state replication TCP port
  `58000..60000` (`catalog_replication_port`): membership has no listener or
  port of its own, since it's pushed by the CLI, not replicated peer-to-peer.
  All remote state lives under
  `/etc/jiji/network/{slug}/` (mesh bootstrap) and `/etc/jiji/agent/{slug}/`
  (agent binary, durable store, socket; see `AgentPaths::default_for_project`)
  instead of one shared top-level path. `jiji-dns-{slug}.service` /
  `jiji-service-nat-{slug}.service` / `service_nat_table_name` still exist as
  named constants purely so `network setup`/`teardown` can find and remove
  them from a pre-agent installation (see "Private Networking" above). No
  current code path (re)installs them.

## Container Engine Provisioning (detail)

`engine::ensure_engine` (`crates/jiji-cli/src/engine.rs`) is shared by
`jiji server setup` and `builder.remote` preflight. On Debian/Ubuntu, Podman
is installed as a pinned, SHA256-verified static binary
(`mgoltzsche/podman-static` v5.8.4) rather than the distro package, because
those distros don't ship a Podman new enough for current CDI specs (min
version 5.8.4, up from the old distro-packaged 4.9.3 floor); Fedora/RHEL
still get `dnf install podman`. The static install also writes a managed
`/etc/containers/containers.conf.d/99-jiji-static.conf` pinning
`runtime = "/usr/local/bin/crun"` / `cgroup_manager = "cgroupfs"`, and patches
the AppArmor `podman` profile to permit `/usr/local/bin/podman`. Unlike
Docker (which still requires an operator-managed upgrade), an
already-installed Podman below the minimum is upgraded in place:
`reconcile_managed_podman_static_configuration` re-applies the pinned config
on every `ensure_engine` call so a manual Podman upgrade never drifts from
it. Every exec into a container (`container_runtime::exec_prefix`) also
passes Podman `--no-session` (Docker needs no equivalent flag), avoiding the
overhead of a new PAM/login session on every health-check poll and proxy
command.

## Testing: real-hosts-only incidents

The mock-SSH suite (see CLAUDE.md's "Testing" section for the harness
patterns) is necessary but not sufficient. These bugs only ever surfaced
against real hosts, never against `cargo test`:

Docker's `ps --format` `.Labels` being a flat string rather than a map (and
the Podman inverse: `.Label "key"` doesn't exist on Podman's `ps` reporter,
it needs `index .Labels "key"`); the old kamal-proxy fork (retired, see
"jiji-proxy" above) always emitting ANSI color codes even non-interactively;
Podman refusing to push/pull the local loopback registry over plain HTTP
unless `--tls-verify=false` is passed, where Docker trusts `localhost`
registries by default; and `jiji server exec --interactive` hanging forever
after the remote shell exited, because `tokio::io::stdin().read()` was
polled fresh inside a `tokio::select!` loop each iteration: cancelling that
future does not cancel the underlying blocking read, and the Tokio runtime
waits for its blocking pool to drain on shutdown, so a still-blocked stdin
read (input that will never arrive once the remote side is done) hung
process exit indefinitely. Only reproduced against a real allocated PTY (a
captured-pipe test subprocess has no controlling terminal to hang on); fixed
by reading stdin on a dedicated `std::thread` forwarding through an `mpsc`
channel instead. Live-test CLI-command-rendering work against a real Docker
(and ideally Podman) host, and interactive/PTY work against a real terminal,
before considering it done: `cargo test` passing is not sufficient evidence
for anything that shells out to `docker`/`podman`/`jiji-proxy`/`systemctl`/
`nft`, or that drives a local TTY.

Three more real-hosts-only bugs from Phase 9's live validation, none of which
any mock test could have caught: `jiji_network::proxy_script::
render_nftables`'s ingress DNAT rules matched on destination port alone,
with no destination-address restriction -- one chain (`output`) silently
hijacked the host's own outbound HTTPS/apt traffic, found only after a real
reboot broke package installs; the other (`prerouting`) hijacked genuine
cross-host container-to-container mesh traffic on the same ports, found via
`tcpdump` on a live cluster (fixed by scoping both to `ip daddr
{public_host}` and removing `output` entirely). Separately, the old
kamal-proxy fork's own network namespace kept an ARP cache independent of
the host's, so a dynamically-leased address reused by a fresh container
could sit `STALE` (old MAC) for tens of seconds before re-resolving,
surfacing as "no route to host" on a concurrent deploy -- only reproducible
against two real, concurrently-deploying hosts, never against the mock SSH
harness; resolved as a side effect of the jiji-proxy cutover, since
jiji-proxy resolves backends by DNS on its own schedule rather than caching
ARP entries for explicitly pushed target addresses.

Three more real-hosts-only bugs, this time from the kamal-proxy -> jiji-proxy
cutover's own live validation, again none catchable by the mock-SSH suite
since they all depend on `jiji-agent`'s systemd unit and reconcile loop
actually running: the jiji-proxy Dockerfile's `CMD` still referenced its
pre-phase-3 flat-flags invocation (`["--config", "path"]`) after `main.rs`
was restructured into `run`/`route` subcommands, so the built image would
never have actually started the daemon; `jiji-agent`'s own continuous
reconcile loop (`local_reconcile.rs`, `proxy_bringup.rs`) stayed alive
through most of `jiji server teardown` (the agent was removed last, after
the proxy/network-layer steps) and silently reapplied a proxy route and
recreated the jiji-proxy container moments after the same teardown run had
already reported both removed -- fixed by stopping the agent first,
unconditionally, before any step it reconciles; and
`network_teardown::remove_wireguard` only ever deleted the WireGuard config
and key files, never the live kernel interface, because before Phase 9 the
interface was owned by a `wg-quick@{iface}.service` unit whose `stop` tore
the interface down as a side effect, and Phase 9's move to the agent
bringing the interface up directly dropped that implicit cleanup without
anything replacing it -- every `jiji server teardown` between Phase 9 and
this fix leaked a WireGuard interface on every host it touched, only
observable by inspecting `ip link` on a real host after a real teardown.
