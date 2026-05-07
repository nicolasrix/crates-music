use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("HTTP transport: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("invalid URL: {0}")]
    InvalidUrl(#[from] url::ParseError),

    #[error("malformed Subsonic response: {0}")]
    BadResponse(String),

    #[error("client misconfiguration: {0}")]
    Config(String),

    #[error("Subsonic error {code}: {message}")]
    Subsonic { code: i32, message: String },

    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
}

impl Error {
    /// Returns `(code, message)` if this is a server-side Subsonic error,
    /// otherwise `None`. Useful for distinguishing transport failures
    /// from API-level rejections.
    pub fn subsonic_error(&self) -> Option<(i32, &str)> {
        if let Self::Subsonic { code, message } = self {
            Some((*code, message.as_str()))
        } else {
            None
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
