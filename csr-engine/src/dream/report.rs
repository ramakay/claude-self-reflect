//! `csr-engine dream --report`: a self-contained static HTML dream journal.
//!
//! Renders one story card per usable session from stored episodes, narratives,
//! anchors, code attribution, and receipt-bearing verdicts. Everything is
//! inlined in one HTML file — CSS, the vendored Mermaid runtime, and the
//! interaction script are embedded directly, with zero network requests. The template
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
const MERMAID_SOURCE: &str = include_str!("../../assets/mermaid.min.js");

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
        <div class="story-flow" aria-label="Session flow" data-stage-ids="{{ card.stage_ids }}">
          <pre class="mermaid">{{ card.mermaid | safe }}</pre>
        </div>
        {% if card.artifacts %}
        <div class="artifact-list" aria-label="Session artifacts">
        {% for artifact in card.artifacts %}
          <span class="artifact-chip">
            <span class="symbol">{{ artifact.label }}</span>
            <span class="file">{{ artifact.file }}</span>
            {% if artifact.receipt_badge %}<span class="badge superseded">{{ artifact.receipt_badge }}</span>{% endif %}
          </span>
        {% endfor %}
        </div>
        {% endif %}
        <script type="application/json" class="episode-data">{{ card.episode_json | safe }}</script>
        <div class="stage-popover" role="tooltip" hidden></div>
        <div class="episode-detail" aria-live="polite" hidden></div>
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
    min-height: 7rem; overflow-x: auto;
  }
  .story-flow .mermaid { margin: 0; min-width: 30rem; text-align: center; }
  .story-flow svg { display: block; margin: 0 auto; max-width: 100%; height: auto; }
  .artifact-list {
    display: flex; flex-wrap: wrap; gap: 0.45rem; padding: 0 1rem 1rem;
  }
  .artifact-chip {
    display: inline-flex; flex-wrap: wrap; align-items: center; gap: 0.4rem;
    max-width: 100%; border: 1px solid var(--border); border-radius: 8px;
    background: var(--bg); padding: 0.4rem 0.55rem;
  }
  .artifact-chip .symbol { overflow-wrap: anywhere; }
  .artifact-chip .file { overflow-wrap: anywhere; }
  .stage-popover, .episode-detail {
    background: var(--bg-card); color: var(--fg); border: 1px solid var(--border);
    border-radius: 10px; box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18);
    padding: 0.85rem 0.95rem; overflow-wrap: anywhere; white-space: normal;
  }
  .stage-popover {
    position: fixed; z-index: 1000; width: min(360px, calc(100vw - 24px));
    max-height: min(70vh, 34rem); overflow-y: auto; pointer-events: none;
  }
  .episode-detail { margin: 0 1rem 1rem; box-shadow: none; }
  .stage-popover h3, .episode-detail h3 { margin: 0 0 0.65rem; font-size: 0.8rem; letter-spacing: 0.06em; }
  .detail-section + .detail-section { margin-top: 0.65rem; }
  .detail-label { display: block; margin-bottom: 0.2rem; color: var(--fg-muted); font-size: 0.68rem; font-weight: 800; text-transform: uppercase; letter-spacing: 0.04em; }
  .detail-value { margin: 0; font-size: 0.82rem; line-height: 1.45; white-space: pre-wrap; overflow-wrap: anywhere; }
  .detail-list { margin: 0.25rem 0 0; padding-left: 1.15rem; font-size: 0.8rem; line-height: 1.45; }
  .detail-artifact { border-top: 1px solid var(--border); padding-top: 0.5rem; margin-top: 0.5rem; }
  .detail-artifact .file { display: block; margin-top: 0.2rem; }
  [hidden] { display: none !important; }
  .omission-summary { display: flex; justify-content: center; gap: 1rem; color: var(--fg-muted); font-size: 0.75rem; }
  @media (max-width: 680px) {
    .story-card > summary { grid-template-columns: minmax(0, 1fr) auto; }
    .story-meta { grid-column: 1 / -1; grid-row: 2; }
  }
"#;

const STORY_SCRIPT: &str = r#"
mermaid.initialize({startOnLoad:true});
(() => {
  const labels = {
    S_INPUT: "INPUT",
    S_DELIB: "DELIBERATION",
    S_ART: "ARTIFACTS",
    S_STEER: "STEER"
  };

  const element = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined && text !== null) node.textContent = String(text);
    return node;
  };

  const section = (root, label, value) => {
    if (value === undefined || value === null || value === "") return;
    const wrapper = element("div", "detail-section");
    wrapper.append(element("span", "detail-label", label));
    wrapper.append(element("p", "detail-value", value));
    root.append(wrapper);
  };

  const list = (root, label, values, format) => {
    if (!Array.isArray(values) || values.length === 0) return;
    const wrapper = element("div", "detail-section");
    wrapper.append(element("span", "detail-label", label));
    const items = element("ul", "detail-list");
    values.forEach(value => items.append(element("li", "", format(value))));
    wrapper.append(items);
    root.append(wrapper);
  };

  const renderStage = (root, stageId, data) => {
    root.replaceChildren(element("h3", "", labels[stageId]));
    if (stageId === "S_INPUT") {
      section(root, "Request", data.request);
    } else if (stageId === "S_DELIB") {
      section(root, "Narrative", data.narrative);
      list(root, "Investigated files", data.investigated, value => value);
    } else if (stageId === "S_ART") {
      (data.artifacts || []).forEach(artifact => {
        const wrapper = element("div", "detail-artifact");
        wrapper.append(element("span", "symbol", artifact.symbol || "File artifact"));
        wrapper.append(element("span", "file", artifact.file));
        if (artifact.superseded_receipt) {
          wrapper.append(element("span", "badge superseded", `superseded at ${artifact.superseded_receipt.slice(0, 8)}`));
        }
        list(wrapper, "Conversations", artifact.conversations, value => value);
        root.append(wrapper);
      });
    } else if (stageId === "S_STEER") {
      section(root, "Outcome", data.outcome);
      list(root, "Todos", data.todos, todo => todo.status ? `${todo.content} [${todo.status}]` : todo.content);
    }
  };

  const positionPopover = (popover, event) => {
    const gap = 14;
    const left = Math.max(12, Math.min(event.clientX + gap, window.innerWidth - popover.offsetWidth - 12));
    const top = Math.max(12, Math.min(event.clientY + gap, window.innerHeight - popover.offsetHeight - 12));
    popover.style.left = `${left}px`;
    popover.style.top = `${top}px`;
  };

  const findStateNode = (svg, stageId) => {
    const candidates = svg.querySelectorAll("[id], [data-id]");
    const match = Array.from(candidates).find(node =>
      node.id === stageId ||
      node.id.startsWith(`state-${stageId}-`) ||
      node.getAttribute("data-id") === stageId
    );
    return match ? (match.closest("g") || match) : null;
  };

  const bindCard = card => {
    const svg = card.querySelector(".story-flow svg");
    if (!svg) return;
    const data = JSON.parse(card.querySelector(".episode-data").textContent);
    const popover = card.querySelector(".stage-popover");
    const panel = card.querySelector(".episode-detail");
    const stageIds = card.querySelector(".story-flow").dataset.stageIds.split(/\s+/).filter(Boolean);

    stageIds.forEach(stageId => {
      const node = findStateNode(svg, stageId);
      if (!node || node.dataset.episodeBound === "true") return;
      node.dataset.episodeBound = "true";
      node.style.cursor = "pointer";
      node.addEventListener("pointerenter", event => {
        renderStage(popover, stageId, data);
        popover.hidden = false;
        positionPopover(popover, event);
      });
      node.addEventListener("pointermove", event => positionPopover(popover, event));
      node.addEventListener("pointerleave", () => { popover.hidden = true; });
      node.addEventListener("click", () => {
        if (!panel.hidden && panel.dataset.stageId === stageId) {
          panel.hidden = true;
          delete panel.dataset.stageId;
          return;
        }
        renderStage(panel, stageId, data);
        panel.dataset.stageId = stageId;
        panel.hidden = false;
      });
    });
  };

  const bindAll = () => document.querySelectorAll(".story-card").forEach(bindCard);
  const storyList = document.querySelector(".story-list");
  if (storyList) new MutationObserver(bindAll).observe(storyList, {childList: true, subtree: true});
  bindAll();
  window.addEventListener("load", bindAll, {once: true});
})();
"#;

const MAX_STORY_CARDS: usize = 50;

#[derive(Debug, Clone, Serialize)]
struct StoryStageView {
    id: &'static str,
    label: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct StoryArtifactView {
    label: String,
    file: String,
    receipt_badge: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct StoryCardView {
    summary: String,
    project: String,
    date: String,
    session_short: String,
    tier_slug: &'static str,
    tier_label: Option<&'static str>,
    stage_ids: String,
    mermaid: String,
    artifacts: Vec<StoryArtifactView>,
    episode_json: String,
}

#[derive(Serialize)]
struct EpisodeDataView<'a> {
    request: Option<&'a str>,
    narrative: Option<&'a str>,
    investigated: &'a [String],
    artifacts: &'a [StoryArtifact],
    outcome: Option<&'a str>,
    todos: &'a [crate::storage::dream_report::StoryTodo],
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
    (!artifacts.is_empty()).then_some(StoryStageView {
        id: "S_ART",
        label: "ARTIFACTS",
    })
}

fn mermaid_for_stages(stages: &[StoryStageView]) -> String {
    let mut lines = vec![
        "stateDiagram-v2".to_string(),
        "    direction LR".to_string(),
    ];
    lines.extend(
        stages
            .iter()
            .map(|stage| format!("    state \"{}\" as {}", stage.label, stage.id)),
    );
    lines.extend(
        stages
            .windows(2)
            .map(|pair| format!("    {} --> {}", pair[0].id, pair[1].id)),
    );
    lines.join("\n")
}

fn json_for_html_script<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value)
        .expect("episode report data contains only infallibly serializable fields")
        .replace('&', "\\u0026")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
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

    let input_text = request.or_else(|| {
        session
            .first_prompt
            .as_deref()
            .filter(|value| !value.trim().is_empty())
    });
    let input = input_text.map(|_| StoryStageView {
        id: "S_INPUT",
        label: "INPUT",
    });

    let deliberation =
        (narrative.is_some() || !session.investigated.is_empty()).then_some(StoryStageView {
            id: "S_DELIB",
            label: "DELIBERATION",
        });

    let steer = (outcome.is_some() || !session.todos.is_empty()).then_some(StoryStageView {
        id: "S_STEER",
        label: "STEER",
    });

    let mut stages = Vec::with_capacity(4);
    stages.extend(input);
    stages.extend(deliberation);
    stages.extend(artifacts_stage(&session.artifacts));
    stages.extend(steer);
    let stage_ids = stages
        .iter()
        .map(|stage| stage.id)
        .collect::<Vec<_>>()
        .join(" ");
    let mermaid = mermaid_for_stages(&stages);

    let artifacts = session
        .artifacts
        .iter()
        .map(|artifact| StoryArtifactView {
            label: artifact
                .symbol
                .clone()
                .unwrap_or_else(|| basename(&artifact.file)),
            file: artifact.file.clone(),
            receipt_badge: artifact
                .superseded_receipt
                .as_deref()
                .map(|receipt| format!("superseded at {}", short_oid(receipt))),
        })
        .collect();
    let episode_json = json_for_html_script(&EpisodeDataView {
        request: input_text,
        narrative,
        investigated: &session.investigated,
        artifacts: &session.artifacts,
        outcome,
        todos: &session.todos,
    });

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
        stage_ids,
        mermaid,
        artifacts,
        episode_json,
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

/// Replace exactly one occurrence of `legacy` with `replacement`, panicking
/// with a message naming the drifted fragment if `legacy` does not occur in
/// `template` exactly once.
///
/// `str::replacen(..., 1)` silently no-ops when the needle is absent, and
/// silently leaves a second copy in place when the needle occurs more than
/// once — either way the embedded template could drift out from under this
/// renderer's assumptions without any error (finding 9). This asserts the
/// precondition up front, at report-generation time, instead of hoping a
/// downstream substring check happens to catch the drift.
fn replace_exactly_once(template: &str, legacy: &str, replacement: &str, name: &str) -> String {
    let occurrences = template.matches(legacy).count();
    assert_eq!(
        occurrences, 1,
        "embedded dream report template fragment {name} must occur exactly once \
         before replacement (found {occurrences}); the compiled-in template \
         (report_template.html.jinja) has drifted from what report.rs expects — \
         update the LEGACY_* constant in report.rs to match"
    );
    template.replacen(legacy, replacement, 1)
}

/// Render `data` through the embedded template into a complete, standalone
/// HTML document.
fn render_html(data: &DreamReportData) -> Result<String> {
    let template = replace_exactly_once(
        TEMPLATE,
        LEGACY_HERO_META,
        STORY_HERO_META,
        "LEGACY_HERO_META",
    );
    let template = replace_exactly_once(
        &template,
        LEGACY_EMPTY_TEMPLATE,
        STORY_EMPTY_TEMPLATE,
        "LEGACY_EMPTY_TEMPLATE",
    );
    let template = replace_exactly_once(
        &template,
        LEGACY_TIMELINE_TEMPLATE,
        JOURNAL_TIMELINE_TEMPLATE,
        "LEGACY_TIMELINE_TEMPLATE",
    );
    let template = replace_exactly_once(
        &template,
        LEGACY_FORGOTTEN_TEMPLATE,
        "",
        "LEGACY_FORGOTTEN_TEMPLATE",
    );
    let template = replace_exactly_once(
        &template,
        "</style>",
        &format!("{STORY_CSS}</style>"),
        "</style> (STORY_CSS insertion point)",
    );
    if template.contains(LEGACY_HERO_META)
        || template.contains(LEGACY_EMPTY_TEMPLATE)
        || template.contains(LEGACY_TIMELINE_TEMPLATE)
        || template.contains(LEGACY_FORGOTTEN_TEMPLATE)
        || template.contains("Generated {{ data.generated_at }}")
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
    let scripts =
        format!("<script>{MERMAID_SOURCE}</script>\n<script>{STORY_SCRIPT}</script>\n</body>");
    Ok(rendered.replacen("</body>", &scripts, 1))
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
    use std::collections::BTreeSet;

    const EPISODE_DATA_OPEN: &str = r#"<script type="application/json" class="episode-data">"#;

    fn mermaid_blocks(html: &str) -> Vec<&str> {
        html.split(r#"<pre class="mermaid">"#)
            .skip(1)
            .filter_map(|tail| tail.split_once("</pre>").map(|(block, _)| block))
            .collect()
    }

    fn mermaid_stage_ids(block: &str) -> BTreeSet<&str> {
        block
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .filter(|token| token.starts_with("S_"))
            .collect()
    }

    fn episode_data_blocks(html: &str) -> Vec<serde_json::Value> {
        html.split(EPISODE_DATA_OPEN)
            .skip(1)
            .map(|tail| {
                let (json, _) = tail
                    .split_once("</script>")
                    .expect("episode-data script must be closed");
                serde_json::from_str(json).expect("episode-data must contain valid JSON")
            })
            .collect()
    }

    fn assert_no_external_resources(html: &str) {
        let lower = html.to_ascii_lowercase();
        for forbidden in [r#"src="http"#, r#"href="http"#, "cdn.", "integrity="] {
            assert!(
                !lower.contains(forbidden),
                "report must not contain external resource reference {forbidden:?}"
            );
        }
    }

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
        assert_no_external_resources(&html);
        assert_eq!(
            html.matches(r#""use strict";var __esbuild_esm_mermaid_nm;"#)
                .count(),
            1,
            "the vendored Mermaid runtime must be embedded exactly once"
        );
        assert_eq!(
            html.matches("mermaid.initialize({startOnLoad:true})")
                .count(),
            1
        );
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
    fn hostile_symbol_names_are_escaped_in_html_and_episode_json() {
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
        assert!(
            html.contains(r#"\u003cscript\u003ealert(1)\u003c/script\u003e"#),
            "JSON script context must neutralize HTML delimiters"
        );
        let episode_data = episode_data_blocks(&html);
        assert_eq!(episode_data.len(), 1);
        assert_eq!(episode_data[0]["artifacts"][0]["symbol"], payload);
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

    /// Regression for finding 9: `replace_exactly_once` must reject a
    /// missing legacy fragment loudly rather than let `replacen` silently
    /// no-op, which would leave stale legacy markup in the rendered report
    /// with no error at all.
    #[test]
    #[should_panic(expected = "SOME_FRAGMENT")]
    fn replace_exactly_once_panics_when_fragment_is_absent() {
        replace_exactly_once("nothing to see here", "NEEDLE", "X", "SOME_FRAGMENT");
    }

    /// Regression for finding 9: a fragment that drifted into occurring more
    /// than once must also be rejected — `replacen(..., 1)` would otherwise
    /// silently leave a second stale copy behind after replacing only the
    /// first.
    #[test]
    #[should_panic(expected = "found 2")]
    fn replace_exactly_once_panics_when_fragment_repeats() {
        replace_exactly_once("NEEDLE and NEEDLE again", "NEEDLE", "X", "SOME_FRAGMENT");
    }

    #[test]
    fn replace_exactly_once_replaces_the_single_occurrence() {
        assert_eq!(
            replace_exactly_once("a NEEDLE b", "NEEDLE", "X", "SOME_FRAGMENT"),
            "a X b"
        );
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
    fn story_cards_render_full_mermaid_flow_newest_first() {
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
        let blocks = mermaid_blocks(&html);
        assert_eq!(blocks.len(), 2);
        let expected = BTreeSet::from(["S_INPUT", "S_DELIB", "S_ART", "S_STEER"]);
        for block in blocks {
            assert!(block.contains("stateDiagram-v2"));
            assert!(block.contains("direction LR"));
            assert_eq!(mermaid_stage_ids(block), expected);
        }
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
        let blocks = mermaid_blocks(&html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            mermaid_stage_ids(blocks[0]),
            BTreeSet::from(["S_INPUT", "S_STEER"]),
            "tier-2 cards must draw only their populated stages"
        );
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
        let blocks = mermaid_blocks(&html);
        assert_eq!(blocks.len(), 1);
        assert_eq!(mermaid_stage_ids(blocks[0]), BTreeSet::from(["S_ART"]));
    }

    #[test]
    fn episode_data_json_round_trips_fixture_content() {
        let storage = Storage::open_memory().unwrap();
        let session_id = "episode-json-session";
        seed_episode(
            &storage,
            session_id,
            "2026-02-05T01:00:00Z",
            "Trace the request exactly",
            "shipped safely",
            &["/repo/src/report.rs", "/repo/src/storage.rs"],
            &[("publish follow-up", "pending")],
        );
        seed_story(
            &storage,
            session_id,
            "2026-02-05T01:00:00Z",
            "Narrative prose with exact fixture content.",
        );
        seed_anchor(
            &storage,
            session_id,
            "2026-02-05T01:00:00Z",
            "fixture_symbol",
        );

        let data = gather_report_data_with(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        let html = render_html(&data).unwrap();
        let episodes = episode_data_blocks(&html);

        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0]["request"], "Trace the request exactly");
        assert_eq!(
            episodes[0]["narrative"],
            "Narrative prose with exact fixture content."
        );
        assert_eq!(
            episodes[0]["investigated"],
            serde_json::json!(["/repo/src/report.rs", "/repo/src/storage.rs"])
        );
        assert_eq!(episodes[0]["outcome"], "shipped safely");
        assert_eq!(
            episodes[0]["todos"],
            serde_json::json!([{"content": "publish follow-up", "status": "pending"}])
        );
        assert_eq!(
            episodes[0]["artifacts"],
            serde_json::json!([{
                "symbol": "fixture_symbol",
                "file": "/repo/src/lib.rs",
                "conversations": [session_id]
            }])
        );
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
