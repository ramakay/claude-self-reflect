# HNSW Cache Bloat Prevention — Hotfix

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Prevent the 60GB index cache bloat that causes 14s cold-start rebuilds and blocks MCP tool calls.

**Architecture:** Three fixes: (1) clean stale numbered HNSW files after every `dump_to_disk`, (2) cap file retention to last 2 dumps, (3) add startup cache GC that runs before first tool call. All changes in `search/mod.rs` with zero API changes.

**Tech Stack:** Rust, hnsw_rs, std::fs, glob patterns

---

## Task 1: Clean Stale Files After dump_to_disk

hnsw_rs `file_dump` creates new numbered files (chunks-7905.hnsw.data) each call without removing old ones. Fix: after successful dump, remove all numbered files that aren't the ones we just created.

**Files:**
- Modify: `csr-engine/src/search/mod.rs:299-350` (dump_to_disk)

**Step 1: Write failing test**

In `csr-engine/src/search/mod.rs` (bottom of file, in `#[cfg(test)] mod tests`):

```rust
#[test]
fn test_dump_cleans_stale_files() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path();
    // Create fake stale files
    std::fs::write(dir.join("chunks-100.hnsw.data"), "old").unwrap();
    std::fs::write(dir.join("chunks-100.hnsw.graph"), "old").unwrap();
    std::fs::write(dir.join("chunks-200.hnsw.data"), "old").unwrap();
    std::fs::write(dir.join("chunks-200.hnsw.graph"), "old").unwrap();
    std::fs::write(dir.join("reflections-50.hnsw.data"), "old").unwrap();
    std::fs::write(dir.join("reflections-50.hnsw.graph"), "old").unwrap();

    // Create a minimal SearchEngine and dump
    let mut engine = SearchEngine::new(10);
    // Insert one item so dump actually writes files
    engine.insert_chunk("test_chunk".to_string(), vec![0.1; 384]);
    engine.insert_reflection("test_refl".to_string(), vec![0.2; 384]);
    engine.dump_to_disk(dir, 1, 1).unwrap();

    // Stale numbered files should be gone
    assert!(!dir.join("chunks-100.hnsw.data").exists());
    assert!(!dir.join("chunks-200.hnsw.data").exists());
    assert!(!dir.join("reflections-50.hnsw.data").exists());
    // Current files should exist (chunks.hnsw.data or a new numbered one)
    let remaining: Vec<_> = std::fs::read_dir(dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains(".hnsw."))
        .collect();
    // Should have exactly the fresh dump files (2 for chunks + 2 for reflections)
    assert!(remaining.len() <= 6, "stale files not cleaned: {:?}", remaining);
}
```

**Step 2: Implement cleanup in dump_to_disk**

After the manifest write (line 345), before setting `self.dirty = false`, add:

```rust
// Clean stale numbered HNSW files from previous dumps.
// hnsw_rs file_dump creates new numbered files each call without removing old ones.
// Keep only the unnumbered files (chunks.hnsw.data) which are the latest dump.
if let Ok(entries) = std::fs::read_dir(dir) {
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Match pattern: chunks-NNN.hnsw.data/graph or reflections-NNN.hnsw.data/graph
        if (name.starts_with("chunks-") || name.starts_with("reflections-"))
            && (name.ends_with(".hnsw.data") || name.ends_with(".hnsw.graph"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
```

**Step 3: Run test**

Run: `cd csr-engine && cargo test --lib search::tests::test_dump_cleans_stale_files`
Expected: PASS

**Step 4: Commit**

```bash
git add csr-engine/src/search/mod.rs
git commit -m "fix(cache): clean stale numbered HNSW files after dump — prevents 60GB bloat"
```

---

## Task 2: Startup Cache GC

Add a lightweight GC at engine startup that removes numbered HNSW files older than the manifest. This catches cases where dump_to_disk doesn't fire (crash, kill, etc).

**Files:**
- Modify: `csr-engine/src/engine.rs:58-100` (Engine::new, after load_from_disk)

**Step 1: Add cleanup_stale_index_files function**

In `csr-engine/src/search/mod.rs`, add a public function:

```rust
/// Remove stale numbered HNSW files from the index directory.
/// Called at startup to clean up files from previous sessions that didn't
/// get cleaned during dump_to_disk (crash, SIGKILL, etc).
pub fn cleanup_stale_index_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if (name.starts_with("chunks-") || name.starts_with("reflections-"))
            && (name.ends_with(".hnsw.data") || name.ends_with(".hnsw.graph"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
```

**Step 2: Call it in Engine::new**

In `engine.rs`, after the `load_from_disk` / rebuild logic completes, add:

```rust
// Clean stale numbered index files from previous sessions
crate::search::cleanup_stale_index_files(&index_dir);
```

**Step 3: Run full tests**

Run: `cd csr-engine && cargo test`
Expected: All pass

**Step 4: Commit**

```bash
git add csr-engine/src/search/mod.rs csr-engine/src/engine.rs
git commit -m "fix(cache): add startup GC for stale HNSW files — resilient to crashes"
```

---

## Task 3: Full Build Verification + Release

**Step 1: Full test suite**

```bash
cd csr-engine && cargo test
cd csr-engine && cargo test --test hooks_integration
cd csr-engine && cargo test --test integration
```

**Step 2: Clippy + fmt**

```bash
cargo fmt && cargo clippy
```

**Step 3: Release build + install + eval**

```bash
cargo build --release
cp target/release/csr-engine /usr/local/bin/csr-engine
csr-engine eval
```

**Step 4: Commit if needed, tag**

```bash
git tag -a v9.0.1 -m "hotfix: HNSW cache bloat prevention (60GB → 109MB)"
```

---

## Dependency Graph

```
Task 1 (dump cleanup) — standalone
Task 2 (startup GC) — uses same helper, depends on Task 1
Task 3 (verification) — depends on all above
```
