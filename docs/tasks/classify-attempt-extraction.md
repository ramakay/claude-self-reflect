# Extract classify_attempt for subprocess-404 testability

**Status:** open
**Files:** `csr-engine/src/narrative.rs`, `csr-engine/src/hooks/session_briefing.rs`, `csr-engine/src/summarizer.rs`

The model-not-found detector (walks the chain `CSR_NARRATIVE_MODEL` → `haiku` → CLI default) keys off the real `claude -p` failure shape: non-zero exit + error JSON on STDOUT with `api_error_status:404` and wording like "issue with the selected model… may not exist". The detection logic is exercised via the fixture (`tests/fixtures/claude_p_result.json`) but the **attempt/classification step is embedded in the call sites**, so the walk-vs-fail decision isn't unit-testable without spawning a subprocess.

## What to do

Extract a pure function, e.g. `classify_attempt(exit_status, stdout, stderr) -> AttemptOutcome { Success, ModelNotFound, OtherFailure }`, call it from both call sites (briefing + story), and unit-test it against: fixture 404 JSON, transient network error, timeout, success JSON, and garbage output. Guards against the CLI changing its error wording silently.
