use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use jiji_config::{ContainerEngine, HealthcheckConfig};
use jiji_ssh::{CommandResult, SshSession};
use thiserror::Error;

use crate::container_ops;
use crate::container_runtime::exec_prefix;

const DEFAULT_INTERVAL: Duration = Duration::from_secs(2);
const DEFAULT_DEPLOY_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HealthCheckPlan {
    /// Always a real, single-shot command: an HTTP/command check when configured, or a
    /// container-readiness check (`{engine} inspect ... | grep -qx running`) otherwise. Keeping
    /// this a plain `String` (never absent) gives the deployment transaction one authoritative
    /// gate before it publishes the candidate as active.
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
/// error. `on_attempt` is called with a one-line summary of each failed attempt, but only when
/// that summary changes from the last one reported -- a command that keeps failing identically
/// stays silent after the first report, matching `DeployProgressHandle::set_status`'s
/// one-line-per-state-transition invariant for non-TTY output.
pub async fn wait_until_healthy(
    session: &SshSession,
    engine: ContainerEngine,
    container_name: &str,
    plan: &HealthCheckPlan,
    on_attempt: impl Fn(&str),
) -> Result<(), HealthCheckError> {
    let start = Instant::now();
    let mut last_reported: Option<String> = None;
    let last_error = loop {
        let attempt_error = match session.execute(&plan.command).await {
            Ok(result) if result.success => return Ok(()),
            Ok(result) => summarize_attempt(&result),
            Err(error) => error.to_string(),
        };
        if last_reported.as_deref() != Some(attempt_error.as_str()) {
            on_attempt(&attempt_error);
            last_reported = Some(attempt_error.clone());
        }
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

/// Prefers `stderr` (to avoid a large body/script output flooding one progress row), falls back
/// to `stdout` the same way, falls back to the exit status if both are empty.
fn summarize_attempt(result: &CommandResult) -> String {
    if let Some(summary) = summarize_stream(&result.stderr) {
        return summary;
    }
    if let Some(summary) = summarize_stream(&result.stdout) {
        return summary;
    }
    match result.code {
        Some(code) => format!("exited with status {code}"),
        None => "no exit status (killed by signal)".to_string(),
    }
}

/// A failing command's own diagnostic conventionally starts on its first line of output, with
/// anything after it either elaboration or unrelated trailing noise (a cleanup message, a blank
/// prompt, a generic "command failed" footer) -- taking only the *last* non-empty line instead
/// surfaced whichever of those a multi-line failure happened to end with and silently dropped the
/// actual cause above it. Keeps this a single line (no embedded newline, matching
/// `wait_until_healthy`'s one-line-per-progress-row contract) by carrying the last line alongside
/// the first only when it differs, rather than by discarding one of them outright.
fn summarize_stream(text: &str) -> Option<String> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next()?;
    match lines.next_back() {
        Some(last) if last != first => Some(format!("{first} (... {last})")),
        _ => Some(first.to_string()),
    }
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

    fn result(stdout: &str, stderr: &str, code: Option<u32>) -> CommandResult {
        CommandResult {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            success: false,
            code,
        }
    }

    #[test]
    fn summarize_attempt_prefers_stderr() {
        let summary = summarize_attempt(&result("some stdout", "boom", Some(1)));
        assert_eq!(summary, "boom");
    }

    #[test]
    fn summarize_attempt_falls_back_to_stdout_when_stderr_is_empty() {
        let summary = summarize_attempt(&result("still starting", "", Some(1)));
        assert_eq!(summary, "still starting");
    }

    #[test]
    fn summarize_attempt_falls_back_to_exit_status_when_both_are_empty() {
        assert_eq!(
            summarize_attempt(&result("", "", Some(7))),
            "exited with status 7"
        );
        assert_eq!(
            summarize_attempt(&result("", "", None)),
            "no exit status (killed by signal)"
        );
    }

    #[test]
    fn summarize_attempt_keeps_both_ends_of_multiline_output() {
        let summary = summarize_attempt(&result("", "line one\n\nline two\n", Some(1)));
        assert_eq!(summary, "line one (... line two)");
    }

    #[test]
    fn summarize_attempt_does_not_lose_the_root_cause_behind_a_generic_trailing_line() {
        // The real diagnostic is on the first line; a generic footer follows it, matching a
        // common real-world shape (e.g. curl's actual error, then a script's own "failed" echo).
        let summary = summarize_attempt(&result(
            "",
            "curl: (7) Failed to connect to localhost port 3000\nhealthcheck failed\n",
            Some(1),
        ));
        assert!(summary.contains("Failed to connect"), "summary: {summary}");
    }

    #[test]
    fn summarize_attempt_collapses_to_one_line_when_every_repeated_line_is_identical() {
        let summary = summarize_attempt(&result("", "boom\nboom\nboom\n", Some(1)));
        assert_eq!(summary, "boom");
    }
}
