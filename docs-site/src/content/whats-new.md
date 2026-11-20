---
title: What's new in 10.1
---

10.1.0 is the first public 10.x release. It upgrades from 9.5.8 and replaces it. Self-Reflect stays one Rust binary, `csr-engine`, with no Python, Docker or Qdrant. To upgrade, see [Upgrading](#/docs/upgrading).

## Context on arrival

- **Recap paragraph.** At SessionStart, one causal paragraph replaces the old `CONTINUUM` / `LAST` / `NEXT` / `ANCHORS` fragments: intent, what got done, what was settled, what is open now, the next step. Each clause drops when there is no evidence for it, `Next:` is never invented, and there is no LLM call. A feed error falls back to the old fragments. `CSR_NO_RECAP=1` turns it off.
- **Learnt line.** A separate line at startup or resume names what CSR stored for the previous session: memory notes, `csr_resolve` verdicts you confirmed, and anchors a commit retired. Each clause carries a receipt, and there is no line when there is no evidence. `CSR_NO_LEARNT=1` turns it off.
- **Compact receipts.** A compact leaves pointers to the turns it replaced, with `csr_transcript` slice calls to read them back. `CSR_NO_COMPACT_POINTER=1` turns the pointer off.

## Dreaming and witness anchors

- `codewitness` stamps code spans at commit OIDs and checks old conclusions against your git history. Search shows `[stale anchor]` and `[evolved]` notes with commit receipts. The daemon dreams every 6 hours (`CSR_DREAM_INTERVAL_SECS`; `CSR_NO_DREAMING=1` turns it off).

## More sources

- Plans, task state, subagent transcripts, Codex sessions and the session registry join the conversation corpus.
- New tool `csr_transcript` and `csr-engine transcript`: stats, prompts, tools, files, errors, slice and grep over one session's transcript. CSR has 16 MCP tools in all.

## Code map

- A live map of definitions in supported source files in the repos your sessions work in, per worktree. `csr_search_by_file` and `csr_code_graph` use it, so they answer for code no conversation touched.

## Provenance and self-contamination guard

- CSR's own output is labeled and ranks below your words. CSR no longer embeds its own MCP tool calls, their results, or hook-injected blocks.

## Companion plugin

- A pane beside the session. The companion now runs inside the engine (`csr-engine companion serve`); no Node, TypeScript or Python runs at run time. See [Choose your view](#/docs/companion-views).
- `csr-engine setup` also registers the MCP server with Codex when the `codex` CLI is on `PATH`.

## Installer

- `csr-engine setup` repoints the MCP registration on upgrade, and the installers report when another `csr-engine` is still in use.

---

Everything else, including performance numbers and known limitations, is in the [full changelog](https://github.com/ramakay/claude-self-reflect/blob/main/CHANGELOG.md).
