# Testing

What each test layer covers and how to write tests for it. The constraints
mock tests cannot cover are in
`docs/architecture-notes.md#testing-boundaries`.

## Test layers

- **Pure logic** (command rendering, naming rules, config-derived
  candidates): `#[cfg(test)] mod tests` in the same file, no SSH.
- **Mock SSH**: a real in-process SSH server built on russh's `server`
  module. See `TestServer`/`spawn_test_server`/`CannedResponse` in
  `crates/jiji-cli/tests/server_setup_test.rs`, `deploy_test.rs`,
  `server_teardown_test.rs`: a `HashMap<String, CannedResponse>` of exact
  command strings to canned exit code/stdout/stderr, defaulting unmatched
  commands to success. Tests run the compiled `jiji` binary as a subprocess
  and assert on exit status, output, and, for ordering, a shared
  `received: Arc<Mutex<Vec<String>>>` command log. No mock-object framework.
- **Local engine**: a fake `docker`/`podman` first on `PATH` that logs argv
  and stdin (`registry_teardown_test.rs`, `registry_auth_test.rs`). Use this
  when the command never leaves the local machine.
- **Local HTTP**: a minimal in-process `TcpListener` HTTP/1.1 stand-in for
  commands that talk to an HTTP service (`update_test.rs`).
- **jiji-agent**: no SSH or CLI. Replication tests run two or more real
  `AgentStore`s over loopback `TcpListener`/`TcpStream`, binding the
  outbound side to a distinct loopback address (e.g. `127.0.0.2`) so
  `RecordProvenance::Peer`'s source-address check has something real to
  authenticate, and call `sync_once`/`serve` directly. Membership tests call
  `MembershipView::apply` on plain records.
- **Docker-in-Docker** (`mise test-docker`): see below.

The mock-SSH suite is necessary but not sufficient. Live-test command
rendering against a real Docker host and, when possible, Podman. Test
interactive and PTY work in a real terminal.

## Docker-in-Docker suite

`crates/jiji-cli/tests/docker_*_test.rs` run the real `jiji` binary over real
SSH against `vm1` (and `vm2` for multi-host tests): privileged containers
from `test/docker/compose.yml` that boot real systemd. Setup really installs
Podman via the static-binary path, the agent unit really runs under systemd,
WireGuard really comes up against the host kernel module, and traffic really
flows through jiji-proxy's nftables DNAT. Modeled on how basecamp/kamal's CI
runs privileged nested-Docker containers as fake hosts; nested KVM was
rejected because `/dev/kvm` on GitHub-hosted runners is unreliable.

- Gated behind `JIJI_DOCKER_TESTS=1` (`docker_support::skip_unless_enabled()`),
  so `mise test` stays fast and dependency-free.
- Every docker test binary is in nextest's `docker-integration` group
  (`max-threads = 1`, `.config/nextest.toml`): concurrent `server setup`
  runs on the same OS race on the `apt`/`dpkg` lock.
- The vm image (`test/docker/vm/`) is deliberately minimal: openssh-server,
  systemd, `iproute2`, `iputils-ping` (setup's WireGuard peer check shells
  out to `ping`). It does not pre-install Podman, wireguard-tools, nftables,
  or dnsutils; setup must install them.
- `test/docker/sshkeys/` generates one shared keypair into
  `test/docker/.shared/` (gitignored), read by both hosts and
  `crates/jiji-cli/tests/docker_support/mod.rs`.
- vm1 and vm2 reach each other over a dedicated `mesh` compose network with
  static IPs (`172.30.0.11`/`.12`); WireGuard needs both directions to dial
  out, which the `127.0.0.1:2201` published port cannot provide.
- Podman's storage root (`/var/lib/containers`) is a named Docker volume:
  native overlay nested on vm1's own overlay fails. If that ever proves
  insufficient, the fallback is a CI-only `storage.conf` written by vm1's
  boot script before setup, never a change to jiji's install code.
- `jiji-proxy` binds host ports 80/443 once setup has run, whether or not
  any service configures `proxy:`; `network_mode: host` test services must
  use another port.

### Coverage

| Test | Proves |
|---|---|
| `docker_server_lifecycle_test.rs` | setup, then real `podman info`/`systemctl is-active`/`wg show`, then teardown |
| `docker_deploy_test.rs` | `nginx:alpine` HTTP-proxied deploy serves a real request through jiji-proxy |
| `docker_build_deploy_test.rs` | `deploy --build` through jiji's own local registry and reverse SSH tunnel, then `registry teardown` |
| `docker_rolling_deploy_test.rs` | a never-healthy candidate leaves the active container ID unchanged and no stray container |
| `docker_mesh_deploy_test.rs` | two hosts: WireGuard handshake, cross-host catalog/DNS replication, cross-host L3 reachability |
| `docker_load_balancer_test.rs` | jiji-proxy alternates backends across hosts (more than one distinct body in 40 requests) |
| `docker_scale_test.rs` | `scale: 2` alternates across same-host replicas; `service scale 1` removes the retired container |
| `docker_network_mode_test.rs` | `network_mode: host` and a `service:<name>` dependent land in the right namespace (`SandboxKey` match) |
| `docker_cron_test.rs` | cron `list`/`run`/`logs` on the owning agent (logs polled; `--follow` loses the race with `echo`) |
| `docker_cron_ownership_transfer_test.rs` | dropping vm1 from a service's `servers:` moves cron ownership to vm2 and sweeps vm1's stale spec |
| `docker_image_retention_test.rs` | `retain: 2` over 4 builds with distinct Dockerfiles leaves exactly the 2 newest tags |
| `docker_stop_first_test.rs` | a host-port redeploy fails without `stop_first` (previous container untouched) and succeeds with it |
| `docker_service_restart_rollback_test.rs` | `service rollback --version v1` and `service restart` replace the container with the right image |

An unchanged Dockerfile builds the same image ID regardless of `--version`,
so tests that need distinct images must change the Dockerfile.

### Not yet covered

Ubuntu 26.04's AppArmor local-include path (the base image has no packaged
`wg`/`wg-quick` profiles), `builder.remote`, `jiji network
backup/restore/recover`, jiji-proxy TLS/ACME and raw TCP relay,
and `jiji server setup --rotate-key`/`--import`.
Do not assume broader real-host coverage than the table above.
