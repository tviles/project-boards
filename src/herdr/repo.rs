use crate::model::RepoSlug;
use std::path::Path;
use std::process::Command;

/// `owner/name` from a GitHub remote URL in any of git's usual forms.
pub fn parse_github_remote(url: &str) -> Option<RepoSlug> {
    let url = url.trim();
    let rest = [
        "https://github.com/",
        "http://github.com/",
        "git@github.com:",
        "ssh://git@github.com/",
        "git://github.com/",
    ]
    .iter()
    .find_map(|prefix| url.strip_prefix(prefix))?;
    let rest = rest.trim_end_matches('/');
    rest.strip_suffix(".git").unwrap_or(rest).parse().ok()
}

/// Runs `git -C dir <args>` and returns stdout, or `None` when git fails or is missing.
pub fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The GitHub repository of the checkout at `dir`: its `origin` remote, else the first
/// GitHub remote. `git` runs a git command in `dir`; tests pass a fake.
pub fn detect_repo_with(
    dir: &Path,
    git: impl Fn(&Path, &[&str]) -> Option<String>,
) -> Option<RepoSlug> {
    if let Some(slug) =
        git(dir, &["remote", "get-url", "origin"]).and_then(|u| parse_github_remote(&u))
    {
        return Some(slug);
    }
    git(dir, &["remote", "-v"])?
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .find_map(parse_github_remote)
}

pub fn detect_repo(dir: &Path) -> Option<RepoSlug> {
    detect_repo_with(dir, run_git)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_remote_form() {
        for url in [
            "https://github.com/tviles/app.git",
            "https://github.com/tviles/app",
            "https://github.com/tviles/app/",
            "git@github.com:tviles/app.git",
            "ssh://git@github.com/tviles/app.git",
        ] {
            assert_eq!(
                parse_github_remote(url).unwrap().to_string(),
                "tviles/app",
                "{url}"
            );
        }
        assert!(parse_github_remote("https://gitlab.com/tviles/app.git").is_none());
    }

    #[test]
    fn prefers_origin_then_any_github_remote() {
        let dir = Path::new("/code/app");
        let with_origin = |_: &Path, args: &[&str]| match args {
            ["remote", "get-url", "origin"] => Some("git@github.com:tviles/app.git\n".to_string()),
            _ => None,
        };
        assert_eq!(
            detect_repo_with(dir, with_origin).unwrap().to_string(),
            "tviles/app"
        );
        let no_origin = |_: &Path, args: &[&str]| {
            match args {
            ["remote", "-v"] => Some("upstream\thttps://gitlab.com/x/y.git (fetch)\nfork\tgit@github.com:tviles/fork.git (fetch)\n".to_string()),
            _ => None,
        }
        };
        assert_eq!(
            detect_repo_with(dir, no_origin).unwrap().to_string(),
            "tviles/fork"
        );
        assert!(detect_repo_with(dir, |_: &Path, _: &[&str]| None).is_none());
    }

    /// Spawns real git; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn detects_origin_of_a_real_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        run(&["init", "-q"]);
        run(&["remote", "add", "fork", "git@github.com:tviles/app.git"]);
        assert_eq!(detect_repo(dir.path()).unwrap().to_string(), "tviles/app");
    }
}
