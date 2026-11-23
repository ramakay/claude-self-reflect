---
title: Features
---

Self-Reflect lets Claude look back at its own work: what it did, what it settled, what is still open, and where code came from. Underneath sits a local index of your sessions. That index is infrastructure. The features below are what Claude does with it.

## One binary

`csr-engine` is a single Rust binary. There is no Python, no Docker and no vector database to run. Storage (SQLite), search, embeddings, the MCP server, the hooks and the companion pane all run inside it. See [Architecture](#/docs/architecture).

## What it reads

| Source | How it gets in | Notes |
|--------|----------------|-------|
| Conversations: `~/.claude/projects/<project>/*.jsonl` | Watcher import | The primary corpus |
| Subagent sidechains: `<session>/subagents/agent-*.jsonl` | Watcher import | Filed under the real project; the parent session wins over the child in search |
| Tasks: `~/.claude/tasks/<session>/` | Stop hook | Task state sets the episode outcome. Open tasks mean "partial", never "success". A completed task that matches a still-open verdict becomes a proposal you confirm with `csr_resolve` |
| Plans: `~/.claude/plans/*.md` | Daemon, every 30 minutes | Linked to the conversation that produced them. The origin conversation outranks its plan in search. A plan's age is its file time |
| Codex sessions: `~/.codex/sessions/**/rollout-*.jsonl` | Daemon and setup | Optional. Detected automatically. Nothing to set up |
| Session history: `~/.claude/history.jsonl` | Daemon, every 10 minutes | A registry of sessions. It is never embedded or injected. `status` reports its coverage |

Memory files and the paste cache are not indexed. That is deliberate: indexing them would feed Self-Reflect its own output.

If Claude Code renames an internal format, the `aux.schema_misses` field of the `csr-engine status` JSON shows the parse failures.

## Recap at session start

On startup and resume, SessionStart injects one short paragraph, but only when the last session left evidence: intent, what got done, what was settled, what is open and the next step. Each clause drops when there is no evidence. `Next:` is never invented. This path makes no model call. With no evidence, the older `CSR CONTINUUM` and `LAST` lines appear instead.

A separate learnt line lists what Self-Reflect stored for the previous session: notes, verdicts you confirmed and anchors a commit retired.

`CSR_NO_RECAP=1` turns the recap off.

## Dreams

Dreaming runs in the background and is on by default. A dream checks old conclusions against your git history. When a conclusion no longer holds, search marks it `[stale anchor]` or `[evolved]` and cites the commit.

- `CSR_NO_DREAMING=1` stops the daemon's dream cycle and the companion's looks, even when Dreams is on in `/config`. The Dreams row in `/config` stops the companion's looks only. The companion reads the variable from the environment Claude Code runs in. The daemon reads it from its launchd agent, and setup copies the `CSR_*` variables of the environment it runs in into that agent, so a variable you export later reaches the companion but not an installed daemon until you run setup again.
- `CSR_DREAM_INTERVAL_SECS` changes the six-hour default.
- `csr-engine dreams` prints this week's dream for each project. `csr-engine dream` runs one cycle.

Dream threads call a model through your own Claude Code login. See [Installation](#/docs/installation) for what calls `claude -p` and how to switch each caller off.

## Receipts and compaction

Claude Code compacts long sessions. Self-Reflect keeps what compaction drops.

- PreCompact refreshes the recap anchor. PostCompact stores Claude Code's own compaction summary once, as derived context.
- After a compact, SessionStart shows a compact receipt.
- `csr_transcript` reads a past or deleted session: compact receipts, prompts, tools, files, errors, slices and grep.
- `csr-engine mirror` keeps scrubbed, compressed copies of your transcripts after Claude Code deletes the originals.

The recap states a clause only when it has evidence for it. Dream notes in search results cite the commit behind them.

## Code map

`csr-engine codemap` builds a per-worktree map of the definitions in supported source files in the repos your sessions work in. Map one by hand with `csr-engine codemap scan --repo <path>`. `csr_search_by_file` and `csr_code_graph` then answer for code no conversation touched. `CSR_NO_CODEMAP=1` turns it off.

## Companion pane

The companion plugin shows what Self-Reflect handed Claude and which code changed since your past sessions. It needs Claude Code 2.1.296 or later. Inside the [herdr](https://herdr.dev) terminal multiplexer it is a pane beside the session that opens by itself. Without herdr it is a panel inside Claude Code that you open with `/csr`. `csr-engine setup` installs the plugin after you confirm. `csr-engine setup --no-companion` skips it. See [Choose your view](#/docs/companion-views). For what starts a dream look, what it costs and how to stop it, see [How the companion thinks](#/docs/how-the-companion-thinks).

## MCP tools and hooks

Sixteen MCP tools: search, activity, and storage. See [MCP Tools](#/docs/mcp-tools). Eight hooks fire on seven Claude Code events and never block a session. See [Hooks](#/docs/hooks).

## Kill switches

| Variable | Effect |
|----------|--------|
| `CSR_NO_DREAMING=1` | No daemon dream cycle and no companion looks (the `/config` Dreams row stops the looks only) |
| `CSR_NO_RECAP=1` | No recap paragraph |
| `CSR_NO_AI_NARRATIVES=1` | No `claude -p` narratives or briefings, and no model calls for dreams (dream threads, strategy dreams, backfill). The local dream cycle and the companion's looks keep running |
| `CSR_NO_CODEMAP=1` | No code map |
| `CSR_NO_DAEMON=1` | Setup does not install or start the daemon |

## Privacy

Search, storage and embeddings run on your machine. There are no accounts and no telemetry. See [Privacy & Security](#/docs/privacy).
