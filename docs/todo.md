# Follow-up Work

This file lists confirmed gaps in the current code.

## Current gaps

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
