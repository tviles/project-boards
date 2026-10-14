//! Long text in the user's editor: titles, bodies, comments and new items.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// `$VISUAL`, else `$EDITOR`, else `vi`. A value is split into words as a shell would
/// (quotes group, so `code -w` and `'/path with spaces/ed'` both work), but it never runs
/// through a shell. A blank or unparseable value (an unterminated quote) is skipped.
pub fn editor_command(env: impl Fn(&str) -> Option<String>) -> Vec<String> {
    for var in ["VISUAL", "EDITOR"] {
        let Some(value) = env(var) else { continue };
        match shell_words::split(&value) {
            Ok(words) if !words.is_empty() => return words,
            Ok(_) => {}
            Err(_) => tracing::warn!(variable = var, "could not split the editor command"),
        }
    }
    vec!["vi".to_string()]
}

/// The directory under the plugin's state directory that holds the editor's files.
pub fn edit_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("edit")
}

/// An editor that returns sooner than this probably opened a window and did not wait for it.
pub const QUICK: Duration = Duration::from_secs(1);

/// How an editor session ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Edited {
    /// The editor exited cleanly and its file holds `text`. `quick`: it returned within
    /// `QUICK`.
    Saved { text: String, quick: bool },
    /// The editor exited with an error.
    Failed,
    /// The file holds bytes that aren't UTF-8; `lossy` is the text with U+FFFD for them.
    NotUtf8 { lossy: String },
    /// The editor could not start, or its file could not be written or read: the reason, for
    /// the status line ("Could not start vi: ...").
    Error(String),
}

/// A plain save (`Some`) or a failed run (`None`), for callers that do not time the editor.
impl From<Option<String>> for Edited {
    fn from(text: Option<String>) -> Self {
        match text {
            Some(text) => Edited::Saved { text, quick: false },
            None => Edited::Failed,
        }
    }
}

/// Writes `initial` to a private (0600) `.md` file in `dir` (created 0700 if missing), runs
/// `command` with the file as its last argument, and reads what the file holds afterwards.
/// The file, and the swap and backup files editors commonly leave next to it, are removed
/// whatever happened.
pub fn edit_text(initial: &str, command: &[String], dir: &Path) -> Edited {
    let Some((program, args)) = command.split_first() else {
        return Edited::Error("No editor command".into());
    };
    let path = dir.join(format!("edit-{}.md", std::process::id()));
    if let Err(e) =
        private_dir(dir).and_then(|()| crate::fsutil::write_atomic(&path, initial.as_bytes()))
    {
        remove_with_leftovers(&path);
        return Edited::Error(format!("Could not write the editor's file: {e}"));
    }
    let started = Instant::now();
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status();
    let quick = started.elapsed() < QUICK;
    let edited = match status {
        Err(e) => Edited::Error(format!("Could not start {program}: {e}")),
        Ok(s) if !s.success() => Edited::Failed,
        Ok(_) => match std::fs::read(&path) {
            Err(e) => Edited::Error(format!("Could not read the editor's file: {e}")),
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Edited::Saved { text, quick },
                Err(e) => Edited::NotUtf8 {
                    lossy: String::from_utf8_lossy(e.as_bytes()).into_owned(),
                },
            },
        },
    };
    remove_with_leftovers(&path);
    edited
}

/// Removes `path` and what editors commonly leave next to it: vim's `.<name>.swp` and
/// `.<name>.swo`, `<name>~` backups and nano's `<name>.save`. Best effort.
fn remove_with_leftovers(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return;
    };
    let names = [
        name.to_string(),
        format!(".{name}.swp"),
        format!(".{name}.swo"),
        format!("{name}~"),
        format!("{name}.save"),
    ];
    for name in names {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Empties `dir` of the files of editor sessions whose board process is gone (`alive` says
/// whether a pid still runs): text a crash or a killed pane left behind, and its swap and
/// backup files. A file naming no pid is removed too; a live pane's session is left alone.
/// A missing `dir` is fine.
pub fn clear_stale(dir: &Path, alive: impl Fn(u32) -> bool) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    // Every file is tried; the first failure is reported.
    let mut failed = None;
    for entry in entries {
        let removed = entry.and_then(|entry| {
            if session_pid(&entry.file_name().to_string_lossy()).is_some_and(&alive) {
                return Ok(());
            }
            std::fs::remove_file(entry.path())
        });
        if let Err(e) = removed {
            failed.get_or_insert(e);
        }
    }
    failed.map_or(Ok(()), Err)
}

/// The pid in a session file's name: `edit-<pid>.md` and the leftovers named after it.
fn session_pid(name: &str) -> Option<u32> {
    let rest = &name[name.find("edit-")? + "edit-".len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Whether a process with this pid runs (`kill -0`, no shell). When that can't be told, it is
/// taken to run, so nothing of a live session is removed. 0 and anything above `i32::MAX` are
/// no process: `kill` would read them as a process group or wrap them to -1 ("every process").
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || i32::try_from(pid).is_err() {
        return false;
    }
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

/// Creates `dir` readable by its owner only; an existing one is left as it is.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// `text` without its leading and trailing blank (whitespace-only) lines. Indentation and
/// the lines between are kept (Decision 25: Markdown indentation matters).
pub fn trim_blank_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(start) = lines.iter().position(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(start);
    lines[start..=end].join("\n")
}

/// The first non-blank line is the title (trimmed); everything after it is the body, without
/// leading or trailing blank lines but with its indentation. `None` when there is no title.
pub fn title_and_body(text: &str) -> Option<(String, String)> {
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    let title = lines.next()?.trim().to_string();
    let rest = lines.collect::<Vec<_>>().join("\n");
    Some((title, trim_blank_lines(&rest)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_comes_from_visual_then_editor_then_vi() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            editor_command(env(&[("VISUAL", "code -w"), ("EDITOR", "nano")])),
            ["code", "-w"]
        );
        assert_eq!(
            editor_command(env(&[("VISUAL", " "), ("EDITOR", "nano")])),
            ["nano"]
        );
        assert_eq!(editor_command(env(&[])), ["vi"]);
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".to_string(), "-c".to_string(), script.to_string()]
    }

    fn saved(text: &str, quick: bool) -> Edited {
        Edited::Saved {
            text: text.into(),
            quick,
        }
    }

    #[test]
    fn the_saved_text_comes_back_and_a_failing_editor_gives_none() {
        let dir = tempfile::tempdir().unwrap();
        let edited = edit_text(
            "old",
            &sh("printf 'New title\\n\\nNew body' > \"$0\""),
            dir.path(),
        );
        assert_eq!(edited, saved("New title\n\nNew body", true));
        let kept = edit_text("unchanged", &sh("true"), dir.path());
        assert_eq!(kept, saved("unchanged", true), "returned at once");
        let slow = edit_text("unchanged", &sh("sleep 1.2"), dir.path());
        assert_eq!(slow, saved("unchanged", false));
        assert_eq!(edit_text("x", &sh("exit 3"), dir.path()), Edited::Failed);
        match edit_text("x", &["/no/such/editor".to_string()], dir.path()) {
            Edited::Error(m) => assert!(m.starts_with("Could not start /no/such/editor: "), "{m}"),
            other => panic!("expected an error, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "the file is removed"
        );
    }

    #[test]
    fn text_that_is_not_utf8_comes_back_lossily() {
        let dir = tempfile::tempdir().unwrap();
        let edited = edit_text("x", &sh("printf 'caf\\351 ok' > \"$0\""), dir.path());
        assert_eq!(
            edited,
            Edited::NotUtf8 {
                lossy: "caf\u{FFFD} ok".into()
            }
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn swap_and_backup_files_next_to_the_file_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let script = "d=$(dirname \"$0\"); n=$(basename \"$0\"); \
                      for f in \".$n.swp\" \".$n.swo\" \"$n~\" \"$n.save\" keep.txt; do \
                      echo x > \"$d/$f\"; done";
        assert_eq!(edit_text("t", &sh(script), dir.path()), saved("t", true));
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["keep.txt"]);
    }

    #[test]
    fn stale_sessions_are_cleared_and_live_ones_kept() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("edit");
        assert!(
            clear_stale(&dir, |_| true).is_ok(),
            "a missing directory is fine"
        );
        std::fs::create_dir(&dir).unwrap();
        for name in [
            "edit-11.md",
            ".edit-11.md.swp",
            "edit-11.md~",
            "edit-22.md",
            ".edit-22.md.swo",
            "stray",
        ] {
            std::fs::write(dir.join(name), "text").unwrap();
        }
        clear_stale(&dir, |pid| pid == 22).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".edit-22.md.swo", "edit-22.md"]);
    }

    #[test]
    fn this_process_is_alive_and_an_impossible_pid_is_not() {
        assert!(pid_alive(std::process::id()));
        // Above every system's pid limit, but a valid pid_t.
        assert!(!pid_alive(i32::MAX as u32));
        // Not pids at all: `kill` would take 0 as the process group and wrap u32::MAX to -1.
        assert!(!pid_alive(0));
        assert!(!pid_alive(u32::MAX));
    }

    #[test]
    fn the_first_line_is_the_title_and_the_rest_the_body() {
        assert_eq!(
            title_and_body("\n  Title  \n\nBody line\nmore\n"),
            Some(("Title".into(), "Body line\nmore".into()))
        );
        assert_eq!(
            title_and_body("Only a title"),
            Some(("Only a title".into(), String::new()))
        );
        assert_eq!(title_and_body(" \n\n"), None);
    }

    /// Decision 25: only blank lines are trimmed off the body; Markdown indentation stays.
    #[test]
    fn the_body_keeps_its_indentation() {
        assert_eq!(
            title_and_body("Title\n\n    let x = 1;\n  - item\n\n  \n"),
            Some(("Title".into(), "    let x = 1;\n  - item".into()))
        );
        assert_eq!(
            trim_blank_lines("\n \n  indented\n\nlast  \n\n"),
            "  indented\n\nlast  "
        );
        assert_eq!(trim_blank_lines(" \n\t\n"), "");
    }

    /// Quotes group words as a shell would, but nothing runs through a shell.
    #[test]
    fn the_editor_command_is_split_like_a_shell_would() {
        let env = |visual: &'static str| {
            move |k: &str| match k {
                "VISUAL" => Some(visual.to_string()),
                "EDITOR" => Some("nano".to_string()),
                _ => None,
            }
        };
        assert_eq!(
            editor_command(env("'/Applications/My Editor/bin/ed' --wait \"two words\"")),
            ["/Applications/My Editor/bin/ed", "--wait", "two words"]
        );
        assert_eq!(
            editor_command(env("vim; rm -rf ~")),
            ["vim;", "rm", "-rf", "~"]
        );
        assert_eq!(
            editor_command(env("'unterminated")),
            ["nano"],
            "unparseable: the next one"
        );
    }

    /// The text never sits in a file others can read, and the file is a `.md` (so editors
    /// highlight Markdown).
    #[cfg(unix)]
    #[test]
    fn the_file_is_private_markdown_in_a_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("edit");
        let (copy, name) = (root.path().join("copy"), root.path().join("name"));
        // `cp -p` keeps the mode, so the copy shows what the editor was given.
        let script = format!(
            "cp -p \"$0\" '{}' && printf %s \"$0\" > '{}'",
            copy.display(),
            name.display()
        );
        let out = edit_text("secret", &sh(&script), &dir);
        assert!(matches!(out, Edited::Saved { text, .. } if text == "secret"));
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&copy), 0o600);
        assert!(std::fs::read_to_string(&name).unwrap().ends_with(".md"));
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "the file is removed"
        );
    }
}
