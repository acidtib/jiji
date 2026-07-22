use thiserror::Error;

#[derive(Debug, Error)]
pub enum JijiError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}
