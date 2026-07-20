#!/usr/bin/env python3
"""E3 scorer: out/{c0,c1,csham,c5}.jsonl -> origin rank, echo@10, displacement, repair delta.
Origins = E2 owner-audited mapping (5/5 injected maps confirmed)."""
import json, os
from collections import defaultdict

E3 = os.path.dirname(os.path.abspath(__file__))
ORIGINS = {
    "Q5": "219ef49f-e007-4b56-bda8-b848516dc9f8",
    "Q9": "02d270b9-f48d-4bb4-bd03-9017e1723fa3",
    "Q12": "8b266e91-292b-4e39-bdaa-29d32b9e22f5",
    "A3": "71b8d18d-450b-4cd1-aba5-6db7fdcfae6e",
    "A5": "46d2386b-c3e9-4f09-a695-ca7de9e7ae84",
    "A6": "92fcab9e-a193-405e-92f6-34c72edc9cd2",
    "A7": "a3e84413-c8f2-481e-8661-8086accd8fcc",
    "A8": "92fcab9e-a193-405e-92f6-34c72edc9cd2",
}
CONDS = ["c0", "c1", "csham", "c5"]
data = {}
for c in CONDS:
    p = os.path.join(E3, "out", f"{c}.jsonl")
    if not os.path.exists(p):
        print(f"MISSING {p}")
        continue
    runs, meta = defaultdict(dict), None
    for line in open(p):
        d = json.loads(line)
        if "meta" in d:
            meta = d["meta"]
        else:
            runs[d["arm"]][d["qid"]] = d
    data[c] = {"meta": meta, "runs": runs}

def orank(convs, qid):
    o = ORIGINS[qid]
    return convs.index(o) + 1 if o in convs else None  # None = not in top-10

def echo10(chunks):
    return sum(1 for ch in chunks[:10] if ch.get("echo"))

print("=== meta ===")
for c in CONDS:
    if c in data:
        print(c, json.dumps(data[c]["meta"]))

for arm in ["knn_exact", "full_exact", "full_no_echo_exact"]:
    print(f"\n=== {arm}: origin rank per condition (None = outside top-10) ===")
    print(f"{'qid':<5}" + "".join(f"{c:>8}" for c in CONDS) + f"{'echo@10 c0->c5':>18}")
    mrr = {c: [] for c in CONDS}
    for qid in ORIGINS:
        row = f"{qid:<5}"
        for c in CONDS:
            r = data.get(c, {}).get("runs", {}).get(arm, {}).get(qid)
            rk = orank(r["convs"], qid) if r else "?"
            row += f"{str(rk):>8}"
            if r:
                mrr[c].append(1.0 / rk if isinstance(rk, int) else 0.0)
        r0 = data.get("c0", {}).get("runs", {}).get(arm, {}).get(qid)
        e0 = echo10(r0["chunks"]) if r0 else "?"
        r5 = data.get("c5", {}).get("runs", {}).get(arm, {}).get(qid)
        e5 = echo10(r5["chunks"]) if r5 else "?"
        row += f"{str(e0)+' -> '+str(e5):>18}"
        print(row)
    print("oMRR " + "".join(f"{round(sum(v)/len(v),3) if v else '?':>8}" for v in (mrr[c] for c in CONDS)))

# displacement + repair
print("\n=== displacement events (origin rank worsens vs c0) ===")
for arm in ["knn_exact", "full_exact", "full_no_echo_exact"]:
    for cond in ["c1", "csham", "c5"]:
        ev = 0
        for qid in ORIGINS:
            try:
                r0 = orank(data["c0"]["runs"][arm][qid]["convs"], qid)
                rc = orank(data[cond]["runs"][arm][qid]["convs"], qid)
            except KeyError:
                continue
            r0v = r0 if r0 else 99
            rcv = rc if rc else 99
            if rcv > r0v:
                ev += 1
        print(f"{arm:<22} {cond}: {ev}/8 queries displaced")
