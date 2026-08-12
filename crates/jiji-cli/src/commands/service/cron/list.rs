use std::collections::{BTreeMap, BTreeSet};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_config::{validate_config, Config, Service};
use jiji_network::{NetworkPlan, NetworkPlanner};
use jiji_tui::Ui;

use super::select_cron_services;
use crate::{container_runtime, mounts};

/// Reads local configuration for the job list itself, then connects to each selected service's
/// current owner (`cron_reconcile::find_owner`) to determine installation state: `not-deployed`
/// (no matching spec installed there), `installed` (installed spec's canonical hash matches what
/// re-applying the current config would produce), or `drifted` (it doesn't -- the plan's
/// "Configuration Reconciliation" section: `list` is what surfaces a schedule/command/etc. change
/// that hasn't been picked up by a deploy yet).
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    services: Option<&str>,
) -> anyhow::Result<()> {
    let start = std::env::current_dir()?;
    let (config, path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    Ui::say(&format!("Configuration loaded from: {}", path.display()), 1);

    let validation = validate_config(&config);
    if !validation.valid {
        Ui::error(&format!(
            "Configuration validation failed with {} error(s):",
            validation.errors.len()
        ));
        for e in &validation.errors {
            Ui::say(&format!("{}: {}", e.path, e.message), 1);
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }

    Ui::section("Service Cron Jobs:");
    let rows = select_cron_services(&config, services);
    if rows.is_empty() {
        Ui::say("No cron jobs are configured for the selected services.", 1);
        return Ok(());
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service cron list.",
            path.display()
        )
    })?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let service_names: BTreeSet<&str> = rows
        .iter()
        .map(|(service_name, _, _)| *service_name)
        .collect();
    for service_name in service_names {
        let service = &config.services[service_name];
        list_service_crons(&ssh, &config, &plan, service_name, service).await;
    }
    Ok(())
}

async fn list_service_crons(
    ssh: &jiji_config::Ssh,
    config: &Config,
    plan: &NetworkPlan,
    service_name: &str,
    service: &Service,
) {
    let (owner, resolved, newly_opened) = match crate::cron_reconcile::find_owner(
        ssh,
        config,
        service_name,
        service,
        &BTreeMap::new(),
    )
    .await
    {
        Ok(found) => found,
        Err(error) => {
            for cron_name in service.crons.keys() {
                Ui::say(
                        &format!(
                            "{service_name} {cron_name}: state=not-deployed (owner unavailable: {error})"
                        ),
                        1,
                    );
            }
            return;
        }
    };

    let installed_specs = match crate::agent_client::call(
        &owner.session,
        &config.project,
        None,
        RequestBody::CronSpecList,
    )
    .await
    {
        Ok(ResponseBody::CronSpecs { specs }) => specs,
        Ok(_) | Err(_) => Vec::new(),
    };

    // Drift detection must recompute the exact same absolute paths `cron_reconcile.rs` sends on
    // install (`remote_home_dir`/`absolutize`/`absolutize_mount_args`), or every installed job
    // would report `drifted` unconditionally: the actually-installed hash always has the
    // absolute form baked in, since `jiji-agent` spawns cron containers directly, never over SSH
    // (see `docs/architecture-notes.md#scheduled-cron-execution-crons`).
    let owner_home = crate::cron_reconcile::remote_home_dir(&owner.session).await;
    let mount_args = match (
        &owner_home,
        mounts::build_all_mount_args(service, &config.project, service_name),
    ) {
        (Ok(home), Ok(args)) => Some(crate::cron_reconcile::absolutize_mount_args(home, args)),
        _ => None,
    };
    let resource_args = container_runtime::render_resource_options(service);
    let env_file_path = owner_home.as_ref().ok().map(|home| {
        crate::cron_reconcile::absolutize(
            home,
            &crate::cron_reconcile::owner_env_file_path(
                &config.project,
                service_name,
                &owner.server_name,
            ),
        )
    });
    let server = plan.servers.get(&owner.server_name);

    for (cron_name, cron) in &service.crons {
        let installed = installed_specs
            .iter()
            .find(|spec| spec.service == service_name && spec.cron_name == *cron_name);
        let (state, ok) = match installed {
            None => ("not-deployed".to_string(), false),
            Some(installed) => match server {
                None => (
                    format!(
                        "installed (drift unknown: '{}' is not in the current network plan)",
                        owner.server_name
                    ),
                    true,
                ),
                Some(server) => match (&mount_args, &env_file_path) {
                    (None, _) | (_, None) => (
                        format!(
                            "installed (drift unknown: could not determine the cron owner's home directory on '{}')",
                            owner.server_name
                        ),
                        true,
                    ),
                    (Some(mount_args), Some(env_file_path)) => {
                        let expected = crate::cron_reconcile::render_apply_request(
                            service_name,
                            cron_name,
                            cron,
                            &owner.record.image,
                            mount_args,
                            &resource_args,
                            env_file_path,
                            &owner.record.deployment_id,
                            &owner.assignment.replica_id,
                            &server.bridge_name,
                            server.dns_address,
                            owner.record.revision,
                        );
                        let RequestBody::CronSpecApply {
                            canonical_hash: expected_hash,
                            ..
                        } = expected
                        else {
                            unreachable!("render_apply_request always returns CronSpecApply")
                        };
                        if expected_hash == installed.canonical_hash {
                            ("installed".to_string(), true)
                        } else {
                            ("drifted".to_string(), false)
                        }
                    }
                },
            },
        };
        let detail = format!(
            "{service_name} {cron_name}: schedule=\"{}\" timezone={} owner={} state={state}",
            cron.schedule, cron.timezone, owner.server_name
        );
        if ok {
            Ui::result_ok(
                &format!("{service_name}/{cron_name}"),
                &format!("{state} owner={}", owner.server_name),
            );
            Ui::say(&detail, 2);
        } else {
            Ui::result_warn(&format!("{service_name}/{cron_name}"), &state);
            Ui::say(&detail, 2);
        }
    }

    crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
}
