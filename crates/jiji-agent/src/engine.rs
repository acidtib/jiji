//! The agent's own minimal container-engine enum, deliberately independent of
//! `jiji_config::ContainerEngine` so this crate (and the binary it produces, which runs
//! standalone on a host) does not need to depend on the full config-schema crate. `jiji-cli`
//! converts from its own `ContainerEngine` when installing the agent (`agent_install.rs`).

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Docker,
    Podman,
}

impl Engine {
    pub fn as_str(&self) -> &'static str {
        match self {
            Engine::Docker => "docker",
            Engine::Podman => "podman",
        }
    }
}

impl fmt::Display for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown container engine '{0}', expected 'docker' or 'podman'")]
pub struct UnknownEngine(String);

impl FromStr for Engine {
    type Err = UnknownEngine;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "docker" => Ok(Engine::Docker),
            "podman" => Ok(Engine::Podman),
            other => Err(UnknownEngine(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_display_and_from_str() {
        for engine in [Engine::Docker, Engine::Podman] {
            assert_eq!(engine.to_string().parse::<Engine>().unwrap(), engine);
        }
    }

    #[test]
    fn rejects_unknown_engines() {
        assert!("moby".parse::<Engine>().is_err());
    }
}
