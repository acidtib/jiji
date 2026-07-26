use std::path::Path;

use anyhow::Context;
use jiji_config::{Config, ContainerEngine};
use jiji_ssh::ConnectOptions;

use crate::{build_engine, registry, ssh_adapter};

pub struct BuildPlanEntry {
    pub service_name: String,
    pub build: build_engine::ResolvedBuildConfig,
    pub platforms: Vec<String>,
    pub version_ref: String,
    pub latest_ref: String,
}

/// Where builds actually run, resolved purely from configuration -- no SSH connection is
/// attempted here. `Remote`'s `connect_options` is fully resolved (URI overrides applied on top
/// of `ssh.*`/`~/.ssh/config`) so a later connect step never re-derives it.
#[derive(Debug)]
pub enum ExecutorTarget {
    Local,
    Remote {
        connect_options: Box<ConnectOptions>,
    },
}

impl ExecutorTarget {
    pub fn identity(&self) -> String {
        match self {
            ExecutorTarget::Local => "local".to_string(),
            ExecutorTarget::Remote {
                connect_options, ..
            } => format!(
                "{}@{}:{}",
                connect_options.user, connect_options.host, connect_options.port
            ),
        }
    }
}

/// Resolves which executor a build runs on. Assumes `validate_config` already ran (both call
/// sites run it first), so `builder.local`/`builder.remote` are known to be in one of the two
/// valid states; the remaining checks here are defense in depth, not the primary guard.
pub fn select_executor(config: &Config) -> anyhow::Result<ExecutorTarget> {
    let builder = &config.builder;
    if builder.local {
        return Ok(ExecutorTarget::Local);
    }

    let raw_remote = builder.remote.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "'builder.local: false' requires `builder.remote` to be set to `ssh://[user@]hostname[:port]`."
        )
    })?;
    let remote = jiji_config::parse_remote_builder_uri(raw_remote)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let ssh = config.ssh.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Remote builder '{raw_remote}' requires an `ssh:` section for authentication (keys, jump hosts, etc)."
        )
    })?;
    let connect_options = ssh_adapter::connect_options_for_remote_builder(&remote, ssh)?;

    Ok(ExecutorTarget::Remote {
        connect_options: Box::new(connect_options),
    })
}

pub fn select_buildable_services(
    config: &Config,
    service_filters: &[String],
) -> anyhow::Result<Vec<String>> {
    let mut services: Vec<String> = config
        .services
        .iter()
        .filter(|(name, service)| {
            service.build.is_some()
                && (service_filters.is_empty()
                    || service_filters
                        .iter()
                        .any(|filter| jiji_core::matches_pattern(name, filter)))
        })
        .map(|(name, _)| name.clone())
        .collect();
    services.sort();
    if services.is_empty() {
        if service_filters.is_empty() {
            anyhow::bail!(
                "No service has `build:` configured. Add build configuration before running `jiji build`."
            );
        }
        anyhow::bail!(
            "No build-configured service matched --services '{}'. Check the filter or add `build:` to a matching service.",
            service_filters.join(",")
        );
    }
    Ok(services)
}

pub fn compute_plan(
    config: &Config,
    project: &str,
    services: &[String],
    version_tag: &str,
) -> anyhow::Result<Vec<BuildPlanEntry>> {
    services
        .iter()
        .map(|name| {
            let service = config.services.get(name).with_context(|| {
                format!("Build service '{name}' is not defined in configuration")
            })?;
            let build = service
                .build
                .as_ref()
                .with_context(|| format!("Service '{name}' has no `build:` configuration"))?;
            Ok(BuildPlanEntry {
                service_name: name.clone(),
                build: build_engine::resolve_build_config(build),
                platforms: build_engine::required_arches(config, service),
                version_ref: registry::full_image_name(
                    &config.builder.registry,
                    project,
                    name,
                    version_tag,
                )?,
                latest_ref: registry::full_image_name(
                    &config.builder.registry,
                    project,
                    name,
                    "latest",
                )?,
            })
        })
        .collect()
}

pub fn render_plan_summary(plan: &[BuildPlanEntry]) -> String {
    plan.iter()
        .map(|entry| {
            format!(
                "{}: {} [{}]",
                entry.service_name,
                entry.version_ref,
                entry.platforms.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(clippy::too_many_arguments)]
pub async fn build_one(
    entry: &BuildPlanEntry,
    executor: &crate::build_executor::BuildExecutor,
    engine: ContainerEngine,
    no_cache: bool,
    push: bool,
    project: &str,
    project_root: &Path,
    local_registry: bool,
) -> anyhow::Result<()> {
    executor
        .build_and_push(
            entry,
            engine,
            no_cache,
            push,
            project,
            project_root,
            local_registry,
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_config::Config;

    fn config(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).expect("test fixture must be valid YAML")
    }

    #[test]
    fn select_executor_returns_local_by_default() {
        let config = config(
            r#"
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
"#,
        );
        assert!(matches!(
            select_executor(&config).expect("resolve"),
            ExecutorTarget::Local
        ));
        assert_eq!(select_executor(&config).unwrap().identity(), "local");
    }

    #[test]
    fn select_executor_resolves_remote_identity_from_uri_and_ssh_section() {
        let config = config(
            r#"
project: demo
builder:
  engine: podman
  local: false
  remote: ssh://build@10.0.0.9:2222
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
ssh:
  user: fallback
"#,
        );
        let target = select_executor(&config).expect("resolve");
        assert_eq!(target.identity(), "build@10.0.0.9:2222");
    }

    #[test]
    fn select_executor_remote_without_ssh_section_is_a_clear_error() {
        let config = config(
            r#"
project: demo
builder:
  engine: podman
  local: false
  remote: ssh://build@10.0.0.9
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
"#,
        );
        let error = select_executor(&config).expect_err("reject");
        assert!(error.to_string().contains("`ssh:` section"));
    }

    #[test]
    fn select_executor_does_not_perform_any_network_io() {
        // TEST-NET-3 (RFC 5737): guaranteed non-routable, so a hang here would mean this
        // resolution path is doing more than pure config parsing.
        let config = config(
            r#"
project: demo
builder:
  engine: podman
  local: false
  remote: ssh://build@203.0.113.1
servers:
  web:
    host: 10.0.0.1
services:
  app:
    image: nginx:latest
    servers: [web]
ssh:
  user: fallback
"#,
        );
        let target = select_executor(&config).expect("resolve without connecting");
        assert_eq!(target.identity(), "build@203.0.113.1:22");
    }
}
