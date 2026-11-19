//! Other projects' jiji-agents on the same host. They share one jiji-proxy container, so an older
//! one can undo this project's proxy upgrade, and one left behind by a removed project keeps
//! reconciling that proxy with nothing in `jiji.yml` pointing at it.

use jiji_agent::paths::DEFAULT_ROOT;
use jiji_config::ContainerEngine;
use jiji_ssh::SshSession;

use crate::engine::parse_version;
use crate::version_requirements::{AGENT_BUILD_VERSION, MIN_PROXY_DOWNGRADE_GUARD_AGENT_VERSION};

/// Window `recent_proxy_creations` counts over, and the count `network diagnostics` reports.
pub const PROXY_CREATION_WINDOW_SECS: u64 = 600;
pub const PROXY_CREATION_WARN_COUNT: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoResidentAgent {
    pub unit: String,
    /// `systemctl is-active` output: `active`, `inactive`, `failed`, ...
    pub state: String,
    pub version: Option<String>,
}

impl CoResidentAgent {
    fn is_active(&self) -> bool {
        self.state == "active"
    }

    /// Unknown versions count: binaries that old predate the `version` subcommand.
    pub fn downgrades_shared_proxy(&self) -> bool {
        self.is_active()
            && version_below(
                self.version.as_deref(),
                MIN_PROXY_DOWNGRADE_GUARD_AGENT_VERSION,
            )
    }

    fn older_than_this_cli(&self) -> bool {
        version_below(self.version.as_deref(), AGENT_BUILD_VERSION)
    }

    fn describe(&self) -> String {
        format!(
            "{} (jiji-agent {}, {})",
            self.unit,
            self.version
                .as_deref()
                .map(|version| format!("v{version}"))
                .unwrap_or_else(|| "unknown version".to_string()),
            self.state
        )
    }
}

fn version_below(found: Option<&str>, floor: &str) -> bool {
    match (found.and_then(parse_version), parse_version(floor)) {
        (Some(found), Some(floor)) => found < floor,
        (None, _) => true,
        (Some(_), None) => false,
    }
}

/// One `unit state version` line per installed `jiji-agent-*` unit; `-` for an unknown version.
fn list_command() -> String {
    format!(
        "for unit in $(systemctl list-unit-files --no-legend 'jiji-agent-*.service' 2>/dev/null \
         | awk '{{print $1}}'); do \
         slug=${{unit#jiji-agent-}}; slug=${{slug%.service}}; \
         state=$(systemctl is-active \"$unit\" 2>/dev/null); \
         version=$({DEFAULT_ROOT}/\"$slug\"/bin/jiji-agent version 2>/dev/null); \
         printf '%s %s %s\\n' \"$unit\" \"${{state:-unknown}}\" \"${{version:--}}\"; done"
    )
}

fn parse(output: &str, own_unit: &str) -> Vec<CoResidentAgent> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let unit = fields.next()?;
            let state = fields.next()?;
            let version = fields.next()?;
            (unit != own_unit).then(|| CoResidentAgent {
                unit: unit.to_string(),
                state: state.to_string(),
                version: (version != "-").then(|| version.to_string()),
            })
        })
        .collect()
}

/// Every other project's jiji-agent unit installed on the host.
pub async fn inspect(session: &SshSession, project: &str) -> anyhow::Result<Vec<CoResidentAgent>> {
    let own_unit = jiji_agent::AgentPaths::default_for_project(project).unit_name;
    let result = session.execute(&list_command()).await?;
    if !result.success {
        anyhow::bail!(
            "could not list jiji-agent units on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(parse(&result.stdout, &own_unit))
}

/// One line per agent that needs attention, phrased with the fix.
pub fn warnings(agents: &[CoResidentAgent]) -> Vec<String> {
    agents
        .iter()
        .filter_map(|agent| {
            if agent.downgrades_shared_proxy() {
                Some(format!(
                    "{} predates jiji-agent v{MIN_PROXY_DOWNGRADE_GUARD_AGENT_VERSION} and puts its \
                     own older jiji-proxy back whenever the shared one is newer, which drops every \
                     route on the host each time. Run `jiji server upgrade` from that project \
                     first, or `systemctl disable --now {}` if that project no longer uses this \
                     host.",
                    agent.describe(),
                    agent.unit
                ))
            } else if !agent.is_active() {
                Some(format!(
                    "{} is not running. If that project no longer uses this host, run `jiji \
                     server teardown` from that project or `systemctl disable --now {}`.",
                    agent.describe(),
                    agent.unit
                ))
            } else if agent.older_than_this_cli() {
                Some(format!(
                    "{} is older than v{AGENT_BUILD_VERSION}. Run `jiji server upgrade` from that \
                     project.",
                    agent.describe()
                ))
            } else {
                None
            }
        })
        .collect()
}

/// How many times the jiji-proxy container was created in the last
/// `PROXY_CREATION_WINDOW_SECS`. More than one or two means agents are fighting over it.
pub async fn recent_proxy_creations(
    session: &SshSession,
    engine: ContainerEngine,
) -> anyhow::Result<usize> {
    let result = session.execute(&proxy_creations_command(engine)).await?;
    if !result.success {
        anyhow::bail!(
            "could not read jiji-proxy events on {}: {}",
            session.host(),
            result.stderr.trim()
        );
    }
    Ok(result
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count())
}

fn proxy_creations_command(engine: ContainerEngine) -> String {
    let engine = match engine {
        ContainerEngine::Docker => "docker",
        ContainerEngine::Podman => "podman",
    };
    format!(
        "now=$(date +%s); {engine} events --since $((now - {PROXY_CREATION_WINDOW_SECS})) \
         --until \"$now\" --filter container={name} --filter event=create",
        name = jiji_network::CONTAINER_NAME
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(state: &str, version: Option<&str>) -> CoResidentAgent {
        CoResidentAgent {
            unit: "jiji-agent-saca-1234.service".to_string(),
            state: state.to_string(),
            version: version.map(str::to_string),
        }
    }

    #[test]
    fn parse_skips_this_projects_own_unit_and_reads_unknown_versions() {
        let output = "jiji-agent-casa-ed21.service active 0.7.7\n\
                      jiji-agent-saca-1234.service active 0.6.5\n\
                      jiji-agent-old-9999.service failed -\n";
        let agents = parse(output, "jiji-agent-casa-ed21.service");
        assert_eq!(
            agents,
            vec![
                CoResidentAgent {
                    unit: "jiji-agent-saca-1234.service".to_string(),
                    state: "active".to_string(),
                    version: Some("0.6.5".to_string()),
                },
                CoResidentAgent {
                    unit: "jiji-agent-old-9999.service".to_string(),
                    state: "failed".to_string(),
                    version: None,
                },
            ]
        );
    }

    #[test]
    fn an_active_agent_without_the_downgrade_guard_is_flagged() {
        assert!(agent("active", Some("0.6.5")).downgrades_shared_proxy());
        assert!(agent("active", None).downgrades_shared_proxy());
        assert!(
            !agent("active", Some(MIN_PROXY_DOWNGRADE_GUARD_AGENT_VERSION))
                .downgrades_shared_proxy()
        );
        assert!(!agent("inactive", Some("0.6.5")).downgrades_shared_proxy());
    }

    #[test]
    fn warnings_name_the_unit_and_the_fix() {
        let warnings = warnings(&[agent("active", Some("0.6.5"))]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("jiji-agent-saca-1234.service"));
        assert!(warnings[0].contains("v0.6.5"));
        assert!(warnings[0].contains("jiji server upgrade"));
    }

    #[test]
    fn a_stopped_agent_is_reported_as_possibly_orphaned() {
        let warnings = warnings(&[agent("inactive", Some(AGENT_BUILD_VERSION))]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("not running"), "{}", warnings[0]);
        assert!(
            warnings[0].contains("systemctl disable --now"),
            "{}",
            warnings[0]
        );
    }

    #[test]
    fn a_current_running_agent_needs_no_warning() {
        assert!(warnings(&[agent("active", Some(AGENT_BUILD_VERSION))]).is_empty());
    }

    #[test]
    fn the_list_command_reads_each_units_own_binary() {
        let command = list_command();
        assert!(command.contains("systemctl list-unit-files"));
        assert!(command.contains("/etc/jiji/agent/\"$slug\"/bin/jiji-agent version"));
    }

    #[test]
    fn the_proxy_event_query_is_bounded_so_it_terminates() {
        let command = proxy_creations_command(ContainerEngine::Podman);
        assert!(command.starts_with("now=$(date +%s); podman events --since"));
        assert!(command.contains("--until \"$now\""));
        assert!(command.contains("--filter container=jiji-proxy --filter event=create"));
    }
}
