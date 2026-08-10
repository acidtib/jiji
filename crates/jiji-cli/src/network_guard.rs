use jiji_network::NetworkPlan;
use jiji_ssh::SshSession;

fn mesh_generation_path(project: &str) -> String {
    format!(
        "{}/mesh-generation",
        crate::commands::network::setup::network_dir(&jiji_network::systemd_unit_slug(project))
    )
}

fn legacy_generation_path(project: &str) -> String {
    format!(
        "{}/generation",
        crate::commands::network::setup::network_dir(&jiji_network::systemd_unit_slug(project))
    )
}

pub async fn generation_is_current(
    session: &SshSession,
    plan: &NetworkPlan,
) -> anyhow::Result<bool> {
    let path = mesh_generation_path(&plan.project);
    let current = crate::commands::network::setup::network_current(
        &jiji_network::systemd_unit_slug(&plan.project),
    );
    // The generation marker is valid only while `current` is the atomic symlink installed by
    // network activation. A failed rollback can leave the separate marker behind. Trusting that
    // marker alone skips prerequisite repair and can make a later write create `current` as a
    // real directory, after which activation cannot replace it with a symlink.
    let command = format!("if test -L {current}; then cat {path} 2>/dev/null || true; fi");
    let result = session.execute(&command).await?;
    if result.stdout.trim().is_empty() {
        let legacy_path = legacy_generation_path(&plan.project);
        let legacy = session
            .execute(&format!("cat {legacy_path} 2>/dev/null || true"))
            .await?;
        if !legacy.stdout.trim().is_empty() {
            anyhow::bail!(
                "Host {} has a monolithic network generation installed. This development build requires clean separated mesh/service-runtime state; run `jiji server teardown` followed by `jiji server setup`.",
                session.host()
            );
        }
    }
    Ok(result.stdout.trim() == plan.mesh_generation)
}
