//! Long text in the user's editor: titles, bodies, comments and new items.

use std::path::Path;

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

/// Writes `initial` to a private (0600) `.md` file in `dir` (created 0700 if missing), runs
/// `command` with the file as its last argument, and returns what the file holds afterwards.
/// `Ok(None)` when the editor exits with an error; `Err` when it could not run. The file is
/// removed either way.
pub fn edit_text(initial: &str, command: &[String], dir: &Path) -> std::io::Result<Option<String>> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| std::io::Error::other("no editor command"))?;
    private_dir(dir)?;
    let path = dir.join(format!("edit-{}.md", std::process::id()));
    crate::fsutil::write_atomic(&path, initial.as_bytes())?;
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status();
    let result = match status {
        Ok(s) if s.success() => std::fs::read_to_string(&path).map(Some),
        Ok(_) => Ok(None),
        Err(e) => Err(e),
    };
    // Best effort: a failed removal must not lose what the user wrote.
    let _ = std::fs::remove_file(&path);
    result
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

/// The first non-empty line is the title; everything after it is the body (trimmed).
/// `None` when there is no title.
pub fn title_and_body(text: &str) -> Option<(String, String)> {
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    let title = lines.next()?.trim().to_string();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Some((title, body))
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

    #[test]
    fn the_saved_text_comes_back_and_a_failing_editor_gives_none() {
        let dir = tempfile::tempdir().unwrap();
        let sh = |script: &str| vec!["sh".to_string(), "-c".to_string(), script.to_string()];
        let saved = edit_text(
            "old",
            &sh("printf 'New title\\n\\nNew body' > \"$0\""),
            dir.path(),
        )
        .unwrap();
        assert_eq!(saved.as_deref(), Some("New title\n\nNew body"));
        let kept = edit_text("unchanged", &sh("true"), dir.path()).unwrap();
        assert_eq!(kept.as_deref(), Some("unchanged"));
        assert_eq!(edit_text("x", &sh("exit 3"), dir.path()).unwrap(), None);
        assert!(edit_text("x", &["/no/such/editor".to_string()], dir.path()).is_err());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "the file is removed"
        );
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
        let out = edit_text("secret", &["sh".into(), "-c".into(), script], &dir).unwrap();
        assert_eq!(out.as_deref(), Some("secret"));
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
