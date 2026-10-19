---
title: btw, these too
---

Smaller features that ship in this release. Each line says what you get.

## Memory that shows up by itself

- **Recap line:** a session starts with one line on where you left off: intent, what got done, what was settled, what is open, what you corrected, the next step.
- **Learnt line:** one line at session start on what CSR stored for your last session: memory notes, verdicts you confirmed, anchors a commit retired. Each clause carries a receipt. At most 320 bytes.
- **Worktree and subfolder recall:** start Claude in a worktree or a subfolder and you get the repo's history, not an empty memory.
- **Resume lineage:** after `claude -c` or a resume, CSR names the session you continued and treats it as the last session.
- **Dream line at prompt time:** when your prompt names a symbol or file a dream concluded on, one line appears. The same dream never repeats.
- **File history on mention:** name a file in your prompt and its edit history comes along.
- **Trust labels:** model-written context is labelled `[briefing: model-derived, trust=...]`, so Claude knows what is a guess.
- **No self-echo:** CSR text that comes back as input is labeled `[CSR text]`. It stays searchable, ranks below your own words, and is kept out of recaps and briefings. `csr-engine status` counts what it labeled.

## It keeps what Claude Code throws away

- **Transcript mirror:** a scrubbed, zstd-compressed copy of every main and subagent transcript, kept after Claude Code deletes the original. Manage it with `csr-engine mirror sync|status|purge --yes`.
- **Queued prompts:** what you type while Claude is still busy is indexed and searchable like any other prompt.
- **Codex sessions:** if `~/.codex/sessions` exists, your Codex work is searchable next to Claude's. Nothing to set up.
- **Memory file catalogue:** CSR knows which Claude memory files exist (metadata only, never embedded).

## Privacy built in

- **`<private>` tags:** wrap text in `<private>...</private>` and it never reaches memory or the mirror.
- **Secret redaction:** API keys and tokens become `[secret redacted: <kind>]` before they reach the index or the mirror. `CSR_NO_SECRET_SCRUB=1` turns it off.
- **Private companion storage:** the companion's data folder is `0700`, its files `0600`, and tokens and keys are redacted from the text it keeps.
- **Loopback-only journal:** the dream journal binds to 127.0.0.1 only. Its POSTs need a CSRF token, and its pages run no scripts (CSP `default-src 'none'`).

## Commands you can run yourself

- **`csr-engine transcript <session> stats|prompts|tools|files|errors|slice|grep`:** read any session's prompts, tools, files and errors without hand-rolled `jq`.
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

## The dream journal in your browser

Open `http://127.0.0.1:7373/`. `CSR_JOURNAL_PORT` moves the port and `CSR_NO_JOURNAL_SERVER=1` turns the server off.

- **Home:** the week's dreams.
- **`/board`:** open items at a glance. **`/corrections`:** your corrections over time.
- **`/dream/{id}`:** the code's shape before and after, as an AST diff.
- **Act without Claude:** resolve or dismiss a dream in the browser.
- **Scripting:** `/api/dreams` (JSON) and `/healthz`.

## Companion pane, beyond the five axes

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
- **`/csr statusline`:** moves your status bar into the pane, and restores it when the pane closes.
- **Phone layout:** the pane has a layout for a narrow terminal.
- **One dreamer per session, crash guard:** a lease stops two dreamers on one session, and a broken edit to the pane does not kill it.

## Quiet MCP niceties

- **Argument completions:** project, file, session, time range and concept complete as you type.
- **Background tasks:** heavy tools run as MCP Tasks, so long calls never block.
- **Upgrade notice:** after an upgrade, results tell you to restart a session that still runs the previous build.
- **Keyword fallback:** results come back as `[keyword]` matches when the vector path is down.
- **Health resource:** `status://system-health`, readable from inside Claude.
- **Confirmations:** you confirm before a reflection over 2,000 characters is stored, and `csr_resolve` verdicts are confirmed against a SHA-256 of the payload.

## It looks after itself

- **Hands-free upgrades:** the daemon exits when its binary is replaced, so launchd restarts it on the new build.
- **Recall canary:** a known-item recall check runs on a six-hour loop.
- **Catch-up sweep:** every 30 minutes, so no transcript is missed.
- **Installer checks:** it warns when an older `csr-engine` earlier on your PATH would win, and restarts the daemon so an upgrade takes effect.

## Dials

- **Kill switches:** `CSR_NO_RECAP`, `CSR_NO_LEARNT`, `CSR_NO_DREAMING`, `CSR_NO_DREAM_INJECT`, `CSR_NO_DREAM_ON_COMPACT`, `CSR_NO_MIRROR`, `CSR_NO_SECRET_SCRUB`, `CSR_NO_STORY_MAP`, `CSR_NO_CODEX_IMPORT`, `CSR_NO_HEALTH`, `CSR_NO_JOURNAL_SERVER`, `CSR_NO_AI_NARRATIVES`.
- **Dream cost:** `CSR_DREAM_EFFORT=less|balanced|max` and `CSR_DREAM_BUDGET`.
- **Dream timing:** `CSR_DREAM_INTERVAL_SECS`, `CSR_DREAM_IDLE_MINS` (default 30), `CSR_DREAM_FLOOR_HOUR` (default 3).
- **Search path:** `CSR_SEARCH_MODE=hybrid|vector|fts`.
- **CPU caps:** `CSR_EMBED_THREADS`, `CSR_TOKIO_WORKERS`.
