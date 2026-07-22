use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    NotFound(String),

    #[error("Configuration file not found: {0}")]
    FileNotFound(String),

    #[error("Configuration file must contain a valid YAML object")]
    NotAnObject,

    #[error("Failed to load configuration from {path}: {source}")]
    Load {
        path: String,
        #[source]
        source: serde_yaml::Error,
    },

    #[error("Configuration validation failed:\n{0}")]
    Invalid(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
