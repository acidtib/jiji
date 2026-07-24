use jiji_network::{
    ActiveSlotState, BackendSlot, NetworkPlan, ServiceNatArtifacts, ServiceRuntimeError,
};
use jiji_ssh::{CommandResult, SshSession};

const CURRENT_STATE_PATH: &str = "/etc/jiji/network/service-nat-current/active-slots";
const GENERATIONS_PATH: &str = "/etc/jiji/network/service-nat-generations";
const CURRENT_LINK_PATH: &str = "/etc/jiji/network/service-nat-current";
const RESTORE_COMMAND: &str = "/etc/jiji/network/restore-service-nat.sh";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedServiceCutover {
    endpoint_identity: String,
    previous_slot: Option<BackendSlot>,
    candidate_slot: BackendSlot,
}

impl PreparedServiceCutover {
    pub fn endpoint_identity(&self) -> &str {
        &self.endpoint_identity
    }

    pub fn previous_slot(&self) -> Option<BackendSlot> {
        self.previous_slot
    }

    pub fn candidate_slot(&self) -> BackendSlot {
        self.candidate_slot
    }
}

pub async fn load_active_slots(
    session: &SshSession,
) -> Result<ActiveSlotState, ServiceRuntimeError> {
    let result = session
        .execute(&format!("cat {CURRENT_STATE_PATH}"))
        .await
        .map_err(|error| ServiceRuntimeError::Remote(error.to_string()))?;
    if !result.success {
        return Err(ServiceRuntimeError::Remote(format!(
            "Could not read active service slots on {}: {}. Run `jiji network setup` and retry.",
            session.host(),
            result.stderr.trim()
        )));
    }
    ActiveSlotState::parse(&result.stdout)
}

pub async fn deployment_slot(
    session: &SshSession,
    endpoint_identity: &str,
) -> Result<BackendSlot, ServiceRuntimeError> {
    Ok(load_active_slots(session)
        .await?
        .deployment_slot(endpoint_identity))
}

pub async fn prepare_cutover(
    session: &SshSession,
    plan: &NetworkPlan,
    endpoint_identity: &str,
) -> Result<PreparedServiceCutover, ServiceRuntimeError> {
    if !plan.endpoints.contains_key(endpoint_identity) {
        return Err(ServiceRuntimeError::UnknownEndpoint {
            identity: endpoint_identity.to_string(),
        });
    }
    let state = load_active_slots(session).await?;
    Ok(prepare_cutover_from_state(&state, endpoint_identity))
}

/// Runs the caller's health check before changing the stable VIP. A failed check leaves the
/// previous mapping untouched, so the caller can remove the unhealthy candidate safely.
pub async fn commit_after_health_check(
    session: &SshSession,
    plan: &NetworkPlan,
    cutover: &PreparedServiceCutover,
    health_command: &str,
) -> Result<(), ServiceRuntimeError> {
    let result = session
        .execute(health_command)
        .await
        .map_err(|error| ServiceRuntimeError::Remote(error.to_string()))?;
    if !result.success {
        return Err(ServiceRuntimeError::Remote(format!(
            "Health check `{health_command}` failed on {} (exit {:?}): {}. The existing service VIP mapping was not changed; remove the candidate container and retry the deployment.",
            session.host(),
            result.code,
            result.stderr.trim()
        )));
    }
    activate_slot(
        session,
        plan,
        cutover.endpoint_identity(),
        cutover.candidate_slot(),
    )
    .await
}

pub async fn rollback_cutover(
    session: &SshSession,
    plan: &NetworkPlan,
    cutover: &PreparedServiceCutover,
) -> Result<(), ServiceRuntimeError> {
    match cutover.previous_slot() {
        Some(slot) => activate_slot(session, plan, cutover.endpoint_identity(), slot).await,
        None => deactivate_slot(session, plan, cutover.endpoint_identity()).await,
    }
}

/// Clears every active-slot entry belonging to `project` (identity prefix `"{project}:"`),
/// leaving other projects' mappings on a shared host untouched. The teardown counterpart to
/// `reconcile_slots`, which trims to the current project's own plan instead.
pub async fn deactivate_project(
    session: &SshSession,
    plan: &NetworkPlan,
    project: &str,
) -> Result<(), ServiceRuntimeError> {
    let mut state = load_active_slots(session).await?;
    let before = state.render();
    let prefix = format!("{project}:");
    state.retain(|identity| !identity.starts_with(&prefix));
    if state.render() == before {
        return Ok(());
    }
    persist_state(session, plan, &state).await
}

pub async fn reconcile_slots(
    session: &SshSession,
    plan: &NetworkPlan,
    server_name: &str,
) -> Result<(), ServiceRuntimeError> {
    let mut state = load_active_slots(session).await?;
    let before = state.render();
    state.retain(|identity| {
        plan.endpoints
            .get(identity)
            .is_some_and(|endpoint| endpoint.server == server_name)
    });
    if state.render() == before {
        return Ok(());
    }
    persist_state(session, plan, &state).await
}

/// Persists and applies a complete VIP mapping generation. Calling this with the prior slot is
/// the rollback operation; calling it with the replacement slot is the healthy cutover.
pub async fn activate_slot(
    session: &SshSession,
    plan: &NetworkPlan,
    endpoint_identity: &str,
    slot: BackendSlot,
) -> Result<(), ServiceRuntimeError> {
    if !plan.endpoints.contains_key(endpoint_identity) {
        return Err(ServiceRuntimeError::UnknownEndpoint {
            identity: endpoint_identity.to_string(),
        });
    }
    let mut state = load_active_slots(session).await?;
    state.activate(endpoint_identity, slot);
    persist_state(session, plan, &state).await
}

pub(crate) async fn deactivate_slot(
    session: &SshSession,
    plan: &NetworkPlan,
    endpoint_identity: &str,
) -> Result<(), ServiceRuntimeError> {
    let mut state = load_active_slots(session).await?;
    state.deactivate(endpoint_identity);
    persist_state(session, plan, &state).await
}

async fn persist_state(
    session: &SshSession,
    plan: &NetworkPlan,
    state: &ActiveSlotState,
) -> Result<(), ServiceRuntimeError> {
    let artifacts = ServiceNatArtifacts::render(plan, state)?;

    let create = format!("mktemp -d {GENERATIONS_PATH}/cutover.XXXXXX");
    let result = session
        .execute(&create)
        .await
        .map_err(|error| ServiceRuntimeError::Remote(error.to_string()))?;
    ensure_success(session, &create, &result)?;
    let generation = result.stdout.trim();
    if !generation.starts_with(&format!("{GENERATIONS_PATH}/cutover."))
        || generation.contains(char::is_whitespace)
    {
        return Err(ServiceRuntimeError::Remote(format!(
            "Host {} returned unsafe service mapping generation path '{generation}'",
            session.host()
        )));
    }

    write_generation_file(
        session,
        &format!("{generation}/active-slots"),
        &artifacts.state,
    )
    .await?;
    write_generation_file(
        session,
        &format!("{generation}/service-nat.nft"),
        &artifacts.nftables,
    )
    .await?;

    let activate = format!(
        "set -eu; \
         nft add table ip jiji_service_nat 2>/dev/null || true; \
         nft --check --file {generation}/service-nat.nft; \
         ln -s {generation} {CURRENT_LINK_PATH}.new; \
         mv -Tf {CURRENT_LINK_PATH}.new {CURRENT_LINK_PATH}; \
         {RESTORE_COMMAND}"
    );
    let result = session
        .execute(&activate)
        .await
        .map_err(|error| ServiceRuntimeError::Remote(error.to_string()))?;
    ensure_success(session, &activate, &result)
}

fn prepare_cutover_from_state(
    state: &ActiveSlotState,
    endpoint_identity: &str,
) -> PreparedServiceCutover {
    PreparedServiceCutover {
        endpoint_identity: endpoint_identity.to_string(),
        previous_slot: state.active_slot(endpoint_identity),
        candidate_slot: state.deployment_slot(endpoint_identity),
    }
}

async fn write_generation_file(
    session: &SshSession,
    path: &str,
    content: &str,
) -> Result<(), ServiceRuntimeError> {
    let command = format!("install -m 0644 /dev/stdin {path}");
    let result = session
        .execute_with_input(&command, content.as_bytes())
        .await
        .map_err(|error| ServiceRuntimeError::Remote(error.to_string()))?;
    ensure_success(session, &command, &result)
}

fn ensure_success(
    session: &SshSession,
    command: &str,
    result: &CommandResult,
) -> Result<(), ServiceRuntimeError> {
    if result.success {
        return Ok(());
    }
    Err(ServiceRuntimeError::Remote(format!(
        "Command `{command}` failed on {} (exit {:?}): {}",
        session.host(),
        result.code,
        result.stderr.trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_cutover_preserves_previous_slot_and_chooses_inactive_candidate() {
        let mut state = ActiveSlotState::default();
        let first = prepare_cutover_from_state(&state, "demo:web:app");
        assert_eq!(first.previous_slot(), None);
        assert_eq!(first.candidate_slot(), BackendSlot::A);

        state.activate("demo:web:app", BackendSlot::A);
        let replacement = prepare_cutover_from_state(&state, "demo:web:app");
        assert_eq!(replacement.previous_slot(), Some(BackendSlot::A));
        assert_eq!(replacement.candidate_slot(), BackendSlot::B);
    }

    #[test]
    fn slot_reconciliation_preserves_known_local_endpoints_only() {
        let mut state = ActiveSlotState::default();
        state.activate("demo:web:app", BackendSlot::A);
        state.activate("demo:web:data", BackendSlot::B);
        state.activate("removed:api:app", BackendSlot::A);
        state.retain(|identity| identity == "demo:web:app");
        assert_eq!(state.render(), "demo:web:app=a\n");
    }

    #[test]
    fn deactivate_project_predicate_clears_only_the_matching_prefix() {
        let mut state = ActiveSlotState::default();
        state.activate("demo:web:app", BackendSlot::A);
        state.activate("demo:redis:app", BackendSlot::B);
        state.activate("other:web:app", BackendSlot::A);
        let prefix = "demo:".to_string();
        state.retain(|identity| !identity.starts_with(&prefix));
        assert_eq!(state.render(), "other:web:app=a\n");
    }

    #[test]
    fn deactivate_project_prefix_never_matches_a_similarly_named_project() {
        // "demo" must not clear "demo-extended:..." -- the trailing ':' in the prefix guards this.
        let mut state = ActiveSlotState::default();
        state.activate("demo:web:app", BackendSlot::A);
        state.activate("demo-extended:web:app", BackendSlot::A);
        let prefix = "demo:".to_string();
        state.retain(|identity| !identity.starts_with(&prefix));
        assert_eq!(state.render(), "demo-extended:web:app=a\n");
    }
}
