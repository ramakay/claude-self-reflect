---
title: Project Structure
---

![csr-engine source modules: the 26 module folders under csr-engine/src](/claude-self-reflect/images/module-map-10.svg)

## Key Patterns

### Engine Orchestrator
`Engine` owns `Storage`, `EmbeddingEngine`, and `SearchEngine`.

### Thread Safety
- `Storage`: `Mutex<Connection>` (rusqlite isn't Send)
- `EmbeddingEngine`: `Mutex` (embed needs `&mut self`)
- `SearchEngine`: `tokio::sync::RwLock` for async access

### Hook Error Handling
Every hook wraps inner logic in catch-all — never blocks Claude Code.

### HNSW Persistence
Dumps to disk after mutations, loads on startup with staleness detection via IndexManifest.
