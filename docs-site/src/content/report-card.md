---
title: What we're working on
---

Seven things in 10.1.0 are not where we want them. Each one has a measured number, a target and the work under way. All measurements are from 2026-11-18 on one real install: 1,083,965 chunks across 6,322 conversations and 345 projects, read with the installed `csr-engine 10.1.0`. They are single readings on one machine, not benchmarks. "Not measured yet" means we have no number and are not guessing.

| Item | Measured 2026-11-18 | Target |
|------|---------------------|--------|
| Daemon memory under load | 1.72 GiB resident at 9 h, 1.09 GiB at 10 h 52 min (2 samples, not a load test) | RSS within 10% of its 1-hour value after a 24 h replay of real history |
| Known-item recall, top 5 | 14 of 18 (78%) | 16 of 18 (89%) or better on a fresh sample drawn each release |
| Index lookup p95 | 18.47 ms | Under 1 ms p95 on the live index |
| Relevance labels too often "weak" | Not measured yet | Under 25% of correct rank-1 hits labelled weak |
| `csr_quick_check` false positives | Not measured yet | Under 10% on a labelled 30-query set |
| Status surfaces disagree | 4 cases | 0 |
| Thin MCP tools | Not measured yet | Each returns a non-empty answer with receipts on a 10-query test set |

## Daemon memory under load

The daemon holds the vector index and runs enrichment, dreams and the dream journal. On 2026-11-18 at 09:29 PST it had run for 9 h 03 min and used 1.72 GiB resident (`ps` RSS 1,807,808 KB). At 11:18 PST, same process, 10 h 52 min, it used 1.09 GiB. That is 2 samples, not a load test. No log or status field carries an RSS series yet, so we cannot tell steady state from growth.

- **Measured:** 1.72 GiB resident at 9 h 03 min uptime and 1.09 GiB at 10 h 52 min. 2 samples, not a load test.
- **Target:** RSS within 10% of its 1-hour value after a 24 h replay of real history on the time-travel soak rig.
- **Doing:** adding an RSS series to daemon history and `status`, and running the soak each release.

## Known-item recall, top 5

The engine probes itself with questions whose answer is a known past conversation. `eval --full` on 2026-11-18 found 14 of 18 in the top five. All four misses were `not_in_top5`: the item existed and ranked lower. Two probes were invalidated. The sample is frozen, so the same 14 of 18 is not a fresh draw.

- **Measured:** 14 of 18 (78%).
- **Target:** 16 of 18 (89%) or better on a fresh sample drawn each release.
- **Doing:** drawing a fresh sample per release and inspecting the four misses.

## Index lookup p95

`eval --full` times 50 vector index lookups. The result is amber and does not gate the release. It covers the lookup only, not a full recall call.

- **Measured:** 18.47 ms p95 against a 1 ms eval target.
- **Target:** under 1 ms p95 on the live index.
- **Doing:** profiling the lookup path on a 1M-chunk index.

## Relevance labels too often "weak"

Search results carry a relevance label. We think too many correct results come back labeled "weak", but we cannot measure it: the label is not stored with each retrieval, and `eval --full` prints no label distribution.

- **Measured:** not measured yet.
- **Target:** log the label for every result. Under 25% of correct rank-1 hits labelled weak.
- **Doing:** storing the label per retrieval event and reporting it in `eval --full`.

## `csr_quick_check` false positives

`csr_quick_check` answers "did we discuss this?" with a count and the top match. It has two paths. A semantic query is checked against a score floor derived from probing topics that were never discussed; below the floor it returns `found=false` with no preview. A query that holds an exact identifier uses literal evidence: `found` is true only when a stored chunk holds that identifier as a whole token. A false positive is a "yes" for a topic that was never discussed.

- **Measured:** not measured yet.
- **Target:** false positives under 10% on a labelled 30-query set.
- **Doing:** building the labelled set and adding it to `eval`.

## Status surfaces disagree

`csr-engine status` and `csr-engine telemetry` read the same engine and sometimes disagree. We counted four cases on 2026-11-18:

- `status` reports `healthy: true` and `state: "degraded"` in the same output. The cause is a Codex timestamp repair that stopped after 10 attempts.
- The chunk count differs by surface within one minute: 1,083,965 in `status`, 1,083,901 in the stored provenance figure and 1,084,116 in `telemetry`. Live ingest explains part of the gap. The provenance figure is a stored snapshot.
- Import coverage reads 99.23% (515 of 519 files) in one place and 758 of 1,146 registry sessions (66%) in another. The two use different denominators.
- Dream spend reads as current spend, but it is a lifetime total.

- **Measured:** 4 cases.
- **Target:** 0. One source per count, and every percentage names its denominator.
- **Doing:** routing every surface through one status snapshot.

## Thin MCP tools

Three tools return less than their names promise: `csr_code_graph`, `csr_search_insights` and `get_session_learnings`. All three are listed in [MCP Tools](#/docs/mcp-tools).

- **Measured:** not measured yet.
- **Target:** each returns a non-empty answer with receipts on a 10-query test set.
- **Doing:** writing the test set and deepening the three tools.

## How we measure

Numbers come from the installed binary: `csr-engine status`, `telemetry` and `eval`. The date on each number is the day it was read.
