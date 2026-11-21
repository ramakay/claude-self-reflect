---
title: Enrichment Pipeline
---

## Overview

Raw conversations are noisy — 74% tool_use blocks. The enrichment pipeline transforms this into searchable knowledge. Each layer supersedes the previous.

## Layer 1: Heuristic (free, instant)

At import time. Extracts user requests, errors, tool usage, file paths.

## Layer 2: V3 Extraction (free, seconds)

At session end. Structured extraction: search summary, edit patterns, error recovery flows.

Includes V3 story synthesis (zero-cost, local):

```
## Session Story: Docker health checks
Project: my-api | 45 min, 34 messages

### Key Decisions
- /healthz for Kubernetes compatibility
- 30-second interval, 3 retries
- Separate liveness and readiness probes
```

## Layer 3: AI Narrative (optional)

Generated locally via the Claude Code CLI (`claude -p`) — no API key, billed through your existing Claude subscription. Generates rich narratives.

| Metric | Without AI | With AI |
|--------|-----------|---------|
| Search relevance | 0.074 | 0.691 **(9.3x)** |
| Token compression | 100% | 18% **(82% reduction)** |

### Enable

```bash
csr-engine daemon
```

### Model Selection (v9.3)

Narratives resolve the model through a chain — no dated model pin to go stale:

1. `CSR_NARRATIVE_MODEL` (if set)
2. `haiku` alias (cheapest tier)
3. CLI default (only if the alias itself is unavailable)

The chain only advances on a real model-not-found error, never on transient failures.

### Cost Controls (v9.3)

Every narrative call is metered — including failures and timeouts:

```bash
csr-engine status            # "narratives" block: calls/tokens today + total
csr-engine status --compact  # "AI 3c/12.4k tok today" or "AI off"
```

Kill switch:

```bash
export CSR_NO_AI_NARRATIVES=1   # disables all AI narrative generation
```

The session briefing is also content-hash cached — it only regenerates when episode content actually changes.

> **Tip**: Start without AI narratives. Free Layer 1+2 provides good search. Add Layer 3 later for best recall.

## Check Status

```bash
csr-engine status
# enrichment.heuristic_completed, extracted_v3_completed, ai_narrative_completed
# narratives.calls_today, narratives.tokens_today, narratives.disabled
```
