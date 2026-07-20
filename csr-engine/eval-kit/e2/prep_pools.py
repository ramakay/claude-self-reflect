#!/usr/bin/env python3
"""E2 pool builder: parse frozen probe rank lists + date-filtered code_evolution file-touch.
Output: e2/pools.json {qid: {arm_a: [conv...], arm_b: [conv...], file_touch: [conv...], pool: [conv...]}}"""
import json, os, re, subprocess, glob

SP = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
E2 = os.path.join(SP, "e2")
DB = "$HOME/.claude-self-reflect/csr-engine.db"
SQLITE = "/opt/homebrew/opt/sqlite/bin/sqlite3"
FREEZE = "2026-07-15"

QUERIES = json.load(open(os.path.join(E2, "queries.json")))

def parse_probe(path):
    """Extract ranked conv ids from a probe output file (order = rank).
    Arm B: 'conv_<id>:' headers. Arm A (MCP XML): '<cid>id</cid>' per <r rank=N>."""
    convs = []
    text = open(path).read()
    for m in re.finditer(r"^conv_([A-Za-z0-9-]+):", text, re.M):
        if m.group(1) not in convs:
            convs.append(m.group(1))
    if not convs:
        for m in re.finditer(r"<cid>([A-Za-z0-9-]+)</cid>", text):
            if m.group(1) not in convs:
                convs.append(m.group(1))
    return convs

def file_touch(target):
    if not target:
        return []
    q = ("SELECT DISTINCT ce.session_id FROM code_evolution ce "
         "WHERE ce.file_path LIKE '%' || ? "
         "AND EXISTS (SELECT 1 FROM chunks c WHERE c.conversation_id = ce.session_id "
         "            AND c.timestamp <= '" + FREEZE + "T23:59:59Z')")
    out = subprocess.run([SQLITE, DB, q], input=target, capture_output=True, text=True)
    # param binding via stdin doesn't work for sqlite3 CLI; inline safely (targets are our own constants)
    q2 = q.replace("?", "'" + target.replace("'", "''") + "'")
    out = subprocess.run([SQLITE, DB, q2], capture_output=True, text=True)
    return [l.strip() for l in out.stdout.splitlines() if l.strip()]

pools = {}
for q in QUERIES:
    qid = q["id"]
    probe_dir = os.path.join(SP, "probe-out" if qid.startswith("Q") else "probe-out2")
    n = int(qid[1:])
    a = parse_probe(os.path.join(probe_dir, f"q{n:02d}_a.txt"))
    b = parse_probe(os.path.join(probe_dir, f"q{n:02d}_b.txt"))
    ft = file_touch(q.get("target", ""))
    pool = []
    for c in a + b + ft:
        if c not in pool:
            pool.append(c)
    pools[qid] = {"arm_a": a, "arm_b": b, "file_touch": ft, "pool": pool}
    print(qid, "a=%d b=%d ft=%d pool=%d" % (len(a), len(b), len(ft), len(pool)))

json.dump(pools, open(os.path.join(E2, "pools.json"), "w"), indent=1)
print("pools.json written")
