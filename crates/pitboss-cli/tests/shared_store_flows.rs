//! End-to-end integration tests for the shared store feature.
//! Drives fake-claude subprocesses through real SessionHandle + MCP socket +
//! real mcp-bridge to exercise identity injection and the full tool stack.

use pitboss_cli::shared_store::SharedStore;
use std::sync::Arc;

#[tokio::test]
async fn scaffolding_smoke() {
    let s = Arc::new(SharedStore::new());
    s.set("/ref/bootstrap", b"ok".to_vec(), "lead")
        .await
        .unwrap();
    let e = s.get("/ref/bootstrap").await.unwrap();
    assert_eq!(e.value, b"ok");
}
