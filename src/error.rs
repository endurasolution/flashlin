use thiserror::Error;

#[derive(Debug, Error)]
pub enum FlashError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Config error: {0}")]
    Config(String),

    #[error("HTTP parse error: {0}")]
    Http(String),

    #[error("TLS error: {0}")]
    Tls(String),

    #[error("PHP backend error: {0}")]
    PhpBackend(String),

    #[error("Proxy error: {0}")]
    Proxy(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Access denied")]
    AccessDenied,

    #[error("Path traversal attempt detected")]
    PathTraversal,

    #[error("Sensitive file access blocked")]
    SensitiveFile,

    #[error("Request body too large")]
    BodyTooLarge,

    #[error("Request headers too large")]
    HeadersTooLarge,

    #[error("Backend timeout")]
    Timeout,

    #[error("Not found")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, FlashError>;
