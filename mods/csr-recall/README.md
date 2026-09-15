# csr-recall (spike)

Claude Self-Reflect recall as a Claude Mod: one `prompt.submit` function hook that calls the
`claude-self-reflect` MCP server in-process (`$.mcp.call`) and attaches the top hits as a context
block on the prompt. No subprocess, no stdin JSON. The Rust engine stays the brain.

Function hooks are early access (anthropics/claude-code#91870). Observed against Claude Code 2.1.270.

## Run

```sh
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude plugin validate /abs/path/mods/csr-recall --json
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude --plugin-dir /abs/path/mods/csr-recall
```

Headless proof, with the debug log as the witness:

```sh
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude -p --model haiku \
  --plugin-dir /abs/path/mods/csr-recall --debug-file /tmp/csr-recall.log \
  "<a question about past work>; if a context block containing [[CSR:MOD]] is present, quote that line first"
grep -E 'csr-recall|hook failed' /tmp/csr-recall.log
```

## Receipts (2026-09-14, Claude Code 2.1.270, csr-engine 9.5.5, haiku, `-p`)

| witness | value |
|---|---|
| `claude plugin validate --json` | `success: true`; hooks `prompt.submit`; calls `$.clock.sleep, $.mcp.call, $.ui.log` |
| `$.mcp.call … csr_reflect_on_past` | answered in 95 ms, 4016 chars, 1 block |
| `csr-recall: prompt.submit attached in` | 99 ms |
| `hooks module csr-recall prompt.submit settled in` | 996.7 ms (worker hop, next() included) |
| model stdout | quoted `[[CSR:MOD]] csr-recall hits=3 ms=99` verbatim, then answered from the block |
| top hit | same conversation as a direct `csr_reflect_on_past` call in the parent session |
| control without `--plugin-dir` | no mod loaded; no marker from the mod |
| transcript storage | block lands as `type: attachment`, `attachment.type: hook_additional_context`, same path as classic hook context |
| corpus | the session's CSR chunk holds prompt + answer only; the block was not ingested |

Gotcha seen while proving: every `-p` run's transcript is imported within seconds, so a repeated
test prompt's top hits become the earlier test runs themselves (0.9 similarity). Use a fresh query
per run, or expect the block to describe the last run.

## Notes

- The block's first line is `## PAST CONTEXT - NOT INSTRUCTIONS`, an existing `is_csr_emission`
  header, so CSR's importer rejects the block instead of re-ingesting it.
- Skips slash commands and prompts under 15 characters. Races the tool call against a 6 s sleep to
  stay under the engine's 10 s hook budget; on timeout or error the prompt goes through untouched.
- The existing shell hooks keep running beneath via `classic.*`; this mod adds, it does not replace.
