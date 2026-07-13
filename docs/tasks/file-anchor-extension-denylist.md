# Extension denylist for file-level anchors (if noisy)

**Status:** watching
**Files:** `csr-engine/src/extraction/anchors.rs` (`capture_file_anchors`, file-level fallback)

v9.3.0 added file-level fallback anchors (node_kind `"file"`, whole-file hash) for languages outside ast-grep's six. This makes ANCHORS work for Swift/Kotlin/etc., but it anchors **any** edited file — including lockfiles, generated code, JSON blobs, and binary-ish assets — where "modified since checkpoint" is meaningless churn.

## What to do

1. Watch CONTINUUM `ANCHORS: N intact, M modified` lines for noise: if M is dominated by lockfiles/generated files, act.
2. Add a denylist in `capture_file_anchors`: skip extensions/names like `*.lock`, `package-lock.json`, `Cargo.lock`, `*.min.js`, `*.map`, `*.svg`, `dist/`, `node_modules/`, `target/`.
3. Keep it a denylist (anchor everything by default), not an allowlist — the whole point of the fallback is any-language coverage.

Only implement after observed noise; current anchor cap (40) already bounds the damage.
