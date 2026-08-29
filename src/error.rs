use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error("session not found: {0}")]
    NotFound(String),
    #[error("ambiguous session {reference:?}: {matches:?}")]
    Ambiguous {
        reference: String,
        matches: Vec<String>,
    },
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl Error {
    pub fn msg(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
