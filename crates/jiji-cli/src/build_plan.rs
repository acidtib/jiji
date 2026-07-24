use std::path::Path;

use anyhow::Context;
use jiji_config::{Builder, Config, ContainerEngine};

use crate::{build_engine, registry};

pub struct BuildPlanEntry {
    pub service_name: String,
    pub build: build_engine::ResolvedBuildConfig,
    pub platforms: Vec<String>,
    pub version_ref: String,
    pub latest_ref: String,
}

pub fn check_scope_guards(builder: &Builder) -> anyhow::Result<()> {
    if !builder.local {
        anyhow::bail!(
            "Remote builders are not implemented yet. Set `builder.local: true`; builds currently run on this machine."
        );
    }
    Ok(())
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

pub async fn build_one(
    entry: &BuildPlanEntry,
    engine: ContainerEngine,
    no_cache: bool,
    push: bool,
    project: &str,
    project_root: &Path,
    local_registry: bool,
) -> anyhow::Result<()> {
    build_engine::build_and_push(
        engine,
        &entry.build,
        no_cache,
        &entry.platforms,
        &[entry.version_ref.clone(), entry.latest_ref.clone()],
        push,
        project,
        &entry.service_name,
        project_root,
        local_registry,
    )
    .await
}
