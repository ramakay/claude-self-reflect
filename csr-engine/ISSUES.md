# CSR Engine — Issue Log (Phase 3.0 Dogfooding)

## Bug 1: Temporal parser missing "weeks" and "months" units
- **Severity**: MEDIUM
- **File**: `src/temporal/mod.rs:82-98`
- **Reproduction**: `get_timeline(time_range="last 6 months")` → error "Could not parse time expression"
- **Root cause**: `parse_dynamic_expression()` only handles `strip_suffix(" days")`, no `" weeks"` or `" months"`
- **Fix**: Add `strip_suffix(" weeks")` with `Duration::weeks(n)` and `strip_suffix(" months")` with month arithmetic
- **Status**: FIXED in source (lines 96-119)

## Bug 2: `get_recent_work` returns too few conversations
- **Severity**: MEDIUM
- **File**: `src/mcp/tools.rs:178`, `src/storage/queries.rs:186-212`
- **Reproduction**: `get_recent_work(limit=10, project="claude-self-reflect")` → returns only 2 conversations
- **Root cause**: Fetches `limit * 3` = 30 raw chunks ordered by timestamp. With 14K chunks, the 30 most recent all belong to 2-3 conversations. Format groups by conversation → only 2-3 shown.
- **Fix**: Use `SELECT ... GROUP BY conversation_id` to get 1 chunk per conversation, then LIMIT applies to distinct conversations
- **Status**: PARTIALLY FIXED — DISTINCT query added but see Bug 5 (inflated counts)

## Bug 3: `search_by_recency` silently defaults to 7-day window
- **Severity**: LOW
- **File**: `src/mcp/tools.rs:211-212`
- **Reproduction**: `search_by_recency(query="procsolve", project="procsolve-website")` → no results (procsolve data is 45+ days old)
- **Root cause**: When no `time_range`/`since`/`until` provided, defaults to `now - 7 days`. User has no indication of the time constraint applied.
- **Not a code bug**: Expected behavior, but the empty response should indicate what time range was searched.

## Bug 4: Cross-project search finds irrelevant results
- **Severity**: LOW (expected behavior)
- **Reproduction**: `csr_reflect_on_past("Phase 3 HNSW persistence")` without project filter → finds EnhanceMe "Phase 3/4" conversations
- **Root cause**: Generic terms like "Phase 3" match across all projects. User should pass `project` param.
- **Possible improvement**: When results are from different projects, highlight the project name more prominently in output.

## Bug 5: `get_recent_work` returns inflated conversation counts
- **Severity**: MEDIUM
- **File**: `src/storage/queries.rs` (get_recent_chunks query)
- **Reproduction**: `get_recent_work(limit=5, project="all", group_by="day")` → output says "15 conversations" but limit was 5
- **Root cause**: The DISTINCT fix (Bug 2) uses a JOIN that returns ALL chunks for the top-N distinct conversations. If 5 conversations average 3 chunks each, the query returns 15 rows. The format function then counts all rows in the day group.
- **Fix**: Added Rust-side dedup: `HashSet<conversation_id>` + `retain()` after query to keep 1 chunk per conversation.
- **Status**: FIXED

## Bug 6: Temporal parser singular/plural unit mismatch (deployment)
- **Severity**: LOW (code fix present, binary stale)
- **File**: `src/temporal/mod.rs:96-119`
- **Reproduction**: `search_by_recency(time_range="last 1 month")` or `get_timeline(time_range="last 2 weeks")` → error
- **Root cause**: The fix for Bug 1 (weeks/months) IS in the source code and compiled binary (10:32 AM), but the running MCP server process (started 9:13 AM) still uses the old binary. Process needs restart.
- **Fix**: Restart Claude Code to reload the MCP server with the updated binary. Not a code bug.
- **Status**: FIXED IN SOURCE — awaiting restart

---

## Red Team Testing Results (Phase 3.0)

### PASSED (no vulnerabilities)
- **SQL injection in search query**: `SELECT * FROM chunks; DROP TABLE chunks;--` → treated as semantic search, found SQL-related discussions
- **SQL injection in get_full_conversation**: `'; DROP TABLE chunks; --` → rejected by alphanumeric+hyphen+underscore validation
- **Path traversal in file search**: `../../../../etc/passwd` → "No conversations found"
- **XSS in search query**: `<script>alert('xss')</script>` → XML-escaped in output
- **Empty string search**: 0 results returned gracefully
- **Very long string** (500 chars of 'A'): 0 results, 8ms, no crash
- **Special chars in store_reflection**: quotes, backticks, unicode, emoji, null chars → stored successfully
- **Nonexistent session ID**: 0 results with helpful message

### EDGE CASES (not vulnerabilities, but notable)
- **"last 0 days"**: No results (0-width time window). Could warn user.
- **"last 999999 days"**: Timeline starts at `-0712-03-21` (712 BC). Works but start date display is absurd. Consider capping at data range.
- **"3 days ago" as time_range**: Returns no results for recency search. This parses as a specific day, not "last 3 days". Correct behavior but potentially confusing.

## Bug 7: JSONL chunker drops 74% of coding session content
- **Severity**: HIGH
- **File**: `src/import/mod.rs:146`
- **Reproduction**: Import a coding session JSONL (e.g., Phase 3 impl, 3.8MB, 1171 lines) → only 3 chunks produced
- **Root cause (two issues)**:
  1. **Type mismatch**: Chunker checked `type == "human"` but Claude Code JSONL uses `type == "user"` — 272 user messages silently dropped
  2. **Tool-only messages discarded**: 74% of messages are tool_use/tool_result (Read, Edit, Bash, Grep) with no text content blocks. The chunker only extracted text, ignoring all tool context.
- **Impact**: A 3.8MB session with 716 human/assistant messages produced only 3 chunks (150 messages). Search could not find sessions by files edited, commands run, or tools used.
- **Fix**:
  1. Accept `"user"` alongside `"human"` in type filter
  2. Added `extract_tool_context()` — extracts tool names + key params (file_path, command, pattern, query) as searchable `[Tool: param]` annotations
  3. Also fixed `parse_jsonl_messages()` (extraction module) to accept `"user"` type
- **Result**: 2.8x more chunks across 21 affected sessions (58 → 161 chunks). Phase 3 session: 3 → 9 chunks.
- **Status**: FIXED — 3 new unit tests added, 229 tests pass

---

*Issues logged 2026-02-15 during Phase 3.0 dogfooding*
*Red team testing completed 2026-02-15*
*Bug 7 discovered and fixed 2026-02-15 during session continuity troubleshooting*
