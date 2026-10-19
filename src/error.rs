//! Errors from any provider, in one type the store, sync and UI share.

use crate::model::Provider;
use serde::Deserialize;

/// One entry of a GraphQL response's `errors` array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphqlErrorEntry {
    pub message: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BackendError {
    #[error("{}", no_credentials(.provider))]
    NoCredentials { provider: Provider },
    #[error("{}", unauthorized(.provider))]
    Unauthorized { provider: Provider },
    #[error("{}", missing_permission(.provider, .needed))]
    MissingPermission {
        provider: Provider,
        needed: Vec<String>,
    },
    #[error("rate limited; retry in {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("the server returned an error: {}", messages(.0))]
    Api(Vec<GraphqlErrorEntry>),
    #[error("network error: {0}")]
    Network(String),
    #[error("could not decode the response: {0}")]
    Decode(String),
}

impl BackendError {
    /// The variant's name, for logs that must not carry the server's message text.
    pub fn kind(&self) -> &'static str {
        match self {
            BackendError::NoCredentials { .. } => "NoCredentials",
            BackendError::Unauthorized { .. } => "Unauthorized",
            BackendError::MissingPermission { .. } => "MissingPermission",
            BackendError::RateLimited { .. } => "RateLimited",
            BackendError::Api(_) => "Api",
            BackendError::Network(_) => "Network",
            BackendError::Decode(_) => "Decode",
        }
    }
}

fn no_credentials(provider: &Provider) -> String {
    match provider {
        Provider::Github => "no GitHub token found: set GH_TOKEN or run `gh auth login`".into(),
        Provider::Linear => {
            "no Linear API key found: set LINEAR_API_KEY or linear_api_key_command".into()
        }
    }
}

fn unauthorized(provider: &Provider) -> String {
    match provider {
        Provider::Github => {
            "GitHub rejected the token (401 Unauthorized); run `gh auth login`".into()
        }
        Provider::Linear => {
            "Linear rejected the API key; create a new one in Linear's settings".into()
        }
    }
}

fn missing_permission(provider: &Provider, needed: &[String]) -> String {
    match provider {
        Provider::Github => format!(
            "the token is missing the {} scope; run `gh auth refresh -s project`",
            needed.join(", ")
        ),
        Provider::Linear => "Linear refused the request: the API key cannot see this board".into(),
    }
}

fn messages(errors: &[GraphqlErrorEntry]) -> String {
    errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_texts_are_unchanged() {
        let gh = Provider::Github;
        assert_eq!(
            BackendError::NoCredentials { provider: gh }.to_string(),
            "no GitHub token found: set GH_TOKEN or run `gh auth login`"
        );
        assert_eq!(
            BackendError::Unauthorized { provider: gh }.to_string(),
            "GitHub rejected the token (401 Unauthorized); run `gh auth login`"
        );
        assert_eq!(
            BackendError::MissingPermission {
                provider: gh,
                needed: vec!["read:project".into()],
            }
            .to_string(),
            "the token is missing the read:project scope; run `gh auth refresh -s project`"
        );
        assert_eq!(
            BackendError::Network("reset".into()).to_string(),
            "network error: reset"
        );
    }

    #[test]
    fn linear_texts_name_linear_and_never_suggest_gh() {
        let ln = Provider::Linear;
        for e in [
            BackendError::NoCredentials { provider: ln },
            BackendError::Unauthorized { provider: ln },
            BackendError::MissingPermission {
                provider: ln,
                needed: vec![],
            },
        ] {
            let text = e.to_string();
            assert!(text.contains("Linear"), "{text}");
            assert!(!text.contains("gh "), "{text}");
        }
    }

    #[test]
    fn neutral_variants_name_no_provider() {
        assert_eq!(
            BackendError::RateLimited {
                retry_after_secs: 30
            }
            .to_string(),
            "rate limited; retry in 30s"
        );
        let api = BackendError::Api(vec![GraphqlErrorEntry {
            message: "Field is not valid".into(),
            kind: None,
        }]);
        assert_eq!(
            api.to_string(),
            "the server returned an error: Field is not valid"
        );
        assert_eq!(
            BackendError::Decode("eof".into()).to_string(),
            "could not decode the response: eof"
        );
    }

    #[test]
    fn kinds_name_the_variant_and_never_the_message() {
        let api = BackendError::Api(vec![GraphqlErrorEntry {
            message: "Secret plan title".into(),
            kind: None,
        }]);
        assert_eq!(api.kind(), "Api");
        assert_eq!(
            BackendError::NoCredentials {
                provider: Provider::Linear
            }
            .kind(),
            "NoCredentials"
        );
        assert_eq!(BackendError::Decode("x".into()).kind(), "Decode");
    }
}
