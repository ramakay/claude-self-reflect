#!/usr/bin/env python3
"""T4 Tier A — symbol time-travel replay.

Free labels from git history: stamp `codewitness` committed-tier witnesses at
~13 evenly-sampled release tags between v8.0.0 and v9.5.0 of the outer
claude-self-reflect repo (read-only — never `~/.claude-self-reflect`), then
check a deterministic SQL-only "staleness" rule against the actual future.

For a belief held at an intermediate tag T_i (a (file, symbol) -> stamp pair
witnessed by `codegraph stamp-spans --at T_i`):

  * PREDICTION rule (the practical rule you could compute online from a
    handful of periodic snapshots, mirroring the v10 dream join):
      - superseded  if some LATER sampled tag T_j (i < j, up to and
                     including the final tag) has the same (file, symbol)
                     present with a DIFFERENT stamp than at T_i.
      - obsolete    if the symbol is absent at the final tag.
      - intact      otherwise.
  * GROUND TRUTH (defined directly off the final tag, v9.5.0, the actual
    future the rule is trying to predict):
      - obsolete    if absent at the final tag.
      - superseded  if present at the final tag with a different stamp.
      - intact      if present at the final tag with the same stamp.

"stale" = superseded or obsolete, for both prediction and ground truth.
Because the prediction rule's superseded-check always includes the final
tag itself as one of the "later tags", predicted-stale is structurally a
superset of ground-truth-stale (recall(stale) == 1.0 by construction,
proven not merely observed) — the interesting empirical number is
PRECISION: how often an intermediate tag's stamp changed and then reverted
back to the original value by the final tag, which the rule over-flags as
"superseded" but the ground truth calls "intact".

Usage:
    python3 replay.py [--repo PATH] [--binary PATH] [--scratch-dir PATH]
                       [--tags-count N] [--out-dir PATH] [--keep-db]

Writes tierA_results.json and tierA_results.csv into --out-dir (default:
this script's directory). Deterministic: tag sampling is a pure linspace
index formula (no RNG), git tag listing uses `--sort=version:refname`, and
every iteration order in this script is over an explicitly sorted sequence.
"""

import argparse
import csv
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

FIRST_TAG = "v8.0.0"
LAST_TAG = "v9.5.0"


def run(cmd, **kwargs):
    return subprocess.run(cmd, capture_output=True, text=True, check=True, **kwargs)


def list_tags_in_range(repo: str, first: str, last: str) -> list[str]:
    """All `v*` tags, version-sorted, sliced to the inclusive [first, last]
    range. Deterministic: git's own `version:refname` sort, not date-based."""
    out = run(["git", "-C", repo, "tag", "--list", "v*", "--sort=version:refname"]).stdout
    all_tags = [t for t in out.splitlines() if t]
    if first not in all_tags or last not in all_tags:
        raise SystemExit(f"expected tags {first!r}/{last!r} not found in {repo}")
    i0 = all_tags.index(first)
    i1 = all_tags.index(last)
    if i0 > i1:
        raise SystemExit(f"{first} sorts after {last} — range is empty")
    return all_tags[i0 : i1 + 1]


def evenly_sample(tags: list[str], k: int) -> list[str]:
    """k evenly-spaced tags from `tags`, always including both endpoints.
    Pure integer linspace-index formula — no randomness, so this is
    reproducible byte-for-byte across runs without seeding anything."""
    n = len(tags)
    if k >= n:
        return list(tags)
    if k < 2:
        raise SystemExit("--tags-count must be >= 2 (need both endpoints)")
    idxs = sorted({round(i * (n - 1) / (k - 1)) for i in range(k)})
    return [tags[i] for i in idxs]


def resolve_commit(repo: str, rev: str) -> str:
    return run(["git", "-C", repo, "rev-parse", f"{rev}^{{commit}}"]).stdout.strip()


def stamp_at(binary: str, db_path: str, projects_dir: str, repo: str, tag: str) -> dict:
    """Run `codegraph stamp-spans --at <tag> --repo <repo>` against the
    scratch DB. Returns parsed stats; raises on a non-zero exit (a resolve
    failure for one of our own release tags is not an expected outcome)."""
    proc = run(
        [
            binary,
            "--db-path",
            db_path,
            "--projects-dir",
            projects_dir,
            "codegraph",
            "stamp-spans",
            "--at",
            tag,
            "--repo",
            repo,
        ]
    )
    at_oid = None
    for line in proc.stdout.splitlines():
        line = line.strip()
        if line.startswith("at_commit"):
            at_oid = line.rsplit(":", 1)[-1].strip()
    return {"tag": tag, "stdout": proc.stdout, "at_oid": at_oid}


def load_ledger_by_oid(db_path: str) -> dict[str, dict[tuple[str, str], str]]:
    """oid -> {(file, symbol): stamp} for every 'committed'-tier backfill
    witness in the scratch DB — symbol-level rows only (whole-file NULL-
    symbol fallback rows are excluded; the replay tracks symbol beliefs)."""
    conn = sqlite3.connect(db_path)
    try:
        rows = conn.execute(
            "SELECT at_oid, file, symbol, stamp FROM witness_ledger "
            "WHERE tier = 'committed' AND symbol IS NOT NULL"
        ).fetchall()
    finally:
        conn.close()
    by_oid: dict[str, dict[tuple[str, str], str]] = {}
    for oid, file, symbol, stamp in rows:
        by_oid.setdefault(oid, {})[(file, symbol)] = stamp
    return by_oid


def classify_belief(
    stamp_i: str,
    key: tuple[str, str],
    later_maps: list[dict[tuple[str, str], str]],
    final_map: dict[tuple[str, str], str],
) -> tuple[str, str]:
    """Returns (predicted_label, ground_truth_label)."""
    superseded_pred = any(
        key in m and m[key] != stamp_i for m in later_maps
    )
    obsolete_pred = key not in final_map
    if superseded_pred:
        pred = "superseded"
    elif obsolete_pred:
        pred = "obsolete"
    else:
        pred = "intact"

    final_stamp = final_map.get(key)
    if final_stamp is None:
        gt = "obsolete"
    elif final_stamp != stamp_i:
        gt = "superseded"
    else:
        gt = "intact"
    return pred, gt


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--repo",
        default="/Users/ramakrishnanannaswamy/projects/claude-self-reflect",
        help="Outer repo to replay (read-only).",
    )
    ap.add_argument(
        "--binary",
        default=str(Path(__file__).resolve().parents[2] / "target" / "release" / "csr-engine"),
        help="csr-engine release binary.",
    )
    ap.add_argument(
        "--scratch-dir",
        default=None,
        help="Scratch dir for the throwaway DB + projects-dir (NEVER ~/.claude-self-reflect). "
        "Default: a fresh csr-t4-* temp dir, removed on exit unless --keep-db.",
    )
    ap.add_argument("--tags-count", type=int, default=13)
    ap.add_argument("--out-dir", default=str(Path(__file__).resolve().parent))
    ap.add_argument("--keep-db", action="store_true", help="Don't delete the scratch DB when done.")
    args = ap.parse_args()

    repo = os.path.abspath(args.repo)
    # Resolve to absolute first (the README documents a relative
    # `--binary target/release/csr-engine` invocation), THEN require it to
    # exist — the subprocess later runs with a different cwd.
    binary = os.path.abspath(args.binary)
    if not os.path.exists(binary):
        raise SystemExit(f"binary not found: {binary} (run `cargo build --release` first)")

    # Create the default scratch dir lazily, only when the caller omitted
    # --scratch-dir (mkdtemp in the argparse default would also run for
    # --help / argument errors / explicit --scratch-dir, and its root was
    # never cleaned up).
    owns_scratch_root = args.scratch_dir is None
    scratch_dir = Path(args.scratch_dir or tempfile.mkdtemp(prefix="csr-t4-"))
    db_dir = scratch_dir / "scratch_db"
    projects_dir = scratch_dir / "scratch_projects"
    db_path = db_dir / "tierA.db"
    for p in (db_dir, projects_dir):
        if p.exists():
            shutil.rmtree(p)
        p.mkdir(parents=True, exist_ok=True)

    all_tags = list_tags_in_range(repo, FIRST_TAG, LAST_TAG)
    sampled = evenly_sample(all_tags, args.tags_count)
    print(f"[replay] {len(all_tags)} tags in range, sampled {len(sampled)}: {sampled}")

    tag_to_oid: dict[str, str] = {}
    run_log = []
    for tag in sampled:
        expected_oid = resolve_commit(repo, tag)
        info = stamp_at(binary, str(db_path), str(projects_dir), repo, tag)
        if info["at_oid"] != expected_oid:
            raise SystemExit(
                f"stamp-spans resolved {tag} to {info['at_oid']}, expected {expected_oid}"
            )
        tag_to_oid[tag] = expected_oid
        run_log.append(info)
        print(f"[replay] stamped {tag} @ {expected_oid[:12]}")
        for line in info["stdout"].splitlines():
            if line.strip().startswith(("files ", "spans ", "whole-file", "skipped", "disambig")):
                print(f"    {line.strip()}")

    by_oid = load_ledger_by_oid(str(db_path))
    maps = {tag: by_oid.get(tag_to_oid[tag], {}) for tag in sampled}

    final_tag = sampled[-1]
    final_map = maps[final_tag]

    per_tag_rows = []
    survival_rows = []
    for i, tag in enumerate(sampled):
        map_i = maps[tag]
        n_beliefs = len(map_i)
        n_intact_gt = sum(
            1 for key, stamp in map_i.items() if final_map.get(key) == stamp
        )
        survival_rows.append(
            {
                "tag": tag,
                "n_beliefs": n_beliefs,
                "n_intact_at_final": n_intact_gt,
                "survival_fraction": (n_intact_gt / n_beliefs) if n_beliefs else None,
            }
        )

        is_intermediate = 0 < i < len(sampled) - 1
        if not is_intermediate:
            continue

        later_maps = [maps[t] for t in sampled[i + 1 :]]
        tp = fp = fn = tn = 0
        gt_counts = {"intact": 0, "superseded": 0, "obsolete": 0}
        pred_counts = {"intact": 0, "superseded": 0, "obsolete": 0}
        for key, stamp_i in sorted(map_i.items()):
            pred, gt = classify_belief(stamp_i, key, later_maps, final_map)
            gt_counts[gt] += 1
            pred_counts[pred] += 1
            pred_stale = pred != "intact"
            gt_stale = gt != "intact"
            if pred_stale and gt_stale:
                tp += 1
            elif pred_stale and not gt_stale:
                fp += 1
            elif not pred_stale and gt_stale:
                fn += 1
            else:
                tn += 1

        precision = tp / (tp + fp) if (tp + fp) else None
        recall = tp / (tp + fn) if (tp + fn) else None
        f1 = (
            2 * precision * recall / (precision + recall)
            if precision and recall and (precision + recall) > 0
            else None
        )
        per_tag_rows.append(
            {
                "tag": tag,
                "at_oid": tag_to_oid[tag],
                "n_beliefs": n_beliefs,
                "tp": tp,
                "fp": fp,
                "fn": fn,
                "tn": tn,
                "precision_stale": precision,
                "recall_stale": recall,
                "f1_stale": f1,
                "gt_counts": gt_counts,
                "pred_counts": pred_counts,
            }
        )

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    results = {
        "repo": repo,
        "first_tag": FIRST_TAG,
        "last_tag": LAST_TAG,
        "tags_in_range": len(all_tags),
        "sampled_tags": sampled,
        "tag_to_oid": tag_to_oid,
        "per_tag_metrics": per_tag_rows,
        "survival_curve": survival_rows,
    }
    (out_dir / "tierA_results.json").write_text(json.dumps(results, indent=2, sort_keys=False))

    with open(out_dir / "tierA_results.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(
            [
                "tag",
                "at_oid",
                "n_beliefs",
                "tp",
                "fp",
                "fn",
                "tn",
                "precision_stale",
                "recall_stale",
                "f1_stale",
                "survival_fraction",
            ]
        )
        survival_by_tag = {r["tag"]: r["survival_fraction"] for r in survival_rows}
        for row in per_tag_rows:
            w.writerow(
                [
                    row["tag"],
                    row["at_oid"],
                    row["n_beliefs"],
                    row["tp"],
                    row["fp"],
                    row["fn"],
                    row["tn"],
                    row["precision_stale"],
                    row["recall_stale"],
                    row["f1_stale"],
                    survival_by_tag.get(row["tag"]),
                ]
            )

    print("\n[replay] per-tag precision/recall of 'stale':")
    print(f"{'tag':<10} {'n':>6} {'TP':>5} {'FP':>5} {'FN':>5} {'TN':>5} {'prec':>7} {'recall':>7} {'survival':>9}")
    for row in per_tag_rows:
        surv = survival_by_tag.get(row["tag"])
        print(
            f"{row['tag']:<10} {row['n_beliefs']:>6} {row['tp']:>5} {row['fp']:>5} "
            f"{row['fn']:>5} {row['tn']:>5} "
            f"{(row['precision_stale'] or 0):>7.3f} {(row['recall_stale'] or 0):>7.3f} "
            f"{(surv or 0):>9.3f}"
        )
    print(f"\n[replay] wrote {out_dir / 'tierA_results.json'} and tierA_results.csv")

    if not args.keep_db:
        shutil.rmtree(db_dir, ignore_errors=True)
        shutil.rmtree(projects_dir, ignore_errors=True)
        if owns_scratch_root:
            # Only remove the root this script itself created — never a
            # caller-provided --scratch-dir.
            shutil.rmtree(scratch_dir, ignore_errors=True)

    return 0


if __name__ == "__main__":
    sys.exit(main())
