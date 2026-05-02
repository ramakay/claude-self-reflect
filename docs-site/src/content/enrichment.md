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

## Layer 3: AI Narrative (optional, ~$0.012/conv)

Uses Anthropic Batch API (50% discount). Generates rich narratives.

| Metric | Without AI | With AI |
|--------|-----------|---------|
| Search relevance | 0.074 | 0.691 **(9.3x)** |
| Token compression | 100% | 18% **(82% reduction)** |

### Enable

```bash
export ANTHROPIC_API_KEY=sk-ant-...
csr-engine daemon
```

### Cost Estimates

| Conversations | Cost |
|---------------|------|
| 100 | ~$1.20 |
| 500 | ~$6.00 |
| 1,000 | ~$12.00 |

> **Tip**: Start without AI narratives. Free Layer 1+2 provides good search. Add Layer 3 later for best recall.

## Check Status

```bash
csr-engine status
# enrichment.heuristic_completed, extracted_v3_completed, ai_narrative_completed
```
