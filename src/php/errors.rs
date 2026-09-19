use thiserror::Error;

#[derive(Debug, Error)]
pub enum PhpError {
    #[error("FastCGI connection failed: {0}")]
    Connection(#[from] std::io::Error),

    #[error("FastCGI protocol error: {0}")]
    Protocol(String),

    #[error("PHP backend timeout")]
    Timeout,

    #[error("PHP backend returned invalid response: {0}")]
    InvalidResponse(String),

    #[error("PHP script not found: {0}")]
    ScriptNotFound(String),

    #[error("PHP execution denied in path: {0}")]
    ExecutionDenied(String),
}
