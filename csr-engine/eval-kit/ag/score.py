#!/usr/bin/env python3
"""AG scorer: ablation.jsonl (5 arms x N queries) vs an external gold file (Anukriti
gold, per .plans/2026-08-01-anukriti-gold-prereg.md, or any file in the same shape).
Adapted from eval-kit/h1/score.py — the only differences are (a) the gold file is a
single external JSON path taken from a CLI arg (mirroring eval-kit/e2's per-query
{origin, grades} data, folded into one file instead of e2's split
queries.json/mapping.json/grades.json — Lane A's gold builder produces this shape), and
(b) this docstring. Scoring math (origin-MRR over the mapped-origin subset, nDCG@10 over
all queries, Recall@10(>=2) over graded pools) is identical to h1.

Gold file shape (one JSON object; a bare top-level array of the same items is also
accepted):
{
  "queries": [
    {
      "id": "A1",
      "text": "...",                   # optional here — canonical text lives in the
                                         # queries file passed to codegraph_ablation.rs
      "origin": "<conv-id>" | null,     # or "origin_conv_id"; null/absent = UNRESOLVED,
                                         # excluded from origin-MRR (same convention as
                                         # e2/mapping.json's unmapped_unresolved list)
      "grades": {"<conv-id>": 2, ...},  # or "graded"/"relevant" — graded relevance pool,
                                         # 0-3, same convention as e2/grades.json items
      "receipt": "...",                 # artifact receipt (commit/file) — informational
      "rationale": "..."                # informational
    },
    ...
  ]
}

Run: python3 eval-kit/ag/score.py <gold.json> [ablation.jsonl]
(ablation.jsonl defaults to eval-kit/ag/ablation.jsonl next to this script)
"""
import json, math, os, sys
from collections import defaultdict

AG = os.path.dirname(os.path.abspath(__file__))

if len(sys.argv) < 2:
    sys.exit("usage: score.py <gold.json> [ablation.jsonl]")
gold_path = sys.argv[1]
ablation_path = sys.argv[2] if len(sys.argv) > 2 else os.path.join(AG, "ablation.jsonl")

with open(gold_path) as f:
    gold_raw = json.load(f)

gold_queries = (
    gold_raw["queries"]
    if isinstance(gold_raw, dict) and "queries" in gold_raw
    else gold_raw
)

G = {}  # qid -> {"items": {conv: {"grade": n}}}  (same shape as e2/grades.json's ["grades"])
ORIGIN = {}  # qid -> origin conv id or None

if isinstance(gold_queries, dict):
    # Anukriti-gold shape (Lane A builder, gold['queries'] keyed by qid) per
    # meta.score_py_adapter_note: 'items' is grades.json's per-key-compatible field
    # name for 'grades', and 'origin' is mapping.json's M['mapped'][qid]['origin']
    # folded in directly (no separate mapping file to fall back on).
    for qid, item in gold_queries.items():
        grades = item.get("items") or item.get("grades") or item.get("graded") or item.get("relevant") or {}
        G[qid] = {
            "items": {
                conv: {"grade": g["grade"] if isinstance(g, dict) else g}
                for conv, g in grades.items()
            }
        }
        origin = item["origin"] if "origin" in item else item.get("origin_conv_id")
        ORIGIN[qid] = origin or None
elif isinstance(gold_queries, list):
    for item in gold_queries:
        qid = item["id"]
        grades = item.get("grades") or item.get("graded") or item.get("relevant") or {}
        G[qid] = {"items": {conv: {"grade": g} for conv, g in grades.items()}}
        origin = item["origin"] if "origin" in item else item.get("origin_conv_id")
        ORIGIN[qid] = origin or None
else:
    sys.exit(
        f"{gold_path}: expected a top-level array, an object with a \"queries\" array, "
        'or an object with a "queries" dict keyed by qid'
    )

runs = defaultdict(dict)  # arm -> qid -> convs
meta = None
with open(ablation_path) as f:
    for line in f:
        d = json.loads(line)
        if "meta" in d:
            meta = d["meta"]
            continue
        runs[d["arm"]][d["qid"]] = d["convs"]


def grade(qid, conv):
    v = G.get(qid, {"items": {}})["items"].get(conv)
    return v["grade"] if v else None  # None = ungraded (outside the gold pool)


def ndcg(convs, qid, k=10):
    qg = {c: v["grade"] for c, v in G.get(qid, {"items": {}})["items"].items()}
    dcg = sum(
        (2 ** (grade(qid, c) or 0) - 1) / math.log2(i + 2) for i, c in enumerate(convs[:k])
    )
    ideal = sorted(qg.values(), reverse=True)[:k]
    idcg = sum((2**g - 1) / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg > 0 else None


def recall2(convs, qid, k=10):
    rel = {c for c, v in G.get(qid, {"items": {}})["items"].items() if v["grade"] >= 2}
    return len(rel & set(convs[:k])) / len(rel) if rel else None


def mrr(convs, qid):
    origin = ORIGIN.get(qid)
    if not origin:
        return None
    return 1.0 / (convs.index(origin) + 1) if origin in convs else 0.0


def avg(xs):
    xs = [x for x in xs if x is not None]
    return round(sum(xs) / len(xs), 3) if xs else None


# AG codegraph ablation arms (see examples/codegraph_ablation.rs) — identical to h1:
#   S       = no expansion (common base only)
#   S_F     = + file co-edit spread (H2 channel)
#   S_A     = + AST structural spread (H1 channel)
#   S_Asham = + AST spread over degree-preserving shuffled edges (H1 control)
#   S_FA    = + file + AST spread
order = ["S", "S_F", "S_A", "S_Asham", "S_FA"]
print("index build:", json.dumps(meta))
print(f"{'arm':<12}{'oMRR':>7}{'nDCG@10':>9}{'R>=2@10':>9}{'ungraded':>10}")
table = {}
per_query = {}
for arm in order:
    if arm not in runs:
        print(f"{arm:<12}  MISSING")
        continue
    qs = runs[arm]
    ung = sum(1 for qid, convs in qs.items() for c in convs[:10] if grade(qid, c) is None)
    per_query[arm] = {
        "mrr": {q: mrr(v, q) for q, v in qs.items()},
        "ndcg10": {q: ndcg(v, q) for q, v in qs.items()},
        "recall2": {q: recall2(v, q) for q, v in qs.items()},
    }
    row = {
        "origin_mrr": avg(list(per_query[arm]["mrr"].values())),
        "ndcg10": avg(list(per_query[arm]["ndcg10"].values())),
        "recall2": avg(list(per_query[arm]["recall2"].values())),
        "ungraded_at10": ung,
        "n_queries": len(qs),
    }
    table[arm] = row
    print(f"{arm:<12}{row['origin_mrr']!s:>7}{row['ndcg10']!s:>9}{row['recall2']!s:>9}{ung:>10}")

with open(os.path.join(AG, "results.json"), "w") as f:
    json.dump(
        {"meta": meta, "gold_path": gold_path, "arms": table, "per_query": per_query},
        f,
        indent=1,
    )
print("\nresults.json written (includes per_query for bootstrap.py)")
