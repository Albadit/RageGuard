//! Structured logging setup.

use tracing_subscriber::EnvFilter;

/// Initialises `tracing`. `RUST_LOG`, when set, overrides `LOG_LEVEL` completely; otherwise
/// RageGuard logs at `level` and dependencies only log warnings.
pub fn init(level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,rageguard={level}")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
