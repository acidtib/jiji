use jiji_network::NetworkPlan;
use jiji_ssh::SshSession;

const NETWORK_GENERATION_PATH: &str = "/etc/jiji/network/generation";

fn generation_mismatch_message(server_name: &str, installed: &str, expected: &str) -> String {
    format!(
        "Host {server_name} has network generation {installed}, expected {expected}. Run `jiji network setup` and retry `jiji deploy`."
    )
}

/// `deploy` must never repair the host network implicitly -- network repair stays owned by
/// `server setup`/`network setup`. This only compares the installed generation (written by
/// `network/setup.rs::activate_host` at the same path) against the locally compiled plan.
pub async fn verify_generation(
    session: &SshSession,
    plan: &NetworkPlan,
    server_name: &str,
) -> anyhow::Result<()> {
    let command = format!("cat {NETWORK_GENERATION_PATH} 2>/dev/null || true");
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
