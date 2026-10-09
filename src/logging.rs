use std::path::Path;
use tracing_subscriber::EnvFilter;

/// Logs to `<state_dir>/project-boards.log`; verbosity from RUST_LOG (default `info`).
/// The returned guard flushes the log when dropped. Nothing here ever logs the token.
pub fn init(state_dir: &Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    std::fs::create_dir_all(state_dir).ok()?;
    let appender = tracing_appender::rolling::never(state_dir, "project-boards.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .ok()?;
    Some(guard)
}
