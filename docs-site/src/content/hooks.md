---
title: Active Memory Hooks
---

## The Hook Architecture

Six hooks fire at specific moments in the session lifecycle. Each performs its work in milliseconds and either injects context or stores data.

This is what separates CSR from passive memory tools. **Context finds you.**

## Session Lifecycle

![Session lifecycle hook timeline — six hooks fire at key moments, injecting context and storing data](/claude-self-reflect/images/hooks-lifecycle-2.png)

## SessionStart — Past Context Injection

Fires when you start a conversation. Injects one recap paragraph and, on `startup` and `resume`, one `learnt` line. No LLM writes either.

- **Recap**: a single causal paragraph about your last session. Each clause drops out when there is no evidence for it, `Next:` is never invented, and every settled claim carries a receipt.
- **Learnt line**: what CSR stored for the previous session: memory notes, `csr_resolve` verdicts you confirmed, anchors retired by a commit. At most 320 bytes.
- **Fallback**: if the recap feeds fail, SessionStart prints the older fragments (CONTINUUM, EPISODE INDEX) byte for byte.

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

**Intent routing (v9.2, extended v9.3)** — a semantic classifier (exemplar embeddings over the same local MiniLM model, no extra model shipped) detects three intents:

| Route | Intent | Threshold | Action |
|-------|--------|-----------|--------|
| A | Continue / StateRecall ("pick up where we left off", "what were we doing?") | 0.60 / 0.55 | Inject the matching episode's state directly |
| A | Explore ("how does the ring navigation work?") | 0.55 | Inject a **CODE MAP** — file pointers from the conversation that built the feature, plus a ready-to-run recall call — before the agent re-maps the codebase |
| B | Topic correlation | recency-weighted, 7-day half-life | Match prompt to a past episode; surface it as a pickup pointer |

Everything else falls through to multi-signal scoring:

| Signal | Weight |
|--------|--------|
| Semantic similarity | 50% |
| Recency | 20% |
| File overlap | 20% |
| Error pattern match | 10% |

500-token budget. Skips trivial prompts and slash commands.

## Session story — kept current at each compaction

A session's story is the short account later sessions read about it. After each compaction, SessionStart starts a detached `csr-engine generate-story --from-compaction`, which rebuilds the story from the summary Claude Code writes as it compacts. The new story replaces the old one, and CSR records how many of the session's messages it covers.

By default CSR extracts the story from that summary: what was asked, then where the work stood. There is no model call. The summary has a blind spot, though. Claude Code keeps the last few messages verbatim after it, so the summary never sees how the work ended.

Set `CSR_STORY_REWRITES=1` and Haiku writes the story instead. It reads the summary for the session's whole arc and the last 20 messages for where it stood. At SessionEnd it runs again, if you asked something since the last story, so the ending reaches the story too. Each is one metered `claude -p` call, about $0.003. On five long sessions, two blind judges ranked this telling first. The other four were the free extract, Haiku over a sample of messages, Haiku rewriting the earlier story, and Haiku over the summary alone.

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

## PreCompact: Import, Refresh, Dream

Fires before a compaction. Imports the transcript so it stays searchable after the compact, refreshes the saved task anchor so the post-compaction recap names the current task, and starts one detached dream cycle. It never blocks the compaction. Opt out of the dream with `CSR_NO_DREAM_ON_COMPACT=1` or `CSR_NO_DREAMING=1`.

## PostToolUse — File Edit Tracking

Records files edited with deduplication. Enables `csr_search_by_file`.

## Error Handling

Every hook is wrapped in a catch-all. If anything fails, it logs and returns Ok — **never blocks Claude Code**.

## Install

```bash
csr-engine hook install --apply
```
