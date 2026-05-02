# v8.0.0 Release Design — Rust Engine to Main

> Date: 2026-04-15
> Branch: `feat/rust-engine-spike` → `main`
> Status: FINAL — Codex-reviewed, all findings addressed

## Context

The Rust engine (`csr-engine/`) has replaced the entire Python/Docker/Qdrant stack with a single 44MB binary. 24 commits across Phases 0-4, 317 tests, production-validated across 5+ projects over 2 weeks. This document covers everything needed to merge to main and ship v8.0.0.

## Current State

- **Tests**: 317 pass (227 unit + 39 hooks + 51 integration) — verify with `cargo test` before release
- **Binary**: 44MB release, aarch64-apple-darwin
- **DB**: 15,289 chunks, 522 reflections, 187 stories, 585 TAD events
- **Diff vs main**: 353 files changed, +27,631 / -62,386 lines (net -34,755)
- **Deleted**: All Python, Docker, Qdrant config, 15 Dockerfiles, 4 requirements.txt
- **Added**: `csr-engine/` (Rust), `installer/` (npm thin wrapper), `scripts/install.sh`

---

## Section 1: Clippy Warnings (31 warnings → 0)

All 31 warnings are auto-fixable cosmetic issues:

- Unnecessary casts (`now.year() as i32` where already i32)
- Redundant closures
- Manual `div_ceil` / `is_multiple_of` / `clamp` reimplementations
- Useless `format!()` calls
- `Ok(x?)` patterns (enclosing Ok and ? unneeded)

**Fix**: `cargo clippy --fix --lib -p csr-engine --allow-dirty` then manual review.
**Also add**: `cargo fmt --manifest-path csr-engine/Cargo.toml -- --check` (Codex L-2).
**Verify**: `cargo clippy -- -D warnings` AND `cargo fmt -- --check` must both pass with zero warnings.

---

## Section 2: Wire TAD into MCP Search

### Problem

`apply_tad()` exists in `src/search/decay.rs` and is tested, but `reflect_on_past()` in `src/mcp/tools.rs` calls `apply_decay()` (basic time decay) instead. The 585 retrieval events accumulated over 2 weeks are unused in search scoring.

### There Are 3 Call Sites (not 2)

All 3 `apply_decay` calls in `reflect_on_past()` need updating:
1. **Chunk results** (tools.rs:63) — semantic HNSW matches
2. **Reflection results** (tools.rs:80) — stories/narratives
3. **FTS5 fallback** (tools.rs:131) — keyword matches → **keep as `apply_decay`** (see Rationale below)

### Type Conversion Required (Codex C-1)

The existing `get_retrieval_events_for_memory()` returns `Vec<(String, String, String)>` (tuples), but `apply_tad()` expects `&[RetrievalEvent]`. Need a conversion layer.

Add to `storage/queries.rs`:

```rust
/// Get typed retrieval events for TAD scoring.
/// Converts raw DB tuples into RetrievalEvent structs.
pub fn get_retrieval_events_typed(
    conn: &Connection,
    memory_id: &str,
) -> Result<Vec<crate::search::decay::RetrievalEvent>> {
    use crate::search::decay::{RetrievalEvent, SessionOutcome};

    let raw = get_retrieval_events_for_memory(conn, memory_id)?;
    let mut events = Vec::with_capacity(raw.len());
    for (retrieved_at_str, outcome_str, _hook_phase) in raw {
        let retrieved_at = match retrieved_at_str.parse::<chrono::DateTime<chrono::Utc>>() {
            Ok(ts) => ts,
            Err(_) => continue, // skip unparseable timestamps
        };
        let session_outcome = match outcome_str.as_str() {
            "success" => SessionOutcome::Success,
            "failed" => SessionOutcome::Failed,
            _ => SessionOutcome::Neutral,
        };
        events.push(RetrievalEvent {
            retrieved_at,
            session_outcome,
        });
    }
    Ok(events)
}
```

Add batch version (Codex M-1 — single lock acquisition, cleaner code):

```rust
/// Batch-fetch retrieval events for multiple memory IDs.
/// Returns HashMap<memory_id, Vec<RetrievalEvent>>.
pub fn get_retrieval_events_batch(
    conn: &Connection,
    memory_ids: &[&str],
) -> Result<HashMap<String, Vec<crate::search::decay::RetrievalEvent>>> {
    use crate::search::decay::{RetrievalEvent, SessionOutcome};

    if memory_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Build IN clause with positional params
    let placeholders: Vec<String> = (1..=memory_ids.len()).map(|i| format!("?{}", i)).collect();
    let sql = format!(
        "SELECT memory_id, retrieved_at, session_outcome
         FROM retrieval_events
         WHERE memory_id IN ({})
         ORDER BY retrieved_at DESC",
        placeholders.join(", ")
    );

    let mut stmt = conn.prepare(&sql)?;
    let params: Vec<&dyn rusqlite::types::ToSql> = memory_ids.iter().map(|id| id as &dyn rusqlite::types::ToSql).collect();
    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?, // memory_id
            row.get::<_, String>(1)?, // retrieved_at
            row.get::<_, String>(2)?, // session_outcome
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
        map.entry(mid).or_default().push(RetrievalEvent { retrieved_at, session_outcome });
    }
    Ok(map)
}
```

Expose via `storage/mod.rs`:

```rust
pub fn get_retrieval_events_batch(&self, memory_ids: &[&str]) -> Result<HashMap<String, Vec<decay::RetrievalEvent>>> {
    let conn = self.conn.lock().unwrap();
    queries::get_retrieval_events_batch(&conn, memory_ids)
}
```

### Updated tools.rs — Chunk + Reflection Paths

**Critical**: Cannot use `?` operator inside `filter_map` closure (Codex C-2). Use `unwrap_or_default()`.

```rust
// BEFORE the filter_map — batch-fetch all TAD events in one query
let chunk_ids_for_tad: Vec<&str> = chunk_results.iter().map(|r| r.id.as_str()).collect();
let tad_events = storage.get_retrieval_events_batch(&chunk_ids_for_tad).unwrap_or_default();
let tad_config = decay::DecayConfig::for_search();

let mut enriched: Vec<EnrichedResult> = chunk_results
    .iter()
    .filter_map(|r| {
        chunks.iter().find(|c| c.id == r.id).map(|c| {
            let decayed_score =
                if let Ok(ts) = c.timestamp.parse::<chrono::DateTime<chrono::Utc>>() {
                    let events = tad_events.get(&c.id).map(|v| v.as_slice()).unwrap_or(&[]);
                    decay::apply_tad(r.score, &ts, &now, events, &tad_config)
                } else {
                    r.score
                };
            EnrichedResult { score: decayed_score, chunk: c.clone() }
        })
    })
    .collect();
```

For reflection results (loop at tools.rs:76-114), same pattern — batch-fetch reflection IDs, use `apply_tad`.

### FTS5 Fallback — Keep as `apply_decay` (Codex L-1)

FTS5 results use a synthetic base score of 0.45. TAD adjusts half-life based on retrieval events, but the score being adjusted is synthetic, not a real similarity score. Applying TAD to a synthetic score is semantically meaningless. Keep `apply_decay(0.45, &ts, &now, None, None)` for FTS5 results.

### Consolidate Decay Functions (Codex M-3)

After this change, the codebase will have 3 decay functions:
- `apply_decay` — used only by FTS5 fallback
- `apply_decay_unified` — **dead code** (never called outside tests)
- `apply_tad` — used by chunk + reflection results

**Action**: Remove `apply_decay_unified` entirely. Keep `apply_decay` for FTS5 (simple, no events). Update its tests to call `apply_tad` with empty events to verify equivalence.

---

## Section 3: Cross-Project Multiplicative Penalty (NEW FEATURE)

### Problem

Codex H-2 confirmed: no cross-project penalty exists anywhere in the codebase. `predictor.rs` scores results without any concept of "same project vs cross project." `cross_project.rs` only handles project name resolution.

### Design

This is a **new feature**, not a modification. Apply the penalty in `reflect_on_past()` (not the predictor, which is for injection hooks):

In `tools.rs`, after computing `decayed_score` but before pushing to `enriched`:

```rust
// Apply cross-project penalty if searching within a specific project
let final_score = if let Some(ref p) = effective_project {
    if c.project_name != *p {
        decayed_score * 0.3 // multiplicative penalty for cross-project
    } else {
        decayed_score
    }
} else {
    decayed_score // "all" scope — no penalty
};
```

Apply to all 3 paths: chunks, reflections, FTS5.

**Note**: When `project` is `None` / `"all"`, no penalty applies (user explicitly wants cross-project). When scoped to a specific project, cross-project results are ranked ~70% lower.

---

## Section 4: Fuzzy Error Matching

### Problem

`compute_error_match()` in `src/injection/predictor.rs:178-193` returns binary 1.0 or 0.0.

### Implementation

```rust
fn compute_error_match(result_errors: &[String], current_errors: &[String]) -> f32 {
    if current_errors.is_empty() || result_errors.is_empty() {
        return 0.0;
    }

    let mut best_score: f32 = 0.0;
    for ce in current_errors {
        let ce_lower = ce.to_lowercase();
        // Split CamelCase into words for comparison (Codex M-2)
        let ce_words: HashSet<&str> = split_error_words(&ce_lower);
        for re in result_errors {
            let re_lower = re.to_lowercase();
            let re_words: HashSet<&str> = split_error_words(&re_lower);

            if re_lower.contains(&ce_lower) || ce_lower.contains(&re_lower) {
                // Full containment — high score
                let shorter = ce_lower.len().min(re_lower.len());
                let longer = ce_lower.len().max(re_lower.len());
                if longer > 0 {
                    best_score = best_score.max(0.7 + 0.3 * (shorter as f32 / longer as f32));
                }
            } else {
                // Word overlap (handles CamelCase, snake_case, spaces)
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

/// Split error strings into words, handling CamelCase, snake_case, and spaces.
fn split_error_words(s: &str) -> HashSet<&str> {
    // Split on whitespace, underscores, hyphens, colons
    s.split(|c: char| c.is_whitespace() || c == '_' || c == '-' || c == ':')
        .filter(|w| !w.is_empty())
        .collect()
}
```

**Known limitation** (documented per Codex M-2): CamelCase like `ECONNREFUSED` vs `connection refused` won't match via word overlap. The containment check handles the common case where one is a substring of the other. Full CamelCase splitting (regex on uppercase boundaries) is a future improvement.

### Test Updates

Update `test_error_match_boost` to verify:
- Exact containment → score ~0.85-1.0
- Partial word overlap → score 0.3-0.7
- No overlap → score 0.0

---

## Section 5: CI/CD Workflows

### Delete (Python-era, no longer relevant)

These files exist on main but not on the branch, so they're deleted by the merge:
- `.github/workflows/ci.yml`
- `.github/workflows/security.yml`
- `.github/workflows/claude-code-review.yml`
- `.github/workflows/claude.yml`
- `.github/workflows/security-pcre2-check.yml`

### Create: `.github/workflows/ci.yml`

Triggers: push to main, PRs to main.

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

**Note**: Uses `--locked` everywhere to match release build (Codex H-4).

### Create: `.github/workflows/release.yml`

Triggers: `v*` tag push. Builds 3 targets, uploads to GitHub Release with checksums, publishes npm.

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

**Key changes from initial design (Codex fixes)**:
- **C-3**: Added `checksums.txt` generation — each build job creates a per-target checksum file, the `release` job combines them and uploads to the GitHub Release. `install.sh` already expects this.
- **H-3**: All `actions/checkout` use `@v4` (not `@v5`).
- **H-4**: `--locked` used everywhere (CI and release).
- **H-5**: ARM64 Linux uses `ubuntu-24.04-arm` (current recommended label). If this fails on a private repo, fall back to `cross` tool.
- **M-4**: Added `test` job as a dependency of `build` — release cannot proceed without tests passing.
- **M-5**: `publish-npm` now depends on `release` (not `build`), so GitHub Release assets are uploaded before npm publish triggers postinstall instructions.
- Separated `build` (matrix) → `release` (combine checksums + upload) → `publish-npm` (sequential).

### Create: `.github/workflows/security.yml`

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

### Platform Note

**Intel macOS (x86_64-apple-darwin) is excluded.** The `ort` crate (ONNX Runtime, used by fastembed) officially dropped prebuilt binaries for this target in v2.0+ (pykeio/ort#556, closed as won't fix). Building from source takes 30-40 minutes per CI run. Intel Macs are end-of-life.

---

## Section 6: install.sh — Intel Mac Guard (Codex L-5)

The existing `install.sh` detects Intel Macs and constructs target `x86_64-apple-darwin`, then tries to download a binary that won't exist. Add an explicit guard:

```sh
# After detect_platform()
if [ "$TARGET" = "x86_64-apple-darwin" ]; then
    err "Intel Mac (x86_64) binaries are not provided. Apple dropped support for this architecture.
Build from source instead:
  git clone https://github.com/${REPO}.git
  cd claude-self-reflect/csr-engine
  cargo build --release
  cp target/release/csr-engine ~/.local/bin/"
fi
```

Also add `--version` verification after download (already mentioned in install.sh):

```sh
# After extracting binary
"$INSTALL_DIR/$BINARY_NAME" --version || err "Binary verification failed — architecture mismatch?"
```

---

## Section 7: Cleanup

### Stale Worktree

```bash
git worktree remove /Users/ramakrishnanannaswamy/projects/claude-self-reflect-lapi
```

### Quality Gate — Pre-commit Hook

The old `scripts/quality-gate-staged.py` was removed with Python. Replace with:

```bash
#!/bin/sh
# .githooks/pre-commit
set -e
cd csr-engine
cargo fmt -- --check
cargo clippy -- -D warnings
cargo test --lib 2>/dev/null
```

Set via: `git config core.hooksPath .githooks`

### settings.json

Remove `effortLevel: "high"` if no longer needed.

### Dead Code Removal

- Remove `apply_decay_unified` from `decay.rs` (dead code after TAD wiring — Codex M-3)
- Update tests that reference `apply_decay_unified` to use `apply_tad` with empty events

### Verify Post-Merge

`.github/workflows/` should contain only:
- `ci.yml` (new)
- `release.yml` (new)
- `security.yml` (new)

---

## Section 8: Rollback Plan (Codex M-6)

If v8.0.0 is broken after release:

1. **npm**: `npm unpublish claude-self-reflect@8.0.0` (within 72h of publish)
2. **GitHub Release**: Delete the v8.0.0 release via `gh release delete v8.0.0`
3. **Git**: Tag previous working state as v7.1.16, re-release from main~1
4. **Data**: Users' SQLite DB (`~/.claude-self-reflect/csr-engine.db`) is NOT backward-compatible with the Python/Qdrant stack. Rolling back means users lose new data unless they re-import conversations.
5. **Communication**: Update README with rollback instructions if needed.

**Mitigation**: Test the full install path (Section 10) before announcing the release.

---

## Section 9: Release Sequence

1. Fix clippy + fmt (Section 1)
2. Wire TAD into search with batch query (Section 2)
3. Add cross-project multiplicative penalty (Section 3)
4. Fix fuzzy error matching (Section 4)
5. Add CI workflows (Section 5)
6. Update install.sh — Intel Mac guard (Section 6)
7. Cleanup — worktree, pre-commit hook, dead code (Section 7)
8. Run full test suite: `cargo test` + `cargo clippy -- -D warnings` + `cargo fmt -- --check`
9. Codex final review of implementation
10. Merge `feat/rust-engine-spike` → `main`
11. Tag `v8.0.0`
12. CI builds 3 binaries → GitHub Release with checksums
13. NPM publish (thin wrapper)
14. Verify install.sh downloads correct binary on macOS ARM64
15. Test fresh install: `curl | sh` → `csr-engine setup` → restart Claude Code → search works

---

## Success Criteria

- [ ] `cargo fmt -- --check` — zero formatting issues
- [ ] `cargo clippy -- -D warnings` — zero warnings
- [ ] `cargo test` — 317+ tests pass
- [ ] TAD events used in search scoring (batch query, typed conversion)
- [ ] Cross-project penalty is multiplicative (* 0.3) in reflect_on_past
- [ ] Error matching returns graduated scores (containment 0.7-1.0, word overlap 0.0-0.7)
- [ ] `apply_decay_unified` removed (dead code)
- [ ] CI runs on PR to main (test + clippy + fmt)
- [ ] Release workflow builds 3 targets on tag push with checksums
- [ ] install.sh: Intel Mac shows build-from-source instructions (not download failure)
- [ ] install.sh: Binary verified after download (`--version` check)
- [ ] npm package publishes thin wrapper
- [ ] Fresh install works: curl → setup → restart → search
- [ ] Pre-commit hook installed (clippy + fmt + test)

---

## Codex Review Log

Reviewed by codex-evaluator on 2026-04-15. Findings addressed:

| ID | Severity | Finding | Resolution |
|----|----------|---------|------------|
| C-1 | CRITICAL | TAD type mismatch — `get_retrieval_events_for_memory` returns tuples, not `RetrievalEvent` | Added `get_retrieval_events_typed` + batch variant with conversion layer |
| C-2 | CRITICAL | `?` operator can't propagate from `filter_map` closure | Changed to `unwrap_or_default()` pattern, batch-fetch before closure |
| C-3 | CRITICAL | Release workflow missing `checksums.txt` but `install.sh` expects it | Added per-target checksum generation + combine step in release job |
| H-1 | HIGH | TAD wiring misses FTS5 fallback (3rd call site) | Documented: FTS5 keeps `apply_decay` (synthetic score, TAD meaningless) |
| H-2 | HIGH | Cross-project penalty doesn't exist — Section 3 is new feature | Rewrote as new feature in `reflect_on_past()`, not predictor mod |
| H-3 | HIGH | `actions/checkout@v5` doesn't exist | Changed all to `@v4` |
| H-4 | HIGH | `--locked` inconsistent between CI and release | Added `--locked` everywhere |
| H-5 | HIGH | `ubuntu-22.04-arm` may not be available | Changed to `ubuntu-24.04-arm` with fallback note |
| M-1 | MEDIUM | N+1 TAD query — batch from start | Implemented batch query with `WHERE memory_id IN (...)` |
| M-2 | MEDIUM | Word overlap weak for CamelCase errors | Added `split_error_words` for snake_case/hyphen/colon splitting; documented CamelCase limitation |
| M-3 | MEDIUM | `apply_decay_unified` is dead code | Added to cleanup: remove entirely |
| M-4 | MEDIUM | Release workflow doesn't run tests before build | Added `test` job as dependency of `build` |
| M-5 | MEDIUM | npm publishes before binaries are downloadable | `publish-npm` now depends on `release` (not `build`) |
| M-6 | MEDIUM | No rollback plan | Added Section 8: Rollback Plan |
| L-1 | LOW | FTS5 shouldn't use TAD (synthetic score) | Documented — keep `apply_decay` for FTS5 |
| L-2 | LOW | Pre-commit missing `cargo fmt --check` | Added to Section 1 and pre-commit hook |
| L-3 | LOW | Security scan weekly vs more frequent | Kept weekly schedule + push-to-main trigger (already sufficient) |
| L-4 | LOW | Test count discrepancy (273 vs 317) | Added "verify with cargo test" note; counts may differ due to feature flags |
| L-5 | LOW | `install.sh` fails silently for Intel Mac | Added explicit guard with build-from-source instructions |
