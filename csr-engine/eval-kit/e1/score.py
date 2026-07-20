#!/usr/bin/env python3
"""E1 scorer: ablation.jsonl (7 arms x 20 queries) vs E2 graded gold.
Origin-MRR over 12 mapped queries; nDCG@10 / Recall@10(>=2) over graded pools.
Convs outside the E2 graded pool score 0 and are counted as `ungraded` (disclosed)."""
import json, math, os, sys
from collections import defaultdict

E1 = os.path.dirname(os.path.abspath(__file__))
E2 = os.path.join(os.path.dirname(E1), "e2")
with open(os.path.join(E2, "grades.json")) as f:
    G = json.load(f)["grades"]
with open(os.path.join(E2, "mapping.json")) as f:
    M = json.load(f)

runs = defaultdict(dict)  # arm -> qid -> convs
meta = None
with open(os.path.join(E1, "ablation.jsonl")) as f:
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

order = ["a_knn", "b_full", "c_blend_only", "d_graph_only", "e_episode_only", "f_no_rerank", "g_no_echo"]
print("index build:", json.dumps(meta))
print(f"{'arm':<15}{'oMRR':>7}{'nDCG@10':>9}{'R>=2@10':>9}{'ungraded':>10}")
table = {}
for arm in order:
    if arm not in runs:
        print(f"{arm:<15}  MISSING")
        continue
    qs = runs[arm]
    ung = sum(1 for qid, convs in qs.items() for c in convs[:10] if grade(qid, c) is None)
    row = {"origin_mrr": avg([mrr(v, q) for q, v in qs.items()]),
           "ndcg10": avg([ndcg(v, q) for q, v in qs.items()]),
           "recall2": avg([recall2(v, q) for q, v in qs.items()]),
           "ungraded_at10": ung, "n_queries": len(qs)}
    table[arm] = row
    print(f"{arm:<15}{row['origin_mrr']!s:>7}{row['ndcg10']!s:>9}{row['recall2']!s:>9}{ung:>10}")

with open(os.path.join(E1, "results.json"), "w") as f:
    json.dump({"meta": meta, "arms": table}, f, indent=1)
print("\nresults.json written")
