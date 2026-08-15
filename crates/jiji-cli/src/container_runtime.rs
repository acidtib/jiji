use std::net::Ipv4Addr;

use jiji_config::{CommandValue, ContainerEngine, CpusValue, Service};
use jiji_network::{NetworkedContainerRun, ServerPlan};

/// Jiji does not need persistent exec sessions. Disabling them avoids Podman clients waiting on
/// stale session state after the process inside the container has already exited.
pub fn exec_prefix(engine: ContainerEngine) -> &'static str {
    match engine {
        ContainerEngine::Docker => "docker exec",
        ContainerEngine::Podman => "podman exec --no-session",
    }
}

/// Expands Docker-compatible short names so Podman never depends on host-specific
/// `unqualified-search-registries` configuration.
pub fn normalize_image_name(image: &str) -> String {
    let Some((first, _)) = image.split_once('/') else {
        return format!("docker.io/library/{image}");
    };
    if first == "localhost" || first.contains('.') || first.contains(':') {
        image.to_string()
    } else {
        format!("docker.io/{image}")
    }
}

/// Applies `--version` to an image reference that has no explicit tag on its last path segment
/// (checked after the final `/`, so a registry port like `localhost:5000/app` is never mistaken
/// for a tag). Rejects `--version` outright if the image already carries an explicit tag, rather
/// than silently ignoring a flag the user passed.
pub fn resolve_image_reference(image: &str, version: Option<&str>) -> anyhow::Result<String> {
    let last_segment = image.rsplit('/').next().unwrap_or(image);
    let has_explicit_tag = last_segment.contains(':');

    let resolved = match version {
        None => image.to_string(),
        Some(version) if has_explicit_tag => {
            anyhow::bail!(
                "Image '{image}' already has an explicit tag; remove it or drop --version '{version}' to resolve the conflict."
            )
        }
        Some(version) => format!("{image}:{version}"),
    };
    Ok(normalize_image_name(&resolved))
}

pub fn render_labels(project: &str, service: &str, server: &str) -> Vec<String> {
    vec![
        "--label".to_string(),
        "jiji.managed=true".to_string(),
        "--label".to_string(),
        format!("jiji.project={project}"),
        "--label".to_string(),
        format!("jiji.service={service}"),
        "--label".to_string(),
        format!("jiji.server={server}"),
        "--label".to_string(),
        "jiji.resource=service".to_string(),
    ]
}

pub fn render_dynamic_labels(
    project: &str,
    service: &str,
    server: &str,
    replica_id: &str,
    deployment_id: &str,
    lease_address: Ipv4Addr,
    lifecycle: &str,
) -> Vec<String> {
    let mut labels = render_labels(project, service, server);
    for value in [
        "jiji.catalog-managed=true".to_string(),
        format!("jiji.replica={replica_id}"),
        format!("jiji.deployment={deployment_id}"),
        format!("jiji.lease={lease_address}"),
        format!("jiji.lifecycle={lifecycle}"),
    ] {
        labels.push("--label".to_string());
        labels.push(value);
    }
    labels
}

#[allow(clippy::too_many_arguments)]
pub fn build_dynamic_run(
    engine: ContainerEngine,
    project: &str,
    service_name: &str,
    server_name: &str,
    replica_id: &str,
    deployment_id: &str,
    image: &str,
    address: Ipv4Addr,
    server: &ServerPlan,
    service: &Service,
    mount_args: &[String],
    env_file_path: &str,
) -> NetworkedContainerRun {
    let name = dynamic_container_name(project, service_name, deployment_id);
    let mut run = NetworkedContainerRun::dynamic(engine, name, image, address, server);
    run.extra_args = vec![
        "--detach".to_string(),
        "--restart".to_string(),
        service.restart.unwrap_or_default().to_string(),
    ];
    run.extra_args.extend(render_dynamic_labels(
        project,
        service_name,
        server_name,
        replica_id,
        deployment_id,
        address,
        "candidate",
    ));
    run.extra_args.extend(render_ports(&service.ports));
    run.extra_args.extend(mount_args.iter().cloned());
    run.extra_args.push("--env-file".to_string());
    run.extra_args.push(env_file_path.to_string());
    run.extra_args.extend(render_resource_options(service));
    run.command = render_command(&service.command);
    run
}

/// Builds the run for a `network_mode: service:<other>` dependent: joins `target_container_name`'s
/// network namespace instead of leasing its own address. `upstream_address` is the upstream's
/// current address, kept only for the `jiji.lease` label and observability -- it is never rendered
/// into `--ip`, since the dependent has no address of its own (see
/// `NetworkedContainerRun::shared`). Ports are deliberately not rendered here: they belong to
/// whichever service owns the shared network namespace, never to the dependent.
#[allow(clippy::too_many_arguments)]
pub fn build_shared_run(
    engine: ContainerEngine,
    project: &str,
    service_name: &str,
    server_name: &str,
    replica_id: &str,
    deployment_id: &str,
    image: &str,
    target_container_name: &str,
    upstream_address: Ipv4Addr,
    server: &ServerPlan,
    service: &Service,
    mount_args: &[String],
    env_file_path: &str,
) -> NetworkedContainerRun {
    let name = dynamic_container_name(project, service_name, deployment_id);
    let mut run = NetworkedContainerRun::shared(
        engine,
        name,
        image,
        target_container_name,
        upstream_address,
        server,
    );
    run.extra_args = vec![
        "--detach".to_string(),
        "--restart".to_string(),
        service.restart.unwrap_or_default().to_string(),
    ];
    run.extra_args.extend(render_dynamic_labels(
        project,
        service_name,
        server_name,
        replica_id,
        deployment_id,
        upstream_address,
        "candidate",
    ));
    run.extra_args.extend(mount_args.iter().cloned());
    run.extra_args.push("--env-file".to_string());
    run.extra_args.push(env_file_path.to_string());
    run.extra_args.extend(render_resource_options(service));
    run.command = render_command(&service.command);
    run
}

/// Builds the run for a `network_mode: host` service: shares the host's own network namespace
/// instead of a leased bridge address. `address` is the server's `management_address`, kept only
/// for the `jiji.lease` label and catalog/observability use -- it is never rendered into `--ip`,
/// since a host-networked container has no address of its own (see `NetworkedContainerRun::host`).
/// Ports are deliberately never rendered here: `-p` combined with `--network host` is at best a
/// discarded, warning-spamming no-op and at worst a hard error on some engine versions (see
/// `docs/architecture-notes.md#container-namespace-sharing`); the app must already bind whatever
/// port it wants directly on the host.
#[allow(clippy::too_many_arguments)]
pub fn build_host_run(
    engine: ContainerEngine,
    project: &str,
    service_name: &str,
    server_name: &str,
    replica_id: &str,
    deployment_id: &str,
    image: &str,
    address: Ipv4Addr,
    server: &ServerPlan,
    service: &Service,
    mount_args: &[String],
    env_file_path: &str,
) -> NetworkedContainerRun {
    let name = dynamic_container_name(project, service_name, deployment_id);
    let mut run = NetworkedContainerRun::host(engine, name, image, address, server);
    run.extra_args = vec![
        "--detach".to_string(),
        "--restart".to_string(),
        service.restart.unwrap_or_default().to_string(),
    ];
    run.extra_args.extend(render_dynamic_labels(
        project,
        service_name,
        server_name,
        replica_id,
        deployment_id,
        address,
        "candidate",
    ));
    run.extra_args.extend(mount_args.iter().cloned());
    run.extra_args.push("--env-file".to_string());
    run.extra_args.push(env_file_path.to_string());
    run.extra_args.extend(render_resource_options(service));
    run.command = render_command(&service.command);
    run
}

pub fn dynamic_container_name(project: &str, service: &str, deployment_id: &str) -> String {
    let suffix = deployment_id.get(..12).unwrap_or(deployment_id);
    format!("{project}-{service}-{suffix}")
}

/// Raw passthrough: each `ports` entry becomes `-p {value}` unmodified (no transformation of
/// host/container port or `/udp` suffix), matching the original tool's behavior.
pub fn render_ports(ports: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(ports.len() * 2);
    for port in ports {
        args.push("-p".to_string());
        args.push(port.clone());
    }
    args
}

/// A bind mount source starts with `/` or `.`; anything else names a named (engine-managed)
/// volume. Shared with `crate::volume_teardown` so candidate-name computation there can never
/// drift from what deploy actually renders here.
pub fn is_named_volume_source(source: &str) -> bool {
    !(source.starts_with('/') || source.starts_with('.'))
}

/// A bind mount passes through unchanged. A named volume gets prefixed with the service name (not
/// the project name) so volumes from different services never collide: `web_storage:/data` ->
/// `-v myservice-web_storage:/data`.
pub fn render_volumes(volumes: &[String], service_name: &str) -> Vec<String> {
    let mut args = Vec::with_capacity(volumes.len() * 2);
    for volume in volumes {
        args.push("-v".to_string());
        let Some(colon) = volume.find(':') else {
            args.push(volume.clone());
            continue;
        };
        let (source, rest) = volume.split_at(colon);
        if is_named_volume_source(source) {
            args.push(format!("{service_name}-{source}{rest}"));
        } else {
            args.push(volume.clone());
        }
    }
    args
}

pub fn render_resource_options(service: &Service) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(cpus) = &service.cpus {
        let value = match cpus {
            CpusValue::Number(n) => n.to_string(),
            CpusValue::Text(t) => t.clone(),
        };
        args.push("--cpus".to_string());
        args.push(value);
    }
    if let Some(memory) = &service.memory {
        args.push("--memory".to_string());
        args.push(memory.clone());
    }
    if let Some(gpus) = &service.gpus {
        args.push("--gpus".to_string());
        args.push(gpus.clone());
    }
    for device in &service.devices {
        args.push("--device".to_string());
        args.push(device.clone());
    }
    if service.privileged {
        args.push("--privileged".to_string());
    }
    for cap in &service.cap_add {
        args.push("--cap-add".to_string());
        args.push(cap.clone());
    }
    args
}

/// String form is one shell token (its own escaping happens later, in
/// `NetworkedContainerRun::shell_command`); array form is one token per element. No `${VAR}`
/// interpolation is performed (not required by the current command surface).
pub fn render_command(command: &Option<CommandValue>) -> Vec<String> {
    match command {
        None => Vec::new(),
        Some(CommandValue::Single(value)) => vec![value.clone()],
        Some(CommandValue::Multiple(values)) => values.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(yaml: &str) -> Service {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn resolve_image_reference_appends_version_when_untagged() {
        assert_eq!(
            resolve_image_reference("example/web", Some("1.2.3")).unwrap(),
            "docker.io/example/web:1.2.3"
        );
        assert_eq!(
            resolve_image_reference("example/web", None).unwrap(),
            "docker.io/example/web"
        );
    }

    #[test]
    fn resolve_image_reference_ignores_registry_port_when_checking_for_a_tag() {
        assert_eq!(
            resolve_image_reference("localhost:5000/app", Some("2")).unwrap(),
            "localhost:5000/app:2"
        );
    }

    #[test]
    fn resolve_image_reference_rejects_conflicting_explicit_tag() {
        assert!(resolve_image_reference("example/web:v1", Some("v2")).is_err());
    }

    #[test]
    fn image_names_are_fully_qualified_for_podman_compatibility() {
        assert_eq!(
            normalize_image_name("nginx:latest"),
            "docker.io/library/nginx:latest"
        );
        assert_eq!(
            normalize_image_name("dxflrs/garage:v2.1.0"),
            "docker.io/dxflrs/garage:v2.1.0"
        );
        assert_eq!(
            normalize_image_name("ghcr.io/owner/repo:v1"),
            "ghcr.io/owner/repo:v1"
        );
        assert_eq!(
            normalize_image_name("localhost:5000/app:v1"),
            "localhost:5000/app:v1"
        );
        assert_eq!(
            normalize_image_name("10.0.0.1:5000/app:v1"),
            "10.0.0.1:5000/app:v1"
        );
    }

    #[test]
    fn labels_are_always_present() {
        let labels = render_labels("demo", "web", "app").join(" ");
        assert!(labels.contains("jiji.managed=true"));
        assert!(labels.contains("jiji.project=demo"));
        assert!(labels.contains("jiji.service=web"));
        assert!(labels.contains("jiji.server=app"));
        assert!(labels.contains("jiji.resource=service"));
    }

    #[test]
    fn ports_pass_through_unmodified() {
        let ports = vec![
            "3000".to_string(),
            "8080:80".to_string(),
            "80/udp".to_string(),
        ];
        assert_eq!(
            render_ports(&ports),
            vec!["-p", "3000", "-p", "8080:80", "-p", "80/udp"]
        );
    }

    #[test]
    fn bind_mount_volumes_pass_through_unchanged() {
        let volumes = vec!["/data:/data".to_string(), "./relative:/data".to_string()];
        let rendered = render_volumes(&volumes, "web");
        assert_eq!(
            rendered,
            vec!["-v", "/data:/data", "-v", "./relative:/data"]
        );
    }

    #[test]
    fn is_named_volume_source_agrees_with_render_volumes_rule() {
        assert!(!is_named_volume_source("/data"));
        assert!(!is_named_volume_source("./relative"));
        assert!(!is_named_volume_source("../parent"));
        assert!(is_named_volume_source("web_storage"));
        assert!(is_named_volume_source("justaname"));
    }

    #[test]
    fn named_volumes_get_prefixed_with_service_name_only() {
        let volumes = vec!["web_storage:/data".to_string()];
        let rendered = render_volumes(&volumes, "web");
        assert_eq!(rendered, vec!["-v", "web-web_storage:/data"]);
    }

    #[test]
    fn volume_without_colon_passes_through_unchanged() {
        let volumes = vec!["justaname".to_string()];
        assert_eq!(render_volumes(&volumes, "web"), vec!["-v", "justaname"]);
    }

    #[test]
    fn resource_options_present_only_when_configured() {
        let none = service("image: example/web\nservers: [app]\n");
        assert!(render_resource_options(&none).is_empty());

        let all = service(
            r#"
image: example/web
servers: [app]
cpus: 1.5
memory: "512m"
gpus: "all"
devices: ["/dev/video0"]
privileged: true
cap_add: ["SYS_ADMIN"]
"#,
        );
        let rendered = all;
        let args = render_resource_options(&rendered);
        assert!(args.contains(&"--cpus".to_string()));
        assert!(args.contains(&"1.5".to_string()));
        assert!(args.contains(&"--memory".to_string()));
        assert!(args.contains(&"--gpus".to_string()));
        assert!(args.contains(&"--device".to_string()));
        assert!(args.contains(&"--privileged".to_string()));
        assert!(args.contains(&"--cap-add".to_string()));
    }

    #[test]
    fn command_string_is_a_single_token_array_form_is_multiple() {
        assert_eq!(
            render_command(&Some(CommandValue::Single("./run.sh --flag".to_string()))),
            vec!["./run.sh --flag".to_string()]
        );
        assert_eq!(
            render_command(&Some(CommandValue::Multiple(vec![
                "./run.sh".to_string(),
                "--flag".to_string()
            ]))),
            vec!["./run.sh".to_string(), "--flag".to_string()]
        );
        assert!(render_command(&None).is_empty());
    }

    #[test]
    fn build_host_run_uses_network_host_omits_ports_and_labels_management_address() {
        use jiji_network::NetworkPlanner;

        let config: jiji_config::Config = serde_yaml::from_str(
            "project: demo\nbuilder: { engine: docker }\nservers:\n  app: { host: 203.0.113.10 }\nservices:\n  web: { image: nginx, servers: [app], network_mode: host, ports: [\"8080\"] }\n",
        )
        .unwrap();
        let plan = NetworkPlanner::new().plan(&config).unwrap();
        let server = &plan.servers["app"];
        let service = &config.services["web"];
        let run = build_host_run(
            ContainerEngine::Docker,
            "demo",
            "web",
            "app",
            "replica-1",
            "deadbeefdeadbeef",
            "nginx:latest",
            server.management_address,
            server,
            service,
            &[],
            "/tmp/env",
        );
        let command = run.shell_command();
        assert!(command.contains("--network host"));
        assert!(!command.contains("-p 8080"));
        assert!(command.contains(&format!("jiji.lease={}", server.management_address)));
    }
}
