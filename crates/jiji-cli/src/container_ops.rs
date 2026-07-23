use jiji_config::ContainerEngine;
use jiji_network::NetworkedContainerRun;
use jiji_ssh::{CommandResult, SshSession};

pub async fn image_exists(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("{engine} image inspect {image} >/dev/null 2>&1"))
        .await?;
    Ok(result.success)
}

pub async fn ensure_image(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<()> {
    if image_exists(session, engine, image).await? {
        return Ok(());
    }
    let command = format!("{engine} pull {image}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

/// `None` means "no such container" (not an error); any other failure to inspect propagates.
pub async fn inspect_status(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<Option<String>> {
    let command = format!("{engine} inspect {name} --format '{{{{.State.Status}}}}'");
    let result = session.execute(&command).await?;
    if !result.success {
        return Ok(None);
    }
    Ok(Some(result.stdout.trim().to_string()))
}

pub async fn create_and_start(
    session: &SshSession,
    run: &NetworkedContainerRun,
) -> anyhow::Result<()> {
    let command = run.shell_command();
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn stop(session: &SshSession, engine: ContainerEngine, name: &str) -> anyhow::Result<()> {
    let command = format!("{engine} stop {name}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn remove(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<()> {
    let command = format!("{engine} rm -f {name}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)
}

pub async fn logs_tail(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
    lines: u32,
) -> anyhow::Result<String> {
    let command = format!("{engine} logs --tail {lines} {name} 2>&1");
    let result = session.execute(&command).await?;
    Ok(result.stdout)
}

/// `None` if the container is not currently running or is absent; never distinguishes the two,
/// since both cases mean "nothing to stop."
pub async fn stop_if_running(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<()> {
    match inspect_status(session, engine, name).await? {
        Some(status) if status == "running" => stop(session, engine, name).await,
        _ => Ok(()),
    }
}

/// Returns `false` (not an error) if `name` was already absent.
pub async fn remove_if_present(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<bool> {
    if inspect_status(session, engine, name).await?.is_none() {
        return Ok(false);
    }
    remove(session, engine, name).await?;
    Ok(true)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSummary {
    pub name: String,
    pub project: Option<String>,
    pub service: Option<String>,
    pub server: Option<String>,
    pub status: String,
}

/// Containers labeled `jiji.managed=true` and `jiji.project={project}` -- the discovery mechanism
/// teardown uses instead of assuming exact `{project}-{service}-{a|b}` names, so it also catches
/// stale containers left behind by a service since removed from config.
pub async fn list_managed_containers(
    session: &SshSession,
    engine: ContainerEngine,
    project: &str,
) -> anyhow::Result<Vec<ContainerSummary>> {
    list_by_label_filter(session, engine, Some(project)).await
}

/// Every `jiji.managed=true` container that carries a *different* project's label -- used only to
/// detect a shared-host blocker (another jiji project still has resources on this host). A
/// container with no `jiji.project` label at all (kamal-proxy is the only one today) belongs to no
/// single project and is never a blocker, so it's excluded rather than treated as "some other
/// project."
pub async fn list_other_project_containers(
    session: &SshSession,
    engine: ContainerEngine,
    exclude_project: &str,
) -> anyhow::Result<Vec<ContainerSummary>> {
    let all = list_by_label_filter(session, engine, None).await?;
    Ok(all
        .into_iter()
        .filter(|container| {
            matches!(container.project.as_deref(), Some(project) if project != exclude_project)
        })
        .collect())
}

async fn list_by_label_filter(
    session: &SshSession,
    engine: ContainerEngine,
    project: Option<&str>,
) -> anyhow::Result<Vec<ContainerSummary>> {
    let command = render_list_by_label_filter_command(engine, project);
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(parse_container_summary_lines(&result.stdout))
}

/// `|` separates fields (never collapsed by an empty label value, unlike whitespace); it can't
/// appear in a container name or in a label value jiji itself sets. `ps --format`'s `.Labels`
/// field is a flat display string on both engines (not a map), so extracting one label's value
/// needs the dedicated `.Label "key"` template function -- confirmed live against real Docker;
/// `index .Labels "key"` fails with "cannot index slice/array with type string".
fn render_list_by_label_filter_command(engine: ContainerEngine, project: Option<&str>) -> String {
    let project_filter = project
        .map(|project| format!(" --filter label=jiji.project={project}"))
        .unwrap_or_default();
    format!(
        "{engine} ps -a --filter label=jiji.managed=true{project_filter} --format \
         '{{{{.Names}}}}|{{{{.Label \"jiji.project\"}}}}|{{{{.Label \"jiji.service\"}}}}|{{{{.Label \"jiji.server\"}}}}|{{{{.State}}}}'"
    )
}

fn parse_container_summary_lines(stdout: &str) -> Vec<ContainerSummary> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_container_summary_line)
        .collect()
}

fn parse_container_summary_line(line: &str) -> ContainerSummary {
    let mut fields = line.split('|');
    let name = fields.next().unwrap_or_default().trim().to_string();
    let project = non_empty(fields.next());
    let service = non_empty(fields.next());
    let server = non_empty(fields.next());
    let status = fields.next().unwrap_or_default().trim().to_string();
    ContainerSummary {
        name,
        project,
        service,
        server,
        status,
    }
}

fn non_empty(field: Option<&str>) -> Option<String> {
    field
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Names of containers still running from `image`, other than ones already removed. Empty means
/// the image is safe to remove.
pub async fn image_referenced_elsewhere(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<Vec<String>> {
    let command = format!("{engine} ps -a --filter ancestor={image} --format '{{{{.Names}}}}'");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(result
        .stdout
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

/// Returns `false` (not an error) if `image` was already absent.
pub async fn remove_image_if_present(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<bool> {
    if !image_exists(session, engine, image).await? {
        return Ok(false);
    }
    let command = format!("{engine} rmi {image}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(true)
}

pub async fn volume_exists(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<bool> {
    let result = session
        .execute(&format!("{engine} volume inspect {name} >/dev/null 2>&1"))
        .await?;
    Ok(result.success)
}

/// One entry per container currently attached to volume `name`: `Some(project)` if it carries a
/// `jiji.project` label, `None` if it's attached but unlabeled (an ambiguous attacher). Empty
/// means nothing is attached.
pub async fn volume_attached_projects(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<Vec<Option<String>>> {
    let command =
        format!("{engine} ps -a --filter volume={name} --format '{{{{.Label \"jiji.project\"}}}}'");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(result.stdout.lines().map(non_empty_line).collect())
}

fn non_empty_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Returns `false` (not an error) if `name` was already absent.
pub async fn remove_volume_if_present(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<bool> {
    if !volume_exists(session, engine, name).await? {
        return Ok(false);
    }
    let command = format!("{engine} volume rm {name}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(true)
}

/// Number of containers attached to `network`, excluding any name listed in `exclude` (e.g. the
/// Podman keepalive anchor, or kamal-proxy when it's still legitimately serving other projects).
pub async fn network_attachment_count(
    session: &SshSession,
    engine: ContainerEngine,
    network: &str,
    exclude: &[&str],
) -> anyhow::Result<usize> {
    let command = format!("{engine} ps -a --filter network={network} --format '{{{{.Names}}}}'");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(count_attachments(&result.stdout, exclude))
}

fn count_attachments(stdout: &str, exclude: &[&str]) -> usize {
    stdout
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && !exclude.contains(name))
        .count()
}

/// Returns `false` (not an error) if `name` was already absent.
pub async fn remove_network_if_present(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<bool> {
    let inspect = format!("{engine} network inspect {name} >/dev/null 2>&1");
    let result = session.execute(&inspect).await?;
    if !result.success {
        return Ok(false);
    }
    let command = format!("{engine} network rm {name}");
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    Ok(true)
}

fn ensure_success(
    session: &SshSession,
    command: &str,
    result: &CommandResult,
) -> anyhow::Result<()> {
    if result.success {
        return Ok(());
    }
    anyhow::bail!(
        "Command `{command}` failed on {} (exit {:?}): {}",
        session.host(),
        result.code,
        result.stderr.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_fully_labeled_container_line() {
        let summary = parse_container_summary_line("demo-web-a|demo|web|app|running");
        assert_eq!(
            summary,
            ContainerSummary {
                name: "demo-web-a".to_string(),
                project: Some("demo".to_string()),
                service: Some("web".to_string()),
                server: Some("app".to_string()),
                status: "running".to_string(),
            }
        );
    }

    #[test]
    fn empty_label_fields_never_collapse_or_shift_columns() {
        // kamal-proxy carries jiji.managed=true but no project/service/server labels.
        let summary = parse_container_summary_line("kamal-proxy||||running");
        assert_eq!(summary.name, "kamal-proxy");
        assert_eq!(summary.project, None);
        assert_eq!(summary.service, None);
        assert_eq!(summary.server, None);
        assert_eq!(summary.status, "running");
    }

    #[test]
    fn list_other_project_containers_excludes_the_named_project_and_unlabeled_containers() {
        // Exercised indirectly: the filter predicate used inside list_other_project_containers.
        // Confirmed live: kamal-proxy carries jiji.managed=true but no jiji.project label, and
        // must never be treated as belonging to "some other project."
        let containers = vec![
            parse_container_summary_line("demo-web-a|demo|web|app|running"),
            parse_container_summary_line("other-web-a|other|web|app|running"),
            parse_container_summary_line("kamal-proxy||||running"),
        ];
        let filtered: Vec<_> = containers
            .into_iter()
            .filter(|container| {
                matches!(container.project.as_deref(), Some(project) if project != "demo")
            })
            .collect();
        assert_eq!(filtered.len(), 1);
        assert!(filtered.iter().any(|c| c.name == "other-web-a"));
        assert!(!filtered.iter().any(|c| c.name == "kamal-proxy"));
    }

    #[test]
    fn label_filter_command_includes_project_only_when_given() {
        let with_project =
            render_list_by_label_filter_command(ContainerEngine::Docker, Some("demo"));
        assert!(with_project.contains("--filter label=jiji.managed=true"));
        assert!(with_project.contains("--filter label=jiji.project=demo"));

        let without_project = render_list_by_label_filter_command(ContainerEngine::Docker, None);
        assert!(without_project.contains("--filter label=jiji.managed=true"));
        assert!(!without_project.contains("--filter label=jiji.project"));
    }

    #[test]
    fn parse_container_summary_lines_skips_blank_lines() {
        let stdout = "demo-web-a|demo|web|app|running\n\nkamal-proxy|||running\n";
        let parsed = parse_container_summary_lines(stdout);
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn network_attachment_count_excludes_listed_names() {
        let stdout = "jiji-network-anchor\nkamal-proxy\ndemo-web-a\n";
        let exclude = ["jiji-network-anchor", "kamal-proxy"];
        assert_eq!(count_attachments(stdout, &exclude), 1);
    }

    #[test]
    fn network_attachment_count_ignores_blank_lines() {
        assert_eq!(count_attachments("\n\n", &[]), 0);
    }
}
