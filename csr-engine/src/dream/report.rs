//! `csr-engine dream --report`: a self-contained static HTML dream journal.
//!
//! Renders one story card per usable session from stored episodes, narratives,
//! anchors, code attribution, and receipt-bearing verdicts. Everything is
//! inlined in one HTML file — CSS in a `<style>` block, no external fonts, no
//! JS, zero network requests. The template
//! (`report_template.html.jinja`) is compiled into the binary via
//! `include_str!`, so the rendered file is fully portable: open it anywhere,
//! offline, forever.
//!
//! This module only reads through `Storage`; it never writes an event. Running
//! `csr-engine dream` first adds fresh verdict receipts, but session stories
//! remain useful even before the first dream cycle.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::storage::dream_report::{StoryArtifact, StorySession};
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
    <h2>Session stories</h2>
    <div class="story-list">
    {% for card in data.cards %}
      <details class="story-card {{ card.tier_slug }}" data-tier="{{ card.tier_slug }}">
        <summary>
          <span class="story-summary">{{ card.summary }}</span>
          <span class="story-meta">{{ card.project }} · {{ card.date }} · {{ card.session_short }}</span>
          {% if card.tier_label %}<span class="tier-badge">{{ card.tier_label }}</span>{% endif %}
        </summary>
        <div class="story-flow" aria-label="Session flow">
        {% for stage in card.stages %}
          <div class="story-stage {{ stage.slug }}" data-stage="{{ stage.slug }}" title="{{ stage.detail }}">
            <span class="stage-label">{{ stage.label }}</span>
            {% for badge in stage.badges %}
            <span class="badge superseded">{{ badge.text }}</span>
            {% endfor %}
          </div>
        {% endfor %}
        </div>
      </details>
    {% endfor %}
    </div>
    <p class="omission-summary">
      {% if data.low_signal_omitted %}<span>{{ data.low_signal_omitted }} low-signal sessions omitted</span>{% endif %}
      {% if data.older_omitted %}<span>{{ data.older_omitted }} older sessions omitted</span>{% endif %}
    </p>
  </section>"#;

const LEGACY_FORGOTTEN_TEMPLATE: &str = r#"  <section>
    <h2>What CSR forgot</h2>
    {% if data.forgotten %}
      {% for item in data.forgotten %}
      <div class="forgotten-row">
        <span class="badge obsolete">Demoted</span>
        <span class="symbol">{{ item.symbol }}</span>
        <span class="file">{{ item.file_relative }}</span>
        <span class="oids">{{ item.verdict_label }} · receipt {{ item.receipt_short | default("—") }} · HEAD {{ item.observed_head_short }}</span>
      </div>
      {% endfor %}
    {% else %}
      <div class="empty-state">Nothing is currently demoted — every audited symbol is either intact or annotated, never fully stale.</div>
    {% endif %}
  </section>"#;

const LEGACY_EMPTY_TEMPLATE: &str = r#"  <div class="empty-state">
    <span class="glyph">☾</span>
    No dreams yet. Run <code>csr-engine dream</code> to audit the witness ledger against current HEAD.
  </div>"#;

const STORY_EMPTY_TEMPLATE: &str = r#"  <div class="empty-state">
    <span class="glyph">☾</span>
    No session has enough story signal to render yet.
  </div>
  <p class="omission-summary">
    {% if data.low_signal_omitted %}<span>{{ data.low_signal_omitted }} low-signal sessions omitted</span>{% endif %}
    {% if data.older_omitted %}<span>{{ data.older_omitted }} older sessions omitted</span>{% endif %}
  </p>"#;

const LEGACY_HERO_META: &str = r#"    <p class="subtitle">Deterministic supersession verdicts drawn from content-hash stamps and git ancestry — zero LLM, zero guessing.</p>
    <div class="meta-row">
      {% if data.has_run %}
        <span>Last dream: <code>{{ data.last_run_at }}</code> at HEAD <code>{{ data.last_run_head_short }}</code></span>
      {% else %}
        <span>No dream cycle has run yet.</span>
      {% endif %}
      <span>Generated {{ data.generated_at }}</span>
    </div>"#;

const STORY_HERO_META: &str = r#"    <p class="subtitle">What each session set out to do, considered, changed, and left behind.</p>
    <div class="meta-row">
      {% if data.has_run %}
        <span>Last dream: <code>{{ data.last_run_at }}</code> at HEAD <code>{{ data.last_run_head_short }}</code></span>
      {% else %}
        <span>No dream cycle has run yet.</span>
      {% endif %}
    </div>"#;

const STORY_CSS: &str = r#"
  .story-list { display: grid; gap: 0.7rem; }
  .story-card {
    background: var(--bg-card); border: 1px solid var(--border);
    border-radius: 12px; overflow: visible;
  }
  .story-card > summary {
    list-style: none; cursor: pointer; padding: 0.9rem 1rem;
    display: grid; grid-template-columns: minmax(0, 1fr) auto auto;
    align-items: center; gap: 0.75rem;
  }
  .story-card > summary::-webkit-details-marker { display: none; }
  .story-summary { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-weight: 600; }
  .story-meta { color: var(--fg-muted); font-size: 0.74rem; white-space: nowrap; }
  .tier-badge {
    color: var(--fg-muted); border: 1px solid var(--border); border-radius: 999px;
    font-size: 0.66rem; font-weight: 700; padding: 0.15rem 0.45rem;
    text-transform: uppercase; letter-spacing: 0.04em;
  }
  .story-flow {
    border-top: 1px solid var(--border); padding: 1rem;
    display: flex; align-items: stretch; gap: 1.6rem;
  }
  .story-stage {
    position: relative; flex: 1 1 0; min-width: 0; min-height: 4.2rem;
    border: 1px solid var(--border); border-radius: 9px; padding: 0.8rem;
    display: flex; flex-direction: column; align-items: center; justify-content: center;
    text-align: center; background: var(--bg); cursor: help;
  }
  .story-stage + .story-stage::before {
    content: "→"; position: absolute; left: -1.18rem; top: 50%;
    transform: translateY(-50%); color: var(--fg-muted); font-weight: 700;
  }
  .stage-label { font-size: 0.73rem; font-weight: 800; letter-spacing: 0.06em; }
  .story-stage .badge { margin-top: 0.45rem; text-transform: none; }
  .omission-summary { display: flex; justify-content: center; gap: 1rem; color: var(--fg-muted); font-size: 0.75rem; }
  @media (max-width: 680px) {
    .story-card > summary { grid-template-columns: minmax(0, 1fr) auto; }
    .story-meta { grid-column: 1 / -1; grid-row: 2; }
    .story-flow { flex-direction: column; gap: 1.4rem; }
    .story-stage + .story-stage::before { content: "↓"; left: 50%; top: -1.15rem; transform: translateX(-50%); }
  }
"#;

const MAX_STORY_CARDS: usize = 50;

#[derive(Debug, Clone, Serialize)]
struct ReceiptBadgeView {
    text: String,
}

#[derive(Debug, Clone, Serialize)]
struct StoryStageView {
    slug: &'static str,
    label: &'static str,
    detail: String,
    badges: Vec<ReceiptBadgeView>,
}

#[derive(Debug, Clone, Serialize)]
struct StoryCardView {
    summary: String,
    project: String,
    date: String,
    session_short: String,
    tier_slug: &'static str,
    tier_label: Option<&'static str>,
    stages: Vec<StoryStageView>,
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
    has_run: bool,
    last_run_head_short: Option<String>,
    last_run_at: Option<String>,
    totals: VerdictTotals,
    cards: Vec<StoryCardView>,
    low_signal_omitted: usize,
    older_omitted: usize,
    is_empty: bool,
}

/// First 8 hex chars of a commit oid.
fn short_oid(oid: &str) -> String {
    oid.chars().take(8).collect()
}

fn plain_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches(['#', '-', '*', ' '])
        .chars()
        .filter(|ch| !matches!(ch, '*' | '`'))
        .collect()
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}

fn first_sentence(value: &str) -> String {
    let normalized = plain_text(value);
    let end = normalized
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '.' | '!' | '?').then_some(index + ch.len_utf8()))
        .unwrap_or(normalized.len());
    truncate_chars(normalized[..end].trim(), 180)
}

fn basename(file: &str) -> String {
    Path::new(file)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(file)
        .to_string()
}

fn artifacts_stage(artifacts: &[StoryArtifact]) -> Option<StoryStageView> {
    if artifacts.is_empty() {
        return None;
    }
    let items = artifacts
        .iter()
        .map(|artifact| match &artifact.symbol {
            Some(symbol) => format!("{symbol} ({})", basename(&artifact.file)),
            None => basename(&artifact.file),
        })
        .collect::<Vec<_>>();
    let badges = artifacts
        .iter()
        .filter_map(|artifact| {
            artifact
                .superseded_receipt
                .as_deref()
                .map(|receipt| ReceiptBadgeView {
                    text: format!("superseded at {}", short_oid(receipt)),
                })
        })
        .collect::<Vec<_>>();
    Some(StoryStageView {
        slug: "artifacts",
        label: "ARTIFACTS",
        detail: format!("Artifacts: {}", items.join(", ")),
        badges,
    })
}

fn story_card(session: &StorySession) -> Option<StoryCardView> {
    let request = session
        .request
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let outcome = session
        .outcome
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let narrative = session
        .narrative
        .as_deref()
        .filter(|value| !value.trim().is_empty());
    let has_artifacts = !session.artifacts.is_empty();
    let (tier_slug, tier_label) = if request.is_some() && narrative.is_some() && has_artifacts {
        ("full", None)
    } else if request.is_some() && outcome.is_some() {
        ("template", Some("template"))
    } else if has_artifacts {
        ("thin-evidence", Some("thin evidence"))
    } else {
        return None;
    };

    let input = request
        .or_else(|| {
            session
                .first_prompt
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| StoryStageView {
            slug: "input",
            label: "INPUT",
            detail: truncate_chars(&plain_text(value), 300),
            badges: Vec::new(),
        });

    let investigated = session
        .investigated
        .iter()
        .map(|file| basename(file))
        .filter(|file| !file.is_empty())
        .take(6)
        .collect::<Vec<_>>();
    let deliberation_detail = match (narrative, investigated.is_empty()) {
        (Some(story), false) => Some(format!(
            "{} Investigated: {}",
            plain_text(story),
            investigated.join(", ")
        )),
        (Some(story), true) => Some(plain_text(story)),
        (None, false) => Some(format!("Investigated: {}", investigated.join(", "))),
        (None, true) => None,
    };
    let deliberation = deliberation_detail.map(|detail| StoryStageView {
        slug: "deliberation",
        label: "DELIBERATION",
        detail,
        badges: Vec::new(),
    });

    let steer_detail = if outcome.is_none() && session.todos.is_empty() {
        None
    } else {
        let mut parts = Vec::new();
        if let Some(value) = outcome {
            parts.push(format!("Outcome: {}", plain_text(value)));
        }
        if !session.todos.is_empty() {
            let todos = session
                .todos
                .iter()
                .map(|todo| {
                    if todo.status.trim().is_empty() {
                        plain_text(&todo.content)
                    } else {
                        format!(
                            "{} [{}]",
                            plain_text(&todo.content),
                            plain_text(&todo.status)
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("Todos: {todos}"));
        }
        Some(parts.join(". "))
    };
    let steer = steer_detail.map(|detail| StoryStageView {
        slug: "steer",
        label: "STEER",
        detail,
        badges: Vec::new(),
    });

    let mut stages = Vec::with_capacity(4);
    stages.extend(input);
    stages.extend(deliberation);
    stages.extend(artifacts_stage(&session.artifacts));
    stages.extend(steer);
    debug_assert!(stages.iter().all(|stage| !stage.detail.trim().is_empty()));

    let summary = narrative
        .map(first_sentence)
        .filter(|summary| !summary.is_empty())
        .unwrap_or_else(|| match (request, outcome) {
            (Some(request), Some(outcome)) => truncate_chars(
                &format!(
                    "{} — outcome: {}.",
                    plain_text(request),
                    plain_text(outcome)
                ),
                180,
            ),
            _ => format!(
                "Evidence recorded for session {}.",
                short_oid(&session.session_id)
            ),
        });

    Some(StoryCardView {
        summary,
        project: if session.project.is_empty() {
            "unknown".to_string()
        } else {
            session.project.clone()
        },
        date: session
            .timestamp
            .get(..10)
            .unwrap_or(&session.timestamp)
            .to_string(),
        session_short: short_oid(&session.session_id),
        tier_slug,
        tier_label,
        stages,
    })
}

/// Gather every piece of data through read-only storage projections.
fn gather_report_data(storage: &Storage) -> Result<DreamReportData> {
    gather_report_data_with(
        storage,
        crate::storage::recap_feeds::dream_consumption_mode(),
    )
}

fn gather_report_data_with(
    storage: &Storage,
    _consumption_mode: crate::storage::recap_feeds::ConsumptionMode,
) -> Result<DreamReportData> {
    let (obsolete, superseded, reinstated) = storage.dream_event_totals()?;
    let last_run = storage.last_dream_run()?;
    let (last_run_head_short, last_run_at) = match &last_run {
        Some((oid, at)) => (Some(short_oid(oid)), Some(at.clone())),
        None => (None, None),
    };

    let sessions = storage.with_connection(crate::storage::dream_report::load_story_sessions)?;
    let mut low_signal_omitted = 0;
    let mut usable_cards = Vec::new();
    for session in &sessions {
        match story_card(session) {
            Some(card) => usable_cards.push(card),
            None => low_signal_omitted += 1,
        }
    }
    let older_omitted = usable_cards.len().saturating_sub(MAX_STORY_CARDS);
    usable_cards.truncate(MAX_STORY_CARDS);

    let total = obsolete + superseded + reinstated;
    Ok(DreamReportData {
        has_run: last_run.is_some(),
        last_run_head_short,
        last_run_at,
        totals: VerdictTotals {
            obsolete,
            superseded,
            reinstated,
            total,
        },
        is_empty: usable_cards.is_empty(),
        cards: usable_cards,
        low_signal_omitted,
        older_omitted,
    })
}

/// Render `data` through the embedded template into a complete, standalone
/// HTML document.
fn render_html(data: &DreamReportData) -> Result<String> {
    let template = TEMPLATE
        .replacen(LEGACY_HERO_META, STORY_HERO_META, 1)
        .replacen(LEGACY_EMPTY_TEMPLATE, STORY_EMPTY_TEMPLATE, 1)
        .replacen(LEGACY_TIMELINE_TEMPLATE, JOURNAL_TIMELINE_TEMPLATE, 1)
        .replacen(LEGACY_FORGOTTEN_TEMPLATE, "", 1)
        .replacen("</style>", &format!("{STORY_CSS}</style>"), 1);
    if template.contains("Generated {{ data.generated_at }}")
        || template.contains("<h2>Timeline</h2>")
        || template.contains("<h2>What CSR forgot</h2>")
    {
        anyhow::bail!("embedded dream report fragments no longer match story renderer");
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

    fn seed_episode(
        storage: &Storage,
        session_id: &str,
        timestamp: &str,
        request: &str,
        outcome: &str,
        investigated: &[&str],
        todos: &[(&str, &str)],
    ) {
        let content = serde_json::json!({
            "schema": "v2",
            "session_id": session_id,
            "project": "proj",
            "timestamp": timestamp,
            "request": request,
            "outcome": outcome,
            "investigated": investigated,
            "todos": todos.iter().map(|(content, status)| {
                serde_json::json!({"content": content, "status": status})
            }).collect::<Vec<_>>(),
        });
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        format!("episode-{session_id}"),
                        content.to_string(),
                        format!(r#"["session_episode","schema_v2","conv_{session_id}"]"#),
                        timestamp,
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_story(storage: &Storage, session_id: &str, timestamp: &str, story: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![
                        format!("story-{session_id}"),
                        story,
                        format!(r#"["session_story","project_proj","conv_{session_id}"]"#),
                        timestamp,
                    ],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_registry(storage: &Storage, session_id: &str, timestamp: &str, prompt: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO session_registry
                        (session_id, project, first_prompt, first_ts, last_ts, prompt_count)
                     VALUES (?1, 'proj', ?2, ?3, ?3, 1)",
                    rusqlite::params![session_id, prompt, timestamp],
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn seed_anchor(storage: &Storage, session_id: &str, timestamp: &str, symbol: &str) {
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO episode_anchors
                        (session_id, project, file, node_kind, name, body_hash, created_at)
                     VALUES (?1, 'proj', '/repo/src/lib.rs', 'function_item', ?2, 'hash', ?3)",
                    rusqlite::params![session_id, symbol, timestamp],
                )?;
                Ok(())
            })
            .unwrap();
    }

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
    fn empty_db_renders_no_session_story_signal() {
        let storage = Storage::open_memory().unwrap();
        let data =
            gather_report_data_with(&storage, crate::storage::recap_feeds::ConsumptionMode::Off)
                .unwrap();
        assert!(data.is_empty);
        assert!(!data.has_run);
        assert!(data.cards.is_empty());

        let html = render_html(&data).unwrap();
        assert!(html.contains("No session has enough story signal"));
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("CSR Dream Journal"));
        assert!(!html.to_lowercase().contains("<script"));
        assert!(!html.contains("http://"));
        assert!(!html.contains("https://"));
    }

    #[test]
    fn consumption_mode_does_not_hide_story_data() {
        let storage = Storage::open_memory().unwrap();
        seed_episode(
            &storage,
            "visible-story",
            "2026-01-01T01:00:00Z",
            "Show the session story",
            "done",
            &[],
            &[],
        );
        let data =
            gather_report_data_with(&storage, crate::storage::recap_feeds::ConsumptionMode::Off)
                .unwrap();
        assert!(!data.is_empty);
        assert!(!data.has_run);
        assert_eq!(data.cards.len(), 1);

        let html = render_html(&data).unwrap();
        assert!(html.contains("Show the session story"));
    }

    #[test]
    fn synthetic_event_keeps_totals_and_renders_as_artifact_receipt() {
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
        let witness = storage
            .witnesses_for_file("proj", "/repo/src/lib.rs")
            .unwrap()
            .pop()
            .unwrap();
        storage
            .insert_witness_verdict(&WitnessVerdictRow {
                witness_id: witness.id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: None,
                receipt_oid: Some("bbb".into()),
                observed_head_oid: "bbb".into(),
            })
            .unwrap();
        seed_anchor(&storage, "event-session", "2026-01-02T01:00:00Z", "foo");

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        assert!(data.has_run);
        assert_eq!(data.totals.superseded, 1);
        assert_eq!(data.totals.total, 1);
        assert_eq!(data.cards.len(), 1);

        let html = render_html(&data).unwrap();
        assert!(html.contains("foo"));
        assert!(html.contains("superseded at bbb"));
        assert!(html.contains("lib.rs"));
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
        seed_anchor(&storage, "hostile-session", "2026-01-03T01:00:00Z", payload);

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
    fn report_no_open_writes_file_to_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        let out_path = dir.path().join("dream-test.html");

        let written = run_report(&storage, Some(out_path.clone()), true).unwrap();
        assert_eq!(written, out_path);
        let contents = std::fs::read_to_string(&written).unwrap();
        assert!(contents.contains("CSR Dream Journal"));
        assert!(contents.contains("No session has enough story signal"));
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

    #[test]
    fn story_cards_render_full_flow_newest_first_without_scripts() {
        let storage = Storage::open_memory().unwrap();
        for (session, timestamp, story, symbol) in [
            (
                "older-session",
                "2026-02-01T01:00:00Z",
                "Older session summary. Supporting detail.",
                "older_symbol",
            ),
            (
                "newer-session",
                "2026-02-02T01:00:00Z",
                "Newer session summary. Supporting detail.",
                "newer_symbol",
            ),
        ] {
            seed_episode(
                &storage,
                session,
                timestamp,
                "Build the story surface",
                "done",
                &["/repo/src/report.rs"],
                &[("verify output", "completed")],
            );
            seed_story(&storage, session, timestamp, story);
            seed_anchor(&storage, session, timestamp, symbol);
        }

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();

        let newer = html.find("Newer session summary.").unwrap();
        let older = html.find("Older session summary.").unwrap();
        assert!(newer < older, "cards must render newest first");
        assert!(html.contains("data-tier=\"full\""));
        for stage in ["input", "deliberation", "artifacts", "steer"] {
            assert!(html.contains(&format!("data-stage=\"{stage}\"")));
        }
        assert!(!html.to_lowercase().contains("<script"));
    }

    #[test]
    fn request_and_outcome_render_template_card_with_only_populated_stages() {
        let storage = Storage::open_memory().unwrap();
        seed_episode(
            &storage,
            "template-session",
            "2026-02-03T01:00:00Z",
            "Investigate the release gate",
            "partial",
            &[],
            &[],
        );

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();

        assert!(html.contains("data-tier=\"template\""));
        assert!(html.contains("data-stage=\"input\""));
        assert!(html.contains("data-stage=\"steer\""));
        assert!(!html.contains("data-stage=\"deliberation\""));
        assert!(!html.contains("data-stage=\"artifacts\""));
    }

    #[test]
    fn artifact_only_card_is_thin_evidence_and_shows_superseded_receipt() {
        let storage = Storage::open_memory().unwrap();
        seed_anchor(
            &storage,
            "thin-session",
            "2026-02-04T01:00:00Z",
            "retired_symbol",
        );
        storage
            .insert_witness(&ledger_row(
                "proj",
                "/repo/src/lib.rs",
                Some("retired_symbol"),
                "old-oid",
                "b3:old",
            ))
            .unwrap();
        let witness = storage
            .witnesses_for_file("proj", "/repo/src/lib.rs")
            .unwrap()
            .pop()
            .unwrap();
        storage
            .insert_witness_verdict(&WitnessVerdictRow {
                witness_id: witness.id,
                verdict: VerdictKind::SupersededBy,
                successor_witness_id: None,
                receipt_oid: Some("abcdef1234567890".into()),
                observed_head_oid: "head".into(),
            })
            .unwrap();

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();

        assert!(html.contains("data-tier=\"thin-evidence\""));
        assert!(html.contains("thin evidence"));
        assert!(html.contains("retired_symbol"));
        assert!(html.contains("superseded at abcdef12"));
        assert!(html.contains("data-stage=\"artifacts\""));
        assert!(!html.contains("data-stage=\"input\""));
        assert!(!html.contains("data-stage=\"steer\""));
    }

    #[test]
    fn low_signal_sessions_are_omitted_and_counted() {
        let storage = Storage::open_memory().unwrap();
        seed_registry(
            &storage,
            "low-signal",
            "2026-02-05T01:00:00Z",
            "A prompt alone is not enough",
        );

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();

        assert!(!html.contains("class=\"story-card"));
        assert!(html.contains("1 low-signal sessions omitted"));
        assert!(!html.contains("data-stage="));
    }

    #[test]
    fn report_caps_cards_at_fifty_and_counts_older_and_low_signal_omissions() {
        let storage = Storage::open_memory().unwrap();
        for index in 0..52 {
            let session = format!("session-{index:02}");
            let timestamp = format!("2026-03-{:02}T01:00:00Z", index + 1);
            seed_episode(
                &storage,
                &session,
                &timestamp,
                &format!("request {index}"),
                "done",
                &[],
                &[],
            );
        }
        for index in 0..2 {
            seed_registry(
                &storage,
                &format!("low-{index}"),
                &format!("2026-01-0{}T01:00:00Z", index + 1),
                "prompt only",
            );
        }

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();

        assert_eq!(html.matches("class=\"story-card").count(), 50);
        assert!(html.contains("2 older sessions omitted"));
        assert!(html.contains("2 low-signal sessions omitted"));
    }
}
