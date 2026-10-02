use crate::github::GithubError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    GhTokenEnv,
    GithubTokenEnv,
    GhCli,
}

impl TokenSource {
    pub fn describe(&self) -> &'static str {
        match self {
            TokenSource::GhTokenEnv => "GH_TOKEN",
            TokenSource::GithubTokenEnv => "GITHUB_TOKEN",
            TokenSource::GhCli => "gh auth token",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Token {
    pub value: String,
    pub source: TokenSource,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("source", &self.source)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Resolves the token in spec order: GH_TOKEN, GITHUB_TOKEN, then `gh auth token`.
/// Blank values are skipped. GitHub does not let fine-grained tokens read boards owned by a
/// user account; those need a classic token or the `gh` login.
pub fn resolve_token(
    env: impl Fn(&str) -> Option<String>,
    gh_auth_token: impl FnOnce() -> Option<String>,
) -> Result<Token, GithubError> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(value) = clean(env("GH_TOKEN")) {
        return Ok(Token {
            value,
            source: TokenSource::GhTokenEnv,
        });
    }
    if let Some(value) = clean(env("GITHUB_TOKEN")) {
        return Ok(Token {
            value,
            source: TokenSource::GithubTokenEnv,
        });
    }
    if let Some(value) = clean(gh_auth_token()) {
        return Ok(Token {
            value,
            source: TokenSource::GhCli,
        });
    }
    Err(GithubError::NoToken)
}

pub fn resolve_token_from_system() -> Result<Token, GithubError> {
    resolve_token(
        |key| std::env::var(key).ok(),
        || {
            let out = std::process::Command::new("gh")
                .args(["auth", "token"])
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
        },
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

    #[test]
    fn gh_token_wins() {
        let t = resolve_token(env_of(&[("GH_TOKEN", "a"), ("GITHUB_TOKEN", "b")]), || {
            Some("c".into())
        })
        .unwrap();
        assert_eq!((t.value.as_str(), t.source), ("a", TokenSource::GhTokenEnv));
    }

    #[test]
    fn blank_env_falls_through_to_gh() {
        let t = resolve_token(env_of(&[("GH_TOKEN", "  "), ("GITHUB_TOKEN", "")]), || {
            Some("c\n".into())
        })
        .unwrap();
        assert_eq!((t.value.as_str(), t.source), ("c", TokenSource::GhCli));
    }

    #[test]
    fn nothing_found_is_no_token() {
        assert_eq!(
            resolve_token(env_of(&[]), || None),
            Err(GithubError::NoToken)
        );
    }

    #[test]
    fn debug_never_prints_the_token() {
        let t = Token {
            value: "ghp_secret".into(),
            source: TokenSource::GhCli,
        };
        assert!(!format!("{t:?}").contains("ghp_secret"));
    }
}
