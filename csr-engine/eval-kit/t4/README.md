# T4 — git history as free labels

Two eval tiers that get staleness/provenance ground truth for free from git
history instead of hand-labeling — the harness for the v10 "dreaming"
(evidence-grounded forgetting) design's precision claims. Runs against the
outer `claude-self-reflect` repo's own history (read-only); writes only to a
scratch DB and this directory. Both tiers are implemented as subcommands of
the `codewitness` binary (`codewitness labels`, `codewitness bench`) — no
Python, no LLM, fully deterministic.

## Tier A — symbol time-travel replay (`codewitness bench`)

**Question**: given a `codewitness` stamp for `(file, symbol)` recorded at an
old release tag, can a simple SQL rule — "did a later snapshot see a
different stamp for this symbol?" — correctly predict whether that belief
is still true today, without re-deriving anything?

**Method**:
1. Build the release binaries; every DB write goes to a scratch SQLite file
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
cd csr-engine && cargo build --release --bin csr-engine
cd ../codewitness && cargo build --release
target/release/codewitness bench \
  --repo /path/to/claude-self-reflect \
  --binary ../csr-engine/target/release/csr-engine \
  --scratch-dir /path/to/scratch \
  --out eval-kit/t4/results.json   # scratch dir: NEVER ~/.claude-self-reflect
```

### Results (run at `5d2bd81`, 17 tags in range → 13 sampled)

| tag | beliefs | TP | FP | FN | TN | precision(stale) | recall(stale) | survival→v9.5.0 |
|---|---|---|---|---|---|---|---|---|
| v8.0.1 | 894 | 139 | 0 | 0 | 755 | 1.000 | 1.000 | 0.845 |
| v8.0.3 | 894 | 139 | 0 | 0 | 755 | 1.000 | 1.000 | 0.845 |
| v8.0.4 | 894 | 139 | 0 | 0 | 755 | 1.000 | 1.000 | 0.845 |
| v8.0.5 | 894 | 139 | 0 | 0 | 755 | 1.000 | 1.000 | 0.845 |
| v8.2.0 | 941 | 136 | 0 | 0 | 805 | 1.000 | 1.000 | 0.855 |
| v8.3.0 | 999 | 124 | 0 | 0 | 875 | 1.000 | 1.000 | 0.876 |
| v9.0.0 | 1081 | 104 | 0 | 0 | 977 | 1.000 | 1.000 | 0.904 |
| v9.2.0 | 1540 | 120 | 0 | 0 | 1420 | 1.000 | 1.000 | 0.922 |
| v9.3.0 | 1589 | 106 | 0 | 0 | 1483 | 1.000 | 1.000 | 0.933 |
| v9.3.1 | 1898 | 68 | 0 | 0 | 1830 | 1.000 | 1.000 | 0.964 |
| v9.4.1 | 2002 | 51 | 0 | 0 | 1951 | 1.000 | 1.000 | 0.975 |

(v8.0.0 and v9.5.0 are the endpoints — excluded from the metrics table by
definition, since "later tag" / "final tag" are undefined or trivial for
them; both appear in the survival curve in `results.json`.)

Sampled tags → resolved commits: v8.0.0→624e7229, v8.0.1→34c541a6,
v8.0.3→b06ea0ef, v8.0.4→fb9a8248, v8.0.5→e6ab788b, v8.2.0→366c51ca,
v8.3.0→eb61cdc1, v9.0.0→24814e95, v9.2.0→788b6e40, v9.3.0→75ab97d0,
v9.3.1→f9a1997f, v9.4.1→10e894a5, v9.5.0→ef6d5d9d.

**Reading it**: precision is 1.000 everywhere in this run — i.e. in this
repo's actual history, no symbol's stamp changed at an intermediate sampled
tag and then reverted to its original value by `v9.5.0`. That is consistent
with Tier B finding **zero** `Revert`-subject commits anywhere in this
repo's reachable history (see below) — there is nothing here for the
revert-noise failure mode to catch. The 1.0/1.0 result is not a tautology
of the metric (FP could be nonzero; it happens to be zero for this corpus)
but it does mean this run alone can't demonstrate the precision gap the
rule is designed to expose — a repo with real reverts would show
precision < 1.0 on some tags. Survival rises monotonically with tag
recency, from 0.845 at the oldest sampled tag to 1.0 at the final tag —
i.e. the older the belief, the less of it survives, as expected.

### H1 rematch — dream rule vs grep/recency baselines

The rematch scores five classifiers on the **same 13,626 beliefs** from the
11 intermediate tags and the same final-tag ground truth. The ground-truth
vector is computed once; every arm consumes it through the same confusion-
matrix scorer, with no arm-specific filtering. Beliefs, files, tags, and
symbols are iterated in sorted order, and no LLM is used anywhere.

- **dream/CSR** is the prediction rule above, unchanged.
- **grep** predicts stale when the source-level symbol name is not a plain
  substring of its file at `v9.5.0`, or when that file is gone. File content
  comes only from `git show v9.5.0:<repo-relative-file>`, never the working
  tree. Ledger-qualified methods use their final declared-name component for
  both `Container::method` and `Container.method` forms, with any deterministic
  `#N` collision suffix removed before matching.
- **recency-N** predicts stale when the belief tag's committed date is
  strictly more than N × 86,400 seconds before the final tag's committed
  date. Dates are integer `%ct` commit timestamps from git; N is 30, 90, or
  180.

| arm | beliefs | TP | FP | TN | FN | precision(stale) | recall(stale) | F1(stale) |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| dream/CSR | 13,626 | 1,265 | 0 | 12,361 | 0 | 1.000 | 1.000 | 1.000 |
| grep | 13,626 | 20 | 0 | 12,361 | 1,245 | 1.000 | 0.016 | 0.031 |
| recency-30 | 13,626 | 920 | 5,677 | 6,684 | 345 | 0.139 | 0.727 | 0.234 |
| recency-90 | 13,626 | 556 | 3,020 | 9,341 | 709 | 0.155 | 0.440 | 0.230 |
| recency-180 | 13,626 | 0 | 0 | 12,361 | 1,265 | 0.000 | 0.000 | 0.000 |

**Corrections and honest reading.** Two defects in earlier runs of this
harness were found by adversarial audit and are corrected here:

1. *Unfair grep normalization* (withdrawn earlier): the original grep arm
   handled `::`-qualified symbols but not dot-qualified Python/TypeScript/
   JavaScript symbols; all 66 originally-reported grep false positives
   disappear under fair normalization. The once-claimed dream-over-grep
   *precision* gap is withdrawn.
2. *Ground-truth extractor collision* (fixed at `5d2bd81`): the extractor
   that mints witnessed symbols previously collapsed coexisting same-named
   definitions (e.g. two `is_empty` methods on different types in one file)
   into a single key before qualification, so both prediction and ground
   truth were derived from partially-wrong labels. The fix keys extraction
   by `(node, span, AST ordinal)`; this run's belief population grew from
   13,575 to 13,626 and the stale count moved from 1,275 to 1,265. The
   numbers above are from the corrected extractor.

Grep is **precise but blind** here: its 20 stale predictions are all correct
(precision 1.000), but it finds only 20 of 1,265 stale beliefs (recall
0.016). The dream arm's later-snapshot scan includes the final tag, so its
predicted-stale set is a superset of final-tag staleness and recall 1.000 is
guaranteed **by construction**, not established empirically. F1 inherits
that advantage; **precision is the real H1 comparison column**. Dream
precision is also 1.000 in this corpus, but the corpus has zero observed
revert commits, so the revert-driven false-positive mode never occurs. The
honest conclusion is therefore that dream precision 1.000 is **unfalsified,
not proven** by this run; meanwhile grep trades away nearly all recall to
remain precise.

Run baselines by default; pass `--skip-baselines` to retain only the
dream/CSR arm in `arm_metrics`.

### Determinism and provenance

- `results.json` carries a `provenance` block: `repo_head_at_run` (the
  benched repo's HEAD commit at run time), `binary_sha256` (exact stamper
  binary), and per-tag stamping stats including captured stderr.
- Rerunning the bench back-to-back produces **identical metrics**: hashing
  the results with the `provenance` block removed gives the same SHA-256
  across runs. The only bytes that differ between runs are the captured
  stderr timing strings inside `provenance` (retained deliberately — the
  subprocess contract requires stderr be kept, and startup timings vary).
- `codewitness labels` output is byte-identical across runs.

## Tier B — episode ancestry labels (`codewitness labels`)

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
codewitness labels \
  --repo /path/to/claude-self-reflect \
  --csr-db ~/.claude-self-reflect/csr-engine.db \
  --out eval-kit/t4/labels.json
```

### Coverage (run at `5d2bd81`)

| metric | value |
|---|---|
| release tags | 140 (v1.0.0 → v9.5.0) |
| commits reachable from HEAD | 558 |
| labeled shipped or reverted | 540 (96.8%) |
| — shipped | 540 |
| — reverted | 0 |
| — unreleased | 18 |
| commits with session linkage | 60 (10.75%) |
| commits with >1 candidate session (fan-out) | 40 |

The 18 unreleased commits are the open `feat/rmcp-3.1` branch work not yet
claimed by any tag — expected while the branch is open; they will be
claimed by the next release tag.

Output artifacts in this directory: `results.json` (Tier A + H1 arms, full
per-tag detail, survival curve, provenance), `labels.json` (Tier B per-commit
labels + summary counts).
