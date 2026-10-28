# jiji-agent

Traps specific to this crate. Root `AGENTS.md` rules still apply.

- **The systemd unit needs `KillMode=process`.** Podman runs with cgroupfs, so
  conmon/crun stay in the unit's cgroup; `control-group` kills every managed
  container, `jiji-proxy` included, on any agent restart.
- **The agent spawns containers directly, never over SSH.** Its cwd is `/`, so
  `.jiji/...`-relative paths from SSH helpers do not resolve; send absolute
  paths. Cron container addresses come from
  `mesh_config.local_runtime.container_subnet`, never `container_cidr`.
- **Absence is not deletion.** A missing peer, record, or container never
  deletes durable state; only an explicit tombstone does.
- **Replication is direct-only.** Never relay a catalog or desired-state record
  through a third node. Inbound records are authenticated by the TCP source
  address against local membership. Membership itself is CLI-pushed, never
  replicated.
- **DNS publishes only `active` + `healthy` records.**
- **Respect `jiji.lease-owned`.** `recover_labeled_leases` must skip
  `jiji.lease-owned=false` (host-mode and `service:<name>` containers) and keep
  the unconditional recovery path for containers with no label at all.
- **Cron specs and run history are local.** Never add them to replication.
- **Schema changes go through `store.rs` migrations.** Reopening an existing
  store must preserve data and never re-run a migration.
