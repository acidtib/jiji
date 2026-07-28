use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use jiji_config::{ContainerEngine, HealthcheckConfig};
use jiji_ssh::SshSession;
use thiserror::Error;

use crate::container_ops;
use crate::container_runtime::exec_prefix;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_DEPLOY_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HealthCheckPlan {
    /// Always a real, single-shot command: an HTTP/command check when configured, or a
    /// container-readiness check (`{engine} inspect ... | grep -qx running`) otherwise. Keeping
    /// this a plain `String` (never absent) lets the exact same command be reused as the
    /// authoritative gate `service_network::commit_after_health_check` re-runs before flipping
    /// the VIP, instead of maintaining two different notions of "healthy".
    pub command: String,
    pub interval: Duration,
    pub deploy_timeout: Duration,
}

/// Parses `"<digits>s"` / `"<digits>m"` durations. Any other shape returns `None`, letting the
/// caller fall back to a documented default rather than guessing.
pub fn parse_duration(value: &str) -> Option<Duration> {
    let value = value.trim();
    if value.len() < 2 {
        return None;
    }
    let (digits, unit) = value.split_at(value.len() - 1);
    let amount: u64 = digits.parse().ok()?;
    match unit {
        "s" => Some(Duration::from_secs(amount)),
        "m" => Some(Duration::from_secs(amount * 60)),
        _ => None,
    }
}

fn container_readiness_command(engine: ContainerEngine, container_name: &str) -> String {
    format!("{engine} inspect {container_name} --format '{{{{.State.Status}}}}' | grep -qx running")
}

/// Selects the health check for a deploying candidate: a command check (executed inside the
/// container via `cmd_runtime`, defaulting to the deploying engine) takes precedence over an HTTP
/// path check (curled directly against the candidate's own backend address -- never the VIP,
/// which still points at the old backend until cutover). Neither configured -> a container
/// readiness check.
pub fn plan_for_candidate(
    engine: ContainerEngine,
    container_name: &str,
    backend_address: Ipv4Addr,
    port: u32,
    healthcheck: Option<&HealthcheckConfig>,
) -> HealthCheckPlan {
    let interval = healthcheck
        .and_then(|check| check.interval.as_deref())
        .and_then(parse_duration)
        .unwrap_or(DEFAULT_INTERVAL);
    let deploy_timeout = healthcheck
        .and_then(|check| check.deploy_timeout.as_deref())
        .and_then(parse_duration)
        .unwrap_or(DEFAULT_DEPLOY_TIMEOUT);

    let command = healthcheck
        .and_then(|check| {
            if let Some(cmd) = &check.cmd {
                let runtime = check.cmd_runtime.unwrap_or(engine);
                Some(format!("{} {container_name} {cmd}", exec_prefix(runtime)))
            } else {
                check.path.as_ref().map(|path| {
                    let timeout = check
                        .timeout
                        .as_deref()
                        .and_then(parse_duration)
                        .unwrap_or(DEFAULT_HTTP_TIMEOUT);
                    format!(
                        "curl -fsS --max-time {} http://{backend_address}:{port}{path}",
                        timeout.as_secs().max(1)
                    )
                })
            }
        })
        .unwrap_or_else(|| container_readiness_command(engine, container_name));

    HealthCheckPlan {
        command,
        interval,
        deploy_timeout,
    }
}

#[derive(Debug, Error)]
pub enum HealthCheckError {
    #[error("Health check `{command}` did not succeed on {host} within {deploy_timeout:?}: {last_error}. Recent logs: {logs}")]
    Failed {
        command: String,
        host: String,
        deploy_timeout: Duration,
        last_error: String,
        logs: String,
    },
}

/// Polls `plan.command` every `plan.interval` until it succeeds or `plan.deploy_timeout` elapses.
/// On failure, captures the candidate's recent logs so the caller can present an actionable
/// error.
pub async fn wait_until_healthy(
    session: &SshSession,
    engine: ContainerEngine,
    container_name: &str,
    plan: &HealthCheckPlan,
) -> Result<(), HealthCheckError> {
    let start = Instant::now();
    let last_error = loop {
        let attempt_error = match session.execute(&plan.command).await {
            Ok(result) if result.success => return Ok(()),
            Ok(result) => result.stderr.trim().to_string(),
            Err(error) => error.to_string(),
        };
        if start.elapsed() >= plan.deploy_timeout {
            break attempt_error;
        }
        tokio::time::sleep(plan.interval).await;
    };

    let logs = container_ops::logs_tail(session, engine, container_name, 50)
        .await
        .unwrap_or_default();
    Err(HealthCheckError::Failed {
        command: plan.command.clone(),
        host: session.host().to_string(),
        deploy_timeout: plan.deploy_timeout,
        last_error,
        logs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_seconds_and_minutes() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("bogus"), None);
    }

    fn healthcheck(yaml: &str) -> HealthcheckConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn command_check_takes_precedence_over_http_path() {
        let check = healthcheck("cmd: \"test -f /ready\"\npath: /health\ncmd_runtime: podman\n");
        let plan = plan_for_candidate(
            ContainerEngine::Docker,
            "demo-web-a",
            "10.0.0.2".parse().unwrap(),
            3000,
            Some(&check),
        );
        assert_eq!(
            plan.command,
            "podman exec --no-session demo-web-a test -f /ready"
        );
    }

    #[test]
    fn http_path_check_targets_the_candidates_own_backend_address_not_a_vip() {
        let check = healthcheck("path: /health\ntimeout: 5s\n");
        let plan = plan_for_candidate(
            ContainerEngine::Docker,
            "demo-web-a",
            "10.0.0.2".parse().unwrap(),
            3000,
            Some(&check),
        );
        assert_eq!(
            plan.command,
            "curl -fsS --max-time 5 http://10.0.0.2:3000/health"
        );
    }

    #[test]
    fn no_healthcheck_configured_falls_back_to_container_readiness() {
        let plan = plan_for_candidate(
            ContainerEngine::Docker,
            "demo-web-a",
            "10.0.0.2".parse().unwrap(),
            3000,
            None,
        );
        assert!(plan.command.contains("inspect demo-web-a"));
        assert!(plan.command.contains("grep -qx running"));
    }
}
