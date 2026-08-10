//! `csr-engine dream --report`: a self-contained static HTML dream journal.
//!
//! Renders `witness_verdicts` (joined to `witness_ledger`) into a single
//! HTML file with everything inlined — CSS in a `<style>` block, no external
//! fonts, no JS, zero network requests. The template
//! (`report_template.html.jinja`) is compiled into the binary via
//! `include_str!`, so the rendered file is fully portable: open it anywhere,
//! offline, forever.
//!
//! This module only READS `witness_ledger`/`witness_verdicts` (through
//! `Storage`'s existing append-only query surface plus the small bulk
//! additions in `storage::witness_verdicts`) — it never writes an event.
//! Run `csr-engine dream` (no `--report`) first to actually produce new
//! verdicts; this just narrates whatever is already on record.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::storage::witness_verdicts::{DemotedSymbol, DreamEventRow, VerdictKind};
use crate::storage::Storage;

/// The compiled-in Jinja template — see the module doc. Named `.html` so
/// minijinja's default auto-escape callback treats every `{{ }}`
/// interpolation as HTML and escapes it; nothing in this report is trusted
/// input (symbol/file names ultimately come from a real codebase), so this
/// is a real safety property, not decoration.
const TEMPLATE: &str = include_str!("report_template.html.jinja");
const TEMPLATE_NAME: &str = "report.html";

// The standalone template is shared with older report shapes and is outside
// this renderer's change boundary. Replace only its timeline fragment before
// compilation, keeping the rest of the established document and CSS intact.
const LEGACY_TIMELINE_TEMPLATE: &str = r#"  <section>
    <h2>Timeline</h2>
    {% for day in data.days %}
    <div class="day-group">
      <p class="day-heading">{{ day.date }}</p>
      {% for event in day.events %}
      <div class="event">
        <span class="badge {{ event.verdict_slug }}">{{ event.verdict_label }}</span>
        <span class="symbol">{{ event.symbol }}</span>
        <span class="file">{{ event.file_relative }}</span>
        <span class="oids">receipt {{ event.receipt_short | default("—") }} · HEAD {{ event.observed_head_short }} · {{ event.time }}</span>
        {% if event.successor_line %}
        <span class="successor-line">{{ event.successor_line }}</span>
        {% endif %}
      </div>
      {% endfor %}
    </div>
    {% endfor %}
  </section>"#;

const JOURNAL_TIMELINE_TEMPLATE: &str = r#"  <section>
    <h2>Timeline</h2>
    <p class="subtitle">Searches touching these anchors carry [evolved] annotations with these receipts</p>

    {% for group in data.superseded_groups %}
    <div class="day-group">
      <p class="day-heading">{{ group.project }} · {{ group.file_relative }}</p>
      {% for event in group.events %}
      <div class="event">
        <span class="badge superseded">Superseded</span>
        <span class="symbol">{{ event.symbol }}</span>
        <span class="file">{{ event.file_relative }}</span>
        <span class="oids">receipt {{ event.receipt_short | default("—") }}</span>
        <span class="successor-line">{{ event.conversation_line }}</span>
      </div>
      {% endfor %}
    </div>
    {% endfor %}

    {% for day in data.other_days %}
    <div class="day-group">
      <p class="day-heading">{{ day.date }}</p>
      {% for event in day.events %}
      <div class="event">
        <span class="badge {{ event.verdict_slug }}">{{ event.verdict_label }}</span>
        <span class="symbol">{{ event.symbol }}</span>
        <span class="file">{{ event.file_relative }}</span>
        <span class="oids">receipt {{ event.receipt_short | default("—") }} · HEAD {{ event.observed_head_short }} · {{ event.time }}</span>
      </div>
      {% endfor %}
    </div>
    {% endfor %}

    {% for group in data.internal_groups %}
    <details class="day-group">
      <summary>{{ group.events | length }} internal/test symbols · {{ group.project }}</summary>
      {% for event in group.events %}
      <div class="event">
        <span class="badge {{ event.verdict_slug }}">{{ event.verdict_label }}</span>
        <span class="symbol">{{ event.symbol }}</span>
        <span class="file">{{ event.file_relative }}</span>
        {% if event.receipt_short %}<span class="oids">receipt {{ event.receipt_short }}</span>{% endif %}
        {% if event.conversation_line %}<span class="successor-line">{{ event.conversation_line }}</span>{% endif %}
      </div>
      {% endfor %}
    </details>
    {% endfor %}

    {% if data.reinstated %}
    <details class="day-group">
      <summary>{{ data.reinstated.count }} anchors re-observed at HEAD {{ data.reinstated.head_short }}</summary>
      {% for event in data.reinstated.events %}
      <div class="event">
        <span class="badge reinstated">Reinstated</span>
        <span class="symbol">{{ event.symbol }}</span>
        <span class="file">{{ event.file_relative }}</span>
        <span class="oids">HEAD {{ event.observed_head_short }} · {{ event.time }}</span>
      </div>
      {% endfor %}
    </details>
    {% endif %}
  </section>"#;

/// One rendered timeline event — the shape the template walks per day group.
#[derive(Debug, Clone, Serialize)]
struct EventView {
    symbol: String,
    file_relative: String,
    verdict_slug: &'static str,
    verdict_label: &'static str,
    receipt_short: Option<String>,
    observed_head_short: String,
    /// Set only for `superseded_by` — the "successor link line" the task
    /// asks for (the successor is identified by its receipt oid; there is
    /// no richer successor identity to show than that, since `witness_id`
    /// has no stable human-facing name).
    successor_line: Option<String>,
    conversation_line: Option<String>,
    /// `HH:MM:SS` slice of `created_at`.
    time: String,
}

/// Newest-first day group of events, mirroring `dream::group_by_anchor`'s
/// contiguous-grouping style (the source rows are already newest-first, so
/// grouping needs no re-sort — see `all_events_with_anchor`).
#[derive(Debug, Clone, Serialize)]
struct DayGroup {
    /// `YYYY-MM-DD`.
    date: String,
    events: Vec<EventView>,
}

/// User-code supersessions are the journal's primary evidence, grouped by
/// the exact source file whose anchor evolved.
#[derive(Debug, Clone, Serialize)]
struct SupersededFileGroup {
    project: String,
    file_relative: String,
    events: Vec<EventView>,
}

/// Test and fixture anchors remain available without dominating the journal.
#[derive(Debug, Clone, Serialize)]
struct InternalProjectGroup {
    project: String,
    events: Vec<EventView>,
}

/// Reinstatement receipts equal the observed HEAD, so they are narrated once
/// and their anchors are kept in a receipt-free disclosure list.
#[derive(Debug, Clone, Serialize)]
struct ReinstatedSummary {
    count: usize,
    head_short: String,
    events: Vec<EventView>,
}

/// One "what CSR forgot" row — a symbol currently on the `Demote` channel.
#[derive(Debug, Clone, Serialize)]
struct ForgottenView {
    symbol: String,
    file_relative: String,
    verdict_label: &'static str,
    receipt_short: Option<String>,
    observed_head_short: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct VerdictTotals {
    obsolete: i64,
    superseded: i64,
    reinstated: i64,
    total: i64,
}

/// Everything the template needs, gathered once from `Storage`.
#[derive(Debug, Clone, Serialize)]
struct DreamReportData {
    generated_at: String,
    has_run: bool,
    last_run_head_short: Option<String>,
    last_run_at: Option<String>,
    totals: VerdictTotals,
    days: Vec<DayGroup>,
    superseded_groups: Vec<SupersededFileGroup>,
    other_days: Vec<DayGroup>,
    internal_groups: Vec<InternalProjectGroup>,
    reinstated: Option<ReinstatedSummary>,
    forgotten: Vec<ForgottenView>,
    /// `true` iff zero events have ever been recorded — drives the "no
    /// dreams yet" empty state (the timeline section is skipped entirely;
    /// the forgotten section, which is independently emptiable, is not
    /// gated by this and gets its own "nothing demoted" message).
    is_empty: bool,
}

fn verdict_slug(v: VerdictKind) -> &'static str {
    match v {
        VerdictKind::AnchorObsolete => "obsolete",
        VerdictKind::SupersededBy => "superseded",
        VerdictKind::AnchorReinstated => "reinstated",
    }
}

fn verdict_label(v: VerdictKind) -> &'static str {
    match v {
        VerdictKind::AnchorObsolete => "Obsolete",
        VerdictKind::SupersededBy => "Superseded",
        VerdictKind::AnchorReinstated => "Reinstated",
    }
}

/// First 8 hex chars of a commit oid — long enough to disambiguate in any
/// real repo, short enough to keep the timeline scannable. `None`/empty in
/// is passed through as `None` so the template's `default("—")` filter can
/// render a dash instead of an empty string.
fn short_oid(oid: &str) -> String {
    oid.chars().take(8).collect()
}

fn short_oid_opt(oid: Option<&str>) -> Option<String> {
    oid.filter(|s| !s.is_empty()).map(short_oid)
}

/// A displayable symbol name — witness_ledger's `symbol` column is `None`
/// for whole-file witnesses.
fn symbol_display(symbol: Option<&str>) -> String {
    symbol.unwrap_or("(whole file)").to_string()
}

/// Best-effort repo-relative path for display: prefer the `code_nodes`-
/// stored `repo_root` (same resolution order `dream`'s own join uses), fall
/// back to a live git walk, fall back to the raw absolute path if neither
/// resolves — a report must never fail to render just because a file's repo
/// root can't be found.
fn repo_relative_file(storage: &Storage, file: &str) -> String {
    let repo_root = storage
        .stored_repo_root_for_file(file)
        .unwrap_or(None)
        .or_else(|| crate::extraction::repo_root::repo_root_for_file(file));
    match repo_root {
        Some(root) => {
            let root_trimmed = root.trim_end_matches('/');
            match file.strip_prefix(root_trimmed) {
                Some(rel) => rel.trim_start_matches('/').to_string(),
                None => file.to_string(),
            }
        }
        None => file.to_string(),
    }
}

fn conversation_line(
    e: &DreamEventRow,
    links: &BTreeMap<(String, String, String), Vec<String>>,
) -> Option<String> {
    if e.verdict != VerdictKind::SupersededBy {
        return None;
    }
    let ids = e
        .symbol
        .as_ref()
        .and_then(|symbol| links.get(&(e.project.clone(), e.file.clone(), symbol.clone())));
    Some(match ids {
        Some(ids) if !ids.is_empty() => {
            let previews = ids
                .iter()
                .take(3)
                .map(|id| id.chars().take(8).collect::<String>())
                .collect::<Vec<_>>()
                .join(" ");
            format!("appears in {} conversation(s): {previews}", ids.len())
        }
        _ => "no linked conversations".to_string(),
    })
}

fn event_view(
    storage: &Storage,
    e: &DreamEventRow,
    links: &BTreeMap<(String, String, String), Vec<String>>,
) -> EventView {
    let successor_line = if e.verdict == VerdictKind::SupersededBy {
        short_oid_opt(e.receipt_oid.as_deref())
            .map(|r| format!("superseded by the witness recorded at receipt {r}"))
    } else {
        None
    };
    EventView {
        symbol: symbol_display(e.symbol.as_deref()),
        file_relative: repo_relative_file(storage, &e.file),
        verdict_slug: verdict_slug(e.verdict),
        verdict_label: verdict_label(e.verdict),
        receipt_short: (e.verdict != VerdictKind::AnchorReinstated)
            .then(|| short_oid_opt(e.receipt_oid.as_deref()))
            .flatten(),
        observed_head_short: short_oid(&e.observed_head_oid),
        successor_line,
        conversation_line: conversation_line(e, links),
        time: e.created_at.get(11..19).unwrap_or("").to_string(),
    }
}

fn forgotten_view(storage: &Storage, d: &DemotedSymbol) -> ForgottenView {
    let rep = &d.state.representative;
    ForgottenView {
        symbol: symbol_display(d.symbol.as_deref()),
        file_relative: repo_relative_file(storage, &d.file),
        verdict_label: verdict_label(rep.verdict),
        receipt_short: short_oid_opt(rep.receipt_oid.as_deref()),
        observed_head_short: short_oid(&rep.observed_head_oid),
    }
}

/// Group already newest-first events into contiguous newest-first day
/// buckets (date = `created_at`'s first 10 chars, `YYYY-MM-DD`) — same
/// "already sorted, just group contiguous runs" shape as
/// `dream::group_by_anchor`.
fn group_by_day(
    storage: &Storage,
    events: &[DreamEventRow],
    links: &BTreeMap<(String, String, String), Vec<String>>,
) -> Vec<DayGroup> {
    let mut days: Vec<DayGroup> = Vec::new();
    for e in events {
        let date = e.created_at.get(..10).unwrap_or(&e.created_at).to_string();
        let view = event_view(storage, e, links);
        match days.last_mut() {
            Some(d) if d.date == date => d.events.push(view),
            _ => days.push(DayGroup {
                date,
                events: vec![view],
            }),
        }
    }
    days
}

fn is_testish(e: &DreamEventRow) -> bool {
    let symbol_is_test = e
        .symbol
        .as_deref()
        .is_some_and(|symbol| symbol.starts_with("test_") || symbol.starts_with("fixture_"));
    let normalized = e.file.replace('\\', "/");
    symbol_is_test || normalized.contains("/tests/") || normalized.starts_with("tests/")
}

fn journal_sections(
    storage: &Storage,
    events: &[DreamEventRow],
    links: &BTreeMap<(String, String, String), Vec<String>>,
) -> (
    Vec<SupersededFileGroup>,
    Vec<DayGroup>,
    Vec<InternalProjectGroup>,
    Option<ReinstatedSummary>,
) {
    let mut superseded: BTreeMap<(String, String), Vec<&DreamEventRow>> = BTreeMap::new();
    let mut other = Vec::new();
    let mut internal: BTreeMap<String, Vec<&DreamEventRow>> = BTreeMap::new();
    let mut reinstated = Vec::new();

    for event in events {
        if event.verdict == VerdictKind::AnchorReinstated {
            reinstated.push(event);
        } else if is_testish(event) {
            internal
                .entry(event.project.clone())
                .or_default()
                .push(event);
        } else if event.verdict == VerdictKind::SupersededBy {
            superseded
                .entry((event.project.clone(), event.file.clone()))
                .or_default()
                .push(event);
        } else {
            other.push(event.clone());
        }
    }

    let superseded_groups = superseded
        .into_iter()
        .map(|((project, file), mut rows)| {
            rows.sort_by_key(|event| std::cmp::Reverse(event.event_id));
            SupersededFileGroup {
                project,
                file_relative: repo_relative_file(storage, &file),
                events: rows
                    .into_iter()
                    .map(|event| event_view(storage, event, links))
                    .collect(),
            }
        })
        .collect();
    let internal_groups = internal
        .into_iter()
        .map(|(project, mut rows)| {
            rows.sort_by_key(|event| std::cmp::Reverse(event.event_id));
            InternalProjectGroup {
                project,
                events: rows
                    .into_iter()
                    .map(|event| event_view(storage, event, links))
                    .collect(),
            }
        })
        .collect();
    let reinstated_summary = if let Some(newest) = reinstated.first() {
        let count = reinstated.len();
        let head_short = short_oid(&newest.observed_head_oid);
        Some(ReinstatedSummary {
            count,
            head_short,
            events: reinstated
                .into_iter()
                .map(|event| event_view(storage, event, links))
                .collect(),
        })
    } else {
        None
    };

    (
        superseded_groups,
        group_by_day(storage, &other, links),
        internal_groups,
        reinstated_summary,
    )
}

/// Gather every piece of data the template needs. Read-only: three existing
/// `witness_ledger`/`witness_verdicts` query paths, no writes. Thin
/// wrapper over `gather_report_data_with` for real callers — reads
/// `CSR_DREAM_CONSUMPTION` from the real process env exactly once here.
fn gather_report_data(storage: &Storage) -> Result<DreamReportData> {
    gather_report_data_with(
        storage,
        crate::storage::recap_feeds::dream_consumption_mode(),
    )
}

/// Core of [`gather_report_data`] with the v10.1 dream-consumption opt-in
/// passed in as a parameter — mirrors `resolve_validity_with`/
/// `recap_retired_since_with`'s pattern so tests can drive the ON path
/// directly instead of mutating the real process env (parallel-test
/// race — same discipline as everywhere else this switch appears).
/// `consumption_enabled = false` renders the same neutral "no dreams
/// yet" state the template already supports (`is_empty: true`), WITHOUT
/// touching witness_ledger/witness_verdicts at all, rather than
/// computing real totals and hiding them — mirrors
/// `recap_retired_since_with`'s early return. `csr-engine dream
/// --report` still writes a valid, openable HTML file either way; only
/// its verdict content is gated.
fn gather_report_data_with(
    storage: &Storage,
    consumption_mode: crate::storage::recap_feeds::ConsumptionMode,
) -> Result<DreamReportData> {
    if consumption_mode == crate::storage::recap_feeds::ConsumptionMode::Off {
        return Ok(DreamReportData {
            generated_at: chrono::Utc::now()
                .format("%Y-%m-%d %H:%M:%S UTC")
                .to_string(),
            has_run: false,
            last_run_head_short: None,
            last_run_at: None,
            totals: VerdictTotals {
                obsolete: 0,
                superseded: 0,
                reinstated: 0,
                total: 0,
            },
            days: Vec::new(),
            superseded_groups: Vec::new(),
            other_days: Vec::new(),
            internal_groups: Vec::new(),
            reinstated: None,
            forgotten: Vec::new(),
            is_empty: true,
        });
    }

    let events = storage.all_dream_events()?;
    let (obsolete, superseded, reinstated) = storage.dream_event_totals()?;
    let last_run = storage.last_dream_run()?;
    let forgotten_raw = if consumption_mode == crate::storage::recap_feeds::ConsumptionMode::Full {
        storage.all_demoted_symbols()?
    } else {
        Vec::new()
    };

    let generated_at = chrono::Utc::now()
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string();

    let (last_run_head_short, last_run_at) = match &last_run {
        Some((oid, at)) => (Some(short_oid(oid)), Some(at.clone())),
        None => (None, None),
    };

    let anchors = events
        .iter()
        .filter(|event| event.verdict == VerdictKind::SupersededBy)
        .filter_map(|event| {
            event
                .symbol
                .as_ref()
                .map(|symbol| (event.project.clone(), event.file.clone(), symbol.clone()))
        })
        .collect::<Vec<_>>();
    let links = storage.with_connection(|conn| {
        crate::storage::witness_verdicts::conversation_ids_for_anchors(conn, &anchors)
    })?;
    let days = group_by_day(storage, &events, &links);
    let (superseded_groups, other_days, internal_groups, reinstated_summary) =
        journal_sections(storage, &events, &links);
    let forgotten = forgotten_raw
        .iter()
        .map(|d| forgotten_view(storage, d))
        .collect();

    let total = obsolete + superseded + reinstated;
    Ok(DreamReportData {
        generated_at,
        has_run: last_run.is_some(),
        last_run_head_short,
        last_run_at,
        totals: VerdictTotals {
            obsolete,
            superseded,
            reinstated,
            total,
        },
        days,
        superseded_groups,
        other_days,
        internal_groups,
        reinstated: reinstated_summary,
        forgotten,
        is_empty: total == 0,
    })
}

/// Render `data` through the embedded template into a complete, standalone
/// HTML document.
fn render_html(data: &DreamReportData) -> Result<String> {
    let template = TEMPLATE.replacen(LEGACY_TIMELINE_TEMPLATE, JOURNAL_TIMELINE_TEMPLATE, 1);
    if template == TEMPLATE {
        anyhow::bail!("embedded dream report timeline fragment no longer matches renderer");
    }
    let mut env = minijinja::Environment::new();
    env.add_template(TEMPLATE_NAME, &template)
        .context("compiling embedded dream report template")?;
    let tmpl = env
        .get_template(TEMPLATE_NAME)
        .context("loading embedded dream report template")?;
    let rendered = tmpl
        .render(minijinja::context! { data => minijinja::Value::from_serialize(data) })
        .context("rendering dream report template")?;
    Ok(rendered)
}

/// Default output path: `~/.claude-self-reflect/reports/dream-<YYYY-MM-DD>.html`.
fn default_report_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    home.join(".claude-self-reflect")
        .join("reports")
        .join(format!("dream-{date}.html"))
}

/// Best-effort `open <path>` on macOS; a no-op elsewhere (`--no-open` is the
/// only way to suppress it on macOS itself — there is nothing to suppress on
/// other platforms since this never fires there). Failure to launch a viewer
/// is never fatal — the file is written either way.
#[cfg(target_os = "macos")]
fn open_in_viewer(path: &Path) {
    let _ = std::process::Command::new("open").arg(path).status();
}

#[cfg(not(target_os = "macos"))]
fn open_in_viewer(_path: &Path) {}

/// Render the dream journal and write it to `out` (or the default dated
/// path under `~/.claude-self-reflect/reports/`), creating the parent
/// directory if needed. Opens it via `open` on macOS unless `no_open`.
/// Returns the path actually written, for the CLI to report back.
pub fn run_report(storage: &Storage, out: Option<PathBuf>, no_open: bool) -> Result<PathBuf> {
    let data = gather_report_data(storage)?;
    let html = render_html(&data)?;

    let path = out.unwrap_or_else(default_report_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating report directory {}", parent.display()))?;
    }
    std::fs::write(&path, html)
        .with_context(|| format!("writing dream report to {}", path.display()))?;

    if !no_open {
        open_in_viewer(&path);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::witness_ledger::{self, WitnessLedgerRow};
    use crate::storage::witness_verdicts::{self, VerdictKind, WitnessVerdictRow};

    fn ledger_row(
        project: &str,
        file: &str,
        symbol: Option<&str>,
        at_oid: &str,
        stamp: &str,
    ) -> WitnessLedgerRow {
        WitnessLedgerRow {
            id: 0,
            project: project.into(),
            file: file.into(),
            symbol: symbol.map(|s| s.to_string()),
            span_start: symbol.map(|_| 1),
            span_end: symbol.map(|_| 3),
            stamp: stamp.into(),
            tier: "committed".into(),
            at_oid: Some(at_oid.into()),
            source_kind: "backfill".into(),
            source_id: Some(at_oid.into()),
        }
    }

    #[test]
    fn empty_db_renders_no_dreams_yet() {
        let storage = Storage::open_memory().unwrap();
        let data =
            gather_report_data_with(&storage, crate::storage::recap_feeds::ConsumptionMode::Off)
                .unwrap();
        assert!(data.is_empty);
        assert!(!data.has_run);
        assert!(data.days.is_empty());
        assert!(data.forgotten.is_empty());

        let html = render_html(&data).unwrap();
        assert!(html.contains("No dreams yet"));
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("CSR Dream Journal"));
        // Zero network requests: no external script/link/stylesheet tags.
        assert!(!html.to_lowercase().contains("<script"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }

    #[test]
    fn consumption_off_renders_no_dreams_yet_even_with_real_verdicts() {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                conn.execute_batch("DROP TABLE witness_verdicts")?;
                Ok(())
            })
            .unwrap();
        // consumption_enabled reads real env (unset in this test process by
        // default) — dropping witness_verdicts on top proves the guard
        // clause returns before any query would have needed that table.
        let data =
            gather_report_data_with(&storage, crate::storage::recap_feeds::ConsumptionMode::Off)
                .unwrap();
        assert!(data.is_empty);
        assert!(!data.has_run);
        assert!(data.days.is_empty());
        assert!(data.forgotten.is_empty());

        let html = render_html(&data).unwrap();
        assert!(html.contains("No dreams yet"));
    }

    #[test]
    fn synthetic_events_render_key_strings() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_witness(&ledger_row(
                "proj",
                "/repo/src/lib.rs",
                Some("foo"),
                "aaa",
                "b3:1",
            ))
            .unwrap();
        storage
            .insert_witness(&ledger_row(
                "proj",
                "/repo/src/lib.rs",
                Some("foo"),
                "bbb",
                "b3:2",
            ))
            .unwrap();
        let w1 = storage
            .witnesses_for_file("proj", "/repo/src/lib.rs")
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some("aaa"))
            .unwrap();
        let w2 = storage
            .witnesses_for_file("proj", "/repo/src/lib.rs")
            .unwrap()
            .into_iter()
            .find(|r| r.at_oid.as_deref() == Some("bbb"))
            .unwrap();
        storage
            .insert_witness_verdict(&WitnessVerdictRow {
                witness_id: w1.id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: Some(w2.id),
                receipt_oid: Some("bbb".into()),
                observed_head_oid: "bbb".into(),
            })
            .unwrap();

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        assert!(!data.is_empty);
        assert!(data.has_run);
        assert_eq!(data.totals.superseded, 1);
        assert_eq!(data.totals.total, 1);
        assert_eq!(data.days.len(), 1);
        assert_eq!(data.days[0].events.len(), 1);
        assert_eq!(data.days[0].events[0].symbol, "foo");
        assert_eq!(data.days[0].events[0].verdict_slug, "superseded");
        assert!(data.days[0].events[0].successor_line.is_some());

        let html = render_html(&data).unwrap();
        assert!(html.contains("foo"));
        assert!(html.contains("Superseded"));
        assert!(html.contains("lib.rs"));
        assert!(!html.contains("No dreams yet"));
    }

    #[test]
    fn journal_leads_with_user_supersessions_and_collapses_noise_with_links() {
        let storage = Storage::open_memory().unwrap();
        let seed_event =
            |symbol: &str, file: &str, at_oid: &str, receipt: &str, verdict: VerdictKind| {
                storage
                    .insert_witness(&ledger_row("proj", file, Some(symbol), at_oid, at_oid))
                    .unwrap();
                let witness = storage
                    .witnesses_for_file("proj", file)
                    .unwrap()
                    .into_iter()
                    .find(|row| row.at_oid.as_deref() == Some(at_oid))
                    .unwrap();
                storage
                    .insert_witness_verdict(&WitnessVerdictRow {
                        witness_id: witness.id,
                        verdict,
                        successor_witness_id: (verdict == VerdictKind::SupersededBy)
                            .then_some(witness.id),
                        receipt_oid: Some(receipt.into()),
                        observed_head_oid: receipt.into(),
                    })
                    .unwrap();
            };

        seed_event(
            "business_old",
            "/repo/src/lib.rs",
            "anchor-old",
            "11111111-old",
            VerdictKind::SupersededBy,
        );
        seed_event(
            "business_new",
            "/repo/src/lib.rs",
            "anchor-new",
            "22222222-new",
            VerdictKind::SupersededBy,
        );
        seed_event(
            "test_helper",
            "/repo/src/lib.rs",
            "anchor-test",
            "33333333-test",
            VerdictKind::SupersededBy,
        );
        seed_event(
            "fixture_builder",
            "/repo/src/lib.rs",
            "anchor-fixture",
            "44444444-fixture",
            VerdictKind::SupersededBy,
        );
        seed_event(
            "integration_helper",
            "/repo/tests/integration.rs",
            "anchor-integration",
            "55555555-integration",
            VerdictKind::SupersededBy,
        );
        seed_event(
            "restored_one",
            "/repo/src/lib.rs",
            "anchor-restored-one",
            "headfeed-restored",
            VerdictKind::AnchorReinstated,
        );
        seed_event(
            "restored_two",
            "/repo/src/other.rs",
            "anchor-restored-two",
            "headfeed-restored",
            VerdictKind::AnchorReinstated,
        );
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO code_nodes
                        (id, project, file, kind, name, first_conv_id, last_conv_id)
                     VALUES ('business-node', 'proj', '/repo/src/lib.rs', 'function',
                             'business_new', 'aaaaaaaa-alpha', 'bbbbbbbb-beta')",
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();

        assert!(html.contains(
            "Searches touching these anchors carry [evolved] annotations with these receipts"
        ));
        let new_pos = html.find("business_new").unwrap();
        let old_pos = html.find("business_old").unwrap();
        let tests_pos = html.find("internal/test symbols").unwrap();
        assert!(
            new_pos < old_pos,
            "newest receipt must render first within its file"
        );
        assert!(
            old_pos < tests_pos,
            "user symbols must lead collapsed test symbols"
        );
        assert!(html.contains("3 internal/test symbols"));
        assert!(html.contains("appears in 2 conversation(s): aaaaaaaa bbbbbbbb"));
        assert!(html.contains("no linked conversations"));
        assert!(html.contains("2 anchors re-observed at HEAD headfeed"));
        assert!(html.contains("<details"));
        assert!(
            !html.contains("receipt headfeed"),
            "reinstatement details must not expose a tautological receipt column"
        );
    }

    /// Symbol names are user code identifiers interpolated into HTML; minijinja
    /// auto-escaping depends on TEMPLATE_NAME ending in `.html`. This regression
    /// test fails if that wiring is ever broken (e.g. the template gets renamed).
    #[test]
    fn hostile_symbol_names_are_html_escaped() {
        let storage = Storage::open_memory().unwrap();
        let payload = "<script>alert(1)</script>";
        storage
            .insert_witness(&ledger_row(
                "proj",
                "/repo/src/lib.rs",
                Some(payload),
                "aaa",
                "b3:1",
            ))
            .unwrap();
        let w = storage
            .witnesses_for_file("proj", "/repo/src/lib.rs")
            .unwrap()
            .pop()
            .unwrap();
        storage
            .insert_witness_verdict(&WitnessVerdictRow {
                witness_id: w.id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("bbb".into()),
                observed_head_oid: "bbb".into(),
            })
            .unwrap();

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();
        assert!(!html.contains(payload), "raw script tag leaked into HTML");
        // minijinja HTML-escapes `/` as well: </script> -> &lt;&#x2f;script&gt;
        assert!(html.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
    }

    #[test]
    fn demoted_symbol_appears_in_forgotten_section() {
        let storage = Storage::open_memory().unwrap();
        storage
            .insert_witness(&ledger_row(
                "proj",
                "/repo/src/gone.rs",
                Some("vanished"),
                "aaa",
                "b3:1",
            ))
            .unwrap();
        let w = storage
            .witnesses_for_file("proj", "/repo/src/gone.rs")
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        storage
            .insert_witness_verdict(&WitnessVerdictRow {
                witness_id: w.id,
                verdict: VerdictKind::AnchorObsolete,
                successor_witness_id: None,
                receipt_oid: Some("deadbeef".into()),
                observed_head_oid: "deadbeef".into(),
            })
            .unwrap();

        let data =
            gather_report_data_with(&storage, crate::storage::recap_feeds::ConsumptionMode::Full)
                .unwrap();
        assert_eq!(data.forgotten.len(), 1);
        assert_eq!(data.forgotten[0].symbol, "vanished");

        let html = render_html(&data).unwrap();
        assert!(html.contains("What CSR forgot"));
        assert!(html.contains("vanished"));
        assert!(html.contains("Demoted"));
    }

    #[test]
    fn report_no_open_writes_file_to_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        let out_path = dir.path().join("dream-test.html");

        let written = run_report(&storage, Some(out_path.clone()), true).unwrap();
        assert_eq!(written, out_path);
        let contents = std::fs::read_to_string(&written).unwrap();
        assert!(contents.contains("CSR Dream Journal"));
        assert!(contents.contains("No dreams yet"));
    }

    #[test]
    fn report_creates_parent_directory_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        let out_path = dir.path().join("nested").join("deeper").join("dream.html");

        let written = run_report(&storage, Some(out_path.clone()), true).unwrap();
        assert!(written.exists());
    }

    #[test]
    fn short_oid_truncates_to_eight_chars() {
        assert_eq!(short_oid("0123456789abcdef"), "01234567");
        assert_eq!(short_oid("abc"), "abc");
    }

    #[test]
    fn witness_ledger_module_smoke() {
        // Sanity: the helper row constructor round-trips through the real
        // storage module used by `gather_report_data` above.
        let conn_row = ledger_row("p", "/f.rs", None, "oid", "stamp");
        assert_eq!(conn_row.symbol, None);
        let _ = witness_ledger::insert_witness;
        let _ = witness_verdicts::VerdictChannel::Demote;
    }
}
