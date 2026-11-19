use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("embedding dimension {got} does not match store dimension {expected}")]
    WrongDimensions { got: usize, expected: usize },
    #[error("query text is empty or was fully stripped by FTS tokenization")]
    EmptyQuery,
}

pub type Result<T> = std::result::Result<T, MemoryError>;
