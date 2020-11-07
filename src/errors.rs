use thiserror::Error;

/// Result shorthand for a `std::result::Result` wrapping our own `Error`
pub type Result<T> = std::result::Result<T, BeelineError>;

#[derive(Error, Debug)]
pub enum BeelineError {
    #[error("")]
    PropagationError(String),
}
