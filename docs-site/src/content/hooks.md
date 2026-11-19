---
title: Session hooks
---

## The Hook Architecture

Eight hooks fire on seven events in the session lifecycle; SessionStart runs two (the recap, then the async briefing). The synchronous hooks inject context or store data and never block the session. The briefing hook is async: Claude Code does not wait for it, and it makes one `claude -p` call. Run `csr-engine telemetry` to see the latency of each hook on your own machine.

Claude does not have to ask. **Context finds you.**

## Session Lifecycle

![Session lifecycle: eight hooks on seven events, from SessionStart through SessionEnd, each hook with its csr-engine command](images/hook-lifecycle-10.svg)

## SessionStart — Past Context Injection

Fires when you start a conversation. On `startup` and `resume` it injects one recap paragraph only when your last session left evidence to state (a commit, a finished or open task, a next step, a verdict, a correction, a dream finding), and one `learnt` line only when CSR stored notes, verdicts or retired anchors for that session. After a `compact` it adds a compact receipt: a pointer line and `csr_transcript` slice pointers for the turns the summary replaced. No LLM writes any of these.

- **Recap**: a single causal paragraph about your last session. Each clause drops out when there is no evidence for it, `Next:` is never invented, and every settled claim carries a receipt. On `startup`, "last session" is the project's newest session that was not headless (`claude -p`, entrypoint `sdk-*`), so a row of `-p` workers all start from the same interactive session. On `resume` it is the session you resumed, and on `compact` the session itself.
- **Learnt line**: what CSR stored for the previous session: memory notes, `csr_resolve` verdicts you confirmed, anchors retired by a commit. When CSR stored none of these, there is no line. At most 320 bytes.
- **Fallback**: with no evidence to state (no commit, finished or open task, next step, verdict, correction or dream finding), the paragraph would only repeat your prompt, so SessionStart prints the older `CSR CONTINUUM` and `LAST` lines in its place. The same lines appear if the recap feeds fail. The `EPISODE INDEX` follows in both cases.

The recap grammar:
```
recap [<age>]: <intent>: <completed>. Settled: <claim> (<receipt>). Now: <blockers|still-open|proposals|todos>. Learnt-then-retired while away: <label> (superseded <date>, <oid>). Next: <evidenced next step>.
```

The learnt line:
```
learnt [<age>]: <clauses>. [[CSR:LEARNT]]
```

Turn them off with `CSR_NO_RECAP=1` and `CSR_NO_LEARNT=1`.

## UserPromptSubmit — Predictive Injection

Fires every prompt. Routes first, then scores.

**Intent routing** — a semantic classifier (exemplar embeddings over the same local MiniLM model, no extra model shipped) detects three intents:

| Route | Intent | Threshold | Action |
|-------|--------|-----------|--------|
| A | Continue / StateRecall ("pick up where we left off", "what were we doing?") | 0.60 / 0.55 | Inject a recent project episode's state directly. This route can run with a missing session ID |
| A | Explore ("how does the ring navigation work?") | 0.55 | Inject a **CODE MAP** — file pointers from the conversation that built the feature, plus a ready-to-run recall call — before the agent re-maps the codebase |
| B | Topic correlation | recency-weighted, 7-day half-life | When the content block clears its gate, it can include a pickup pointer to a matching episode |

The machine check uses the original prompt and the hook's `source` when present. Recognized harness and agent turns, including a forwarded note with nothing typed after it, get no block and log `reason=machine_prompt`. When you type a reply after a forwarded note, the prompt can pass and the note can influence recall alongside your reply. When `source` is absent or blank, the text drives the check. An explicit `sdk`, `poll_event`, or other unrecognized source answers silently.

Everything else falls through to multi-signal scoring:

| Signal | Weight |
|--------|--------|
| Semantic similarity | 30% |
| Recency | 25% |
| File overlap | 20% |
| Error pattern match | 10% |
| Phase boost (how well the result type fits the hook phase) | 15% |

500-token budget. Skips trivial prompts and slash commands.

The content block can include reflections, topic pickup pointers, dream findings, anchor notices, and code notes. Its gate requires at least three rendered conversation excerpts or reflections and a best match that meets the relevance floor. Hybrid search lets keyword hits fill unused chunk slots, and those hits count if rendered. The most recent project session can receive a continuity boost. A single delivery may include multiple distinct excerpts from a conversation or represent it once with a pickup pointer or reflection.

## Session story — kept current at each compaction

A session's story is the short account later sessions read about it. After each compaction, SessionStart starts a detached `csr-engine generate-story --from-compaction`, which rebuilds the story from the summary Claude Code writes as it compacts. The new story replaces the old one, and CSR records how many of the session's messages it covers.

By default CSR extracts the story from that summary: what was asked, then where the work stood. There is no model call. The summary has a blind spot, though. Claude Code keeps the last few messages verbatim after it, so the summary never sees how the work ended.

Set `CSR_STORY_REWRITES=1` and Haiku writes the story instead. It reads the summary for the session's whole arc and the last 20 messages for where it stood. At SessionEnd it runs again, if you asked something since the last story, so the ending reaches the story too. Each is one metered `claude -p` call.

## SessionEnd — Automatic Narrative

Imports final transcript, runs V3 extraction, synthesizes session story locally (free), falls back to an AI narrative via `claude -p` if needed (model chain: `CSR_NARRATIVE_MODEL` → `haiku` → CLI default; every call metered, `CSR_NO_AI_NARRATIVES=1` disables).

Stored narrative example:
```
## Session Story: csr-engine hook system
Project: claude-self-reflect | 2 hours, 102 messages

### Key Decisions
- catch-all wrappers to never block Claude Code
- UTF-8 safe truncation with floor_char_boundary()
- Iteration dedup prevents 500 embeddings/session
```

## Stop: Transcript and Episode

Fires at the end of every response. Imports the growing transcript so it is searchable now, writes a rolling `session_latest` summary so SessionStart has something to show if the session dies, and extracts a structured episode: what was asked, files investigated and modified, tools, outcome, errors and todos. It also caches the anchor that a later compaction recap uses.

## PreCompact: Refresh, Dream

Fires before a compaction. Reads the transcript to refresh the saved task anchor, so the post-compaction recap names the current task, and starts one detached dream cycle. It does not index the transcript: the file survives the compaction, and the daemon's watcher and the Stop worker index it, so nothing waits on embeddings while Claude Code compacts. Opt out of the dream with `CSR_NO_DREAM_ON_COMPACT=1` or `CSR_NO_DREAMING=1`.

## PostCompact: Keep Claude Code's Summary

Fires after a compaction. Stores the summary Claude Code wrote, once, as derived context. It is recalled as context and is never a source that a claim is checked against.

## PostToolUse — File Edit Tracking

Records files edited with deduplication. Enables `csr_search_by_file`. It does not index the transcript, so an edit never waits on embeddings; the daemon's watcher and the Stop worker do that.

## Code Map: Which Directory a Session Works In

SessionStart and PostToolUse each write one row for the session's working directory (one indexed upsert, 100 ms busy timeout, never a parse). The daemon maps the repos with a row from the last 24 hours (`CSR_CODEMAP_ACTIVE_HOURS`). PostToolUse also queues the edited code file with one insert, so the map picks it up without a parse in the hook.

Repos in `~/Desktop`, `~/Documents`, `~/Downloads`, iCloud Drive and `/Volumes` are different. The daemon never opens them, so the macOS privacy prompt never fires for it. A session working there has already opened the folder, so its own child process maps the repo:

- SessionStart starts one detached `csr-engine codemap scan --repo <cwd> --session`.
- PostToolUse queues the edited file and starts one detached `csr-engine codemap drain --repo <cwd>` unless one is running. The drain waits 5 seconds, rescans the queued files, repeats while new edits arrive and exits (after nine minutes it hands off like an idle drain: it releases its lock and looks at the queue once more, so a late edit is never dropped).

Both children have no stdio and the hook never waits for them. Nothing watches a protected folder, so an edit made outside Claude Code is not seen until the next session event in that repo. `CSR_NO_CODEMAP=1` turns all of this off.

## Error Handling

Every hook is wrapped in a catch-all. If anything fails, it logs and returns Ok — **never blocks Claude Code**.

## Install

```bash
csr-engine hook install --apply
```
