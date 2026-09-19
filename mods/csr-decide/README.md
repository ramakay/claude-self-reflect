# csr-decide (spike)

Typed decisions for Claude Self-Reflect as a Claude Mod, answered by a LOCAL endpoint that speaks
Typesafe Jev's System One wire format (`POST {model, state, questions}` →
`{answers: {name: {choice, probabilities, confidence} | {noul} | {score}}}`). No cloud call, no API key.

Two lifecycle points:

- `prompt.submit`, shadow mode: labels how the human's new turn reacts to the assistant's last turn
  (`correction | acceptance | none`) and logs `{decision, confidence, ms}`. Injects nothing. This is the
  input the shell hook never had: the engine's reaction labeler embeds only the user text
  (`csr-engine/src/daemon/trained_rerank.rs`), a third of transcript "user" turns are harness text, and one
  user turn is stored once per assistant entry. Here `e.text` is what the human typed,
  `$.session.messages()` holds the assistant turn, and the hook fires once per prompt.
- `session.compact`: truncates old tool results instead of summarizing, using
  [fast-jev-compaction](https://github.com/tamaratran/fast-jev-compaction) (MIT) unchanged. By default a rule
  answers the library's questions in-process (every old call stays, every old result is truncated): no model,
  no endpoint, no network. `CSR_DECIDE_COMPACT=model` sends them to the local endpoint instead. Falls back to
  the built-in summary on any error or under 25% reduction. Every truncated result gets a pointer to
  `csr_transcript`, so a truncation costs one recall, not the fact.

With the endpoint down, slow, or answering garbage, both hooks pass the event on untouched.

Function hooks are early access (anthropics/claude-code#91870). Observed against Claude Code 2.1.277.

## Run

The compaction library is not on npm and is not vendored here (`vendor/` is ignored):

```sh
git clone https://github.com/tamaratran/fast-jev-compaction /abs/path/mods/csr-decide/vendor/fast-jev-compaction
git -C /abs/path/mods/csr-decide/vendor/fast-jev-compaction checkout e3f262a
```

```sh
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude plugin validate /abs/path/mods/csr-decide --json
CLAUDE_CODE_ENABLE_FUNCTION_HOOKS=1 claude -p --model haiku \
  --plugin-dir /abs/path/mods/csr-decide --debug-file /tmp/csr-decide.log "<prompt>"
grep -E 'csr-decide|hook failed' /tmp/csr-decide.log
```

| env | default | what |
|---|---|---|
| `CSR_DECIDE_URL` | `http://127.0.0.1:8765/v1/systemone` | the local System One endpoint |
| `CSR_DECIDE_COMPACT` | rule | `model`: ask the endpoint per old call instead of applying the rule (scored below the rule, see the table further down) |
| `CSR_DECIDE_ESCALATE` | off | `1`: below confidence 0.6, ask the same question through `$.model.classify` and log agreement; `always`: every prompt (proof aid) |
| `CSR_DECIDE_PROBE` | off | `1`: also time a `$.mcp.call` to CSR's MCP server, as the transport baseline |
| `CSR_DECIDE_FORCE_COMPACT` | off | `1`: request one compaction after the first turn, to exercise the hook under `-p` |

## Receipts (2026-09-18, Claude Code 2.1.277, haiku, `-p`, endpoint = Qwen3-1.7B-4bit read by logits, Apple M5 Max)

| witness | value |
|---|---|
| `claude plugin validate --json` | `success: true`; hooks `prompt.submit, session.compact, turn.complete`; env reads `CSR_DECIDE_ESCALATE, CSR_DECIDE_FORCE_COMPACT, CSR_DECIDE_PROBE, CSR_DECIDE_URL` |
| control without `--plugin-dir` | no `csr-decide` line in the debug log |
| first prompt of a session | `prompt.submit no assistant turn yet, skipped` |
| `$.http.fetch` to loopback | `200 in 116ms`, of which the engine reports 116 ms: transport is ~2 ms |
| `$.session.messages()` | returned the prior assistant turn on a resumed session (`n=3`, 0 ms) |
| "no, that is wrong. I asked for baz, not bar" | `reaction=correction conf=1.00 http=118ms` |
| "yes, that works, go ahead with exactly that" | `reaction=acceptance conf=1.00 http=112ms` |
| "hm, and the tests?" | `reaction=none conf=0.93 http=264ms` |
| "actually no, undo that, the tests already use bar" | `reaction=none conf=0.37 p=correction:0.50,…,none:0.50`; `$.model.classify` (haiku) said `correction` in 494 ms, `agree=false` |
| `$.mcp.call` probe, same prompt | timed out at 6,005 ms in this run (95 ms in the csr-recall spike); not chased |
| endpoint on a dead port | `$.http.fetch … ConnectionRefused`, logged, prompt answered normally, exit 0 |
| `$.session.compact()` from `turn.complete` | refused under `-p`: "not available in a headless (-p / SDK) session yet" |
| `/compact` as a `-p` prompt on a session with 8 Read calls | `session.compact kept 27/27 messages, no summary (44% reduction; 1 kept, 5 results truncated, 2 pinned; state ~1156 tokens) in 611ms`; engine: `a hook's 27 messages stand … core never ran` |
| transcript after compaction | 3 truncated results carry the fast-jev note plus the `csr_transcript` pointer |
| the rule (default), same shape of session, nothing listening on 8765 | `session.compact kept 27/27 messages, no summary (58% reduction; 6 results truncated, 2 pinned; state ~1156 tokens) in 1ms via rule`; zero `$.http.fetch` lines in the debug log; engine: `core never ran`; 4 of 4 truncation notes in the transcript carry the pointer |

## What the endpoint's answers are worth (2026-09-18): not much yet

The plumbing above is proven. The decider behind it is not. Off-the-shelf 0.6B to 2B instruct models read by
logits, no tuning:

| read | result |
|---|---|
| latency, resident state, one question, p95 | hybrids pass under 150 ms and stay flat to 25k state tokens (Qwen3.5-2B 67 ms, Qwen3.5-0.8B 44 ms, LFM2.5-1.2B 68 ms); plain attention fails at long state (Qwen3-1.7B 210 ms, Qwen3-0.6B 179 ms at ~25k) |
| compaction, real fast-jev library, 3 transcripts of 331 to 394 old calls | Qwen3-1.7B: 19% / 0.3% / 38% reduction, 116 to 163 s. Qwen3.5-2B: 66% / 41% / 47%, 64 to 68 s |
| keep/drop vs a blind Opus judge, 60 sampled calls | 3-way action agreement 10% (1.7B) and 28% (2B), kappa -0.06 and 0.04. The 1.7B kept 325 of 331 results in one session and 0 of 391 in another; the 2B dropped all 5 results the judge wanted verbatim |
| the same judge vs no model at all | the judge dropped the verbatim result on 55 of 60 calls: "truncate every old result, leave the pointer" agrees 92% on keep-result, above both models (48%, 88%) |
| reactions with the assistant turn, 200-text gold set, 0.90-precision gate | fails: dev accuracy 0.18 to 0.45, no threshold reaches 0.90; neutral follow-ups read as `acceptance` (46 of 76), 6 of 38 corrections caught zero-shot. The 8 ms fixed-label head (Brier 0.375) beats every variant (0.43 to 0.47) |

So: `prompt.submit` stays shadow-only, and `session.compact` does not trust this decider. Because every
truncated result keeps a `csr_transcript` pointer, the rule with no model is the better compaction today, and
it is the default. The rule keeps every call where the judge would drop 42 of 60: the call is what the pointer
hangs on, and inputs are small next to results.

## Validator rules learned the hard way

- `modules` takes one hooks module per plugin; a second entry is refused.
- `$` is followed only into functions declared in the hooks module itself, never across an import. Helpers in
  other files must be pure (`hooks/ask.ts`).
- `$.env.get` takes a literal name, so the env a module reads can be listed. No constants.

## Notes

- One deviation from csr-recall: the compaction pointer carries no CSR emission header and no machine
  sentinel. A sentinel marks the whole conversation contaminated and the reaction harvest then stores zero
  labels for it; the pointer is plain text inside a tool result, which the importer already treats as tool
  output.
- The 1.5 s race only bounds a wedged endpoint; a local decision answers in tens of milliseconds.
- The existing shell hooks keep running beneath via `classic.*`; this mod adds, it does not replace.
