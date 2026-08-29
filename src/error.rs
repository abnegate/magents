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

#[cfg(test)]
mod tests {
    use super::Error;
    use std::path::PathBuf;

    #[test]
    fn displays_every_variant() {
        assert_eq!(Error::msg("x").to_string(), "x");
        assert_eq!(
            Error::NotFound("s".into()).to_string(),
            "session not found: s"
        );
        assert!(
            Error::Ambiguous {
                reference: "r".into(),
                matches: vec!["a".into(), "b".into()],
            }
            .to_string()
            .contains("ambiguous")
        );
        let io = Error::Io {
            path: PathBuf::from("/tmp/x"),
            source: std::io::Error::other("boom"),
        };
        assert!(io.to_string().contains("/tmp/x"));
        let json = Error::from(serde_json::from_str::<u8>("nope").unwrap_err());
        assert!(json.to_string().contains("json error"));
    }
}
