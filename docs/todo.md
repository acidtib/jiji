# Follow-up Work

This file lists confirmed gaps in the current code.

## Current gaps

### Keep public documentation within implemented behavior

The public website must not present schema-only or deferred behavior as an
available feature. Keep these limits visible until their runtime work is
complete:

- External `secrets:` adapters do not resolve values. Implement the adapter
  path described below.
- Cron jobs do not retry, transfer ownership during an outage, or run once per
  replica.

When a release implements one of these items, update the website and this file
in the same change.

Sources:

- `crates/jiji-config/src/jiji.yml`
- `crates/jiji-cli/src/commands/service/prune.rs`
- `crates/jiji-agent/src/scheduler.rs`

### Implement external secret adapters

The `secrets:` configuration parses, but no runtime path reads
`Config.secrets`. A configured adapter is currently a silent no-op.

- Add a small adapter interface and an adapter factory.
- Add Doppler as the first implementation.
- Resolve adapter values on the local machine, once per command.
- Use this precedence: `.env`, adapter, then host environment when
  `--host-env` is set.
- Use adapter values for service secrets.
- Decide whether `builder.registry.password` also uses adapter values.
- Show the selected source in `jiji secrets print` without exposing values by
  default.
- Reject unknown adapter names and report missing local tools clearly.
- Test the adapter with a fake executable on `PATH`.
- Test precedence and combined missing-secret errors.

Sources:

- `crates/jiji-config/src/schema.rs`
- `crates/jiji-config/src/jiji.yml`
- `crates/jiji-cli/src/env_resolution.rs`
- `crates/jiji-cli/src/commands/secrets/print.rs`
- `crates/jiji-cli/src/registry.rs`

### Increase project capacity

The current limits are 32 servers, 500 services, and 2,000 logical replicas
per project. Use these initial targets for the next capacity increase:

- Support 64 servers per project.
- Use a `/15` project container range.
- Keep one `/21` container subnet for each server.
- Support 10,000 logical replicas per project.
- Keep a limit of 2,000 replicas for each service.
- Reject placement that exceeds the address capacity of a server.

Complete these measurements before you increase the limits:

- Measure WireGuard configuration and repair with 64 nodes.
- Measure direct catalog replication between 64 nodes.
- Measure catalog frame sizes with the maximum records on one node.
- Measure aggregate DNS response sizes with 2,000 service replicas.
- Measure placement, deployment fan-out, and agent store usage with 10,000 replicas.
- Define migration behavior for existing `/16` project ranges.
- Define collision behavior for the 32 available `/15` ranges in `100.64.0.0/10`.

Sources:

- `crates/jiji-config/src/validation.rs`
- `crates/jiji-network/src/planner.rs`
- `crates/jiji-agent/src/catalog_replication.rs`
- `crates/jiji-agent/src/dns.rs`
- `crates/jiji-agent/src/leases.rs`
- `crates/jiji-cli/src/placement.rs`
- `crates/jiji-cli/src/commands/service/scale.rs`

## Deferred cron features

These items are feature additions, not defects in the current cron contract.

- Add a configurable retry policy and record retry runs with a distinct
  cause.
- Add an optional per-replica execution mode and make cron commands
  replica-aware.
- Add automatic owner failover when the current owner is unavailable.
- Make run metadata and container retention configurable per job.

Sources:

- `crates/jiji-config/src/schema.rs`
- `crates/jiji-cli/src/cron_reconcile.rs`
- `crates/jiji-agent/src/cron.rs`
- `crates/jiji-agent/src/cron_exec.rs`
- `crates/jiji-agent/src/scheduler.rs`
