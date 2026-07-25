use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use jiji_config::{BuildValue, Config, ContainerEngine, Service};

use crate::local_exec;

pub const BUILDX_BUILDER_NAME: &str = "jiji-builder";
pub const LOCAL_BUILDX_BUILDER_NAME: &str = "jiji-builder-local";

#[derive(Debug, Clone)]
pub struct ResolvedBuildConfig {
    pub context: String,
    pub dockerfile: String,
    pub args: BTreeMap<String, String>,
    pub target: Option<String>,
}

pub fn resolve_build_config(build: &BuildValue) -> ResolvedBuildConfig {
    match build {
        BuildValue::Context(context) => ResolvedBuildConfig {
            context: context.clone(),
            dockerfile: "Dockerfile".into(),
            args: BTreeMap::new(),
            target: None,
        },
        BuildValue::Detailed(build) => ResolvedBuildConfig {
            context: build.context.clone(),
            dockerfile: build
                .dockerfile
                .clone()
                .unwrap_or_else(|| "Dockerfile".into()),
            args: build.args.clone().unwrap_or_default().into_iter().collect(),
            target: build.target.clone(),
        },
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
        .hosts
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
            let builder_name = if local_registry {
                LOCAL_BUILDX_BUILDER_NAME
            } else {
                BUILDX_BUILDER_NAME
            };
            let inspect = local_exec::run_captured(
                "docker",
                &render_buildx_inspect(builder_name),
                None,
                Some(cwd),
            )
            .await?;
            if !inspect.success {
                stream(
                    engine,
                    render_buildx_create(builder_name, local_registry),
                    cwd,
                )
                .await?;
            }
            stream(
                engine,
                render_buildx_build(build, no_cache, platforms, tags, builder_name),
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
    use jiji_config::{Builder, NamedServer, Registry};
    use std::collections::HashMap;

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
            BUILDX_BUILDER_NAME,
        );
        assert_eq!(args[5], "linux/arm64,linux/amd64");
        assert_eq!(args.last().unwrap(), ".");
        assert!(args.contains(&"--push".into()));
        assert_eq!(
            render_buildx_create(LOCAL_BUILDX_BUILDER_NAME, true),
            [
                "buildx",
                "create",
                "--name",
                "jiji-builder-local",
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
            hosts: vec!["one".into(), "two".into(), "three".into()],
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
        };
        let server = |arch: Option<&str>| NamedServer {
            host: "example".into(),
            arch: arch.map(str::to_string),
            user: None,
            port: None,
            key_path: None,
            key_passphrase: None,
            keys: None,
            key_data: None,
        };
        let config = Config {
            project: "demo".into(),
            builder: Builder {
                engine: ContainerEngine::Docker,
                local: true,
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
