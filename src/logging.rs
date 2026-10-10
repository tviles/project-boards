use std::path::Path;
use tracing_subscriber::EnvFilter;

pub const LOG_FILE: &str = "project-boards.log";

/// Logs to `<state_dir>/project-boards.log` (mode 0600); verbosity from RUST_LOG (default
/// `info`). The returned guard flushes the log when dropped. Nothing here ever logs the token.
pub fn init(state_dir: &Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    std::fs::create_dir_all(state_dir).ok()?;
    // The appender appends to an existing file and keeps its mode, so create it private first.
    crate::fsutil::ensure_private_file(&state_dir.join(LOG_FILE)).ok()?;
    let appender = tracing_appender::rolling::never(state_dir, LOG_FILE);
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
