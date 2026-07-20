#!/usr/bin/env python3
"""E2 digest builder: per query, per pool conversation, extract operator turns +
edit events touching the target file from the original JSONL transcripts.
Deterministic, no judgment. Output: e2/digests/<qid>.md + e2/digest_index.json"""
import json, os, glob, re

E2 = os.path.dirname(os.path.abspath(__file__))
PROJ = "$HOME/.claude/projects"
QUERIES = {q["id"]: q for q in json.load(open(os.path.join(E2, "queries.json")))}
POOLS = json.load(open(os.path.join(E2, "pools.json")))
OUTD = os.path.join(E2, "digests")
os.makedirs(OUTD, exist_ok=True)

TURN_CAP = 30          # max operator turns per conversation
TURN_LEN = 500         # chars per turn
EDIT_CAP = 15          # max edit events listed

def find_jsonl(conv_id):
    hits = glob.glob(os.path.join(PROJ, "*", conv_id + ".jsonl"))
    return hits[0] if hits else None

def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for c in content:
            if isinstance(c, dict) and c.get("type") == "text":
                parts.append(c.get("text", ""))
        return "\n".join(parts)
    return ""

SQLITE = "/opt/homebrew/opt/sqlite/bin/sqlite3"
DB = "$HOME/.claude-self-reflect/csr-engine.db"

def digest_conv_db(conv_id, target, in_file_touch):
    """Fallback for purged JSONLs: reconstruct from CSR DB chunks (pre-freeze rows, immutable).
    Lower fidelity: no role separation, no tool-event stream — disclosed via source marker."""
    import subprocess
    q = ("SELECT substr(content,1,400) FROM chunks WHERE conversation_id='"
         + conv_id.replace("'", "''") + "' ORDER BY rowid LIMIT 25")
    out = subprocess.run([SQLITE, DB, q], capture_output=True, text=True)
    rows = [r for r in out.stdout.split("\n") if r.strip()]
    if not rows:
        return None
    q2 = ("SELECT MIN(timestamp), MAX(timestamp) FROM chunks WHERE conversation_id='"
          + conv_id.replace("'", "''") + "'")
    ts = subprocess.run([SQLITE, DB, q2], capture_output=True, text=True).stdout.strip().split("|")
    return {
        "conv_id": conv_id, "jsonl": None, "db_source": True,
        "first_ts": ts[0] if ts else None, "last_ts": ts[-1] if ts else None,
        "user_turns": [("", r.replace("\n", " ")) for r in rows],
        "target_edits": [], "target_via_code_graph": in_file_touch,
        "other_edit_count": -1,
        "sidechain": conv_id.startswith("agent-"),
    }

def digest_conv(conv_id, target, in_file_touch=False):
    path = find_jsonl(conv_id)
    if not path:
        return digest_conv_db(conv_id, target, in_file_touch)
    tbase = os.path.basename(target) if target else None
    user_turns, edits, first_ts, last_ts = [], [], None, None
    with open(path, errors="replace") as f:
        for line in f:
            try:
                rec = json.loads(line)
            except Exception:
                continue
            ts = rec.get("timestamp")
            if ts:
                first_ts = first_ts or ts
                last_ts = ts
            typ = rec.get("type")
            msg = rec.get("message") or {}
            if typ == "user" and not rec.get("isMeta"):
                t = text_of(msg.get("content"))
                if t and not t.startswith("<local-command") and "tool_result" not in str(msg.get("content"))[:60]:
                    if len(t.strip()) > 2:
                        user_turns.append((ts or "", t.strip()[:TURN_LEN]))
            elif typ == "assistant":
                for c in (msg.get("content") or []):
                    if isinstance(c, dict) and c.get("type") == "tool_use" and c.get("name") in ("Edit", "Write", "MultiEdit", "NotebookEdit"):
                        fp = (c.get("input") or {}).get("file_path", "")
                        if fp:
                            edits.append((ts or "", fp))
    # prioritize turns mentioning the target basename, keep chronological order
    if tbase and len(user_turns) > TURN_CAP:
        scored = [(i, ts, t) for i, (ts, t) in enumerate(user_turns)]
        keep = set()
        for i, ts, t in scored:
            if tbase.split(".")[0].lower() in t.lower():
                keep.add(i)
        rest = [i for i, _, _ in scored if i not in keep]
        # fill with head+tail turns
        for i in rest[: (TURN_CAP - len(keep)) // 2]:
            keep.add(i)
        for i in rest[-((TURN_CAP - len(keep))):] if TURN_CAP > len(keep) else []:
            keep.add(i)
        user_turns = [user_turns[i] for i in sorted(keep)][:TURN_CAP]
    else:
        user_turns = user_turns[:TURN_CAP]
    target_edits = [(ts, fp) for ts, fp in edits if tbase and tbase in fp]
    other_edit_count = len(edits) - len(target_edits)
    return {
        "conv_id": conv_id, "jsonl": path,
        "first_ts": first_ts, "last_ts": last_ts,
        "user_turns": user_turns,
        "target_edits": target_edits[:EDIT_CAP],
        "other_edit_count": other_edit_count,
        "sidechain": conv_id.startswith("agent-"),
    }

index = {}
for qid, pool in POOLS.items():
    q = QUERIES[qid]
    lines = [f"# DIGEST {qid}: {q['text']}", f"TARGET: {q['target'] or '(none)'}", ""]
    found, missing = [], []
    for conv in pool["pool"]:
        d = digest_conv(conv, q["target"], in_file_touch=(conv in pool["file_touch"]))
        if d is None:
            missing.append(conv)
            continue
        found.append(conv)
        src = "[SOURCE: db-chunks — JSONL purged; mixed roles, infer operator turns]" if d.get("db_source") else ""
        lines.append(f"## CONV {conv} {'[SIDECHAIN/subagent]' if d['sidechain'] else '[MAIN session]'} {src}")
        lines.append(f"span: {d['first_ts']} .. {d['last_ts']}")
        if d.get("target_via_code_graph"):
            lines.append("TARGET-FILE LINK: code-graph records this conversation modified the target file (edit stream unavailable)")
        if d["target_edits"]:
            lines.append(f"EDITS TOUCHING TARGET ({len(d['target_edits'])}):")
            for ts, fp in d["target_edits"]:
                lines.append(f"  {ts} {fp}")
        else:
            lines.append("EDITS TOUCHING TARGET: none")
        lines.append(f"(other-file edit events in conv: {d['other_edit_count']})")
        lines.append(f"OPERATOR TURNS ({len(d['user_turns'])} shown):")
        for ts, t in d["user_turns"]:
            t1 = t.replace("\n", " ")
            lines.append(f"  [{ts}] {t1}")
        lines.append("")
    open(os.path.join(OUTD, qid + ".md"), "w").write("\n".join(lines))
    index[qid] = {"digest": os.path.join(OUTD, qid + ".md"), "convs": found, "missing_jsonl": missing}
    print(qid, f"convs={len(found)} missing={len(missing)} bytes={os.path.getsize(os.path.join(OUTD, qid + '.md'))}")

json.dump(index, open(os.path.join(E2, "digest_index.json"), "w"), indent=1)
print("digest_index.json written")
