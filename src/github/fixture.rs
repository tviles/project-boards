//! A Transport that replays queued responses. Used by every offline test.

use crate::github::GithubError;
use crate::github::transport::{
    BoxFuture, GraphqlRequest, GraphqlResponse, RateInfo, Transport, interpret_body,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;

type Queue = VecDeque<Result<serde_json::Value, GithubError>>;

#[derive(Default)]
pub struct FixtureTransport {
    queues: Mutex<HashMap<String, Queue>>,
    log: Mutex<Vec<GraphqlRequest>>,
    rate: Mutex<RateInfo>,
}

impl FixtureTransport {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues `data` as the next response to `operation`.
    pub fn push(&self, operation: &str, data: serde_json::Value) {
        self.queue(operation, Ok(data));
    }

    pub fn push_error(&self, operation: &str, error: GithubError) {
        self.queue(operation, Err(error));
    }

    /// Queues a recorded file: a full GraphQL body (`{"data": ...}`) or bare data.
    pub fn push_file(&self, operation: &str, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("fixture {}: {e}", path.display()));
        let value: serde_json::Value = serde_json::from_str(&text).expect("fixture is JSON");
        let result = if value.get("data").is_some() || value.get("errors").is_some() {
            interpret_body(value)
        } else {
            Ok(value)
        };
        self.queue(operation, result);
    }

    pub fn set_rate(&self, rate: RateInfo) {
        *self.rate.lock().unwrap() = rate;
    }

    /// Every request executed so far, in order.
    pub fn requests(&self) -> Vec<GraphqlRequest> {
        self.log.lock().unwrap().clone()
    }

    fn queue(&self, operation: &str, result: Result<serde_json::Value, GithubError>) {
        self.queues
            .lock()
            .unwrap()
            .entry(operation.to_string())
            .or_default()
            .push_back(result);
    }
}

impl Transport for FixtureTransport {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, GithubError>> {
        let operation = request.operation_name.clone();
        self.log.lock().unwrap().push(request);
        let next = self
            .queues
            .lock()
            .unwrap()
            .get_mut(&operation)
            .and_then(|q| q.pop_front());
        let rate = self.rate.lock().unwrap().clone();
        Box::pin(async move {
            match next {
                Some(Ok(data)) => Ok(GraphqlResponse {
                    data,
                    rate,
                    scopes: None,
                }),
                Some(Err(e)) => Err(e),
                None => Err(GithubError::Network(format!(
                    "no fixture queued for {operation}"
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn replays_in_order_then_reports_missing() {
        let t = FixtureTransport::new();
        t.push("Viewer", json!({"n": 1}));
        t.push("Viewer", json!({"n": 2}));
        let req = || GraphqlRequest::raw("Viewer", "q", json!({}));
        assert_eq!(t.execute(req()).await.unwrap().data["n"], 1);
        assert_eq!(t.execute(req()).await.unwrap().data["n"], 2);
        assert!(matches!(
            t.execute(req()).await,
            Err(GithubError::Network(_))
        ));
        assert_eq!(t.requests().len(), 3);
    }

    #[tokio::test]
    async fn push_file_accepts_full_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.json");
        std::fs::write(&file, r#"{"data":{"ok":true}}"#).unwrap();
        let t = FixtureTransport::new();
        t.push_file("Op", &file);
        let r = t
            .execute(GraphqlRequest::raw("Op", "q", json!({})))
            .await
            .unwrap();
        assert_eq!(r.data["ok"], true);
    }
}
