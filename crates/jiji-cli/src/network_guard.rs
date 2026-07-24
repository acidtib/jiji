use jiji_network::NetworkPlan;
use jiji_ssh::SshSession;

fn network_generation_path(project: &str) -> String {
    format!(
        "{}/generation",
        crate::commands::network::setup::network_dir(&jiji_network::systemd_unit_slug(project))
    )
}

fn generation_mismatch_message(server_name: &str, installed: &str, expected: &str) -> String {
    format!(
        "Host {server_name} has network generation {installed}, expected {expected}. Run `jiji network setup` and retry `jiji deploy`."
    )
}

pub async fn generation_is_current(
    session: &SshSession,
    plan: &NetworkPlan,
) -> anyhow::Result<bool> {
    let path = network_generation_path(&plan.project);
    let command = format!("cat {path} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    Ok(result.stdout.trim() == plan.generation)
}

/// This remains as a final defense against changing the network between reconciliation and
/// deploying an endpoint.
pub async fn verify_generation(
    session: &SshSession,
    plan: &NetworkPlan,
    server_name: &str,
) -> anyhow::Result<()> {
    let path = network_generation_path(&plan.project);
    let command = format!("cat {path} 2>/dev/null || true");
    let result = session.execute(&command).await?;
    let installed = result.stdout.trim();
    if installed == plan.generation {
        return Ok(());
    }
    let installed_display = if installed.is_empty() {
        "none"
    } else {
        installed
    };
    anyhow::bail!(generation_mismatch_message(
        server_name,
        installed_display,
        &plan.generation
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_reports_installed_and_expected_generation() {
        let message = generation_mismatch_message("app", "old-gen", "new-gen");
        assert!(message.contains("Host app has network generation old-gen, expected new-gen"));
        assert!(message.contains("jiji network setup"));
    }

    #[test]
    fn message_reports_none_when_no_generation_file_exists() {
        let message = generation_mismatch_message("app", "none", "new-gen");
        assert!(message.contains("generation none"));
    }
}
