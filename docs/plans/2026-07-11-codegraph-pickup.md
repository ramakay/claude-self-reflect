# Codegraph Pickup (B+C) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a prompt is exploration-shaped ("where is X", "map the radio code"), inject a CSR CODE MAP block pointing at the files past episodes touched — and make that work for languages ast-grep can't parse (Swift etc.) via file-level fallback anchors.

**Architecture:** (C) `capture_file_anchors` gains a fallback: unsupported language → one file-level sentinel anchor (`node_kind: "file"`, name = file basename, body_hash = whole-file content hash). `verify_anchor`'s existing lookup-by-name + hash-compare works on it unchanged; `stop.rs` and the `episode_anchors` table need zero changes. (B) `Intent` enum gains `Explore`; on classify hit, prompt_submit reuses `correlate_episode` (Route B machinery) to pick the topic-matched episode and emits a CODE MAP block built from `episode.files_modified` + `episode.anchors` — file pointers with a ready-to-run `csr_reflect_on_past` call.

**Tech Stack:** Rust, existing csr-engine internals only (intent.rs exemplar classifier, prompt_submit routing, extraction/anchors.rs). No new crates, no migrations.

## Global Constraints

- No new crate dependencies. No SQLite schema migrations (episode_anchors columns are reused as-is: `file, node_kind, name, body_hash`).
- Hooks stay catch-all: every new path degrades to "no injection", never an error surfaced to Claude Code.
- Intent thresholds: `Explore => 0.55` (same floor as StateRecall). `thresholds_ordered_continue_stricter` test must keep passing (Continue 0.60 stays strictest).
- Adding the enum variant + exemplars auto-invalidates the on-disk probe cache via `exemplar_hash()` — no manual cache handling.
- The CODE MAP block emits ONLY when the correlated episode has at least one non-empty `files_modified` entry; otherwise fall through to the normal injection flow (no empty maps).
- CODE MAP caps: max 5 files, each line ≤120 chars. Block ends with the exact imperative `Read these before mapping; full thread: csr_reflect_on_past("conv_<id>")`.
- File-level anchors: `node_kind` exactly `"file"`, `name` = file basename (fallback to full path if basename is empty), `body_hash` computed with the SAME hash helper existing function anchors use in anchors.rs (reuse it — do not add a new hash fn).
- rusqlite: cast usize → i64 where needed. cargo fmt before every commit; pre-commit hook runs fmt-check + full test suite — never bypass with --no-verify.
- Commit trailer on every commit: `Claude-Session: https://claude.ai/code/session_0125jUYpBd7RhtNzcSx5mUeN`

---

### Task 1: File-level fallback anchors (Feature C)

**Files:**
- Modify: `csr-engine/src/extraction/anchors.rs` (capture_file_anchors ~line 96; tests module ~line 230)

**Interfaces:**
- Consumes: `lang_from_path_str` (ast_analysis.rs:388), existing body-hash helper in anchors.rs (find the fn `capture_file_anchors` uses to fill `body_hash` — reuse it verbatim on the whole file content).
- Produces: `capture_file_anchors(path)` now returns `vec![FunctionAnchor { file, node_kind: "file", name: <basename>, body_hash: <whole-file hash> }]` for existing-but-unsupported-language files. Unchanged for supported languages and missing files (empty vec).

**Why this shape:** `verify_anchor` (anchors.rs:139-155) re-runs `capture_file_anchors` and looks up by `name`, comparing `body_hash` — with the fallback inside capture, verification of file-level anchors works with ZERO changes to verify_anchor: file unchanged → Intact, edited → Modified, deleted → Broken (capture returns empty). `stop.rs:363-374` already loops `files_modified` through capture, so Swift files start producing episode_anchors rows automatically.

- [ ] **Step 1: Update the existing counter-test and add new failing tests**

The current test `unsupported_language_yields_no_anchors` (anchors.rs:236-242) asserts the OLD behavior — rewrite it and add coverage:

```rust
#[test]
fn unsupported_language_yields_file_level_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("RadioSheet.swift");
    std::fs::write(&path, "class RadioSheet { func show() {} }").unwrap();
    let anchors = capture_file_anchors(&path);
    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].node_kind, "file");
    assert_eq!(anchors[0].name, "RadioSheet.swift");
    assert!(!anchors[0].body_hash.is_empty());
}

#[test]
fn file_level_anchor_verifies_intact_then_modified() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Player.swift");
    std::fs::write(&path, "struct Player {}").unwrap();
    let anchor = capture_file_anchors(&path).remove(0);
    assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Intact);
    std::fs::write(&path, "struct Player { var ring: Bool }").unwrap();
    assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Modified);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(verify_anchor(&anchor, dir.path()), AnchorVerdict::Broken);
}

#[test]
fn missing_file_still_yields_no_anchors() {
    let anchors = capture_file_anchors(std::path::Path::new("/nonexistent/thing.swift"));
    assert!(anchors.is_empty());
}
```

Adapt `verify_anchor`'s second argument to its real signature (report says `verify_anchor(a, cwd)`; if the anchor's `file` field stores an absolute path, cwd handling may differ — match the existing tests' calling convention in anchors.rs).

- [ ] **Step 2: Run tests to verify the new ones fail**

Run: `cargo test anchors:: --lib`
Expected: new tests FAIL (capture returns empty vec for .swift); old suite otherwise green.

- [ ] **Step 3: Implement the fallback in capture_file_anchors**

At the `lang_from_path_str` gate (anchors.rs:103-105), replace early-empty-return for unsupported languages with:

```rust
let Some(lang) = lang_from_path_str(&path_str) else {
    // Unsupported language (Swift, C#, …): fall back to one file-level
    // anchor so episodes still track WHICH files changed and whether they
    // drifted since checkpoint. node_kind "file" is the sentinel; verify
    // works via the same name-lookup + body_hash comparison as symbols.
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_str.clone());
    return vec![FunctionAnchor {
        file: path_str,
        node_kind: "file".into(),
        name,
        body_hash: body_hash(&content), // ← reuse the SAME helper symbol anchors use; adapt name
    }];
};
```

Match the actual variable names/flow at the gate site. If the existing hash helper takes different input (e.g. normalized body), pass the raw file content — determinism is what matters, not normalization.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test anchors:: --lib` then `cargo test --lib`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/extraction/anchors.rs
git commit -m "feat: file-level fallback anchors for ast-unsupported languages"
```

---

### Task 2: Intent::Explore variant (Feature B, classifier side)

**Files:**
- Modify: `csr-engine/src/hooks/intent.rs` (enum :24, exemplars :33, threshold :85, tests :225)
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (classify match ~:137 — no-op arm only; real wiring is Task 4)

**Interfaces:**
- Produces: `Intent::Explore` variant; `threshold(Intent::Explore) == 0.55`; exemplars for code-location questions.
- The prompt_submit match arm added here is a deliberate no-op (falls through to normal flow) so this task compiles and ships independently; Task 4 replaces it.

- [ ] **Step 1: Write failing tests**

Add to intent.rs tests (extend `synthetic_probes()` at :234 with a third axis, e.g. `Explore` = +z):

```rust
#[test]
fn classify_selects_explore_above_threshold() {
    let probes = synthetic_probes(); // now includes (Explore, +z)
    let got = probes.classify(&[0.0, 0.0, 1.0]);
    assert_eq!(got.map(|(i, _)| i), Some(Intent::Explore));
}

#[test]
fn explore_threshold_matches_staterecall() {
    assert_eq!(threshold(Intent::Explore), threshold(Intent::StateRecall));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test intent:: --lib`
Expected: FAIL — `Explore` variant doesn't exist (compile error counts as RED here).

- [ ] **Step 3: Implement**

1. Enum (intent.rs:24-29): add `Explore`.
2. `INTENT_EXEMPLARS` (intent.rs:33-73): add tuple:

```rust
(
    Intent::Explore,
    &[
        "where is the code for this feature",
        "which files implement the bottom sheet",
        "map the radio code surface",
        "how does the player handle channel switching",
        "find the implementation of the ring navigation",
        "what code handles authentication here",
        "show me where lyrics rendering lives",
        "which module owns the audio pipeline",
        "locate the code that draws the underline animation",
        "where do we handle haptic feedback",
        "what files would I touch to change the now playing screen",
        "walk me through how this feature works in the code",
    ],
),
```

3. `threshold()` (intent.rs:85-90): `Intent::Explore => 0.55,` — add one doc-comment line: Explore shares StateRecall's floor; synthetic-only calibration, revisit with live probes.
4. prompt_submit.rs classify match (~:137-144): add arm

```rust
Intent::Explore => {
    // Wired to CODE MAP emission in the codegraph-pickup plan Task 4.
    // Until then, explore prompts keep today's behavior (fall through).
}
```

Adapt: if the match arms currently produce a reason string then share an `emit_pickup` call, restructure minimally so Continue/StateRecall behavior is byte-identical and Explore falls through to the code below Route A (do NOT return early in the Explore arm).

- [ ] **Step 4: Run tests**

Run: `cargo test intent:: --lib && cargo test --lib`
Expected: PASS, incl. existing `thresholds_ordered_continue_stricter`.

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/hooks/intent.rs csr-engine/src/hooks/prompt_submit.rs
git commit -m "feat: Explore intent class for code-location prompts"
```

---

### Task 3: CODE MAP block builder (Feature B, formatting side)

**Files:**
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (new pure fn near format helpers; tests in existing test module)

**Interfaces:**
- Consumes: `Episode` (stop.rs:31-57 — fields `files_modified: Vec<String>`, `anchors: Vec<FunctionAnchor>`, `outcome: String`, `session_id`), the age-string helper `emit_pickup`/CONTINUUM already uses (find it — same "[2d ago]" formatting).
- Produces: `pub(crate) fn format_code_map(ep: &Episode, age: &str) -> Option<String>` — `None` when no usable files.

- [ ] **Step 1: Write failing tests**

```rust
fn code_map_episode() -> Episode {
    let mut ep = /* construct the same way existing Episode tests do (see tests/hooks_integration.rs test_episode_struct_serialization) */;
    ep.session_id = "abc123".into();
    ep.outcome = "partial".into();
    ep.files_modified = vec![
        "src/radio/RadioSheet.swift".into(),
        "src/radio/ChannelRing.swift".into(),
    ];
    ep.anchors = vec![FunctionAnchor {
        file: "src/radio/RadioSheet.swift".into(),
        node_kind: "file".into(),
        name: "RadioSheet.swift".into(),
        body_hash: "h".into(),
    }];
    ep
}

#[test]
fn code_map_lists_files_with_anchor_counts_and_lookup() {
    let out = format_code_map(&code_map_episode(), "2d ago").unwrap();
    assert!(out.starts_with("CSR CODE MAP"));
    assert!(out.contains("src/radio/RadioSheet.swift"));
    assert!(out.contains("1 anchor"));
    assert!(out.contains("outcome=partial"));
    assert!(out.contains("csr_reflect_on_past(\"conv_abc123\")"));
    assert!(out.contains("Read these before mapping"));
}

#[test]
fn code_map_none_when_no_files() {
    let mut ep = code_map_episode();
    ep.files_modified.clear();
    assert!(format_code_map(&ep, "2d ago").is_none());
}

#[test]
fn code_map_caps_at_five_files() {
    let mut ep = code_map_episode();
    ep.files_modified = (0..9).map(|i| format!("src/file_{i}.swift")).collect();
    let out = format_code_map(&ep, "1h ago").unwrap();
    assert_eq!(out.matches("src/file_").count(), 5);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test code_map --lib` — FAIL (fn missing).

- [ ] **Step 3: Implement**

```rust
/// Exploration-intent injection: file pointers from the correlated episode.
/// Payload over prose — each line is a path the agent can open immediately,
/// and the footer is a ready-to-run recall call (agents obey literal calls,
/// not "consider using" advice).
pub(crate) fn format_code_map(ep: &Episode, age: &str) -> Option<String> {
    let files: Vec<&String> = ep
        .files_modified
        .iter()
        .filter(|f| !f.trim().is_empty())
        .take(5)
        .collect();
    if files.is_empty() {
        return None;
    }
    let mut out = format!(
        "CSR CODE MAP — prompt matches feature work from conv_{} ({}):\n",
        ep.session_id, age
    );
    for f in &files {
        let anchor_count = ep.anchors.iter().filter(|a| &a.file == *f).count();
        let mut line = format!("  {}", f);
        if anchor_count > 0 {
            line.push_str(&format!(
                " ({} anchor{})",
                anchor_count,
                if anchor_count == 1 { "" } else { "s" }
            ));
        }
        line.push_str(&format!(" (outcome={})", ep.outcome));
        line.truncate(120);
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!(
        "Read these before mapping; full thread: csr_reflect_on_past(\"conv_{}\")\n",
        ep.session_id
    ));
    Some(out)
}
```

Adjust field access to real Episode field types (e.g. `outcome` may be Option — degrade to `outcome=unknown`). If anchors' `file` field stores absolute paths while `files_modified` stores relative (or vice versa), match on path suffix instead of equality.

- [ ] **Step 4: Run tests** — `cargo test code_map --lib && cargo test --lib` — PASS.

- [ ] **Step 5: Commit**

```bash
git add csr-engine/src/hooks/prompt_submit.rs
git commit -m "feat: CODE MAP block builder for exploration prompts"
```

---

### Task 4: Wire Explore route (Feature B, routing side)

**Files:**
- Modify: `csr-engine/src/hooks/prompt_submit.rs` (Explore no-op arm from Task 2)
- Test: `csr-engine/tests/hooks_integration.rs`

**Interfaces:**
- Consumes: `correlate_episode()` (prompt_submit.rs:473-549 — takes query_vec + engine/idx + project scoping; returns the topic-matched episode + age), `format_code_map` (Task 3).
- Produces: Explore classify hit → correlated episode with files → print CODE MAP → `return Ok(())` (short-circuit like Route A pickup). Any miss (no correlation, no files) → fall through unchanged.

- [ ] **Step 1: Replace the no-op arm**

```rust
Intent::Explore => {
    // Exploration prompt: the user is asking WHERE code lives. The
    // topic-matched episode (not the latest one) knows which files past
    // work touched — hand those over instead of letting the agent
    // re-map the codebase from scratch.
    if let Some((ep, age)) = correlate_episode(/* same args Route B passes at :194-211 */) {
        if let Some(map) = format_code_map(&ep, &age) {
            println!("{}", map);
            return Ok(());
        }
    }
    // No correlated episode or no files — normal flow continues below.
}
```

Adapt to `correlate_episode`'s real signature and return shape (it may return the episode with a raw score rather than an age string — reuse whatever `emit_pickup`'s callers do to derive the age string). If Route B (:194-211) would run again later in the same invocation and re-derive the same episode, that's fine — the Explore arm returns early on success, so no double emission.

- [ ] **Step 2: Integration test**

Follow the `test_e2e_store_reflection_then_prompt_finds_it` fixture pattern (hooks_integration.rs:908+): in-memory Engine, seed an episode-shaped reflection (serialize an `Episode` with `files_modified`, tags `session_episode` + `project_<name>` + `conv_<id>`), then:

```rust
#[test]
fn test_explore_prompt_never_fails() {
    // Same Engine::from_parts scaffold as test_prompt_submit_catch_all_never_fails.
    // Prompt: "where is the code for the radio bottom sheet feature"
    // Assert result.is_ok() — stdout content is covered by format_code_map unit
    // tests; hook-level tests in this file assert Ok-ness only (existing pattern).
}
```

- [ ] **Step 3: Run** — `cargo test --lib && cargo test --test hooks_integration` — PASS.

- [ ] **Step 4: Commit**

```bash
git add csr-engine/src/hooks/prompt_submit.rs csr-engine/tests/hooks_integration.rs
git commit -m "feat: exploration prompts get CSR CODE MAP injection from correlated episode"
```

---

### Task 5: Codex review

- [ ] Run `/codex:review` over the branch diff (base main). Focus: Explore-arm short-circuit correctness (must not swallow Route B for non-explore prompts), file-anchor hash determinism, probe-cache invalidation on exemplar change.
- [ ] Fix CONFIRMED correctness findings via the implementer lane; re-run affected tests.

### Task 6: Build verification + live probe

- [ ] `cargo fmt --check && cargo clippy -- -D warnings && cargo test` (full suite incl. hooks_integration + integration) — all green.
- [ ] Live probe (no binary install — that's a release step): `echo '{"prompt":"where is the code for the radio bottom sheet"}' | cargo run -- hook prompt-submit` style invocation from a dir with episode data, confirm CODE MAP block or clean silence, never an error. Also probe a Continue prompt to confirm Route A unchanged.
- [ ] Update ledger + report.
