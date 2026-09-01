//! Shared helpers for the Docker-in-Docker integration suite (`docker_*_test.rs`). These tests
//! run the real `jiji` binary over real SSH against `vm1`, a privileged container running its
//! own systemd and (once `jiji server setup` installs it) its own Podman -- see
//! `test/docker/compose.yml`. Nothing here mocks SSH; `crates/jiji-cli/tests/server_setup_test.rs`
//! is the in-process mock-SSH suite, this is its real-host counterpart.
//!
//! The compose stack itself is orchestrated externally (`.mise/tasks/test-docker`, or the
//! `docker-integration.yml` workflow), not by this module: these helpers assume `vm1` is already
//! up and healthy by the time a test calls them.

#![allow(dead_code)]

use std::env;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub const VM1_SSH_PORT: u16 = 2201;

/// Returns `true` (and prints a skip message) unless `JIJI_DOCKER_TESTS=1` is set. Call at the
/// top of every test in this suite and `return` early on `true`, so plain `cargo nextest run
/// --workspace` / `mise test` stays fast and dependency-free, matching the rest of the suite's
/// default behavior.
pub fn skip_unless_enabled() -> bool {
    if env::var("JIJI_DOCKER_TESTS").as_deref() == Ok("1") {
        false
    } else {
        eprintln!(
            "skipping docker integration test: set JIJI_DOCKER_TESTS=1 with `test/docker/compose.yml` up (see `mise test-docker`)"
        );
        true
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve workspace root")
}

fn compose_file() -> PathBuf {
    workspace_root().join("test/docker/compose.yml")
}

/// Path to the private half of the keypair `test/docker/sshkeys` generates into the shared bind
/// mount. Only readable once the `sshkeys` service has actually started (compose brings it up
/// before `vm1` via `depends_on: condition: service_healthy`).
pub fn ssh_key_path() -> PathBuf {
    workspace_root().join("test/docker/.shared/ssh/id_ed25519")
}

fn compose(args: &[&str]) -> Output {
    Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(compose_file())
        .args(args)
        .output()
        .expect("run docker compose")
}

/// Runs a command inside the named compose service (`vm1`, `vm2`) via `docker compose exec`, for
/// asserting real post-`jiji`-run state (`systemctl is-active ...`, `wg show ...`, `podman info`,
/// `dig ...`, ...).
pub fn exec_service(service: &str, cmd: &[&str]) -> Output {
    let mut args = vec!["exec", "-T", service];
    args.extend_from_slice(cmd);
    compose(&args)
}

pub fn exec_service_ok(service: &str, cmd: &[&str]) -> bool {
    exec_service(service, cmd).status.success()
}

pub fn exec_service_stdout(service: &str, cmd: &[&str]) -> String {
    let output = exec_service(service, cmd);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Same as `exec_service`, but pipes `input` to the command's stdin -- needed to talk to
/// `jiji-agent`'s own control socket directly (`cron_spec_names_on`), which reads one JSON
/// request from stdin per `jiji-agent request --socket ...` invocation.
pub fn exec_service_with_stdin(service: &str, cmd: &[&str], input: &[u8]) -> Output {
    use std::io::Write;
    use std::process::Stdio;

    let mut args = vec!["exec", "-T", service];
    args.extend_from_slice(cmd);
    let mut child = Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(compose_file())
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn docker compose exec");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input)
        .expect("write request to child stdin");
    child
        .wait_with_output()
        .expect("wait for docker compose exec")
}

/// Every cron spec a host's own `jiji-agent` genuinely has installed for `project`, queried
/// directly through the agent's control socket (`jiji-agent request --socket ...`, the same
/// `RequestBody::CronSpecList` mechanism `jiji-cli`'s own `agent_client::call` uses) rather than
/// through `jiji service cron list/status`, which only ever show the *current* owner's state --
/// this can see a stale spec left behind on a former owner that those commands would never
/// surface.
pub fn cron_spec_names_on(service_container: &str, project: &str) -> Vec<String> {
    let paths = jiji_agent::AgentPaths::default_for_project(project);
    let request = jiji_agent::api::Request {
        idempotency_key: None,
        body: jiji_agent::api::RequestBody::CronSpecList,
    };
    let input = serde_json::to_vec(&request).expect("serialize CronSpecList request");
    let command = format!(
        "{} request --socket {}",
        paths.binary_path.display(),
        paths.socket_path.display()
    );
    let output = exec_service_with_stdin(service_container, &["sh", "-c", &command], &input);
    assert!(
        output.status.success(),
        "jiji-agent request --socket ... failed on {service_container}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: jiji_agent::api::ApiResult =
        serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
            panic!("could not parse agent response on {service_container}: {error}\nraw: {stdout}")
        });
    match result.expect("agent rejected CronSpecList request") {
        jiji_agent::api::ResponseBody::CronSpecs { specs } => specs
            .into_iter()
            .filter(|spec| spec.project == project)
            .map(|spec| spec.cron_name)
            .collect(),
        other => panic!("expected CronSpecs, got: {other:?}"),
    }
}

pub fn exec_vm1(cmd: &[&str]) -> Output {
    exec_service("vm1", cmd)
}

pub fn exec_vm1_ok(cmd: &[&str]) -> bool {
    exec_service_ok("vm1", cmd)
}

pub fn exec_vm1_stdout(cmd: &[&str]) -> String {
    exec_service_stdout("vm1", cmd)
}

/// IDs of every container on vm1 labeled `jiji.service={service}` (`container_runtime.rs`'s
/// `jiji.service=` label), running or not. Used to assert a failed candidate deploy leaves no
/// stray container behind (`deploy_transaction.rs::release_candidate` removes it outright, it
/// doesn't just stop it) and that the previously active container's ID is unchanged across a
/// failed redeploy.
pub fn all_container_ids_for_service(service: &str) -> Vec<String> {
    let label_filter = format!("label=jiji.service={service}");
    exec_vm1_stdout(&[
        "podman",
        "ps",
        "-a",
        "--filter",
        label_filter.as_str(),
        "--format",
        "{{.ID}}",
    ])
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty())
    .map(str::to_string)
    .collect()
}

/// The single running container ID for `jiji.service={service}` on vm1 -- panics if there isn't
/// exactly one, since every network-mode scenario in this suite deploys with the default `scale:
/// 1` and expects precisely one container per service.
pub fn the_container_id_for_service(service: &str) -> String {
    let ids = all_container_ids_for_service(service);
    match ids.as_slice() {
        [id] => id.clone(),
        other => panic!("expected exactly one container for service '{service}', found: {other:?}"),
    }
}

/// `podman inspect <container> --format '{{format}}'` on vm1, trimmed. `format` uses Go template
/// syntax (`.HostConfig.NetworkMode`, `.NetworkSettings.SandboxKey`, ...).
pub fn podman_inspect(container_id: &str, format: &str) -> String {
    exec_vm1_stdout(&[
        "podman",
        "inspect",
        container_id,
        "--format",
        &format!("{{{{{format}}}}}"),
    ])
    .trim()
    .to_string()
}

/// Polls a compose service over `docker compose exec` until it accepts a trivial command, since
/// `depends_on: condition: service_healthy` only proves compose brought the container up, not
/// that systemd has finished booting sshd inside it.
pub fn wait_for_service_ready(service: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if exec_service_ok(service, &["true"]) {
            return;
        }
        if Instant::now() >= deadline {
            panic!("{service} did not become ready within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub fn wait_for_vm1_ready(timeout: Duration) {
    wait_for_service_ready("vm1", timeout);
}

/// Writes a `{dir}/.jiji/deploy.yml` fixture (the real layout `jiji init` itself writes, not a
/// bare `deploy.yml` at `dir`'s root) pointing at the real `vm1` container over its published SSH
/// port, using the shared test keypair and `root` (matching how jiji actually connects to a
/// fresh, unprovisioned host: AGENTS.md's "a host's trust boundary is 'this file was installed by
/// root'"). `builder.engine: podman` drives `jiji server setup` to install real static Podman on
/// vm1, the same install path a real Ubuntu droplet goes through.
pub fn write_config(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services: {{}}
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

pub fn run_jiji(config_path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jiji"))
        .args(args)
        .arg("-c")
        .arg(config_path)
        .output()
        .expect("run jiji")
}

/// Host header the proxied fixture service routes on. No real DNS involved: tests reach vm1's
/// published proxy port directly and set this as the `Host` header themselves, the same way
/// Kamal's own integration suite constructs requests against its load balancer.
pub const PROXY_TEST_HOST: &str = "app.jiji.test";
pub const PROXY_HTTP_PORT: u16 = 8080;

/// Same as `write_config`, plus one service (a stock `nginx:alpine`, not `build:` -- see
/// `write_config_with_build_service` for that) proxied over HTTP on `PROXY_TEST_HOST`,
/// health-checked at `/` before `jiji deploy` will ever route traffic to it.
pub fn write_config_with_proxied_web_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: [vm1]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = PROXY_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Distinct from `PROXY_TEST_HOST`/`BUILD_PROXY_TEST_HOST`: gives the rolling-deploy fixture its
/// own Host header so it never depends on another test's route already being cleared.
pub const ROLLING_TEST_HOST: &str = "rolling.jiji.test";

/// Same fixture service both times (same project, same `dir`, overwriting `{dir}/.jiji/
/// deploy.yml`) so a second call models a real redeploy against an already-Active/Healthy
/// service, not a fresh one. `healthy: false` points the healthcheck at a path nginx will always
/// 404 on, with a short `deploy_timeout` so the health-gated deploy transaction fails fast and
/// deterministically -- proving the "previous container keeps serving, only the candidate is torn
/// down" invariant doesn't require literally racing a container kill mid-deploy.
pub fn write_rolling_deploy_config(dir: &Path, project: &str, healthy: bool) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    let healthcheck_path = if healthy { "/" } else { "/definitely-missing" };
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: [vm1]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: {healthcheck_path}
        interval: 1s
        deploy_timeout: 5s
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = ROLLING_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Content the `docker_build_deploy_test` fixture's image bakes into its own index page, so a
/// passing HTTP assertion proves the image was actually rebuilt and pulled through, not that a
/// stale/cached image was already sitting there.
pub const BUILD_TEST_MARKER: &str = "jiji-docker-build-test";

/// Distinct from `PROXY_TEST_HOST`: this test runs under a different project name but on the
/// same shared, host-global jiji-proxy, so giving it its own Host header avoids depending on
/// `docker_deploy_test`'s own teardown having already cleared its route by the time this runs.
pub const BUILD_PROXY_TEST_HOST: &str = "build.jiji.test";

/// A service built from a real local Dockerfile and pushed through jiji's own build pipeline: no
/// `registry:` block, so `Registry::is_local()` is true and jiji manages its own local registry
/// container, builds and pushes to it, then opens a reverse SSH tunnel to vm1 during `jiji deploy
/// --build` so vm1's Podman can pull from `localhost:{port}` as if the registry were local to it
/// (see `crates/jiji-cli/src/registry.rs` and `commands/deploy.rs`'s `start_reverse_forward`
/// call). Config lives at `{dir}/.jiji/deploy.yml`, not `{dir}/deploy.yml`: `build: ./app` resolves
/// relative to the project root, which jiji derives as two directories above the config file
/// (`env_resolution::project_root_from_config_path`) -- that only lines up with `{dir}/app` when
/// the config actually sits under a `.jiji/` subdirectory, the real-world layout `jiji init`
/// itself writes.
pub fn write_config_with_build_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");

    let app_dir = dir.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("Dockerfile"),
        format!(
            "FROM nginx:alpine\nRUN echo '{BUILD_TEST_MARKER}' > /usr/share/nginx/html/index.html\n"
        ),
    )
    .expect("write test app Dockerfile");

    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    build: ./app
    servers: [vm1]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = BUILD_PROXY_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// A bare-bones HTTP/1.0 GET over a raw `TcpStream`, since `jiji-cli`'s `reqwest` dependency
/// isn't built with the `blocking` feature and pulling in a whole async runtime just to check one
/// status code isn't worth it here. Returns `(status_code, body)`.
fn http_get(port: u16, host_header: &str, path: &str) -> std::io::Result<(u16, String)> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    let status_line = response
        .lines()
        .next()
        .ok_or_else(|| std::io::Error::other("empty HTTP response"))?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other(format!("unparseable status line: {status_line}")))?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status_code, body))
}

/// Polls `PROXY_HTTP_PORT` until it returns HTTP 200, since a successful `jiji deploy` still
/// leaves a short window before jiji-proxy's route is actually reachable end-to-end.
pub fn wait_for_http_ok(host_header: &str, path: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let outcome = http_get(PROXY_HTTP_PORT, host_header, path);
        if let Ok((200, body)) = outcome {
            return body;
        }
        if Instant::now() >= deadline {
            let reason = match outcome {
                Ok((status, _)) => format!("got HTTP {status}"),
                Err(err) => err.to_string(),
            };
            panic!(
                "http://{host_header}{path} via 127.0.0.1:{PROXY_HTTP_PORT} did not return 200 within {timeout:?}: {reason}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Repeats a GET through the proxy, pausing briefly between each, until at least
/// `expected_min_distinct` distinct 200 response bodies have been seen or `timeout` elapses,
/// whichever comes first. Used to prove genuine backend alternation, not just that a route is
/// reachable: `docker_load_balancer_test.rs`'s/`docker_scale_test.rs`'s fixture serves each
/// replica's own hostname as its whole body, so more than one distinct value means jiji-proxy
/// actually picked different backends, not just one. A short fixed-count sample can race two
/// separate convergence delays and see only one backend (confirmed live, not a one-off): a
/// route's own `RouteApply` runs a synchronous re-resolve, but that resolve itself can still
/// return a stale DNS answer if cross-host catalog anti-entropy hasn't reached the host serving
/// jiji-proxy's DNS lookups yet (the same class of race `wait_for_dns_replica_count` closes for
/// direct DNS queries); separately, `refresh_interval_secs` (default 5s) means jiji-proxy's own
/// periodic re-resolution can lag a newly Active replica by several seconds even once catalog
/// replication has caught up. Polling with a generous timeout gives both room to settle instead
/// of asserting on whatever a fixed short window happened to catch.
pub fn distinct_response_bodies(
    host_header: &str,
    path: &str,
    expected_min_distinct: usize,
    timeout: Duration,
) -> std::collections::HashSet<String> {
    let deadline = Instant::now() + timeout;
    let mut bodies = std::collections::HashSet::new();
    loop {
        if let Ok((200, body)) = http_get(PROXY_HTTP_PORT, host_header, path) {
            bodies.insert(body);
        }
        if bodies.len() >= expected_min_distinct || Instant::now() >= deadline {
            return bodies;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Static IPs on the `mesh` compose network (`test/docker/compose.yml`), reachable both from the
/// test runner (a Linux Docker bridge is directly routable from the host, no published port
/// needed) and from each other -- unlike vm1's `127.0.0.1:2201` published-port address, which
/// only vm1 itself can be dialed on. WireGuard needs both directions to work.
pub const MESH_VM1_HOST: &str = "172.30.0.11";
pub const MESH_VM2_HOST: &str = "172.30.0.12";

pub const MESH_TEST_HOST: &str = "mesh.jiji.test";

/// Two-host fixture: `vm1` and `vm2` both in `servers:`, one replica of the same service on each
/// (`servers: [vm1, vm2]`), addressed by their `mesh` network IPs rather than vm1's usual
/// published-port address.
pub fn write_config_with_two_host_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: {vm1_host}
    port: 22
    keys:
      - {key_path}
  vm2:
    host: {vm2_host}
    port: 22
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: [vm1, vm2]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            vm1_host = MESH_VM1_HOST,
            vm2_host = MESH_VM2_HOST,
            key_path = ssh_key_path().display(),
            proxy_host = MESH_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// The per-server reserved DNS address `jiji-agent` binds its `.jiji` resolver to
/// (`jiji_network::planner::ServerPlan::dns_address`), computed the same deterministic way jiji
/// itself does rather than guessed or discovered by inspecting a running host.
pub fn dns_address_for(config_path: &Path, server: &str) -> std::net::Ipv4Addr {
    let config = jiji_config::load_from_file(config_path).expect("load test config");
    let plan = jiji_network::NetworkPlanner::new()
        .plan(&config)
        .expect("compute network plan");
    plan.servers
        .get(server)
        .unwrap_or_else(|| panic!("server '{server}' not present in the computed network plan"))
        .dns_address
}

/// Polls `dig +short` against `dns_addr` for `name` on `service` until it returns at least
/// `expected` distinct addresses, since cross-host catalog/DNS anti-entropy (AGENTS.md's
/// "continuous direct-only peer-to-peer anti-entropy") converges asynchronously after `deploy`
/// succeeds: a single query right after a successful deploy can race a replica's record not
/// having propagated to this host's own local catalog yet (confirmed live -- a real flake, not a
/// mock-suite-only concern). Panics with the last `dig` output seen on timeout.
pub fn wait_for_dns_replica_count(
    service: &str,
    dns_addr: Ipv4Addr,
    name: &str,
    expected: usize,
    timeout: Duration,
) -> Vec<Ipv4Addr> {
    let deadline = Instant::now() + timeout;
    loop {
        let dig_output = exec_service_stdout(
            service,
            &[
                "dig",
                "+short",
                "+time=5",
                "+tries=3",
                &format!("@{dns_addr}"),
                name,
            ],
        );
        let addresses: Vec<Ipv4Addr> = dig_output
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect();
        if addresses.len() >= expected {
            return addresses;
        }
        if Instant::now() >= deadline {
            panic!(
                "expected {service}'s own DNS resolver to know at least {expected} replicas for '{name}' within {timeout:?}, got: {dig_output:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Single-host fixture exercising both non-bridge `network_mode` shapes at once: `webhost`
/// (`network_mode: host`, sharing vm1's own network namespace directly -- no `proxy:`/`-p`,
/// validation rejects both for `host` mode) and `dependent` (`network_mode: service:upstream`,
/// joining `upstream`'s namespace instead of getting its own bridge address; deploying `upstream`
/// automatically cascades `dependent` into the same deploy). `webhost` listens on 8081, not 80:
/// `jiji-proxy` itself binds host ports 80/443 unconditionally once `server setup` has run
/// (confirmed live), regardless of whether any service in this project actually configures
/// `proxy:` -- matching AGENTS.md's "ports 80 and 443 remain reserved for HTTP ingress". Uses
/// `busybox` with an explicit `httpd -p 8081`, not `nginx:alpine`: nginx's listen port is baked
/// into its own config, not overridable via a bare `command:`.
pub fn write_config_with_network_mode_services(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  webhost:
    image: busybox:1.36
    servers: [vm1]
    network_mode: host
    ports: ["8081"]
    command: ["sh", "-c", "mkdir -p /www && echo ok > /www/index.html && httpd -f -p 8081 -h /www"]
  upstream:
    image: nginx:alpine
    servers: [vm1]
  dependent:
    image: nginx:alpine
    servers: [vm1]
    network_mode: "service:upstream"
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// The command a scheduled cron in this suite's fixture runs; assert on this appearing in
/// `jiji service cron run ... --follow`'s streamed output to prove the run actually executed in
/// a real container, not just that the CLI accepted the request.
pub const CRON_TEST_MARKER: &str = "jiji-docker-cron-marker";

/// A service with one `crons:` entry, scheduled far enough in the future (`0 0 1 1 *`, next Jan
/// 1st) that it can never fire on its own during a test run -- the test triggers it explicitly
/// via `jiji service cron run`, so this is purely to prove installation (`jiji service cron
/// list`) without a stray natural firing racing the explicit one. `command` is the array form,
/// not a single string: `nginx:alpine`'s own entrypoint does `exec "$@"`, and a single-string
/// command becomes one literal argv token ("echo foo" as one word, command-not-found), not two
/// shell-split words (confirmed live).
pub fn write_config_with_cron_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: [vm1]
    crons:
      ping:
        schedule: "0 0 1 1 *"
        command: ["echo", "{marker}"]
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            marker = CRON_TEST_MARKER,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Two hosts always present in the project's own top-level `servers:` (so jiji can always reach
/// both, cron sweep included), but `web_servers` controls which of them the `web` *service*
/// itself deploys to -- dropping a host from this narrower list, while it stays known to the
/// project overall, is what actually moves cron ownership in this codebase's real placement
/// algorithm (`placement::assignments_for` sorts servers alphabetically and assigns ordinals
/// per-server; the lowest-ordinal Active/Healthy replica owns the cron, and that can only change
/// if the server hosting it stops being desired for this service, not merely from a scale number
/// change alone -- confirmed live).
pub fn write_config_with_transferable_cron_service(
    dir: &Path,
    project: &str,
    web_servers: &[&str],
) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    let web_servers_yaml = format!("[{}]", web_servers.join(", "));
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: {vm1_host}
    port: 22
    keys:
      - {key_path}
  vm2:
    host: {vm2_host}
    port: 22
    keys:
      - {key_path}
services:
  web:
    image: nginx:alpine
    servers: {web_servers_yaml}
    crons:
      ping:
        schedule: "0 0 1 1 *"
        command: ["echo", "{marker}"]
ssh:
  user: root
  keys_only: true
"#,
            vm1_host = MESH_VM1_HOST,
            vm2_host = MESH_VM2_HOST,
            key_path = ssh_key_path().display(),
            marker = CRON_TEST_MARKER,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// A `build:`-configured service with `retain: 2`, no proxy needed: only the number of image
/// tags jiji's own local image cache ends up with after repeated redeploys is under test here.
pub fn write_config_with_retained_build_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");

    let app_dir = dir.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(app_dir.join("Dockerfile"), "FROM nginx:alpine\n")
        .expect("write test app Dockerfile");

    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    build: ./app
    servers: [vm1]
    retain: 2
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Every distinct image tag jiji's own local Podman cache has for `{project}-web`, in whatever
/// order `podman images` lists them (newest first, per `prune.rs`'s own doc comment -- the same
/// order `jiji service prune` relies on rather than parsing `CreatedAt`).
pub fn image_tags_for(project: &str, service: &str) -> Vec<String> {
    let needle = format!("{project}-{service}");
    exec_vm1_stdout(&["podman", "images", "--format", "{{.Repository}}:{{.Tag}}"])
        .lines()
        .filter(|line| line.contains(&needle))
        .filter_map(|line| line.rsplit_once(':').map(|(_, tag)| tag.to_string()))
        .collect()
}

pub const LOAD_BALANCER_TEST_HOST: &str = "lb.jiji.test";

pub const SCALE_TEST_HOST: &str = "scale.jiji.test";

/// One host, `scale: 2`: two replicas of the same service on the *same* server, each serving its
/// own container's hostname as its whole response body, same trick
/// `write_config_with_load_balanced_service` uses across two hosts. Proves per-host multi-replica
/// placement (distinct leased addresses, distinct containers, both routed by jiji-proxy) on a
/// single node, which the two-host load-balancer fixture can't exercise since it only ever runs
/// one replica per host.
pub fn write_config_with_scalable_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: busybox:1.36
    servers: [vm1]
    scale: 2
    command: ["sh", "-c", "mkdir -p /www && hostname > /www/index.html && httpd -f -p 80 -h /www"]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = SCALE_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

pub const STOP_FIRST_HOST_PORT: u16 = 8082;

/// A service that binds a fixed host port directly (`ports: ["{STOP_FIRST_HOST_PORT}:80"]`, not
/// `proxy:`), so two of its containers can never run at once: podman refuses to bind an
/// already-claimed host port. `stop_first` controls whether the deploy transaction stops the
/// previous container before starting the candidate (`stop_first: true`) or leases and starts the
/// candidate first, health-gated, before ever touching the previous one (the default) -- see
/// AGENTS.md's "Health-Gated Deployment Strategy". No `healthcheck:` is configured, so a
/// candidate only needs to reach `running` state (`container_readiness_command`); the port bind
/// happens at container-create time, before any health check runs, so proving this doesn't need a
/// real HTTP probe.
pub fn write_config_with_direct_port_service(
    dir: &Path,
    project: &str,
    stop_first: bool,
) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    image: busybox:1.36
    servers: [vm1]
    ports: ["{host_port}:80"]
    stop_first: {stop_first}
    command: ["sh", "-c", "httpd -f -p 80"]
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            host_port = STOP_FIRST_HOST_PORT,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

pub const RESTART_ROLLBACK_TEST_HOST: &str = "restart-rollback.jiji.test";

/// Same shape as `write_config_with_build_service`, but the Dockerfile content is parameterized
/// by `marker` so a test can build and push two genuinely distinct images under two distinct
/// `--version` tags (an unchanged Dockerfile builds to the same image ID regardless of
/// `--version`, confirmed live -- see `write_config_with_retained_build_service`'s doc comment),
/// to exercise `jiji service rollback` between two real, already-pushed versions and `jiji
/// service restart` against whatever ends up active. `deploy_timeout: 60s` widens both the
/// container health check and `deploy_transaction.rs::activate_proxy_routes`'s own
/// `verify_route_address` poll (jiji's internal wait for jiji-proxy to report the new backend
/// healthy) past the 30s production default: this fixture runs alphabetically near the end of
/// the docker suite, after 11 other tests have already churned real connections through the same
/// shared jiji-proxy on the CI runner, and 30s isn't always enough there (confirmed live -- the
/// production default itself is untouched, only this fixture's own config).
pub fn write_config_with_marked_build_service(dir: &Path, project: &str, marker: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");

    let app_dir = dir.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("Dockerfile"),
        format!("FROM nginx:alpine\nRUN echo '{marker}' > /usr/share/nginx/html/index.html\n"),
    )
    .expect("write test app Dockerfile");

    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: 127.0.0.1
    port: {port}
    keys:
      - {key_path}
services:
  web:
    build: ./app
    servers: [vm1]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
        deploy_timeout: 60s
ssh:
  user: root
  keys_only: true
"#,
            port = VM1_SSH_PORT,
            key_path = ssh_key_path().display(),
            proxy_host = RESTART_ROLLBACK_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}

/// Two replicas of the same service, one per host, each serving its own container's hostname as
/// its entire response body (computed once at container start via `hostname > index.html`) --
/// the cheapest way to make two otherwise-identical `busybox` responses distinguishable, so a
/// test can prove jiji-proxy is actually alternating between backends rather than just that both
/// are independently reachable (`docker_mesh_deploy_test.rs` already covers the latter).
pub fn write_config_with_load_balanced_service(dir: &Path, project: &str) -> PathBuf {
    let jiji_dir = dir.join(".jiji");
    std::fs::create_dir_all(&jiji_dir).expect("create .jiji dir");
    let config_path = jiji_dir.join("deploy.yml");
    std::fs::write(
        &config_path,
        format!(
            r#"
project: {project}
builder:
  engine: podman
servers:
  vm1:
    host: {vm1_host}
    port: 22
    keys:
      - {key_path}
  vm2:
    host: {vm2_host}
    port: 22
    keys:
      - {key_path}
services:
  web:
    image: busybox:1.36
    servers: [vm1, vm2]
    command: ["sh", "-c", "mkdir -p /www && hostname > /www/index.html && httpd -f -p 80 -h /www"]
    proxy:
      port: 80
      ssl: false
      hosts: [{proxy_host}]
      healthcheck:
        path: /
ssh:
  user: root
  keys_only: true
"#,
            vm1_host = MESH_VM1_HOST,
            vm2_host = MESH_VM2_HOST,
            key_path = ssh_key_path().display(),
            proxy_host = LOAD_BALANCER_TEST_HOST,
        ),
    )
    .expect("write test deploy.yml");
    config_path
}
