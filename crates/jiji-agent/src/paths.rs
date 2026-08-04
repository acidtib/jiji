//! Every filesystem path the agent touches on a host, derived purely from the project name (via
//! `jiji_network::systemd_unit_slug`, the same slug the network layer already uses) plus a root
//! directory. Two projects on one host get disjoint paths under the same root by construction --
//! there is no code path that lists or globs a sibling project's directory.

use std::path::{Path, PathBuf};

use jiji_network::systemd_unit_slug;

/// Default root jiji owns on a host for agent binaries and state, mirroring the existing
/// `/etc/jiji/...` convention used by the network layer (`commands/network/setup.rs`) and
/// kamal-proxy ingress rules (`proxy_ingress.rs`) rather than splitting state across `/var/lib`.
pub const DEFAULT_ROOT: &str = "/etc/jiji/agent";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPaths {
    /// `{root}/{slug}`
    pub project_dir: PathBuf,
    /// `{root}/{slug}/bin/jiji-agent`
    pub binary_path: PathBuf,
    /// `{root}/{slug}/state`
    pub state_dir: PathBuf,
    /// `{root}/{slug}/state/agent.sqlite3`
    pub db_path: PathBuf,
    /// `{root}/{slug}/agent.sock`
    pub socket_path: PathBuf,
    /// `{root}/{slug}/mesh.json`
    pub mesh_config_path: PathBuf,
    /// `{root}/{slug}/membership-bootstrap.json`
    pub membership_bootstrap_path: PathBuf,
    /// `jiji-agent-{slug}.service`
    pub unit_name: String,
    /// `/etc/systemd/system/jiji-agent-{slug}.service`
    pub unit_path: PathBuf,
}

impl AgentPaths {
    pub fn for_project(project: &str, root: &Path) -> Self {
        let slug = systemd_unit_slug(project);
        let project_dir = root.join(&slug);
        let unit_name = format!("jiji-agent-{slug}.service");
        Self {
            binary_path: project_dir.join("bin").join("jiji-agent"),
            state_dir: project_dir.join("state"),
            db_path: project_dir.join("state").join("agent.sqlite3"),
            socket_path: project_dir.join("agent.sock"),
            mesh_config_path: project_dir.join("mesh.json"),
            membership_bootstrap_path: project_dir.join("membership-bootstrap.json"),
            unit_path: Path::new("/etc/systemd/system").join(&unit_name),
            unit_name,
            project_dir,
        }
    }

    pub fn default_for_project(project: &str) -> Self {
        Self::for_project(project, Path::new(DEFAULT_ROOT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adversarial_inputs() -> Vec<&'static str> {
        vec![
            "demo",
            "My App",
            "my_app",
            "a-very-long-project-name-that-goes-on-and-on-and-on",
            "unicode-\u{1F600}-project",
            "has/slashes/and spaces",
        ]
    }

    #[test]
    fn paths_are_deterministic_and_nested_under_root() {
        let root = Path::new("/etc/jiji/agent");
        for project in adversarial_inputs() {
            let a = AgentPaths::for_project(project, root);
            let b = AgentPaths::for_project(project, root);
            assert_eq!(a, b);
            assert!(a.binary_path.starts_with(&a.project_dir));
            assert!(a.state_dir.starts_with(&a.project_dir));
            assert!(a.db_path.starts_with(&a.state_dir));
            assert!(a.socket_path.starts_with(&a.project_dir));
            assert!(a.mesh_config_path.starts_with(&a.project_dir));
            assert!(a.membership_bootstrap_path.starts_with(&a.project_dir));
            assert!(a.unit_path.starts_with("/etc/systemd/system"));
            assert!(a.unit_name.starts_with("jiji-agent-"));
            assert!(a.unit_name.ends_with(".service"));
        }
    }

    #[test]
    fn two_projects_never_share_a_path() {
        let root = Path::new("/etc/jiji/agent");
        let a = AgentPaths::for_project("project-a", root);
        let b = AgentPaths::for_project("project-b", root);
        assert_ne!(a.project_dir, b.project_dir);
        assert_ne!(a.binary_path, b.binary_path);
        assert_ne!(a.state_dir, b.state_dir);
        assert_ne!(a.db_path, b.db_path);
        assert_ne!(a.socket_path, b.socket_path);
        assert_ne!(a.mesh_config_path, b.mesh_config_path);
        assert_ne!(a.membership_bootstrap_path, b.membership_bootstrap_path);
        assert_ne!(a.unit_name, b.unit_name);
        assert_ne!(a.unit_path, b.unit_path);
        assert!(!a.project_dir.starts_with(&b.project_dir));
        assert!(!b.project_dir.starts_with(&a.project_dir));
    }

    #[test]
    fn default_for_project_uses_default_root() {
        let paths = AgentPaths::default_for_project("demo");
        assert!(paths.project_dir.starts_with(DEFAULT_ROOT));
    }
}
