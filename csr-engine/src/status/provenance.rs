//! The provenance coverage block of `status`.
//!
//! The exact counts are two table scans (the span join and the receipt join),
//! about 6 s on a 980k-chunk store. The daemon measures them hourly and stores
//! the result in `meta`; every status read that is not `--deep` serves that row
//! (or reports it unavailable) and prints its age on stderr, and `--deep`
//! measures live. The JSON keys are the same in every mode.

use anyhow::Result;
use serde::Serialize;

use super::{ArtifactTierCounts, ProvenanceCoverageStatus};
use crate::storage::Storage;

/// `meta` row holding the daemon's last [`ProvenanceCache`] measurement.
pub const PROVENANCE_CACHE_META_KEY: &str = "status_provenance_cache_v1";

/// What the cached read prints, measured by the daemon so a status refresh
/// never scans `chunks` or the receipt tables.
#[derive(Serialize, serde::Deserialize, Debug, PartialEq)]
struct ProvenanceCache {
    chunks_with_spans: i64,
    chunks_unknown: i64,
    chunks_total: i64,
    tool_result_share_mean: Option<f64>,
    source_missing: i64,
    source_unparsed: i64,
    source_unmatched: i64,
    measured_at: String,
}

/// Measure the provenance coverage and store it in `meta` (daemon only; a
/// read-only handle measures and stores nothing). Both queries are table
/// scans, which is why status reads this row instead.
pub fn refresh_provenance_cache(storage: &Storage) -> Result<()> {
    let (chunks_with_spans, chunks_unknown, chunks_total, tool_result_share_mean) =
        storage.provenance_coverage()?;
    let [source_missing, source_unparsed, source_unmatched] =
        storage.provenance_failure_counts()?;
    let cache = ProvenanceCache {
        chunks_with_spans,
        chunks_unknown,
        chunks_total,
        tool_result_share_mean,
        source_missing,
        source_unparsed,
        source_unmatched,
        measured_at: chrono::Utc::now().to_rfc3339(),
    };
    if !storage.with_connection(|conn| Ok(conn.is_readonly("main")?))? {
        storage.set_meta(PROVENANCE_CACHE_META_KEY, &serde_json::to_string(&cache)?)?;
    }
    Ok(())
}

/// How old the daemon's provenance measurement may be before status stops
/// serving it. The daemon refreshes hourly, so a day-old row means the refresh
/// is failing while imports go on, and old counts would read as current.
const PROVENANCE_CACHE_MAX_AGE_HOURS: i64 = 24;

/// Where a report's provenance counts came from. Not part of the JSON: the
/// report keeps its key set, and the age is printed on stderr by `handle`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum ProvenanceBasis {
    /// Measured by this call (`--deep`), or not measured at all (compact).
    #[default]
    Live,
    /// The daemon's stored measurement, however old.
    Stored { measured_at: String, age_secs: i64 },
    /// No usable stored measurement and no live scan was asked for.
    Unavailable,
}

impl ProvenanceBasis {
    /// The one stderr line that says where the counts came from, if any.
    pub fn note(&self) -> Option<String> {
        match self {
            ProvenanceBasis::Live => None,
            ProvenanceBasis::Stored {
                measured_at,
                age_secs,
            } => Some(format!(
                "provenance_coverage: stored measurement from {measured_at}, {age_secs} s old; `status --deep` measures it now"
            )),
            ProvenanceBasis::Unavailable => Some(
                "provenance_coverage: unavailable (the daemon has not stored a measurement); `status --deep` measures it now"
                    .to_string(),
            ),
        }
    }
}

/// The daemon's last measurement and its age in seconds, whatever the age, or
/// `None` when there is none, it does not parse, or `measured_at` is in the
/// future. No artifact histogram.
fn stored_coverage_at(
    storage: &Storage,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(ProvenanceCoverageStatus, chrono::Duration)> {
    let cache = storage
        .get_meta(PROVENANCE_CACHE_META_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<ProvenanceCache>(&json).ok())?;
    let age = chrono::DateTime::parse_from_rfc3339(&cache.measured_at)
        .map(|at| now.signed_duration_since(at))
        .ok()
        .filter(|age| *age >= chrono::Duration::zero())?;
    let status = ProvenanceCoverageStatus {
        chunks_with_spans: cache.chunks_with_spans,
        chunks_unknown: cache.chunks_unknown,
        chunks_total: cache.chunks_total,
        tool_result_share_mean: cache.tool_result_share_mean,
        source_missing: cache.source_missing,
        source_unparsed: cache.source_unparsed,
        source_unmatched: cache.source_unmatched,
        artifacts: Vec::new(),
        basis: ProvenanceBasis::Stored {
            measured_at: cache.measured_at,
            age_secs: age.num_seconds(),
        },
    };
    Some((status, age))
}

/// The compact line's coverage: the daemon's last measurement, or the empty
/// block (no `prov` segment) when there is none, when it is older than
/// [`PROVENANCE_CACHE_MAX_AGE_HOURS`], or when it is not servable.
pub(super) fn cached_provenance_coverage(storage: &Storage) -> ProvenanceCoverageStatus {
    stored_coverage_at(storage, chrono::Utc::now())
        .filter(|(_, age)| *age <= chrono::Duration::hours(PROVENANCE_CACHE_MAX_AGE_HOURS))
        .map(|(status, _)| status)
        .unwrap_or_default()
}

/// The full report's coverage. `--deep` measures live (exact counts). Without
/// it the daemon's last measurement is served whatever its age, and the age is
/// printed; with none, the block is empty and marked unavailable. Status never
/// scans `chunks` or the receipts unless asked, so a stopped daemon cannot make
/// it slow. The per-family artifact histogram reads small tables and is always
/// live.
pub(super) fn full_provenance_coverage(storage: &Storage, deep: bool) -> ProvenanceCoverageStatus {
    full_coverage_at(storage, deep, chrono::Utc::now())
}

fn full_coverage_at(
    storage: &Storage,
    deep: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> ProvenanceCoverageStatus {
    if deep {
        return gather_provenance_coverage(storage);
    }
    let served = stored_coverage_at(storage, now)
        .map(|(status, _)| status)
        .unwrap_or_else(|| ProvenanceCoverageStatus {
            basis: ProvenanceBasis::Unavailable,
            ..Default::default()
        });
    ProvenanceCoverageStatus {
        artifacts: artifact_tier_counts(storage),
        ..served
    }
}

fn artifact_tier_counts(storage: &Storage) -> Vec<ArtifactTierCounts> {
    storage
        .artifact_tier_histograms()
        .map(|rows| {
            rows.into_iter()
                .map(|(kind, c)| ArtifactTierCounts {
                    kind: kind.as_str().to_string(),
                    unknown: c[0],
                    external: c[1],
                    trusted_tool: c[2],
                    user_history: c[3],
                    user_confirmed: c[4],
                    system: c[5],
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The live provenance coverage block: scans every chunk and every receipt, so
/// only `--deep` reaches it.
fn gather_provenance_coverage(storage: &Storage) -> ProvenanceCoverageStatus {
    let [source_missing, source_unparsed, source_unmatched] =
        storage.provenance_failure_counts().unwrap_or_default();
    let artifacts = artifact_tier_counts(storage);
    storage
        .provenance_coverage()
        .map(
            |(chunks_with_spans, chunks_unknown, chunks_total, tool_result_share_mean)| {
                ProvenanceCoverageStatus {
                    chunks_with_spans,
                    chunks_unknown,
                    chunks_total,
                    tool_result_share_mean,
                    source_missing,
                    source_unparsed,
                    artifacts: artifacts.clone(),
                    source_unmatched,
                    basis: ProvenanceBasis::Live,
                }
            },
        )
        .unwrap_or_default()
}
