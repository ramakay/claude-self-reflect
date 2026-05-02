# v8.0.0 Release Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship v8.0.0 — fix all code quality issues, wire TAD into search, add cross-project penalty, fix error matching, add CI/CD, clean up, and merge to main.

**Architecture:** All changes are in `csr-engine/` (Rust). The 3 algorithm fixes (TAD, cross-project, error matching) modify `src/mcp/tools.rs`, `src/storage/queries.rs`, and `src/injection/predictor.rs`. CI/CD adds 3 GitHub Actions workflows. install.sh gets an Intel Mac guard.

**Tech Stack:** Rust 1.93, rusqlite 0.38, hnsw_rs 0.3, GitHub Actions, softprops/action-gh-release

**Design doc:** `docs/plans/2026-04-15-v8-release-design.md`

---

### Task 1: Fix Clippy Warnings + Format

**Files:**
- Modify: `csr-engine/src/` (multiple files, auto-fix)

**Step 1: Run clippy auto-fix**

Run: `cd csr-engine && cargo clippy --fix --lib -p csr-engine --allow-dirty 2>&1 | tail -5`
Expected: Most warnings auto-fixed

**Step 2: Run cargo fmt**

Run: `cd csr-engine && cargo fmt`
Expected: Files formatted

**Step 3: Verify zero warnings**

Run: `cd csr-engine && cargo clippy -- -D warnings 2>&1 | tail -5`
Expected: `warning: 0 warnings` or clean output with no warnings

**Step 4: Verify formatting clean**

Run: `cd csr-engine && cargo fmt -- --check`
Expected: No output (clean)

**Step 5: Run tests to confirm nothing broke**

Run: `cd csr-engine && cargo test 2>&1 | tail -5`
Expected: All tests pass

**Step 6: Commit**

```bash
git add csr-engine/src/
git commit -m "fix: resolve all clippy warnings and format code"
```

---

### Task 2: Add Batch TAD Query to Storage

**Files:**
- Modify: `csr-engine/src/storage/queries.rs` (add `get_retrieval_events_batch`)
- Modify: `csr-engine/src/storage/mod.rs` (expose new method)
- Test: `csr-engine/tests/integration.rs` (add TAD batch test)

**Step 1: Write the failing test**

Add to `csr-engine/tests/integration.rs`:

```rust
#[test]
fn test_tad_batch_retrieval_events() {
    let dir = tempdir().unwrap();
    let storage = Storage::new(dir.path().join("test.db")).unwrap();

    // Insert a chunk so we have a valid memory_id
    let chunk = ConversationChunk {
        id: "chunk-tad-1".into(),
        conversation_id: "conv-tad-1".into(),
        project_name: "test".into(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        content: "test content".into(),
        message_count: 1,
        summary: None,
    };
    storage.insert_chunk(&chunk, &[0.1; 384]).unwrap();

    // Log retrieval events
    storage.log_retrieval_event("chunk-tad-1", "chunk", "prompt_submit", "session-1").unwrap();
    storage.update_session_outcome("session-1", "success").unwrap();

    // Batch fetch
    let events = storage.get_retrieval_events_batch(&["chunk-tad-1", "nonexistent"]).unwrap();
    assert!(events.contains_key("chunk-tad-1"));
    assert!(!events.contains_key("nonexistent"));

    let chunk_events = &events["chunk-tad-1"];
    assert_eq!(chunk_events.len(), 1);
    assert_eq!(chunk_events[0].session_outcome, crate::csr_engine::search::decay::SessionOutcome::Success);
}
```

**Step 2: Run test to verify it fails**

Run: `cd csr-engine && cargo test test_tad_batch -- 2>&1 | tail -10`
Expected: FAIL — method `get_retrieval_events_batch` not found

**Step 3: Add batch query to queries.rs**

Add to `csr-engine/src/storage/queries.rs` at the end (before final `}`):

```rust
use std::collections::HashMap;

/// Batch-fetch typed retrieval events for TAD scoring.
/// Returns HashMap<memory_id, Vec<RetrievalEvent>>.
pub fn get_retrieval_events_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, Vec<crate::search::decay::RetrievalEvent>>> {
    use crate::search::decay::{RetrievalEvent, SessionOutcome};

    if memory_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let placeholders: Vec<String> = (1..=memory_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT memory_id, retrieved_at, session_outcome
         FROM retrieval_events
         WHERE memory_id IN ({})
         ORDER BY retrieved_at DESC",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> =
        memory_ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut map: HashMap<String, Vec<RetrievalEvent>> = HashMap::new();
    for row in rows {
        let (mid, ts_str, outcome_str) = row?;
        let retrieved_at = match ts_str.parse::<chrono::DateTime<chrono::Utc>>() {
            Ok(ts) => ts,
            Err(_) => continue,
        };
        let session_outcome = match outcome_str.as_str() {
            "success" => SessionOutcome::Success,
            "failed" => SessionOutcome::Failed,
            _ => SessionOutcome::Neutral,
        };
        map.entry(mid)
            .or_default()
            .push(RetrievalEvent { retrieved_at, session_outcome });
    }
    Ok(map)
}
```

**Step 4: Expose via storage/mod.rs**

Add to `csr-engine/src/storage/mod.rs` inside `impl Storage` block, after `get_retrieval_events_for_memory`:

```rust
pub fn get_retrieval_events_batch(
    &self,
    memory_ids: &[&str],
) -> Result<std::collections::HashMap<String, Vec<crate::search::decay::RetrievalEvent>>> {
    let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
    queries::get_retrieval_events_batch(&conn, memory_ids)
}
```

**Step 5: Run test to verify it passes**

Run: `cd csr-engine && cargo test test_tad_batch -- 2>&1 | tail -10`
Expected: PASS

**Step 6: Commit**

```bash
git add csr-engine/src/storage/ csr-engine/tests/
git commit -m "feat: add batch TAD retrieval events query for search scoring"
```

---

### Task 3: Wire TAD into reflect_on_past (Chunks + Reflections)

**Files:**
- Modify: `csr-engine/src/mcp/tools.rs:53-73` (chunk scoring)
- Modify: `csr-engine/src/mcp/tools.rs:76-115` (reflection scoring)

**Step 1: Add HashMap import to tools.rs**

At top of `csr-engine/src/mcp/tools.rs`, add `use std::collections::HashMap;` if not present (HashSet is already imported).

**Step 2: Replace chunk scoring (lines 53-73)**

Replace the current chunk scoring block with TAD-aware version. The key changes:
1. Batch-fetch TAD events for all chunk IDs before the filter_map
2. Replace `decay::apply_decay(r.score, ...)` with `decay::apply_tad(r.score, ...)`
3. Use `unwrap_or_default()` (cannot use `?` in closure)

Replace from `let now = chrono::Utc::now();` through the `.collect();` on the enriched block:

```rust
    let now = chrono::Utc::now();

    // Batch-fetch TAD events for all chunk results (single DB query)
    let chunk_ids_for_tad: Vec<&str> = chunk_results.iter().map(|r| r.id.as_str()).collect();
    let tad_events = storage.get_retrieval_events_batch(&chunk_ids_for_tad).unwrap_or_default();
    let tad_config = decay::DecayConfig::for_search();

    let mut enriched: Vec<EnrichedResult> = chunk_results
        .iter()
        .filter_map(|r| {
            chunks
                .iter()
                .find(|c| c.id == r.id)
                .map(|c| {
                    let decayed_score =
                        if let Ok(ts) = c.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                            let events = tad_events.get(&c.id).map(|v| v.as_slice()).unwrap_or(&[]);
                            decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                        } else {
                            r.score
                        };
                    EnrichedResult {
                        score: decayed_score,
                        chunk: c.clone(),
                    }
                })
        })
        .collect();
```

**Step 3: Update reflection scoring (lines 76-115)**

Add TAD batch fetch for reflection IDs, then update the decay call. Before the `for r in &reflection_results` loop, add:

```rust
    // Batch-fetch TAD events for reflection results
    let reflection_ids_for_tad: Vec<&str> = reflection_results.iter().map(|r| r.id.as_str()).collect();
    let reflection_tad_events = storage.get_retrieval_events_batch(&reflection_ids_for_tad).unwrap_or_default();
```

Then inside the loop, replace the `decayed_score` computation:

```rust
            let decayed_score =
                if let Some(ts) = crate::temporal::parse_timestamp(&timestamp) {
                    let events = reflection_tad_events.get(&r.id).map(|v| v.as_slice()).unwrap_or(&[]);
                    decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                } else {
                    r.score
                };
```

**Step 4: Keep FTS5 using apply_decay (no change)**

The FTS5 block at line 131 stays as `decay::apply_decay(0.45, &ts, &now, None, None)` — TAD on synthetic scores is meaningless.

**Step 5: Verify compilation**

Run: `cd csr-engine && cargo build 2>&1 | tail -5`
Expected: Clean build

**Step 6: Run all tests**

Run: `cd csr-engine && cargo test 2>&1 | tail -10`
Expected: All tests pass

**Step 7: Commit**

```bash
git add csr-engine/src/mcp/tools.rs
git commit -m "feat: wire TAD into reflect_on_past — chunks and reflections use retrieval events"
```

---

### Task 4: Add Cross-Project Multiplicative Penalty

**Files:**
- Modify: `csr-engine/src/mcp/tools.rs` (chunk, reflection, and FTS5 scoring paths)

**Step 1: Add cross-project penalty to chunk scoring**

In `tools.rs`, inside the `filter_map` closure for chunks, after computing `decayed_score` and before creating `EnrichedResult`, wrap the score:

```rust
                    // Cross-project multiplicative penalty
                    let final_score = if let Some(ref p) = effective_project {
                        if c.project_name != *p {
                            decayed_score * 0.3
                        } else {
                            decayed_score
                        }
                    } else {
                        decayed_score
                    };
                    EnrichedResult {
                        score: final_score,
                        chunk: c.clone(),
                    }
```

**Step 2: Add cross-project penalty to reflection scoring**

In the reflection loop, after computing `decayed_score` and before `enriched.push(...)`:

```rust
            // Cross-project multiplicative penalty
            let final_score = if let Some(ref p) = effective_project {
                if project_name != *p {
                    decayed_score * 0.3
                } else {
                    decayed_score
                }
            } else {
                decayed_score
            };
            enriched.push(EnrichedResult {
                score: final_score,
                ...
```

**Step 3: Add cross-project penalty to FTS5 scoring**

In the FTS5 loop, after computing `fts_score`, add:

```rust
                let final_fts_score = if let Some(ref p) = effective_project {
                    if chunk.project_name != *p {
                        fts_score * 0.3
                    } else {
                        fts_score
                    }
                } else {
                    fts_score
                };
```

Use `final_fts_score` in the `EnrichedResult`.

**Step 4: Verify compilation and tests**

Run: `cd csr-engine && cargo test 2>&1 | tail -5`
Expected: All tests pass

**Step 5: Commit**

```bash
git add csr-engine/src/mcp/tools.rs
git commit -m "feat: add cross-project multiplicative penalty (0.3x) in search scoring"
```

---

### Task 5: Fuzzy Error Matching

**Files:**
- Modify: `csr-engine/src/injection/predictor.rs:177-194` (replace `compute_error_match`)
- Test: `csr-engine/src/injection/predictor.rs` (update `test_error_match_boost`)

**Step 1: Write new tests first**

Add to the `tests` module in `csr-engine/src/injection/predictor.rs`:

```rust
    #[test]
    fn test_error_match_exact_containment() {
        let result_errors = vec!["connection reset by peer".into()];
        let current_errors = vec!["connection reset".into()];
        let score = compute_error_match(&result_errors, &current_errors);
        // "connection reset" contained in "connection reset by peer" → high score
        assert!(score >= 0.7, "containment score={score} should be >= 0.7");
        assert!(score <= 1.0, "containment score={score} should be <= 1.0");
    }

    #[test]
    fn test_error_match_word_overlap() {
        let result_errors = vec!["timeout waiting for response from server".into()];
        let current_errors = vec!["timeout error on server connection".into()];
        let score = compute_error_match(&result_errors, &current_errors);
        // Word overlap: "timeout", "server" → 2/5 or 2/6 = ~0.33-0.4
        assert!(score > 0.0, "word overlap score={score} should be > 0.0");
        assert!(score < 0.7, "word overlap score={score} should be < 0.7 (not containment)");
    }

    #[test]
    fn test_error_match_no_overlap() {
        let result_errors = vec!["out of memory".into()];
        let current_errors = vec!["permission denied".into()];
        let score = compute_error_match(&result_errors, &current_errors);
        assert_eq!(score, 0.0, "no overlap should be 0.0");
    }

    #[test]
    fn test_error_match_empty() {
        assert_eq!(compute_error_match(&[], &["error".into()]), 0.0);
        assert_eq!(compute_error_match(&["error".into()], &[]), 0.0);
    }
```

**Step 2: Run tests — new tests should fail (old binary match)**

Run: `cd csr-engine && cargo test test_error_match -- 2>&1 | tail -15`
Expected: `test_error_match_exact_containment` FAILS (returns 1.0, not in 0.7-1.0 range after the assertion checks... actually 1.0 >= 0.7 passes). The `test_error_match_word_overlap` should FAIL because current binary returns 0.0.

**Step 3: Replace compute_error_match and add split_error_words**

Replace `compute_error_match` (lines 177-194) in `csr-engine/src/injection/predictor.rs`:

```rust
/// Compute error pattern match with graduated scoring.
/// Returns: containment 0.7-1.0, word overlap 0.0-0.7, no match 0.0.
fn compute_error_match(result_errors: &[String], current_errors: &[String]) -> f32 {
    if current_errors.is_empty() || result_errors.is_empty() {
        return 0.0;
    }

    let mut best_score: f32 = 0.0;
    for ce in current_errors {
        let ce_lower = ce.to_lowercase();
        let ce_words: HashSet<&str> = split_error_words(&ce_lower);
        for re in result_errors {
            let re_lower = re.to_lowercase();

            if re_lower.contains(&ce_lower) || ce_lower.contains(&re_lower) {
                let shorter = ce_lower.len().min(re_lower.len());
                let longer = ce_lower.len().max(re_lower.len());
                if longer > 0 {
                    best_score = best_score.max(0.7 + 0.3 * (shorter as f32 / longer as f32));
                }
            } else {
                let re_words: HashSet<&str> = split_error_words(&re_lower);
                let overlap = ce_words.intersection(&re_words).count();
                let total = ce_words.len().max(re_words.len());
                if total > 0 {
                    best_score = best_score.max(overlap as f32 / total as f32);
                }
            }
        }
    }
    best_score
}

/// Split error strings into words on whitespace, underscores, hyphens, colons.
fn split_error_words(s: &str) -> HashSet<&str> {
    s.split(|c: char| c.is_whitespace() || c == '_' || c == '-' || c == ':')
        .filter(|w| !w.is_empty())
        .collect()
}
```

**Note:** `HashSet` is already imported via `use std::collections::HashSet;` — verify at top of file. If not, add it.

**Step 4: Run all error match tests**

Run: `cd csr-engine && cargo test test_error_match -- 2>&1 | tail -15`
Expected: All 4 new tests + original `test_error_match_boost` pass

**Step 5: Run full test suite**

Run: `cd csr-engine && cargo test 2>&1 | tail -5`
Expected: All tests pass

**Step 6: Commit**

```bash
git add csr-engine/src/injection/predictor.rs
git commit -m "feat: graduated fuzzy error matching — containment 0.7-1.0, word overlap 0.0-0.7"
```

---

### Task 6: Remove Dead Code (apply_decay_unified)

**Files:**
- Modify: `csr-engine/src/search/decay.rs` (remove `apply_decay_unified`, update tests)

**Step 1: Remove apply_decay_unified function**

Delete lines 72-88 in `csr-engine/src/search/decay.rs` (the `apply_decay_unified` function and its doc comment).

**Step 2: Update tests that reference apply_decay_unified**

In `decay.rs` tests:

- `test_unified_decay_matches_original` — change `apply_decay_unified` to `apply_tad` with empty events:
```rust
    #[test]
    fn test_tad_no_events_matches_original() {
        let now = Utc::now();
        let past = now - Duration::days(90);
        let config = DecayConfig::for_search();
        let tad = apply_tad(1.0, &past, &now, &[], &config);
        let original = apply_decay(1.0, &past, &now, None, None);
        assert!(
            (tad - original).abs() < 0.001,
            "tad={} original={}",
            tad,
            original
        );
    }
```

- `test_tad_reinforced_memory_decays_slower` — change `apply_decay_unified` to `apply_tad(..., &[], ...)`:
```rust
        let standard = apply_tad(1.0, &past, &now, &[], &config);
```

- `test_tad_failed_memory_decays_faster` — same change:
```rust
        let standard = apply_tad(1.0, &past, &now, &[], &config);
```

- `test_tad_no_events_equals_standard` — already uses `apply_decay_unified`, change:
```rust
        let standard = apply_tad(1.0, &past, &now, &[], &config);
        // Should equal apply_decay with default params
        let basic = apply_decay(1.0, &past, &now, None, None);
        assert!((standard - basic).abs() < 0.001);
```

**Step 3: Verify compilation**

Run: `cd csr-engine && cargo build 2>&1 | tail -5`
Expected: Clean build (no references to removed function)

**Step 4: Run tests**

Run: `cd csr-engine && cargo test decay 2>&1 | tail -15`
Expected: All decay tests pass

**Step 5: Commit**

```bash
git add csr-engine/src/search/decay.rs
git commit -m "refactor: remove dead apply_decay_unified — TAD with empty events is equivalent"
```

---

### Task 7: Add CI Workflow

**Files:**
- Create: `.github/workflows/ci.yml`

**Step 1: Create directory**

Run: `mkdir -p .github/workflows`

**Step 2: Write ci.yml**

Create `.github/workflows/ci.yml`:

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy, rustfmt
      - uses: Swatinem/rust-cache@v2
      - name: Format check
        run: cargo fmt --manifest-path csr-engine/Cargo.toml -- --check
      - name: Clippy
        run: cargo clippy --manifest-path csr-engine/Cargo.toml --locked -- -D warnings
      - name: Test
        run: cargo test --manifest-path csr-engine/Cargo.toml --locked
      - name: Verify npm package
        run: npm pack --dry-run
```

**Step 3: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: add Rust CI workflow — clippy, fmt, test, npm verify"
```

---

### Task 8: Add Release Workflow

**Files:**
- Create: `.github/workflows/release.yml`

**Step 1: Write release.yml**

Create `.github/workflows/release.yml`:

```yaml
name: Release

on:
  push:
    tags: ['v*']

permissions:
  contents: write

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: clippy
      - uses: Swatinem/rust-cache@v2
      - name: Test
        run: cargo test --manifest-path csr-engine/Cargo.toml --locked
      - name: Clippy
        run: cargo clippy --manifest-path csr-engine/Cargo.toml --locked -- -D warnings

  build:
    needs: test
    name: Build ${{ matrix.target }}
    runs-on: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        include:
          - target: aarch64-apple-darwin
            os: macos-14
            artifact: csr-engine-aarch64-apple-darwin.tar.gz
          - target: x86_64-unknown-linux-gnu
            os: ubuntu-latest
            artifact: csr-engine-x86_64-unknown-linux-gnu.tar.gz
          - target: aarch64-unknown-linux-gnu
            os: ubuntu-24.04-arm
            artifact: csr-engine-aarch64-unknown-linux-gnu.tar.gz

    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          shared-key: release-${{ matrix.target }}

      - name: Build release binary
        run: cargo build --release --locked --manifest-path csr-engine/Cargo.toml

      - name: Package binary
        run: |
          cd csr-engine/target/release
          tar -czvf ${{ matrix.artifact }} csr-engine
          mv ${{ matrix.artifact }} ../../../

      - name: Generate checksum
        run: shasum -a 256 ${{ matrix.artifact }} >> checksums-${{ matrix.target }}.txt

      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: |
            ${{ matrix.artifact }}
            checksums-${{ matrix.target }}.txt

  release:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          merge-multiple: true

      - name: Combine checksums
        run: cat checksums-*.txt > checksums.txt

      - name: Create GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            *.tar.gz
            checksums.txt
          generate_release_notes: true

  publish-npm:
    needs: release
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: 20
          registry-url: https://registry.npmjs.org/
      - run: npm publish
        env:
          NODE_AUTH_TOKEN: ${{ secrets.NPM_TOKEN }}
```

**Step 2: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: add release workflow — 3-target build, checksums, npm publish"
```

---

### Task 9: Add Security Workflow

**Files:**
- Create: `.github/workflows/security.yml`

**Step 1: Write security.yml**

Create `.github/workflows/security.yml`:

```yaml
name: Security

on:
  push:
    branches: [main]
  schedule:
    - cron: '0 2 * * 1'

jobs:
  secrets-scan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: gitleaks/gitleaks-action@v2
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}

  cargo-audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo install cargo-audit
      - run: cargo audit --file csr-engine/Cargo.lock
```

**Step 2: Commit**

```bash
git add .github/workflows/security.yml
git commit -m "ci: add security workflow — gitleaks + cargo audit"
```

---

### Task 10: Update install.sh — Intel Mac Guard + Version Check

**Files:**
- Modify: `scripts/install.sh`

**Step 1: Add Intel Mac guard after detect_platform()**

In `scripts/install.sh`, after the `detect_platform` function definition (after line 39), but before it's called — actually, add the guard inside `main()` right after `detect_platform` is called (line 163):

After `detect_platform` call (line 163), add:

```sh
    # Intel Mac: no prebuilt binaries (ort/ONNX dropped x86_64-apple-darwin)
    if [ "$TARGET" = "x86_64-apple-darwin" ]; then
        err "Intel Mac (x86_64) binaries are not provided.
Build from source instead:
  git clone https://github.com/${REPO}.git
  cd claude-self-reflect/csr-engine
  cargo build --release
  cp target/release/csr-engine ~/.local/bin/"
    fi
```

**Step 2: Verify the script syntax**

Run: `sh -n scripts/install.sh`
Expected: No output (valid syntax)

**Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "fix: install.sh — explicit error for Intel Mac (no prebuilt binaries)"
```

---

### Task 11: Add Pre-commit Hook

**Files:**
- Create: `.githooks/pre-commit`

**Step 1: Create hook**

```bash
mkdir -p .githooks
```

Create `.githooks/pre-commit`:

```sh
#!/bin/sh
set -e
cd csr-engine
cargo fmt -- --check
cargo clippy -- -D warnings
cargo test --lib 2>/dev/null
```

**Step 2: Make executable**

Run: `chmod +x .githooks/pre-commit`

**Step 3: Commit**

```bash
git add .githooks/
git commit -m "chore: add pre-commit hook — fmt, clippy, test"
```

---

### Task 12: Cleanup — Worktree + Stale Settings

**Files:**
- None (git operations and settings)

**Step 1: Remove stale worktree (if exists)**

Run: `git worktree remove /Users/ramakrishnanannaswamy/projects/claude-self-reflect-lapi 2>/dev/null || echo "already removed"`

**Step 2: Run final verification**

Run: `cd csr-engine && cargo fmt -- --check && cargo clippy -- -D warnings && cargo test 2>&1 | tail -10`
Expected: fmt clean, clippy clean, all tests pass

**Step 3: Build release binary**

Run: `cd csr-engine && cargo build --release 2>&1 | tail -3`
Expected: Clean build

**Step 4: Verify test count**

Run: `cd csr-engine && cargo test 2>&1 | grep "^test result"`
Expected: Multiple lines showing total test counts (should be 317+)

---

### Task 13: Final Verification + Merge

**Step 1: Run Codex review on final state**

Use the codex-evaluator agent to review the complete set of changes since the last review.

**Step 2: Merge to main**

```bash
git checkout main
git merge feat/rust-engine-spike --no-ff -m "feat: v8.0.0 — Rust engine replaces Python/Docker/Qdrant stack"
```

**Step 3: Tag release**

```bash
git tag v8.0.0
```

**Step 4: Push**

```bash
git push origin main
git push origin v8.0.0
```

This triggers the release workflow: test → build (3 targets) → upload to GitHub Release → npm publish.

**Step 5: Verify release artifacts**

Run: `gh release view v8.0.0` (after CI completes)
Expected: 3 tar.gz files + checksums.txt

**Step 6: Test install.sh end-to-end**

Run (on a separate machine or in a clean environment):
```bash
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
csr-engine setup
# Restart Claude Code, verify search works
```
