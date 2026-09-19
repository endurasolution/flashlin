pub mod access_log;
pub mod error_log;

/// Initialise the global tracing subscriber from the environment or config.
pub fn init(config: Option<&crate::config::LoggingConfig>) -> anyhow::Result<()> {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.map(|c| c.level.as_str()).unwrap_or("info")));

    if config.is_some_and(|c| c.json) {
        fmt()
            .with_env_filter(filter)
            .with_target(false)
            .json()
            .init();
    } else {
        fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_thread_ids(false)
            .compact()
            .init();
    }

    Ok(())
}
