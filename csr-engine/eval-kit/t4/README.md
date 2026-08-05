# T4 — git history as free labels

Two eval tiers that get staleness/provenance ground truth for free from git
history instead of hand-labeling — the harness for the v10 "dreaming"
(evidence-grounded forgetting) design's precision claims. Runs against the
outer `claude-self-reflect` repo's own history (read-only); writes only to a
scratch DB and this directory. Requires the `witness_ledger` +
`stamp-spans --at` substrate from `feat/rmcp-3.1` (commit `dbc8dcf`).

## Tier A — symbol time-travel replay (`replay.py`)

**Question**: given a `codewitness` stamp for `(file, symbol)` recorded at an
old release tag, can a simple SQL rule — "did a later snapshot see a
different stamp for this symbol?" — correctly predict whether that belief
is still true today, without re-deriving anything?

**Method**:
1. Build the release binary; every DB write goes to a scratch SQLite file
   under `--scratch-dir` (never `~/.claude-self-reflect`).
2. List `v*` tags between `v8.0.0` and `v9.5.0` (17 in-range), sample 13
   evenly by an integer linspace-index formula — deterministic, no RNG,
   endpoints always included.
3. For each sampled tag, run `codegraph stamp-spans --at <tag> --repo
   <outer-repo>` into the scratch DB — this mints `committed`-tier
   `witness_ledger` rows anchored to that historical commit, reading blob
   content straight from the commit's own tree (not the code graph's node
   list, so it can see files the graph never touched).
4. For each **intermediate** tag T_i (not first/last), every `(file,
   symbol)` witnessed at T_i is a "belief held at T_i". Classify it two
   ways:
   - **Prediction** (the practical online rule): `superseded` if any
     *later sampled tag* has the same key with a different stamp;
     `obsolete` if the key is absent at the *final* tag (`v9.5.0`);
     `intact` otherwise.
   - **Ground truth**: defined directly off the final tag — `obsolete` if
     absent there, `superseded` if present with a different stamp,
     `intact` if present with the same stamp.
5. `stale` = `superseded` or `obsolete`, for both sides. Precision/recall of
   `stale` per T_i, plus a survival curve (fraction of T_i's symbols still
   `intact` at the final tag).

**Structural note** (proven, not just observed): the prediction rule's
`superseded` check always includes the final tag itself as one of the
"later tags" it scans, so predicted-stale is a superset of ground-truth-
stale — **recall(stale) = 1.0 is guaranteed by construction** for every
T_i. The only way precision can drop below 1.0 is a belief whose stamp
changed at some *intermediate* sampled tag and then reverted back to the
original value by the final tag — the rule over-flags that as
`superseded`, ground truth calls it `intact`. Precision is therefore the
one empirically meaningful number here.

### Invocation

```bash
cd csr-engine
cargo build --release --bin csr-engine
python3 eval-kit/t4/replay.py \
  --binary target/release/csr-engine \
  --scratch-dir /path/to/scratch   # NEVER ~/.claude-self-reflect
```

### Results (this run, `/Users/ramakrishnanannaswamy/projects/claude-self-reflect`, 17 tags in range → 13 sampled)

| tag | beliefs | TP | FP | FN | TN | precision(stale) | recall(stale) | survival→v9.5.0 |
|---|---|---|---|---|---|---|---|---|
| v8.0.1 | 890 | 140 | 0 | 0 | 750 | 1.000 | 1.000 | 0.843 |
| v8.0.3 | 890 | 140 | 0 | 0 | 750 | 1.000 | 1.000 | 0.843 |
| v8.0.4 | 890 | 140 | 0 | 0 | 750 | 1.000 | 1.000 | 0.843 |
| v8.0.5 | 890 | 140 | 0 | 0 | 750 | 1.000 | 1.000 | 0.843 |
| v8.2.0 | 937 | 137 | 0 | 0 | 800 | 1.000 | 1.000 | 0.854 |
| v8.3.0 | 995 | 125 | 0 | 0 | 870 | 1.000 | 1.000 | 0.874 |
| v9.0.0 | 1076 | 104 | 0 | 0 | 972 | 1.000 | 1.000 | 0.903 |
| v9.2.0 | 1535 | 121 | 0 | 0 | 1414 | 1.000 | 1.000 | 0.921 |
| v9.3.0 | 1584 | 107 | 0 | 0 | 1477 | 1.000 | 1.000 | 0.932 |
| v9.3.1 | 1892 | 69 | 0 | 0 | 1823 | 1.000 | 1.000 | 0.964 |
| v9.4.1 | 1996 | 52 | 0 | 0 | 1944 | 1.000 | 1.000 | 0.974 |

(v8.0.0 and v9.5.0 are the endpoints — excluded from the metrics table by
definition, since "later tag" / "final tag" are undefined or trivial for
them; both appear in the survival curve in `tierA_results.json`.)

Sampled tags → resolved commits: v8.0.0→624e7229, v8.0.1→34c541a6,
v8.0.3→b06ea0ef, v8.0.4→fb9a8248, v8.0.5→e6ab788b, v8.2.0→366c51ca,
v8.3.0→eb61cdc1, v9.0.0→24814e95, v9.2.0→788b6e40, v9.3.0→75ab97d0,
v9.3.1→f9a1997f, v9.4.1→10e894a5, v9.5.0→ef6d5d9d.

**Reading it**: precision is 1.000 everywhere in this run — i.e. in this
repo's actual history, no symbol's stamp changed at an intermediate sampled
tag and then reverted to its original value by `v9.5.0`. That is a
substantive (if convenient) empirical finding on its own: it is consistent
with Tier B finding **zero** `Revert`-subject commits anywhere in this
repo's reachable history (see below) — there is nothing here for the
revert-noise failure mode to catch. The 1.0/1.0 result is not a tautology
of the metric (FP could be nonzero; it happens to be zero for this corpus)
but it does mean this run alone can't demonstrate the precision gap the
rule is designed to expose — a repo with real reverts would show
precision < 1.0 on some tags. Survival declines monotonically from 0.843
(oldest sampled tag) toward 1.0 (final tag), as expected.

Output: `tierA_results.json` (full detail incl. per-tag `gt_counts`/
`pred_counts` 3-way breakdowns and the survival curve), `tierA_results.csv`
(flat per-tag row).

## Tier B — episode ancestry labels (`labels.py`)

**Question**: of the commits that make up this repo's history, how many can
be tied to a release, a revert, or a CSR conversation session — for free,
from git and the existing attribution tables, no new labeling?

**Method**:
1. List **all** `v*` tags (140), version-sorted. Walk oldest→newest; for
   each tag, `git rev-list T_prev..T_cur` (full ancestry for the first tag)
   minus whatever an earlier tag in the walk already claimed = "shipped in
   T_cur". This is robust to non-linear tag/branch topology: it's "first
   tag in version order that claims this commit", not "first tag by date".
2. Revert detection: commit subject matching `^Revert` (case-insensitive)
   → `label = reverted`; if its body has a `This reverts commit <sha>`
   trailer that resolves (by unambiguous prefix) to another commit in our
   set, that original commit is *also* relabeled `reverted` (its
   `release_tag` is left untouched — it still records what shipped it,
   `label` just says it didn't survive).
3. Commits reachable from `HEAD` claimed by no tag → `unreleased`.
4. Session linkage: two-hop join through the live `code_node_attribution`
   table — `channel='git', source_id=<sha>` → `node_id` →
   `channel='transcript'` on the same `node_id` → `source_id` = session.
   Opened via `file:<path>?mode=ro` (SQLite read-only URI) — the live DB is
   **never** opened for write. Fan-out (a commit's nodes spanning >1
   session) picks the lexicographically smallest session_id deterministically.

### Invocation

```bash
python3 eval-kit/t4/labels.py \
  --repo /Users/ramakrishnanannaswamy/projects/claude-self-reflect \
  --csr-db /Users/ramakrishnanannaswamy/.claude-self-reflect/csr-engine.db
```

### Coverage (this run)

| metric | value |
|---|---|
| release tags | 140 (v1.0.0 → v9.5.0) |
| commits reachable from HEAD | 546 |
| labeled shipped or reverted | 540 (98.9%) |
| — shipped | 540 |
| — reverted | 0 |
| — unreleased | 6 |
| commits with session linkage | 60 (10.99%) |
| commits with >1 candidate session (fan-out) | 40 |

Output: `labels.csv` (`commit, release_tag, label, session_id`, one row per
commit reachable from HEAD, in `git rev-list HEAD`'s deterministic order),
`labels_summary.json` (coverage stats + full per-tag shipped-commit-count
table for all 140 tags).

## Caveats

- **Zero reverts in this history**: this repo has no `Revert`-subject
  commits, so Tier B's `reverted` label and Tier A's precision-gap failure
  mode are both exercised by the code path but not by real data here — a
  repo with actual reverts would be a better stress test for both.
- **Session linkage denominator**: 10.99% of *all* reachable commits get a
  session, not 10.99% of commits that plausibly could — docs-only, config,
  CI, and vendored-file commits never touch a `code_nodes` span and so can
  never join through `code_node_attribution`, whatever the DB's actual
  attribution quality is. The honest number to trust for "how good is
  attribution" is elsewhere (H4/H5/H6, see repo history); this just reports
  what fraction of *raw git log* got linked.
- **`git rev-list` history size**: 546 commits reachable from HEAD is far
  smaller than "140 releases" might suggest — this repo's history was
  squashed/rewritten around `v6.0.4` (251 commits land under that one tag
  in `labels_summary.json`'s per-tag table); Tier B's ancestry walk handles
  this correctly (it's a pure git-ancestry fact, not an assumption), but a
  reader expecting one-commit-per-logical-change across the full 140-tag
  span should know the early tags are historical waypoints on a
  reconstructed/condensed history, not evidence the repo was that thin.
- **`--tags-count 13` is a request, not a guarantee** when the in-range tag
  count is close to it: `evenly_sample` returns `min(13, n)` distinct tags
  by rounding linspace indices, so a tighter or wider release cadence
  between the two endpoints you pick will change the exact count slightly
  (it was exactly 13 for the 17 in-range tags used here).
- Both scripts assume `git` on `PATH` and a `csr-engine` release binary
  already built; Tier A additionally assumes `--repo` resolves cleanly for
  every sampled tag (a resolve failure for one of *our own* release tags
  is treated as a hard error, not a skip — unlike `stamp-spans --at`'s own
  skip-per-repo behavior for arbitrary revs).
