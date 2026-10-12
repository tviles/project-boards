use serde::Deserialize;

/// One entry of a GraphQL response's `errors` array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphqlErrorEntry {
    pub message: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GithubError {
    #[error("no GitHub token found: set GH_TOKEN or run `gh auth login`")]
    NoToken,
    #[error("GitHub rejected the token (401 Unauthorized); run `gh auth login`")]
    Unauthorized,
    #[error("the token is missing the {} scope; run `gh auth refresh -s project`", .missing.join(", "))]
    InsufficientScopes { missing: Vec<String> },
    #[error("rate limited by GitHub; retry in {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("GitHub returned an error: {}", messages(.0))]
    Graphql(Vec<GraphqlErrorEntry>),
    #[error("network error: {0}")]
    Network(String),
    #[error("could not decode GitHub's response: {0}")]
    Decode(String),
}

fn messages(errors: &[GraphqlErrorEntry]) -> String {
    errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Extracts the scopes GitHub says are required from an INSUFFICIENT_SCOPES message, e.g.
/// "... requires one of the following scopes: ['read:project'], but your token has only been granted the: ['repo'] scopes."
pub fn required_scopes(message: &str) -> Vec<String> {
    let marker = "scopes: [";
    let Some(start) = message.find(marker) else {
        return Vec::new();
    };
    let rest = &message[start + marker.len()..];
    let Some(end) = rest.find(']') else {
        return Vec::new();
    };
    rest[..end]
        .split(',')
        .map(|s| s.trim().trim_matches('\'').trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Errors GitHub attaches to otherwise usable data, for nodes the viewer cannot see.
pub fn is_tolerable(error: &GraphqlErrorEntry) -> bool {
    matches!(error.kind.as_deref(), Some("NOT_FOUND") | Some("FORBIDDEN"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_required_scopes() {
        let msg = "Your token has not been granted the required scopes to execute this query. The 'projectsV2' field requires one of the following scopes: ['read:project'], but your token has only been granted the: ['repo'] scopes.";
        assert_eq!(required_scopes(msg), vec!["read:project".to_string()]);
        assert!(required_scopes("something else").is_empty());
    }

    #[test]
    fn scope_error_message_names_the_fix() {
        let e = GithubError::InsufficientScopes {
            missing: vec!["read:project".into()],
        };
        assert!(e.to_string().contains("gh auth refresh -s project"));
    }

    #[test]
    fn only_not_found_and_forbidden_are_tolerable() {
        let e = |k: &str| GraphqlErrorEntry {
            message: "m".into(),
            kind: Some(k.into()),
        };
        assert!(is_tolerable(&e("NOT_FOUND")));
        assert!(is_tolerable(&e("FORBIDDEN")));
        assert!(!is_tolerable(&e("INTERNAL")));
    }
}
