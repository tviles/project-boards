use crate::github::error::{GithubError, GraphqlErrorEntry, is_tolerable, required_scopes};
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphqlRequest {
    #[serde(rename = "operationName")]
    pub operation_name: String,
    pub query: String,
    pub variables: serde_json::Value,
}

impl GraphqlRequest {
    /// Builds a request from a `graphql_client` query body.
    pub fn from_body<V: Serialize>(body: graphql_client::QueryBody<V>) -> Self {
        Self {
            operation_name: body.operation_name.to_string(),
            query: body.query.to_string(),
            variables: serde_json::to_value(body.variables).unwrap_or(serde_json::Value::Null),
        }
    }

    /// Builds a request from a hand-written query (probes and tests).
    pub fn raw(operation_name: &str, query: &str, variables: serde_json::Value) -> Self {
        Self {
            operation_name: operation_name.into(),
            query: query.into(),
            variables,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RateInfo {
    pub remaining: Option<u32>,
    pub limit: Option<u32>,
    pub reset_epoch: Option<u64>,
}

impl RateInfo {
    /// True when less than 10% of the primary budget remains.
    pub fn is_low(&self) -> bool {
        match (self.remaining, self.limit) {
            (Some(remaining), Some(limit)) if limit > 0 => {
                u64::from(remaining) * 10 < u64::from(limit)
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphqlResponse {
    pub data: serde_json::Value,
    pub rate: RateInfo,
    /// `X-OAuth-Scopes`, present for classic and OAuth tokens only.
    pub scopes: Option<Vec<String>>,
}

pub trait Transport: Send + Sync {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, GithubError>>;
}

/// Turns a GraphQL response body into its data, or the error it describes.
/// Data with only NOT_FOUND/FORBIDDEN errors is kept; those mark nodes the viewer cannot see.
pub fn interpret_body(body: serde_json::Value) -> Result<serde_json::Value, GithubError> {
    let errors: Vec<GraphqlErrorEntry> = match body.get("errors") {
        Some(e) => {
            serde_json::from_value(e.clone()).map_err(|e| GithubError::Decode(e.to_string()))?
        }
        None => Vec::new(),
    };
    if let Some(e) = errors
        .iter()
        .find(|e| e.kind.as_deref() == Some("INSUFFICIENT_SCOPES"))
    {
        return Err(GithubError::InsufficientScopes {
            missing: required_scopes(&e.message),
        });
    }
    let data = body.get("data").cloned().unwrap_or(serde_json::Value::Null);
    if !errors.is_empty() && (data.is_null() || !errors.iter().all(is_tolerable)) {
        return Err(GithubError::Graphql(errors));
    }
    if !errors.is_empty() {
        tracing::debug!(
            count = errors.len(),
            "kept data with tolerable GraphQL errors"
        );
    }
    Ok(data)
}

pub struct HttpTransport {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl HttpTransport {
    pub const GITHUB_ENDPOINT: &'static str = "https://api.github.com/graphql";

    pub fn new(token: String) -> Self {
        Self::with_endpoint(token, Self::GITHUB_ENDPOINT.to_string())
    }

    pub fn with_endpoint(token: String, endpoint: String) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("project-boards/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client builds");
        Self {
            client,
            endpoint,
            token,
        }
    }
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Seconds until the primary budget resets: `x-ratelimit-reset` minus `now`, or a minute when
/// GitHub sent no reset time.
fn primary_wait(rate: &RateInfo, now: u64) -> u64 {
    rate.reset_epoch
        .map(|reset| reset.saturating_sub(now))
        .unwrap_or(60)
}

impl Transport for HttpTransport {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, GithubError>> {
        Box::pin(async move {
            let operation = request.operation_name.clone();
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .json(&request)
                .send()
                .await
                .map_err(|e| GithubError::Network(e.without_url().to_string()))?;
            let status = response.status();
            let headers = response.headers().clone();
            let rate = RateInfo {
                remaining: header_u64(&headers, "x-ratelimit-remaining").map(|v| v as u32),
                limit: header_u64(&headers, "x-ratelimit-limit").map(|v| v as u32),
                reset_epoch: header_u64(&headers, "x-ratelimit-reset"),
            };
            let scopes = headers
                .get("x-oauth-scopes")
                .and_then(|v| v.to_str().ok())
                .map(|s| {
                    s.split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect()
                });
            tracing::debug!(%operation, status = status.as_u16(), "graphql request");

            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(GithubError::Unauthorized);
            }
            // A secondary limit says how long to wait.
            if (status == reqwest::StatusCode::FORBIDDEN
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
                && let Some(secs) = header_u64(&headers, "retry-after")
            {
                return Err(GithubError::RateLimited {
                    retry_after_secs: secs,
                });
            }
            let primary_limited = || GithubError::RateLimited {
                retry_after_secs: primary_wait(
                    &rate,
                    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64,
                ),
            };
            // An exhausted primary budget, whatever the status: the next request would fail.
            if rate.remaining == Some(0) {
                return Err(primary_limited());
            }
            let body: serde_json::Value = response
                .json()
                .await
                .map_err(|e| GithubError::Decode(e.to_string()))?;
            // GraphQL reports an exhausted primary budget as HTTP 200 with a RATE_LIMITED error.
            if body["errors"]
                .as_array()
                .is_some_and(|errors| errors.iter().any(|e| e["type"] == "RATE_LIMITED"))
            {
                return Err(primary_limited());
            }
            if !status.is_success() && body.get("data").is_none() && body.get("errors").is_none() {
                return Err(GithubError::Network(format!("HTTP {status}")));
            }
            Ok(GraphqlResponse {
                data: interpret_body(body)?,
                rate,
                scopes,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn server_with(template: ResponseTemplate) -> (MockServer, HttpTransport) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("authorization", "Bearer t0k"))
            .respond_with(template)
            .mount(&server)
            .await;
        let transport =
            HttpTransport::with_endpoint("t0k".into(), format!("{}/graphql", server.uri()));
        (server, transport)
    }

    fn request() -> GraphqlRequest {
        GraphqlRequest::raw("Viewer", "query Viewer { viewer { login } }", json!({}))
    }

    #[tokio::test]
    async fn success_reads_data_rate_and_scopes() {
        let (_s, t) = server_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {"viewer": {"login": "tviles"}}}))
                .insert_header("x-ratelimit-remaining", "4999")
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-oauth-scopes", "repo, project"),
        )
        .await;
        let r = t.execute(request()).await.unwrap();
        assert_eq!(r.data["viewer"]["login"], "tviles");
        assert_eq!(r.rate.remaining, Some(4999));
        assert_eq!(r.rate.limit, Some(5000));
        assert_eq!(
            r.scopes,
            Some(vec!["repo".to_string(), "project".to_string()])
        );
    }

    #[tokio::test]
    async fn unauthorized() {
        let (_s, t) = server_with(
            ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})),
        )
        .await;
        assert_eq!(t.execute(request()).await, Err(GithubError::Unauthorized));
    }

    #[tokio::test]
    async fn secondary_rate_limit_uses_retry_after() {
        let (_s, t) = server_with(
            ResponseTemplate::new(403)
                .insert_header("retry-after", "30")
                .set_body_json(json!({"message": "slow down"})),
        )
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(GithubError::RateLimited {
                retry_after_secs: 30
            })
        );
    }

    #[tokio::test]
    async fn insufficient_scopes() {
        let body = json!({"errors": [{"type": "INSUFFICIENT_SCOPES", "message": "The 'projectsV2' field requires one of the following scopes: ['read:project'], but your token has only been granted the: ['repo'] scopes."}]});
        let (_s, t) = server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        assert_eq!(
            t.execute(request()).await,
            Err(GithubError::InsufficientScopes {
                missing: vec!["read:project".into()]
            })
        );
    }

    #[tokio::test]
    async fn partial_data_with_not_found_is_kept() {
        let body = json!({"data": {"user": null, "organization": {"login": "acme"}},
                          "errors": [{"type": "NOT_FOUND", "message": "Could not resolve to a User"}]});
        let (_s, t) = server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        let r = t.execute(request()).await.unwrap();
        assert_eq!(r.data["organization"]["login"], "acme");
    }

    #[tokio::test]
    async fn errors_without_data_fail() {
        let body = json!({"errors": [{"type": "INTERNAL", "message": "boom"}]});
        let (_s, t) = server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        assert!(matches!(
            t.execute(request()).await,
            Err(GithubError::Graphql(_))
        ));
    }

    fn now() -> u64 {
        time::OffsetDateTime::now_utc().unix_timestamp() as u64
    }

    fn assert_wait_near(result: Result<GraphqlResponse, GithubError>, expected: u64) {
        match result {
            Err(GithubError::RateLimited { retry_after_secs }) => assert!(
                (expected.saturating_sub(2)..=expected).contains(&retry_after_secs),
                "waited {retry_after_secs}s, expected about {expected}s"
            ),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn graphql_rate_limited_error_waits_until_the_reset() {
        let body = json!({"errors": [{"type": "RATE_LIMITED", "message": "API rate limit exceeded for user ID 1."}]});
        let (_s, t) = server_with(
            ResponseTemplate::new(200)
                .set_body_json(body)
                .insert_header("x-ratelimit-reset", (now() + 300).to_string().as_str()),
        )
        .await;
        assert_wait_near(t.execute(request()).await, 300);
    }

    #[tokio::test]
    async fn graphql_rate_limited_error_without_a_reset_waits_a_minute() {
        let body = json!({"data": null, "errors": [{"type": "RATE_LIMITED", "message": "API rate limit exceeded"}]});
        let (_s, t) = server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        assert_eq!(
            t.execute(request()).await,
            Err(GithubError::RateLimited {
                retry_after_secs: 60
            })
        );
    }

    #[tokio::test]
    async fn an_exhausted_budget_is_rate_limited_whatever_the_status() {
        for status in [200, 403, 502] {
            let (_s, t) = server_with(
                ResponseTemplate::new(status)
                    .set_body_json(json!({"data": {"viewer": {"login": "tviles"}}}))
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", (now() + 90).to_string().as_str()),
            )
            .await;
            assert_wait_near(t.execute(request()).await, 90);
        }
    }

    #[test]
    fn primary_wait_counts_to_the_reset_or_defaults_to_a_minute() {
        let rate = |reset| RateInfo {
            remaining: Some(0),
            limit: Some(5000),
            reset_epoch: reset,
        };
        assert_eq!(primary_wait(&rate(Some(1_000_120)), 1_000_000), 120);
        assert_eq!(primary_wait(&rate(Some(999_000)), 1_000_000), 0);
        assert_eq!(primary_wait(&rate(None), 1_000_000), 60);
    }

    #[test]
    fn rate_is_low_below_ten_percent() {
        let r = |rem, lim| RateInfo {
            remaining: Some(rem),
            limit: Some(lim),
            reset_epoch: None,
        };
        assert!(r(400, 5000).is_low());
        assert!(!r(600, 5000).is_low());
        assert!(!RateInfo::default().is_low());
    }
}
