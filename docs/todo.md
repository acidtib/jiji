# Follow-up Work

This file lists confirmed gaps in the current code.

## Current gaps

### Keep public documentation within implemented behavior

The public website must not present schema-only or deferred behavior as an
available feature. Keep these limits visible until their runtime work is
complete:

- `network_mode: host` and `network_mode: none` do not change container
  networking. Implement or reject them as described below.
- External `secrets:` adapters do not resolve values. Implement the adapter
  path described below.
- `service.retain` does not prune images during deployment. Operators must run
  `jiji service prune`.
- Deploy progress does not show output from failed health-check attempts. It
  shows the useful output only in the final error.
- Cron jobs do not retry, transfer ownership during an outage, or run once per
  replica.
- Audit coverage is not yet complete for every state-changing command. The
  current trail covers deploys, service lifecycle operations, scaling, pruning,
  server setup and teardown, and manual lock changes.

When a release implements one of these items, update the website and this file
in the same change.

Sources:

- `crates/jiji-config/src/jiji.yml`
- `crates/jiji-cli/src/container_runtime.rs`
- `crates/jiji-cli/src/commands/service/prune.rs`
- `crates/jiji-cli/src/health_check.rs`
- `crates/jiji-agent/src/scheduler.rs`
- `crates/jiji-cli/src/audit.rs`

### Complete audit coverage

The CLI describes the audit trail as a record of every state-changing command,
but several commands do not write an audit entry yet. These include network,
registry, and proxy mutations.

- List every command that can change local or remote state.
- Add a success or failure entry for each command.
- Keep audit writes best-effort so an audit failure does not hide the command
  result.
- Use stable action names and include the affected lock scope when applicable.
- Add tests that compare the state-changing command surface with audit coverage.

Sources:

- `crates/jiji-cli/src/audit.rs`
- `crates/jiji-cli/src/commands/network/`
- `crates/jiji-cli/src/commands/registry/`
- `crates/jiji-cli/src/commands/proxy/`

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

### Add mounted build secrets

Jiji resolves build arguments, but build arguments are not safe for secret
values. Docker and Podman support temporary secret mounts for build steps.

- Add `services.<name>.build.secrets` to the configuration schema.
- Resolve each secret from the selected `.env` file or the host environment.
- Pass each value with the engine's `--secret` option.
- Keep secret values out of command arguments, logs, errors, and image metadata.
- Support local builders, remote builders, and multi-architecture builds.
- Remove staged secret files after success, failure, cancellation, and timeout.
- Document the required Dockerfile `RUN --mount=type=secret` syntax.
- Add tests for Docker, Podman, missing values, redaction, and cleanup.

Sources:

- `crates/jiji-config/src/schema.rs`
- `crates/jiji-config/src/jiji.yml`
- `crates/jiji-cli/src/build_engine.rs`
- `crates/jiji-cli/src/build_executor.rs`
- `crates/jiji-cli/src/remote_build.rs`
- `crates/jiji-cli/src/env_resolution.rs`

### Default the build context to the project root

The detailed `build:` form currently requires `context`. Configuration loading
fails before any command can inspect a service when this field is absent.

This configuration must be valid:

```yaml
services:
  site:
    build:
      dockerfile: Dockerfile
```

It must behave like this explicit configuration:

```yaml
services:
  site:
    build:
      context: .
      dockerfile: Dockerfile
```

- Default `BuildConfig.context` to `.` during deserialization.
- Keep an explicit `context` value unchanged.
- Apply the same path rules to local and remote builders.
- Update the configuration reference with the optional field and its default.
- Add parsing, validation, build-plan, and CLI regression tests.
- Make sure that `jiji secrets print` accepts the shorthand configuration.

Sources:

- `crates/jiji-config/src/schema.rs`
- `crates/jiji-config/src/jiji.yml`
- `crates/jiji-cli/src/build_engine.rs`
- `crates/jiji-cli/src/build_context.rs`
- `crates/jiji-cli/src/commands/secrets/print.rs`

### Enforce image retention automatically

`service.retain` is only enforced by `jiji service prune`. Old images can
accumulate when an operator does not run this command. The configuration
reference incorrectly says that deployment prunes images automatically.

- Define how the agent receives each service image repository and retain
  count.
- Add safe pruning to the agent reconciliation loop.
- Keep images that a container still references.
- Decide whether `jiji service prune` remains as a manual override.
- Correct the configuration reference when the runtime behavior is final.
- Add tests for ordering, retained images, and images in use.

Sources:

- `crates/jiji-cli/src/commands/service/prune.rs`
- `crates/jiji-agent/src/local_reconcile.rs`
- `crates/jiji-agent/src/runtime.rs`
- `crates/jiji-config/src/jiji.yml`

### Show health-check failures while a deploy waits

The deploy UI shows one static message until the health check succeeds or
times out. Failed attempts discard stdout. Stderr appears only in the final
error.

- Capture stdout and stderr from every failed attempt.
- Send the latest useful output through the existing deployment progress
  callback.
- Keep the final container-log tail in timeout errors.
- Prevent secret values from appearing in progress output.
- Add tests for stdout, stderr, repeated attempts, and timeout output.

Sources:

- `crates/jiji-cli/src/health_check.rs`
- `crates/jiji-cli/src/deploy_transaction.rs`

### Implement or reject `network_mode: host` and `network_mode: none`

The configuration reference documents both modes. The runtime currently
starts these services with normal project bridge networking instead.

- Implement both modes with explicit container-engine arguments, or reject
  them during configuration validation.
- Define address leasing, DNS, port, proxy, health-check, cron, and scaling
  rules for each implemented mode.
- Add validation and deployment tests for the selected behavior.
- Update the configuration reference to match the implementation.

Sources:

- `crates/jiji-config/src/jiji.yml`
- `crates/jiji-config/src/validation.rs`
- `crates/jiji-cli/src/container_runtime.rs`
- `crates/jiji-cli/src/deploy_transaction.rs`

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

## Removed stale items

The Kamal proxy health-check bug and target-address activation race do not
apply to the current proxy architecture. Jiji-proxy now resolves aggregate
service DNS records and does not receive a mutable backend address during
route activation.
