# Narrative Cost Controls Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every `claude -p` AI-narrative call (session briefing + session story) accounted, model-resilient, opt-out-able, and cached — so `csr-engine status` shows exactly what was spent and no call happens when nothing changed.

**Architecture:** A new shared module `src/narrative.rs` holds the pure logic (opt-out gate, model candidate chain, `--output-format json` parsing, FNV-1a content hash). Both existing call sites — `src/hooks/session_briefing.rs` (sync, spawn_blocking) and `src/summarizer.rs` (tokio async) — keep their own process plumbing but switch to JSON output, walk the model chain on model-not-found failures, and record usage rows into a new `narrative_usage` SQLite table. `src/status.rs` aggregates that table into the JSON and `--compact` outputs. Briefing additionally skips the call entirely when the episode window hash is unchanged since the last successful briefing.

**Tech Stack:** Rust, rusqlite 0.40 (bundled), serde_json, existing `meta` key/value table, `claude` CLI headless mode.

## Global Constraints

- Never break hooks: all hook paths must stay catch-all — a failure in accounting/model resolution must degrade to "no narrative", never to a hook error surfaced to Claude Code.
- No new crate dependencies. Hashing is a ~10-line inline FNV-1a; JSON via existing `serde_json`.
- rusqlite: no `ToSql` for `usize` — cast to `i64`.
- `csr-engine status` opens SQLite directly without running Engine migrations — every new-table query in `status.rs` must tolerate the table not existing (older DBs) and fall back to zeros.
- Env var names (exact): `CSR_NO_AI_NARRATIVES` (kill switch), `CSR_NARRATIVE_MODEL` (model override).
- Model chain order (exact): `CSR_NARRATIVE_MODEL` env → `haiku` alias → no `--model` flag (CLI default). Never a dated model ID.
- All timestamps stored as UTC ISO-8601 via SQLite `strftime('%Y-%m-%dT%H:%M:%fZ','now')`; "today" = `ts >= date('now')` (UTC midnight).
- Run all cargo commands from `csr-engine/` with native aarch64 toolchain (`source ~/.cargo/env` if cargo missing).

---

### Task 1: Research probe — pin down real `claude -p` JSON shape and model-failure behavior

**Files:**
- Create: `csr-engine/tests/fixtures/claude_p_result.json` (real captured output)
- Modify: this plan file (record findings in the two `RESEARCH:` blocks below if they differ)

**Interfaces:**
- Produces: a committed fixture file that Task 2's parser tests consume verbatim.

- [ ] **Step 1: Capture a real successful JSON result**

Run:
```bash
echo "Say exactly: ok" | claude --model haiku -p - --output-format json > /Users/ramakrishnanannaswamy/projects/claude-self-reflect/csr-engine/tests/fixtures/claude_p_result.json
cat /Users/ramakrishnanannaswamy/projects/claude-self-reflect/csr-engine/tests/fixtures/claude_p_result.json
```
Expected: a single JSON object containing at least `"result"` (string), `"is_error": false`, and a `"usage"` object with `input_tokens`, `output_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`. A `"modelUsage"` object keyed by resolved model ID is expected but optional.

RESEARCH: If field names differ from the assumptions above, update the parser code in Task 3 Step 1/3 to match the fixture — the fixture is the source of truth, not this plan.

- [ ] **Step 2: Capture model-not-found behavior**

Run:
```bash
echo "hi" | claude --model no-such-model-zz9 -p - --output-format json; echo "exit=$?"
```
Expected: non-zero exit and/or a JSON body with `is_error: true`; note the exact stderr/JSON error wording.

RESEARCH: Record the wording here. Task 3's `is_model_not_found()` matches lowercase substrings `"model"` + (`"not found"` | `"invalid"` | `"unknown"`); extend that list if the real wording differs.

- [ ] **Step 3: Confirm the `haiku` alias resolves**

Run: `echo "hi" | claude --model haiku -p - --output-format json | head -c 400`
Expected: success JSON; note the resolved model ID inside `modelUsage` — proves alias→current-Haiku resolution, the decommission-proofing this plan relies on.

- [ ] **Step 4: Commit**

```bash
git add csr-engine/tests/fixtures/claude_p_result.json docs/plans/2026-07-11-narrative-cost-controls.md
git commit -m "test: capture real claude -p JSON fixture for narrative parsing"
```

---

### Task 2: `narrative_usage` table + storage methods

**Files:**
- Modify: `csr-engine/src/storage/migrations.rs` (append to the idempotent `CREATE TABLE IF NOT EXISTS` statement list, near line 302 where `meta` is created)
- Modify: `csr-engine/src/storage/queries.rs` (new functions after `set_meta`, ~line 1130)
- Modify: `csr-engine/src/storage/mod.rs` (wrappers after `set_meta`, ~line 434)
- Test: inline `#[cfg(test)]` in `csr-engine/src/storage/queries.rs`

**Interfaces:**
- Consumes: existing `Connection`-based query pattern (`get_meta`/`set_meta`).
- Produces:
  - `queries::record_narrative_usage(conn: &Connection, row: &NarrativeUsageRow) -> Result<()>`
  - `queries::narrative_usage_summary(conn: &Connection) -> Result<NarrativeUsageSummary>`
  - `Storage::record_narrative_usage(&self, row: &NarrativeUsageRow) -> Result<()>`
  - `Storage::narrative_usage_summary(&self) -> Result<NarrativeUsageSummary>`
  - Types (defined in `queries.rs`, re-exported from `storage::mod`):
    ```rust
    pub struct NarrativeUsageRow {
        pub call_site: String,           // "briefing" | "story"
        pub model: String,               // resolved model id or "unknown"
        pub input_tokens: i64,
        pub output_tokens: i64,
        pub cache_read_tokens: i64,
        pub cache_creation_tokens: i64,
        pub duration_ms: i64,
        pub success: bool,
    }
    #[derive(Default, serde::Serialize)]
    pub struct NarrativeUsageSummary {
        pub calls_today: i64,
        pub tokens_today: i64,            // input + output, today
        pub calls_total: i64,
        pub tokens_total: i64,
        pub last_model: Option<String>,
    }
    ```

- [ ] **Step 1: Write failing tests**

In `csr-engine/src/storage/queries.rs` test module:

```rust
#[test]
fn test_narrative_usage_record_and_summary() {
    let conn = test_connection(); // reuse the module's existing in-memory test helper
    let row = NarrativeUsageRow {
        call_site: "briefing".into(),
        model: "claude-haiku-4-5".into(),
        input_tokens: 1500,
        output_tokens: 300,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        duration_ms: 4200,
        success: true,
    };
    record_narrative_usage(&conn, &row).unwrap();
    record_narrative_usage(&conn, &row).unwrap();

    let s = narrative_usage_summary(&conn).unwrap();
    assert_eq!(s.calls_total, 2);
    assert_eq!(s.tokens_total, 3600);
    assert_eq!(s.calls_today, 2); // rows stamped 'now' are today
    assert_eq!(s.tokens_today, 3600);
    assert_eq!(s.last_model.as_deref(), Some("claude-haiku-4-5"));
}

#[test]
fn test_narrative_usage_summary_empty() {
    let conn = test_connection();
    let s = narrative_usage_summary(&conn).unwrap();
    assert_eq!(s.calls_total, 0);
    assert_eq!(s.last_model, None);
}
```

(If the module's test helper has a different name than `test_connection`, use the existing one — every storage test file already opens an in-memory DB with migrations applied.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test narrative_usage --lib`
Expected: FAIL — `record_narrative_usage` not found.

- [ ] **Step 3: Add migration**

Append to the statement list in `csr-engine/src/storage/migrations.rs`:

```rust
"CREATE TABLE IF NOT EXISTS narrative_usage (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    call_site TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    success INTEGER NOT NULL DEFAULT 1
);",
```

- [ ] **Step 4: Implement queries**

In `csr-engine/src/storage/queries.rs`:

```rust
pub fn record_narrative_usage(conn: &Connection, row: &NarrativeUsageRow) -> Result<()> {
    conn.execute(
        "INSERT INTO narrative_usage
         (call_site, model, input_tokens, output_tokens, cache_read_tokens,
          cache_creation_tokens, duration_ms, success)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            row.call_site, row.model, row.input_tokens, row.output_tokens,
            row.cache_read_tokens, row.cache_creation_tokens, row.duration_ms,
            row.success as i64,
        ],
    )?;
    Ok(())
}

pub fn narrative_usage_summary(conn: &Connection) -> Result<NarrativeUsageSummary> {
    let (calls_total, tokens_total, calls_today, tokens_today) = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens + output_tokens), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN input_tokens + output_tokens ELSE 0 END), 0)
         FROM narrative_usage",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)),
    )?;
    let last_model = conn
        .query_row(
            "SELECT model FROM narrative_usage ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(NarrativeUsageSummary { calls_today, tokens_today, calls_total, tokens_total, last_model })
}
```

(`.optional()` needs `use rusqlite::OptionalExtension;` — already imported in this file for `get_meta`; verify and add if absent.)

- [ ] **Step 5: Add `Storage` wrappers**

In `csr-engine/src/storage/mod.rs`, next to `get_meta`/`set_meta` (~line 434), following their exact lock-then-delegate pattern:

```rust
pub fn record_narrative_usage(&self, row: &NarrativeUsageRow) -> Result<()> {
    let conn = self.conn.lock().unwrap();
    queries::record_narrative_usage(&conn, row)
}

pub fn narrative_usage_summary(&self) -> Result<NarrativeUsageSummary> {
    let conn = self.conn.lock().unwrap();
    queries::narrative_usage_summary(&conn)
}
```

Re-export the types: `pub use queries::{NarrativeUsageRow, NarrativeUsageSummary};`

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test narrative_usage --lib`
Expected: 2 passed.

- [ ] **Step 7: Commit**

```bash
git add csr-engine/src/storage/
git commit -m "feat: narrative_usage table + storage accounting methods"
```

---

### Task 3: Shared `src/narrative.rs` module (opt-out, model chain, JSON parse, FNV hash)

**Files:**
- Create: `csr-engine/src/narrative.rs`
- Modify: `csr-engine/src/lib.rs` (add `pub mod narrative;` alongside the other module declarations)
- Test: inline `#[cfg(test)]` in `csr-engine/src/narrative.rs`

**Interfaces:**
- Produces (consumed by Tasks 4, 5, 6):
  ```rust
  pub fn narratives_disabled() -> bool;
  pub fn model_candidates() -> Vec<Option<String>>;   // None = omit --model flag
  pub struct ParsedNarrative { pub text: String, pub model: String,
      pub input_tokens: i64, pub output_tokens: i64,
      pub cache_read_tokens: i64, pub cache_creation_tokens: i64 }
  pub fn parse_claude_json(stdout: &str) -> Option<ParsedNarrative>;
  pub fn is_model_not_found(stderr_or_json: &str) -> bool;
  pub fn fnv1a_64(data: &[u8]) -> u64;
  ```

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/claude_p_result.json");

    #[test]
    fn test_parse_real_fixture() {
        let p = parse_claude_json(FIXTURE).expect("fixture must parse");
        assert!(!p.text.is_empty());
        assert!(p.input_tokens > 0);
        assert!(p.output_tokens > 0);
        assert_ne!(p.model, ""); // "unknown" acceptable, empty is not
    }

    #[test]
    fn test_parse_rejects_error_result() {
        let json = r#"{"is_error": true, "result": "boom", "usage": {"input_tokens": 1, "output_tokens": 1}}"#;
        assert!(parse_claude_json(json).is_none());
    }

    #[test]
    fn test_parse_rejects_garbage() {
        assert!(parse_claude_json("not json").is_none());
        assert!(parse_claude_json("{}").is_none());
    }

    #[test]
    fn test_model_candidates_default_chain() {
        // Serialize env access: cargo runs tests in parallel and CSR_NARRATIVE_MODEL is process-global.
        std::env::remove_var("CSR_NARRATIVE_MODEL");
        let c = model_candidates();
        assert_eq!(c, vec![Some("haiku".to_string()), None]);
    }

    #[test]
    fn test_model_candidates_env_override_first() {
        std::env::set_var("CSR_NARRATIVE_MODEL", "sonnet");
        let c = model_candidates();
        assert_eq!(c[0], Some("sonnet".to_string()));
        assert_eq!(c[1], Some("haiku".to_string()));
        assert_eq!(c[2], None);
        std::env::remove_var("CSR_NARRATIVE_MODEL");
    }

    #[test]
    fn test_is_model_not_found() {
        assert!(is_model_not_found("Error: model 'zz9' not found"));
        assert!(is_model_not_found("Invalid model specified"));
        assert!(!is_model_not_found("rate limit exceeded"));
        assert!(!is_model_not_found("network error"));
    }

    #[test]
    fn test_fnv1a_stable_across_runs() {
        // FNV-1a is deterministic — unlike DefaultHasher (SipHash, random per-process
        // seed), which is why we can persist it in the meta table.
        assert_eq!(fnv1a_64(b"hello"), 0xa430d84680aabd0b);
        assert_ne!(fnv1a_64(b"hello"), fnv1a_64(b"hello "));
    }

    #[test]
    fn test_narratives_disabled_env() {
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
        assert!(!narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "1");
        assert!(narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "true");
        assert!(narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "0");
        assert!(!narratives_disabled());
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
    }
}
```

NOTE on the two env-var candidate tests: if they flake in parallel runs, mark both `#[serial]` only if the crate already depends on `serial_test`; otherwise merge them into one test function (sequential within one test = no race). Check `Cargo.toml` first — do not add the dependency.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test narrative:: --lib`
Expected: FAIL — module `narrative` not found.

- [ ] **Step 3: Implement the module**

Create `csr-engine/src/narrative.rs`:

```rust
//! Shared logic for AI-narrative `claude -p` invocations (session briefing +
//! session story): opt-out gate, model fallback chain, JSON result parsing,
//! and a persistence-safe content hash.
//!
//! The two call sites keep their own process plumbing (sync vs tokio); only
//! the pure decision/parsing logic lives here.

use serde_json::Value;

/// Kill switch: user disabled all AI-narrative generation.
pub fn narratives_disabled() -> bool {
    std::env::var("CSR_NO_AI_NARRATIVES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Model candidates in preference order. `None` means omit `--model` and let
/// the claude CLI pick its default — the last-resort path if every Haiku-family
/// alias is decommissioned.
pub fn model_candidates() -> Vec<Option<String>> {
    let mut chain = Vec::with_capacity(3);
    if let Ok(m) = std::env::var("CSR_NARRATIVE_MODEL") {
        let m = m.trim().to_string();
        if !m.is_empty() {
            chain.push(Some(m));
        }
    }
    chain.push(Some("haiku".to_string()));
    chain.push(None);
    chain
}

pub struct ParsedNarrative {
    pub text: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// Parse `claude -p --output-format json` stdout. Returns None on error
/// results, missing text, or unparseable output — callers treat None as
/// "no narrative this time", never as a hook failure.
pub fn parse_claude_json(stdout: &str) -> Option<ParsedNarrative> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let text = v.get("result")?.as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let usage = v.get("usage");
    let get = |key: &str| -> i64 {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let model = v
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_else(|| "unknown".to_string());
    Some(ParsedNarrative {
        text,
        model,
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: get("cache_read_input_tokens"),
        cache_creation_tokens: get("cache_creation_input_tokens"),
    })
}

/// Heuristic over stderr (or an error-JSON body): did this invocation fail
/// because the requested model does not exist? Only then do we walk to the
/// next candidate — rate limits and network errors must NOT burn retries
/// across the whole chain.
pub fn is_model_not_found(stderr_or_json: &str) -> bool {
    let s = stderr_or_json.to_lowercase();
    s.contains("model") && (s.contains("not found") || s.contains("invalid") || s.contains("unknown"))
}

/// FNV-1a 64-bit. Deterministic across processes and versions (unlike
/// std's DefaultHasher), so the digest can be persisted in the meta table.
pub fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
```

Add `pub mod narrative;` to `csr-engine/src/lib.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test narrative:: --lib`
Expected: 8 passed. If `test_parse_real_fixture` fails, the fixture's field names differ — fix the parser to match the fixture (Task 1's RESEARCH note), not the other way around.

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/narrative.rs csr-engine/src/lib.rs
git commit -m "feat: shared narrative module — opt-out, model chain, JSON parse, FNV hash"
```

---

### Task 4: Wire the briefing call site (JSON output, model chain, accounting, opt-out)

**Files:**
- Modify: `csr-engine/src/hooks/session_briefing.rs` (`handle_inner` ~line 77, `invoke_haiku_briefing` ~lines 121–178)
- Test: existing `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `crate::narrative::{narratives_disabled, model_candidates, parse_claude_json, is_model_not_found, ParsedNarrative}`; `Storage::record_narrative_usage`, `NarrativeUsageRow` (Task 2).
- Produces: `invoke_narrative_briefing(prompt: &str) -> Result<ParsedNarrative>` (renamed from `invoke_haiku_briefing`; same sync/spawn_blocking call pattern from `handle_inner`).

- [ ] **Step 1: Add the opt-out gate at the top of `handle_inner`**

Immediately after `let project_name = ...`:

```rust
if crate::narrative::narratives_disabled() {
    tracing::debug!("AI narratives disabled via CSR_NO_AI_NARRATIVES — skipping briefing");
    return Ok(());
}
```

- [ ] **Step 2: Rewrite `invoke_haiku_briefing` as `invoke_narrative_briefing`**

Replace the function (keep the doc comment style, the prompt-before-`--mcp-config` ordering comment, the `--strict-mcp-config` isolation, the manual timeout loop, and `CSR_DISABLE_RECURSIVE_HOOKS` exactly as they are):

```rust
fn invoke_narrative_briefing(prompt: &str) -> Result<crate::narrative::ParsedNarrative> {
    let mcp_config_path = write_minimal_mcp_config()?;
    let mut last_err: Option<anyhow::Error> = None;

    for candidate in crate::narrative::model_candidates() {
        let mut cmd = Command::new("claude");
        cmd.arg("-p")
            // The prompt MUST precede --mcp-config: that flag is variadic in the
            // claude CLI and consumes any trailing positional arg as another config
            // file path, failing with ENAMETOOLONG on the episode text.
            .arg(prompt);
        if let Some(model) = &candidate {
            cmd.arg("--model").arg(model);
        }
        cmd.arg("--output-format")
            .arg("json")
            .arg("--strict-mcp-config")
            .arg("--mcp-config")
            .arg(&mcp_config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .env("CSR_DISABLE_RECURSIVE_HOOKS", "1");

        let mut child = cmd.spawn()?;

        // Manual timeout: poll for completion up to BRIEFING_TIMEOUT_SECS.
        let timeout = Duration::from_secs(BRIEFING_TIMEOUT_SECS);
        let start = std::time::Instant::now();
        loop {
            match child.try_wait()? {
                Some(_status) => break,
                None => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        anyhow::bail!("claude -p timed out after {}s", BRIEFING_TIMEOUT_SECS);
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }

        let output = child.wait_with_output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if crate::narrative::is_model_not_found(&stderr) {
                tracing::warn!(model = ?candidate, "narrative model unavailable — trying next candidate");
                last_err = Some(anyhow::anyhow!("model unavailable: {}", stderr));
                continue; // walk the chain ONLY on model-not-found
            }
            anyhow::bail!("claude -p failed: {}", stderr);
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        match crate::narrative::parse_claude_json(&stdout) {
            Some(parsed) => return Ok(parsed),
            None if crate::narrative::is_model_not_found(&stdout) => {
                last_err = Some(anyhow::anyhow!("model unavailable (json error result)"));
                continue;
            }
            None => anyhow::bail!("claude -p returned unparseable/error JSON"),
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no narrative model candidates succeeded")))
}
```

- [ ] **Step 3: Record usage in `handle_inner`**

Replace the invoke + store block (~lines 106–118):

```rust
    let prompt = format!("{}{}", BRIEFING_INSTRUCTION, episodes);

    let started = std::time::Instant::now();
    let parsed =
        tokio::task::spawn_blocking(move || invoke_narrative_briefing(&prompt)).await??;
    let duration_ms = started.elapsed().as_millis() as i64;

    // Accounting is best-effort: a failed insert must never fail the hook.
    let _ = engine.storage().record_narrative_usage(&crate::storage::NarrativeUsageRow {
        call_site: "briefing".into(),
        model: parsed.model.clone(),
        input_tokens: parsed.input_tokens,
        output_tokens: parsed.output_tokens,
        cache_read_tokens: parsed.cache_read_tokens,
        cache_creation_tokens: parsed.cache_creation_tokens,
        duration_ms,
        success: true,
    });

    if parsed.text.trim().is_empty() {
        eprintln!("CSR: session-briefing returned empty output");
        return Ok(());
    }

    // Store briefing as a tagged reflection — replace any prior briefing for this project
    store_briefing(engine, project_name, &parsed.text)?;
```

(Adjust the exact `storage` path to how `session_briefing.rs` already reaches storage — it uses `engine`; follow the file's existing pattern, e.g. `engine.storage()`.)

- [ ] **Step 4: Update the file's tests and run them**

The existing test module (~line 346) references timeout constants only; add:

```rust
#[test]
fn test_briefing_skips_when_disabled() {
    std::env::set_var("CSR_NO_AI_NARRATIVES", "1");
    assert!(crate::narrative::narratives_disabled());
    std::env::remove_var("CSR_NO_AI_NARRATIVES");
}
```

Run: `cargo test session_briefing --lib`
Expected: PASS (all existing + new).

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/hooks/session_briefing.rs
git commit -m "feat: briefing uses model chain + JSON output + usage accounting + opt-out"
```

---

### Task 5: Wire the story call site (same treatment, tokio flavor)

**Files:**
- Modify: `csr-engine/src/summarizer.rs` (`generate_session_story` ~line 36, `call_claude_headless` ~lines 88–127, `generate_and_store` ~line 325)
- Test: existing `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: same `crate::narrative` + `Storage` items as Task 4.
- Produces: `generate_session_story(ctx: &str) -> Option<(String, crate::narrative::ParsedNarrative)>` — callers get story text plus usage; `generate_and_store` records the usage row.

- [ ] **Step 1: Rewrite `call_claude_headless` with JSON output + model chain**

```rust
/// Invoke `claude -p` (model chain: env override → haiku alias → CLI default)
/// with prompt piped via stdin (avoids OS arg length limits).
async fn call_claude_headless(prompt: &str) -> Option<crate::narrative::ParsedNarrative> {
    use tokio::io::AsyncWriteExt;

    for candidate in crate::narrative::model_candidates() {
        let attempt = tokio::time::timeout(HAIKU_TIMEOUT, async {
            let mut cmd = tokio::process::Command::new("claude");
            if let Some(model) = &candidate {
                cmd.args(["--model", model]);
            }
            let mut child = match cmd
                .args(["-p", "-", "--output-format", "json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(_) => return None, // claude CLI not found — no point walking the chain
            };

            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(prompt.as_bytes()).await;
                drop(stdin);
            }
            child.wait_with_output().await.ok()
        })
        .await;

        match attempt {
            Ok(Some(output)) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                match crate::narrative::parse_claude_json(&stdout) {
                    Some(mut parsed) => {
                        // Cap at 1000 chars — stories should be concise
                        parsed.text = parsed.text.chars().take(1000).collect();
                        return Some(parsed);
                    }
                    None if crate::narrative::is_model_not_found(&stdout) => continue,
                    None => return None,
                }
            }
            Ok(Some(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if crate::narrative::is_model_not_found(&stderr) {
                    continue; // next candidate
                }
                return None; // real failure — don't burn the chain on rate limits
            }
            _ => return None, // timeout or spawn failure
        }
    }
    None
}
```

- [ ] **Step 2: Update `generate_session_story` signature and gate**

```rust
pub async fn generate_session_story(ctx: &str) -> Option<(String, crate::narrative::ParsedNarrative)> {
    if crate::narrative::narratives_disabled() {
        return None;
    }
    let prompt = build_story_prompt(ctx);
    let parsed = call_claude_headless(&prompt).await?;
    Some((parsed.text.clone(), parsed))
}
```

(Match the real current body — line 40 is `call_claude_headless(&prompt).await`; keep whatever prompt-building precedes it. Fix any other caller/test of `generate_session_story` to destructure the tuple.)

- [ ] **Step 3: Record usage in `generate_and_store`**

Replace the story match (~line 337):

```rust
    let story = match generate_session_story(&ctx).await {
        Some((text, parsed)) => {
            let _ = engine.storage().record_narrative_usage(&crate::storage::NarrativeUsageRow {
                call_site: "story".into(),
                model: parsed.model.clone(),
                input_tokens: parsed.input_tokens,
                output_tokens: parsed.output_tokens,
                cache_read_tokens: parsed.cache_read_tokens,
                cache_creation_tokens: parsed.cache_creation_tokens,
                duration_ms: 0, // HAIKU_TIMEOUT bounds it; wall-clock not tracked on this path
                success: true,
            });
            text
        }
        None => {
            log_story_event(project, conv_id, "skip:haiku_unavailable_or_timeout");
            return Ok(());
        }
    };
```

- [ ] **Step 4: Fix compile errors from the signature change, then run tests**

Run: `cargo test summarizer --lib && cargo build`
Expected: PASS + clean build. Grep for other callers first: `grep -rn "generate_session_story" csr-engine/src csr-engine/tests`.

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/summarizer.rs
git commit -m "feat: story generation uses model chain + JSON output + usage accounting + opt-out"
```

---

### Task 6: Briefing content-hash cache (skip the call when episodes unchanged)

**Files:**
- Modify: `csr-engine/src/hooks/session_briefing.rs` (`handle_inner`, after the episodes load ~line 98)
- Test: existing `#[cfg(test)]` module in the same file

**Interfaces:**
- Consumes: `crate::narrative::fnv1a_64`, `Storage::{get_meta, set_meta}`.
- Produces: meta key `briefing_input_hash:<project_name>` holding the hex FNV-1a digest of the episode window that produced the last successful briefing.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn test_briefing_hash_key_roundtrip() {
    // Digest must be stable and hex-encoded — it is persisted in the meta table.
    let digest = format!("{:016x}", crate::narrative::fnv1a_64(b"episode window"));
    assert_eq!(digest.len(), 16);
    assert_eq!(digest, format!("{:016x}", crate::narrative::fnv1a_64(b"episode window")));
}
```

Run: `cargo test briefing_hash --lib` — PASS immediately (pure function); it documents the format. The behavioral check is Step 2's code review + Task 10's live verification.

- [ ] **Step 2: Add the skip-if-unchanged gate in `handle_inner`**

After `if episodes.is_empty() { ... return }` and BEFORE the prompt build:

```rust
    // Content-hash cache: if the episode window is byte-identical to the one
    // that produced the last successful briefing, a new Haiku call would emit
    // the same briefing — skip it entirely. Debounce catches rapid resumes;
    // this catches "resumed hours later but nothing new happened".
    let hash_key = format!("briefing_input_hash:{}", project_name);
    let episode_digest = format!("{:016x}", crate::narrative::fnv1a_64(episodes.as_bytes()));
    if engine.storage().get_meta(&hash_key).ok().flatten().as_deref() == Some(episode_digest.as_str())
        && recent_briefing_exists(engine, project_name, i64::MAX / 2)
    {
        tracing::debug!(project = project_name, "episodes unchanged since last briefing — skipping claude -p");
        return Ok(());
    }
```

(The `recent_briefing_exists(_, _, i64::MAX / 2)` guard means "a stored briefing exists at all" — if the user deleted reflections, regenerate despite a matching hash. If that helper can't take a huge window cleanly, add a sibling `briefing_exists(engine, project)` using the same query without the time filter.)

- [ ] **Step 3: Store the digest after a successful briefing**

Immediately after `store_briefing(engine, project_name, &parsed.text)?;`:

```rust
    let _ = engine.storage().set_meta(&hash_key, &episode_digest);
```

- [ ] **Step 4: Run tests**

Run: `cargo test session_briefing --lib`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/hooks/session_briefing.rs
git commit -m "feat: skip briefing claude -p call when episode window unchanged (FNV-1a meta cache)"
```

---

### Task 7: Surface accounting in `csr-engine status`

**Files:**
- Modify: `csr-engine/src/status.rs` (`StatusReport` ~line 16, `gather_status`, `print_compact` ~line 314)
- Test: existing `#[cfg(test)]` module in `status.rs` (~line 351)

**Interfaces:**
- Consumes: the `narrative_usage` table directly via the file's existing raw-`Connection` pattern (status does NOT construct an Engine, and must not start requiring one).
- Produces: `StatusReport.narratives: NarrativeStatus` in JSON output; ` | AI 3c/12.4k tok today` segment in compact output.

- [ ] **Step 1: Write the failing test**

In `status.rs` tests, alongside `test_compact_format` (~line 351), extend the existing constructed report with narratives and assert the segment renders:

```rust
#[test]
fn test_compact_includes_narratives() {
    let s = format_narrative_segment(&NarrativeStatus {
        calls_today: 3, tokens_today: 12_400,
        calls_total: 120, tokens_total: 480_000,
        last_model: Some("claude-haiku-4-5".into()),
        disabled: false,
    });
    assert_eq!(s, "AI 3c/12.4k tok today");

    let off = format_narrative_segment(&NarrativeStatus { disabled: true, ..Default::default() });
    assert_eq!(off, "AI off");

    let idle = format_narrative_segment(&NarrativeStatus::default());
    assert_eq!(idle, "AI 0c today");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test compact_includes_narratives --lib`
Expected: FAIL — `NarrativeStatus` not found.

- [ ] **Step 3: Implement**

Add to `status.rs`:

```rust
#[derive(Serialize, Default)]
pub struct NarrativeStatus {
    pub calls_today: i64,
    pub tokens_today: i64,
    pub calls_total: i64,
    pub tokens_total: i64,
    pub last_model: Option<String>,
    pub disabled: bool,
}
```

Add `pub narratives: NarrativeStatus,` to `StatusReport`.

In `gather_status`, populate it tolerantly (the table may not exist on a DB the Engine hasn't migrated yet):

```rust
    let narratives = gather_narratives(&conn);
```

```rust
/// Aggregate narrative_usage. Tolerates the table not existing (pre-migration
/// DBs): status opens SQLite directly and must never fail on schema gaps.
fn gather_narratives(conn: &rusqlite::Connection) -> NarrativeStatus {
    let disabled = crate::narrative::narratives_disabled();
    let base = conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(input_tokens + output_tokens), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN ts >= date('now') THEN input_tokens + output_tokens ELSE 0 END), 0)
         FROM narrative_usage",
        [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)),
    );
    let last_model = conn
        .query_row("SELECT model FROM narrative_usage ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .ok();
    match base {
        Ok((calls_total, tokens_total, calls_today, tokens_today)) => NarrativeStatus {
            calls_today, tokens_today, calls_total, tokens_total, last_model, disabled,
        },
        Err(_) => NarrativeStatus { disabled, ..Default::default() }, // table absent
    }
}

fn format_narrative_segment(n: &NarrativeStatus) -> String {
    if n.disabled {
        return "AI off".to_string();
    }
    if n.calls_today == 0 {
        return "AI 0c today".to_string();
    }
    let tok = if n.tokens_today >= 1000 {
        format!("{:.1}k", n.tokens_today as f64 / 1000.0)
    } else {
        n.tokens_today.to_string()
    };
    format!("AI {}c/{} tok today", n.calls_today, tok)
}
```

In `print_compact`, append the segment to the existing one-liner with the same separator the line already uses (inspect the current format string and match it, e.g. `| {}` with `format_narrative_segment(&report.narratives)`).

- [ ] **Step 4: Run tests**

Run: `cargo test --lib status`
Expected: PASS including `test_compact_format` (fix its constructed `StatusReport` for the new field — add `narratives: NarrativeStatus::default()`).

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/status.rs
git commit -m "feat: surface AI-narrative call/token accounting in status + compact"
```

---

### Task 8: Docs disclosure

**Files:**
- Modify: `README.md` (root) — inside the section describing enrichment/AI narratives
- Modify: `CLAUDE.md` (root, repo copy) — Key Commands or Hooks area, one line

**Interfaces:** none (prose only).

- [ ] **Step 1: Add README disclosure**

Locate the enrichment/narratives description in `README.md` and add:

```markdown
> **Token transparency:** Optional AI narratives (session briefings + story extraction) run
> `claude -p` against your existing Claude subscription — smallest available model, capped
> prompts, debounced, and skipped entirely when nothing changed. Every call is counted:
> `csr-engine status` shows calls and tokens spent today. Disable anytime with
> `CSR_NO_AI_NARRATIVES=1`; pin a model with `CSR_NARRATIVE_MODEL=<model>`.
```

- [ ] **Step 2: Add CLAUDE.md line**

In the repo-root `CLAUDE.md`, add to the environment/commands notes:

```markdown
- **AI narratives**: `claude -p` (model chain: `CSR_NARRATIVE_MODEL` → `haiku` → CLI default); usage counted in `narrative_usage` table, shown in `csr-engine status`; kill switch `CSR_NO_AI_NARRATIVES=1`.
```

- [ ] **Step 3: Commit**

```bash
git add README.md CLAUDE.md
git commit -m "docs: disclose AI-narrative token usage, accounting, and opt-out"
```

---

### Task 9: Codex review

**Files:** none (review gate).

- [ ] **Step 1: Run the Codex review over the branch diff**

Invoke `/codex:review` (or `codex:adversarial-review` for the model-chain retry logic specifically). Scope: Tasks 2–7 diffs.

- [ ] **Step 2: Triage findings**

Fix CONFIRMED correctness issues in place (amend the relevant task's file, commit as `fix:`). Explicitly reject style-only findings with a one-line reason.

---

### Task 10: Build verification (live)

**Files:** none.

- [ ] **Step 1: Full gate**

Run:
```bash
cd /Users/ramakrishnanannaswamy/projects/claude-self-reflect/csr-engine
cargo fmt && cargo clippy -- -D warnings && cargo test
```
Expected: clean fmt, zero clippy warnings, all tests pass (52+ unit, 68 hooks integration, 44 Phase 1 integration, plus new ones).

- [ ] **Step 2: Live probe — accounting end to end**

```bash
cargo build --release
CSR_TEST=1 ./target/release/csr-engine hook session-briefing < /dev/null || true
./target/release/csr-engine status | python3 -c "import json,sys; print(json.load(sys.stdin)['narratives'])"
./target/release/csr-engine status --compact
```
Expected: `narratives` object present (zeros acceptable on a fresh DB); compact line contains `AI `. Exact hook-invocation stdin shape: reuse whatever `tests/hooks_integration` feeds the briefing hook.

- [ ] **Step 3: Live probe — opt-out**

```bash
CSR_NO_AI_NARRATIVES=1 ./target/release/csr-engine status | grep -o '"disabled": true'
```
Expected: `"disabled": true`.

- [ ] **Step 4: Commit any straggler fixes; do NOT install the binary**

Binary install (`cp` → codesign SIGKILL gotcha on macOS) is a release step, not part of this plan.
