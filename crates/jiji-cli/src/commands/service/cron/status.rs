use std::collections::{BTreeMap, BTreeSet};

use jiji_agent::api::{RequestBody, ResponseBody};
use jiji_config::validate_config;
use jiji_tui::Ui;

use super::{format_epoch, select_cron_services};

/// Reads durable run state from each selected service's current owner (`CronStatus`, Phase 2's
/// agent API). `-H` narrows which owners are worth showing (a service whose owner isn't among the
/// matched hosts is skipped, not treated as an error, since ownership is a live fact this command
/// can't otherwise predict without connecting).
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
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

    let rows = select_cron_services(&config, services);
    if rows.is_empty() {
        anyhow::bail!(
            "No service with cron jobs matched the selected filter. Set -S to a service with a `crons:` map."
        );
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running service cron status.",
            path.display()
        )
    })?;
    let host_filters = crate::commands::deploy::split_comma_trimmed(hosts);

    Ui::section("Service Cron Status:");
    let service_names: BTreeSet<&str> = rows
        .iter()
        .map(|(service_name, _, _)| *service_name)
        .collect();
    let mut any_shown = false;
    let mut failures = Vec::new();
    for service_name in service_names {
        let service = &config.services[service_name];
        let (owner, resolved, newly_opened) = match crate::cron_reconcile::find_owner(
            &ssh,
            &config,
            service_name,
            service,
            &BTreeMap::new(),
        )
        .await
        {
            Ok(found) => found,
            Err(error) => {
                failures.push(error.to_string());
                continue;
            }
        };

        if !host_filters.is_empty()
            && !host_filters
                .iter()
                .any(|filter| jiji_core::matches_pattern(&owner.server_name, filter))
        {
            crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
            continue;
        }

        match crate::agent_client::call(
            &owner.session,
            &config.project,
            None,
            RequestBody::CronStatus {
                service: Some(service_name.to_string()),
                cron_name: None,
            },
        )
        .await
        {
            Ok(ResponseBody::CronStatuses { statuses }) => {
                for status in statuses {
                    any_shown = true;
                    Ui::say(
                        &format!(
                            "{} {}: owner={} last_scheduled={} last_started={} last_finished={} last_state={} last_exit_code={} next_due={} active_run={} skipped_overlap={}",
                            status.service,
                            status.cron_name,
                            owner.server_name,
                            format_epoch(status.last_scheduled_at),
                            format_epoch(status.last_started_at),
                            format_epoch(status.last_finished_at),
                            status
                                .last_state
                                .map(|state| format!("{state:?}"))
                                .unwrap_or_else(|| "-".to_string()),
                            status
                                .last_exit_code
                                .map(|code| code.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                            format_epoch(status.next_due_at),
                            status.active_run_id.as_deref().unwrap_or("-"),
                            status.skipped_overlap_count,
                        ),
                        1,
                    );
                }
            }
            Ok(other) => failures.push(format!(
                "service '{service_name}': agent returned an unexpected response: {other:?}"
            )),
            Err(error) => failures.push(format!("service '{service_name}': {error}")),
        }
        crate::cron_reconcile::close_newly_opened(&resolved, &newly_opened).await;
    }

    for failure in &failures {
        Ui::error(failure);
    }
    if !any_shown && failures.is_empty() {
        Ui::say("No cron job matched -H/-S after resolving ownership.", 1);
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Could not read cron status for {} service(s); see the errors above.",
            failures.len()
        );
    }
    Ok(())
}
