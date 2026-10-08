//! Wraps a transport and writes every successful response to disk, for PB_RECORD=1 runs.

use crate::github::GithubError;
use crate::github::transport::{BoxFuture, GraphqlRequest, GraphqlResponse, Transport};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct RecordingTransport {
    inner: Arc<dyn Transport>,
    dir: PathBuf,
    counts: Mutex<HashMap<String, usize>>,
}

impl RecordingTransport {
    pub fn new(inner: Arc<dyn Transport>, dir: PathBuf) -> Self {
        Self {
            inner,
            dir,
            counts: Mutex::new(HashMap::new()),
        }
    }
}

impl Transport for RecordingTransport {
    fn execute(
        &self,
        request: GraphqlRequest,
    ) -> BoxFuture<'_, Result<GraphqlResponse, GithubError>> {
        Box::pin(async move {
            let operation = request.operation_name.clone();
            let response = self.inner.execute(request).await?;
            let n = {
                let mut counts = self.counts.lock().unwrap();
                let n = counts.entry(operation.clone()).or_insert(0);
                *n += 1;
                *n
            };
            std::fs::create_dir_all(&self.dir).map_err(|e| GithubError::Network(e.to_string()))?;
            let body = serde_json::json!({ "data": response.data });
            let path = self.dir.join(format!("{operation}__{n}.json"));
            std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap())
                .map_err(|e| GithubError::Network(e.to_string()))?;
            Ok(response)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fixture::FixtureTransport;
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
}
