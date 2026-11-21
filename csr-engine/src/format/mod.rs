use chrono::Utc;

use crate::import::ConversationChunk;
use crate::temporal::parse_timestamp;

/// Escape special XML characters in user-generated content.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Preview truncation length (chars, not bytes) used across all result renderers.
pub const PREVIEW_CHARS: usize = 500;

/// Return the longest prefix of `s` with at most `max` chars, cut on a char
/// boundary (never panics on multi-byte UTF-8, unlike `&s[..max]` byte slicing).
pub fn truncate_chars(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

/// Abstention floor for `csr_quick_check`: below this top score the tool makes a
/// negative existence claim instead of reporting a match.
///
/// Measured 2026-08-03 (`cargo run --release --example quick_check_floor`, exact
/// cosine over all 141,964 chunk embeddings): 8 fabricated topics (never
/// discussed on this machine) scored 0.308–0.605; 12 topics verified present in
/// `chunks.content` scored 0.468–0.816. **The distributions overlap** — there is
/// no floor that suppresses every fabricated probe without also suppressing real
/// ones (a floor at 0.605 would lose 4 of 12 genuine topics). 0.45 is therefore
/// the highest floor that misses no verified-genuine probe; it suppresses only
/// the clearly-hopeless tail. The overlap band above it is handled by the `weak`
/// label plus an honest calibration warning and a top1-top2 margin signal, not
/// by silent confidence.
pub const QUICK_CHECK_FLOOR: f32 = 0.45;

/// Top of the measured fabrication-overlap band. At or above this, a match is
/// at least `partial`; below it a match is `weak` — absolute cosine is weakly
/// calibrated in this band on this corpus and does not reliably separate
/// genuine matches from noise by score alone. `format_quick_check` surfaces a
/// top1-top2 margin alongside the score as a cheap discriminating signal
/// instead of pretending the score settles it.
///
/// This edge came from the 2026-08-03 measurement (`examples/quick_check_
/// floor.rs`, exact cosine over the then-current corpus): the highest
/// fabricated probe measured 0.605, rounded up to 0.62. It is a fixed
/// constant, not recomputed as the corpus grows. Corpus growth has already
/// pushed fabricated probes above it: a 2026-09-22 re-measurement
/// (`examples/quick_check_floor_margin.rs`) found one at 0.629, which lands
/// in `partial` and carries no caveat at all — a known, tracked gap (not
/// asserted as a hard invariant, since asserting it would block a future fix
/// for it) — see `weak_band_fabricated_probes_still_carry_a_caveat`.
pub const WEAK_BAND_TOP: f32 = 0.62;

/// Band a similarity score into the relevance vocabulary shared by every result
/// renderer. Single source of truth — `format_search_results` and
/// `format_quick_check` must not drift apart on what a score means.
pub fn relevance_label(score: f32) -> &'static str {
    if score >= 0.85 {
        "high"
    } else if score >= 0.75 {
        "good"
    } else if score >= WEAK_BAND_TOP {
        "partial"
    } else {
        "weak"
    }
}

/// Gap (cross-encoder logits, top minus runner-up) a `high` label needs.
///
/// The `CE_*` bands are in raw cross-encoder logit units and read margins
/// only, never an absolute score or a cosine. The two gap floors below
/// (`CE_GAP_TIE`, `CE_GAP_GOOD`) and the spread edges were calibrated on the
/// 40 judged pairs of the S9 review pass (`s9/judge-s9`), reading each
/// rank-1 from the final ranker summary (`ce-top`, `ce-margin`, `ce-median`):
/// 33 of the 40 rank-1s were one of the two judged chunks.
///
/// What those pairs showed: the rank-1 was the reviewer's pick about 74% of the
/// time in every band (weak 7 of 10 decided pairs, partial 7 of 9, good 3 of
/// 4), so no logit margin predicts correctness here. Two failures were
/// visible and are what the floors fix. A `good` from the spread alone fired
/// on a near-tie at the top (gap 0.04, the reviewer picked the other side), and
/// the old `weak` band (spread under 1) held 7 rank-1s the reviewer picked. The
/// bands now say how much evidence separates the top from the rest, not that
/// the top is right.
pub const CE_GAP_HIGH: f32 = 1.5;
/// Gap a `good` label needs. Calibrated on the 40 judged pairs: the two
/// reviewer-picked rank-1s with a real lead sat at a gap of 1.11 and 1.73 and
/// stay `good`/`high`; the echo of the query itself (gap 0.53) and the
/// near-tie miss (gap 0.04) fall to `partial`. A third picked rank-1 (gap
/// 0.37) falls to `partial` with them. A recall with no runner-up has no
/// rival and is not held to the floor.
pub const CE_GAP_GOOD: f32 = 1.0;
/// Gap below which the top is a near-tie with the runner-up (logits). A
/// `weak` label needs this and a spread under `CE_SPREAD_PARTIAL`: nothing
/// singles the top out of the pool. Calibrated on the 40 judged pairs: 0.05 leaves
/// one reviewer-picked rank-1 labelled `weak` (gap 0.01); the next picked one
/// sits at a gap of 0.06.
pub const CE_GAP_TIE: f32 = 0.05;
/// Spread (top minus median of the pooled session scores) a `high` label needs.
/// S2 first-cut value, kept: the judged pairs held one `high`, and it was a
/// reviewer-picked rank-1.
pub const CE_SPREAD_HIGH: f32 = 3.0;
/// Spread a `good` label needs. S2 first-cut value, kept: the 40 judged pairs
/// give no case for moving it.
pub const CE_SPREAD_GOOD: f32 = 2.0;
/// Spread a `partial` label needs when the top is also a near-tie (see
/// `CE_GAP_TIE`); every recall that fails `high`, `good` and `weak` is
/// `partial`. S2 first-cut value, kept.
pub const CE_SPREAD_PARTIAL: f32 = 1.0;

/// The six margin constants of the cross-encoder label rule, as one value so
/// a model can carry its own.
///
/// `SHIPPED` is the `CE_*` constants above, calibrated on the S9 pairs with the
/// shipped `ettin-reranker-32m-v1`. A model whose logit scale differs gets its
/// own entry in [`CE_TUNED_BANDS`], keyed on the sha256 of its ONNX file, and
/// only a model loaded from a file with that exact hash reads it. Every other
/// model, and a model that cannot be hashed, reads `SHIPPED`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CeBands {
    pub gap_high: f32,
    pub gap_good: f32,
    pub gap_tie: f32,
    pub spread_high: f32,
    pub spread_good: f32,
    pub spread_partial: f32,
}

/// File name of the encoder graph inside a reranker model directory.
const CE_ONNX_FILE: &str = "model_qint8_arm64.onnx";

/// Per-model band entries: (sha256 of the ONNX file, bands).
///
/// STOPGAP, not a fix. Each entry quantile-matches the six `SHIPPED` constants
/// onto that model's logit distribution so the label mix (the share of
/// `high` / `good` / `partial` / `weak`) stays what the shipped model gave. It
/// preserves label frequency, not label correctness: the labels still describe
/// margin, and the inverted-label defect on exact answers is not addressed.
pub const CE_TUNED_BANDS: &[(&str, CeBands)] = &[(
    // tuned-run2 (ettin-tune), fit on the 310 audit queries run on the Apple
    // laptop CPU. The same ONNX scores on a Mac Studio differ (narrower, other
    // rank-1s), so these bands are for the machine they were fit on.
    "73b9d13ac00020e657dc43319a9717fbf0fc10d7243ad5710d48837c6137abab",
    CeBands {
        gap_high: 4.21,
        gap_good: 3.03,
        gap_tie: 0.215,
        spread_high: 7.355,
        spread_good: 6.01,
        spread_partial: 3.98,
    },
)];

impl CeBands {
    pub const SHIPPED: Self = Self {
        gap_high: CE_GAP_HIGH,
        gap_good: CE_GAP_GOOD,
        gap_tie: CE_GAP_TIE,
        spread_high: CE_SPREAD_HIGH,
        spread_good: CE_SPREAD_GOOD,
        spread_partial: CE_SPREAD_PARTIAL,
    };

    /// Bands for a model whose ONNX file hashes to `sha256` (lower-case hex),
    /// from `table`; `SHIPPED` when the hash is not listed.
    pub fn lookup(sha256: &str, table: &[(&str, CeBands)]) -> Self {
        table
            .iter()
            .find(|(sha, _)| sha.eq_ignore_ascii_case(sha256))
            .map_or(Self::SHIPPED, |(_, bands)| *bands)
    }

    /// Bands for the reranker in `dir`, read from [`CE_TUNED_BANDS`];
    /// `SHIPPED` when the ONNX file is absent, unreadable or not listed.
    pub fn for_model_dir(dir: &std::path::Path) -> Self {
        Self::for_model_dir_in(dir, CE_TUNED_BANDS)
    }

    pub(crate) fn for_model_dir_in(dir: &std::path::Path, table: &[(&str, CeBands)]) -> Self {
        match file_sha256(&dir.join(CE_ONNX_FILE)) {
            Some(sha) => Self::lookup(&sha, table),
            None => Self::SHIPPED,
        }
    }

    /// Bands for the reranker this process loads, decided once per process.
    /// Keyed on the sha256 of the ONNX bytes the scorer is built from: the
    /// compiled-in graph, or the `model_qint8_arm64.onnx` in `CSR_CE_MODEL_DIR`.
    pub fn active() -> &'static Self {
        static ACTIVE: std::sync::OnceLock<CeBands> = std::sync::OnceLock::new();
        ACTIVE.get_or_init(|| {
            Self::for_source(&crate::search::cross_encoder::ModelSource::from_env())
        })
    }

    /// Bands for the model `source` loads, read from [`CE_TUNED_BANDS`]. The
    /// key is the hash of the model bytes, never of a path.
    pub(crate) fn for_source(source: &crate::search::cross_encoder::ModelSource) -> Self {
        use crate::search::cross_encoder::{embedded_onnx, ModelSource};
        match source {
            ModelSource::Embedded => Self::lookup(&sha256_hex(embedded_onnx()), CE_TUNED_BANDS),
            ModelSource::Dir(dir) => Self::for_model_dir(dir),
        }
    }
}

/// Lower-case hex sha256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Lower-case hex sha256 of a file, streamed; `None` when it cannot be read.
fn file_sha256(path: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// Cross-encoder score summary of one recall: the best pooled session score,
/// the second best (when there is one) and the median, all in raw logits over
/// the pooled sessions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CeSummary {
    pub top: f32,
    pub runner_up: Option<f32>,
    pub median: f32,
}

impl CeSummary {
    /// Summary of pooled session scores, or `None` for an empty set. Scores
    /// that are not finite are ignored.
    pub fn from_scores(scores: &[f32]) -> Option<Self> {
        let mut sorted: Vec<f32> = scores.iter().copied().filter(|s| s.is_finite()).collect();
        if sorted.is_empty() {
            return None;
        }
        sorted.sort_by(|a, b| b.total_cmp(a));
        let n = sorted.len();
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        };
        Some(Self {
            top: sorted[0],
            runner_up: sorted.get(1).copied(),
            median,
        })
    }

    /// The relevance label this summary earns under the bands of the loaded
    /// reranker.
    pub fn label(&self) -> &'static str {
        self.label_with(CeBands::active())
    }

    /// The relevance label this summary earns under explicit `bands`.
    pub fn label_with(&self, bands: &CeBands) -> &'static str {
        ce_relevance_label_with(self.top, self.runner_up, self.median, bands)
    }
}

/// Band a cross-encoder result into the relevance vocabulary from margins
/// only, with the shipped model's bands: `gap` is `top - runner_up` (0 when
/// there is no runner-up) and `spread` is `top - median`. Absolute scores and
/// cosines never enter.
///
/// `high` needs a clear gap and spread, `good` a spread and a real lead over
/// the runner-up, `weak` a near-tie that is also no spread at all, and
/// `partial` is everything between.
pub fn ce_relevance_label(top: f32, runner_up: Option<f32>, median: f32) -> &'static str {
    ce_relevance_label_with(top, runner_up, median, &CeBands::SHIPPED)
}

/// [`ce_relevance_label`] under explicit `bands`.
pub fn ce_relevance_label_with(
    top: f32,
    runner_up: Option<f32>,
    median: f32,
    bands: &CeBands,
) -> &'static str {
    let gap = runner_up.map_or(0.0, |second| top - second);
    let spread = top - median;
    if gap >= bands.gap_high && spread >= bands.spread_high {
        "high"
    } else if spread >= bands.spread_good && runner_up.is_none_or(|_| gap >= bands.gap_good) {
        "good"
    } else if spread < bands.spread_partial && gap < bands.gap_tie {
        "weak"
    } else {
        "partial"
    }
}

/// Margin (top score minus a *comparable* runner-up score) above which a
/// weak-band top result is treated as having clearly beaten the rest of the
/// list, even though its absolute score is still weakly calibrated.
///
/// A prior version of this constant was paired with a
/// `FABRICATED_PROBE_CEILING` (0.605, measured 2026-08-03) that let a
/// sufficiently high absolute score alone claim "clears the highest
/// fabricated-probe score measured on this corpus". That ceiling was
/// falsified by corpus growth: re-measuring 2026-09-22 found fabricated
/// probes as high as 0.629 (drifted straight through the weak band into
/// "partial", where they carry no caveat at all — a separate, still-open
/// gap tracked by `weak_band_fabricated_probes_still_carry_a_caveat`) and
/// 0.606 still inside the weak band, both above the 0.605 ceiling. A ceiling
/// this close to the top of the weak band cannot be re-derived to a value
/// that is both inside the band and safely above every fabricated score, so
/// the ceiling-decisive branch was removed rather than patched with a new
/// magic number (review finding [high] #1). Decisiveness in the weak band is
/// margin-only now.
///
/// Derived from the 2026-09-22 burst-failure study (`recall_bursts.json`
/// fixture): among weak-band bursts where the session's own next turn
/// actually used the rank-1 result, the largest observed margin was 0.113
/// (top 0.563 over runner-up 0.450) — but that runner-up is
/// `score_fts_candidate`'s synthetic 0.45 floor from a `[keyword] ` fallback
/// row, not a cosine measurement (review finding [high] #2), so it is not
/// actually a valid basis for this threshold either. With no confirmed
/// same-scale decisive case left in the fixture, 0.10 is kept as a
/// conservative floor above the remaining cosine-vs-cosine margin cluster
/// (which sits within +/-0.06 of zero, several negative — rank-1 by rerank,
/// not by raw score) rather than re-derived from a single contaminated data
/// point. Re-deriving it against real reflect_on_past-path margins (hybrid
/// rerank, not exact-cosine quick_check) is a follow-up, not done here.
///
/// W3-W update (2026-09-22): that follow-up surfaced two more reasons this
/// constant cannot be applied on the reflect_on_past/search path yet, beyond
/// the synthetic-runner-up issue above. (a) The search page mixes chunks,
/// reflections, TAD-decayed rows and scope-multiplied rows (0.3x for
/// out-of-family reflections, `CURRENT_PROJECT_ALL_SCOPE_BOOST` for
/// in-family ones) — margins on that page are not on the same scale this
/// constant was measured against. (b) Live receipts on that page (F2: 0.526
/// vs 0.414, margin 0.112; F5: 0.488 vs 0.349-by-position while rank 6 on
/// the same page actually scores 0.511, higher than rank 1) both clear this
/// threshold while being false or meaningless "leads" claims. So
/// `relevance_assessment`'s `margin_validated_for_path` parameter keeps this
/// constant decisive for `quick_check` (its exact-cosine top1-top2 margin is
/// what it was measured against) and forces the low-margin hedge on the
/// search path unconditionally, regardless of the arithmetic, until a
/// same-path threshold is derived.
pub const WEAK_BAND_DECISIVE_MARGIN: f32 = 0.10;

/// A relevance band plus the extra context needed to word a `weak`-band result
/// honestly. Bands themselves are unchanged from [`relevance_label`] — this
/// wraps it, never replaces it, so ordering/score/label-band behaviour for
/// every existing caller stays byte-identical. Only `weak`-band callers that
/// opt into this function gain qualifier/warning text; every other band gets
/// `qualifier: None, warning: None`.
pub struct Relevance {
    pub band: &'static str,
    /// Set only in the `weak` band: which side of the measured fabrication
    /// overlap this particular top score falls on.
    pub qualifier: Option<&'static str>,
    /// Human-readable caveat, set only in the `weak` band.
    pub warning: Option<String>,
}

/// Band a top score into the relevance vocabulary, and — only in the `weak`
/// band — decide whether the wording should be relative (this result leads
/// the field by a real margin) or an honest low-confidence caveat.
///
/// `runner_up` is the next-ranked result's score, when one exists, *restricted
/// to a comparable runner-up*: the caller must skip any candidate whose score
/// did not come from cosine similarity (see `synthetic_top` below) before
/// passing it here, or pass `None` if no comparable candidate exists. This
/// function does not itself know which channel `runner_up` came from — it
/// trusts the caller's filtering. `list_len` is the number of results the
/// caller is choosing among (informs the "list"/"field" phrasing only — it
/// plays no role in the band or margin math). `synthetic_top` marks a rank-1
/// score that did not come from cosine similarity at all — a keyword-only
/// chunk/reflection fallback score (`score_fts_candidate`,
/// `FTS_REFLECTION_SCORE`) is a fixed undated floor, not a measurement on the
/// same scale as [`WEAK_BAND_DECISIVE_MARGIN`] (derived from cosine scores).
/// Passing `true` suppresses the decisive branch entirely, regardless of the
/// arithmetic — a synthetic score "beating the runner-up by 0.10" is not
/// evidence of anything; it is just where the constant happens to sit. Never
/// touches score, order, `QUICK_CHECK_FLOOR`, or the abstention behaviour in
/// [`format_quick_check`] — those are computed entirely before this function
/// is called.
///
/// There is no absolute-score decisive branch — a prior version of this
/// function had one (top at or above a fabrication ceiling), and it was
/// removed because the ceiling it depended on was falsified by corpus growth
/// (see [`WEAK_BAND_DECISIVE_MARGIN`] docs). Decisiveness is margin-only,
/// gated on [`WEAK_BAND_DECISIVE_MARGIN`], and the margin is only ever
/// stated when it is positive — a negative margin (rerank moved a
/// lower-raw-score result to rank 1) or a missing runner-up never renders a
/// "leads by ..." clause, it falls through to the honest low-confidence
/// caveat instead.
///
/// `margin_validated_for_path` (review finding [high] #1): whether
/// [`WEAK_BAND_DECISIVE_MARGIN`] was actually derived against the scale this
/// call's margin lives on. `format_quick_check` computes an exact-cosine
/// top1-top2 margin — the same scale the constant was measured against — so
/// it passes `true`. `format_search_results_with_rank_scores` renders a page
/// that mixes chunks, reflections, TAD-decayed rows and scope-multiplied
/// rows; re-deriving a threshold for that mixed scale is a follow-up, not
/// done here, so it passes `false` and this function always renders the
/// low-margin hedge for it, regardless of how large the arithmetic margin
/// is. Live receipts prove this is not a hypothetical: on the search page,
/// "migrating csr-engine storage from SQLite to PostgreSQL" (0.526 vs 0.414,
/// margin 0.112) and "adding a Redis caching layer ..." (0.488 vs the actual
/// best-of-rest 0.511 — a NEGATIVE margin once the runner-up is computed
/// correctly) both cleared 0.10 by position before this fix while being
/// false or meaningless "leads" claims.
pub fn relevance_assessment(
    top: f32,
    runner_up: Option<f32>,
    list_len: usize,
    synthetic_top: bool,
    margin_validated_for_path: bool,
) -> Relevance {
    let band = relevance_label(top);
    if band != "weak" {
        return Relevance {
            band,
            qualifier: None,
            warning: None,
        };
    }

    let margin = runner_up.map(|second| top - second);
    let field_word = if list_len > 1 { "list" } else { "field" };
    // WEAK_BAND_DECISIVE_MARGIN is > 0.0, so `m >= WEAK_BAND_DECISIVE_MARGIN`
    // already implies a positive margin — no separate `m > 0.0` check needed.
    let decisive = margin_validated_for_path
        && !synthetic_top
        && margin.is_some_and(|m| m >= WEAK_BAND_DECISIVE_MARGIN);

    if decisive {
        // `decisive` only fires when `margin` is `Some` and positive, so this
        // always has a real number to report.
        let m = margin.expect("decisive requires a margin");
        Relevance {
            band,
            qualifier: Some("leads the field"),
            warning: Some(format!(
                "weak band by absolute score, but this result leads the {field_word} by a {m:.3} margin — absolute cosine similarity is weakly calibrated in the {QUICK_CHECK_FLOOR:.2}-{WEAK_BAND_TOP:.2} range on this corpus overall, but the margin over the rest of the {field_word} is real. Read the preview to confirm rather than treating the absolute score alone as a verdict."
            )),
        }
    } else {
        let channel_note = if synthetic_top {
            " — and this score came from a keyword match, not cosine similarity, so the margin check above does not apply to it at all"
        } else {
            ""
        };
        // Review finding [low] (W3-W): `margin_validated_for_path=false`
        // forces this branch regardless of the arithmetic (see the flag's
        // doc comment above), but the wording used to say "low margin" even
        // when the margin genuinely clears `WEAK_BAND_DECISIVE_MARGIN`. That
        // is a false statement of fact — the real reason for hedging on this
        // path is that the margin is uncalibrated, not that it is small.
        // Only claim "low margin" when it actually is (below the threshold,
        // or missing); otherwise say plainly that it is not calibrated here.
        let margin_uncalibrated_but_large = !margin_validated_for_path
            && !synthetic_top
            && margin.is_some_and(|m| m >= WEAK_BAND_DECISIVE_MARGIN);
        if margin_uncalibrated_but_large {
            Relevance {
                band,
                qualifier: Some("uncalibrated margin"),
                warning: Some(format!(
                    "weak match; this result's margin over the rest of the {field_word} is not calibrated on this search path — absolute cosine similarity is weakly calibrated in the {QUICK_CHECK_FLOOR:.2}-{WEAK_BAND_TOP:.2} range on this corpus overall, and {WEAK_BAND_DECISIVE_MARGIN:.2} was only ever validated against quick_check's exact-cosine margin, not this page's mixed scale{channel_note}. Read the preview to confirm before treating this as evidence the topic came up."
                )),
            }
        } else {
            Relevance {
                band,
                qualifier: Some("low margin"),
                warning: Some(format!(
                    "weak match, low margin over the rest of the {field_word} — absolute cosine similarity is weakly calibrated in the {QUICK_CHECK_FLOOR:.2}-{WEAK_BAND_TOP:.2} range on this corpus (genuine and fabricated-probe scores overlap here){channel_note}. Read the preview to confirm before treating this as evidence the topic came up."
                )),
            }
        }
    }
}

/// A search result enriched with chunk metadata.
#[derive(Clone)]
pub struct EnrichedResult {
    pub score: f32,
    pub chunk: ConversationChunk,
    /// Resolution-ledger annotation, when a verdict has been recorded for this chunk.
    pub resolution: Option<String>,
    /// Set ONLY by `mcp::tools::apply_validity_partition` when the v10 dream
    /// verdict structurally sank this result (Demote channel). The footer
    /// count reads THIS flag, never a substring of `resolution` — ledger
    /// evidence text that happens to contain "[stale anchor]" must not fire
    /// the dream-verdict note, and with `CSR_NO_VALIDITY_PARTITION=1` the
    /// flag is never set, so output is byte-identical to pre-partition
    /// behavior.
    pub validity_demoted: bool,
    /// Cached row-level provenance floor (chunks.min_trust or
    /// reflections.min_trust), read from the cache column only and always
    /// rendered: Unknown is a label, never an omission.
    pub trust: crate::provenance::TrustTier,
}

/// Effective rerank score for a result whose position differs from raw-score
/// order. Kept separate from [`EnrichedResult`] so other search surfaces retain
/// their existing result model.
#[derive(Clone)]
pub(crate) struct DisplayRankScore {
    pub chunk_id: String,
    pub adjusted_score: f32,
}

/// Drop multi-route and near-duplicate results, keeping first occurrence.
///
/// Input is assumed sorted by score descending. A result is dropped if either:
/// - its `chunk.id` was already seen, or
/// - the compound key `(conversation_id, normalized 200-char content prefix)`
///   was already seen.
///
/// Normalization: lowercase, collapse whitespace, take first 200 chars.
/// Cross-conversation collapsing is out of scope — identical content from
/// different conversation_ids both survive.
pub fn dedupe_results(results: &mut Vec<EnrichedResult>) {
    use std::collections::HashSet;

    let mut seen_ids = HashSet::new();
    let mut seen_keys = HashSet::new();

    results.retain(|r| {
        if !seen_ids.insert(r.chunk.id.clone()) {
            return false;
        }
        let normalized: String = r
            .chunk
            .content
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(200)
            .collect();
        let compound = (r.chunk.conversation_id.clone(), normalized);
        seen_keys.insert(compound)
    });
}

/// Drop plan-doc chunks whose origin conversation is already in the result set.
///
/// A `~/.claude/plans/*.md` chunk (conversation_id `plan:<slug>`) correlated to
/// conversation C carries the same decision text that C's ExitPlanMode turn does.
/// When both surface for one query, the origin conversation always survives and
/// the plan chunk is dropped — deterministic, no similarity threshold (Codex
/// adversarial review: content-shingle thresholds mis-collapse boilerplate and
/// can evict the authoritative side). `origin_of` maps a plan chunk id to its
/// correlated conversation id (from `chunk_provenance.source_conv_id`); plans
/// without a correlation are never dropped here.
pub fn dedupe_plan_origins(
    results: &mut Vec<EnrichedResult>,
    origin_of: &std::collections::HashMap<String, String>,
) {
    use std::collections::HashSet;
    let present_convs: HashSet<String> = results
        .iter()
        .filter(|r| !r.chunk.conversation_id.starts_with("plan:"))
        .map(|r| r.chunk.conversation_id.clone())
        .collect();
    results.retain(|r| {
        if !r.chunk.conversation_id.starts_with("plan:") {
            return true;
        }
        match origin_of.get(&r.chunk.id) {
            Some(conv) => !present_convs.contains(conv),
            None => true,
        }
    });
}

/// Drop sidechain chunks when their parent conversation is present. The parent
/// is the authoritative origin; a sidechain remains independently searchable
/// when the parent did not match.
pub fn dedupe_sidechain_origins(
    results: &mut Vec<EnrichedResult>,
    parent_of: &std::collections::HashMap<String, String>,
) {
    use std::collections::HashSet;
    let present_convs: HashSet<String> = results
        .iter()
        .filter(|result| !result.chunk.is_sidechain)
        .map(|result| result.chunk.conversation_id.clone())
        .collect();
    results.retain(|result| {
        if !result.chunk.is_sidechain {
            return true;
        }
        match parent_of.get(&result.chunk.id) {
            Some(parent) => !present_convs.contains(parent),
            None => true,
        }
    });
}

/// Format search results as rich XML matching the Python server output.
pub fn format_search_results(
    results: &[EnrichedResult],
    query: &str,
    project_scope: &str,
    search_ms: u64,
    embed_ms: u64,
) -> String {
    format_search_results_with_rank_scores(results, query, project_scope, search_ms, embed_ms, &[])
}

/// Format search results with the effective rerank scores for candidates whose
/// position differs from pure raw-score order.
pub(crate) fn format_search_results_with_rank_scores(
    results: &[EnrichedResult],
    query: &str,
    project_scope: &str,
    search_ms: u64,
    embed_ms: u64,
    display_rank_scores: &[DisplayRankScore],
) -> String {
    format_search_results_with_ce(
        results,
        query,
        project_scope,
        search_ms,
        embed_ms,
        display_rank_scores,
        None,
    )
}

/// Make the recall page's timing report `total_ms`: the time of the whole tool
/// call. That is the header's `PERFORMANCE: Nms` and the `<ttl>` of its `<perf>`
/// block. The formatter only knows the embed and search stages; a caller that
/// timed the call from request entry restamps the page with this. A page that
/// does not open with the results header (no results, an error note) is returned
/// as it came; a keyword-only page has no `PERFORMANCE` line, only the `<ttl>`.
pub(crate) fn restamp_performance(page: String, total_ms: u64) -> String {
    const HEADER: &str = "\u{1F3AF} RESULTS: ";
    const PERFORMANCE: &str = "\u{26A1} PERFORMANCE: ";
    if !page.starts_with(HEADER) {
        return page;
    }
    let Some((first, rest)) = page.split_once('\n') else {
        return page;
    };
    let (line, tail) = match rest.split_once('\n') {
        Some((second, tail)) => (second, tail),
        None => (rest, ""),
    };
    let stamped_line = line.strip_prefix(PERFORMANCE).and_then(|after| {
        let digits = after.chars().take_while(char::is_ascii_digit).count();
        (digits > 0 && after[digits..].starts_with("ms"))
            .then(|| format!("{PERFORMANCE}{total_ms}{}", &after[digits..]))
    });
    match stamped_line {
        Some(line) => format!("{first}\n{line}\n{}", restamp_ttl(tail, total_ms)),
        // No performance line: the `<ttl>` is somewhere in `rest`.
        None => format!("{first}\n{}", restamp_ttl(rest, total_ms)),
    }
}

/// The `<ttl>N</ttl>` of the page's `<perf>` block, set to `total_ms`. Only the
/// part before `<results>` is read: a result's own text is never touched.
fn restamp_ttl(tail: &str, total_ms: u64) -> String {
    const OPEN: &str = "<ttl>";
    const CLOSE: &str = "</ttl>";
    let head_len = tail.find("<results>").unwrap_or(tail.len());
    if let Some(start) = tail[..head_len].find(OPEN) {
        let value = start + OPEN.len();
        if let Some(len) = tail[value..].find(CLOSE) {
            if tail[value..value + len].chars().all(|c| c.is_ascii_digit()) {
                return format!("{}{}{}", &tail[..value], total_ms, &tail[value + len..]);
            }
        }
    }
    tail.to_string()
}

/// [`format_search_results_with_rank_scores`] plus the cross-encoder summary of
/// the recall. Relevance always describes the printed top score; CE margins
/// remain separate diagnostics in `ce-top` / `ce-margin`.
pub(crate) fn format_search_results_with_ce(
    results: &[EnrichedResult],
    query: &str,
    project_scope: &str,
    search_ms: u64,
    embed_ms: u64,
    display_rank_scores: &[DisplayRankScore],
    ce: Option<&CeSummary>,
) -> String {
    let mut out = String::new();
    let keyword_only = !results.is_empty()
        && results
            .iter()
            .all(|r| r.chunk.content.starts_with("[keyword] "));
    let labels = page_labels::PageLabels::new(results, query, keyword_only);

    // Upfront summary
    if results.is_empty() {
        out.push_str(&format!(
            "❌ NO RESULTS: No conversations found matching '{}'\n",
            query
        ));
    } else if keyword_only {
        out.push_str(&format!(
            "🎯 RESULTS: {} matches (keyword relevance; semantic confidence unavailable)\n",
            results.len()
        ));
    } else {
        let top_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::NEG_INFINITY, f32::max);
        let relevance = relevance_label(top_score);
        out.push_str(&format!(
            "🎯 RESULTS: {} matches ({} relevance, top score: {:.3})\n",
            results.len(),
            relevance,
            top_score,
        ));
        out.push_str(&format!(
            "⚡ PERFORMANCE: {}ms (1 collection searched)\n",
            search_ms + embed_ms,
        ));
    }

    out.push_str("\n<search>\n");

    // Summary
    if keyword_only {
        out.push_str(&format!(
            "  <summary count=\"{}\" relevance=\"keyword\">\n",
            results.len()
        ));
        out.push_str("    <confidence>semantic confidence unavailable</confidence>\n");
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(truncate_chars(&results[0].chunk.content, 100))
        ));
        out.push_str("  </summary>\n");
    } else if !results.is_empty() {
        let top_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::NEG_INFINITY, f32::max);
        let relevance = relevance_label(top_score);

        let preview = &results[0].chunk.content;
        let preview_short = if preview.chars().count() > 100 {
            format!("{}...", truncate_chars(preview, 100))
        } else {
            preview.clone()
        };

        // rank-1's own score (which may differ from `top_score`, the max raw
        // score across the whole page — display order follows the rerank, not
        // raw score).
        let rank1_score = results[0].score;

        // Rank-1's content carries a `[keyword] ` prefix exactly when its
        // score is synthetic (`score_fts_candidate` / `FTS_REFLECTION_SCORE`
        // in `mcp::tools`, both the FTS chunk fallback and the keyword-only
        // reflection path stamp this prefix) — that is the existing,
        // already-rendered signal for "this score is not cosine similarity",
        // reused here rather than threading a new field through the ~40
        // `EnrichedResult` construction sites.
        let rank1_synthetic = results
            .first()
            .is_some_and(|r| r.chunk.content.starts_with("[keyword] "));
        // Review finding [high] #2: rank 2 by position is not necessarily a
        // comparable runner-up. A `[keyword] `-prefixed row is the same
        // synthetic FTS/keyword-fallback floor as `rank1_synthetic` guards
        // against for rank 1 — comparing a real cosine top score's margin
        // against that fixed floor produced false "leads the list by ..."
        // claims (live receipt: a fabricated probe scoring 0.601 against a
        // `[keyword] ` rank-2 at 0.405 read as decisive with a 0.196
        // "margin" that was actually cosine-vs-floor, not cosine-vs-cosine).
        // Skip synthetic rows.
        //
        // Review finding [high] #1 (W3-W): the runner-up must be the MAX
        // score among ALL other non-`[keyword] ` rows on the page, not the
        // next one by display position — rerank order is not monotone in
        // score, so a lower-positioned row can outscore rank 1 (live
        // receipt: "adding a Redis caching layer ..." had rank 1 at 0.488
        // and rank 6 at 0.511, above rank 1; the old by-position runner-up
        // (rank 2 at 0.349) produced a false 0.139 "leads" margin instead of
        // the true negative one). A lower-positioned row that outscores
        // rank 1 must never let this claim "leads".
        //
        // Review finding [medium] (W3-W follow-up): this same
        // `comparable_runner_up` — MAX of the rest of the page, [keyword]
        // rows excluded — must also drive the machine-readable `margin`
        // attribute rendered in `<summary>` below, not just the `<caveat>`
        // wording. It used to be computed separately there as
        // `rank1_score - results.get(1).score` (next row by DISPLAY
        // POSITION, [keyword] rows counted) — the same by-position and
        // synthetic-floor defects this comment describes, just on a
        // different output field, which let the attribute contradict the
        // caveat sitting right next to it (live receipt: F2 rendered
        // `margin="0.270"` next to a caveat correctly saying "low margin
        // over the rest of the list", while ranks 4-5 on that same page
        // actually outscored rank 1). Hoisted above the `<summary>` format!
        // so both fields share one computation and can never drift apart.
        let comparable_runner_up = results
            .iter()
            .skip(1)
            .filter(|r| !r.chunk.content.starts_with("[keyword] "))
            .map(|r| r.score)
            .fold(None, |acc: Option<f32>, score| {
                Some(acc.map_or(score, |best: f32| best.max(score)))
            });
        let margin_attr = match comparable_runner_up {
            Some(second) => format!("{:.3}", rank1_score - second),
            None => "n/a".to_string(),
        };

        let ce_attrs = ce.map_or_else(String::new, |summary| {
            let gap = summary.runner_up.map_or_else(
                || "n/a".to_string(),
                |second| format!("{:.2}", summary.top - second),
            );
            format!(
                " ce-top=\"{:.2}\" ce-margin=\"{}\" ce-median=\"{:.2}\"",
                summary.top, gap, summary.median
            )
        });
        out.push_str(&format!(
            "  <summary count=\"{}\" relevance=\"{}\" top-score=\"{:.3}\" rank1-score=\"{:.3}\" margin=\"{}\"{}>\n",
            results.len(),
            relevance,
            top_score,
            rank1_score,
            margin_attr,
            ce_attrs,
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview_short)
        ));
        // Rank-1 can be in the weak band even when `relevance` (computed from
        // the page's max raw score) is not, and vice versa — the caveat is
        // about what Claude is about to read at the top of the list, so it is
        // keyed on rank 1 specifically.
        // Review finding [high] #1 part (b): even with the runner-up fixed,
        // this page's margin is not on a validated scale — it mixes chunks,
        // reflections, TAD-decayed rows and scope-multiplied rows (see
        // `WEAK_BAND_DECISIVE_MARGIN` docs). `quick_check`'s exact-cosine
        // top1-top2 margin is the only validated basis for this constant, so
        // `margin_validated_for_path: false` here always renders the honest
        // low-margin hedge on this path, regardless of the arithmetic.
        let rank1_assessment = relevance_assessment(
            rank1_score,
            comparable_runner_up,
            results.len(),
            rank1_synthetic,
            false,
        );
        if let Some(warning) = &rank1_assessment.warning {
            out.push_str(&format!("    <caveat>{}</caveat>\n", warning));
        }
        out.push_str("  </summary>\n");
    }

    // Meta
    out.push_str("  <meta>\n");
    out.push_str(&format!("    <q>{}</q>\n", xml_escape(query)));
    out.push_str(&format!(
        "    <scope>{}</scope>\n",
        xml_escape(project_scope)
    ));
    out.push_str(&format!("    <count>{}</count>\n", results.len()));
    if !results.is_empty() && !keyword_only {
        let min_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::INFINITY, f32::min);
        let max_score = results
            .iter()
            .map(|result| result.score)
            .fold(f32::NEG_INFINITY, f32::max);
        out.push_str(&format!(
            "    <range>{:.3}-{:.3}</range>\n",
            min_score, max_score,
        ));
    }
    out.push_str(&labels.meta_lines());
    out.push_str("    <perf>\n");
    out.push_str(&format!("      <ttl>{}</ttl>\n", search_ms + embed_ms));
    out.push_str(&format!("      <emb>{}</emb>\n", embed_ms));
    out.push_str(&format!("      <srch>{}</srch>\n", search_ms));
    out.push_str("      <cols>1</cols>\n");
    out.push_str("    </perf>\n");
    out.push_str("  </meta>\n");

    // Results
    out.push_str("  <results>\n");
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("    <r rank=\"{}\">\n", i + 1));
        if r.chunk.content.starts_with("[keyword] ") {
            out.push_str("      <relevance>keyword</relevance>\n");
        } else if let Some(rank_score) = display_rank_scores
            .iter()
            .find(|rank_score| rank_score.chunk_id == r.chunk.id)
        {
            out.push_str(&format!(
                "      <s adj=\"{:.3}\">{:.3}</s>\n",
                rank_score.adjusted_score, r.score
            ));
        } else {
            out.push_str(&format!("      <s>{:.3}</s>\n", r.score));
        }
        out.push_str(&format!(
            "      <p>{}</p>\n",
            xml_escape(&r.chunk.project_name)
        ));

        // Age stamp
        out.push_str(&format!("      <t>{}</t>\n", age_stamp(&r.chunk.timestamp)));

        // Excerpt
        let excerpt = &r.chunk.content;
        out.push_str(&format!(
            "      <excerpt><![CDATA[{}]]></excerpt>\n",
            excerpt
        ));

        // Conversation ID
        out.push_str(&format!("      <cid>{}</cid>\n", r.chunk.conversation_id));
        out.push_str(&format!("      <id>{}</id>\n", r.chunk.id));

        if let Some(ref note) = r.resolution {
            out.push_str(&format!(
                "      <resolution>{}</resolution>\n",
                xml_escape(note)
            ));
        }
        out.push_str(&format!("      <trust>{}</trust>\n", r.trust));
        if labels.is_weak(i) {
            out.push_str("      <tier>weak</tier>\n");
        }

        out.push_str("    </r>\n");
    }
    out.push_str("  </results>\n");

    let resolved_count = results
        .iter()
        .filter(|r| {
            r.resolution
                .as_deref()
                .is_some_and(|s| s.starts_with("resolved"))
        })
        .count();
    if resolved_count > 0 {
        out.push_str(&format!(
            "  <note>{} resolved item(s) demoted within page — matched but verified addressed</note>\n",
            resolved_count
        ));
    }

    // v10 dream verdicts: chunks whose bound code symbol is gone/fully stale
    // at the observed HEAD are sunk below every non-demoted result (never
    // dropped) by the search validity partition — see
    // `mcp::tools::apply_validity_partition`, the only writer of
    // `validity_demoted`. The flag (not a substring of `resolution`) drives
    // this count: ledger evidence text containing "[stale anchor]" must not
    // fire it, and with the kill switch on the flag is never set, so this
    // block contributes nothing (byte-identical pre-partition output).
    let demoted_count = results.iter().filter(|r| r.validity_demoted).count();
    if demoted_count > 0 {
        out.push_str(&format!(
            "  <note>{} item(s) demoted within page — bound code anchor no longer current (dream verdict)</note>\n",
            demoted_count
        ));
    }

    out.push_str("</search>\n");

    out
}

mod more_results;
pub use more_results::{format_more_keyword, format_more_results};
mod page_labels;
mod quick_check;
pub use quick_check::{
    format_keyword_quick_check, format_quick_check, format_quick_check_exact,
    format_quick_check_exact_miss, ExactHit,
};

/// Format search insights/summary as XML.
pub fn format_search_insights(results: &[EnrichedResult], query: &str) -> String {
    let mut out = String::new();

    if results.is_empty() {
        return "<search_summary><message>No matches found</message></search_summary>".into();
    }

    let keyword_only = results
        .iter()
        .all(|r| r.chunk.content.starts_with("[keyword] "));
    let avg_score: f32 = results.iter().map(|r| r.score).sum::<f32>() / results.len() as f32;

    out.push_str("<search_summary>\n");
    out.push_str(&format!("  <query>{}</query>\n", xml_escape(query)));
    out.push_str(&format!(
        "  <total_matches>{}</total_matches>\n",
        results.len()
    ));
    if keyword_only {
        out.push_str("  <relevance>keyword</relevance>\n");
        out.push_str("  <confidence>semantic confidence unavailable</confidence>\n");
    } else {
        out.push_str(&format!(
            "  <average_score>{:.3}</average_score>\n",
            avg_score
        ));
    }
    out.push_str("  <collections_matched>1</collections_matched>\n");
    if keyword_only {
        out.push_str(&format!(
            "  <insight>Found {} matches with keyword relevance</insight>\n",
            results.len()
        ));
    } else {
        out.push_str(&format!(
            "  <insight>Found {} matches with average relevance of {:.3}</insight>\n",
            results.len(),
            avg_score,
        ));
    }
    out.push_str(&insights::aggregates(results));
    out.push_str("</search_summary>\n");
    out
}

/// Format recent work results as XML.
pub fn format_recent_work(
    chunks: &[ConversationChunk],
    group_by: &str,
    notes: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::new();

    if chunks.is_empty() {
        return "<no_results>No recent conversations found.</no_results>".into();
    }

    match group_by {
        "day" => {
            // Group by day using BTreeMap for ordering
            let mut groups: std::collections::BTreeMap<String, Vec<&ConversationChunk>> =
                std::collections::BTreeMap::new();
            for chunk in chunks {
                let key = if let Some(ts) = parse_timestamp(&chunk.timestamp) {
                    ts.format("%Y-%m-%d").to_string()
                } else {
                    "unknown".into()
                };
                groups.entry(key).or_default().push(chunk);
            }

            out.push_str(&format!("<recent_work days='{}'>\n", groups.len()));
            for (day, day_chunks) in groups.iter().rev() {
                let projects: std::collections::HashSet<&str> =
                    day_chunks.iter().map(|c| c.project_name.as_str()).collect();
                out.push_str(&format!(
                    "  <day date='{}' conversations='{}'>\n",
                    day,
                    day_chunks.len()
                ));
                out.push_str(&format!(
                    "    <projects>{}</projects>\n",
                    projects.into_iter().collect::<Vec<_>>().join(", ")
                ));
                out.push_str("  </day>\n");
            }
            out.push_str("</recent_work>");
        }
        _ => {
            // Default: group by conversation. `chunks` arrives one row per
            // conversation, already ordered by last-activity descending
            // (`queries::get_recent_chunks`) — preserve that order instead of
            // re-sorting by conversation id, which is not chronological.
            let mut order: Vec<String> = Vec::new();
            let mut convs: std::collections::HashMap<String, Vec<&ConversationChunk>> =
                std::collections::HashMap::new();
            for chunk in chunks {
                convs
                    .entry(chunk.conversation_id.clone())
                    .or_insert_with(|| {
                        order.push(chunk.conversation_id.clone());
                        Vec::new()
                    })
                    .push(chunk);
            }

            out.push_str(&format!("<recent_work conversations='{}'>\n", order.len()));
            for conv_id in &order {
                let conv_chunks = &convs[conv_id];
                let most_recent = conv_chunks.iter().max_by_key(|c| &c.timestamp).unwrap();
                let preview = if most_recent.content.chars().count() > PREVIEW_CHARS {
                    format!("{}...", truncate_chars(&most_recent.content, PREVIEW_CHARS))
                } else {
                    most_recent.content.clone()
                };
                let relative = age_stamp(&most_recent.timestamp);
                out.push_str(&format!(
                    "  <conversation id='{}' time='{}' project='{}'>\n",
                    conv_id, relative, most_recent.project_name
                ));
                out.push_str(&format!(
                    "    <preview>{}</preview>\n",
                    xml_escape(&preview)
                ));
                if let Some(note) = notes.get(&most_recent.id) {
                    out.push_str(&format!(
                        "    <resolution>{}</resolution>\n",
                        xml_escape(note)
                    ));
                }
                out.push_str("  </conversation>\n");
            }
            out.push_str("</recent_work>");
        }
    }

    out
}

/// Format time-constrained search results.
pub fn format_recency_results(
    results: &[EnrichedResult],
    query: &str,
    time_range_desc: &str,
) -> String {
    format_recency_results_hinted(results, query, time_range_desc, None)
}

/// [`format_recency_results`] with a `hint` for the caller: said once, in the
/// header, when the window holds nothing that clears the relevance floor and a
/// wider window is worth trying. Rows are never added or dropped by it.
pub fn format_recency_results_hinted(
    results: &[EnrichedResult],
    query: &str,
    time_range_desc: &str,
    hint: Option<&str>,
) -> String {
    let mut out = String::new();
    let hint_line = hint
        .map(|text| format!("  <window_note>{}</window_note>\n", xml_escape(text)))
        .unwrap_or_default();

    if results.is_empty() {
        return format!(
            "<no_results>No results found for '{}' in the window {}.{}</no_results>",
            query,
            xml_escape(time_range_desc),
            hint.map(|text| format!(" {}", xml_escape(text)))
                .unwrap_or_default()
        );
    }

    out.push_str(&format!(
        "<search_results query='{}' time_range='{}' count='{}'>\n{hint_line}",
        xml_escape(query),
        xml_escape(time_range_desc),
        results.len()
    ));

    for (i, r) in results.iter().enumerate() {
        let relative = age_stamp(&r.chunk.timestamp);
        let preview = if r.chunk.content.chars().count() > PREVIEW_CHARS {
            format!("{}...", truncate_chars(&r.chunk.content, PREVIEW_CHARS))
        } else {
            r.chunk.content.clone()
        };
        if r.chunk.content.starts_with("[keyword] ") {
            out.push_str(&format!(
                "  <result rank='{}' relevance='keyword' time='{}'>\n",
                i + 1,
                relative
            ));
        } else {
            out.push_str(&format!(
                "  <result rank='{}' score='{:.3}' time='{}'>\n",
                i + 1,
                r.score,
                relative
            ));
        }
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview)
        ));
        out.push_str(&format!(
            "    <conversation_id>{}</conversation_id>\n",
            xml_escape(&r.chunk.conversation_id)
        ));
        if let Some(ref note) = r.resolution {
            out.push_str(&format!(
                "    <resolution>{}</resolution>\n",
                xml_escape(note)
            ));
        }
        out.push_str(&format!("    <trust>{}</trust>\n", r.trust));
        out.push_str("  </result>\n");
    }

    out.push_str("</search_results>");
    out
}

/// Format timeline grouped by period.
pub fn format_timeline(
    groups: &std::collections::BTreeMap<String, Vec<&ConversationChunk>>,
    time_range_desc: &str,
) -> String {
    let mut out = String::new();

    if groups.is_empty() {
        return "<timeline>No activity found in the specified time range.</timeline>".into();
    }

    out.push_str(&format!(
        "<timeline range='{}' periods='{}'>\n",
        time_range_desc,
        groups.len()
    ));

    for (period_key, chunks) in groups {
        let msg_count: usize = chunks.iter().map(|c| c.message_count).sum();
        out.push_str(&format!(
            "  <period key='{}' chunks='{}'>\n",
            period_key,
            chunks.len()
        ));
        out.push_str(&format!("    <stats messages='{}'/>\n", msg_count));
        out.push_str("  </period>\n");
    }

    out.push_str("</timeline>");
    out
}

/// Format file-based search results. `indexed` reports whether the code
/// graph has any extraction record for this file at all — false here means
/// this is a pure keyword fallback with no graph backing whatsoever.
pub fn format_file_results(
    chunks: &[ConversationChunk],
    file_path: &str,
    indexed: bool,
    notes: &std::collections::HashMap<String, String>,
) -> String {
    let mut out = String::new();

    if chunks.is_empty() {
        return format!(
            "<file_search indexed='{}'><message>No conversations found analyzing {}</message></file_search>",
            indexed,
            xml_escape(file_path)
        );
    }

    out.push_str(&format!(
        "<file_search file='{}' count='{}' indexed='{}'>\n",
        xml_escape(file_path),
        chunks.len(),
        indexed
    ));

    for (i, chunk) in chunks.iter().enumerate() {
        let relative = age_stamp(&chunk.timestamp);
        let preview = if chunk.content.chars().count() > PREVIEW_CHARS {
            format!("{}...", truncate_chars(&chunk.content, PREVIEW_CHARS))
        } else {
            chunk.content.clone()
        };
        out.push_str(&format!(
            "  <result rank='{}' time='{}'>\n",
            i + 1,
            relative
        ));
        out.push_str(&format!(
            "    <preview>{}</preview>\n",
            xml_escape(&preview)
        ));
        out.push_str(&format!(
            "    <conversation_id>{}</conversation_id>\n",
            xml_escape(&chunk.conversation_id)
        ));
        if let Some(note) = notes.get(&chunk.id) {
            out.push_str(&format!(
                "    <resolution>{}</resolution>\n",
                xml_escape(note)
            ));
        }
        out.push_str("  </result>\n");
    }

    out.push_str("</file_search>");
    out
}

/// Format get_full_conversation response.
pub fn format_full_conversation(
    conversation_id: &str,
    file_path: Option<&str>,
    project: Option<&str>,
) -> String {
    if let Some(path) = file_path {
        format!(
            "<conversation_file>\n<conversation_id>{}</conversation_id>\n<file_path>{}</file_path>\n<project>{}</project>\n<message>Use the Read tool with this file path to read the complete conversation. For structured facts (stats/prompts/tools/files/errors/slice/grep) without reading the raw file, use csr_transcript or `csr-engine transcript {} <view>` instead.</message>\n</conversation_file>",
            conversation_id,
            path,
            project.unwrap_or("unknown"),
            conversation_id,
        )
    } else {
        format!(
            "<conversation_file>\n<error>Conversation ID '{}' not found in any project.</error>\n<suggestion>The conversation may not have been imported yet, or the ID may be incorrect.</suggestion>\n</conversation_file>",
            conversation_id
        )
    }
}

/// get_full_conversation response when the JSONL is gone and only the
/// transcript mirror's scrubbed zstd copy is left. The file is not readable
/// with the Read tool, so the message routes to `csr_transcript`.
pub fn format_mirrored_conversation(
    conversation_id: &str,
    mirror_path: &str,
    project: &str,
) -> String {
    format!(
        "<conversation_file>\n<conversation_id>{}</conversation_id>\n<mirror_path>{}</mirror_path>\n<project>{}</project>\n<message>The original JSONL no longer exists. CSR kept a scrubbed, zstd-compressed copy at this path; it cannot be read with the Read tool. Read it with csr_transcript(session=\"{}\", view=...) or `csr-engine transcript {} <view>`.</message>\n</conversation_file>",
        conversation_id, mirror_path, project, conversation_id, conversation_id,
    )
}

/// Format session learnings response.
pub fn format_session_learnings(
    session_id: &str,
    learnings: &[(String, Vec<String>, String)], // (content, tags, timestamp)
) -> String {
    let mut out = String::new();

    out.push_str("<session_learnings>\n");
    out.push_str(&format!("  <session_id>{}</session_id>\n", session_id));
    out.push_str(&format!("  <count>{}</count>\n", learnings.len()));

    if learnings.is_empty() {
        out.push_str(&format!(
            "  <message>No learnings stored yet for this session. Use store_reflection() with tags=['session_{}'] to store iteration learnings.</message>\n",
            session_id
        ));
    } else {
        out.push_str("  <learnings>\n");
        for (content, tags, timestamp) in learnings {
            // Extract iteration from tags
            let iteration = tags
                .iter()
                .find(|t| t.starts_with("iteration_"))
                .map(|t| t.strip_prefix("iteration_").unwrap_or("unknown"))
                .unwrap_or("unknown");

            out.push_str(&format!("    <learning iteration=\"{}\">\n", iteration));
            out.push_str(&format!(
                "      <timestamp>{}</timestamp>\n",
                xml_escape(&age_stamp(timestamp))
            ));
            out.push_str(&format!(
                "      <content>{}</content>\n",
                xml_escape(content)
            ));
            out.push_str(&format!(
                "      <tags>{}</tags>\n",
                xml_escape(&tags.join(", "))
            ));
            out.push_str("    </learning>\n");
        }
        out.push_str("  </learnings>\n");
    }

    out.push_str("</session_learnings>\n");
    out
}

/// Helper: compute relative time string from an ISO timestamp.
fn relative_time_str(timestamp: &str) -> String {
    if let Some(ts) = parse_timestamp(timestamp) {
        let days_ago = (Utc::now() - ts).num_days();
        match days_ago {
            0 => "today".into(),
            1 => "yesterday".into(),
            d if d < 7 => format!("{}d ago", d),
            d if d < 30 => format!("{}w ago", d / 7),
            d => format!("{}mo ago", d / 30),
        }
    } else {
        "unknown".into()
    }
}

/// Render an absolute-date + relative-age stamp: `as of 2026-07-03 (3w ago)`.
/// Falls back to the raw string unchanged if the timestamp cannot be parsed.
pub fn age_stamp(timestamp: &str) -> String {
    match parse_timestamp(timestamp) {
        Some(ts) => format!(
            "as of {} ({})",
            ts.format("%Y-%m-%d"),
            relative_time_str(timestamp)
        ),
        None => timestamp.to_string(),
    }
}

/// Render a resolution-ledger entry as a short annotation string.
/// `created_at` is an RFC3339 timestamp; only the date portion (first 10
/// chars, `YYYY-MM-DD`) is shown — falls back to the full string if shorter
/// than 10 chars.
pub fn resolution_note(
    entry_status: &str,
    evidence: &str,
    created_at: &str,
    source: &str,
) -> Option<String> {
    if source != crate::storage::queries::RESOLUTION_SOURCE_USER_CONFIRMED {
        return None;
    }
    let date = if created_at.len() >= 10 {
        &created_at[..10]
    } else {
        created_at
    };
    Some(match entry_status {
        "resolved" => format!("resolved — {} (verified {})", evidence, date),
        "still_open" => format!("still open — verified {}", date),
        "regressed" => format!("regressed — {} ({})", evidence, date),
        other => format!("{} — {} ({})", other, evidence, date),
    })
}

// ─── v9.4 code property graph formatters ───

/// Format a file ledger (§8b): deterministic, immutable per-file dossier.
pub fn format_file_ledger(ledger: &crate::storage::codegraph::FileLedger) -> String {
    use std::fmt::Write as _;

    if ledger.symbols.is_empty() && ledger.timeline.is_empty() {
        return format!(
            "<file_ledger file='{}' indexed='{}'><message>No graph or evolution history for {}</message></file_ledger>",
            xml_escape(&ledger.file),
            ledger.indexed,
            xml_escape(&ledger.file)
        );
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "<file_ledger file='{}' indexed='{}'>",
        xml_escape(&ledger.file),
        ledger.indexed
    );

    // Symbols now (with conversation provenance).
    out.push_str("  <symbols_now>\n");
    for s in &ledger.symbols {
        // span_start/span_end are 0-based line numbers; the module-node
        // sentinel hardcodes both to 0, which is not a real span — omit
        // `lines=` rather than print the misleading '1-1'.
        let lines_attr = if s.span_start == 0 && s.span_end == 0 {
            String::new()
        } else {
            format!(" lines='{}-{}'", s.span_start + 1, s.span_end + 1)
        };
        // WP2 Stage 2: `attribution` (code_node_attribution, two channels)
        // replaces `first_conv_id` as introduction evidence here —
        // first_conv_id is a file-level projection (H4, receipt R2), never
        // a per-symbol fact. `last_conv` stays: it is a "last touched"
        // signal, not an introduction claim.
        let attribution = if s.attribution.is_empty() {
            "unattributed"
        } else {
            s.attribution.as_str()
        };
        let _ = writeln!(
            out,
            "    <symbol kind='{}' name='{}' attribution='{}' last_conv='{}' body_hash='{}'{}/>",
            xml_escape(&s.kind),
            xml_escape(&s.name),
            xml_escape(attribution),
            xml_escape(&s.last_conv_id),
            xml_escape(&s.body_hash),
            lines_attr,
        );
    }
    out.push_str("  </symbols_now>\n");

    // Timeline (code_evolution).
    out.push_str("  <timeline>\n");
    for t in &ledger.timeline {
        let added = parse_json_names(&t.functions_added);
        let removed = parse_json_names(&t.functions_removed);
        let rel = relative_time_str(&t.timestamp);
        let _ = writeln!(
            out,
            "    <change time='{}' session='{}' tool='{}' fns_added='{}' fns_removed='{}'/>",
            rel,
            xml_escape(&t.session_id),
            xml_escape(&t.tool_name),
            xml_escape(&added.join(",")),
            xml_escape(&removed.join(",")),
        );
    }
    out.push_str("  </timeline>\n");

    // Callers (who depends on this file).
    out.push_str("  <callers>\n");
    for (name, file) in &ledger.callers {
        let _ = writeln!(
            out,
            "    <caller name='{}' file='{}'/>",
            xml_escape(name),
            xml_escape(file),
        );
    }
    out.push_str("  </callers>\n");

    out.push_str("</file_ledger>");
    out
}

/// Format a code-graph query result (neighbors | callers | callees).
pub fn format_code_graph(
    mode: &str,
    target: &str,
    nodes: &[crate::storage::codegraph::NodeRow],
    neighbors: &[crate::storage::codegraph::NeighborEdge],
) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "<code_graph mode='{}' target='{}'>",
        xml_escape(mode),
        xml_escape(target)
    );

    if mode == "neighbors" {
        if neighbors.is_empty() {
            out.push_str("  <message>No neighbors found</message>\n");
        }
        for ne in neighbors {
            let attribution = if ne.node.attribution.is_empty() {
                "unattributed"
            } else {
                ne.node.attribution.as_str()
            };
            let _ = writeln!(
                out,
                "  <edge dir='{}' kind='{}' resolved='{}' name='{}' node_kind='{}' file='{}' last_conv='{}' attribution='{}'/>",
                xml_escape(&ne.direction),
                xml_escape(&ne.edge_kind),
                ne.resolved,
                xml_escape(&ne.node.name),
                xml_escape(&ne.node.kind),
                xml_escape(&ne.node.file),
                xml_escape(&ne.node.last_conv_id),
                xml_escape(attribution),
            );
        }
    } else {
        if nodes.is_empty() {
            let _ = writeln!(out, "  <message>No {} found</message>", xml_escape(mode));
        }
        for n in nodes {
            let match_attr = if n.name_only {
                "name-only"
            } else {
                "definition"
            };
            // span_start/span_end are 0-based line numbers; the module-node
            // sentinel hardcodes both to 0 — omit `lines=` rather than print '1-1'.
            let lines_attr = if n.span_start == 0 && n.span_end == 0 {
                String::new()
            } else {
                format!(" lines='{}-{}'", n.span_start + 1, n.span_end + 1)
            };
            let attribution = if n.attribution.is_empty() {
                "unattributed"
            } else {
                n.attribution.as_str()
            };
            let _ = writeln!(
                out,
                "  <node name='{}' kind='{}' file='{}' last_conv='{}' match='{}' attribution='{}'{}/>",
                xml_escape(&n.name),
                xml_escape(&n.kind),
                xml_escape(&n.file),
                xml_escape(&n.last_conv_id),
                match_attr,
                xml_escape(attribution),
                lines_attr,
            );
        }
    }

    out.push_str("</code_graph>");
    out
}

/// The file-search answer when no conversation touched the file but the code
/// map holds it: the plain statement, then the map's section.
pub fn format_file_map_only(
    file_path: &str,
    indexed: bool,
    views: usize,
    code_map_section: &str,
) -> String {
    format!(
        "<file_search file='{}' indexed='{}'>\n  <message>No conversation touched this file; the code map holds it in {} {}.</message>\n{}\n</file_search>",
        xml_escape(file_path),
        indexed,
        views,
        if views == 1 { "view" } else { "views" },
        code_map_section
    )
}

/// The code-graph answer when the conversation code graph does not know the
/// target but the code map does. Says where the answer comes from and, for the
/// edge modes, that call edges are not in the map.
pub fn format_code_graph_from_map(mode: &str, target: &str, code_map_section: &str) -> String {
    let edges = match mode {
        "callers" => " Callers are not in the code map.",
        "callees" => " Callees are not in the code map.",
        _ => "",
    };
    format!(
        "<code_graph mode='{}' target='{}' source='code_map'>\n  <message>Not in the conversation code graph. This answer comes from the code map: definitions, not call edges.{}</message>\n{}\n</code_graph>",
        xml_escape(mode),
        xml_escape(target),
        edges,
        code_map_section
    )
}

/// Byte budget of a `<code_map>` section.
pub const CODE_MAP_SECTION_BUDGET: usize = 1500;

fn lines_attr(span_start: i64, span_end: i64) -> String {
    format!("{}-{}", span_start + 1, span_end + 1)
}

/// Render `render(a, b)` with `a` and `b` shrunk until the text fits the budget:
/// `b` (defs per file) halves down to zero first, then `a` (items) drops by one.
/// Falls back to `empty` when even one bare item does not fit.
fn fit_section(
    items: usize,
    max_inner: usize,
    empty: String,
    render: impl Fn(usize, usize) -> String,
) -> String {
    let mut shown = items;
    let mut inner = max_inner;
    loop {
        let text = render(shown, inner);
        if text.len() <= CODE_MAP_SECTION_BUDGET {
            return text;
        }
        if inner > 0 {
            inner = if inner > 3 { inner / 2 } else { inner - 1 };
        } else if shown > 1 {
            shown -= 1;
        } else {
            return empty;
        }
    }
}

/// The `<code_map>` section for files: per path, the views holding it grouped
/// by blob, and the definitions of the blob most views share. At most
/// [`CODE_MAP_SECTION_BUDGET`] bytes; empty string when there are no files.
/// `total_paths` is how many paths matched before the caller's cap.
pub fn format_code_map_files(files: &[crate::codemap::CodemapFile], total_paths: usize) -> String {
    use std::fmt::Write as _;

    if files.is_empty() {
        return String::new();
    }
    let total_paths = total_paths.max(files.len());
    let render = |shown: usize, max_defs: usize| {
        let mut out = String::new();
        let _ = writeln!(out, "<code_map paths='{total_paths}'>");
        for file in files.iter().take(shown) {
            let _ = writeln!(
                out,
                "  <file path='{}' repo='{}' views='{}'>",
                xml_escape(&file.path),
                xml_escape(&crate::codemap::files::repo_label(&file.repo)),
                file.view_count(),
            );
            let _ = writeln!(
                out,
                "    <presence>{}</presence>",
                xml_escape(&crate::codemap::files::presence_summary(&file.presence))
            );
            for def in file.defs.iter().take(max_defs) {
                let _ = writeln!(
                    out,
                    "    <def name='{}' kind='{}' lines='{}'/>",
                    xml_escape(&def.name),
                    xml_escape(&def.kind),
                    lines_attr(def.span_start, def.span_end),
                );
            }
            let hidden = file
                .defs_total
                .saturating_sub(max_defs.min(file.defs.len()));
            if hidden > 0 {
                let _ = writeln!(out, "    <more defs='{hidden}'/>");
            }
            out.push_str("  </file>\n");
        }
        if shown < files.len() || total_paths > files.len() {
            let _ = writeln!(out, "  <more paths='{}'/>", total_paths - shown);
        }
        out.push_str("</code_map>");
        out
    };
    fit_section(
        files.len(),
        crate::codemap::files::MAX_FILE_DEFS.min(12),
        format!("<code_map paths='{total_paths}' truncated='true'/>"),
        render,
    )
}

/// The `<code_map>` section for definitions of one symbol name: each with its
/// file, lines and per-view presence. At most [`CODE_MAP_SECTION_BUDGET`]
/// bytes; empty string when there are no hits.
pub fn format_code_map_definitions(hits: &[crate::codemap::CodemapHit]) -> String {
    use std::fmt::Write as _;

    if hits.is_empty() {
        return String::new();
    }
    let render = |shown: usize, _unused: usize| {
        let mut out = String::new();
        let _ = writeln!(out, "<code_map definitions='{}'>", hits.len());
        for hit in hits.iter().take(shown) {
            let _ = writeln!(
                out,
                "  <definition name='{}' kind='{}' repo='{}' path='{}' lines='{}' presence='{}'/>",
                xml_escape(&hit.name),
                xml_escape(&hit.kind),
                xml_escape(&crate::codemap::files::repo_label(&hit.repo)),
                xml_escape(&hit.path),
                lines_attr(hit.span_start, hit.span_end),
                xml_escape(&crate::codemap::files::presence_summary(&hit.presence)),
            );
        }
        if shown < hits.len() {
            let _ = writeln!(out, "  <more definitions='{}'/>", hits.len() - shown);
        }
        out.push_str("</code_map>");
        out
    };
    fit_section(
        hits.len(),
        0,
        format!("<code_map definitions='{}' truncated='true'/>", hits.len()),
        render,
    )
}

/// Parse a JSON string array of names; empty vec on any error.
fn parse_json_names(json: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json).unwrap_or_default()
}

mod insights;

#[cfg(test)]
mod quick_check_tests;

#[cfg(test)]
mod code_map_tests;

#[cfg(test)]
mod score_timeline_tests;

#[cfg(test)]
mod tests;
