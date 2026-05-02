---
title: Project Structure
---

![csr-engine project structure — src/ modules for mcp, hooks, search, storage, import, embeddings, extraction, injection, format, temporal, daemon, eval, and api alongside tests/ and Cargo.toml](/claude-self-reflect/images/project-structure-2.png)

## Key Patterns

### Engine Orchestrator
`Engine` owns `Storage`, `EmbeddingEngine`, and `SearchIndex`.

### Thread Safety
- `Storage`: `Mutex<Connection>` (rusqlite isn't Send)
- `EmbeddingEngine`: `Mutex` (embed needs `&mut self`)
- `SearchIndex`: `tokio::sync::RwLock` for async access

### Hook Error Handling
Every hook wraps inner logic in catch-all — never blocks Claude Code.

### HNSW Persistence
Dumps to disk after mutations, loads on startup with staleness detection via IndexManifest.
