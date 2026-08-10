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
          <span class="story-head">
            <span class="story-summary">{{ card.summary }}</span>
            {% if card.outcome_badge %}<span class="outcome-badge {{ card.outcome_slug }}">{{ card.outcome_badge }}</span>{% endif %}
            {% if card.tier_label %}<span class="tier-badge">{{ card.tier_label }}</span>{% endif %}
          </span>
          {% if card.description %}<span class="story-description">{{ card.description }}</span>{% endif %}
          <span class="story-meta"><span class="project-pill">{{ card.project }}</span><span>{{ card.date }}</span><span>{{ card.session_short }}</span></span>
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
    display: flex; flex-direction: column; gap: 0.4rem;
  }
  .story-card > summary::-webkit-details-marker { display: none; }
  .story-head { display: flex; align-items: baseline; gap: 0.55rem; flex-wrap: wrap; }
  .story-summary { font-weight: 650; font-size: 0.95rem; line-height: 1.35; overflow-wrap: anywhere; }
  .story-description {
    color: var(--fg-muted); font-size: 0.82rem; line-height: 1.5;
    overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 3;
    -webkit-box-orient: vertical; overflow: hidden;
  }
  .story-meta { display: flex; align-items: center; gap: 0.55rem; color: var(--fg-muted); font-size: 0.72rem; flex-wrap: wrap; }
  .project-pill {
    border: 1px solid var(--border); border-radius: 999px; padding: 0.1rem 0.5rem;
    font-weight: 700; letter-spacing: 0.02em;
  }
  .outcome-badge {
    border-radius: 999px; font-size: 0.66rem; font-weight: 800; padding: 0.15rem 0.5rem;
    text-transform: uppercase; letter-spacing: 0.04em; border: 1px solid transparent;
  }
  .outcome-badge.success { color: #1a7f37; background: rgba(26, 127, 55, 0.12); }
  .outcome-badge.partial { color: #9a6700; background: rgba(154, 103, 0, 0.12); }
  .outcome-badge.failed { color: #cf222e; background: rgba(207, 34, 46, 0.12); }
  .outcome-badge.noted { color: var(--fg-muted); border-color: var(--border); }
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
"#;

const STORY_SCRIPT: &str = r#"
// startOnLoad would render every diagram while its <details> parent is still
// closed (display:none) — mermaid measures zero geometry there and produces
// broken, unbindable SVGs. Diagrams render lazily on first card open instead.
mermaid.initialize({startOnLoad:false});
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
    // Mermaid prefixes every element id with its render id
    // ("mermaid-<ts>-state-S_INPUT-0"), so equality/startsWith never match —
    // verified live in headless Chrome. `includes` on the "-state-<id>-"
    // infix matches exactly one g.statediagram-state per stage.
    const candidates = svg.querySelectorAll("[id], [data-id]");
    const match = Array.from(candidates).find(node =>
      node.id === stageId ||
      node.id.includes(`-state-${stageId}-`) ||
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

  const renderCard = async card => {
    if (card.dataset.mermaidDone === "true") return;
    card.dataset.mermaidDone = "true";
    const pre = card.querySelector("pre.mermaid");
    if (pre && window.mermaid) {
      try { await mermaid.run({nodes: [pre]}); } catch (e) { /* keep raw source visible */ }
    }
    bindCard(card);
  };
  document.querySelectorAll("details.story-card").forEach(card => {
    card.addEventListener("toggle", () => { if (card.open) renderCard(card); });
    if (card.open) renderCard(card);
  });
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
    /// 1–3 sentence always-visible elaboration under the headline. Deterministic
    /// fallback (narrative/request excerpt); replaced by the curated version
    /// when headline curation runs.
    description: String,
    project: String,
    date: String,
    session_short: String,
    tier_slug: &'static str,
    tier_label: Option<&'static str>,
    /// "success" | "partial" | "failed" | "noted" — colors the outcome badge.
    outcome_slug: &'static str,
    outcome_badge: Option<String>,
    stage_ids: String,
    mermaid: String,
    artifacts: Vec<StoryArtifactView>,
    episode_json: String,
}

/// Classify a free-text episode outcome into a badge. Conservative: only
/// unambiguous wording gets a colored verdict; anything else shows as a
/// neutral "noted" chip with the leading words.
fn outcome_badge(outcome: Option<&str>) -> (&'static str, Option<String>) {
    let Some(text) = outcome else {
        return ("noted", None);
    };
    let lower = text.to_lowercase();
    if lower.contains("success") || lower.contains("shipped") || lower.contains("complete") {
        ("success", Some("success".into()))
    } else if lower.contains("partial") {
        ("partial", Some("partial".into()))
    } else if lower.contains("fail") || lower.contains("blocked") {
        ("failed", Some("failed".into()))
    } else {
        ("noted", Some(truncate_chars(&plain_text(text), 24)))
    }
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
    // A sentence ends at ./!/? only when followed by whitespace or the end of
    // the text — a bare '.' inside "v10.1", "github.com", or "clip-480p.mp4"
    // is not a boundary.
    let mut end = normalized.len();
    let mut chars = normalized.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if matches!(ch, '.' | '!' | '?') {
            match chars.peek() {
                None => {
                    end = index + ch.len_utf8();
                    break;
                }
                Some((_, next)) if next.is_whitespace() => {
                    end = index + ch.len_utf8();
                    break;
                }
                _ => {}
            }
        }
    }
    truncate_chars(normalized[..end].trim(), 180)
}

/// Strip machine scaffold that leaks into stored prompts/outcomes — extractor
/// signature blobs appended to session text ("--- Signature: {json…}").
fn strip_scaffold(value: &str) -> &str {
    let cut = value
        .find("--- Signature:")
        .or_else(|| value.find("Signature: {\""));
    match cut {
        Some(index) => value[..index].trim_end_matches(['-', ' ']).trim_end(),
        None => value,
    }
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
        .map(strip_scaffold)
        .filter(|value| !value.trim().is_empty());
    let outcome = session
        .outcome
        .as_deref()
        .map(strip_scaffold)
        .filter(|value| !value.trim().is_empty());
    let narrative = session
        .narrative
        .as_deref()
        .map(strip_scaffold)
        .filter(|value| !value.trim().is_empty());
    let has_artifacts = !session.artifacts.is_empty();
    let (tier_slug, tier_label) = if request.is_some() && narrative.is_some() && has_artifacts {
        ("full", None)
    } else if request.is_some() && outcome.is_some() {
        // Slug is a CSS/data-tier hook; the badge must read as English, not
        // as the internal ladder name.
        ("template", Some("partial record"))
    } else if has_artifacts {
        ("thin-evidence", Some("thin evidence"))
    } else {
        return None;
    };

    let input_text = request.or_else(|| {
        session
            .first_prompt
            .as_deref()
            .map(strip_scaffold)
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

    // Deterministic description: the narrative (or request) beyond what the
    // headline already says. Curation replaces this with a written version.
    let description = {
        let source = narrative.or(input_text).map(plain_text).unwrap_or_default();
        let candidate = truncate_chars(&source, 260);
        if candidate == summary {
            String::new()
        } else {
            candidate
        }
    };
    let (outcome_slug, outcome_badge) = outcome_badge(outcome);

    Some(StoryCardView {
        summary,
        description,
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
        outcome_slug,
        outcome_badge,
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

// --- AI-curated headlines -------------------------------------------------
//
// Raw first-prompts make poor card headlines ("pull the latest data."). When
// AI narratives are enabled, one batched `claude -p` call rewrites the summary
// line of every uncached card; results are cached in `journal_headlines` by a
// content hash, so an unchanged corpus re-renders at zero cost. The kill
// switch (`CSR_NO_AI_NARRATIVES=1`) suppresses the invocation but still
// applies previously cached headlines — they are already paid for.

const HEADLINE_MAX_CHARS: usize = 110;
const DESCRIPTION_MAX_CHARS: usize = 280;

fn headline_hash(card: &StoryCardView) -> String {
    let payload = format!("{}\u{1}{}", card.summary, card.episode_json);
    format!("{:016x}", crate::narrative::fnv1a_64(payload.as_bytes()))
}

/// Parse the model's batch response: a JSON object mapping session-short ids
/// to `{headline, description}` objects (bare strings tolerated as
/// headline-only), possibly wrapped in code fences or prose. Fail-open: any
/// shape problem yields an empty map and the cards keep their raw summaries.
fn parse_headline_batch(text: &str) -> std::collections::BTreeMap<String, (String, String)> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Default::default();
    };
    if end < start {
        return Default::default();
    }
    let clean = |value: &str, limit: usize| -> String {
        truncate_chars(
            &value.split_whitespace().collect::<Vec<_>>().join(" "),
            limit,
        )
    };
    serde_json::from_str::<std::collections::BTreeMap<String, serde_json::Value>>(
        &text[start..=end],
    )
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(id, value)| {
        let (headline, description) = match value {
            serde_json::Value::String(headline) => (headline, String::new()),
            serde_json::Value::Object(fields) => (
                fields
                    .get("headline")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                fields
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            _ => return None,
        };
        let headline = clean(&headline, HEADLINE_MAX_CHARS);
        (!headline.is_empty()).then(|| (id, (headline, clean(&description, DESCRIPTION_MAX_CHARS))))
    })
    .collect()
}

/// A cached row is only a hit when it carries a description too — rows written
/// by the description-less first shape re-curate once and are then complete.
fn cached_headline(storage: &Storage, session_id: &str, hash: &str) -> Option<(String, String)> {
    storage
        .with_connection(|conn| {
            Ok(conn
                .query_row(
                    "SELECT headline, description FROM journal_headlines
                     WHERE session_id = ?1 AND content_hash = ?2
                       AND description != ''",
                    rusqlite::params![session_id, hash],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .ok())
        })
        .ok()
        .flatten()
}

fn store_headline(
    storage: &Storage,
    session_id: &str,
    hash: &str,
    headline: &str,
    description: &str,
    model: &str,
) {
    let _ = storage.with_connection(|conn| {
        conn.execute(
            "INSERT INTO journal_headlines (session_id, content_hash, headline, description, model)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET
                content_hash = excluded.content_hash,
                headline = excluded.headline,
                description = excluded.description,
                model = excluded.model,
                created_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            rusqlite::params![session_id, hash, headline, description, model],
        )?;
        Ok(())
    });
}

fn headline_prompt(misses: &[(&StoryCardView, String)]) -> String {
    let sessions = misses
        .iter()
        .map(|(card, _)| {
            serde_json::json!({
                "id": card.session_short,
                "project": card.project,
                "text": truncate_chars(&card.summary, 300),
                "detail": truncate_chars(&card.episode_json, 600),
            })
        })
        .collect::<Vec<_>>();
    format!(
        "You are curating cards for a developer's private session journal. For each \
         session below produce: a headline (max {HEADLINE_MAX_CHARS} characters) \
         stating what the session actually did or found — name the feature, bug, or \
         artifact, never generic filler like 'worked on project'; and a description \
         (1-3 sentences, max {DESCRIPTION_MAX_CHARS} characters) adding the concrete \
         specifics — what was tried, what changed, what remains. Use only the text and \
         detail fields as evidence; do not invent outcomes they don't support. Return \
         ONLY a JSON object mapping each id to {{\"headline\": \"...\", \
         \"description\": \"...\"}}, no code fences, no commentary.\nSessions: {}",
        serde_json::to_string(&sessions).unwrap_or_default()
    )
}

/// Apply cached headlines and, unless narratives are disabled, batch-generate
/// the missing ones. Every failure path leaves the raw summaries in place.
fn curate_headlines(storage: &Storage, cards: &mut [StoryCardView]) {
    curate_headlines_with(storage, cards, !crate::narrative::narratives_disabled())
}

/// `allow_invoke` is threaded as a parameter (not read from the environment
/// here) so tests can exercise the cache paths without racing other tests on
/// the process-global `CSR_NO_AI_NARRATIVES` variable.
fn curate_headlines_with(storage: &Storage, cards: &mut [StoryCardView], allow_invoke: bool) {
    let hashes: Vec<String> = cards.iter().map(headline_hash).collect();
    let mut misses: Vec<usize> = Vec::new();
    for (index, card) in cards.iter_mut().enumerate() {
        match cached_headline(storage, &card.session_short, &hashes[index]) {
            Some((headline, description)) => {
                card.summary = headline;
                card.description = description;
            }
            None => misses.push(index),
        }
    }
    if misses.is_empty() || !allow_invoke {
        return;
    }

    let batch: Vec<(&StoryCardView, String)> = misses
        .iter()
        .map(|&index| (&cards[index] as &StoryCardView, hashes[index].clone()))
        .collect();
    let prompt = headline_prompt(&batch);
    let started = std::time::Instant::now();
    let parsed = match crate::hooks::session_briefing::invoke_narrative_briefing(&prompt) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(error = %err, "journal headline curation failed — keeping raw summaries");
            return;
        }
    };
    let _ = storage.record_narrative_usage(&crate::storage::NarrativeUsageRow {
        call_site: "journal_headline".into(),
        model: parsed.model.clone(),
        input_tokens: parsed.input_tokens,
        output_tokens: parsed.output_tokens,
        cache_read_tokens: parsed.cache_read_tokens,
        cache_creation_tokens: parsed.cache_creation_tokens,
        duration_ms: started.elapsed().as_millis() as i64,
        success: true,
    });

    let headlines = parse_headline_batch(&parsed.text);
    for &index in &misses {
        let (session_short, hash) = (cards[index].session_short.clone(), hashes[index].clone());
        if let Some((headline, description)) = headlines.get(&session_short) {
            store_headline(
                storage,
                &session_short,
                &hash,
                headline,
                description,
                &parsed.model,
            );
            cards[index].summary = headline.clone();
            if !description.is_empty() {
                cards[index].description = description.clone();
            }
        }
    }
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
    let mut data = gather_report_data(storage)?;
    curate_headlines(storage, &mut data.cards);
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
    fn parse_headline_batch_tolerates_fences_prose_and_clamps_length() {
        // New object shape: headline + description.
        let object = "```json\n{\"abc12345\": {\"headline\": \"Fixed the relevance gate in guardrails\", \"description\": \"Traced the broken gate, rewired it, added a test.\"}}\n```";
        let map = parse_headline_batch(object);
        let (headline, description) = map.get("abc12345").unwrap();
        assert_eq!(headline, "Fixed the relevance gate in guardrails");
        assert_eq!(
            description,
            "Traced the broken gate, rewired it, added a test."
        );

        // Legacy bare-string shape still parses as headline-only.
        let legacy = "{\"legacy01\": \"Plain headline\"}";
        assert_eq!(
            parse_headline_batch(legacy).get("legacy01"),
            Some(&("Plain headline".to_string(), String::new()))
        );

        let long = format!(
            "{{\"id1\": {{\"headline\": \"{}\", \"description\": \"{}\"}}}}",
            "x".repeat(400),
            "y".repeat(600)
        );
        let clamped = parse_headline_batch(&long);
        let (h, d) = clamped.get("id1").unwrap();
        assert!(h.chars().count() <= HEADLINE_MAX_CHARS);
        assert!(d.chars().count() <= DESCRIPTION_MAX_CHARS);

        assert!(parse_headline_batch("no json here").is_empty());
        assert!(parse_headline_batch("{\"id\": \"   \"}").is_empty());
        let multiline = "{\"id2\": \"line one\\n   line two\"}";
        assert_eq!(
            parse_headline_batch(multiline)
                .get("id2")
                .map(|v| v.0.as_str()),
            Some("line one line two")
        );
    }

    #[test]
    fn cached_headlines_apply_without_any_ai_invocation() {
        // Invocation disabled: curate must never spawn claude, yet a
        // previously cached headline (already paid for) still applies, and an
        // uncached card keeps its raw summary.
        let storage = Storage::open_memory().unwrap();
        let session = StorySession {
            session_id: "cached-headline-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("pull the latest data.".into()),
            outcome: Some("done".into()),
            ..Default::default()
        };
        let raw_card = story_card(&session).unwrap();
        let hash = headline_hash(&raw_card);
        store_headline(
            &storage,
            &raw_card.session_short,
            &hash,
            "Overnight metrics pull: MRR snapshot + spend check",
            "Pulled Meta spend and account balances; MRR snapshot confirmed at $25k.",
            "test-model",
        );

        let uncached = StorySession {
            session_id: "never-curated-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("do the other thing".into()),
            outcome: Some("partial".into()),
            ..Default::default()
        };
        let mut cards = vec![raw_card, story_card(&uncached).unwrap()];
        let raw_summary = cards[1].summary.clone();

        curate_headlines_with(&storage, &mut cards, false);

        assert_eq!(
            cards[0].summary,
            "Overnight metrics pull: MRR snapshot + spend check"
        );
        assert_eq!(
            cards[0].description,
            "Pulled Meta spend and account balances; MRR snapshot confirmed at $25k."
        );
        assert_eq!(cards[1].summary, raw_summary);
    }

    #[test]
    fn outcome_badges_classify_conservatively() {
        assert_eq!(outcome_badge(None), ("noted", None));
        assert_eq!(
            outcome_badge(Some("Shipped to production")),
            ("success", Some("success".into()))
        );
        assert_eq!(
            outcome_badge(Some("partial — two items left")),
            ("partial", Some("partial".into()))
        );
        assert_eq!(
            outcome_badge(Some("failed at the gate")),
            ("failed", Some("failed".into()))
        );
        let (slug, badge) = outcome_badge(Some("parked for review"));
        assert_eq!(slug, "noted");
        assert_eq!(badge.as_deref(), Some("parked for review"));
    }

    #[test]
    fn stale_cache_entry_is_ignored_when_content_hash_changes() {
        let storage = Storage::open_memory().unwrap();
        let session = StorySession {
            session_id: "stale-hash-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("original request".into()),
            outcome: Some("done".into()),
            ..Default::default()
        };
        let card = story_card(&session).unwrap();
        store_headline(
            &storage,
            &card.session_short,
            "0000000000000000",
            "Headline for content that no longer exists",
            "Description for content that no longer exists.",
            "test-model",
        );
        let mut cards = vec![card];
        let raw_summary = cards[0].summary.clone();
        curate_headlines_with(&storage, &mut cards, false);
        assert_eq!(
            cards[0].summary, raw_summary,
            "a hash-mismatched cache row must not be applied"
        );
    }

    #[test]
    fn first_sentence_never_cuts_inside_decimals_urls_or_filenames() {
        assert_eq!(
            first_sentence("5-day v10.1 release gate hold. More detail follows."),
            "5-day v10.1 release gate hold."
        );
        assert_eq!(
            first_sentence("review https://github.com/ramakay/csr then merge"),
            "review https://github.com/ramakay/csr then merge"
        );
        assert_eq!(
            first_sentence("upscale saadhana-v6-480p.mp4 to 4k. Done overnight."),
            "upscale saadhana-v6-480p.mp4 to 4k."
        );
        assert_eq!(first_sentence("ends exactly here."), "ends exactly here.");
    }

    #[test]
    fn signature_scaffold_never_reaches_summary_or_popover_json() {
        let session = StorySession {
            session_id: "sig-scrub-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some(
                "generate a cover photo --- Signature: {\"completion_status\":\"partial\"}".into(),
            ),
            outcome: Some("partial".into()),
            ..Default::default()
        };
        let card = story_card(&session).expect("request+outcome renders a card");
        assert!(
            !card.summary.contains("Signature"),
            "summary leaked scaffold: {}",
            card.summary
        );
        assert!(
            !card.episode_json.contains("completion_status"),
            "popover JSON leaked scaffold: {}",
            card.episode_json
        );
        assert!(card.summary.starts_with("generate a cover photo"));
    }

    #[test]
    fn template_tier_badge_reads_as_english_not_the_internal_ladder_name() {
        let session = StorySession {
            session_id: "tier-label-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("do the thing".into()),
            outcome: Some("done".into()),
            ..Default::default()
        };
        let card = story_card(&session).expect("request+outcome renders a card");
        assert_eq!(card.tier_slug, "template", "CSS hook must stay stable");
        assert_eq!(card.tier_label, Some("partial record"));
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
            html.matches("mermaid.initialize({startOnLoad:false})")
                .count(),
            1,
            "diagrams must render lazily on card open — startOnLoad renders \
             zero-geometry SVGs inside closed <details>"
        );
        assert!(
            html.contains(r#"card.addEventListener("toggle""#),
            "the lazy-render toggle hook must be wired"
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
