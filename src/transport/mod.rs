//! The GraphQL transport shared by the GitHub and Linear clients.

pub mod fixture;
pub mod record;

use crate::error::{BackendError, GraphqlErrorEntry};
use crate::github::error::{is_tolerable, required_scopes};
use crate::model::Provider;
use serde::Serialize;
use std::future::Future;
use std::pin::Pin;

/// One page of a connection.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub nodes: Vec<T>,
    pub total: usize,
    /// Cursor for the next page, `None` on the last page.
    pub next: Option<String>,
}

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
    /// Errors GitHub sent alongside usable data (NOT_FOUND, FORBIDDEN). A mutation whose
    /// payload is null is explained by these.
    pub errors: Vec<GraphqlErrorEntry>,
}

pub trait Transport: Send + Sync {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, BackendError>>;
}

/// Turns a GraphQL response body into its data, or the error it describes.
/// Data with only NOT_FOUND/FORBIDDEN errors is kept; those mark nodes the viewer cannot see.
pub fn interpret_body(body: serde_json::Value) -> Result<serde_json::Value, BackendError> {
    interpret_body_keeping_errors(body).map(|(data, _)| data)
}

/// `interpret_body`, also returning the tolerated errors that came with the data.
pub fn interpret_body_keeping_errors(
    body: serde_json::Value,
) -> Result<(serde_json::Value, Vec<GraphqlErrorEntry>), BackendError> {
    let errors: Vec<GraphqlErrorEntry> = match body.get("errors") {
        Some(e) => {
            serde_json::from_value(e.clone()).map_err(|e| BackendError::Decode(e.to_string()))?
        }
        None => Vec::new(),
    };
    if let Some(e) = errors
        .iter()
        .find(|e| e.kind.as_deref() == Some("INSUFFICIENT_SCOPES"))
    {
        return Err(BackendError::MissingPermission {
            provider: Provider::Github,
            needed: required_scopes(&e.message),
        });
    }
    let data = body.get("data").cloned().unwrap_or(serde_json::Value::Null);
    if !errors.is_empty() && (data.is_null() || !errors.iter().all(is_tolerable)) {
        return Err(BackendError::Api(errors));
    }
    if !errors.is_empty() {
        tracing::debug!(
            count = errors.len(),
            "kept data with tolerable GraphQL errors"
        );
    }
    Ok((data, errors))
}

/// Which service's headers and error vocabulary an `HttpTransport` speaks.
#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    Github,
    Linear,
}

pub struct HttpTransport {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    dialect: Dialect,
}

impl HttpTransport {
    pub const GITHUB_ENDPOINT: &'static str = "https://api.github.com/graphql";
    pub const LINEAR_ENDPOINT: &'static str = "https://api.linear.app/graphql";

    pub fn new(token: String) -> Self {
        Self::with_endpoint(token, Self::GITHUB_ENDPOINT.to_string())
    }

    pub fn with_endpoint(token: String, endpoint: String) -> Self {
        Self::build(token, endpoint, Dialect::Github)
    }

    /// A transport for Linear's API with a personal API key.
    pub fn linear(key: String) -> Self {
        Self::linear_with_endpoint(key, Self::LINEAR_ENDPOINT.to_string())
    }

    pub fn linear_with_endpoint(key: String, endpoint: String) -> Self {
        Self::build(key, endpoint, Dialect::Linear)
    }

    fn build(token: String, endpoint: String, dialect: Dialect) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("project-boards/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client builds");
        Self {
            client,
            endpoint,
            token,
            dialect,
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

/// How long to wait after a secondary rate limit that came without `retry-after`.
const SECONDARY_WAIT_SECS: u64 = 60;

/// A 403 or 429 whose body names GitHub's secondary rate limit.
fn is_secondary_limit(status: reqwest::StatusCode, body: &str) -> bool {
    (status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
        && body.to_ascii_lowercase().contains("secondary rate limit")
}

/// Whether a decoded body carries usable data (a `data` key that is not `null`).
fn has_data(body: Option<&serde_json::Value>) -> bool {
    body.and_then(|b| b.get("data"))
        .is_some_and(|d| !d.is_null())
}

impl Transport for HttpTransport {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, BackendError>> {
        if self.dialect == Dialect::Linear {
            return Box::pin(self.execute_linear(request));
        }
        Box::pin(async move {
            let operation = request.operation_name.clone();
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .json(&request)
                .send()
                .await
                .map_err(|e| BackendError::Network(e.without_url().to_string()))?;
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
                return Err(BackendError::Unauthorized {
                    provider: Provider::Github,
                });
            }
            // A secondary limit says how long to wait.
            if (status == reqwest::StatusCode::FORBIDDEN
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
                && let Some(secs) = header_u64(&headers, "retry-after")
            {
                return Err(BackendError::RateLimited {
                    retry_after_secs: secs,
                });
            }
            let primary_limited = || BackendError::RateLimited {
                retry_after_secs: primary_wait(
                    &rate,
                    time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64,
                ),
            };
            let text = response
                .text()
                .await
                .map_err(|e| BackendError::Decode(e.without_url().to_string()))?;
            if is_secondary_limit(status, &text) {
                // No retry-after: GitHub's docs say to wait at least a minute.
                return Err(BackendError::RateLimited {
                    retry_after_secs: SECONDARY_WAIT_SECS,
                });
            }
            let body = serde_json::from_str::<serde_json::Value>(&text);
            // An exhausted budget fails a request only when GitHub sent no data: a 200 with
            // data at remaining 0 was applied, and re-sending it would apply an edit twice.
            if rate.remaining == Some(0) && !has_data(body.as_ref().ok()) {
                return Err(primary_limited());
            }
            let body = body.map_err(|e| BackendError::Decode(e.to_string()))?;
            // GraphQL reports an exhausted primary budget as HTTP 200 with a RATE_LIMITED error.
            if body["errors"]
                .as_array()
                .is_some_and(|errors| errors.iter().any(|e| e["type"] == "RATE_LIMITED"))
            {
                return Err(primary_limited());
            }
            if !status.is_success() && body.get("data").is_none() && body.get("errors").is_none() {
                return Err(BackendError::Network(format!("HTTP {status}")));
            }
            let (data, errors) = interpret_body_keeping_errors(body)?;
            Ok(GraphqlResponse {
                data,
                rate,
                scopes,
                errors,
            })
        })
    }
}

/// One of Linear's two budgets, as its headers report it.
struct LinearBudget {
    remaining: Option<u64>,
    limit: Option<u64>,
    /// Epoch seconds (Linear sends milliseconds).
    reset: Option<u64>,
}

impl LinearBudget {
    fn read(headers: &reqwest::header::HeaderMap, kind: &str) -> Self {
        let get = |what: &str| header_u64(headers, &format!("x-ratelimit-{kind}-{what}"));
        Self {
            remaining: get("remaining"),
            limit: get("limit"),
            reset: get("reset").map(|ms| ms / 1000),
        }
    }

    /// Remaining over limit, scaled by 1e6 so budgets of different sizes compare.
    fn fraction(&self) -> Option<u64> {
        match (self.remaining, self.limit) {
            (Some(remaining), Some(limit)) if limit > 0 => {
                Some(remaining.saturating_mul(1_000_000) / limit)
            }
            _ => None,
        }
    }
}

/// The tighter of Linear's request and complexity budgets, and the wait until the later reset.
fn linear_rate(headers: &reqwest::header::HeaderMap) -> (RateInfo, Option<u64>) {
    let budgets = [
        LinearBudget::read(headers, "requests"),
        LinearBudget::read(headers, "complexity"),
    ];
    let tighter = budgets
        .iter()
        .filter_map(|b| b.fraction().map(|f| (f, b)))
        .min_by_key(|(f, _)| *f)
        .map(|(_, b)| b);
    let rate = RateInfo {
        remaining: tighter.and_then(|b| b.remaining).map(|v| v as u32),
        limit: tighter.and_then(|b| b.limit).map(|v| v as u32),
        reset_epoch: tighter.and_then(|b| b.reset),
    };
    let later_reset = budgets.iter().filter_map(|b| b.reset).max();
    (rate, later_reset)
}

impl HttpTransport {
    /// Linear's dialect: the key goes in `Authorization` as is, errors carry
    /// `extensions.code`, and nothing is tolerated alongside data.
    async fn execute_linear(
        &self,
        request: GraphqlRequest,
    ) -> Result<GraphqlResponse, BackendError> {
        let operation = request.operation_name.clone();
        let mut key = reqwest::header::HeaderValue::from_str(&self.token).map_err(|_| {
            BackendError::Unauthorized {
                provider: Provider::Linear,
            }
        })?;
        key.set_sensitive(true);
        let response = self
            .client
            .post(&self.endpoint)
            .header(reqwest::header::AUTHORIZATION, key)
            .json(&request)
            .send()
            .await
            .map_err(|e| BackendError::Network(e.without_url().to_string()))?;
        let status = response.status();
        let (rate, later_reset) = linear_rate(response.headers());
        tracing::debug!(%operation, status = status.as_u16(), "graphql request");
        let unauthorized = || BackendError::Unauthorized {
            provider: Provider::Linear,
        };
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(unauthorized());
        }
        let text = response
            .text()
            .await
            .map_err(|e| BackendError::Decode(e.without_url().to_string()))?;
        let body = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(body) => body,
            Err(_) if !status.is_success() => {
                return Err(BackendError::Network(format!("HTTP {status}")));
            }
            Err(e) => return Err(BackendError::Decode(e.to_string())),
        };
        let errors: Vec<GraphqlErrorEntry> = body
            .get("errors")
            .and_then(|e| e.as_array())
            .map(|entries| {
                entries
                    .iter()
                    .map(|e| GraphqlErrorEntry {
                        message: e["message"].as_str().unwrap_or_default().to_string(),
                        kind: e["extensions"]["code"].as_str().map(str::to_string),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let has_code = |code: &str| errors.iter().any(|e| e.kind.as_deref() == Some(code));
        if has_code("AUTHENTICATION_ERROR") {
            return Err(unauthorized());
        }
        if has_code("RATELIMITED") {
            let now = time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64;
            return Err(BackendError::RateLimited {
                retry_after_secs: later_reset
                    .map(|reset| reset.saturating_sub(now))
                    .unwrap_or(60),
            });
        }
        if has_code("FORBIDDEN") {
            return Err(BackendError::MissingPermission {
                provider: Provider::Linear,
                needed: vec![],
            });
        }
        if !errors.is_empty() {
            return Err(BackendError::Api(errors));
        }
        if !status.is_success() && body.get("data").is_none() {
            return Err(BackendError::Network(format!("HTTP {status}")));
        }
        let data = body.get("data").cloned().unwrap_or(serde_json::Value::Null);
        Ok(GraphqlResponse {
            data,
            rate,
            scopes: None,
            errors: Vec::new(),
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
    async fn an_empty_scopes_header_means_no_scopes_and_an_absent_one_means_unknown() {
        let body = json!({"data": {"viewer": {"login": "tviles"}}});
        let (_s, t) = server_with(
            ResponseTemplate::new(200)
                .set_body_json(body.clone())
                .insert_header("x-oauth-scopes", ""),
        )
        .await;
        assert_eq!(t.execute(request()).await.unwrap().scopes, Some(vec![]));
        let (_s, t) = server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        assert_eq!(t.execute(request()).await.unwrap().scopes, None);
    }

    #[tokio::test]
    async fn unauthorized() {
        let (_s, t) = server_with(
            ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})),
        )
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(BackendError::Unauthorized {
                provider: Provider::Github
            })
        );
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
            Err(BackendError::RateLimited {
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
            Err(BackendError::MissingPermission {
                provider: Provider::Github,
                needed: vec!["read:project".into()]
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
            Err(BackendError::Api(_))
        ));
    }

    fn now() -> u64 {
        time::OffsetDateTime::now_utc().unix_timestamp() as u64
    }

    fn assert_wait_near(result: Result<GraphqlResponse, BackendError>, expected: u64) {
        match result {
            Err(BackendError::RateLimited { retry_after_secs }) => assert!(
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
            Err(BackendError::RateLimited {
                retry_after_secs: 60
            })
        );
    }

    #[tokio::test]
    async fn an_exhausted_budget_without_data_is_rate_limited_whatever_the_status() {
        let bodies = [
            json!({"message": "API rate limit exceeded"}),
            json!({"data": null}),
        ];
        for status in [200, 403, 502] {
            for body in &bodies {
                let (_s, t) = server_with(
                    ResponseTemplate::new(status)
                        .set_body_json(body.clone())
                        .insert_header("x-ratelimit-remaining", "0")
                        .insert_header("x-ratelimit-reset", (now() + 90).to_string().as_str()),
                )
                .await;
                assert_wait_near(t.execute(request()).await, 90);
            }
        }
        let (_s, t) = server_with(
            ResponseTemplate::new(403)
                .set_body_string("<html>rate limited</html>")
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", (now() + 90).to_string().as_str()),
        )
        .await;
        assert_wait_near(t.execute(request()).await, 90);
    }

    /// The last request of the budget still succeeds: an applied edit must not be re-sent.
    #[tokio::test]
    async fn data_at_an_exhausted_budget_is_a_success_with_the_rate_kept() {
        let (_s, t) = server_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {"viewer": {"login": "tviles"}}}))
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-limit", "5000")
                .insert_header("x-ratelimit-reset", (now() + 90).to_string().as_str()),
        )
        .await;
        let r = t.execute(request()).await.unwrap();
        assert_eq!(r.data["viewer"]["login"], "tviles");
        assert_eq!(
            r.rate.remaining,
            Some(0),
            "the scheduler still sees the budget"
        );
    }

    #[tokio::test]
    async fn data_with_a_rate_limited_error_is_still_rate_limited() {
        let body = json!({"data": {"viewer": null},
                          "errors": [{"type": "RATE_LIMITED", "message": "API rate limit exceeded"}]});
        let (_s, t) = server_with(
            ResponseTemplate::new(200)
                .set_body_json(body)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", (now() + 90).to_string().as_str()),
        )
        .await;
        assert_wait_near(t.execute(request()).await, 90);
    }

    #[tokio::test]
    async fn a_secondary_limit_without_retry_after_waits_a_minute() {
        for status in [403, 429] {
            let (_s, t) = server_with(ResponseTemplate::new(status).set_body_json(json!({
                "message": "You have exceeded a secondary rate limit. Please wait a few minutes before you try again."
            })))
            .await;
            assert_eq!(
                t.execute(request()).await,
                Err(BackendError::RateLimited {
                    retry_after_secs: 60
                }),
                "HTTP {status}"
            );
        }
    }

    #[tokio::test]
    async fn a_plain_forbidden_without_retry_after_is_not_rate_limited() {
        let (_s, t) = server_with(
            ResponseTemplate::new(403).set_body_json(json!({"message": "Resource not accessible"})),
        )
        .await;
        assert!(!matches!(
            t.execute(request()).await,
            Err(BackendError::RateLimited { .. })
        ));
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

    #[tokio::test]
    async fn tolerated_errors_are_kept_with_the_data() {
        let body = json!({"data": {"addLabelsToLabelable": null},
                          "errors": [{"type": "FORBIDDEN", "message": "Resource not accessible by integration"}]});
        let (_s, t) = server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        let r = t.execute(request()).await.unwrap();
        assert!(r.data["addLabelsToLabelable"].is_null());
        assert_eq!(r.errors.len(), 1);
        assert_eq!(
            r.errors[0].message,
            "Resource not accessible by integration"
        );
    }

    #[test]
    fn interpret_body_still_drops_tolerated_errors() {
        let body = json!({"data": {"x": 1}, "errors": [{"type": "NOT_FOUND", "message": "gone"}]});
        assert_eq!(interpret_body(body.clone()).unwrap()["x"], 1);
        let (data, errors) = interpret_body_keeping_errors(body).unwrap();
        assert_eq!((data["x"].as_i64(), errors.len()), (Some(1), 1));
    }

    async fn linear_server_with(template: ResponseTemplate) -> (MockServer, HttpTransport) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(header("authorization", "lin_api_k"))
            .respond_with(template)
            .mount(&server)
            .await;
        let transport = HttpTransport::linear_with_endpoint(
            "lin_api_k".into(),
            format!("{}/graphql", server.uri()),
        );
        (server, transport)
    }

    fn linear_error(code: &str, message: &str) -> serde_json::Value {
        json!({"data": null, "errors": [{"message": message, "extensions": {"code": code}}]})
    }

    #[tokio::test]
    async fn linear_sends_the_key_without_bearer_and_reads_data() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(200).set_body_json(json!({"data": {"viewer": {"id": "u1"}}})),
        )
        .await;
        let r = t.execute(request()).await.unwrap();
        assert_eq!(r.data["viewer"]["id"], "u1");
        assert_eq!(r.rate, RateInfo::default());
        assert_eq!(r.scopes, None);
        assert!(r.errors.is_empty());
    }

    #[tokio::test]
    async fn linear_reports_the_tighter_budget() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": {}}))
                .insert_header("x-ratelimit-requests-remaining", "4500")
                .insert_header("x-ratelimit-requests-limit", "5000")
                .insert_header("x-ratelimit-requests-reset", "1700000100000")
                .insert_header("x-ratelimit-complexity-remaining", "100000")
                .insert_header("x-ratelimit-complexity-limit", "3000000")
                .insert_header("x-ratelimit-complexity-reset", "1700000200000"),
        )
        .await;
        let r = t.execute(request()).await.unwrap();
        assert_eq!(r.rate.remaining, Some(100000));
        assert_eq!(r.rate.limit, Some(3000000));
        assert_eq!(r.rate.reset_epoch, Some(1700000200));
        assert!(r.rate.is_low());
    }

    #[tokio::test]
    async fn linear_bad_key_is_unauthorized() {
        let (_s, t) = linear_server_with(ResponseTemplate::new(401).set_body_json(linear_error(
            "AUTHENTICATION_ERROR",
            "Authentication required, not authenticated",
        )))
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(BackendError::Unauthorized {
                provider: Provider::Linear
            })
        );
    }

    #[tokio::test]
    async fn linear_authentication_error_code_is_unauthorized_at_any_status() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(200)
                .set_body_json(linear_error("AUTHENTICATION_ERROR", "not authenticated")),
        )
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(BackendError::Unauthorized {
                provider: Provider::Linear
            })
        );
    }

    #[tokio::test]
    async fn linear_ratelimited_waits_until_the_reset() {
        let reset_ms = (now() + 120) * 1000;
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(400)
                .set_body_json(linear_error("RATELIMITED", "Rate limit exceeded"))
                .insert_header("x-ratelimit-requests-remaining", "0")
                .insert_header("x-ratelimit-requests-limit", "5000")
                .insert_header("x-ratelimit-requests-reset", reset_ms.to_string().as_str()),
        )
        .await;
        assert_wait_near(t.execute(request()).await, 120);
    }

    #[tokio::test]
    async fn linear_ratelimited_without_resets_waits_a_minute() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(400)
                .set_body_json(linear_error("RATELIMITED", "Rate limit exceeded")),
        )
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(BackendError::RateLimited {
                retry_after_secs: 60
            })
        );
    }

    #[tokio::test]
    async fn linear_forbidden_is_missing_permission() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(200).set_body_json(linear_error("FORBIDDEN", "not allowed")),
        )
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(BackendError::MissingPermission {
                provider: Provider::Linear,
                needed: vec![]
            })
        );
    }

    #[tokio::test]
    async fn linear_entity_not_found_is_api_with_its_code() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(200)
                .set_body_json(linear_error("INPUT_ERROR", "Entity not found: Issue")),
        )
        .await;
        assert_eq!(
            t.execute(request()).await,
            Err(BackendError::Api(vec![GraphqlErrorEntry {
                message: "Entity not found: Issue".into(),
                kind: Some("INPUT_ERROR".into()),
            }]))
        );
    }

    #[tokio::test]
    async fn linear_errors_alongside_data_are_still_an_error() {
        let body = json!({"data": {"x": 1}, "errors": [{"message": "partial", "extensions": {"code": "NOT_FOUND"}}]});
        let (_s, t) = linear_server_with(ResponseTemplate::new(200).set_body_json(body)).await;
        assert!(matches!(
            t.execute(request()).await,
            Err(BackendError::Api(_))
        ));
    }

    #[tokio::test]
    async fn linear_html_502_is_network() {
        let (_s, t) = linear_server_with(
            ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>"),
        )
        .await;
        match t.execute(request()).await {
            Err(BackendError::Network(m)) => assert!(m.contains("502"), "{m}"),
            other => panic!("expected Network, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn linear_non_json_success_is_decode() {
        let (_s, t) = linear_server_with(ResponseTemplate::new(200).set_body_string("nope")).await;
        assert!(matches!(
            t.execute(request()).await,
            Err(BackendError::Decode(_))
        ));
    }
}
