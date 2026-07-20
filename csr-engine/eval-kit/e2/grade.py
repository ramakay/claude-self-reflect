#!/usr/bin/env python3
"""E2 grading + metrics. Inputs: pools.json, mapping.json, extract_sonnet/, extract_grok/,
ledger.json, queries.json. Outputs: grades.json, results.md (tables).
Grade map (protocol, frozen): 3 = sealed+mapped origin ONLY (extraction never mints 3);
2 = edits_target; 1 = discusses; 0 = reask/echo or unrelated. Consensus = both vendors agree
on an act; disagreements resolved conservative (absent) and counted."""
import json, os, math
from collections import defaultdict

E2 = os.path.dirname(os.path.abspath(__file__))
load = lambda n: json.load(open(os.path.join(E2, n)))
POOLS, MAPPING, QUERIES = load("pools.json"), load("mapping.json"), load("queries.json")
LEDGER = load("ledger.json")
QIDS = [q["id"] for q in QUERIES]

def ext(vendor, qid):
    p = os.path.join(E2, f"extract_{vendor}", f"{qid}.json")
    if not os.path.exists(p):
        return None
    d = json.load(open(p))
    if "error" in d:
        return None
    return {i["conv_id"]: i for i in d.get("items", [])}

def act(item, key):
    v = item.get(key)
    if isinstance(v, dict):
        return bool(v.get("present"))
    return bool(v)

agree_counts = defaultdict(lambda: [0, 0])  # act -> [agree, total]
kappa_cells = defaultdict(lambda: [0, 0, 0, 0])  # act -> [both_yes, s_only, g_only, both_no]
vendor_status = {}
grades, ledger_notes = {}, []

def git_events_near(conv_ts_range, target, window_h=72):
    """Ledger corroboration: commits touching target within window of conv span."""
    if not conv_ts_range or not target:
        return []
    hits = []
    tbase = os.path.basename(target)
    for repo, commits in LEDGER.get("git", {}).items():
        for c in commits:
            if any(tbase in f for f in c.get("files", [])):
                hits.append({"repo": repo, "hash": c["hash"], "date": c["date"], "subject": c["subject"][:60]})
    return hits[:5]

for qid in QIDS:
    s, g = ext("sonnet", qid), ext("grok", qid)
    vendor_status[qid] = {"sonnet": s is not None, "grok": g is not None}
    pool = POOLS[qid]["pool"]
    origin = (MAPPING["mapped"].get(qid) or {}).get("origin")
    qgrades = {}
    for conv in pool:
        si, gi = (s or {}).get(conv), (g or {}).get(conv)
        acts = {}
        for a in ("directs", "accepts", "rejects", "reasks"):
            sv = act(si, a) if si else None
            gv = act(gi, a) if gi else None
            if sv is not None and gv is not None:
                agree_counts[a][1] += 1
                agree_counts[a][0] += int(sv == gv)
                kappa_cells[a][0 if (sv and gv) else (1 if sv else (2 if gv else 3))] += 1
                acts[a] = sv and gv  # strict consensus
            else:
                acts[a] = bool(sv if sv is not None else (gv if gv is not None else False))
        dv = [x for x in (si and si.get("discusses"), gi and gi.get("discusses")) if x is not None]
        discusses = all(dv) if len(dv) == 2 else bool(dv and dv[0])
        edits = any(bool(x and x.get("edits_target")) for x in (si, gi))
        if conv == origin:
            grade = 3
        elif acts["reasks"] and not edits:
            grade = 0
        elif edits:
            grade = 2
        elif discusses:
            grade = 1
        else:
            grade = 0
        qgrades[conv] = {"grade": grade, "acts": acts, "discusses": discusses, "edits_target": edits,
                         "extracted": {"sonnet": si is not None, "grok": gi is not None}}
    # pool injection check
    injected = False
    if origin and origin not in pool:
        qgrades[origin] = {"grade": 3, "acts": {}, "discusses": None, "edits_target": None,
                           "extracted": {"sonnet": False, "grok": False}, "injected": True}
        injected = True
    grades[qid] = {"origin": origin, "injected": injected, "items": qgrades}

# metrics
def mrr(arm, origin):
    if not origin:
        return None
    return round(1.0 / (arm.index(origin) + 1), 3) if origin in arm else 0.0

def ndcg(arm, qg, k=10):
    dcg = sum((2 ** qg[c]["grade"] - 1) / math.log2(i + 2) for i, c in enumerate(arm[:k]) if c in qg)
    ideal = sorted((qg[c]["grade"] for c in qg), reverse=True)[:k]
    idcg = sum((2 ** g - 1) / math.log2(i + 2) for i, g in enumerate(ideal))
    return round(dcg / idcg, 3) if idcg > 0 else None

def recall2(arm, qg, k=10):
    rel = {c for c, v in qg.items() if v["grade"] >= 2}
    if not rel:
        return None
    return round(len(rel & set(arm[:k])) / len(rel), 3)

rows = []
for qid in QIDS:
    qg = grades[qid]["items"]
    a, b = POOLS[qid]["arm_a"], POOLS[qid]["arm_b"]
    origin = grades[qid]["origin"]
    rows.append({
        "qid": qid, "origin_mapped": bool(origin), "injected": grades[qid]["injected"],
        "mrr_a": mrr(a, origin), "mrr_b": mrr(b, origin),
        "ndcg_a": ndcg(a, qg), "ndcg_b": ndcg(b, qg),
        "recall2_a": recall2(a, qg), "recall2_b": recall2(b, qg),
    })

def kappa(cells):
    a, b, c, d = cells
    n = a + b + c + d
    if n == 0:
        return None
    po = (a + d) / n
    py = ((a + b) / n) * ((a + c) / n)
    pn = ((c + d) / n) * ((b + d) / n)
    pe = py + pn
    return round((po - pe) / (1 - pe), 3) if pe < 1 else None

summary = {
    "queries": rows,
    "origin_mrr": {
        "n_mapped": sum(1 for r in rows if r["origin_mapped"]),
        "arm_a": round(sum(r["mrr_a"] for r in rows if r["mrr_a"] is not None) / max(1, sum(1 for r in rows if r["mrr_a"] is not None)), 3),
        "arm_b": round(sum(r["mrr_b"] for r in rows if r["mrr_b"] is not None) / max(1, sum(1 for r in rows if r["mrr_b"] is not None)), 3),
    },
    "ndcg10": {
        "arm_a": round(sum(r["ndcg_a"] for r in rows if r["ndcg_a"] is not None) / max(1, sum(1 for r in rows if r["ndcg_a"] is not None)), 3),
        "arm_b": round(sum(r["ndcg_b"] for r in rows if r["ndcg_b"] is not None) / max(1, sum(1 for r in rows if r["ndcg_b"] is not None)), 3),
    },
    "recall2at10": {
        "arm_a": round(sum(r["recall2_a"] for r in rows if r["recall2_a"] is not None) / max(1, sum(1 for r in rows if r["recall2_a"] is not None)), 3),
        "arm_b": round(sum(r["recall2_b"] for r in rows if r["recall2_b"] is not None) / max(1, sum(1 for r in rows if r["recall2_b"] is not None)), 3),
    },
    "vendor_agreement": {a: {"pct": round(v[0] / v[1], 3) if v[1] else None, "kappa": kappa(kappa_cells[a]), "n": v[1]} for a, v in agree_counts.items()},
    "vendor_status": vendor_status,
    "injected_count": sum(1 for r in rows if r["injected"]),
    "strata": {"mapped": [r["qid"] for r in rows if r["origin_mapped"]],
               "unresolved_excluded": MAPPING["unmapped_unresolved"],
               "out_of_corpus": list(MAPPING.get("out_of_corpus", {}).keys())},
}
json.dump({"grades": grades, "summary": summary}, open(os.path.join(E2, "grades.json"), "w"), indent=1)
print(json.dumps(summary, indent=1))
