use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::BackendError;
use crate::model::Provider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    LinearApiKeyEnv,
    Command,
}

impl KeySource {
    pub fn describe(&self) -> &'static str {
        match self {
            KeySource::LinearApiKeyEnv => "LINEAR_API_KEY",
            KeySource::Command => "linear_api_key_command",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey {
    pub value: String,
    pub source: KeySource,
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("source", &self.source)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A dismissed Keychain prompt must not hang the pane.
pub const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

/// Resolves the key: `LINEAR_API_KEY`, then the configured command's output. Blank values are
/// skipped.
pub fn resolve_key(
    env: impl Fn(&str) -> Option<String>,
    command: Option<&[String]>,
    run: impl FnOnce(&[String]) -> Option<String>,
) -> Result<ApiKey, BackendError> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(value) = clean(env("LINEAR_API_KEY")) {
        return Ok(ApiKey {
            value,
            source: KeySource::LinearApiKeyEnv,
        });
    }
    if let Some(argv) = command
        && let Some(value) = clean(run(argv))
    {
        return Ok(ApiKey {
            value,
            source: KeySource::Command,
        });
    }
    Err(BackendError::NoCredentials {
        provider: Provider::Linear,
    })
}

/// Runs `argv` without a shell and returns its stdout. `None` on an empty argv, spawn failure,
/// non-zero exit or timeout (the child is killed and reaped). What the command prints is never
/// logged.
pub fn run_key_command(argv: &[String], timeout: Duration) -> Option<String> {
    let (program, args) = argv.split_first()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // A background process that inherited stdout can keep the pipe open after the child
    // exits, so the read shares the overall deadline and the reader is never joined.
    let status = status?;
    let bytes = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

pub fn resolve_key_from_system(command: Option<&[String]>) -> Result<ApiKey, BackendError> {
    resolve_key(
        |key| std::env::var(key).ok(),
        command,
        |argv| run_key_command(argv, KEY_COMMAND_TIMEOUT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| v.to_string())
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn linear_api_key_wins_over_the_command() {
        let cmd = argv(&["x"]);
        let k = resolve_key(env_of(&[("LINEAR_API_KEY", "a")]), Some(&cmd), |_| {
            panic!("command must not run")
        })
        .unwrap();
        assert_eq!(
            (k.value.as_str(), k.source),
            ("a", KeySource::LinearApiKeyEnv)
        );
    }

    #[test]
    fn blank_values_fall_through_to_the_command() {
        let cmd = argv(&["x"]);
        let k = resolve_key(env_of(&[("LINEAR_API_KEY", "  ")]), Some(&cmd), |_| {
            Some("c\n".into())
        })
        .unwrap();
        assert_eq!((k.value.as_str(), &k.source), ("c", &KeySource::Command));
        assert_eq!(k.source.describe(), "linear_api_key_command");
        assert_eq!(KeySource::LinearApiKeyEnv.describe(), "LINEAR_API_KEY");
    }

    #[test]
    fn nothing_found_is_no_linear_credentials() {
        let none = Err(BackendError::NoCredentials {
            provider: Provider::Linear,
        });
        assert_eq!(resolve_key(env_of(&[]), None, |_| None), none);
        let cmd = argv(&["x"]);
        assert_eq!(
            resolve_key(env_of(&[]), Some(&cmd), |_| Some(" \n".into())),
            none
        );
    }

    #[test]
    fn debug_never_prints_the_key() {
        let k = ApiKey {
            value: "lin_api_secret".into(),
            source: KeySource::Command,
        };
        let shown = format!("{k:?}");
        assert!(!shown.contains("lin_api_secret"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn the_command_runs_without_a_shell() {
        let out = run_key_command(&argv(&["printf", "%s", "k $HOME"]), Duration::from_secs(5));
        assert_eq!(out.as_deref(), Some("k $HOME"));
    }

    #[test]
    fn a_failing_command_gives_none() {
        assert_eq!(
            run_key_command(&argv(&["false"]), Duration::from_secs(5)),
            None
        );
        assert_eq!(
            run_key_command(&argv(&["/nonexistent/x"]), Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn a_hanging_command_times_out() {
        let start = Instant::now();
        let out = run_key_command(&argv(&["sleep", "5"]), Duration::from_millis(200));
        assert_eq!(out, None);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_background_helper_cannot_hold_the_pipe_open() {
        let start = Instant::now();
        let _ = run_key_command(
            &argv(&["sh", "-c", "sleep 5 & echo k"]),
            Duration::from_millis(300),
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_large_output_does_not_deadlock() {
        let out = run_key_command(
            &argv(&["sh", "-c", "head -c 300000 /dev/zero | tr '\\0' k"]),
            Duration::from_secs(10),
        );
        assert_eq!(out.map(|s| s.len()), Some(300000));
    }
}
