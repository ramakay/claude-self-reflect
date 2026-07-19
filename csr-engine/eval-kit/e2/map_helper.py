#!/usr/bin/env python3
"""Metadata-only mapping helper: literal LIKE search over pre-freeze chunks + git file intro.
Usage: map_helper.py <qid> <git_repo> <target_file> <kw1> [kw2 ...]
Prints candidate conversations (date-ordered) whose chunks literally contain ALL keywords,
plus the git introduction/major commits of the target. NO embedding search."""
import sys, subprocess, os

SQLITE = "/opt/homebrew/opt/sqlite/bin/sqlite3"
DB = "$HOME/.claude-self-reflect/csr-engine.db"
FREEZE = "2026-07-15T23:59:59Z"

qid, repo, target = sys.argv[1], sys.argv[2], sys.argv[3]
kws = sys.argv[4:]

if target and repo != "-":
    print(f"--- git history of {target} (oldest 3 + newest 3 commits) ---")
    log = subprocess.run(["git", "-C", repo, "log", "--follow", "--reverse",
                          "--pretty=format:%h %aI %s", "--", target],
                         capture_output=True, text=True).stdout.splitlines()
    for l in log[:3]: print(" ", l)
    if len(log) > 6: print("   ...")
    for l in log[-3:]: print(" ", l)

like = " AND ".join(["content LIKE '%" + k.replace("'", "''") + "%'" for k in kws])
q = (f"SELECT conversation_id, MIN(timestamp), COUNT(*) FROM chunks WHERE {like} "
     f"AND timestamp <= '{FREEZE}' GROUP BY conversation_id ORDER BY MIN(timestamp) LIMIT 20")
out = subprocess.run([SQLITE, DB, q], capture_output=True, text=True).stdout
print(f"--- convs with ALL keywords {kws} (pre-freeze) ---")
print(out if out.strip() else "  (none)")
