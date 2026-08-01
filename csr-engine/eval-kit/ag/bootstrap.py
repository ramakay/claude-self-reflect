#!/usr/bin/env python3
"""Paired bootstrap for the AG (Anukriti-gold) codegraph ablation (5 arms, see
examples/codegraph_ablation.rs and eval-kit/ag/score.py). Adapted from
eval-kit/h1/bootstrap.py per .plans/2026-08-01-anukriti-gold-prereg.md rule 4: same
harness, same arms, same controls as E1b — only the seed changes (new pre-registration,
new gold corpus) and the default results path points at eval-kit/ag.

Contrasts (H1: does AST structural spread beat the no-expansion base AND beat a
degree-preserving shuffled-edge control? H2: does file co-edit spread beat the base?):
  S_A - S        (H1: AST spread vs no expansion)
  S_A - S_Asham  (H1: AST spread vs shuffled-edge control)
  S_F - S        (H2: file spread vs no expansion)
  S_FA - S_F     (does AST add marginal gain on top of file spread?)

Metrics: origin-MRR (mapped-origin subset), nDCG@10 (all queries). Per-query paired
deltas are resampled with replacement, 10,000 times, fixed seed 20260801, and report the
mean delta and the 2.5/97.5 percentile bootstrap CI. Python stdlib only.

Run: python3 eval-kit/ag/bootstrap.py [path/to/results.json]
(results.json is produced by eval-kit/ag/score.py; defaults to eval-kit/ag/results.json)
"""
import json, os, random, sys

AG = os.path.dirname(os.path.abspath(__file__))
results_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(AG, "results.json")

N_RESAMPLES = 10_000
SEED = 20260801

with open(results_path) as f:
    R = json.load(f)
PQ = R["per_query"]

CONTRASTS = [
    ("S_A", "S", "H1: AST spread vs no expansion"),
    ("S_A", "S_Asham", "H1: AST spread vs shuffled-edge control"),
    ("S_F", "S", "H2: file spread vs no expansion"),
    ("S_FA", "S_F", "AST marginal gain on top of file spread"),
]
METRICS = ["mrr", "ndcg10"]
METRIC_LABEL = {"mrr": "origin-MRR", "ndcg10": "nDCG@10"}


def paired_deltas(arm_b, arm_a, metric):
    """Per-qid (arm_b - arm_a) deltas, restricted to qids where the metric is
    defined (non-None) for both arms — i.e. the applicable query subset."""
    vb = PQ[arm_b][metric]
    va = PQ[arm_a][metric]
    qids = sorted(q for q in vb if vb[q] is not None and va.get(q) is not None)
    return qids, [vb[q] - va[q] for q in qids]


def bootstrap_ci(deltas, n_resamples, seed):
    rng = random.Random(seed)
    n = len(deltas)
    means = []
    for _ in range(n_resamples):
        resample = [deltas[rng.randrange(n)] for _ in range(n)]
        means.append(sum(resample) / n)
    means.sort()
    lo = means[int(0.025 * n_resamples)]
    hi = means[int(0.975 * n_resamples) - 1]
    return lo, hi


print(f"paired bootstrap: {N_RESAMPLES} resamples, seed={SEED}\n")

summary = {}
for arm_b, arm_a, label in CONTRASTS:
    print(f"=== {arm_b} - {arm_a}  ({label}) ===")
    summary[f"{arm_b}-{arm_a}"] = {"label": label}
    for metric in METRICS:
        qids, deltas = paired_deltas(arm_b, arm_a, metric)
        if not deltas:
            print(f"  {METRIC_LABEL[metric]:<12} n=0 (no applicable queries)")
            continue
        mean_delta = sum(deltas) / len(deltas)
        lo, hi = bootstrap_ci(deltas, N_RESAMPLES, SEED)
        excl0 = "CI excludes 0" if (lo > 0 or hi < 0) else "CI includes 0"
        print(
            f"  {METRIC_LABEL[metric]:<12} n={len(deltas):<3} mean_delta={mean_delta:+.4f} "
            f"95% CI=[{lo:+.4f}, {hi:+.4f}]  ({excl0})"
        )
        summary[f"{arm_b}-{arm_a}"][metric] = {
            "n": len(deltas),
            "qids": qids,
            "mean_delta": round(mean_delta, 4),
            "ci95": [round(lo, 4), round(hi, 4)],
            "ci_excludes_0": lo > 0 or hi < 0,
        }
    print()

# Pre-registered verdicts.
h1_ci = summary.get("S_A-S", {})
h1_sham_ci = summary.get("S_A-S_Asham", {})
h2_ci = summary.get("S_F-S", {})


def pass_fail(entry, metric="mrr"):
    m = entry.get(metric)
    if not m:
        return "FAIL (no data)"
    return "PASS" if (m["ci_excludes_0"] and m["mean_delta"] > 0) else "FAIL"


h1_verdict = "PASS" if (
    pass_fail(h1_ci) == "PASS" and pass_fail(h1_sham_ci) == "PASS"
) else "FAIL"
h2_verdict = pass_fail(h2_ci)

print("=== Pre-registered verdicts (origin-MRR, CI excludes 0 AND mean_delta > 0) ===")
print(f"A-H1 (S_A > S AND S_A > S_Asham): {h1_verdict}")
print(f"A-H2 (S_F > S): {h2_verdict}")

summary["verdicts"] = {"a_h1": h1_verdict, "a_h2": h2_verdict}
out_path = os.path.join(AG, "bootstrap_results.json")
with open(out_path, "w") as f:
    json.dump(summary, f, indent=1)
print(f"\n{out_path} written")
