mod constants;
mod error;
mod pattern;

pub use constants::*;
pub use error::JijiError;
pub use pattern::matches_pattern;

pub type Result<T> = std::result::Result<T, JijiError>;
