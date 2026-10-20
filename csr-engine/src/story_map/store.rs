//! Shift Ledger (`reflections` + `meta["shift_ledger:<project>"]`) and
//! per-conversation Shift Map (`meta["story_map:<conv_id>"]`) persistence.

use std::collections::{BTreeSet, HashSet};

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::ground::{ground_shifts, GroundedShift, Part};
use super::{story_map_disabled, ShiftEntry};
use crate::storage::Storage;

/// `by` of a map derived from receipts alone (no model wrote it).
pub const RECEIPTS_BY: &str = "receipts";

/// Tag marking a Shift Ledger row in `reflections`.
pub const LEDGER_TAG: &str = "narrative_shift";

const LEDGER_VERSION: u32 = 1;
const LEDGER_MAX_ENTRIES: usize = 200;
const LEDGER_MAX_BYTES: usize = 32_768;
const MAX_RECEIPTS: usize = 6;

const STORY_MAP_VERSION: u32 = 1;
// A full map is four entries of ~800 bytes with two-endpoint sides and a was/now pair
// (about 3.6 KB), plus the previous telling's ~0.4 KB: 4 KiB dropped entries. 6 KiB holds it.
const STORY_MAP_MAX_BYTES: usize = 6_144;
const STORY_MAP_MAX_PREV_ENDPOINTS: usize = 12;
const STORY_MAP_MAX_SESSIONS: usize = 50;

/// One grounded shift entry in `meta["shift_ledger:<project>"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub sig: String,
    pub kind: String,
    pub claim: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    pub before: String,
    pub after: String,
    pub receipts: Vec<String>,
    pub first_conv: String,
    pub last_conv: String,
    pub rev: usize,
    pub first_at: i64,
    pub last_at: i64,
    #[serde(default)]
    pub liked: bool,
    #[serde(default)]
    pub reflection_id: Option<String>,
}

/// Bounded ring buffer of grounded project shifts stored at `meta["shift_ledger:<project>"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub v: u32,
    pub entries: Vec<LedgerEntry>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            v: LEDGER_VERSION,
            entries: Vec::new(),
        }
    }
}

/// Current and previous story map stored at `meta["story_map:<conv_id>"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryMap {
    pub v: u32,
    pub project: String,
    pub conv_id: String,
    pub current: StoryMapCurrent,
    pub previous: Option<StoryMapPrevious>,
}

/// Current telling in [`StoryMap`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryMapCurrent {
    pub rev: usize,
    pub at: i64,
    pub by: String,
    pub covered_msgs: usize,
    pub entries: Vec<GroundedShift>,
}

/// Summary of the previous telling in [`StoryMap`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoryMapPrevious {
    pub rev: usize,
    pub at: i64,
    pub sigs: Vec<String>,
    pub endpoints: Vec<String>,
}

/// A new `reflections` row prepared by [`plan_ledger`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewLedgerRow {
    pub sig: String,
    pub content: String,
    pub tags: Vec<String>,
    pub entry: LedgerEntry,
}

/// Result of [`plan_ledger`]: updated existing entries plus new reflection rows to insert.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerPlan {
    pub existing: Vec<LedgerEntry>,
    pub new_rows: Vec<NewLedgerRow>,
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum RawPart {
            Endpoint {
                scope: String,
                sym: Option<String>,
                mark: Option<String>,
                tier: String,
            },
            Op {
                op: String,
                tier: String,
            },
        }

        let parse_tier = |t: &str| -> Result<&'static str, D::Error> {
            match t {
                "verified" => Ok("verified"),
                "model" => Ok("model"),
                _ => Err(serde::de::Error::custom("invalid part tier")),
            }
        };

        match RawPart::deserialize(deserializer)? {
            RawPart::Endpoint {
                scope,
                sym,
                mark,
                tier,
            } => Ok(Part::Endpoint {
                scope,
                sym,
                mark,
                tier: parse_tier(&tier)?,
            }),
            RawPart::Op { op, tier } => {
                let static_op = match op.as_str() {
                    "->" => "->",
                    "~>" => "~>",
                    "-x->" => "-x->",
                    _ => return Err(serde::de::Error::custom("invalid edge op")),
                };
                Ok(Part::Op {
                    op: static_op,
                    tier: parse_tier(&tier)?,
                })
            }
        }
    }
}

impl<'de> Deserialize<'de> for GroundedShift {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct RawGroundedShift {
            sig: String,
            kind: String,
            grounded: bool,
            moved: bool,
            liked: bool,
            claim: String,
            #[serde(default)]
            was: Option<String>,
            #[serde(default)]
            now: Option<String>,
            receipts: Vec<String>,
            before: Vec<Part>,
            after: Vec<Part>,
        }
        let raw = RawGroundedShift::deserialize(deserializer)?;
        Ok(GroundedShift {
            sig: raw.sig,
            kind: raw.kind,
            grounded: raw.grounded,
            moved: raw.moved,
            liked: raw.liked,
            claim: raw.claim,
            was: raw.was,
            now: raw.now,
            receipts: raw.receipts,
            before: raw.before,
            after: raw.after,
        })
    }
}

/// Returns true when `tags` marks a Shift Ledger reflection (`narrative_shift`).
pub fn is_ledger_row(tags: &[String]) -> bool {
    tags.iter().any(|t| t == LEDGER_TAG)
}

pub(super) fn ledger_meta_key(project: &str) -> String {
    format!("shift_ledger:{project}")
}

fn story_map_meta_key(conv_id: &str) -> String {
    format!("story_map:{conv_id}")
}

/// Renders a `Part` slice back to canonical side text (`"-"`, `"scope:sym+"`, `"a -> b"`).
pub fn render_parts(parts: &[Part]) -> String {
    if parts.is_empty() {
        return "-".to_string();
    }
    let mut out = String::new();
    for part in parts {
        match part {
            Part::Endpoint {
                scope, sym, mark, ..
            } => {
                out.push_str(scope);
                if let Some(s) = sym {
                    out.push(':');
                    out.push_str(s);
                }
                if let Some(m) = mark {
                    out.push_str(m);
                }
            }
            Part::Op { op, .. } => {
                out.push(' ');
                out.push_str(op);
                out.push(' ');
            }
        }
    }
    out
}

/// The reflection text of one shift: `[header] before => after - <what changed>`. A shift
/// with a `now` sentence says it as `Before: <was> After: <now>` (`New: <now>` when there
/// was no before), the way the pane does; an older one keeps its claim.
fn format_ledger_content(
    by: &str,
    kind: &str,
    rev: usize,
    receipts: &[String],
    before: &str,
    after: &str,
    shift: &GroundedShift,
) -> String {
    let said = match (&shift.was, &shift.now) {
        (Some(was), Some(now)) => format!("Before: {was} After: {now}"),
        (None, Some(now)) => format!("New: {now}"),
        _ => shift.claim.clone(),
    };
    format!(
        "[shift: {}, kind={kind}, rev={rev}, receipts={}] {before} => {after} - {said}",
        if by == RECEIPTS_BY {
            "from receipts"
        } else {
            "model-derived"
        },
        receipts.join(",")
    )
}

fn format_ledger_tags(
    by: &str,
    project: &str,
    conv_id: &str,
    rev: usize,
    kind: &str,
    sig: &str,
) -> Vec<String> {
    let mut tags = vec![
        LEDGER_TAG.to_string(),
        format!("shift_project:{project}"),
        format!("shift_conv:{conv_id}"),
        format!("shift_rev_{rev}"),
        format!("shift_kind_{kind}"),
        format!("shift_sig_{sig}"),
        "shift_grounded".to_string(),
    ];
    if by == RECEIPTS_BY {
        tags.push("shift_by_receipts".to_string());
    }
    tags
}

fn union_receipts(a: &[String], b: &[String]) -> Vec<String> {
    let rank = |r: &str| match r.split(':').next() {
        Some("oid") => 0u8,
        Some("edge") => 1,
        Some("node") => 2,
        _ => 3,
    };
    let mut set: BTreeSet<(u8, String)> = BTreeSet::new();
    for r in a.iter().chain(b.iter()) {
        set.insert((rank(r), r.clone()));
    }
    set.into_iter().map(|(_, r)| r).take(MAX_RECEIPTS).collect()
}

/// Loads `meta["shift_ledger:<project>"]`. Missing, corrupt, or oversized values
/// return an empty [`Ledger`].
pub fn load_ledger(storage: &Storage, project: &str) -> Ledger {
    let key = ledger_meta_key(project);
    let Ok(Some(raw)) = storage.get_meta(&key) else {
        return Ledger::default();
    };
    if raw.len() > LEDGER_MAX_BYTES {
        return Ledger::default();
    }
    let Ok(ledger) = serde_json::from_str::<Ledger>(&raw) else {
        return Ledger::default();
    };
    if ledger.v != LEDGER_VERSION || ledger.entries.len() > LEDGER_MAX_ENTRIES {
        return Ledger::default();
    }
    ledger
}

pub(super) fn cap_and_serialize_ledger(mut ledger: Ledger) -> anyhow::Result<String> {
    ledger.v = LEDGER_VERSION;
    if ledger.entries.len() > LEDGER_MAX_ENTRIES {
        ledger.entries.truncate(LEDGER_MAX_ENTRIES);
    }
    loop {
        let json = serde_json::to_string(&ledger)?;
        if json.len() <= LEDGER_MAX_BYTES || ledger.entries.is_empty() {
            return Ok(json);
        }
        ledger.entries.pop();
    }
}

/// Plans ledger updates and new `reflections` rows for grounded shifts.
pub fn plan_ledger(
    storage: &Storage,
    project: &str,
    conv_id: &str,
    rev: usize,
    now: i64,
    by: &str,
    shifts: &[GroundedShift],
) -> LedgerPlan {
    let ledger = load_ledger(storage, project);
    let mut existing = ledger.entries;
    let mut new_rows: Vec<NewLedgerRow> = Vec::new();

    for shift in shifts {
        if !shift.grounded {
            continue;
        }
        if let Some(entry) = existing.iter_mut().find(|e| e.sig == shift.sig) {
            entry.last_conv = conv_id.to_string();
            entry.rev = rev;
            entry.last_at = now;
            entry.receipts = union_receipts(&entry.receipts, &shift.receipts);
            continue;
        }
        if new_rows.iter().any(|r| r.sig == shift.sig) {
            continue;
        }
        let sig_tag = format!("shift_sig_{}", shift.sig);
        let already_in_reflections = storage
            .get_ledger_reflections_by_tag(&sig_tag, 1)
            .map(|rows| {
                rows.iter()
                    .any(|(_, _, tags, _)| tags.iter().any(|t| t == &sig_tag))
            })
            .unwrap_or(false);
        if already_in_reflections {
            continue;
        }

        let before = render_parts(&shift.before);
        let after = render_parts(&shift.after);
        let receipts = union_receipts(&[], &shift.receipts);
        let content =
            format_ledger_content(by, &shift.kind, rev, &receipts, &before, &after, shift);
        let tags = format_ledger_tags(by, project, conv_id, rev, &shift.kind, &shift.sig);
        let entry = LedgerEntry {
            sig: shift.sig.clone(),
            kind: shift.kind.clone(),
            claim: shift.claim.clone(),
            was: shift.was.clone(),
            now: shift.now.clone(),
            before,
            after,
            receipts,
            first_conv: conv_id.to_string(),
            last_conv: conv_id.to_string(),
            rev,
            first_at: now,
            last_at: now,
            liked: false,
            reflection_id: None,
        };
        new_rows.push(NewLedgerRow {
            sig: shift.sig.clone(),
            content,
            tags,
            entry,
        });
    }

    LedgerPlan { existing, new_rows }
}

/// Commits the planned ledger updates and actually-inserted reflection IDs to `meta["shift_ledger:<project>"]`.
pub fn commit_ledger(
    storage: &Storage,
    project: &str,
    plan: &LedgerPlan,
    inserted: &[(String, String)],
) -> anyhow::Result<()> {
    let mut entries: Vec<LedgerEntry> = Vec::with_capacity(inserted.len() + plan.existing.len());
    for (sig, refl_id) in inserted {
        if let Some(row) = plan.new_rows.iter().find(|r| &r.sig == sig) {
            if !entries.iter().any(|e| &e.sig == sig) {
                let mut entry = row.entry.clone();
                entry.reflection_id = Some(refl_id.clone());
                entries.push(entry);
            }
        }
    }
    for entry in &plan.existing {
        if !entries.iter().any(|e| e.sig == entry.sig) {
            entries.push(entry.clone());
        }
    }
    // Newest-first by last_at (stable sort preserves order within the same timestamp).
    entries.sort_by_key(|e| std::cmp::Reverse(e.last_at));

    let json = cap_and_serialize_ledger(Ledger {
        v: LEDGER_VERSION,
        entries,
    })?;
    storage.set_meta(&ledger_meta_key(project), &json)
}

fn collect_previous_endpoints(entries: &[GroundedShift]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries {
        for part in entry.before.iter().chain(entry.after.iter()) {
            if let Part::Endpoint { scope, sym, .. } = part {
                let label = match sym {
                    Some(s) => format!("{scope}:{s}"),
                    None => scope.clone(),
                };
                if !out.contains(&label) {
                    out.push(label);
                    if out.len() >= STORY_MAP_MAX_PREV_ENDPOINTS {
                        return out;
                    }
                }
            }
        }
    }
    out
}

/// Builds a [`StoryMap`] for the current telling, marking `moved` and `liked` on each entry.
#[allow(clippy::too_many_arguments)]
pub fn build_story_map(
    prev: Option<&StoryMap>,
    project: &str,
    conv_id: &str,
    rev: usize,
    at: i64,
    by: &str,
    covered: usize,
    liked: &HashSet<String>,
    mut shifts: Vec<GroundedShift>,
) -> StoryMap {
    let prev_sigs: HashSet<&str> = prev
        .map(|p| p.current.entries.iter().map(|e| e.sig.as_str()).collect())
        .unwrap_or_default();

    for entry in &mut shifts {
        entry.moved = prev.is_none() || !prev_sigs.contains(entry.sig.as_str());
        entry.liked = entry.grounded && liked.contains(&entry.sig);
    }

    let previous = prev.map(|p| StoryMapPrevious {
        rev: p.current.rev,
        at: p.current.at,
        sigs: p.current.entries.iter().map(|e| e.sig.clone()).collect(),
        endpoints: collect_previous_endpoints(&p.current.entries),
    });

    StoryMap {
        v: STORY_MAP_VERSION,
        project: project.to_string(),
        conv_id: conv_id.to_string(),
        current: StoryMapCurrent {
            rev,
            at,
            by: by.to_string(),
            covered_msgs: covered,
            entries: shifts,
        },
        previous,
    }
}

fn cap_and_serialize_story_map(map: &StoryMap) -> anyhow::Result<String> {
    let mut capped = map.clone();
    capped.v = STORY_MAP_VERSION;

    loop {
        let json = serde_json::to_string(&capped)?;
        if json.len() <= STORY_MAP_MAX_BYTES {
            return Ok(json);
        }
        if let Some(pos) = capped.current.entries.iter().rposition(|e| !e.grounded) {
            capped.current.entries.remove(pos);
            continue;
        }
        if !capped.current.entries.is_empty() {
            capped.current.entries.pop();
            continue;
        }
        if let Some(ref mut prev) = capped.previous {
            if !prev.endpoints.is_empty() {
                prev.endpoints.pop();
                continue;
            }
            if !prev.sigs.is_empty() {
                prev.sigs.pop();
                continue;
            }
        }
        if capped.previous.is_some() {
            capped.previous = None;
            continue;
        }
        return Ok(json);
    }
}

/// Saves `map` to `meta["story_map:<conv_id>"]` under the 4096-byte cap, then
/// prunes older `story_map:%` keys so at most 50 remain.
pub fn save_story_map(storage: &Storage, map: &StoryMap) -> anyhow::Result<()> {
    let json = cap_and_serialize_story_map(map)?;
    storage.set_meta(&story_map_meta_key(&map.conv_id), &json)?;
    let _ = prune_story_maps(storage);
    Ok(())
}

fn prune_story_maps(storage: &Storage) -> anyhow::Result<()> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare("SELECT key, value FROM meta WHERE key LIKE 'story_map:%'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut items: Vec<(i64, String)> = Vec::new();
        for row in rows.flatten() {
            let (key, val) = row;
            let at = serde_json::from_str::<serde_json::Value>(&val)
                .ok()
                .and_then(|v| v.get("current")?.get("at")?.as_i64())
                .unwrap_or(i64::MIN);
            items.push((at, key));
        }
        if items.len() <= STORY_MAP_MAX_SESSIONS {
            return Ok(());
        }
        items.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let to_delete = items.len() - STORY_MAP_MAX_SESSIONS;
        for (_, key) in items.into_iter().take(to_delete) {
            let _ = conn.execute("DELETE FROM meta WHERE key = ?1", params![key]);
        }
        Ok(())
    })
}

/// Loads `meta["story_map:<conv_id>"]`. Missing, corrupt, or oversized values return `None`.
pub fn load_story_map(storage: &Storage, conv_id: &str) -> Option<StoryMap> {
    let raw = storage.get_meta(&story_map_meta_key(conv_id)).ok()??;
    if raw.len() > STORY_MAP_MAX_BYTES {
        return None;
    }
    let map = serde_json::from_str::<StoryMap>(&raw).ok()?;
    if map.v != STORY_MAP_VERSION {
        return None;
    }
    Some(map)
}

/// Extracts the UUID from `store_reflection_internal`'s status string (`ID: <uuid>`).
pub(crate) fn reflection_id_from(stored: &str) -> Option<String> {
    for line in stored.lines() {
        if let Some(rest) = line.trim().strip_prefix("ID:") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// Grounds `shifts`, appends new grounded rows to `reflections` and `meta["shift_ledger:<project>"]`,
/// and writes `meta["story_map:<conv_id>"]`. Swallows and logs any error.
pub async fn record_story_map(
    engine: &crate::engine::Engine,
    project: &str,
    conv_id: &str,
    rev: usize,
    covered: usize,
    by: &str,
    shifts: Vec<ShiftEntry>,
) {
    if story_map_disabled() {
        return;
    }
    super::likes::sync_likes(
        engine.storage(),
        project,
        &super::likes::likes_path(engine.data_dir()),
    );
    if shifts.is_empty() {
        return;
    }
    let _op = match engine.storage().operation_lock() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("CSR: story map: operation lock: {e}");
            return;
        }
    };

    let grounded = ground_shifts(engine.storage(), project, conv_id, &shifts);
    if grounded.is_empty() {
        return;
    }

    let now = chrono::Utc::now().timestamp();
    let plan = plan_ledger(engine.storage(), project, conv_id, rev, now, by, &grounded);

    let mut inserted: Vec<(String, String)> = Vec::with_capacity(plan.new_rows.len());
    for row in &plan.new_rows {
        match crate::mcp::tools::store_reflection_internal(
            engine.storage(),
            engine.embeddings(),
            engine.search(),
            &row.content,
            &row.tags,
        )
        .await
        {
            Ok(out) => match reflection_id_from(&out) {
                Some(id) => inserted.push((row.sig.clone(), id)),
                None => eprintln!("CSR: story map: missing ID in stored reflection output"),
            },
            Err(e) => {
                eprintln!("CSR: story map: store reflection {}: {e}", row.sig);
            }
        }
    }

    if let Err(e) = commit_ledger(engine.storage(), project, &plan, &inserted) {
        eprintln!("CSR: story map: commit ledger: {e}");
    }

    let ledger = load_ledger(engine.storage(), project);
    let liked: HashSet<String> = ledger
        .entries
        .iter()
        .filter(|e| e.liked)
        .map(|e| e.sig.clone())
        .collect();
    let prev = load_story_map(engine.storage(), conv_id);
    let map = build_story_map(
        prev.as_ref(),
        project,
        conv_id,
        rev,
        now,
        by,
        covered,
        &liked,
        grounded,
    );
    if let Err(e) = save_story_map(engine.storage(), &map) {
        eprintln!("CSR: story map: save story map: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::story_map::{parse_shift_line, sig};
    use serde_json::json;

    fn sample_grounded(
        project: &str,
        line: &str,
        grounded: bool,
        receipts: &[&str],
    ) -> GroundedShift {
        let entry = parse_shift_line(line).expect("valid line");
        let tier = if grounded { "verified" } else { "model" };
        let to_parts = |side: &Option<crate::story_map::Side>| -> Vec<Part> {
            let Some(s) = side else {
                return Vec::new();
            };
            let mut v = vec![Part::Endpoint {
                scope: s.from.scope.clone(),
                sym: s.from.sym.clone(),
                mark: s.from.mark.map(|m| m.to_string()),
                tier,
            }];
            if let Some((op, to)) = &s.edge {
                v.push(Part::Op {
                    op: op.as_str(),
                    tier,
                });
                v.push(Part::Endpoint {
                    scope: to.scope.clone(),
                    sym: to.sym.clone(),
                    mark: to.mark.map(|m| m.to_string()),
                    tier,
                });
            }
            v
        };
        GroundedShift {
            sig: sig(project, &entry),
            kind: entry.kind.as_str().to_string(),
            grounded,
            moved: false,
            liked: false,
            claim: entry.claim.clone(),
            was: entry.was.clone(),
            now: entry.now.clone(),
            receipts: receipts.iter().map(|s| s.to_string()).collect(),
            before: to_parts(&entry.before),
            after: to_parts(&entry.after),
        }
    }

    #[test]
    fn ledger_roundtrip_newest_first_and_capped_at_200() {
        let storage = Storage::open_memory().unwrap();
        let mut shifts = Vec::new();
        for i in 0..205usize {
            let line = format!("shift|a.rs:f{i}~|b.rs:g{i}+|Claim {i}");
            shifts.push(sample_grounded("csr", &line, true, &["evo:e1"]));
        }

        let plan1 = plan_ledger(&storage, "csr", "c1", 1, 1000, "Haiku", &shifts[..100]);
        let ins1: Vec<(String, String)> = plan1
            .new_rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.sig.clone(), format!("r1_{i}")))
            .collect();
        commit_ledger(&storage, "csr", &plan1, &ins1).unwrap();

        let plan2 = plan_ledger(&storage, "csr", "c2", 2, 2000, "Haiku", &shifts[100..]);
        let ins2: Vec<(String, String)> = plan2
            .new_rows
            .iter()
            .enumerate()
            .map(|(i, r)| (r.sig.clone(), format!("r2_{i}")))
            .collect();
        commit_ledger(&storage, "csr", &plan2, &ins2).unwrap();

        let ledger = load_ledger(&storage, "csr");
        assert_eq!(ledger.v, 1);
        // Both caps apply: 200 entries and 32 KB. A real entry is ~230+ bytes, so the byte cap
        // binds first; the oldest go, the newest stay first.
        assert!(ledger.entries.len() < 205);
        assert!(ledger.entries.len() <= 200);
        assert!(serde_json::to_string(&ledger).unwrap().len() <= 32_768);
        assert_eq!(ledger.entries[0].last_at, 2000);
        assert_eq!(ledger.entries[104].last_at, 2000);
        assert_eq!(ledger.entries[105].last_at, 1000);
        assert!(ledger.entries.len() > 105);
    }

    #[test]
    fn ledger_over_32kb_drops_the_oldest() {
        let storage = Storage::open_memory().unwrap();
        let mut entries = Vec::new();
        for i in 0..180usize {
            entries.push(LedgerEntry {
                sig: format!("{i:016x}"),
                kind: "shift".into(),
                claim: "X".repeat(120),
                was: None,
                now: None,
                before: format!("src/module_{i}/very_long_file_name_{i}.rs:func_before_{i}~"),
                after: format!("src/module_{i}/very_long_file_name_{i}.rs:func_after_{i}+"),
                receipts: vec![
                    "evo:11111111".into(),
                    "node:22222222".into(),
                    "oid:3333333".into(),
                ],
                first_conv: "conv-long-identifier-0001".into(),
                last_conv: "conv-long-identifier-0002".into(),
                rev: i + 1,
                first_at: (2000 - i as i64),
                last_at: (2000 - i as i64),
                liked: false,
                reflection_id: Some(format!("uuid-reflection-{i:04}")),
            });
        }
        let plan = LedgerPlan {
            existing: entries,
            new_rows: Vec::new(),
        };
        commit_ledger(&storage, "csr", &plan, &[]).unwrap();

        let raw = storage.get_meta("shift_ledger:csr").unwrap().unwrap();
        assert!(raw.len() <= 32_768);
        let loaded = load_ledger(&storage, "csr");
        assert!(!loaded.entries.is_empty());
        assert!(loaded.entries.len() < 180);
        assert_eq!(loaded.entries[0].sig, format!("{:016x}", 0));
    }

    #[test]
    fn corrupt_ledger_meta_reads_as_empty() {
        let storage = Storage::open_memory().unwrap();
        assert_eq!(load_ledger(&storage, "csr"), Ledger::default());

        storage.set_meta("shift_ledger:csr", "{not json").unwrap();
        assert_eq!(load_ledger(&storage, "csr"), Ledger::default());

        storage
            .set_meta("shift_ledger:csr", r#"{"v":99,"entries":[]}"#)
            .unwrap();
        assert_eq!(load_ledger(&storage, "csr"), Ledger::default());

        let huge = "x".repeat(40_000);
        storage.set_meta("shift_ledger:csr", &huge).unwrap();
        assert_eq!(load_ledger(&storage, "csr"), Ledger::default());
    }

    #[test]
    fn plan_adds_only_grounded_entries() {
        let storage = Storage::open_memory().unwrap();
        let g = sample_grounded(
            "csr",
            "shift|a.rs:foo~|a.rs:bar+|Grounded claim",
            true,
            &["evo:e1"],
        );
        let m = sample_grounded(
            "csr",
            "risk|b.rs:baz|c.rs:qux!|Model only claim",
            false,
            &[],
        );
        let plan = plan_ledger(&storage, "csr", "c1", 1, 1000, "Haiku", &[g.clone(), m]);
        assert_eq!(plan.new_rows.len(), 1);
        assert_eq!(plan.new_rows[0].sig, g.sig);
    }

    #[test]
    fn plan_dedupes_a_known_sig_and_unions_receipts() {
        let storage = Storage::open_memory().unwrap();
        let s1 = sample_grounded(
            "csr",
            "shift|a.rs:foo~|a.rs:bar+|Keep prior shift rows",
            true,
            &["evo:e1", "node:n1", "evo:e2", "evo:e3"],
        );
        let plan1 = plan_ledger(
            &storage,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            std::slice::from_ref(&s1),
        );
        assert_eq!(plan1.new_rows.len(), 1);
        commit_ledger(
            &storage,
            "csr",
            &plan1,
            &[(s1.sig.clone(), "refl-1".to_string())],
        )
        .unwrap();

        let s2 = sample_grounded(
            "csr",
            "shift|a.rs:foo~|a.rs:bar+|Updated wording same sig",
            true,
            &["oid:abc1234", "edge:1111_2222", "node:n2", "evo:e4"],
        );
        assert_eq!(s1.sig, s2.sig);

        let plan2 = plan_ledger(&storage, "csr", "c2", 3, 2500, "Haiku", &[s2]);
        assert!(plan2.new_rows.is_empty());
        assert_eq!(plan2.existing.len(), 1);
        let updated = &plan2.existing[0];
        assert_eq!(updated.first_conv, "c1");
        assert_eq!(updated.last_conv, "c2");
        assert_eq!(updated.rev, 3);
        assert_eq!(updated.first_at, 1000);
        assert_eq!(updated.last_at, 2500);
        assert_eq!(updated.receipts.len(), 6);
        assert_eq!(updated.receipts[0], "oid:abc1234");
        assert_eq!(updated.receipts[1], "edge:1111_2222");
    }

    #[test]
    fn plan_skips_a_sig_whose_reflection_exists_even_if_the_ledger_forgot_it() {
        let storage = Storage::open_memory().unwrap();
        let s = sample_grounded(
            "csr",
            "decision|-|ground.rs:ground_shifts+|Verify against SQLite",
            true,
            &["evo:e1"],
        );
        let tags = vec![
            "narrative_shift".to_string(),
            format!("shift_sig_{}", s.sig),
        ];
        storage
            .insert_reflection(
                "existing-uuid",
                "prior shift content",
                &tags,
                &[0.0f32; 384],
            )
            .unwrap();

        let plan = plan_ledger(&storage, "csr", "c1", 1, 1000, "Haiku", &[s]);
        assert!(plan.new_rows.is_empty());
        assert!(plan.existing.is_empty());
    }

    #[test]
    fn commit_omits_rows_whose_insert_failed() {
        let storage = Storage::open_memory().unwrap();
        let s1 = sample_grounded("csr", "shift|a.rs:f1~|a.rs:f2+|One", true, &["evo:1"]);
        let s2 = sample_grounded("csr", "shift|b.rs:g1~|b.rs:g2+|Two", true, &["evo:2"]);
        let plan = plan_ledger(
            &storage,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            &[s1.clone(), s2.clone()],
        );
        assert_eq!(plan.new_rows.len(), 2);

        // Only s1 succeeded; s2 failed to insert.
        commit_ledger(
            &storage,
            "csr",
            &plan,
            &[(s1.sig.clone(), "refl-ok".to_string())],
        )
        .unwrap();

        let ledger = load_ledger(&storage, "csr");
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].sig, s1.sig);
        assert_eq!(ledger.entries[0].reflection_id.as_deref(), Some("refl-ok"));

        // Next run still plans s2 because it was not committed.
        let plan_retry = plan_ledger(&storage, "csr", "c1", 2, 1100, "Haiku", &[s1, s2.clone()]);
        assert_eq!(plan_retry.new_rows.len(), 1);
        assert_eq!(plan_retry.new_rows[0].sig, s2.sig);
    }

    #[test]
    fn second_run_of_the_same_shifts_adds_no_rows() {
        let storage = Storage::open_memory().unwrap();
        let s = sample_grounded("csr", "shift|a.rs:f1~|a.rs:f2+|One", true, &["evo:1"]);
        let plan1 = plan_ledger(
            &storage,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            std::slice::from_ref(&s),
        );
        assert_eq!(plan1.new_rows.len(), 1);
        commit_ledger(
            &storage,
            "csr",
            &plan1,
            &[(s.sig.clone(), "refl-1".to_string())],
        )
        .unwrap();

        let plan2 = plan_ledger(&storage, "csr", "c1", 2, 2000, "Haiku", &[s]);
        assert!(plan2.new_rows.is_empty());
        commit_ledger(&storage, "csr", &plan2, &[]).unwrap();
        assert_eq!(load_ledger(&storage, "csr").entries.len(), 1);
    }

    #[test]
    fn rows_use_the_shift_tags_and_never_project_or_conv_tags() {
        let storage = Storage::open_memory().unwrap();
        let s = sample_grounded(
            "csr",
            "shift|a.rs:f1~|a.rs:f2+|Tag isolation check",
            true,
            &["evo:1"],
        );
        let plan = plan_ledger(
            &storage,
            "csr",
            "conv-42",
            4,
            1000,
            "Haiku",
            std::slice::from_ref(&s),
        );
        let tags = &plan.new_rows[0].tags;
        assert!(tags.contains(&"narrative_shift".to_string()));
        assert!(tags.contains(&"shift_project:csr".to_string()));
        assert!(tags.contains(&"shift_conv:conv-42".to_string()));
        assert!(tags.contains(&"shift_rev_4".to_string()));
        assert!(tags.contains(&"shift_kind_shift".to_string()));
        assert!(tags.contains(&format!("shift_sig_{}", s.sig)));
        assert!(tags.contains(&"shift_grounded".to_string()));
        assert!(!tags.iter().any(|t| t.starts_with("project_")));
        assert!(!tags.iter().any(|t| t.starts_with("conv_")));
    }

    #[test]
    fn receipts_only_rows_say_from_receipts_and_carry_the_by_tag() {
        let storage = Storage::open_memory().unwrap();
        let s = sample_grounded("csr", "shift|-|a.rs:f~|Changed f", true, &["evo:9f12a0c1"]);
        let plan = plan_ledger(
            &storage,
            "csr",
            "c1",
            2,
            1000,
            RECEIPTS_BY,
            std::slice::from_ref(&s),
        );
        let row = &plan.new_rows[0];
        assert!(
            row.content
                .starts_with("[shift: from receipts, kind=shift, rev=2, receipts=evo:9f12a0c1] "),
            "{}",
            row.content
        );
        assert!(!row.content.contains("model-derived"));
        assert!(row.tags.contains(&"shift_by_receipts".to_string()));
        let haiku = plan_ledger(
            &storage,
            "csr",
            "c1",
            2,
            1000,
            "Haiku",
            std::slice::from_ref(&s),
        );
        assert!(haiku.new_rows[0]
            .content
            .starts_with("[shift: model-derived, "));
        assert!(!haiku.new_rows[0]
            .tags
            .contains(&"shift_by_receipts".to_string()));
    }

    #[test]
    fn row_content_has_the_model_derived_label_and_receipts() {
        let storage = Storage::open_memory().unwrap();
        let s = sample_grounded(
            "csr",
            "shift|summarizer.rs:store_story~ -> reflections-|summarizer.rs:store_story~ -> reflections:narrative_shift+|Keep prior shift reflections",
            true,
            &["evo:9f12a0c1", "node:44b18e20"],
        );
        let plan = plan_ledger(&storage, "csr", "c1", 4, 1000, "Haiku", &[s]);
        assert_eq!(
            plan.new_rows[0].content,
            "[shift: model-derived, kind=shift, rev=4, receipts=node:44b18e20,evo:9f12a0c1] summarizer.rs:store_story~ -> reflections- => summarizer.rs:store_story~ -> reflections:narrative_shift+ - Keep prior shift reflections"
        );
    }

    #[test]
    fn story_map_marks_new_entries_moved_and_keeps_previous_sigs() {
        let s1 = sample_grounded("csr", "shift|a.rs:f1~|b.rs:f2+|First", true, &["evo:1"]);
        let s2 = sample_grounded("csr", "risk|c.rs:f3|d.rs:f4!|Second", true, &["evo:2"]);
        let mut liked = HashSet::new();
        liked.insert(s1.sig.clone());

        let map1 = build_story_map(
            None,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            20,
            &liked,
            vec![s1.clone()],
        );
        let map2 = build_story_map(
            Some(&map1),
            "csr",
            "c1",
            2,
            2000,
            "Haiku",
            40,
            &liked,
            vec![s1.clone(), s2.clone()],
        );

        assert!(!map2.current.entries[0].moved);
        assert!(map2.current.entries[0].liked);
        assert!(map2.current.entries[1].moved);
        assert!(!map2.current.entries[1].liked);

        let prev = map2.previous.unwrap();
        assert_eq!(prev.rev, 1);
        assert_eq!(prev.at, 1000);
        assert_eq!(prev.sigs, vec![s1.sig]);
        assert_eq!(prev.endpoints, vec!["a.rs:f1", "b.rs:f2"]);
    }

    #[test]
    fn first_telling_marks_everything_moved() {
        let s1 = sample_grounded("csr", "shift|a.rs:f1~|b.rs:f2+|First", true, &["evo:1"]);
        let s2 = sample_grounded("csr", "risk|c.rs:f3|d.rs:f4!|Second", false, &[]);
        let map = build_story_map(
            None,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            15,
            &HashSet::new(),
            vec![s1, s2],
        );
        assert!(map.previous.is_none());
        assert!(map.current.entries.iter().all(|e| e.moved));
    }

    #[test]
    fn story_map_over_the_cap_drops_model_only_entries_first() {
        let storage = Storage::open_memory().unwrap();
        let mut entries = Vec::new();
        for i in 0..16usize {
            let line = format!(
                "shift|src/very_long_path_{i}/mod_{i}.rs:func_before_{i}~ -> src/other_{i}/peer_{i}.rs:callee_{i}~|src/very_long_path_{i}/mod_{i}.rs:func_after_{i}+ -> src/other_{i}/peer_{i}.rs:callee_{i}+|{} {i}",
                "Detailed architectural shift claim text ".repeat(2)
            );
            entries.push(sample_grounded(
                "csr",
                &line,
                i % 2 == 0,
                if i % 2 == 0 {
                    &["evo:12345678", "node:87654321"]
                } else {
                    &[]
                },
            ));
        }
        let map = build_story_map(
            None,
            "csr",
            "c-big",
            1,
            1000,
            "Haiku",
            50,
            &HashSet::new(),
            entries,
        );
        save_story_map(&storage, &map).unwrap();

        let raw = storage.get_meta("story_map:c-big").unwrap().unwrap();
        assert!(raw.len() <= STORY_MAP_MAX_BYTES);
        let loaded = load_story_map(&storage, "c-big").unwrap();
        assert!(!loaded.current.entries.is_empty());
        // Trimmed, and model-only entries went before any grounded one did.
        assert!(loaded.current.entries.len() < 16);
        let grounded = loaded.current.entries.iter().filter(|e| e.grounded).count();
        assert!(
            grounded == loaded.current.entries.len() || grounded == 8,
            "a model-only entry stayed while a grounded one went: {grounded} grounded of {}",
            loaded.current.entries.len()
        );
    }

    #[test]
    fn was_and_now_reach_the_reflection_text_and_the_stored_map() {
        let storage = Storage::open_memory().unwrap();
        let both = sample_grounded(
            "csr",
            "shift|pane.py:draw_story_map-|pane.py:shift_map_rows~|The map drew its labels at 9 to 10 px.|The map writes Before and After as text.",
            true,
            &["evo:1"],
        );
        let fresh = sample_grounded(
            "csr",
            "decision|-|summarizer.rs:ground_shifts+|-|ground_shifts checks each shift against AST rows.",
            true,
            &["evo:2"],
        );
        let old = sample_grounded(
            "csr",
            "risk|a.rs:f~|a.rs:g!|Old four-field claim",
            true,
            &["evo:3"],
        );
        let plan = plan_ledger(
            &storage,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            &[both.clone(), fresh.clone(), old.clone()],
        );
        let said: Vec<&str> = plan.new_rows.iter().map(|r| r.content.as_str()).collect();
        assert!(
            said[0].ends_with(
                "- Before: The map drew its labels at 9 to 10 px. After: The map writes Before and After as text."
            ),
            "{}",
            said[0]
        );
        assert!(
            said[1].ends_with("- New: ground_shifts checks each shift against AST rows."),
            "{}",
            said[1]
        );
        assert!(said[2].ends_with("- Old four-field claim"), "{}", said[2]);
        assert_eq!(plan.new_rows[0].entry.was, both.was);
        assert_eq!(plan.new_rows[1].entry.was, None);
        assert_eq!(plan.new_rows[1].entry.now, fresh.now);

        let map = build_story_map(
            None,
            "csr",
            "c1",
            1,
            1000,
            "Haiku",
            10,
            &HashSet::new(),
            vec![both, fresh, old],
        );
        save_story_map(&storage, &map).unwrap();
        let raw = storage.get_meta("story_map:c1").unwrap().unwrap();
        let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let entries = json["current"]["entries"].as_array().unwrap();
        assert_eq!(entries[0]["was"], "The map drew its labels at 9 to 10 px.");
        assert_eq!(
            entries[0]["now"],
            "The map writes Before and After as text."
        );
        assert!(entries[1].get("was").is_none(), "a new thing has no was");
        assert!(entries[1]["now"].is_string());
        assert!(entries[2].get("was").is_none() && entries[2].get("now").is_none());
        assert_eq!(load_story_map(&storage, "c1").unwrap(), map);
    }

    #[test]
    fn a_map_stored_before_was_and_now_still_loads() {
        let storage = Storage::open_memory().unwrap();
        let old = json!({
            "v": 1, "project": "csr", "conv_id": "c-old",
            "current": {"rev": 2, "at": 1000, "by": "Haiku", "covered_msgs": 10, "entries": [{
                "sig": "0123456789abcdef", "kind": "shift", "grounded": true, "moved": false, "liked": false,
                "claim": "Keep prior shift reflections", "receipts": ["evo:1"],
                "before": [{"scope": "a.rs", "sym": "f", "mark": "~", "tier": "verified"}],
                "after": [{"scope": "a.rs", "sym": "g", "mark": "+", "tier": "verified"}]
            }]},
            "previous": null
        });
        storage
            .set_meta("story_map:c-old", &old.to_string())
            .unwrap();
        let loaded = load_story_map(&storage, "c-old").expect("an old map loads");
        assert_eq!(
            loaded.current.entries[0].claim,
            "Keep prior shift reflections"
        );
        assert_eq!(
            (
                loaded.current.entries[0].was.clone(),
                loaded.current.entries[0].now.clone()
            ),
            (None, None)
        );
        let again = serde_json::to_string(&loaded).unwrap();
        assert!(!again.contains("\"was\"") && !again.contains("\"now\""));
    }

    #[test]
    fn four_full_five_field_entries_fit_the_story_map_cap() {
        let storage = Storage::open_memory().unwrap();
        let sentence = "The map writes Before and After as plain text rows.";
        let mut entries = Vec::new();
        for i in 0..4usize {
            let line = format!(
                "shift|src/very_long_path_{i}/mod_{i}.rs:func_before_{i}~ -> src/other_{i}/peer_{i}.rs:callee_{i}~|src/very_long_path_{i}/mod_{i}.rs:func_after_{i}+ -> src/other_{i}/peer_{i}.rs:callee_{i}+|{sentence}|{sentence}"
            );
            entries.push(sample_grounded(
                "csr",
                &line,
                true,
                &[
                    "oid:abcdef1234567",
                    "edge:12345678",
                    "node:87654321",
                    "evo:11111111",
                ],
            ));
        }
        let prev = build_story_map(
            None,
            "csr",
            "c-full",
            1,
            900,
            "Haiku",
            40,
            &HashSet::new(),
            entries.clone(),
        );
        let map = build_story_map(
            Some(&prev),
            "csr",
            "c-full",
            2,
            1000,
            "Haiku",
            50,
            &HashSet::new(),
            entries,
        );
        save_story_map(&storage, &map).unwrap();
        let raw = storage.get_meta("story_map:c-full").unwrap().unwrap();
        assert!(raw.len() <= STORY_MAP_MAX_BYTES, "{} bytes", raw.len());
        let loaded = load_story_map(&storage, "c-full").unwrap();
        assert_eq!(
            loaded.current.entries.len(),
            4,
            "no entry dropped at {} bytes",
            raw.len()
        );
        assert_eq!(loaded.current.entries[0].now.as_deref(), Some(sentence));
        assert_eq!(loaded.current.entries[0].was.as_deref(), Some(sentence));
    }

    #[test]
    fn story_map_prune_keeps_the_newest_50() {
        let storage = Storage::open_memory().unwrap();
        let s = sample_grounded("csr", "shift|a.rs:f1~|b.rs:f2+|One", true, &["evo:1"]);
        for i in 0..55usize {
            let conv = format!("conv-{i:03}");
            let map = build_story_map(
                None,
                "csr",
                &conv,
                1,
                1000 + i as i64,
                "Haiku",
                10,
                &HashSet::new(),
                vec![s.clone()],
            );
            save_story_map(&storage, &map).unwrap();
        }
        for i in 0..5usize {
            let conv = format!("conv-{i:03}");
            assert!(load_story_map(&storage, &conv).is_none());
        }
        for i in 5..55usize {
            let conv = format!("conv-{i:03}");
            assert!(load_story_map(&storage, &conv).is_some());
        }
    }

    #[test]
    fn save_then_load_story_map_roundtrip_matches_the_pane_contract() {
        let storage = Storage::open_memory().unwrap();
        let s1 = sample_grounded(
            "csr",
            "shift|summarizer.rs:store_story~ -> reflections-|summarizer.rs:store_story~ -> reflections:narrative_shift+|Keep prior shift reflections on story rewrite",
            true,
            &["evo:9f12a0c1", "node:44b18e20"],
        );
        let sig1 = s1.sig.clone();

        let prev_shift = sample_grounded(
            "csr",
            "decision|-|summarizer.rs:store_story~|Prior telling",
            true,
            &["evo:00000001"],
        );
        let prev_sig = prev_shift.sig.clone();
        let prev_map = build_story_map(
            None,
            "csr",
            "65c858ac",
            3,
            1760398200,
            "Haiku",
            30,
            &HashSet::new(),
            vec![prev_shift],
        );

        let map = build_story_map(
            Some(&prev_map),
            "csr",
            "65c858ac",
            4,
            1760400100,
            "Haiku",
            48,
            &HashSet::new(),
            vec![s1],
        );
        save_story_map(&storage, &map).unwrap();

        let loaded = load_story_map(&storage, "65c858ac").unwrap();
        assert_eq!(loaded, map);

        let raw = storage.get_meta("story_map:65c858ac").unwrap().unwrap();
        let val: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            val,
            json!({
                "v": 1,
                "project": "csr",
                "conv_id": "65c858ac",
                "current": {
                    "rev": 4,
                    "at": 1760400100,
                    "by": "Haiku",
                    "covered_msgs": 48,
                    "entries": [
                        {
                            "sig": sig1,
                            "kind": "shift",
                            "grounded": true,
                            "moved": true,
                            "liked": false,
                            "claim": "Keep prior shift reflections on story rewrite",
                            "receipts": ["evo:9f12a0c1", "node:44b18e20"],
                            "before": [
                                {"scope": "summarizer.rs", "sym": "store_story", "mark": "~", "tier": "verified"},
                                {"op": "->", "tier": "verified"},
                                {"scope": "reflections", "sym": null, "mark": "-", "tier": "verified"}
                            ],
                            "after": [
                                {"scope": "summarizer.rs", "sym": "store_story", "mark": "~", "tier": "verified"},
                                {"op": "->", "tier": "verified"},
                                {"scope": "reflections", "sym": "narrative_shift", "mark": "+", "tier": "verified"}
                            ]
                        }
                    ]
                },
                "previous": {
                    "rev": 3,
                    "at": 1760398200,
                    "sigs": [prev_sig],
                    "endpoints": ["summarizer.rs:store_story"]
                }
            })
        );
    }

    #[test]
    fn corrupt_story_map_meta_loads_as_none() {
        let storage = Storage::open_memory().unwrap();
        assert!(load_story_map(&storage, "c1").is_none());

        storage.set_meta("story_map:c1", "{bad").unwrap();
        assert!(load_story_map(&storage, "c1").is_none());

        storage
            .set_meta(
                "story_map:c1",
                r#"{"v":2,"project":"p","conv_id":"c1","current":{"rev":1,"at":1,"by":"Haiku","covered_msgs":1,"entries":[]},"previous":null}"#,
            )
            .unwrap();
        assert!(load_story_map(&storage, "c1").is_none());

        storage
            .set_meta("story_map:c1", &"x".repeat(5_000))
            .unwrap();
        assert!(load_story_map(&storage, "c1").is_none());
    }

    #[test]
    fn reflection_id_is_parsed_from_the_stored_text() {
        let output = "Reflection stored successfully!\n\nID: 550e8400-e29b-41d4-a716-446655440000\nContent: hello\nTags: [\"narrative_shift\"]";
        assert_eq!(
            reflection_id_from(output).as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert!(reflection_id_from("No identifier line here").is_none());
    }

    #[test]
    fn is_ledger_row_matches_only_narrative_shift() {
        assert!(is_ledger_row(&[
            "narrative_shift".to_string(),
            "shift_project:csr".to_string()
        ]));
        assert!(!is_ledger_row(&[
            "session_story".to_string(),
            "conv_123".to_string()
        ]));
        assert!(!is_ledger_row(&[]));
    }

    fn test_engine() -> (crate::engine::Engine, std::sync::Arc<Storage>) {
        let storage = std::sync::Arc::new(Storage::open_memory().unwrap());
        let embeddings = std::sync::Arc::new(crate::embeddings::EmbeddingEngine::new().unwrap());
        let search = std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::search::SearchEngine::new(100),
        ));
        let engine = crate::engine::Engine::from_parts(
            storage.clone(),
            embeddings,
            search,
            std::path::PathBuf::from("/tmp"),
        );
        (engine, storage)
    }

    fn seed_evo_row(storage: &Storage, conv_id: &str, file_path: &str, sym: &str) {
        storage
            .with_connection(|c| {
                c.execute(
                    "INSERT INTO code_evolution (
                        id, session_id, project_name, file_path, language, timestamp,
                        functions_added, functions_removed, types_added, types_removed
                    ) VALUES ('evo_9f12a0c1_1', ?1, 'csr', ?2, 'rust', '2026-10-14T00:00:01Z', ?3, '[]', '[]', '[]')",
                    params![conv_id, file_path, format!("[\"{sym}\"]")],
                )?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn record_story_map_writes_rows_ledger_and_map() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");

        let (engine, storage) = test_engine();
        seed_evo_row(
            &storage,
            "conv-e2e",
            "/repo/src/summarizer.rs",
            "ground_shifts",
        );
        let entry = parse_shift_line(
            "decision|-|summarizer.rs:ground_shifts+|Check shift endpoints against AST rows",
        )
        .unwrap();
        let expected_sig = sig("csr", &entry);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(record_story_map(
            &engine,
            "csr",
            "conv-e2e",
            1,
            24,
            "Haiku",
            vec![entry],
        ));

        let rows = storage
            .get_ledger_reflections_by_tag("narrative_shift", 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        let (refl_id, _, _, _) = &rows[0];

        let ledger = load_ledger(&storage, "csr");
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].sig, expected_sig);
        assert_eq!(
            ledger.entries[0].reflection_id.as_deref(),
            Some(refl_id.as_str())
        );

        let map = load_story_map(&storage, "conv-e2e").expect("story_map stored");
        assert_eq!(map.current.by, "Haiku");
        assert_eq!(map.current.entries.len(), 1);
        assert_eq!(map.current.entries[0].sig, expected_sig);
        assert!(map.current.entries[0].grounded);
    }

    #[test]
    fn record_story_map_twice_adds_no_second_row() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");

        let (engine, storage) = test_engine();
        seed_evo_row(
            &storage,
            "conv-dup",
            "/repo/src/summarizer.rs",
            "ground_shifts",
        );
        let entry = parse_shift_line(
            "decision|-|summarizer.rs:ground_shifts+|Check shift endpoints against AST rows",
        )
        .unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            record_story_map(
                &engine,
                "csr",
                "conv-dup",
                1,
                20,
                "Haiku",
                vec![entry.clone()],
            )
            .await;
            record_story_map(&engine, "csr", "conv-dup", 2, 40, "Haiku", vec![entry]).await;
        });

        let rows = storage
            .get_ledger_reflections_by_tag("narrative_shift", 10)
            .unwrap();
        assert_eq!(rows.len(), 1);
        let ledger = load_ledger(&storage, "csr");
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].rev, 2);
    }

    #[test]
    fn record_story_map_with_the_kill_switch_writes_nothing() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (engine, storage) = test_engine();
        seed_evo_row(
            &storage,
            "conv-kill",
            "/repo/src/summarizer.rs",
            "ground_shifts",
        );
        let entry = parse_shift_line(
            "decision|-|summarizer.rs:ground_shifts+|Check shift endpoints against AST rows",
        )
        .unwrap();

        std::env::set_var("CSR_NO_STORY_MAP", "1");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(record_story_map(
            &engine,
            "csr",
            "conv-kill",
            1,
            20,
            "Haiku",
            vec![entry],
        ));
        std::env::remove_var("CSR_NO_STORY_MAP");

        let rows = storage
            .get_ledger_reflections_by_tag("narrative_shift", 10)
            .unwrap();
        assert!(rows.is_empty());
        assert!(storage.get_meta("story_map:conv-kill").unwrap().is_none());
        assert!(storage.get_meta("shift_ledger:csr").unwrap().is_none());
        assert!(storage.get_meta("story_kind_prefs:csr").unwrap().is_none());
    }

    #[test]
    fn record_story_map_with_no_shifts_leaves_the_old_map() {
        let _guard = crate::story_map::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_STORY_MAP");

        let (engine, storage) = test_engine();
        seed_evo_row(
            &storage,
            "conv-keep",
            "/repo/src/summarizer.rs",
            "ground_shifts",
        );
        let entry = parse_shift_line(
            "decision|-|summarizer.rs:ground_shifts+|Check shift endpoints against AST rows",
        )
        .unwrap();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(record_story_map(
            &engine,
            "csr",
            "conv-keep",
            1,
            20,
            "Haiku",
            vec![entry],
        ));
        let before = storage.get_meta("story_map:conv-keep").unwrap().unwrap();

        rt.block_on(record_story_map(
            &engine,
            "csr",
            "conv-keep",
            2,
            35,
            "Haiku",
            Vec::new(),
        ));
        let after = storage.get_meta("story_map:conv-keep").unwrap().unwrap();
        assert_eq!(before, after);
    }
}
