#!/usr/bin/env python3
"""T4 Tier B — episode ancestry labels.

Free labels from git history, no eval design needed: every commit reachable
from HEAD in the outer claude-self-reflect repo (read-only) is labeled by
which release first shipped it, whether it was later reverted, or whether it
has never shipped at all. Then joined (read-only) to the live CSR corpus DB
to see how many of those commits already have a session attributed to them.

Labeling algorithm (deterministic, no RNG):
  1. List all `v*` tags, version-sorted (`--sort=version:refname`).
  2. Walk tags oldest-to-newest. For each tag T_cur, the "newly shipped"
     commits are `git rev-list T_prev..T_cur` (or the full ancestry of the
     first tag) MINUS whatever a strictly earlier tag in this walk already
     claimed — this stays correct even if the tag sequence doesn't track a
     single linear branch (backports, hotfix tags out of ancestry order),
     because it is "first tag in version order under which this commit
     becomes claimed", not "first tag by commit date".
  3. `label` defaults to 'shipped' if some tag claimed the commit, else
     'unreleased' (reachable from HEAD, in no release tag's ancestry).
  4. Revert detection overrides `label` to 'reverted' for: (a) any commit
     whose subject matches `^Revert` (case-insensitive — `git revert`'s
     default subject), and (b) the ORIGINAL commit named in that revert's
     "This reverts commit <sha>" trailer, when that sha is one of ours.
     `release_tag` is untouched by this override — it still records which
     release the code shipped in, `label` just says it didn't survive.

Session linkage: two-hop join through `code_node_attribution` (populated by
`codegraph backfill-attribution`) — `channel='git', source_id=<sha>` gives
the node(s) that commit touched; `channel='transcript'` on the SAME
node_id gives the session that authored it. Read via a `file:...?mode=ro`
URI against the live DB — SELECT-only, never opened for write.

Usage:
    python3 labels.py [--repo PATH] [--csr-db PATH] [--out-dir PATH]

Writes labels.csv (commit, release_tag, label, session_id) and
labels_summary.json (coverage stats + per-tag shipped counts) to --out-dir.
"""

import argparse
import csv
import json
import re
import sqlite3
import subprocess
import sys
from pathlib import Path

REVERT_SUBJECT_RE = re.compile(r"^Revert\b", re.IGNORECASE)
REVERTS_COMMIT_RE = re.compile(r"This reverts commit ([0-9a-f]{7,40})", re.IGNORECASE)


def run(cmd):
    return subprocess.run(cmd, capture_output=True, text=True, check=True).stdout


def list_all_tags(repo: str) -> list[str]:
    out = run(["git", "-C", repo, "tag", "--list", "v*", "--sort=version:refname"])
    return [t for t in out.splitlines() if t]


def rev_list(repo: str, revspec: str) -> list[str]:
    """`git rev-list <revspec>`, one sha per line, oldest-ancestry-first
    ordering not guaranteed — we only use this as a set, order doesn't
    matter for correctness (see the caller's set-difference logic)."""
    out = run(["git", "-C", repo, "rev-list", revspec])
    return [s for s in out.splitlines() if s]


def build_shipped_map(repo: str, tags: list[str]) -> dict[str, str]:
    """commit sha -> release_tag, first tag in version order that claims
    each commit. Only commits reachable from some tag appear here."""
    claimed: dict[str, str] = {}
    already: set[str] = set()
    for i, tag in enumerate(tags):
        revspec = tag if i == 0 else f"{tags[i - 1]}..{tag}"
        shas = rev_list(repo, revspec)
        new = [s for s in shas if s not in already]
        for s in new:
            claimed[s] = tag
            already.add(s)
    return claimed


def commit_subject_and_body(repo: str, sha: str) -> tuple[str, str]:
    out = run(["git", "-C", repo, "log", "-1", "--format=%s%x00%B", sha])
    subject, _, body = out.partition("\x00")
    return subject, body


def find_reverts(repo: str, all_shas: set[str]) -> set[str]:
    """Shas that must be labeled 'reverted': revert commits themselves, and
    the original commits they name (when that original is one of ours)."""
    reverted: set[str] = set()
    for sha in sorted(all_shas):
        subject, body = commit_subject_and_body(repo, sha)
        if REVERT_SUBJECT_RE.match(subject):
            reverted.add(sha)
            m = REVERTS_COMMIT_RE.search(body)
            if m:
                target_prefix = m.group(1).lower()
                # `This reverts commit <sha>` may be abbreviated; match by
                # prefix against our known commit set (deterministic: at
                # most one full sha in a real repo can share a short prefix
                # at the lengths git uses, and if two DID collide we'd
                # rather flag ambiguity than guess).
                matches = [s for s in all_shas if s.lower().startswith(target_prefix)]
                if len(matches) == 1:
                    reverted.add(matches[0])
    return reverted


def load_git_to_session(csr_db: str) -> tuple[dict[str, str], dict[str, int]]:
    """sha -> session_id via the code_node_attribution two-hop join
    (channel='git' node -> channel='transcript' node, same node_id).
    Returns (mapping, {sha: n_distinct_sessions_seen}) for transparency
    when a commit's nodes fan out to more than one session — the mapping
    picks the lexicographically smallest session_id for determinism."""
    uri = f"file:{csr_db}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True)
        rows = conn.execute(
            """
            SELECT g.source_id AS sha, t.source_id AS session_id
            FROM code_node_attribution g
            JOIN code_node_attribution t ON t.node_id = g.node_id
            WHERE g.channel = 'git' AND t.channel = 'transcript'
            """
        ).fetchall()
        conn.close()
    except sqlite3.Error as e:
        print(f"[labels] WARNING: could not read {csr_db} read-only ({e}); session linkage will be empty", file=sys.stderr)
        return {}, {}

    by_sha: dict[str, set[str]] = {}
    for sha, session_id in rows:
        by_sha.setdefault(sha, set()).add(session_id)
    mapping = {sha: sorted(sessions)[0] for sha, sessions in by_sha.items()}
    fanout = {sha: len(sessions) for sha, sessions in by_sha.items() if len(sessions) > 1}
    return mapping, fanout


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--repo",
        default="/Users/ramakrishnanannaswamy/projects/claude-self-reflect",
        help="Outer repo to walk (read-only).",
    )
    ap.add_argument(
        "--csr-db",
        default="/Users/ramakrishnanannaswamy/.claude-self-reflect/csr-engine.db",
        help="Live CSR DB, opened strictly read-only via a file: URI (mode=ro). Never written.",
    )
    ap.add_argument("--out-dir", default=str(Path(__file__).resolve().parent))
    args = ap.parse_args()

    repo = args.repo
    tags = list_all_tags(repo)
    if not tags:
        raise SystemExit(f"no v* tags found in {repo}")
    print(f"[labels] {len(tags)} release tags, first={tags[0]} last={tags[-1]}")

    all_reachable = rev_list(repo, "HEAD")  # preserves git's deterministic output order
    all_set = set(all_reachable)
    print(f"[labels] {len(all_reachable)} commits reachable from HEAD")

    shipped_map = build_shipped_map(repo, tags)
    reverted_shas = find_reverts(repo, all_set)
    session_map, fanout = load_git_to_session(args.csr_db)

    rows = []
    for sha in all_reachable:
        release_tag = shipped_map.get(sha)
        if sha in reverted_shas:
            label = "reverted"
        elif release_tag is not None:
            label = "shipped"
        else:
            label = "unreleased"
        rows.append(
            {
                "commit": sha,
                "release_tag": release_tag,
                "label": label,
                "session_id": session_map.get(sha),
            }
        )

    n_total = len(rows)
    n_labeled = sum(1 for r in rows if r["release_tag"] is not None)
    n_shipped = sum(1 for r in rows if r["label"] == "shipped")
    n_reverted = sum(1 for r in rows if r["label"] == "reverted")
    n_unreleased = sum(1 for r in rows if r["label"] == "unreleased")
    n_with_session = sum(1 for r in rows if r["session_id"] is not None)

    per_tag_counts: dict[str, int] = {}
    for r in rows:
        if r["release_tag"] is not None:
            per_tag_counts[r["release_tag"]] = per_tag_counts.get(r["release_tag"], 0) + 1
    per_tag_table = [{"release_tag": t, "commits_shipped": per_tag_counts.get(t, 0)} for t in tags]

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    with open(out_dir / "labels.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["commit", "release_tag", "label", "session_id"])
        for r in rows:
            w.writerow([r["commit"], r["release_tag"] or "", r["label"], r["session_id"] or ""])

    summary = {
        "repo": repo,
        "csr_db": args.csr_db,
        "n_tags": len(tags),
        "first_tag": tags[0],
        "last_tag": tags[-1],
        "n_commits_reachable": n_total,
        "n_labeled_shipped_or_reverted": n_labeled,
        "pct_labeled": round(100.0 * n_labeled / n_total, 2) if n_total else None,
        "n_shipped": n_shipped,
        "n_reverted": n_reverted,
        "n_unreleased": n_unreleased,
        "n_with_session_linkage": n_with_session,
        "pct_with_session_linkage": round(100.0 * n_with_session / n_total, 2) if n_total else None,
        "n_commits_with_session_fanout_gt1": len(fanout),
        "per_tag_shipped_counts": per_tag_table,
    }
    (out_dir / "labels_summary.json").write_text(json.dumps(summary, indent=2))

    print(f"\n[labels] {n_total} commits reachable from HEAD")
    print(f"[labels] labeled shipped/reverted : {n_labeled} ({summary['pct_labeled']}%)")
    print(f"[labels]   shipped    : {n_shipped}")
    print(f"[labels]   reverted   : {n_reverted}")
    print(f"[labels]   unreleased : {n_unreleased}")
    print(f"[labels] session linkage: {n_with_session} ({summary['pct_with_session_linkage']}%)")
    if fanout:
        print(f"[labels] {len(fanout)} commits had >1 distinct session in the join (picked min session_id deterministically)")
    print(f"\n[labels] wrote {out_dir / 'labels.csv'} and labels_summary.json")

    return 0


if __name__ == "__main__":
    sys.exit(main())
