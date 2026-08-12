use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use jiji_config::{BuildValue, Config, ContainerEngine, Service};

use crate::local_exec;

/// Project-scoped buildx builder name, e.g. `jiji-builder-myproject` (or
/// `jiji-builder-myproject-local` when pushing to the loopback local registry, which needs its
/// own builder using the host-network driver -- see `render_buildx_create`). Plain interpolation,
/// no sanitization, matching `manifest_name`'s existing convention for the same naming problem.
pub fn buildx_builder_name(project: &str, local_registry: bool) -> String {
    if local_registry {
        format!("jiji-builder-{project}-local")
    } else {
        format!("jiji-builder-{project}")
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedBuildConfig {
    pub context: String,
    pub dockerfile: String,
    pub args: BTreeMap<String, String>,
    pub target: Option<String>,
}

/// `dockerfile:` is resolved relative to `context:` (matching Docker Compose convention), then
/// joined onto context to produce the project-root-relative path every downstream consumer
/// expects: `common_build_flags`'s `-f` value (the local engine runs with cwd = project root, not
/// context) and `build_context.rs::resolve_remote_context`'s `project_root.join(&build.dockerfile)`
/// both operate on `ResolvedBuildConfig::dockerfile` as-is, so this is the single place that needs
/// to know about the context/dockerfile split.
fn resolve_dockerfile_path(context: &str, dockerfile: &str) -> String {
    // Drop `.` components entirely (rather than just normalizing "./x" -> "x") so a "." context
    // still yields a bare "Dockerfile", matching the pre-existing convention every rendered
    // command and test string expects.
    let path = if Path::new(dockerfile).is_absolute() {
        Path::new(dockerfile).to_path_buf()
    } else {
        Path::new(context).join(dockerfile)
    };
    let absolute = path.is_absolute();
    let parts: Vec<_> = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir | Component::RootDir))
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        ".".into()
    } else {
        format!("{}{}", if absolute { "/" } else { "" }, parts.join("/"))
    }
}

pub fn resolve_build_config(build: &BuildValue) -> ResolvedBuildConfig {
    match build {
        BuildValue::Context(context) => ResolvedBuildConfig {
            dockerfile: resolve_dockerfile_path(context, "Dockerfile"),
            context: context.clone(),
            args: BTreeMap::new(),
            target: None,
        },
        BuildValue::Detailed(build) => {
            let dockerfile = build.dockerfile.as_deref().unwrap_or("Dockerfile");
            ResolvedBuildConfig {
                dockerfile: resolve_dockerfile_path(&build.context, dockerfile),
                context: build.context.clone(),
                args: build.args.clone().unwrap_or_default().into_iter().collect(),
                target: build.target.clone(),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStrategy {
    SingleArch,
    MultiArch,
}

pub fn build_strategy(platforms: &[String]) -> BuildStrategy {
    if platforms.len() <= 1 {
        BuildStrategy::SingleArch
    } else {
        BuildStrategy::MultiArch
    }
}

pub fn to_platform(arch: &str) -> String {
    format!("linux/{arch}")
}

pub fn required_arches(config: &Config, service: &Service) -> Vec<String> {
    let mut seen = BTreeSet::new();
    service
        .servers
        .iter()
        .filter_map(|name| config.servers.get(name))
        .map(|server| to_platform(server.arch.as_deref().unwrap_or("amd64")))
        .filter(|platform| seen.insert(platform.clone()))
        .collect()
}

pub fn multi_arch_requires_push(platforms: &[String], push: bool) -> Option<String> {
    (platforms.len() > 1 && !push).then(|| {
        "Multi-architecture builds require pushing because a multi-platform image cannot be loaded into the local engine. Remove --no-push or restrict the service to one architecture.".into()
    })
}

fn common_build_flags(build: &ResolvedBuildConfig, no_cache: bool) -> Vec<String> {
    let mut args = vec!["-f".into(), build.dockerfile.clone()];
    for (key, value) in &build.args {
        args.extend(["--build-arg".into(), format!("{key}={value}")]);
    }
    if let Some(target) = &build.target {
        args.extend(["--target".into(), target.clone()]);
    }
    if no_cache {
        args.push("--no-cache".into());
    }
    args
}

pub fn render_single_arch_build(
    build: &ResolvedBuildConfig,
    no_cache: bool,
    tags: &[String],
) -> Vec<String> {
    let mut args = vec!["build".into()];
    args.extend(common_build_flags(build, no_cache));
    for tag in tags {
        args.extend(["-t".into(), tag.clone()]);
    }
    args.push(build.context.clone());
    args
}

pub fn render_push(engine: ContainerEngine, local_registry: bool, tag: &str) -> Vec<String> {
    let mut args = vec!["push".into()];
    // Docker treats localhost/127.0.0.0-8 registries as insecure automatically; Podman does not
    // and refuses plain HTTP unless told to skip TLS verification.
    if engine == ContainerEngine::Podman && local_registry {
        args.push("--tls-verify=false".into());
    }
    args.push(tag.into());
    args
}

pub fn render_buildx_inspect(builder_name: &str) -> Vec<String> {
    vec!["buildx".into(), "inspect".into(), builder_name.into()]
}

pub fn render_buildx_create(builder_name: &str, host_network: bool) -> Vec<String> {
    let mut args = vec![
        "buildx".into(),
        "create".into(),
        "--name".into(),
        builder_name.into(),
        "--driver".into(),
        "docker-container".into(),
    ];
    if host_network {
        args.extend(["--driver-opt".into(), "network=host".into()]);
    }
    args.push("--bootstrap".into());
    args
}

pub fn render_buildx_build(
    build: &ResolvedBuildConfig,
    no_cache: bool,
    platforms: &[String],
    tags: &[String],
    builder_name: &str,
) -> Vec<String> {
    let mut args = vec![
        "buildx".into(),
        "build".into(),
        "--builder".into(),
        builder_name.into(),
        "--platform".into(),
        platforms.join(","),
    ];
    args.extend(common_build_flags(build, no_cache));
    for tag in tags {
        args.extend(["-t".into(), tag.clone()]);
    }
    args.extend(["--push".into(), build.context.clone()]);
    args
}

pub fn manifest_name(project: &str, service: &str) -> String {
    format!("jiji-{project}-{service}-build")
}

pub fn render_manifest_rm(name: &str) -> Vec<String> {
    vec!["manifest".into(), "rm".into(), name.into()]
}

pub fn render_manifest_create(name: &str) -> Vec<String> {
    vec!["manifest".into(), "create".into(), name.into()]
}

pub fn render_podman_arch_build(
    build: &ResolvedBuildConfig,
    no_cache: bool,
    platform: &str,
    manifest: &str,
) -> Vec<String> {
    let mut args = vec!["build".into(), "--platform".into(), platform.into()];
    args.extend(common_build_flags(build, no_cache));
    args.extend(["--manifest".into(), manifest.into(), build.context.clone()]);
    args
}

pub fn render_manifest_push(manifest: &str, tag: &str, local_registry: bool) -> Vec<String> {
    let mut args = vec!["manifest".into(), "push".into(), "--all".into()];
    if local_registry {
        args.push("--tls-verify=false".into());
    }
    args.push(manifest.into());
    args.push(format!("docker://{tag}"));
    args
}

async fn stream(engine: ContainerEngine, args: Vec<String>, cwd: &Path) -> anyhow::Result<()> {
    if !local_exec::run_streaming(&engine.to_string(), &args, Some(cwd)).await? {
        anyhow::bail!(
            "Local command '{} {}' failed. Fix the reported container-engine error and retry.",
            engine,
            args.join(" ")
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn build_and_push(
    engine: ContainerEngine,
    build: &ResolvedBuildConfig,
    no_cache: bool,
    platforms: &[String],
    tags: &[String],
    push: bool,
    project: &str,
    service_name: &str,
    cwd: &Path,
    local_registry: bool,
) -> anyhow::Result<()> {
    if let Some(error) = multi_arch_requires_push(platforms, push) {
        anyhow::bail!(error);
    }
    if !local_exec::command_exists(&engine.to_string()).await {
        anyhow::bail!(
            "Container engine '{engine}' was not found locally. Install it or update builder.engine."
        );
    }
    match (build_strategy(platforms), engine) {
        (BuildStrategy::SingleArch, _) => {
            stream(engine, render_single_arch_build(build, no_cache, tags), cwd).await?;
            if push {
                for tag in tags {
                    stream(engine, render_push(engine, local_registry, tag), cwd).await?;
                }
            }
        }
        (BuildStrategy::MultiArch, ContainerEngine::Docker) => {
            let builder_name = buildx_builder_name(project, local_registry);
            let inspect = local_exec::run_captured(
                "docker",
                &render_buildx_inspect(&builder_name),
                None,
                Some(cwd),
            )
            .await?;
            if !inspect.success {
                if let Err(create_error) = stream(
                    engine,
                    render_buildx_create(&builder_name, local_registry),
                    cwd,
                )
                .await
                {
                    // Tolerate a concurrent `jiji` process winning the create race: if the
                    // builder exists now, proceed; otherwise the create failure was real, and
                    // re-raising the retry's own error would hide what actually went wrong.
                    let retry_inspect = local_exec::run_captured(
                        "docker",
                        &render_buildx_inspect(&builder_name),
                        None,
                        Some(cwd),
                    )
                    .await?;
                    if !retry_inspect.success {
                        return Err(create_error);
                    }
                }
            }
            stream(
                engine,
                render_buildx_build(build, no_cache, platforms, tags, &builder_name),
                cwd,
            )
            .await?;
        }
        (BuildStrategy::MultiArch, ContainerEngine::Podman) => {
            let manifest = manifest_name(project, service_name);
            let _ =
                local_exec::run_captured("podman", &render_manifest_rm(&manifest), None, Some(cwd))
                    .await;
            stream(engine, render_manifest_create(&manifest), cwd).await?;
            for platform in platforms {
                stream(
                    engine,
                    render_podman_arch_build(build, no_cache, platform, &manifest),
                    cwd,
                )
                .await?;
            }
            for tag in tags {
                stream(
                    engine,
                    render_manifest_push(&manifest, tag, local_registry),
                    cwd,
                )
                .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::{BuildConfig, BuildValue, Builder, NamedServer, Registry};
    use std::collections::HashMap;

    #[test]
    fn dockerfile_defaults_and_paths_resolve_relative_to_context_not_project_root() {
        assert_eq!(
            resolve_build_config(&BuildValue::Context("./api".into())).dockerfile,
            "api/Dockerfile"
        );
        assert_eq!(
            resolve_build_config(&BuildValue::Detailed(BuildConfig {
                context: "./api".into(),
                dockerfile: Some("Dockerfile".into()),
                args: None,
                target: None,
            }))
            .dockerfile,
            "api/Dockerfile"
        );
        assert_eq!(
            resolve_build_config(&BuildValue::Detailed(BuildConfig {
                context: "./api".into(),
                dockerfile: Some("docker/Dockerfile.prod".into()),
                args: None,
                target: None,
            }))
            .dockerfile,
            "api/docker/Dockerfile.prod"
        );
        assert_eq!(
            resolve_build_config(&BuildValue::Context(".".into())).dockerfile,
            "Dockerfile"
        );
        assert_eq!(
            resolve_build_config(&BuildValue::Detailed(BuildConfig {
                context: "./api".into(),
                dockerfile: Some("/etc/jiji/Containerfile".into()),
                args: None,
                target: None,
            }))
            .dockerfile,
            "/etc/jiji/Containerfile"
        );
    }

    #[test]
    fn omitted_context_resolves_the_same_as_an_explicit_project_root() {
        let yaml = r#"
project: demo
builder:
  engine: podman
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
    build:
      dockerfile: Dockerfile
"#;
        let config: jiji_config::Config =
            serde_yaml::from_str(yaml).expect("config with context omitted should parse");
        let build = config.services["app"]
            .build
            .as_ref()
            .expect("build config present");
        let resolved = resolve_build_config(build);
        let expected = resolve_build_config(&BuildValue::Context(".".into()));
        assert_eq!(resolved.context, expected.context);
        assert_eq!(resolved.dockerfile, expected.dockerfile);
    }

    fn build() -> ResolvedBuildConfig {
        ResolvedBuildConfig {
            context: ".".into(),
            dockerfile: "Containerfile".into(),
            args: BTreeMap::from([("B".into(), "2".into()), ("A".into(), "1".into())]),
            target: Some("release".into()),
        }
    }

    #[test]
    fn renderers_are_stable_and_sorted() {
        assert_eq!(
            render_single_arch_build(&build(), true, &["repo/app:v1".into()]),
            [
                "build",
                "-f",
                "Containerfile",
                "--build-arg",
                "A=1",
                "--build-arg",
                "B=2",
                "--target",
                "release",
                "--no-cache",
                "-t",
                "repo/app:v1",
                "."
            ]
        );
        let args = render_buildx_build(
            &build(),
            false,
            &["linux/arm64".into(), "linux/amd64".into()],
            &["repo/app:v1".into()],
            &buildx_builder_name("demo", false),
        );
        assert_eq!(args[5], "linux/arm64,linux/amd64");
        assert_eq!(args.last().unwrap(), ".");
        assert!(args.contains(&"--push".into()));
        assert_eq!(
            render_buildx_create(&buildx_builder_name("demo", true), true),
            [
                "buildx",
                "create",
                "--name",
                "jiji-builder-demo-local",
                "--driver",
                "docker-container",
                "--driver-opt",
                "network=host",
                "--bootstrap"
            ]
        );
        assert_eq!(
            render_manifest_push("local-manifest", "repo/app:v1", false),
            [
                "manifest",
                "push",
                "--all",
                "local-manifest",
                "docker://repo/app:v1"
            ]
        );
        assert_eq!(
            render_manifest_push("local-manifest", "repo/app:v1", true),
            [
                "manifest",
                "push",
                "--all",
                "--tls-verify=false",
                "local-manifest",
                "docker://repo/app:v1"
            ]
        );
        assert_eq!(
            render_push(ContainerEngine::Docker, true, "localhost:31270/app:v1"),
            ["push", "localhost:31270/app:v1"]
        );
        assert_eq!(
            render_push(ContainerEngine::Podman, true, "localhost:31270/app:v1"),
            ["push", "--tls-verify=false", "localhost:31270/app:v1"]
        );
        assert_eq!(
            render_push(ContainerEngine::Podman, false, "ghcr.io/acme/app:v1"),
            ["push", "ghcr.io/acme/app:v1"]
        );
    }

    #[test]
    fn buildx_builder_name_varies_by_project_and_local_registry() {
        assert_eq!(buildx_builder_name("demo", false), "jiji-builder-demo");
        assert_eq!(buildx_builder_name("demo", true), "jiji-builder-demo-local");
        assert_ne!(
            buildx_builder_name("demo", false),
            buildx_builder_name("other", false)
        );
    }

    #[test]
    fn build_strategy_and_push_guard_follow_platform_count() {
        assert_eq!(
            build_strategy(&["linux/amd64".into()]),
            BuildStrategy::SingleArch
        );
        assert_eq!(
            build_strategy(&["linux/amd64".into(), "linux/arm64".into()]),
            BuildStrategy::MultiArch
        );
        assert!(
            multi_arch_requires_push(&["linux/amd64".into(), "linux/arm64".into()], false)
                .is_some()
        );
        assert!(multi_arch_requires_push(&["linux/amd64".into()], false).is_none());
    }

    #[test]
    fn required_arches_defaults_and_deduplicates_in_host_order() {
        let service = Service {
            image: None,
            build: None,
            servers: vec!["one".into(), "two".into(), "three".into()],
            replicas: 1,
            placement: Default::default(),
            ports: vec![],
            volumes: vec![],
            files: vec![],
            directories: vec![],
            environment: Default::default(),
            command: None,
            proxy: None,
            retain: 5,
            network_mode: "private".into(),
            cpus: None,
            memory: None,
            gpus: None,
            devices: vec![],
            privileged: false,
            cap_add: vec![],
            stop_first: false,
            restart: None,
            crons: Default::default(),
        };
        let server = |arch: Option<&str>| NamedServer {
            host: "example".into(),
            arch: arch.map(str::to_string),
            user: None,
            port: None,
            key_passphrase: None,
            keys: None,
        };
        let config = Config {
            project: "demo".into(),
            builder: Builder {
                engine: ContainerEngine::Docker,
                remote: None,
                cache: true,
                registry: Registry::default(),
            },
            servers: HashMap::from([
                ("one".into(), server(None)),
                ("two".into(), server(Some("arm64"))),
                ("three".into(), server(None)),
            ]),
            services: HashMap::new(),
            ssh: None,
            network: None,
            secrets_path: None,
            secrets: None,
            environment: None,
        };
        assert_eq!(
            required_arches(&config, &service),
            ["linux/amd64", "linux/arm64"]
        );
    }
}
