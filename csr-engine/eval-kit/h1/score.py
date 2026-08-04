#!/usr/bin/env python3
"""H1/H2 scorer: ablation.jsonl (5 arms x 20 queries) vs E2 graded gold.
Thin adaptation of eval-kit/e1/score.py — only the `order` list (5 codegraph-ablation
arm names instead of the 7 saga-ablation arm names) and this docstring differ; scoring
math (origin-MRR over 12 mapped queries, nDCG@10, Recall@10(>=2) over graded pools) is
identical. Convs outside the E2 graded pool score 0 and are counted as `ungraded`."""
import json, math, os, sys
from collections import defaultdict

H1 = os.path.dirname(os.path.abspath(__file__))
E2 = os.path.join(os.path.dirname(H1), "e2")
with open(os.path.join(E2, "grades.json")) as f:
    G = json.load(f)["grades"]
with open(os.path.join(E2, "mapping.json")) as f:
    M = json.load(f)

ablation_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(H1, "ablation.jsonl")

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
    v = G[qid]["items"].get(conv)
    return v["grade"] if v else None  # None = ungraded (outside E2 pool)

def ndcg(convs, qid, k=10):
    qg = {c: v["grade"] for c, v in G[qid]["items"].items()}
    dcg = sum((2 ** (grade(qid, c) or 0) - 1) / math.log2(i + 2) for i, c in enumerate(convs[:k]))
    ideal = sorted(qg.values(), reverse=True)[:k]
    idcg = sum((2 ** g - 1) / math.log2(i + 2) for i, g in enumerate(ideal))
    return dcg / idcg if idcg > 0 else None

def recall2(convs, qid, k=10):
    rel = {c for c, v in G[qid]["items"].items() if v["grade"] >= 2}
    return len(rel & set(convs[:k])) / len(rel) if rel else None

def mrr(convs, qid):
    origin = (M["mapped"].get(qid) or {}).get("origin")
    if not origin:
        return None
    return 1.0 / (convs.index(origin) + 1) if origin in convs else 0.0

def avg(xs):
    xs = [x for x in xs if x is not None]
    return round(sum(xs) / len(xs), 3) if xs else None

# H1/H2 codegraph ablation arms (see examples/codegraph_ablation.rs):
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
    row = {"origin_mrr": avg(list(per_query[arm]["mrr"].values())),
           "ndcg10": avg(list(per_query[arm]["ndcg10"].values())),
           "recall2": avg(list(per_query[arm]["recall2"].values())),
           "ungraded_at10": ung, "n_queries": len(qs)}
    table[arm] = row
    print(f"{arm:<12}{row['origin_mrr']!s:>7}{row['ndcg10']!s:>9}{row['recall2']!s:>9}{ung:>10}")

with open(os.path.join(H1, "results.json"), "w") as f:
    json.dump({"meta": meta, "arms": table, "per_query": per_query}, f, indent=1)
print("\nresults.json written (includes per_query for bootstrap.py)")
