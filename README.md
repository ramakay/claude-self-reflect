# Self-Reflect

<div align="center">

<img src="docs-site/public/favicon.svg" alt="Self-Reflect" width="80" height="80" />

[![npm](https://badge.fury.io/js/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect) [![downloads](https://img.shields.io/npm/dm/claude-self-reflect.svg)](https://www.npmjs.com/package/claude-self-reflect) [![MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT) [![Release](https://github.com/ramakay/claude-self-reflect/actions/workflows/release.yml/badge.svg)](https://github.com/ramakay/claude-self-reflect/releases/latest) [![Claude Code](https://img.shields.io/badge/Claude%20Code-6B4FBB)](https://github.com/anthropics/claude-code) [![MCP](https://img.shields.io/badge/MCP-FF6B6B)](https://modelcontextprotocol.io/) [![Rust](https://img.shields.io/badge/Rust-000000?logo=rust)](https://github.com/ramakay/claude-self-reflect/tree/main/csr-engine) [![Local First](https://img.shields.io/badge/Local-4A90E2)](https://github.com/ramakay/claude-self-reflect) [![stars](https://img.shields.io/github/stars/ramakay/claude-self-reflect.svg?style=social)](https://github.com/ramakay/claude-self-reflect/stargazers)

**Claude reflects on its own work, and shows the receipts.**

Self-Reflect is Claude reflecting on its own work: a recap when a session starts, receipts for what got done, dreams that check old conclusions against your git history, and a compact that keeps its evidence. A pane beside the session shows how Claude is doing.

A memory layer sits underneath as plumbing. It is one Rust binary (about 138 MB) with embedded SQLite, no database server, no containers, no Python and no API keys required.

[Install](#install) | [What we're working on](#what-were-working-on) | [How It Works](#what-it-does) | [MCP Tools](#mcp-tools) | [Paper](docs/plans/annaswamy-2026-similarity-drowns-intent.pdf) | [FAQ](https://ramakay.github.io/claude-self-reflect/#/docs/troubleshooting)

> **v10.1.0: Claude reflects on its own work**
> When there is evidence to state (a commit, a finished or open task, a next step, a verdict, a correction, a dream finding), a session that starts or resumes opens with one recap paragraph (intent, what got done, what was settled, what is open, the next step) instead of a pile of fragments. Without evidence it opens with the older `CSR CONTINUUM` and `LAST` lines. A `learnt` line follows only when CSR stored notes, verdicts or retired anchors for your last session. After a compact, you get a compact receipt: a pointer line and `csr_transcript` slices of the turns the summary replaced. Every clause carries a receipt or drops out.
> Dreaming checks old conclusions against your git history and marks a stale anchor with the commit that changed it. No LLM in that check.
> CSR also reads plans, task state, Codex sessions, subagent transcripts, memory-file metadata and the session registry, and ships a companion plugin with a pane beside the session.
> A live code map covers the definitions in supported source files in the repos your sessions work in, across main and every worktree, so CSR can answer for code no conversation touched. Map a repo by hand with `csr-engine codemap scan --repo <path>`.
> Local vector search, zero external dependencies. Gate at the release candidate, run with CI's `cargo test --locked --no-fail-fast` on 2026-11-18: 6,939 passed, 0 failed, 34 ignored. `cargo fmt`, `clippy -D warnings` and `actionlint` are clean, and the installer tests pass 58 of 58.
> [Release notes](CHANGELOG.md)

</div>

## Table of Contents

- [What it does](#what-it-does) — Claude reflecting on its own work
- [What we're working on](#what-were-working-on) — Measured numbers and open work
- [The Architecture](#one-binary) — How CSR solves it
- [The Pipeline](#the-pipeline) — Progressive enrichment
- [The Paper](#the-paper) — The research behind the retrieval design (preprint)
- [btw, these too](#btw-these-too): Smaller features that ship with it
- [Install](#install) — One-command install, consent-first activation
- [What You'll Ask](#what-youll-ask) — Natural language, no syntax
- [Performance](#performance) | [MCP Tools](#mcp-tools) | [Hooks](#hooks) | [CLI](#cli-reference)
- [AI Narratives](#ai-narratives-optional) | [Upgrading](#upgrading-from-v7x) ([10.x](#upgrading-within-10x)) | [Troubleshooting](#troubleshooting)

---

## What it does

Without a record, Claude starts every session not knowing what it finished last time, what it left open, or what you corrected. Self-Reflect keeps that record and makes Claude show the evidence.

- **Recap:** a session opens with one paragraph: intent, what got done, what was settled, what is open, the next step.
- **Receipts:** every clause cites a commit, a task, a verdict or a correction. A clause with no evidence drops out.
- **Dreams:** CSR checks old conclusions against your git history and marks a stale one with the commit that changed it. No LLM in that check.
- **Compaction:** after a compact, Claude gets pointers to the turns the summary replaced, and `csr_transcript` reads them back.
- **How is Claude doing:** the companion pane shows what Claude was told, what it missed and what you corrected.
- **Plumbing:** search over your past sessions (SQLite and a local vector index) sits underneath. Claude can also call it directly through the MCP tools.

No special syntax. Install once, and the recap and receipts show up on their own.

> **[Explore the full documentation →](https://ramakay.github.io/claude-self-reflect/)**

---

## One Binary

Everything runs locally in a single process from one binary of about 138 MB. No Docker, no database server, no API keys required.

- **SQLite** — storage for chunks, embeddings, enrichment state
- **HNSW** — in-memory vector index; `csr-engine eval` times the index lookup on your own data
- **FastEmbed** — 384-dim local embeddings
- **AST** — code-aware search across 6 languages

**8 hooks on 7 events** fire across the session lifecycle (SessionStart runs two: the recap and the async briefing). **16 MCP tools** for explicit search — including `csr_code_graph`, linking code symbols to the conversations that shaped them.

> **[Explore the full documentation →](https://ramakay.github.io/claude-self-reflect/#/docs/architecture)**

---

## The Pipeline

Three layers turn raw chunks into searchable context: the chunks themselves, extracted metadata, and optional AI narratives ([details](https://ramakay.github.io/claude-self-reflect/#/docs/enrichment)).

Higher quality context. Better decisions. Fewer tokens.

<br clear="both" />

> **[Explore the full documentation →](https://ramakay.github.io/claude-self-reflect/#/docs/enrichment)**

---

## The Paper

<a href="docs/plans/annaswamy-2026-similarity-drowns-intent.pdf">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs-site/public/images/card-04-paper-dark.svg" />
  <img align="right" src="docs-site/public/images/card-04-paper-light.svg" alt="Three-Trace Sagas — intent, deliberation, artifact" width="420" />
</picture>
</a>

CSR's retrieval design is documented in a research preprint, measured on the system's own development history:

> **Similarity Drowns Intent: Three-Trace Sagas and Reinstatement Recall for Provenance in Agentic Software Construction**
> 2026 — [PDF](docs/plans/annaswamy-2026-similarity-drowns-intent.pdf)

Ask a memory system "why did we drop Qdrant?" and cosine similarity returns a recording of you asking that question earlier, at 0.984 similarity. The decision itself scores lower than its own echoes. The paper measures this failure, the multi-hop walk built to counter it (+53% and +47% ground-truth session coverage over one-shot kNN on two corpora, pre-registered gates, cross-vendor judging), and a second finding that fell out of evaluation: a self-recording memory system ingests its own eval dialogue and drowns the answers it's being tested on. It also keeps its negative results — a pre-registered ratification-weighting hypothesis died at ρ≈0 and forbade an entire staleness design.

<br clear="both" />

---

## btw, these too

Smaller features that ship in this release. Each line says what you get.

### What Claude sees by itself

- **Recap paragraph:** on startup and resume, one paragraph on where you left off (intent, what got done, what was settled, what is open, what you corrected, the next step), only when the last session left evidence (a commit, a finished or open task, a next step, a verdict, a correction, a dream finding). Without any, the session starts with the older `CSR CONTINUUM` and `LAST` lines.
- **Compact receipt:** after a compact, the new context starts with a pointer line and `csr_transcript` slice pointers for the turns the summary replaced (`CSR_NO_COMPACT_POINTER=1` turns it off).
- **Learnt line:** only at startup or resume, and only when CSR stored something for your last session: memory notes, verdicts you confirmed, anchors a commit retired. Each clause carries a receipt. When CSR stored none of these, there is no line. At most 320 bytes.
- **Worktree and subfolder recall:** start Claude in a worktree or a subfolder and you get the repo's history, not an empty memory.
- **Resume lineage:** after `claude -c` or a resume, CSR names the session you continued and treats it as the last session.
- **Dream line at prompt time:** when your prompt names a symbol or file a dream concluded on, one line appears. The same dream never repeats.
- **File history on mention:** name a file in your prompt and its edit history comes along.
- **Trust labels:** model-written context is labelled `[briefing: model-derived, trust=...]`, so Claude knows what is a guess.
- **No self-echo:** CSR text that comes back as input is labeled `[CSR text]`. It stays searchable, ranks below your own words, and is kept out of recaps and briefings. `csr-engine status` counts what it labeled.

### It keeps what Claude Code throws away

- **Transcript mirror:** a scrubbed, zstd-compressed copy of every main and subagent transcript, kept after Claude Code deletes the original. Manage it with `csr-engine mirror sync|status|purge --yes`.
- **Queued prompts:** what you type while Claude is still busy is indexed and searchable like any other prompt.
- **Codex sessions:** if `~/.codex/sessions` exists, your Codex work is searchable next to Claude's. Nothing to set up.
- **Codex can ask too:** when the `codex` CLI is installed, `csr-engine setup` registers the same MCP server with Codex (`codex mcp add`), so Codex can search the same history. An entry you customized or disabled is left alone. Already set up? Run `csr-engine setup --codex-only`. `CSR_NO_CODEX_MCP=1` skips it.
- **Memory file catalogue:** CSR knows which Claude memory files exist (metadata only, never embedded).

### Privacy built in

The timing log holds timing, counts, sizes and scores. Each prompt detail holds only its character count and a salted 8-hex hash, never prompt text. The salt stays in a local `0600` file. At daemon startup, a one-time scrub removes prompt and old session-start previews from the current and rotated logs. For old prompt entries, the count and hash cover only the saved preview; if the salt is unusable, the scrub writes only the count. New prompt detail lines are omitted if the salt is unusable. Set `CSR_NO_TIMING_DETAIL=1` where Claude Code starts to turn off prompt detail lines; timing lines stay on.

- **`<private>` tags:** wrap text in `<private>...</private>` and it never reaches memory or the mirror.
- **Secret redaction:** API keys and tokens become `[secret redacted: <kind>]` before they reach the index or the mirror. `CSR_NO_SECRET_SCRUB=1` turns it off.
- **Private companion storage:** the companion's data folder is `0700`, its files `0600`, and tokens and keys are redacted from the text it keeps.
- **Loopback-only journal:** the dream journal binds to 127.0.0.1 only. Its POSTs need a CSRF token, and its pages run no scripts (CSP `default-src 'none'`).

### Commands you can run yourself

- **`csr-engine transcript <session> stats|prompts|yourasks|tools|files|errors|slice|grep`:** read any session's prompts (or only what the human asked), tools, files and errors without hand-rolled `jq`.
- **`csr-engine lessons --project P`:** turns corrections you repeated across sessions into candidate CLAUDE.md lines, each with receipts.
- **`csr-engine dreams`:** prints this week's dream per project in the terminal. No browser needed.
- **`csr-engine dream --report`:** writes a self-contained HTML dream journal you can keep or share.
- **`csr-engine ratify <conv-id>`:** scores the sessions you pick right now, without waiting for the queue.
- **`csr-engine bench --format longmemeval|agentmemory`:** reproduces public memory benchmarks.
- **`csr-engine telemetry --tui`:** a live dashboard of hook latency, startup and enrichment health.
- **`csr-engine backfill-stories`:** writes stories for your old sessions.
- **`csr-engine rebuild-index`:** repairs the search index with no daemon running.
- **`csr-engine status --swiftbar`:** macOS menu-bar health, via `scripts/csr-status.30s.sh`.
- **`csr-engine status --deep`:** forces a fresh full integrity check (normally cached).
- **`csr_transcript` (MCP):** the 16th MCP tool, structured reads of any session.

### The dream journal in your browser

Open `http://127.0.0.1:7373/`. `CSR_JOURNAL_PORT` moves the port and `CSR_NO_JOURNAL_SERVER=1` turns the server off.

- **Home:** the week's dreams.
- **`/board`:** open items at a glance. **`/corrections`:** your corrections over time.
- **`/dream/{id}`:** the code's shape before and after, as an AST diff.
- **Act without Claude:** resolve or dismiss a dream in the browser.
- **Scripting:** `/api/dreams` (JSON) and `/healthz`.

### Companion pane, beyond the five axes

The cards and keys below (`i`, `l`, `m`) are the herdr pane's; the band, the guard and the compaction items come from the plugin and work in both views. Without herdr, type `/csr` in Claude Code for a panel with a smaller card set; see [Two ways to see the companion](#two-ways-to-see-the-companion). Picture cards need a terminal that draws kitty graphics (Ghostty, kitty, WezTerm, Warp). In macOS Terminal.app the herdr pane draws its cards as text; `CSR_COMPANION_PIXELS=0` asks for text anywhere, and `CSR_COMPANION_PIXELS=1` forces pictures on (for a herdr server started in Terminal.app and attached from another terminal).

- **How Claude read you:** spots shorthand Claude misread, such as "dknr" read as "done".
- **Your rules:** corrections you typed, kept in your own words, never Claude's.
- **CSR told Claude, Last session, Todos left:** cards showing the injected recap, the previous session and carry-over todos.
- **Cockpit gauges:** context, payback, cache and your account quotas.
- **`i` page:** session info, including how many times CSR handed Claude something.
- **Changed since (`l`):** every commit that moved code your past sessions worked on.
- **Shift Map (`m`):** shift, risk, decision and regression rows, each grounded in receipts.
- **Heads up band:** a note when Claude opens a file that has history.
- **You Should Know:** the same idea as Claude Code's own "You should know", fed from your memory.
- **Terms coach:** offers a `## Terms` line for your CLAUDE.md that teaches Claude your shorthand.
- **Push guard:** opt-in. Blocks `--no-verify` and pushes to protected branches. A guard rail, not a security boundary.
- **"Later" survives compaction:** threads you deferred are carried across compaction, and subagents keep their task brief through their own compaction.
- **autoCompact:** `off`, `suggest` (default) or `auto`.
- **Status line retired:** the pane's masthead shows context and limits. `csr-engine hook install --apply` puts back a status line an older version wrapped, and `/csr statusline` now only says so.
- **Phone layout:** the pane has a layout for a narrow terminal.
- **Road trial line (maintainers):** a trial spec with `"track": "road"` adds one line to the CSR performance card; off by default, switch on with `CSR_PANE_ROAD=1` or `"road": true` under the `csr-companion` entry of `pluginConfigs` in `~/.claude/settings.json`.
- **One dreamer per session, crash guard:** a lease stops two dreamers on one session, and a broken edit to the pane does not kill it.

### Quiet MCP niceties

- **Argument completions:** project, file, session, time range and concept complete as you type.
- **Background tasks:** heavy tools run as MCP Tasks, so long calls never block.
- **Upgrade notice:** after an upgrade, results tell you to restart a session that still runs the previous build.
- **Keyword fallback:** results come back as `[keyword]` matches when the vector path is down.
- **Health resource:** `status://system-health`, readable from inside Claude.
- **Confirmations:** you confirm before a reflection over 2,000 characters is stored, and `csr_resolve` verdicts are confirmed against a SHA-256 of the payload.

### It looks after itself

- **Hands-free upgrades:** the daemon exits when its binary is replaced, so launchd restarts it on the new build.
- **Recall canary:** a known-item recall check runs on a six-hour loop.
- **Catch-up sweep:** every 30 minutes, so no transcript is missed.
- **Installer checks:** it warns when an older `csr-engine` earlier on your PATH would win, and restarts the daemon so an upgrade takes effect.

### Dials

- **Kill switches:** `CSR_NO_RECAP`, `CSR_NO_LEARNT`, `CSR_NO_DREAMING`, `CSR_NO_DREAM_INJECT`, `CSR_NO_DREAM_ON_COMPACT`, `CSR_NO_MIRROR`, `CSR_NO_SECRET_SCRUB`, `CSR_NO_STORY_MAP`, `CSR_NO_CODEX_IMPORT`, `CSR_NO_CODEX_MCP`, `CSR_NO_HEALTH`, `CSR_NO_JOURNAL_SERVER`, `CSR_NO_AI_NARRATIVES`, `CSR_NO_TIMING_DETAIL`. In herdr the pane also follows herdr's event socket; `CSR_PANE_HERDR_EVENTS=0` switches that off.
- **More kill switches**, each read by the code and set to `1` to turn its feature off:
  - `CSR_NO_ANTI_PATTERNS`: the prompt hook skips the "failed approach" search and its second embedding.
  - `CSR_NO_ASSERTIONS`: search skips the assertions stage.
  - `CSR_NO_CODEMAP`: the code map is off.
  - `CSR_NO_COMPACT_POINTER`: a compact start gets no line naming the turns the summary replaced.
  - `CSR_NO_CONTINUED`: the pane reads only the session's own transcript, not the sessions it continued.
  - `CSR_NO_DAEMON_MALLOC_TUNING`: on macOS the daemon keeps malloc's large-block cache on (no restart with `MallocLargeCache=0`).
  - `CSR_NO_DAEMON_PROXY`: an MCP server searches in its own process instead of through the daemon.
  - `CSR_NO_ECHO_FILTER`: recall keeps chunks that only repeat the question.
  - `CSR_NO_EPISODES`: recall leaves out session episodes (wins over `CSR_EPISODES=1`).
  - `CSR_NO_EPISODE_VECTORS`: episode recall uses keywords only, no episode vectors.
  - `CSR_NO_GRAPH_DRAIN`: the daemon's code-graph drain loop is off.
  - `CSR_NO_GRAPH_SLICES`: the prompt hook skips code-graph slices.
  - `CSR_NO_INTENT_CAPTURE`: the Stop hook records no intent events.
  - `CSR_NO_MALLOC_TUNING`: on macOS the relay, the MCP server and the watching pane start with the default allocator (no restart with `MallocLargeCache=0` for the relay, `MallocSpaceEfficient=1` for the other two). A value of either variable you set yourself is kept.
  - `CSR_NO_MEMORY_REGISTRY`: the daemon does not scan Claude Code's memory folders.
  - `CSR_NO_NATIVE_WARN`: no warning when Claude Code's own auto-compact would run first.
  - `CSR_NO_PROMPT_GATE`: turn off the relevance floor and minimum item count for the final content block.
  - `CSR_NO_PROVENANCE_BACKFILL`: the daemon does not backfill chunk provenance.
  - `CSR_NO_REACTION_SHADOW`: the daemon's reaction-shadow scoring loop is off.
  - `CSR_NO_RECALL_LANES`: every read of a recall pass runs on one connection.
  - `CSR_NO_SYNTHETIC_FILTER`: recall keeps eval and synthetic content.
  - `CSR_NO_TASK_RESOLUTION`: the Stop hook proposes no resolutions from completed tasks.
  - `CSR_NO_VALIDITY_PARTITION`: search does not demote superseded or obsolete results.
  - `CSR_NO_VECTOR_GEN`: search ignores the vector generation and builds the index-v3 chunk graph from SQLite when it loads (minutes and about 1.5 GB of memory on a large corpus). The daemon stops regenerating, and no longer keeps that graph on disk once a generation serves.
  - `CSR_NO_YSK_BRIEF`: the dreamer leaves "You should know"'s prompt alone.
- **Dream cost:** `CSR_DREAM_EFFORT=less|balanced|max` and `CSR_DREAM_BUDGET`.
- **Dream timing:** `CSR_DREAM_INTERVAL_SECS`, `CSR_DREAM_IDLE_MINS` (default 30), `CSR_DREAM_FLOOR_HOUR` (default 3).
- **Search path:** `CSR_SEARCH_MODE=hybrid|vector|fts`.
- **CPU caps:** `CSR_EMBED_THREADS`, `CSR_TOKIO_WORKERS`.

---

## What we're working on

These are engineering items with numbers, not promises. Seven items; "not measured yet" means no number exists. Every figure is as of 2026-11-18, from the installed 10.1.0 binary on the maintainer's machine: one machine, one sample per figure, times in Pacific.

**The machine these numbers come from**

| What | Value | How measured |
|---|---|---|
| Corpus | 1,083,965 chunks, 6,322 conversations, 345 projects, 14.1 GB database | `csr-engine status`, 09:29 |
| Prompt hook (`prompt-submit`) | p50 77 ms, p99 520 ms, 882 calls | `hook-timing.log`, last 7 days |
| `session-start` hook | p50 102 ms, p99 1,354 ms, 191 calls | same log and window |
| Detached Stop worker (`stop-work`) | p50 1,228 ms, p99 6,609 ms, max 46,081 ms, 1,722 calls | same log and window; the session does not wait for it |
| Transcript mirror | 19,193 session transcripts whose original JSONL is gone; the mirror holds the only copy | `csr-engine mirror status` plus a check of each mirrored path on disk |

**1. Daemon memory growth under load**
- Now: the daemon's resident memory was 1.72 GiB at 09:29 PST (9 h 03 min of uptime) and 1.09 GiB at 11:18 PST (10 h 52 min, same pid), read with `ps -o rss`. That is 2 samples, not a load test, and not enough for a growth series.
- Target: RSS within 10% of its 1-hour value after a 24 h replay of real history on the time-travel soak rig.
- Doing: add an RSS series to the daemon's history and to `status`, and run the soak each release.

**2. Known-item recall (top-5)**
- Now: 14 of 18 probes found in the top 5 (78%). `csr-engine eval --full` re-measured it at 09:31 and got the same 14 of 18 on the same frozen sample, not a fresh draw. All 4 misses ranked below 5th.
- Target: 16 of 18 (89%) or better on a fresh sample drawn each release.
- Doing: draw a fresh sample per release and inspect the 4 misses.

**3. Index lookup p95**
- Now: 18.47 ms p95 against the 1 ms target of `csr-engine eval --full` (50 timed lookups on the live index of 1,083,965 chunks, 09:31).
- Target: under 1 ms p95 on the live index.
- Doing: profile the lookup path on a 1M-chunk index.

**4. Relevance labels too often "weak"**
- Now: not measured yet. No label is stored, and `eval --full` does not print a label distribution.
- Target: log the label for every result, and under 25% of correct rank-1 hits labelled weak.
- Doing: store the label per retrieval event and report it in `eval --full`.

**5. `csr_quick_check` false positives**
- Now: not measured yet.
- Target: a labelled set of 30 queries, with false positives under 10%.
- Doing: build the labelled set and add it to `eval`.

**6. Status surfaces disagree**
- Now: 4 cases in one read at 09:29. `healthy` is true while `state` is "degraded". The chunk count differs by surface (1,083,965, 1,083,901 or 1,084,116). "Imported" reads 99.23% (515 of 519 files) next to 758 of 1,146 registry sessions, two different denominators. Dream spend reads as current but is lifetime and all dated 2026-08-14.
- Target: 0. One source per count, and every percentage names its denominator.
- Doing: route every surface through one status snapshot.

**7. Thin MCP tools: `csr_code_graph`, `csr_search_insights`, `get_session_learnings`**
- Now: not measured yet.
- Target: each returns a non-empty answer with receipts on a 10-query test set.
- Doing: write the test set, then deepen the three tools.

---

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

Downloads the binary (SHA256-verified), then asks before activating. Setup registers the MCP server, installs 8 hooks on 7 events, imports your conversations, installs the companion plugin and, on macOS, installs and starts the background daemon (a launchd agent that runs dreams and enrichment; `CSR_NO_DAEMON=1` skips it). It only runs with your consent. Restart Claude Code after.

Non-interactive installs never activate on their own: set `CSR_AUTO_SETUP=1` to opt in, or run `csr-engine setup` yourself.

To pin a release, pass `--version` (`curl -fsSL .../install.sh | sh -s -- --version v10.1.0`) or set `CSR_VERSION=10.1.0`. To install from files you already have, set `CSR_TARBALL=/path/to/csr-engine-<target>.tar.gz` with `checksums.txt` beside it (or `CSR_CHECKSUMS=/path`), or `CSR_RELEASE_BASE_URL` to a directory or URL that holds both files. A local tarball, directory or file:// source makes no network call, and the checksum is always verified: without a checksums file the install stops.

Upgrading from 9.5.8 or a 10.x install? Install the new binary, then run setup again through the installer: `claude-self-reflect setup` with npm, or answer yes at "Run setup now?" in the curl installer. That applies new hooks and launch agent settings. Restart Claude Code after. See [Upgrading](#upgrading-within-10x).

**What calls `claude -p`.** Some features run the Claude Code CLI in print mode (`claude -p`) as a child process. It runs under your own Claude account, so it uses your plan's usage (or your API key's billing, if that is how Claude Code is set up); CSR sends nothing to any other service for this. The callers are the `session-briefing` SessionStart hook (on startup and resume; once a briefing is stored for a project, there is no new call for it for 30 minutes), session narratives and stories, the daemon's ratification pass, and dreams. `CSR_NO_AI_NARRATIVES=1` turns all of them off, the briefing included. `CSR_NO_DREAMING=1` stops dreams only and `CSR_NO_RATIFICATION=1` stops ratification only.

How many calls to expect, and what paces them:

| Caller | When it calls | Typical count | Pace and switch |
|---|---|---|---|
| Session briefing | a session start or resume | about one per session; none for a project briefed in the last 30 min | `CSR_NO_AI_NARRATIVES=1` |
| Ratification (daemon) | once per conversation | about one per session you ran | at most 4 an hour (`CSR_RATIFICATION_PER_HOUR`), only conversations active in the last 7 days (`CSR_RATIFICATION_DAYS`, 0 for all); `CSR_NO_RATIFICATION=1` |
| Narratives and stories | a finished session, a story refresh | a few per active session | `CSR_NO_AI_NARRATIVES=1` |
| Dreams (daemon) | one pass per cadence | one pass per 6 h at most, within the budget setup shows | `CSR_DREAM_INTERVAL_SECS`, `CSR_NO_DREAMING=1` |

A fresh install does not work through your whole imported history at once: ratification makes at most 4 calls in its first hour and leaves conversations older than a week alone (`csr-engine ratify` still takes any). `csr-engine status` shows the day's count (`AI <n>c`), and the `narrative_usage` table keeps every call. To change a pace or a switch for the daemon, run `csr-engine setup` with the variable set: setup copies `CSR_*` variables into the launch agent.

The default model differs by caller, and each has its own variable. Each chain tries your variable first (if set), then the caller's default, then the Claude Code CLI's own default (no `--model`).

| Caller | Default model | Variable that changes it |
|--------|---------------|--------------------------|
| Briefing, session narratives and stories, `csr-engine dream backfill`, `csr-engine dream --report` headlines | `haiku` | `CSR_NARRATIVE_MODEL` |
| Ratification (daemon) | `haiku` | `CSR_RATIFICATION_MODEL` |
| Dreams: dream threads and the plan pass (`CSR_DREAM_THREADS=1`), `csr-engine dreams` | `sonnet-5` (the `balanced` tier) | `CSR_DREAM_THREAD_MODEL` names a model; `CSR_DREAM_EFFORT` (`less`, `balanced` or `max`) picks `haiku-4`, `sonnet-5` or `opus-5`, and the model variable wins |

`csr-engine status` shows the first model dreams try in `dream.effort.model`. Set the variable where Claude Code starts (hooks read it there) and run setup with it set so the daemon's agent keeps it. Defaults are unchanged.

| Platform | Support |
|----------|---------|
| macOS (Apple Silicon) | Prebuilt binary |
| Linux x86_64 / WSL | Prebuilt binary |
| Linux ARM64 | Prebuilt binary |
| macOS (Intel) | Not supported (no prebuilt binary; the ONNX runtime has no x86_64-apple-darwin build) |

<details>
<summary>Alternative: npm</summary>

```bash
npm install -g claude-self-reflect
claude-self-reflect setup   # activation is a separate, explicit step
```

By default `npm install` only downloads the checksummed binary — it does not touch `~/.claude` or index conversations. Activation happens when you run `claude-self-reflect setup`, or set `CSR_AUTO_SETUP=1` during install to opt in. That command runs the binary npm downloaded, so it works before `~/.local/bin` is on your PATH.

On macOS, an npm upgrade that replaces the binary also restarts an already loaded daemon (`launchctl kickstart -k`), even without `CSR_AUTO_SETUP`. It does not touch `~/.claude`.

</details>

<details>
<summary>Build from source</summary>

```bash
git clone https://github.com/ramakay/claude-self-reflect.git
cd claude-self-reflect/csr-engine
cargo build --release
cp target/release/csr-engine ~/.local/bin/
csr-engine setup
```

</details>

### The companion pane (optional)

A Claude Code plugin, `csr-companion`. It shows what Self-Reflect handed Claude, the agents and Codex runs in flight, the dreamer's "what if...?", and a compaction coach. It needs Claude Code 2.1.296 or later.

#### Two ways to see the companion

| Where you run `claude` | What you get | How it opens |
|---|---|---|
| Inside [herdr](https://herdr.dev) (`brew install herdr`) | A pane to the right of Claude with the full cards. Pictures in Ghostty, kitty, WezTerm or Warp; text cards in Terminal.app. | By itself at session start in a project folder; `/csr` opens it again |
| A plain terminal, no herdr | A panel inside Claude Code with a smaller card set | Type `/csr`. In the 10.1.0 clean-machine test it did not open by itself. |

Setup installs one plugin for both and does not choose between them: the plugin checks for herdr at each session start and each `/csr`. To switch, start `claude` inside or outside herdr; no reinstall. `claude plugin disable csr-companion@claude-self-reflect` turns the whole companion off. Setup, checks and troubleshooting: [Choose your view](https://ramakay.github.io/claude-self-reflect/#/docs/companion-views).

It runs inside `csr-engine` itself: the plugin is one generated file that starts `csr-engine companion serve`, so it needs no Node, TypeScript or Python. `csr-engine setup` writes that plugin and installs it through Claude Code's own plugin CLI, after the same consent. `csr-engine setup --no-companion` skips it. If setup cannot install it, it prints why, and you can retry with one command:

```bash
csr-engine companion plugin install
```

**The cards the herdr pane draws, top to bottom:**

| Card | What it shows |
|------|---------------|
| **Dreams** | The dreamer's "What if...?" question from your past sessions, with its receipt. |
| **Context** | How full the context window is, the split between tools and conversation, the cache, and the "CSR handed over" row for what the session start gave Claude. |
| **Agents** | Subagents and Codex runs in flight, and the ones that just finished. |
| **What confused the agent** | Asks Claude misread or skipped, one row each. |
| **Same page?** | Replaces the list above when the dreamer has a reading: how many asks it saw, how many Claude missed and how many were unclear, in the dreamer's words, with a message that puts Claude back on course. |
| **Should be a skill / memory / tool** | Asks or patterns you repeat that deserve a skill, a memory or a tool. |
| **Lost and found** | The newest thread of this session that went quiet or waits on you. A tap opens a handoff you can copy. |
| **Where the work stands** | The session story so far. A session with no story yet reads **This session**. |
| **CSR performance** | When a trial of the build is set up: its checks with pass and fail marks, and a clock and bar for the time left. |

In a short pane the lower cards fold into one stack row that opens card by card. Two pages open from the pane:

- **Monologue:** the session thinking out loud, as a timeline of threads.
- **Changes:** every commit that moved code your past sessions worked on.

<a id="what-youll-ask"></a>

<details>
<summary><strong>What You'll Ask</strong> — after install, just ask Claude naturally</summary>

- *"How did we solve re-renders on this component?"*
- *"What did we tell Joe about that commit?"*
- *"What were our frustrations with this approach?"*
- *"Where did we put the auth middleware config?"*

No special syntax. No commands. CSR finds relevant past context and injects it automatically.

</details>

<a id="performance"></a>

<details>
<summary><strong>Performance</strong> — what the built-in eval measures</summary>

| Metric | Value |
|--------|-------|
| **Cached startup** | Loads the saved index; the time grows with index size |
| **Vector index lookup (p95)** | Timed by `csr-engine eval` on your own index (target under 1 ms). It covers the lookup only, not a full recall call. |
| **Binary size** | About 138 MB (137,817,664 bytes, the installed binary on 2026-11-18) |
| **Embedding** | Local MiniLM model, 384 dimensions, no API call |

**Claims and how to recount them.** Run each command from the repository root. The output shown is what it prints on this tree. The three log rows (19 to 21) read `~/.claude-self-reflect/hook-timing.log` (set `LOG` to that path) and its rotated copy `hook-timing.log.1`. The log rotates at 10 MiB and keeps one older generation, so a window that has rotated lives only in `.1`; a command that reads `$LOG` alone prints blanks for it. The rows describe one machine and one dated window, and the upper time bound keeps the count stable as the log grows. When that window ages out of `.1` too, the rows can no longer be recounted on that machine. Rows 1 to 19 read source files only.

| # | Claim | Recount command | Prints |
|---|-------|-----------------|--------|
| 1 | Hooks on 7 events (8 hooks: SessionStart has two) | `grep -c -E '^            "(SessionStart\|SessionEnd\|PreCompact\|PostCompact\|Stop\|PostToolUse\|UserPromptSubmit)": \[' csr-engine/src/hooks/install.rs` | `7` |
| 2 | 16 MCP tools by default | `grep -n 'const DEFAULT_TOOLS' csr-engine/src/mcp/mod.rs` | `usize = 16` |
| 3 | 17 tools are defined; one is measurement-only, behind `CSR_MCP_HOOK_TOOL=1` | `grep -c '^    #\[tool(' csr-engine/src/mcp/mod.rs` | `17` |
| 4 | 384-dimension MiniLM embeddings | `grep -c '384-dim all-MiniLM-L6-v2' csr-engine/src/embeddings/mod.rs` | `1` |
| 5 | Prompt hook stops itself at 560 ms | `grep -n 'PROCESS_BUDGET: Duration' csr-engine/src/hooks/prompt_transport.rs` | `from_millis(560)` |
| 6 | Prompt hook leaves a 40 ms margin, so it waits until 520 ms | `grep -n 'FALLBACK_MARGIN: Duration' csr-engine/src/hooks/prompt_transport.rs` | `from_millis(40)` |
| 7 | Daemon gives each request 420 ms | `grep -n 'REQUEST_DEADLINE: Duration' csr-engine/src/hooks/prompt_transport.rs` | `from_millis(420)` |
| 8 | In-process fallback answers within 255 ms | `grep -n 'FALLBACK_BUDGET: Duration' csr-engine/src/hooks/prompt_transport.rs` | `from_millis(255)` |
| 9 | Stop worker waits up to 3,000 ms for the turn's closing row | `grep -n 'const TURN_END_WAIT' csr-engine/src/hooks/stop.rs` | `from_millis(3000)` |
| 10 | Stop worker polls every 100 ms | `grep -n 'const TURN_END_POLL' csr-engine/src/hooks/stop.rs` | `from_millis(100)` |
| 11 | Stop hook starts no watchdog thread | `grep -c 'spawn_watchdog' csr-engine/src/hooks/stop.rs` | `0` |
| 12 | Learnt line is at most 320 bytes | `grep -n 'LEARNT_MAX_BYTES: usize' csr-engine/src/hooks/learnt.rs` | `= 320;` |
| 13 | Dreams wait for 30 idle minutes by default | `grep -n 'DEFAULT_IDLE_MINS: u64' csr-engine/src/daemon/dream_cadence.rs` | `= 30;` |
| 14 | Dream floor hour defaults to 3 | `grep -n 'DEFAULT_FLOOR_HOUR: u32' csr-engine/src/daemon/dream_cadence.rs` | `= 3;` |
| 15 | Dream floor grace is 6 hours | `grep -n 'DEFAULT_FLOOR_GRACE_HOURS: u64' csr-engine/src/daemon/dream_cadence.rs` | `= 6;` |
| 16 | A reflection over 2,000 characters needs confirmation | `grep -n 'CONFIRMATION_THRESHOLD: usize' csr-engine/src/mcp/elicitation.rs` | `= 2000;` |
| 17 | 36 subcommands | `awk '/^enum Commands/{f=1;next} f&&/^}/{exit} f&&/^    [A-Z][A-Za-z0-9]*( \{\|,\|\()/{n++} END{print n}' csr-engine/src/main.rs` | `36` |
| 18 | The briefing's `claude -p` call times out at 120 s | `grep -n 'BRIEFING_TIMEOUT_SECS: u64' csr-engine/src/hooks/session_briefing.rs` | `= 120;` |
| 19 | Slowest Stop hook 75 ms over 1,527 runs (2026-11-03 to 2026-11-10) | `cat "$LOG.1" "$LOG" 2>/dev/null \| awk '$1>="2026-11-03" && $1<="2026-11-10T19:30:00Z" && /CSR hook stop \[/ {match($0,/total=[0-9]+/); v=substr($0,RSTART+6,RLENGTH-6)+0; if(v>m)m=v; n++} END{print n, m}'` | `1527 75` |
| 20 | Slowest detached Stop worker 33,887 ms over 1,523 runs, same window | same command with `/CSR hook stop-work \[/` | `1523 33887` |
| 21 | Slowest prompt hook 558 ms over 1,376 runs, same window | same command with `/CSR hook prompt-submit \[/` | `1376 558` |

Two numbers in this README have no recount command here because they need a build: the binary size and the test count. Recount them on a release build with `ls -l target/release/csr-engine` and `cargo nextest run`.

</details>

<a id="mcp-tools"></a>

<details>
<summary><strong>MCP Tools</strong> — 16 annotated tools available to Claude</summary>

All tools include [MCP tool annotations](https://modelcontextprotocol.io/specification/2025-11-25/server/tools) so Claude Code understands their safety characteristics.

| Tool | Description | Safety |
|------|-------------|--------|
| `csr_reflect_on_past` | Semantic search across past conversations | read-only |
| `store_reflection` | Store insights for future retrieval | **writes** |
| `csr_quick_check` | Fast existence check (count + top match) | read-only |
| `search_by_recency` | Time-constrained search ("last week") | read-only |
| `get_recent_work` | "What did we work on?" with session grouping | read-only |
| `get_timeline` | Activity timeline with statistics | read-only |
| `csr_search_by_file` | Find conversations that touched a file | read-only |
| `csr_search_by_concept` | Theme-based search ("security", "testing") | read-only |
| `csr_search_insights` | Aggregated patterns from search results | read-only |
| `csr_get_more` | Paginate through additional results | read-only |
| `get_full_conversation` | Retrieve complete JSONL conversation | read-only |
| `get_session_learnings` | Iteration-level memory for Ralph loops | read-only |
| `csr_code_graph` | Which conversations shaped a function or file (AST anchors) | read-only |
| `csr_why` | Provenance chain — why does this code/decision exist | read-only |
| `csr_resolve` | Record verified verdicts (resolved/still_open/regressed) on chunks | **writes** |
| `csr_transcript` | Structured facts from one session transcript: stats, prompts, tools, files, errors, slices, grep | read-only |

</details>

<a id="hooks"></a>

<details>
<summary><strong>Hooks</strong> — 7 session lifecycle hooks</summary>

| Hook | What it does |
|------|-------------|
| **SessionStart** | Surfaces relevant past context at conversation start |
| **UserPromptSubmit** | Sends past context before Claude responds, only when a match clears the relevance gate; otherwise it stays silent |
| **PostToolUse** | Tracks file edits with session-scoped dedup |
| **Stop** | Imports the transcript, keeps a rolling summary, stores a structured episode |
| **PreCompact** | Imports the transcript, refreshes the recap anchor, starts one dream cycle |
| **PostCompact** | Stores Claude Code's own compaction summary once, as derived context |
| **SessionEnd** | Stores session narrative for future retrieval |

All hooks use catch-all error handling. They never block Claude Code.

The prompt hook waits for the daemon's answer until 520 ms after it starts and stops itself at 560 ms. The daemon gives each request 420 ms, and requests queue for the one render slot. An answer later than that is dropped and the prompt goes ahead without recall.

Most prompts get no block. The content route requires a past match that meets the relevance floor and at least three rendered conversation excerpts or reflections. Hybrid search can fill unused chunk slots with keyword hits. A request to continue can pick up a prior episode directly, even when the hook payload has no session ID.

Recognized machine turns such as peer messages, idle notices, system reminders, and forwarded notes with no typed reply get no block. When you type a reply after a forwarded note, the note can stay in the search query and influence recall. If the hook omits `source` or leaves it blank, the prompt text drives the admission check. An explicit `sdk`, `poll_event`, or other unrecognized source is silent.

Abstention reasons in `hook-timing.log` include `no_candidates`, `lookalike_only`, `below_floor`, `thin_pool`, `too_short`, and `machine_prompt`. The client logs a recognized machine turn as `transport=skipped` with `decided=abstain reason=machine_prompt`. A sent block reads `decided=block reason=on_topic`. To count it on your own machine, grep the prompt lines of `hook-timing.log` and its rotated copy `hook-timing.log.1` (the log rotates at 10 MiB). Use `cat "$LOG.1" "$LOG" 2>/dev/null | grep -c 'CSR hook prompt-submit.*decided=block'` for blocks, the same with `decided=abstain` for abstentions, and the same with `transport=unavailable` for prompts that never got an answer. A prompt whose answer misses the deadline gets no block and logs `transport=unavailable` (no `decided=` field), so count it apart from the abstentions. `CSR_NO_PROMPT_GATE=1` turns off only the final gate (the best-score floor and the minimum item count). The search floors, the short-prompt rule and the content filters still apply, so a candidate does not guarantee a block. `eval --full` counts a socket abstention as an answer and a fallback as a miss. It gives no verdict until a project has 30 eligible prompts within the last 72 hours and since the running build was installed.

The Stop hook has no watchdog of its own. It hands the turn to a detached worker and returns. That worker waits up to 3,000 ms for the row that closes the turn, polling every 100 ms. Measured in the timing log on one machine from 2026-11-03 to 2026-11-10: 1,527 Stop hooks, slowest 75 ms; the detached worker (`stop-work`) ran 1,523 times, slowest 33,887 ms; 1,376 prompt hooks, slowest 558 ms, which includes process start. Recount commands are in the claims table under Performance.

</details>

<details>
<summary><strong>Experimental: Claude Mods</strong> — CSR recall as an in-process function hook</summary>

> This is not the companion. The companion (`csr-companion`, the herdr pane or the `/csr` panel) is installed by setup; see [Two ways to see the companion](#two-ways-to-see-the-companion). The spike below is a separate plugin that nothing installs.

Claude Code is adding "Claude Mods" (function hooks, anthropics/claude-code#91870):
plugin TypeScript that runs in-process with a typed `$` instead of a shell command per
event. `mods/csr-recall/` is a spike of CSR recall on that surface: one `prompt.submit`
hook calls the `claude-self-reflect` MCP server through `$.mcp.call` and attaches the top
hits as a context block. Nothing installs it; load it explicitly:

```bash
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude --plugin-dir /abs/path/claude-self-reflect/mods/csr-recall
```

Observed on Claude Code 2.1.270: MCP answered in 95 ms, block attached in 99 ms, hook
settled in 997 ms. The shell hooks keep running underneath. Receipts and caveats in
`mods/csr-recall/README.md`.

</details>

<a id="ai-narratives-optional"></a>

<details>
<summary><strong>AI Narratives</strong> — optional</summary>

Transform raw conversations into rich, searchable narratives. The briefing and story calls run through `claude -p`, which signs in the way Claude Code does on your machine, so CSR needs no API key of its own. An `ANTHROPIC_API_KEY` is read directly only by the optional Batch API path (the daemon or the MCP server), and a `claude -p` call inherits a key that is in its environment.

```bash
csr-engine daemon
```


> **Token transparency:** Optional AI narratives (session briefings + story extraction) run
> `claude -p` against your existing Claude subscription — Haiku by default (dreams default to Sonnet 5), capped
> prompts, debounced, and skipped entirely when nothing changed. Every call — including failures and timeouts — is counted:
> `csr-engine status` shows calls and tokens spent today; cache read/creation tokens (often
> the majority of real usage) are tracked separately in the status JSON (`cache_tokens_today` /
> `cache_tokens_total`). Disable anytime with
> `CSR_NO_AI_NARRATIVES=1` (this does not stop the Batch API path); pin the narrative model with `CSR_NARRATIVE_MODEL=<model>` (the table under "What calls `claude -p`" lists the variable for each caller).

</details>

<a id="cli-reference"></a>

<details>
<summary><strong>CLI Reference</strong></summary>

```
csr-engine                     Start MCP server (default)
csr-engine setup               One-shot setup: import + MCP + hooks
csr-engine status              System status (JSON)
csr-engine status --compact    One-line statusline output
csr-engine daemon              Background enrichment daemon
csr-engine hook install --apply Install Claude Code hooks
csr-engine eval                Quick eval (6 checks)
csr-engine eval --full         Full eval (24 results plus a census line; 88s on a 12.6GB database, most of it the integrity check)
csr-engine quality <file>      AST-based code quality analysis
```

</details>

<a id="upgrading-within-10x"></a>

<details>
<summary><strong>Upgrading from 9.5.8 and within 10.x</strong></summary>

**From 9.5.8 (the last 9.x on npm):** with npm, run `npm install -g claude-self-reflect`, then `claude-self-reflect setup`. With the curl installer, run `scripts/install.sh` and answer yes at "Run setup now?"; where it cannot prompt, it prints the `csr-engine setup` command to run. Then restart Claude Code. Setup repoints the MCP registration at the new binary and adds the hooks 10.1.0 needs. Your data is not touched. The first daemon start rebuilds the vector index once, and MCP tools answer keyword-only until it publishes.

**Within 10.x:** after you install a newer binary, run `csr-engine setup` again. It is safe to repeat. It registers hooks a newer build added (for example `PostCompact`) and rewrites the launch agent when its settings changed (for example `ProcessType Interactive`).

Then run `csr-engine status`:

- `hooks_missing` lists hook events that `~/.claude/settings.json` lacks, and `degraded` says why in words. `csr-engine hook install --apply` fixes just the hooks.
- `stale_processes` lists every live process (daemon, MCP server, companion pane) that still runs the older binary. A dormant pane shows up here until you restart it.

Open Claude Code sessions keep their old MCP server until you restart them. Your data is not touched. More in [the upgrade guide](https://ramakay.github.io/claude-self-reflect/#/docs/upgrading).

</details>

<a id="upgrading-from-v7x"></a>

<details>
<summary><strong>Upgrading from v7.x</strong></summary>

v8.0 replaces the Python/Docker stack with a single Rust binary.

```bash
docker compose down 2>/dev/null
curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/scripts/install.sh | sh
```

Your conversation data (`~/.claude/projects/`) is untouched. The new engine re-imports from the same JSONL files.

</details>

<a id="troubleshooting"></a>

<details>
<summary><strong>Troubleshooting</strong></summary>

| Symptom | Fix |
|---------|-----|
| No search results | Run `csr-engine setup` |
| MCP tools not available | Run `csr-engine setup`, restart Claude Code |
| "spawn ENOENT" in MCP | Ensure `csr-engine` is in PATH |
| Slow first startup | Normal (the index is rebuilt once; later starts load the saved index, which takes longer for a larger index) |

Full guide: [Documentation](https://ramakay.github.io/claude-self-reflect/#/docs/troubleshooting)

</details>

<details>
<summary><strong>Backups</strong> — what Time Machine skips, and a safe nightly copy of the database</summary>

On macOS, `csr-engine setup` tells Time Machine to skip three folders inside `~/.claude-self-reflect/`: `index-v3/` (the search index, about 2 GB on a long history and rewritten whenever it rebuilds), `models/` (the embedding models) and `cache/`. Self-Reflect can rebuild or re-download all three, and they would otherwise be copied into every hourly backup. The exclusion is per folder and sticky: it needs no sudo and stays on the folder if you re-run setup, which leaves already-skipped folders alone. `CSR_NO_TM_EXCLUDE=1` skips this step. If `tmutil` is missing or fails, setup prints one line and carries on.

The database (`csr-engine.db`, with its `-wal` and `-shm` files) and `mirror/` stay in your backups on purpose. The mirror is the only copy of transcripts Claude Code has deleted, and the database holds your verdicts and notes. Neither can be rebuilt.

The live database changes constantly, so Time Machine copies the whole file on each backup, and a copy taken mid-write can be inconsistent. Time Machine is a fine backup for it. If you also want a dated copy that is consistent while Self-Reflect is running, use SQLite's online backup, which is safe in WAL mode:

```bash
mkdir -p "$HOME/csr-backups"
tmutil addexclusion "$HOME/csr-backups"   # Time Machine already copies the live DB; don't copy it twice
dest="$HOME/csr-backups/csr-engine-$(date +%F).db"
ok=1
sqlite3 "$HOME/.claude-self-reflect/csr-engine.db" ".backup '$dest.tmp'" \
  && [ "$(sqlite3 "$dest.tmp" 'PRAGMA quick_check;')" = ok ] \
  && mv "$dest.tmp" "$dest" && ok=0
rm -f "$dest.tmp" "$dest.tmp-wal" "$dest.tmp-shm"   # clear the leftovers of a failed run
if [ "$ok" = 0 ]; then
  ls -t "$HOME"/csr-backups/csr-engine-*.db | tail -n +3 | while IFS= read -r f; do rm -f -- "$f"; done
else
  echo "csr-engine backup failed; older copies kept" >&2; false
fi
```

The copy is written to a temporary name and checked with `PRAGMA quick_check` before it replaces anything. Only a copy that passed is kept, and only then are older copies pruned (the newest two stay), so a failed run never costs you a good copy. A failed run also prints one line to stderr and ends with a non-zero status (there is no `exit`, so it is safe to paste into an interactive shell), which is what cron or launchd needs to tell you the backup did not happen. The `tmutil addexclusion` line keeps the folder out of Time Machine: the database can be many GB, and a fresh copy every night would otherwise be backed up on top of the live file. Run it nightly from cron or launchd. The copies hold your transcripts, so keep them local. Copy the folder to another drive if you want a copy off this disk, and never to a cloud drive or a shared disk.

To undo the exclusion and let Time Machine back a folder up again:

```bash
tmutil removeexclusion "$HOME/.claude-self-reflect/index-v3"
```

Do the same for `models` and `cache`. Running `csr-engine setup` again re-applies the exclusion, so set `CSR_NO_TM_EXCLUDE=1` first if you want it to stay off.

</details>

<details>
<summary><strong>Uninstall</strong></summary>

`csr-engine uninstall` removes what `setup` installed and nothing else:

```bash
csr-engine uninstall --dry-run            # print the plan, change nothing (also the default without --yes)
csr-engine uninstall --yes                # do it; your data folder ~/.claude-self-reflect is kept
csr-engine uninstall --yes --purge        # also delete the data folder, except the transcript mirror
csr-engine uninstall --yes --purge-mirror # delete the data folder and the mirror too (implies --purge)
```

It removes CSR's hooks from `~/.claude/settings.json` (your other hooks stay; a `settings.json.csr-uninstall-<time>` backup is written first), the `claude-self-reflect` MCP entry (and any project-scoped entry of that name, through `claude mcp remove -s local` run from the project folder), the companion plugin and its cache under `~/.claude/plugins/cache/claude-self-reflect`, the launch agent `setup` wrote (a plist you wrote yourself is left alone), and the npm global package `claude-self-reflect` when `npm ls -g` lists it. Your data is kept by default: `~/.claude-self-reflect/` (database, settings, the transcript mirror) stays, and its path and size are printed with "kept; --purge deletes it". `--purge` deletes that folder except the transcript mirror, the only copy of transcripts Claude Code has deleted; `--purge-mirror` deletes the mirror too. (`--keep-data` from older versions is still accepted and does nothing.) `~/.claude/projects` is never touched. The Codex MCP entry is removed through `codex mcp remove`, with Codex's `config.toml` copied aside first. The launch agent is booted out and its plist deleted first, so nothing starts the daemon again at login. Uninstall then waits up to 35 s (launchd's `ExitTimeOut` plus 5 s) for the daemon to exit. A daemon still running then gets one SIGTERM, sent only when launchd and `daemon.lock` name the same pid, and 5 s more. If the daemon has not stopped, the npm package, the binary and, with `--purge`, the data folder stay, the command exits 1, and its last line starts with "not finished:" and gives the exact command that finishes once `launchctl print` no longer finds the job. It also deletes the model cache `~/Library/Caches/csr-engine` (the downloaded embedding model, about 90 MB; it is not your data, so it goes without `--purge` too), and last every csr-engine binary the hooks, the MCP entries and the launch agent named, plus the one running: a copy in `~/.local/bin` is deleted, any other (a build folder, `/usr/local/bin`) is not touched and its exact `rm` line is printed. Every step prints a line for what it removed or kept. Running csr-engine sessions are listed at the end with their PIDs, and never signalled (only the launchd daemon can get that one SIGTERM): restart them, or they keep using the old hooks and, after `--purge`, recreate the data folder. The plan (without `--yes`) runs only read-only probes (`npm ls`, `ps`).

By hand, if you prefer:

```bash
# 1. Stop the daemon LaunchAgent that setup installed (macOS)
launchctl bootout gui/$(id -u)/com.ramakay.csr-engine.daemon
rm ~/Library/LaunchAgents/com.ramakay.csr-engine.daemon.plist

# 2. Unregister the MCP server
claude mcp remove claude-self-reflect

# 3. Delete CSR's data, including the transcript mirror
rm -rf ~/.claude-self-reflect/

# 4. Delete the model cache and the binary
rm -rf ~/Library/Caches/csr-engine
rm ~/.local/bin/csr-engine
npm uninstall -g claude-self-reflect  # if installed via npm
```

Then open `~/.claude/settings.json` and delete every entry under `hooks` whose command starts with `csr-engine hook` (SessionStart, UserPromptSubmit, PostToolUse, Stop, PreCompact, PostCompact, SessionEnd). Claude Code keeps calling them until you do, and they fail once the binary is gone. If you installed the companion plugin, remove it with `/plugin` in Claude Code.

</details>

<details>
<summary><strong>Contributors (v1–v7)</strong></summary>

- **[@TheGordon](https://github.com/TheGordon)** - Fixed timestamp parsing (#10)
- **[@akamalov](https://github.com/akamalov)** - Ubuntu WSL insights
- **[@kylesnowschwartz](https://github.com/kylesnowschwartz)** - Security review (#6)

</details>

---

[Documentation](https://ramakay.github.io/claude-self-reflect/) | [npm](https://www.npmjs.com/package/claude-self-reflect) | [Issues](https://github.com/ramakay/claude-self-reflect/issues) | MIT License
