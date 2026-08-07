use anyhow::Context;
use jiji_config::{validate_config, ContainerEngine};
use jiji_network::NetworkPlanner;
use jiji_ssh::{SshPool, SshSession, StreamChunk};
use jiji_tui::Ui;
use tokio::io::AsyncWriteExt;

use crate::{registry::shell_quote, ssh_adapter};

pub struct LogsOptions<'a> {
    pub environment: Option<&'a str>,
    pub config_file: Option<&'a str>,
    pub hosts: Option<&'a str>,
    pub services: Option<&'a str>,
    pub lines: Option<u32>,
    pub since: Option<&'a str>,
    pub grep: Option<&'a str>,
    pub follow: bool,
}

pub async fn run(
    LogsOptions {
        environment,
        config_file,
        hosts,
        services,
        lines,
        since,
        grep,
        follow,
    }: LogsOptions<'_>,
) -> anyhow::Result<()> {
    Ui::section("Proxy Logs:");
    if services.is_some() {
        anyhow::bail!(
            "`jiji proxy logs` does not accept -S/--services: jiji-proxy logs belong to a host. Use -H/--hosts to select servers instead."
        );
    }
    let start = std::env::current_dir()?;
    let (config, path) = crate::config_loading::load_config_for_ssh(
        environment,
        config_file.map(std::path::Path::new),
        &start,
    )
    .await?;
    let validation = validate_config(&config);
    if !validation.valid {
        for error in validation.errors {
            Ui::error(&format!("{}: {}", error.path, error.message));
        }
        anyhow::bail!("Configuration is invalid; fix the errors above and try again");
    }
    let ssh = config.ssh.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "No `ssh:` section configured in {}. Add at least `ssh.user:` before running proxy logs.",
            path.display()
        )
    })?;
    let plan = NetworkPlanner::new()
        .plan(&config)
        .map_err(|error| anyhow::anyhow!("Could not build the proxy network plan: {error}"))?;
    let selected = plan.select_hosts(&split_comma_trimmed(hosts))?;
    if selected.is_empty() {
        anyhow::bail!("No servers are configured. Add a `servers:` entry and retry.");
    }
    if follow && selected.len() != 1 {
        anyhow::bail!(
            "-H/--hosts matched {} servers ({}). `jiji proxy logs --follow` requires exactly one host; narrow the filter and try again.",
            selected.len(),
            selected
                .iter()
                .map(|server| server.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let effective_lines = effective_lines(lines, since, grep);
    let command = render_logs_command(
        config.builder.engine,
        "jiji-proxy",
        effective_lines,
        since,
        grep,
        None,
        follow,
    );
    if follow {
        let target = selected[0];
        let named_server = config.servers.get(&target.name).ok_or_else(|| {
            anyhow::anyhow!(
                "Server '{}' selected by the network plan is not configured",
                target.name
            )
        })?;
        let options = ssh_adapter::connect_options(&target.name, named_server, &ssh)?;
        let session = SshSession::connect(&options)
            .await
            .with_context(|| format!("Could not connect to '{}'", target.name))?;
        let result = stream_logs(&session, &command).await;
        session.close().await;
        return result;
    }

    let mut operations = Vec::with_capacity(selected.len());
    for target in selected {
        let name = target.name.clone();
        let named_server = config.servers.get(&name).ok_or_else(|| {
            anyhow::anyhow!("Server '{name}' selected by the network plan is not configured")
        })?;
        let options = ssh_adapter::connect_options(&name, named_server, &ssh)?;
        let command = command.clone();
        operations.push(move || async move {
            let result = async {
                let session = SshSession::connect(&options).await?;
                let outcome = session.execute(&command).await;
                session.close().await;
                outcome
            }
            .await;
            (name, result)
        });
    }
    let pool = SshPool::new(ssh.max_concurrent_starts as usize);
    let outcomes = pool.execute_concurrent(operations).await;
    let mut failures = Vec::new();
    for (name, outcome) in outcomes {
        match outcome {
            Ok(result) if result.success => {
                Ui::say(&format!("{name}:"), 1);
                if !result.stdout.is_empty() {
                    print!("{}", result.stdout);
                }
                if !result.stderr.is_empty() {
                    eprint!("{}", result.stderr);
                }
            }
            Ok(result) => {
                let error = format!(
                    "remote logs command failed with status {:?}: {}",
                    result.code,
                    result.stderr.trim()
                );
                Ui::error(&format!("{name}: {error}"));
                failures.push(error);
            }
            Err(error) => {
                Ui::error(&format!("{name}: {error}"));
                failures.push(error.to_string());
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Could not read jiji-proxy logs from {} server(s). Fix the reported hosts and retry `jiji proxy logs`.",
            failures.len()
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_logs_command(
    engine: ContainerEngine,
    container: &str,
    lines: Option<u32>,
    since: Option<&str>,
    grep: Option<&str>,
    grep_options: Option<&str>,
    follow: bool,
) -> String {
    let mut command = format!("{engine} logs --timestamps");
    if follow {
        command.push_str(" --follow");
    }
    if let Some(since) = since {
        command.push_str(&format!(" --since={}", shell_quote(since)));
    }
    if let Some(lines) = lines {
        command.push_str(&format!(" --tail={lines}"));
    }
    command.push_str(&format!(" {container}"));
    if let Some(pattern) = grep {
        command.push_str(" | grep");
        if let Some(options) = grep_options {
            for token in options.split_whitespace() {
                command.push(' ');
                command.push_str(&shell_quote(token));
            }
        }
        command.push_str(&format!(" -- {}", shell_quote(pattern)));
    }
    command
}

pub(crate) fn effective_lines(
    lines: Option<u32>,
    since: Option<&str>,
    grep: Option<&str>,
) -> Option<u32> {
    lines.or_else(|| (since.is_none() && grep.is_none()).then_some(100))
}

pub(crate) async fn stream_logs(session: &SshSession, command: &str) -> anyhow::Result<()> {
    let mut receiver = session.execute_streaming(command).await?;
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    let mut exit_code = None;
    while let Some(item) = receiver.recv().await {
        match item? {
            StreamChunk::Stdout(data) => {
                stdout.write_all(&data).await?;
                stdout.flush().await?;
            }
            StreamChunk::Stderr(data) => {
                stderr.write_all(&data).await?;
                stderr.flush().await?;
            }
            StreamChunk::Exit(code) => exit_code = Some(code),
        }
    }
    match exit_code {
        Some(0) => Ok(()),
        Some(code) => anyhow::bail!("Remote logs command exited with status {code}"),
        None => anyhow::bail!(
            "Remote logs command did not report an exit status. Check the SSH connection and retry."
        ),
    }
}

fn split_comma_trimmed(value: Option<&str>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_all_flags_and_quotes_shell_values() {
        assert_eq!(
            render_logs_command(
                ContainerEngine::Docker,
                "jiji-proxy",
                Some(25),
                Some("1 hour ago"),
                Some("can't; echo bad"),
                None,
                true,
            ),
            "docker logs --timestamps --follow --since='1 hour ago' --tail=25 jiji-proxy | grep -- 'can'\\''t; echo bad'"
        );
    }

    #[test]
    fn renders_minimal_command_without_optional_flags() {
        assert_eq!(
            render_logs_command(
                ContainerEngine::Podman,
                "jiji-proxy",
                None,
                None,
                None,
                None,
                false
            ),
            "podman logs --timestamps jiji-proxy"
        );
    }

    #[test]
    fn grep_options_are_inserted_between_grep_and_the_pattern() {
        assert_eq!(
            render_logs_command(
                ContainerEngine::Docker,
                "demo-web-a",
                None,
                None,
                Some("error"),
                Some("-i"),
                false,
            ),
            "docker logs --timestamps demo-web-a | grep '-i' -- 'error'"
        );
    }

    #[test]
    fn grep_options_supports_multiple_space_separated_flags() {
        assert_eq!(
            render_logs_command(
                ContainerEngine::Docker,
                "demo-web-a",
                None,
                None,
                Some("error"),
                Some("-i -v"),
                false,
            ),
            "docker logs --timestamps demo-web-a | grep '-i' '-v' -- 'error'"
        );
    }

    #[test]
    fn grep_options_cannot_inject_a_second_shell_command() {
        let command = render_logs_command(
            ContainerEngine::Docker,
            "demo-web-a",
            None,
            None,
            Some("error"),
            Some("-i; rm -rf ~/.jiji"),
            false,
        );
        assert_eq!(
            command,
            "docker logs --timestamps demo-web-a | grep '-i;' 'rm' '-rf' '~/.jiji' -- 'error'"
        );
    }

    #[test]
    fn container_name_is_configurable() {
        assert_eq!(
            render_logs_command(
                ContainerEngine::Docker,
                "demo-web-a",
                None,
                None,
                None,
                None,
                false
            ),
            "docker logs --timestamps demo-web-a"
        );
    }

    #[test]
    fn default_lines_are_bounded_only_without_filters() {
        assert_eq!(effective_lines(None, None, None), Some(100));
        assert_eq!(effective_lines(None, Some("1h"), None), None);
        assert_eq!(effective_lines(None, None, Some("error")), None);
        assert_eq!(effective_lines(Some(0), Some("1h"), Some("error")), Some(0));
    }
}
