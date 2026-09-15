//! Journal v4 Phase 5 — the **delivery** side of dreaming: what has been
//! shown to the user, on which channel, and what is still unseen.
//!
//! Three channels deliver dreams outside the journal page (locked decision 6):
//! the statusline badge, the SessionStart recap clause, and the prompt-time
//! match. The first is a count; the other two show ONE dream each and must
//! not repeat themselves, so both record what they showed here.
//!
//! # What identifies a dream on a delivery channel
//!
//! A [`conclusion_id`]: the first 16 hex of
//! `sha256(project ‖ file ‖ symbol ‖ verdict ‖ receipt_oid)`. It is derived
//! entirely from the stored evidence tuple, so the same conclusion keeps the
//! same id across passes, and a conclusion whose receipt changes is a
//! different conclusion — which it is. This is deliberately NOT
//! `dream_clusters`'s cluster id: the cluster feed parses every v2 episode to
//! build a cluster, which is far too much work for a hook or a statusline,
//! and the delivery channels only ever name a single conclusion anyway.
//!
//! # Honesty rules encoded here
//!
//! * A conclusion with **no receipt** is never a delivery candidate. The
//!   receipt is what makes the clause quotable at all.
//! * The badge count is a **measured baseline** written by a pass that
//!   actually ran, minus deliveries actually recorded since. When no pass has
//!   ever written a baseline, [`badge_unread`] returns `None` and the
//!   statusline drops the segment — it never renders `0`, which would claim
//!   "nothing new" on evidence nobody gathered.
//! * A delivery row proves the user was shown something. Its absence proves
//!   nothing, so nothing is ever inferred from it beyond "do not repeat".

use anyhow::Result;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use super::Storage;

/// `meta` key: number of undelivered conclusions counted by the last pass.
pub const META_BADGE_TOTAL: &str = "dream_badge_total";
/// `meta` key: RFC3339 timestamp of the pass that wrote
/// [`META_BADGE_TOTAL`] — reported by `status` so the badge can say *when*
/// it was measured. It is never used for the arithmetic itself (see
/// [`META_BADGE_CURSOR`]).
pub const META_BADGE_AT: &str = "dream_badge_at";
/// `meta` key: `MAX(dream_deliveries.id)` at the moment the baseline was
/// measured. Deliveries are subtracted by **id**, not by timestamp:
/// `delivered_at` is `datetime('now')` at one-second granularity, so a
/// delivery written in the same second as the baseline could otherwise be
/// counted on both sides of the subtraction. An autoincrement id cannot
/// alias.
pub const META_BADGE_CURSOR: &str = "dream_badge_cursor";

/// Which surface showed a dream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryChannel {
    /// The SessionStart recap paragraph's dream clause.
    Recap,
    /// The UserPromptSubmit symbol/file match.
    Prompt,
}

impl DeliveryChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryChannel::Recap => "recap",
            DeliveryChannel::Prompt => "prompt",
        }
    }
}

/// One conclusion, ready to be named on a delivery channel. Every field is
/// copied from a stored row; nothing here is composed or inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DreamHeadline {
    pub id: String,
    pub project: String,
    pub file: String,
    pub symbol: Option<String>,
    pub verdict: String,
    /// Never empty — a headline without a receipt is not constructed.
    pub receipt_oid: String,
    pub witnessed_at: String,
    /// `YYYY-MM-DD` slice of `witnessed_at`, or the whole value when it is
    /// shorter. Presentation only.
    pub witnessed_date: String,
}

impl DreamHeadline {
    /// What the conclusion is about: the symbol when there is one, else the
    /// file's basename. Never a fabricated label.
    pub fn label(&self) -> &str {
        match self.symbol.as_deref() {
            Some(symbol) if !symbol.trim().is_empty() => symbol,
            _ => self.file.rsplit('/').next().unwrap_or(&self.file),
        }
    }

    /// Plain-English phrasing of the stored verdict. Unknown verdicts are
    /// passed through verbatim rather than mapped to a guess.
    pub fn verdict_phrase(&self) -> &str {
        match self.verdict.as_str() {
            "anchor_obsolete" => "went stale",
            "superseded_by" => "was superseded",
            "anchor_reinstated" => "was reinstated",
            other => other,
        }
    }
}

/// `sha256(project ‖ file ‖ symbol ‖ verdict ‖ receipt_oid)`, first 16 hex.
pub fn conclusion_id(
    project: &str,
    file: &str,
    symbol: Option<&str>,
    verdict: &str,
    receipt_oid: &str,
) -> String {
    let mut hasher = Sha256::new();
    for part in [project, file, symbol.unwrap_or(""), verdict, receipt_oid] {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn date_of(timestamp: &str) -> String {
    timestamp.chars().take(10).collect()
}

fn headline_from_parts(
    project: String,
    file: String,
    symbol: Option<String>,
    verdict: String,
    receipt_oid: String,
    witnessed_at: String,
) -> Option<DreamHeadline> {
    if receipt_oid.trim().is_empty() {
        return None;
    }
    Some(DreamHeadline {
        id: conclusion_id(
            &project,
            &file,
            symbol.as_deref(),
            &verdict,
            receipt_oid.trim(),
        ),
        witnessed_date: date_of(&witnessed_at),
        project,
        file,
        symbol,
        verdict,
        receipt_oid: receipt_oid.trim().to_string(),
        witnessed_at,
    })
}

/// Every receipt-bearing conclusion for `project`, newest witnessed first.
/// Adverse verdicts (`anchor_obsolete`, `superseded_by`) sort ahead of
/// restorative ones at equal date, mirroring `dream_clusters`'s tier 2 —
/// this is the same priority order, applied to a single row rather than a
/// cluster.
pub fn receipted_conclusions(conn: &Connection, project: &str) -> Result<Vec<DreamHeadline>> {
    let mut stmt = conn.prepare(
        "SELECT l.file, l.symbol, v.verdict, v.receipt_oid, MAX(v.created_at) AS witnessed_at
         FROM witness_verdicts v
         JOIN witness_ledger l ON l.id = v.witness_id
         WHERE l.project = ?1
           AND v.receipt_oid IS NOT NULL
           AND TRIM(v.receipt_oid) != ''
         GROUP BY l.file, l.symbol, v.verdict, v.receipt_oid
         ORDER BY witnessed_at DESC",
    )?;
    let rows = stmt
        .query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out: Vec<DreamHeadline> = rows
        .into_iter()
        .filter_map(|(file, symbol, verdict, receipt_oid, witnessed_at)| {
            headline_from_parts(
                project.to_string(),
                file,
                symbol,
                verdict,
                receipt_oid,
                witnessed_at,
            )
        })
        .collect();
    out.sort_by(|a, b| {
        b.witnessed_date
            .cmp(&a.witnessed_date)
            .then_with(|| verdict_rank(&a.verdict).cmp(&verdict_rank(&b.verdict)))
            .then_with(|| b.witnessed_at.cmp(&a.witnessed_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// Lower sorts first: adverse before restorative before anything unknown.
fn verdict_rank(verdict: &str) -> u8 {
    match verdict {
        "anchor_obsolete" => 0,
        "superseded_by" => 1,
        "anchor_reinstated" => 2,
        _ => 3,
    }
}

/// Cheap existence probe: does this project have any receipt-bearing verdict
/// at all? Hooks call this before doing any real work, so a project with no
/// dreams costs one indexed count.
pub fn has_receipted_conclusion(conn: &Connection, project: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*)
         FROM witness_verdicts v
         JOIN witness_ledger l ON l.id = v.witness_id
         WHERE l.project = ?1
           AND v.receipt_oid IS NOT NULL
           AND TRIM(v.receipt_oid) != ''",
        params![project],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Has `dream_id` already been delivered on `channel`?
pub fn already_delivered(conn: &Connection, dream_id: &str, channel: DeliveryChannel) -> bool {
    conn.query_row(
        "SELECT 1 FROM dream_deliveries WHERE dream_id = ?1 AND channel = ?2 LIMIT 1",
        params![dream_id, channel.as_str()],
        |_| Ok(()),
    )
    .is_ok()
}

/// Record that `dream_id` was shown on `channel`. Returns `true` when this
/// call is what recorded it (i.e. it had not been delivered before) — the
/// probe-cache dedupe the injection path gates on. `INSERT OR IGNORE`
/// against a `UNIQUE(dream_id, channel)` index makes the check and the write
/// one atomic step, so two hooks racing the same dream cannot both inject.
pub fn record_delivery(
    conn: &Connection,
    dream_id: &str,
    channel: DeliveryChannel,
    session_id: Option<&str>,
) -> Result<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO dream_deliveries (dream_id, channel, session_id)
         VALUES (?1, ?2, ?3)",
        params![dream_id, channel.as_str(), session_id],
    )?;
    Ok(changed > 0)
}

/// Claim `dream_id` for `channel` on a `Storage` handle, fail-soft. `false`
/// on any storage error — a delivery that could not be recorded must not be
/// shown, or it would repeat forever.
pub fn claim_delivery(
    storage: &Storage,
    dream_id: &str,
    channel: DeliveryChannel,
    session_id: Option<&str>,
) -> bool {
    storage
        .with_connection(|conn| record_delivery(conn, dream_id, channel, session_id))
        .unwrap_or(false)
}

/// Count distinct dreams delivered after row id `cursor`.
fn deliveries_after(conn: &Connection, cursor: i64) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT dream_id) FROM dream_deliveries WHERE id > ?1",
        params![cursor],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Highest delivery row id, or 0 when nothing has ever been delivered.
fn delivery_cursor(conn: &Connection) -> Result<i64> {
    let cursor: i64 = conn.query_row(
        "SELECT COALESCE(MAX(id), 0) FROM dream_deliveries",
        [],
        |row| row.get(0),
    )?;
    Ok(cursor)
}

/// Count the receipt-bearing conclusions that have never been delivered on
/// any channel. Run at the end of a pass, not on a hot path.
pub fn count_undelivered(conn: &Connection) -> Result<i64> {
    let mut projects = conn.prepare("SELECT DISTINCT project FROM witness_ledger")?;
    let names: Vec<String> = projects
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut undelivered = 0_i64;
    for project in names {
        for headline in receipted_conclusions(conn, &project)? {
            let delivered: i64 = conn.query_row(
                "SELECT COUNT(*) FROM dream_deliveries WHERE dream_id = ?1",
                params![headline.id],
                |row| row.get(0),
            )?;
            if delivered == 0 {
                undelivered += 1;
            }
        }
    }
    Ok(undelivered)
}

/// Recompute the statusline badge baseline. Called at the end of a completed
/// pass; fail-soft, since a badge is never worth failing a cycle over.
pub fn refresh_badge_baseline(storage: &Storage) {
    let total = match storage.with_connection(count_undelivered) {
        Ok(total) => total,
        Err(error) => {
            tracing::debug!(%error, "dream badge baseline unavailable (non-fatal)");
            return;
        }
    };
    let cursor = storage.with_connection(delivery_cursor).unwrap_or(0);
    let _ = storage.set_meta(META_BADGE_TOTAL, &total.to_string());
    let _ = storage.set_meta(META_BADGE_CURSOR, &cursor.to_string());
    let _ = storage.set_meta(META_BADGE_AT, &chrono::Utc::now().to_rfc3339());
}

/// Unread dream count for the statusline: the last pass's measured baseline
/// minus the dreams delivered since that pass. `None` when no pass has
/// written a baseline — the caller must then render nothing at all, never a
/// zero.
pub fn badge_unread(storage: &Storage) -> Option<i64> {
    let total = storage
        .get_meta(META_BADGE_TOTAL)
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i64>().ok())?;
    let cursor = storage
        .get_meta(META_BADGE_CURSOR)
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(0);
    let delivered = storage
        .with_connection(|conn| deliveries_after(conn, cursor))
        .unwrap_or(0);
    Some((total - delivered).max(0))
}

/// When the badge baseline was measured, for `status`. `None` until a pass
/// has written one.
pub fn badge_measured_at(storage: &Storage) -> Option<String> {
    storage.get_meta(META_BADGE_AT).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::WitnessLedgerRow;
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

    fn seed(storage: &Storage, project: &str, file: &str, symbol: &str, receipt: Option<&str>) {
        storage
            .with_connection(|conn| {
                let stamp = format!("b3:{file}:{symbol}");
                crate::storage::witness_ledger::insert_witness(
                    conn,
                    &WitnessLedgerRow {
                        project: project.into(),
                        file: file.into(),
                        symbol: Some(symbol.into()),
                        stamp: stamp.clone(),
                        tier: "committed".into(),
                        at_oid: Some("aaaa111".into()),
                        source_kind: "test".into(),
                        source_id: Some(stamp.clone()),
                        ..Default::default()
                    },
                )?;
                let witness_id: i64 = conn.query_row(
                    "SELECT id FROM witness_ledger WHERE project = ?1 AND stamp = ?2",
                    params![project, stamp],
                    |row| row.get(0),
                )?;
                witness_verdicts::insert_verdict_if_changed(
                    conn,
                    &WitnessVerdictRow {
                        witness_id,
                        verdict: VerdictKind::AnchorObsolete,
                        successor_witness_id: None,
                        receipt_oid: receipt.map(str::to_string),
                        observed_head_oid: "head".into(),
                    },
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn conclusion_id_is_stable_and_evidence_derived() {
        let a = conclusion_id(
            "proj",
            "src/a.rs",
            Some("run"),
            "anchor_obsolete",
            "abc1234",
        );
        let b = conclusion_id(
            "proj",
            "src/a.rs",
            Some("run"),
            "anchor_obsolete",
            "abc1234",
        );
        assert_eq!(a, b, "same evidence must yield the same id");
        assert_eq!(a.len(), 16);
        let different_receipt = conclusion_id(
            "proj",
            "src/a.rs",
            Some("run"),
            "anchor_obsolete",
            "def5678",
        );
        assert_ne!(a, different_receipt, "a new receipt is a new conclusion");
    }

    #[test]
    fn a_conclusion_without_a_receipt_is_never_a_delivery_candidate() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "proj", "src/a.rs", "run_pass", None);
        let rows = storage
            .with_connection(|conn| receipted_conclusions(conn, "proj"))
            .unwrap();
        assert!(rows.is_empty(), "receiptless verdict must not be offered");
        assert!(!storage
            .with_connection(|conn| has_receipted_conclusion(conn, "proj"))
            .unwrap());
    }

    #[test]
    fn receipted_conclusions_carry_their_receipt_and_label() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "proj", "src/a.rs", "run_pass", Some("abc1234"));
        let rows = storage
            .with_connection(|conn| receipted_conclusions(conn, "proj"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].receipt_oid, "abc1234");
        assert_eq!(rows[0].label(), "run_pass");
        assert_eq!(rows[0].verdict_phrase(), "went stale");
    }

    #[test]
    fn label_falls_back_to_the_file_basename_never_a_placeholder() {
        let headline = headline_from_parts(
            "proj".into(),
            "/repo/src/deep/mod.rs".into(),
            None,
            "superseded_by".into(),
            "abc1234".into(),
            "2026-08-11T10:00:00Z".into(),
        )
        .unwrap();
        assert_eq!(headline.label(), "mod.rs");
        assert_eq!(headline.verdict_phrase(), "was superseded");
        assert_eq!(headline.witnessed_date, "2026-08-11");
    }

    #[test]
    fn an_unknown_verdict_is_passed_through_verbatim() {
        let headline = headline_from_parts(
            "proj".into(),
            "src/a.rs".into(),
            Some("thing".into()),
            "some_future_verdict".into(),
            "abc1234".into(),
            "2026-08-11T10:00:00Z".into(),
        )
        .unwrap();
        assert_eq!(headline.verdict_phrase(), "some_future_verdict");
    }

    #[test]
    fn delivery_is_recorded_once_and_only_once() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                assert!(record_delivery(
                    conn,
                    "dream1",
                    DeliveryChannel::Prompt,
                    Some("s1")
                )?);
                assert!(
                    !record_delivery(conn, "dream1", DeliveryChannel::Prompt, Some("s2"))?,
                    "a second delivery on the same channel must be refused"
                );
                assert!(
                    record_delivery(conn, "dream1", DeliveryChannel::Recap, Some("s1"))?,
                    "channels are independent"
                );
                assert!(already_delivered(conn, "dream1", DeliveryChannel::Prompt));
                assert!(!already_delivered(conn, "dream2", DeliveryChannel::Prompt));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn badge_is_none_until_a_pass_measured_it() {
        let storage = Storage::open_memory().unwrap();
        assert_eq!(
            badge_unread(&storage),
            None,
            "no measured baseline must render nothing, never 0"
        );
    }

    #[test]
    fn badge_counts_undelivered_conclusions_and_falls_as_they_are_delivered() {
        let storage = Storage::open_memory().unwrap();
        seed(&storage, "proj", "src/a.rs", "alpha_fn", Some("abc1234"));
        seed(&storage, "proj", "src/b.rs", "beta_fn", Some("def5678"));
        refresh_badge_baseline(&storage);
        assert_eq!(badge_unread(&storage), Some(2));

        let first = storage
            .with_connection(|conn| receipted_conclusions(conn, "proj"))
            .unwrap()
            .remove(0);
        assert!(claim_delivery(
            &storage,
            &first.id,
            DeliveryChannel::Prompt,
            Some("s1")
        ));
        assert_eq!(
            badge_unread(&storage),
            Some(1),
            "a delivered dream stops being unread"
        );

        // A pass re-measuring after the delivery agrees with the arithmetic.
        refresh_badge_baseline(&storage);
        assert_eq!(badge_unread(&storage), Some(1));
    }

    #[test]
    fn badge_never_goes_negative() {
        let storage = Storage::open_memory().unwrap();
        storage.set_meta(META_BADGE_TOTAL, "1").unwrap();
        storage.set_meta(META_BADGE_CURSOR, "0").unwrap();
        for id in ["a", "b", "c"] {
            assert!(claim_delivery(
                &storage,
                id,
                DeliveryChannel::Prompt,
                Some("s")
            ));
        }
        assert_eq!(badge_unread(&storage), Some(0));
    }
}
