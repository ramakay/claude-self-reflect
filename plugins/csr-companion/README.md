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
