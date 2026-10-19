//! Wraps a transport and writes every successful response to disk, for PB_RECORD=1 runs.

use crate::error::BackendError;
use crate::transport::{BoxFuture, GraphqlRequest, GraphqlResponse, Transport};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Runs on the `{"data": ...}` body before it is written; an `Err` fails the request.
pub type Filter = Box<dyn Fn(&mut serde_json::Value) -> Result<(), String> + Send + Sync>;

pub struct RecordingTransport {
    inner: Arc<dyn Transport>,
    dir: PathBuf,
    counts: Mutex<HashMap<String, usize>>,
    filter: Option<Filter>,
}

impl RecordingTransport {
    pub fn new(inner: Arc<dyn Transport>, dir: PathBuf) -> Self {
        Self {
            inner,
            dir,
            counts: Mutex::new(HashMap::new()),
            filter: None,
        }
    }

    /// Like `new`, but `filter` rewrites each body before it is written. A filter error
    /// fails the request with `BackendError::Decode` and nothing is written.
    pub fn with_filter(inner: Arc<dyn Transport>, dir: PathBuf, filter: Filter) -> Self {
        Self {
            filter: Some(filter),
            ..Self::new(inner, dir)
        }
    }
}

impl Transport for RecordingTransport {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, BackendError>> {
        Box::pin(async move {
            let operation = request.operation_name.clone();
            let response = self.inner.execute(request).await?;
            let n = {
                let mut counts = self.counts.lock().unwrap();
                let n = counts.entry(operation.clone()).or_insert(0);
                *n += 1;
                *n
            };
            let mut body = serde_json::json!({ "data": response.data });
            if let Some(filter) = &self.filter {
                filter(&mut body).map_err(BackendError::Decode)?;
            }
            std::fs::create_dir_all(&self.dir).map_err(|e| BackendError::Network(e.to_string()))?;
            let path = self.dir.join(format!("{operation}__{n}.json"));
            std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap())
                .map_err(|e| BackendError::Network(e.to_string()))?;
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::fixture::FixtureTransport;
    use serde_json::json;

    #[tokio::test]
    async fn writes_numbered_files_per_operation() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(FixtureTransport::new());
        inner.push("Viewer", json!({"viewer": {"login": "a"}}));
        inner.push("Viewer", json!({"viewer": {"login": "b"}}));
        let t = RecordingTransport::new(inner, dir.path().to_path_buf());
        for _ in 0..2 {
            t.execute(GraphqlRequest::raw("Viewer", "q", json!({})))
                .await
                .unwrap();
        }
        let second = std::fs::read_to_string(dir.path().join("Viewer__2.json")).unwrap();
        assert!(second.contains("\"b\""));
    }

    #[tokio::test]
    async fn a_failing_filter_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(FixtureTransport::new());
        inner.push("Viewer", json!({"viewer": {"login": "a"}}));
        let t = RecordingTransport::with_filter(
            inner,
            dir.path().join("out"),
            Box::new(|_| Err("refused".to_string())),
        );
        let err = t
            .execute(GraphqlRequest::raw("Viewer", "q", json!({})))
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::Decode(m) if m == "refused"));
        assert!(!dir.path().join("out").exists());
    }

    #[tokio::test]
    async fn a_filter_rewrites_what_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(FixtureTransport::new());
        inner.push("Viewer", json!({"viewer": {"login": "a"}}));
        let t = RecordingTransport::with_filter(
            inner,
            dir.path().to_path_buf(),
            Box::new(|v| {
                v["data"]["viewer"]["login"] = json!("x");
                Ok(())
            }),
        );
        t.execute(GraphqlRequest::raw("Viewer", "q", json!({})))
            .await
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join("Viewer__1.json")).unwrap();
        assert!(text.contains("\"x\"") && !text.contains("\"a\""));
    }
}
