use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jiji_config::{load_config, validate_config, ContainerEngine, NamedServer};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession};
use jiji_tui::Ui;

use crate::teardown_plan::ServerTeardownPlan;
use crate::{
    container_ops, env_resolution, image_teardown, network_teardown, proxy_teardown,
    service_network, ssh_adapter, teardown_plan, volume_teardown,
};

#[derive(Debug)]
enum TeardownStepResult {
    Removed,
    AlreadyAbsent,
    Retained { reason: String },
    Failed { error: String },
}

enum HostTeardownOutcome {
    Unreachable {
        error: String,
    },
    Blocked {
        blockers: Vec<String>,
    },
    /// Dry run only: successfully discovered and not blocked, but never executed.
    Planned,
    Completed {
        steps: Vec<(String, TeardownStepResult)>,
    },
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    environment: Option<&str>,
    config_file: Option<&str>,
    hosts: Option<&str>,
    services: Option<&str>,
    yes: bool,
    remove_volumes: bool,
    dry_run: bool,
) -> anyhow::Result<()> {
    Ui::section("Server Teardown:");

    let start = std::env::current_dir()?;
    let config_path = config_file.map(std::path::Path::new);
    let (config, path) = load_config(environment, config_path, &start)?;
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

    if services.is_some() {
        anyhow::bail!(
            "`jiji server teardown` does not accept -S/--services: host infrastructure cannot be partially torn down. Use -H/--hosts to select servers instead."
        );
    }

    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running server teardown.",
            path.display()
        )
    })?;

    if config.servers.is_empty() {
        anyhow::bail!("No servers are configured in {}.", path.display());
    }

    // Unlike `network setup`, teardown must remove whatever was actually installed previously,
    // not reflect whether networking is currently enabled in config -- so the plan is always
    // built, even when `network.enabled` is false.
    let network_plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the private network plan: {error}"))?;

    let host_filters = split_comma_trimmed(hosts);
    let target_names: BTreeSet<String> = network_plan
        .select_hosts(&host_filters)?
        .into_iter()
        .map(|server| server.name.clone())
        .collect();

    let mut named_servers: Vec<(String, NamedServer)> = config
        .servers
        .iter()
        .filter(|(name, _)| target_names.contains(*name))
        .map(|(name, server)| (name.clone(), server.clone()))
        .collect();
    named_servers.sort_by(|a, b| a.0.cmp(&b.0));

    Ui::say(
        &format!(
            "Targeting {} server(s): {}",
            named_servers.len(),
            named_servers
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        1,
    );

    let engine = config.builder.engine;
    let project = config.project.clone();

    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let mut connect_operations = Vec::with_capacity(named_servers.len());
    for (name, server) in &named_servers {
        connect_operations.push(ssh_adapter::connect_options(name, server, &ssh)?);
    }

    Ui::section("Connecting:");
    let operations: Vec<_> = connect_operations
        .into_iter()
        .map(|options| move || async move { SshSession::connect(&options).await })
        .collect();
    let connections = pool.execute_concurrent(operations).await;

    let mut sessions: BTreeMap<String, Arc<SshSession>> = BTreeMap::new();
    let mut outcomes: BTreeMap<String, HostTeardownOutcome> = BTreeMap::new();
    for ((name, server), connection) in named_servers.iter().zip(connections) {
        match connection {
            Ok(session) => {
                Ui::say(&format!("{name} ({}): connected", server.host), 1);
                sessions.insert(name.clone(), Arc::new(session));
            }
            Err(error) => {
                // Deliberately does not abort the whole run: one unreachable host must not hide
                // successful teardown work on the rest.
                Ui::error(&format!("{name} ({}): {error}", server.host));
                outcomes.insert(
                    name.clone(),
                    HostTeardownOutcome::Unreachable {
                        error: error.to_string(),
                    },
                );
            }
        }
    }

    Ui::section("Discovering:");
    let mut plans: BTreeMap<String, ServerTeardownPlan> = BTreeMap::new();
    for (name, session) in &sessions {
        match teardown_plan::discover(session, engine, &config, &project, name, remove_volumes)
            .await
        {
            Ok(plan) => {
                if teardown_plan::has_blockers(&plan) {
                    for blocker in &plan.blockers {
                        Ui::warn(&format!("{name}: {blocker}"));
                    }
                    outcomes.insert(
                        name.clone(),
                        HostTeardownOutcome::Blocked {
                            blockers: plan.blockers.clone(),
                        },
                    );
                } else {
                    Ui::say(&teardown_plan::render_summary(&plan), 1);
                    plans.insert(name.clone(), plan);
                }
            }
            Err(error) => {
                Ui::error(&format!(
                    "{name}: could not discover teardown plan: {error}"
                ));
                outcomes.insert(
                    name.clone(),
                    HostTeardownOutcome::Unreachable {
                        error: error.to_string(),
                    },
                );
            }
        }
    }

    if plans.is_empty() {
        close_all(&sessions).await;
        return print_summary_and_exit(&outcomes);
    }

    if dry_run {
        Ui::say("Dry run: no changes were made.", 1);
        for name in plans.keys() {
            outcomes.insert(name.clone(), HostTeardownOutcome::Planned);
        }
        close_all(&sessions).await;
        return print_summary_and_exit(&outcomes);
    }

    Ui::section("About to tear down:");
    if remove_volumes {
        Ui::warn("--volumes will permanently delete jiji-owned named volumes (application data) for the servers listed above.");
    }
    Ui::say(
        "Unrelated container engine resources (containers, images, volumes, networks not owned by jiji) will be retained.",
        1,
    );

    if !yes {
        let confirmed = Ui::confirm_typed(
            &format!("Type the project name \"{project}\" to continue"),
            &project,
        )?;
        if !confirmed {
            close_all(&sessions).await;
            anyhow::bail!("Teardown cancelled: project name did not match.");
        }
    }

    Ui::section("Tearing Down:");
    for (name, plan) in &plans {
        let session = sessions.get(name).expect("connected above");
        Ui::say(&format!("{name}:"), 1);
        let steps = execute_host_teardown(
            session,
            engine,
            &network_plan,
            &project,
            plan,
            remove_volumes,
        )
        .await;
        for (resource, result) in &steps {
            match result {
                TeardownStepResult::Removed => Ui::say(&format!("{resource}: removed"), 2),
                TeardownStepResult::AlreadyAbsent => {
                    Ui::say(&format!("{resource}: already absent"), 2)
                }
                TeardownStepResult::Retained { reason } => {
                    Ui::warn(&format!("  {resource}: retained ({reason})"))
                }
                TeardownStepResult::Failed { error } => {
                    Ui::error(&format!("  {resource}: failed ({error})"))
                }
            }
        }
        outcomes.insert(name.clone(), HostTeardownOutcome::Completed { steps });
    }

    close_all(&sessions).await;
    print_summary_and_exit(&outcomes)
}

/// Runs every teardown step for one host, in the order the approved plan specifies: proxy routes,
/// then application containers, then (optionally) volumes, then images, then the shared proxy
/// container, then the service VIP/NAT mappings, then the network layer. Never returns an error
/// itself -- every failure is captured as a `Failed` step so the rest of this host's teardown (and
/// every other host) can still proceed.
async fn execute_host_teardown(
    session: &SshSession,
    engine: ContainerEngine,
    network_plan: &jiji_network::NetworkPlan,
    project: &str,
    plan: &ServerTeardownPlan,
    remove_volumes: bool,
) -> Vec<(String, TeardownStepResult)> {
    let mut steps = Vec::new();

    match proxy_teardown::remove_project_routes(
        session,
        engine,
        &plan.proxy_routes,
        &plan.proxy_routes,
    )
    .await
    {
        Ok(results) => {
            for (route, removed) in results {
                steps.push((format!("proxy route '{route}'"), present_or_absent(removed)));
            }
        }
        Err(error) => steps.push((
            "proxy routes".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        )),
    }

    let mut application_layer_failed = false;
    for container in &plan.containers {
        if let Err(error) = container_ops::stop_if_running(session, engine, &container.name).await {
            steps.push((
                format!("container '{}'", container.name),
                TeardownStepResult::Failed {
                    error: error.to_string(),
                },
            ));
            application_layer_failed = true;
            continue;
        }
        match container_ops::remove_if_present(session, engine, &container.name).await {
            Ok(removed) => steps.push((
                format!("container '{}'", container.name),
                present_or_absent(removed),
            )),
            Err(error) => {
                steps.push((
                    format!("container '{}'", container.name),
                    TeardownStepResult::Failed {
                        error: error.to_string(),
                    },
                ));
                application_layer_failed = true;
            }
        }
    }

    if remove_volumes {
        match volume_teardown::remove(session, engine, &plan.volumes).await {
            Ok(results) => {
                for (name, removed) in results {
                    let matching = plan.volumes.iter().find(|volume| volume.name == name);
                    let service = matching.map_or("unknown", |volume| volume.service.as_str());
                    let result = if removed {
                        TeardownStepResult::Removed
                    } else {
                        match matching.and_then(|volume| volume.blocked_by.clone()) {
                            Some(reason) => TeardownStepResult::Retained { reason },
                            None => TeardownStepResult::AlreadyAbsent,
                        }
                    };
                    steps.push((format!("volume '{name}' (service '{service}')"), result));
                }
            }
            Err(error) => steps.push((
                "volumes".to_string(),
                TeardownStepResult::Failed {
                    error: error.to_string(),
                },
            )),
        }
    }

    // Image removal correctness depends on this project's containers already being gone (so
    // "referenced elsewhere" only ever means a genuinely unrelated container); skip it if that
    // didn't fully succeed rather than risk removing an image a still-running container needs.
    if !application_layer_failed {
        match image_teardown::discover_and_remove(session, engine, &plan.images).await {
            Ok(results) => {
                for (image, outcome) in results {
                    let result = match outcome {
                        image_teardown::ImageOutcome::Removed => TeardownStepResult::Removed,
                        image_teardown::ImageOutcome::NotPresent => {
                            TeardownStepResult::AlreadyAbsent
                        }
                        image_teardown::ImageOutcome::RetainedInUseBy(names) => {
                            TeardownStepResult::Retained {
                                reason: format!("still used by {}", names.join(", ")),
                            }
                        }
                    };
                    steps.push((format!("image '{image}'"), result));
                }
            }
            Err(error) => steps.push((
                "images".to_string(),
                TeardownStepResult::Failed {
                    error: error.to_string(),
                },
            )),
        }
    }

    // The staged env-file/mount-upload tree (env_resolution::project_staging_dir) is
    // deploy-generated scratch data, not user-facing persistent storage -- it includes resolved
    // secrets in plaintext, so it's removed by default rather than gated behind --volumes.
    if plan.project_directory_exists {
        let path = env_resolution::project_staging_dir(project);
        let command = format!("rm -rf {path}");
        match session.execute(&command).await {
            Ok(result) if result.success => steps.push((
                "project staging directory".to_string(),
                TeardownStepResult::Removed,
            )),
            Ok(result) => steps.push((
                "project staging directory".to_string(),
                TeardownStepResult::Failed {
                    error: result.stderr.trim().to_string(),
                },
            )),
            Err(error) => steps.push((
                "project staging directory".to_string(),
                TeardownStepResult::Failed {
                    error: error.to_string(),
                },
            )),
        }
    } else {
        steps.push((
            "project staging directory".to_string(),
            TeardownStepResult::AlreadyAbsent,
        ));
    }

    let kamal_proxy_still_running =
        match proxy_teardown::teardown_proxy_container_if_unused(session, engine).await {
            Ok(proxy_teardown::ProxyContainerOutcome::Removed) => {
                steps.push((
                    "kamal-proxy container".to_string(),
                    TeardownStepResult::Removed,
                ));
                false
            }
            Ok(proxy_teardown::ProxyContainerOutcome::AlreadyAbsent) => {
                steps.push((
                    "kamal-proxy container".to_string(),
                    TeardownStepResult::AlreadyAbsent,
                ));
                false
            }
            Ok(proxy_teardown::ProxyContainerOutcome::RetainedInUseBy(routes)) => {
                steps.push((
                    "kamal-proxy container".to_string(),
                    TeardownStepResult::Retained {
                        reason: format!("still serving route(s): {}", routes.join(", ")),
                    },
                ));
                true
            }
            Err(error) => {
                steps.push((
                    "kamal-proxy container".to_string(),
                    TeardownStepResult::Failed {
                        error: error.to_string(),
                    },
                ));
                // Unknown state: assume it's still in use rather than risk tearing down the
                // network out from under a proxy we couldn't verify.
                true
            }
        };

    // A prior teardown run may have already removed /etc/jiji/network entirely, in which case
    // there is no active-slots file left to read -- `installed_generation` (read from that same
    // tree) is a reliable proxy for "nothing is left to deactivate," avoiding a hard error from
    // `load_active_slots` that would otherwise make a second teardown run non-idempotent.
    if plan.network.installed_generation.is_none() {
        steps.push((
            "service VIP mappings".to_string(),
            TeardownStepResult::AlreadyAbsent,
        ));
    } else {
        match service_network::deactivate_project(session, network_plan, project).await {
            Ok(()) => steps.push((
                "service VIP mappings".to_string(),
                TeardownStepResult::Removed,
            )),
            Err(error) => steps.push((
                "service VIP mappings".to_string(),
                TeardownStepResult::Failed {
                    error: error.to_string(),
                },
            )),
        }
    }

    // Application removal must precede network removal, and a failed application-layer step must
    // not silently continue into destroying network infrastructure a still-running container (or
    // one we couldn't confirm was removed) still depends on.
    if application_layer_failed {
        steps.push((
            "network layer".to_string(),
            TeardownStepResult::Retained {
                reason: "skipped because application-layer teardown had failures".to_string(),
            },
        ));
        return steps;
    }

    // Must run before stopping jiji-network-restore.service: that unit is what starts the Podman
    // anchor container, so its conmon process lives in the unit's cgroup until the anchor is gone.
    // Disabling the unit first stalls teardown for roughly a minute waiting on a control-group kill.
    match network_teardown::remove_anchor_if_present(session, engine).await {
        Ok(was_present) => steps.push((
            "podman network anchor container".to_string(),
            present_or_absent(was_present),
        )),
        Err(error) => steps.push((
            "podman network anchor container".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        )),
    }

    if let Err(error) = network_teardown::stop_and_disable_units(session, engine).await {
        steps.push((
            "network systemd units".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        ));
    } else {
        steps.push((
            "network systemd units".to_string(),
            TeardownStepResult::Removed,
        ));
    }

    if let Err(error) = network_teardown::remove_wireguard(session).await {
        steps.push((
            "wireguard configuration".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        ));
    } else {
        steps.push((
            "wireguard configuration".to_string(),
            TeardownStepResult::Removed,
        ));
    }

    if let Err(error) = network_teardown::remove_nftables(session).await {
        steps.push((
            "service-nat nftables table".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        ));
    } else {
        steps.push((
            "service-nat nftables table".to_string(),
            TeardownStepResult::Removed,
        ));
    }

    match network_teardown::remove_bridge_and_engine_network(
        session,
        engine,
        kamal_proxy_still_running,
    )
    .await
    {
        Ok(network_teardown::NetworkRemovalOutcome::Removed) => steps.push((
            "jiji bridge network".to_string(),
            TeardownStepResult::Removed,
        )),
        Ok(network_teardown::NetworkRemovalOutcome::AlreadyAbsent) => steps.push((
            "jiji bridge network".to_string(),
            TeardownStepResult::AlreadyAbsent,
        )),
        Ok(network_teardown::NetworkRemovalOutcome::RetainedAttached(count)) => steps.push((
            "jiji bridge network".to_string(),
            TeardownStepResult::Retained {
                reason: format!("{count} container(s) still attached"),
            },
        )),
        Err(error) => steps.push((
            "jiji bridge network".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        )),
    }

    if let Err(error) = network_teardown::remove_compiled_state(session).await {
        steps.push((
            "compiled network state".to_string(),
            TeardownStepResult::Failed {
                error: error.to_string(),
            },
        ));
    } else {
        steps.push((
            "compiled network state".to_string(),
            TeardownStepResult::Removed,
        ));
    }

    steps
}

fn present_or_absent(was_present: bool) -> TeardownStepResult {
    if was_present {
        TeardownStepResult::Removed
    } else {
        TeardownStepResult::AlreadyAbsent
    }
}

/// Prints the final per-host bucket and returns a non-zero exit (via `Err`) if any host wasn't
/// fully torn down. A `Retained` step is a safe, sometimes-expected outcome (e.g. a shared
/// kamal-proxy still serving another project) and never counts as a failure by itself; only
/// `Failed` steps, blocked hosts, and unreachable hosts do.
fn print_summary_and_exit(outcomes: &BTreeMap<String, HostTeardownOutcome>) -> anyhow::Result<()> {
    Ui::section("Teardown Summary:");
    let mut failures = 0usize;
    for (name, outcome) in outcomes {
        match outcome {
            HostTeardownOutcome::Unreachable { error } => {
                Ui::error(&format!("{name}: unreachable ({error})"));
                failures += 1;
            }
            HostTeardownOutcome::Blocked { blockers } => {
                Ui::warn(&format!("{name}: blocked ({} blocker(s))", blockers.len()));
                failures += 1;
            }
            HostTeardownOutcome::Planned => {
                Ui::say(&format!("{name}: plan ready (dry run, nothing changed)"), 1);
            }
            HostTeardownOutcome::Completed { steps } => {
                let failed = steps
                    .iter()
                    .filter(|(_, result)| matches!(result, TeardownStepResult::Failed { .. }))
                    .count();
                if failed > 0 {
                    Ui::error(&format!(
                        "{name}: partially torn down ({failed} step(s) failed)"
                    ));
                    failures += 1;
                } else {
                    Ui::success(&format!("{name}: fully torn down"));
                }
            }
        }
    }
    if failures > 0 {
        anyhow::bail!("Teardown incomplete for {failures} server(s); see the summary above.");
    }
    Ok(())
}

fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

async fn close_all(sessions: &BTreeMap<String, Arc<SshSession>>) {
    for session in sessions.values() {
        session.close().await;
    }
}
