# Reddit post — scoped claims

**Status:** open (user-gated: draft needs review before posting)

Launch post for CSR, deferred from v9.2/v9.3 prep. Use scoped, defensible claims per the Codex competitive review:

**Lead:** "local-first Claude Code continuity layer" — NOT "killer feature no one else offers".

**Defensible claims:**
- AST anchors tying conversations to code symbols (lead differentiator)
- Fully local stack: single Rust binary, local embeddings, SQLite+HNSW, no cloud, no API keys
- Semantic intent-gated injection at the decision point (SessionStart CONTINUUM + prompt-submit routes)
- v9.3 token accounting turns the "hidden AI cost" criticism into a feature — every narrative call metered, kill switch documented

**Avoid (would get torn apart):**
- "beats grep" without caveating single-query eval
- `supersedes` field claims (never populated)
- Any "no one else does memory" framing (mem0/letta/zep/claude-mem exist; differentiation is the AST+local+injection combination, not memory per se)
