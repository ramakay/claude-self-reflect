# csr-companion

CSR beside Claude Code: a pane with this session's work, the dreamer's "what if…?" questions, code that changed
since your past sessions, and a compaction coach. See `hooks/register.tsx` for how each part is wired.

Install (Claude Code 2.1.275 or later):

```
/plugin marketplace add ramakay/claude-self-reflect
/plugin install csr-companion@claude-self-reflect
```

The git guard is off unless a `csr-guard.json` exists, either `<git toplevel>/.claude/csr-guard.json` or
`~/.claude-self-reflect/csr-companion/guard.json` (keys `denyNoVerify`, `protectedPush`, `reason`). It catches
literal mistakes in the command text and is not a security boundary. The stop for publishing is the `prod`
environment's required review: `release.yml` runs `npm publish` only in `prod`.

Everything the plugin keeps is under `~/.claude-self-reflect/csr-companion/`: the dream log, the threads carried
between sessions, the previous-session facts, likes, the error log. It holds text from your transcripts, so the
folder is made `0700` and every file `0600`, whatever your umask; files an earlier version left looser are tightened
the first time the plugin writes. Keys (`sk-`, `xai-`, `ghp_`, `AKIA`), `Bearer` tokens, `password=`/`token=` values
and PEM blocks are removed from the text before it is kept. That is pattern matching: a secret in a shape it does
not know stays. All writes go through `bin/private.py`.

The dreamer (off until you turn it on in `/config`) checks before each look that what it has spent, plus what the
last look cost, stays within 1 token for every 10 Claude spends. It is a check, not a cap: a fork takes no token
ceiling, so one look can cost more than that guess. The real cost is recorded as soon as the fork returns, and any
amount over the share is owed: no look runs until Claude's own spending has covered it. `/csr dreams` shows it.
