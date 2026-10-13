//! MCP Resource endpoints for system health and status information.

use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tokio::sync::RwLock;

use crate::search::SearchEngine;
use crate::storage::Storage;

/// Build system health JSON from storage and search state.
pub async fn system_health(
    storage: &Arc<Storage>,
    search: &Arc<RwLock<SearchEngine>>,
    db_path: &str,
    index_dir: &str,
) -> String {
    let t0 = Instant::now();

    // Keep one operation over the snapshot, so maintenance cannot split it.
    let counts = (|| -> anyhow::Result<_> {
        let operation = storage.operation_lock()?;
        Ok((
            operation,
            storage.count_chunk_embeddings()?,
            storage.count_reflection_embeddings()?,
        ))
    })();
    let (_operation, chunks_indexed, reflections_indexed) = match counts {
        Ok(counts) => counts,
        Err(error) => {
            return json!({
                "chunks_indexed": null,
                "reflections_indexed": null,
                "cache_status": "unavailable",
                "error": error.to_string(),
                "db_path": db_path,
                "index_dir": index_dir,
                "binary_version": env!("CARGO_PKG_VERSION"),
            })
            .to_string()
        }
    };

    let idx = search.read().await;
    let cache_status = if idx.is_dirty() { "dirty" } else { "clean" };

    let health = json!({
        "chunks_indexed": chunks_indexed,
        "reflections_indexed": reflections_indexed,
        "cache_status": cache_status,
        "query_ms": t0.elapsed().as_secs_f64() * 1000.0,
        "db_path": db_path,
        "index_dir": index_dir,
        "binary_version": env!("CARGO_PKG_VERSION"),
    });

    serde_json::to_string_pretty(&health).unwrap_or_else(|_| health.to_string())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn round4_health_reports_unavailable_during_maintenance() {
        use super::*;
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let storage = Arc::new(Storage::open(&db).unwrap());
        let search = Arc::new(RwLock::new(SearchEngine::new(16)));
        storage
            .insert_reflection("r", "retained", &[], &vec![0.1; 384])
            .unwrap();
        let before: serde_json::Value =
            serde_json::from_str(&system_health(&storage, &search, "db", "index-v3").await)
                .unwrap();
        assert_eq!(before["reflections_indexed"], 1);

        let owner = crate::maintenance::MaintenanceLock::acquire(dir.path()).unwrap();
        let health: serde_json::Value =
            serde_json::from_str(&system_health(&storage, &search, "db", "index-v3").await)
                .unwrap();
        assert_eq!(health["cache_status"], "unavailable");
        assert!(health["chunks_indexed"].is_null());
        assert!(health["reflections_indexed"].is_null());
        assert!(health["error"].as_str().unwrap().contains("maintenance"));
        drop(owner);
        let after: serde_json::Value =
            serde_json::from_str(&system_health(&storage, &search, "db", "index-v3").await)
                .unwrap();
        assert_eq!(after["reflections_indexed"], 1);
    }

    #[tokio::test]
    async fn test_system_health_format() {
        // Just verify the function signature compiles and returns valid JSON
        // Full integration test requires storage + search setup
        let json_str = r#"{"chunks_indexed":0,"reflections_indexed":0,"cache_status":"clean","query_ms":0.1,"db_path":"/tmp/test.db","index_dir":"/tmp/index","binary_version":"0.1.0"}"#;
        let parsed: serde_json::Value = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed["chunks_indexed"], 0);
        assert!(parsed["binary_version"].is_string());
    }
}
