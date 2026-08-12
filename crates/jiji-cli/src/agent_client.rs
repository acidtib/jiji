use anyhow::Context;
use jiji_agent::api::{ApiResult, Request, RequestBody, ResponseBody};
use jiji_agent::AgentPaths;
use jiji_ssh::SshSession;

pub async fn call(
    session: &SshSession,
    project: &str,
    idempotency_key: Option<String>,
    body: RequestBody,
) -> anyhow::Result<ResponseBody> {
    let paths = AgentPaths::default_for_project(project);
    let request = Request {
        idempotency_key,
        body,
    };
    let input = serde_json::to_vec(&request)?;
    let request_kind = match &request.body {
        RequestBody::CatalogList => "catalog-list",
        RequestBody::AllocateAddress { .. } => "allocate-address",
        RequestBody::ReleaseAddress { .. } => "release-address",
        RequestBody::CatalogCommit { .. } => "catalog-commit",
        RequestBody::DesiredCommit { .. } => "desired-commit",
        RequestBody::DesiredRead { .. } => "desired-read",
        RequestBody::Health => "health",
        RequestBody::Identity => "identity",
        RequestBody::Diagnostics => "diagnostics",
        RequestBody::Compact => "compact",
        RequestBody::ReconciliationStatus => "reconciliation-status",
        RequestBody::CatalogRead { .. } => "catalog-read",
        RequestBody::LocalTransaction { .. } => "local-transaction",
        RequestBody::CronSpecApply { .. } => "cron-spec-apply",
        RequestBody::CronSpecRemove { .. } => "cron-spec-remove",
        RequestBody::CronSpecList => "cron-spec-list",
        RequestBody::CronStatus { .. } => "cron-status",
        RequestBody::CronRun { .. } => "cron-run",
        RequestBody::CronRuns { .. } => "cron-runs",
        RequestBody::ImageRetentionApply { .. } => "image-retention-apply",
        RequestBody::ImageRetentionRemove { .. } => "image-retention-remove",
        RequestBody::ImageRetentionList => "image-retention-list",
    };
    let command = format!(
        "{} request --socket {} # jiji-request:{request_kind}",
        paths.binary_path.display(),
        paths.socket_path.display()
    );
    let result = session.execute_with_input(&command, &input).await?;
    if !result.success {
        anyhow::bail!(
            "Agent request failed on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    match serde_json::from_str::<ApiResult>(result.stdout.trim())? {
        Ok(response) => Ok(response),
        Err(error) => anyhow::bail!(
            "Agent rejected request on {}: {}",
            session.host(),
            error.message
        ),
    }
}

pub async fn catalog(
    session: &SshSession,
    project: &str,
) -> anyhow::Result<Vec<jiji_agent::catalog::CatalogRecord>> {
    match call(session, project, None, RequestBody::CatalogList).await? {
        ResponseBody::CatalogList { records } => Ok(records),
        response => anyhow::bail!("Agent returned unexpected catalog response: {response:?}"),
    }
}

/// Rejects a `jiji-agent` running below `crate::version_requirements::
/// MIN_AGENT_VERSION`, actionable ("Run `jiji server setup` to update
/// it."): a stale agent left behind after the local `jiji` CLI itself was
/// upgraded is otherwise a silent compatibility risk with no signal to the
/// operator. A host with no agent installed at all (never ran `jiji server
/// setup`, or a pre-agent installation) fails the underlying request
/// itself rather than returning a parseable version -- wrapped with the
/// same actionable hint instead of surfacing the raw remote-command error.
pub async fn check_version(session: &SshSession, project: &str, host: &str) -> anyhow::Result<()> {
    let response = call(session, project, None, RequestBody::Health)
        .await
        .with_context(|| {
            format!("Could not reach jiji-agent on '{host}'. Run `jiji server setup` to install or repair it.")
        })?;
    match response {
        ResponseBody::Health { version, .. } => crate::version_requirements::check_min_version(
            "jiji-agent",
            host,
            &version,
            crate::version_requirements::MIN_AGENT_VERSION,
            "Run `jiji server setup` to update it.",
        ),
        response => anyhow::bail!("Agent returned unexpected health response: {response:?}"),
    }
}
