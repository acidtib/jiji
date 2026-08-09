//! Renders the project-scoped `jiji-agent-{slug}.service` unit, in the same hand-written style as
//! `crates/jiji-cli/src/proxy_ingress.rs`'s ingress-restore unit and
//! `commands/network/setup.rs`'s DNS/service-NAT units.

use crate::engine::Engine;
use crate::paths::AgentPaths;

/// `KillMode=process` (default is `control-group`) is load-bearing, not cosmetic: podman here runs
/// with `CgroupManager=cgroupfs` (`engine.rs`'s static-Podman config), so a container's
/// conmon/crun process stays in this unit's own cgroup rather than escaping into a
/// systemd-delegated scope. Under the default `control-group` mode, stopping/restarting this unit
/// (an agent binary upgrade, `Restart=on-failure`, anything) SIGKILLs that whole cgroup --
/// silently killing every container the agent is managing along with it (confirmed live: the
/// shared jiji-proxy container died on an unrelated agent restart, `podman ps` kept reporting it
/// "Up" since conmon never got the chance to record an orderly exit, and only a resolver-level
/// probe -- `podman exec`/the admin socket -- caught the drift). `process` mode kills only the
/// tracked main PID (jiji-agent itself); already-running containers are untouched and get
/// re-adopted by `local_reconcile.rs`'s own discovery on the next start, same as any other agent
/// restart.
pub fn render_unit(paths: &AgentPaths, project: &str, engine: Engine) -> String {
    format!(
        "[Unit]\n\
         Description=Jiji agent ({project})\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         StartLimitIntervalSec=60\n\
         StartLimitBurst=10\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={binary} run --project {project} --engine {engine} \
         --state-dir {state_dir} --socket {socket} --mesh-config {mesh_config}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         KillMode=process\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        binary = paths.binary_path.display(),
        state_dir = paths.state_dir.display(),
        socket = paths.socket_path.display(),
        mesh_config = paths.mesh_config_path.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn unit_execs_the_agent_binary_with_project_scoped_paths() {
        let paths = AgentPaths::for_project("demo", Path::new("/etc/jiji/agent"));
        let unit = render_unit(&paths, "demo", Engine::Docker);
        assert!(unit.contains(&format!(
            "ExecStart={} run --project demo --engine docker --state-dir {} --socket {} --mesh-config {}\n",
            paths.binary_path.display(),
            paths.state_dir.display(),
            paths.socket_path.display(),
            paths.mesh_config_path.display(),
        )));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=2"));
        assert!(unit.contains("StartLimitIntervalSec=60"));
        assert!(unit.contains("StartLimitBurst=10"));
        assert!(unit.contains("WantedBy=multi-user.target"));
    }

    /// Regression test for a confirmed-live bug: without this, restarting the agent unit
    /// (`Restart=on-failure`, a binary upgrade) SIGKILLs every container the agent manages,
    /// since `cgroupfs`-managed podman containers stay in this unit's own cgroup.
    #[test]
    fn unit_never_kills_the_containers_it_manages_on_its_own_restart() {
        let paths = AgentPaths::for_project("demo", Path::new("/etc/jiji/agent"));
        let unit = render_unit(&paths, "demo", Engine::Podman);
        assert!(unit.contains("KillMode=process"));
    }

    #[test]
    fn unit_records_the_configured_engine() {
        let paths = AgentPaths::for_project("demo", Path::new("/etc/jiji/agent"));
        let unit = render_unit(&paths, "demo", Engine::Podman);
        assert!(unit.contains("--engine podman"));
    }
}
