mod constants;
mod error;

pub use constants::*;
pub use error::JijiError;

pub type Result<T> = std::result::Result<T, JijiError>;
