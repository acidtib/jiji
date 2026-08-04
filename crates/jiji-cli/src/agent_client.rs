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
