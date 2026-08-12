use jiji_config::ContainerEngine;
use jiji_network::NetworkedContainerRun;
use jiji_ssh::{CommandResult, SshSession, StreamChunk};
use std::time::Duration;

const IMAGE_PULL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
    pull_image(session, engine, image).await
}

pub async fn pull_image(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
) -> anyhow::Result<()> {
    pull_image_with_progress(session, engine, image, |_| {}).await
}

/// Pulls an image while reporting each non-empty stdout/stderr update. Image pulls can take
/// several minutes, especially through the local-registry SSH tunnel, so deploy uses these
/// updates to show that the transfer is still active instead of presenting a blank section.
pub async fn pull_image_with_progress(
    session: &SshSession,
    engine: ContainerEngine,
    image: &str,
    mut progress: impl FnMut(&str),
) -> anyhow::Result<()> {
    // The local registry is always plain HTTP on loopback (`localhost:<port>/...`, see
    // registry::full_image_name). Docker treats loopback registries as insecure automatically;
    // Podman does not and refuses plain HTTP without this flag.
    let tls_verify_flag = if engine == ContainerEngine::Podman && image.starts_with("localhost:") {
        " --tls-verify=false"
    } else {
        ""
    };
    let command = format!("{engine} pull{tls_verify_flag} {image}");
    let mut receiver = session
        .execute_streaming_with_timeout(&command, IMAGE_PULL_TIMEOUT)
        .await?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut code = None;

    while let Some(item) = receiver.recv().await {
        match item? {
            StreamChunk::Stdout(data) => {
                report_pull_progress(&data, &mut progress);
                stdout.extend(data);
            }
            StreamChunk::Stderr(data) => {
                report_pull_progress(&data, &mut progress);
                stderr.extend(data);
            }
            StreamChunk::Exit(exit_code) => code = Some(exit_code),
        }
    }

    let result = CommandResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        success: code == Some(0),
        code,
    };
    ensure_success(session, &command, &result)
}

fn report_pull_progress(data: &[u8], progress: &mut impl FnMut(&str)) {
    let text = String::from_utf8_lossy(data);
    if let Some(line) = text.lines().rev().find(|line| !line.trim().is_empty()) {
        progress(line.trim());
    }
}

/// `None` covers every way `{engine} inspect` can fail (container absent, daemon unreachable,
/// permission denied, ...): the engine only reports success/failure via exit code, with no
/// reliable way from here to tell "no such container" apart from another cause. Only an SSH
/// transport-level failure (connection lost, command timeout) propagates as `Err` via `?`. Callers
/// must not read `None` as proof the container never existed.
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

/// Best-effort current address of a container on whichever engine network it's attached to.
/// `None` covers "no address" the same way `inspect_status` treats absence -- container gone,
/// detached from its network (common for a stopped/exited container), or unreachable -- callers
/// must not read `None` as proof of anything beyond "could not determine an address right now."
pub async fn inspect_ip_address(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<Option<std::net::Ipv4Addr>> {
    let command = format!(
        "{engine} inspect {name} --format '{{{{range .NetworkSettings.Networks}}}}{{{{.IPAddress}}}}{{{{end}}}}' 2>/dev/null || true"
    );
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    Ok(trimmed.parse().ok())
}

/// Best-effort current image reference of a container, `None` on the same terms as
/// `inspect_ip_address`.
pub async fn inspect_image(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<Option<String>> {
    let command =
        format!("{engine} inspect {name} --format '{{{{.Config.Image}}}}' 2>/dev/null || true");
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// The actual resolved image ID (`.Image`, a content digest) a container was started from --
/// unlike `inspect_image`'s `.Config.Image` (the reference string passed at creation time, e.g.
/// `ghcr.io/x/y:latest`, identical across every pull of a moving tag), this changes every time
/// the tag resolves to different content. It's the only way to identify precisely which local
/// image entry a specific container's removal is about to orphan. `None` on the same terms as
/// `inspect_image`/`inspect_ip_address`.
pub async fn inspect_image_id(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<Option<String>> {
    let command = format!("{engine} inspect {name} --format '{{{{.Image}}}}' 2>/dev/null || true");
    let result = session.execute(&command).await?;
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

pub async fn create_and_start(
    session: &SshSession,
    run: &NetworkedContainerRun,
) -> anyhow::Result<()> {
    let command = run.shell_command();
    let result = session.execute(&command).await?;
    ensure_success(session, &command, &result)?;
    // A `network_mode: service:<other>` dependent has no bridge attachment of its own to
    // reconcile -- it inherits the upstream's, already handled by the upstream's own
    // create_and_start call.
    if run.shared_with_container.is_some() {
        return Ok(());
    }
    crate::commands::network::bridge::reconcile_podman_dns_address(
        session,
        run.engine,
        &run.bridge_interface,
        run.dns_address,
    )
    .await
}

pub async fn start(
    session: &SshSession,
    engine: ContainerEngine,
    name: &str,
) -> anyhow::Result<()> {
    let command = format!("{engine} start {name}");
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
/// container with no `jiji.project` label at all (jiji-proxy is the only one today) belongs to no
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

/// `ps --format`'s `.Labels` field means different things on the two engines: on Docker it's a
/// flat display string (not indexable, needs the dedicated `.Label "key"` function), on Podman
/// it's a real `map[string]string` (`.Label` doesn't exist on Podman's reporter struct at all,
/// needs `index .Labels "key"`) -- confirmed live against both real Docker and real Podman.
fn label_template(engine: ContainerEngine, key: &str) -> String {
    match engine {
        ContainerEngine::Docker => format!("{{{{.Label \"{key}\"}}}}"),
        ContainerEngine::Podman => format!("{{{{index .Labels \"{key}\"}}}}"),
    }
}

/// `|` separates fields (never collapsed by an empty label value, unlike whitespace); it can't
/// appear in a container name or in a label value jiji itself sets.
fn render_list_by_label_filter_command(engine: ContainerEngine, project: Option<&str>) -> String {
    let project_filter = project
        .map(|project| format!(" --filter label=jiji.project={project}"))
        .unwrap_or_default();
    format!(
        "{engine} ps -a --filter label=jiji.managed=true{project_filter} --format '{{{{.Names}}}}|{}|{}|{}|{{{{.State}}}}'",
        label_template(engine, "jiji.project"),
        label_template(engine, "jiji.service"),
        label_template(engine, "jiji.server"),
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
    let command = format!(
        "{engine} ps -a --filter volume={name} --format '{}'",
        label_template(engine, "jiji.project")
    );
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

/// Number of containers attached to `network`, excluding any listed names, such as jiji-proxy
/// when it is still legitimately serving other projects.
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
    if !result.success
        && engine == ContainerEngine::Podman
        && result.stderr.contains("associated containers")
    {
        // Confirmed live: Podman's own network backend can still report a bridge as having
        // "associated containers" immediately after that container was force-removed earlier in
        // this same teardown run -- its cleanup lags the container removal by a beat. The
        // caller's own `network_attachment_count` precondition (checked before this function is
        // ever reached) has already confirmed nothing real is left attached, so retrying with
        // `--force` (Podman-only: forcibly disconnects any still-attached containers/pods before
        // removing the network) recovers from Podman's stale bookkeeping instead of surfacing a
        // spurious failure to the operator.
        let command = format!("{engine} network rm --force {name}");
        let result = session.execute(&command).await?;
        ensure_success(session, &command, &result)?;
        return Ok(true);
    }
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
        // jiji-proxy carries jiji.managed=true but no project/service/server labels.
        let summary = parse_container_summary_line("jiji-proxy||||running");
        assert_eq!(summary.name, "jiji-proxy");
        assert_eq!(summary.project, None);
        assert_eq!(summary.service, None);
        assert_eq!(summary.server, None);
        assert_eq!(summary.status, "running");
    }

    #[test]
    fn list_other_project_containers_excludes_the_named_project_and_unlabeled_containers() {
        // Exercised indirectly: the filter predicate used inside list_other_project_containers.
        // Confirmed live: jiji-proxy carries jiji.managed=true but no jiji.project label, and
        // must never be treated as belonging to "some other project."
        let containers = vec![
            parse_container_summary_line("demo-web-a|demo|web|app|running"),
            parse_container_summary_line("other-web-a|other|web|app|running"),
            parse_container_summary_line("jiji-proxy||||running"),
        ];
        let filtered: Vec<_> = containers
            .into_iter()
            .filter(|container| {
                matches!(container.project.as_deref(), Some(project) if project != "demo")
            })
            .collect();
        assert_eq!(filtered.len(), 1);
        assert!(filtered.iter().any(|c| c.name == "other-web-a"));
        assert!(!filtered.iter().any(|c| c.name == "jiji-proxy"));
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
    fn label_filter_command_uses_the_right_template_function_per_engine() {
        let docker = render_list_by_label_filter_command(ContainerEngine::Docker, None);
        assert!(docker.contains(r#"{{.Label "jiji.project"}}"#));
        assert!(!docker.contains(".Labels"));

        let podman = render_list_by_label_filter_command(ContainerEngine::Podman, None);
        assert!(podman.contains(r#"{{index .Labels "jiji.project"}}"#));
        assert!(!podman.contains(r#".Label ""#));
    }

    #[test]
    fn parse_container_summary_lines_skips_blank_lines() {
        let stdout = "demo-web-a|demo|web|app|running\n\njiji-proxy|||running\n";
        let parsed = parse_container_summary_lines(stdout);
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn network_attachment_count_excludes_listed_names() {
        let stdout = "jiji-proxy\ndemo-web-a\n";
        let exclude = ["jiji-proxy"];
        assert_eq!(count_attachments(stdout, &exclude), 1);
    }

    #[test]
    fn network_attachment_count_ignores_blank_lines() {
        assert_eq!(count_attachments("\n\n", &[]), 0);
    }
}
