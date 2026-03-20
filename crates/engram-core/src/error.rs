use thiserror::Error;

#[derive(Error, Debug)]
pub enum EngramError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Embedding error: {0}")]
    Embedding(String),

    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, EngramError>;
