//! `csr-engine dream --report`: a self-contained static HTML dream journal.
//!
//! Renders a two-pane mailbox: a dense scannable index of sessions on the
//! left, one session's full ASK -> DELIBERATION -> STEER -> OUTCOME lineage
//! on the right. Thin sessions (some signal, not enough for a full lineage)
//! fold into a collapsed rollup below the index instead of diluting it.
//! Everything is inlined in one HTML file — CSS and the interaction script
//! are embedded directly, with zero network requests. The template
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

use crate::storage::dream_report::{self, StoryArtifact, StorySession};
use crate::storage::Storage;
use crate::transcript::instrumentation::{ErrorEvent, SessionInstrumentation};

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

// Two-pane mailbox: a dense session index on the left, the selected
// session's full lineage on the right. Server-rendered end to end — the
// pane that's visible on load, the day grouping, the stage cards, all
// arrive as real DOM, not JS-constructed after the fact. Vanilla JS only
// handles pane swap, client-side re-sort, and the chip popover.
const JOURNAL_TIMELINE_TEMPLATE: &str = r#"  <section class="mailbox" aria-label="Session mailbox">
    <div class="index-pane">
      <div class="sort-controls" role="group" aria-label="Sort sessions">
        <span class="sort-label">sort</span>
        <button type="button" class="sort-btn active" data-sort="recency">recency</button>
        <button type="button" class="sort-btn" data-sort="group">group</button>
        <button type="button" class="sort-btn" data-sort="type">type</button>
      </div>
      <div class="session-rows" role="listbox" aria-label="Sessions">
        {% for group in data.day_groups %}
        <p class="group-header">{{ group.label }}</p>
        {% for row in group.rows %}
        <button type="button" class="index-row{% if row.selected %} selected{% endif %}" data-session="{{ row.session_short }}" data-ts="{{ row.ts }}" data-project="{{ row.project }}" data-outcome="{{ row.outcome_slug }}" data-day="{{ row.day }}" role="option" aria-selected="{% if row.selected %}true{% else %}false{% endif %}">
          <span class="row-head"><span class="glyph {{ row.outcome_slug }}">{{ row.glyph }}</span><span class="project-pill">{{ row.project }}</span></span>
          <span class="sentence">{{ row.sentence }}</span>
          <span class="instrumentation">{% for segment in row.segments %}<span class="chip{% if segment.degraded %} degraded{% endif %}"{% if segment.popover %} data-popover="{{ segment.popover }}"{% endif %}{% if segment.title %} title="{{ segment.title }}"{% endif %}>{{ segment.text }}</span>{% if not loop.last %} · {% endif %}{% endfor %}</span>
        </button>
        {% endfor %}
        {% endfor %}
      </div>
      {% if data.thin_groups %}
      <div class="thin-rollup">
        <p class="thin-rollup-header">{{ data.thin_total }} thin sessions</p>
        {% for group in data.thin_groups %}
        <details class="thin-group">
          <summary><span class="thin-project">{{ group.project }}</span><span class="thin-count">{{ group.count }}</span></summary>
          {% for row in group.rows %}
          <p class="thin-row"><span class="thin-time">{{ row.time }}</span><span class="thin-excerpt">{{ row.excerpt }}</span></p>
          {% endfor %}
        </details>
        {% endfor %}
      </div>
      {% endif %}
    </div>
    <div class="detail-pane-wrap">
      <div class="chip-popover" role="tooltip" hidden></div>
      {% for pane in data.detail_panes %}
      <article class="detail-pane" data-session="{{ pane.session_short }}"{% if not loop.first %} hidden{% endif %}>
        <header class="detail-head">
          <h2 class="sentence">{{ pane.sentence }}</h2>
          {% if pane.description %}<p class="description">{{ pane.description }}</p>{% endif %}
          <p class="detail-meta"><span class="project-pill">{{ pane.project }}</span><span>{{ pane.date }}</span><span>{{ pane.session_short }}</span>{% if pane.outcome_badge %}<span class="outcome-badge {{ pane.outcome_slug }}">{{ pane.outcome_badge }}</span>{% endif %}{% if pane.unscanned %}<span class="warn-chip" title="transcript not scanned for instrumentation">&#9888; not scanned</span>{% endif %}</p>
        </header>
        <div class="stage-rail">
          {% for stage in pane.stages %}
          <details class="stage-card {{ stage.kind }}" data-stage="{{ stage.id }}" open>
            <summary><span class="stage-glyph">{{ stage.glyph }}</span><span class="stage-label">{{ stage.label }}</span></summary>
            <div class="stage-body">
              {% if stage.kind == "ask" %}
                {% if stage.quote %}<p class="quote">&ldquo;{{ stage.quote }}&rdquo;</p>{% endif %}
              {% elif stage.kind == "deliberation" %}
                {% if stage.narrative %}<p class="narrative">{{ stage.narrative }}</p>{% endif %}
                {% if stage.investigated %}
                <ul class="file-list">
                  {% for file in stage.investigated %}<li>{{ file }}</li>{% endfor %}
                  {% if stage.investigated_more %}<li class="more">&#9656; +{{ stage.investigated_more }} more</li>{% endif %}
                </ul>
                {% endif %}
              {% elif stage.kind == "steer" %}
                <ul class="task-list">
                  {% for todo in stage.todos %}<li class="task {{ todo.slug }}"><span class="glyph">{{ todo.glyph }}</span>{{ todo.content }}</li>{% endfor %}
                </ul>
                {% if stage.steer_count %}
                <p class="steer-count">{{ stage.steer_count }} mid-flight steer{% if stage.steer_count != 1 %}s{% endif %}</p>
                {% for steer in stage.steers %}<p class="steer-line">&#8627; turn {{ steer.turn }} &ldquo;{{ steer.text }}&rdquo;</p>{% endfor %}
                {% endif %}
              {% elif stage.kind == "outcome" %}
                {% if stage.outcome_stats %}<p class="outcome-stats">{{ stage.outcome_stats }}</p>{% endif %}
                {% if stage.error_line %}<p class="outcome-errors">{{ stage.error_line }}</p>{% endif %}
                {% if stage.top_error %}<p class="top-error">&#9656; top error {{ stage.top_error }}</p>{% endif %}
                {% if stage.outcome_text %}<p class="outcome-text">{{ stage.outcome_text }}</p>{% endif %}
                {% if stage.still_open %}<p class="still-open">{{ stage.still_open }}</p>{% endif %}
              {% endif %}
            </div>
          </details>
          {% endfor %}
        </div>
        {% if pane.artifacts %}
        <div class="artifact-bento" aria-label="Session artifacts">
        {% for artifact in pane.artifacts %}
          <div class="artifact-tile">
            <span class="symbol">{{ artifact.label }}</span>
            <span class="file">{{ artifact.file }}</span>
            <span class="receipt">{{ artifact.receipt_line }}</span>
          </div>
        {% endfor %}
        </div>
        {% if pane.artifacts_more %}<p class="artifact-more">&#9656; +{{ pane.artifacts_more }} more artifacts</p>{% endif %}
        {% endif %}
        <script type="application/json" class="episode-data">{{ pane.episode_json | safe }}</script>
      </article>
      {% endfor %}
    </div>
  </section>
  <p class="omission-summary">
    {% if data.older_omitted %}<span>{{ data.older_omitted }} older sessions omitted</span>{% endif %}
    {% if data.omitted %}<span>{{ data.omitted }} sessions omitted — no prompt, no artifact, no episode</span>{% endif %}
  </p>"#;

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

// Q7: the dream-cycle verdict totals (obsolete/superseded/reinstated/total)
// are witness-ledger telemetry, not session telemetry — they no longer sit
// above the session index. A compact form lives in the hero meta line
// (STORY_HERO_META below); the full four-up grid moves here, into the
// empty state that shows when there is no session signal to render at all.
const STORY_EMPTY_TEMPLATE: &str = r#"  <div class="empty-state">
    <span class="glyph">☾</span>
    No session has enough story signal to render yet.
  </div>
  <div class="totals">
    <div class="stat obsolete">
      <span class="n">{{ data.totals.obsolete }}</span>
      <span class="label">Obsolete</span>
    </div>
    <div class="stat superseded">
      <span class="n">{{ data.totals.superseded }}</span>
      <span class="label">Superseded</span>
    </div>
    <div class="stat reinstated">
      <span class="n">{{ data.totals.reinstated }}</span>
      <span class="label">Reinstated</span>
    </div>
    <div class="stat">
      <span class="n">{{ data.totals.total }}</span>
      <span class="label">Total events</span>
    </div>
  </div>
  <p class="omission-summary">
    {% if data.omitted %}<span>{{ data.omitted }} sessions omitted</span>{% endif %}
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
      <span class="totals-inline">{{ data.totals.total }} witness events · {{ data.totals.obsolete }} obsolete · {{ data.totals.superseded }} superseded · {{ data.totals.reinstated }} reinstated</span>
    </div>"#;

const MAILBOX_CSS: &str = r#"
  .mailbox {
    display: grid; grid-template-columns: minmax(260px, 35%) minmax(0, 65%);
    gap: 0; border: 1px solid var(--border); border-radius: 12px; overflow: hidden;
    align-items: stretch;
  }
  .index-pane {
    border-right: 1px solid var(--border); background: var(--bg-card);
    display: flex; flex-direction: column; min-width: 0; overflow-x: hidden;
  }
  .sort-controls {
    display: flex; align-items: center; gap: 0.4rem; padding: 0.75rem 0.9rem;
    border-bottom: 1px solid var(--border); font-size: 0.75rem;
  }
  .sort-label { color: var(--fg-muted); text-transform: uppercase; letter-spacing: 0.05em; font-size: 0.68rem; }
  .sort-btn {
    background: none; border: 1px solid var(--border); border-radius: 999px;
    color: var(--fg-muted); font: inherit; font-size: 0.72rem; padding: 0.2rem 0.6rem;
    cursor: pointer;
  }
  .sort-btn.active { color: var(--accent); border-color: var(--accent); font-weight: 700; }
  .session-rows { padding: 0.4rem 0; overflow-x: hidden; }
  .group-header {
    margin: 0.8rem 0.9rem 0.35rem; font-size: 0.68rem; font-weight: 700;
    color: var(--fg-muted); text-transform: uppercase; letter-spacing: 0.06em;
  }
  .index-row {
    display: block; width: 100%; text-align: left; background: none; border: none;
    border-left: 3px solid transparent; padding: 0.55rem 0.9rem; cursor: pointer;
    color: inherit; font: inherit;
  }
  .index-row:hover { background: var(--bg); }
  .index-row.selected { background: var(--bg); border-left-color: var(--accent); }
  .row-head { display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.15rem; }
  .index-row .glyph { font-weight: 800; flex: none; }
  .index-row .glyph.success { color: var(--green); }
  .index-row .glyph.partial { color: var(--amber); }
  .index-row .glyph.failed { color: var(--red); }
  .index-row .glyph.noted { color: var(--fg-muted); }
  .index-row .project-pill {
    color: var(--fg-muted); font-size: 0.68rem; font-weight: 700; letter-spacing: 0.02em;
  }
  .index-row .sentence {
    display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical;
    overflow: hidden; overflow-wrap: anywhere; font-size: 0.86rem; line-height: 1.35; font-weight: 550;
  }
  .index-row .instrumentation {
    display: block; margin-top: 0.25rem; font-size: 0.7rem; color: var(--fg-muted);
    overflow-wrap: anywhere;
  }
  .thin-rollup { border-top: 1px solid var(--border); margin: 0.6rem 0.9rem 0.9rem; }
  .thin-rollup-header {
    font-size: 0.7rem; color: var(--fg-muted); font-weight: 700; margin: 0.6rem 0 0.4rem;
  }
  .thin-group { border: 1px solid var(--border); border-radius: 8px; margin-bottom: 0.4rem; padding: 0 0.6rem; }
  .thin-group summary {
    display: flex; justify-content: space-between; gap: 0.5rem; cursor: pointer;
    padding: 0.4rem 0; font-size: 0.75rem;
  }
  .thin-count { color: var(--fg-muted); }
  .thin-row {
    display: flex; gap: 0.5rem; font-size: 0.72rem; color: var(--fg-muted);
    margin: 0 0 0.35rem; overflow-wrap: anywhere;
  }
  .thin-time { flex: none; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
  .detail-pane-wrap { min-width: 0; padding: 1rem 1.1rem; overflow-x: hidden; }
  .detail-pane .detail-head { margin-bottom: 0.9rem; }
  .detail-pane .sentence { font-size: 1.1rem; margin: 0 0 0.3rem; overflow-wrap: anywhere; }
  .detail-pane .description { font-size: 0.82rem; color: var(--fg-muted); margin: 0 0 0.5rem; overflow-wrap: anywhere; }
  .detail-meta { display: flex; align-items: center; gap: 0.5rem; color: var(--fg-muted); font-size: 0.75rem; flex-wrap: wrap; }
  .detail-meta .project-pill { border: 1px solid var(--border); border-radius: 999px; padding: 0.05rem 0.5rem; font-weight: 700; }
  .outcome-badge {
    border-radius: 999px; font-size: 0.62rem; font-weight: 800; padding: 0.1rem 0.45rem;
    text-transform: uppercase; letter-spacing: 0.04em;
  }
  .outcome-badge.success { color: var(--green); background: var(--green-bg); }
  .outcome-badge.partial { color: var(--amber); background: var(--amber-bg); }
  .outcome-badge.failed { color: var(--red); background: var(--red-bg); }
  .outcome-badge.noted { color: var(--fg-muted); border: 1px solid var(--border); }
  .warn-chip {
    color: var(--amber); border: 1px solid var(--amber); border-radius: 999px;
    padding: 0.05rem 0.5rem; font-size: 0.68rem; font-weight: 700;
  }
  .chip.degraded { border-bottom: 1px dotted var(--amber); }
  .steer-count { font-weight: 700; margin: 0.5rem 0 0.2rem; }
  .steer-line { margin: 0 0 0.2rem; color: var(--fg-muted); overflow-wrap: anywhere; }
  .outcome-errors { font-weight: 700; margin: 0 0 0.35rem; }
  .top-error { color: var(--fg-muted); margin: 0 0 0.35rem; overflow-wrap: anywhere; }
  .still-open { color: var(--fg-muted); margin: 0.35rem 0 0; overflow-wrap: anywhere; }
  .stage-rail { position: relative; padding-left: 1.9rem; margin-bottom: 1rem; }
  .stage-rail::before {
    content: ""; position: absolute; left: 0.7rem; top: 0.6rem; bottom: 0.6rem;
    width: 2px; background: var(--border);
  }
  .stage-card {
    position: relative; background: var(--bg-card); border: 1px solid var(--border);
    border-radius: 10px; margin-bottom: 0.65rem; overflow: visible; min-width: 0;
  }
  .stage-card > summary {
    list-style: none; cursor: pointer; padding: 0.55rem 0.8rem;
    display: flex; align-items: center; gap: 0.5rem;
  }
  .stage-card > summary::-webkit-details-marker { display: none; }
  .stage-glyph {
    position: absolute; left: -1.9rem; width: 1.3rem; height: 1.3rem; border-radius: 50%;
    background: var(--bg-card); border: 1px solid var(--border); display: flex;
    align-items: center; justify-content: center; font-size: 0.7rem; font-weight: 800;
  }
  .stage-label { font-size: 0.72rem; font-weight: 800; letter-spacing: 0.06em; text-transform: uppercase; color: var(--fg-muted); }
  .stage-body { padding: 0 0.85rem 0.75rem; font-size: 0.82rem; line-height: 1.5; overflow-wrap: anywhere; }
  .stage-body .quote { font-style: italic; margin: 0; }
  .stage-body .narrative { margin: 0 0 0.5rem; }
  .file-list, .task-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 0.3rem; }
  .file-list li, .task-list li { font-size: 0.8rem; overflow-wrap: anywhere; }
  .file-list li.more { color: var(--fg-muted); }
  .task { display: flex; gap: 0.45rem; }
  .task .glyph { font-weight: 800; flex: none; width: 1.1em; text-align: center; }
  .task.done .glyph { color: var(--green); }
  .task.doing .glyph { color: var(--amber); }
  .task.open .glyph { color: var(--fg-muted); }
  .task.unknown .glyph { color: var(--fg-muted); }
  .task.done { color: var(--fg-muted); }
  .outcome-stats { font-weight: 700; margin: 0 0 0.35rem; }
  .outcome-text { margin: 0; }
  .artifact-bento {
    display: grid; gap: 0.6rem; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr));
  }
  .artifact-tile {
    border: 1px solid var(--border); border-radius: 8px; background: var(--bg);
    padding: 0.55rem 0.65rem; display: grid; gap: 0.15rem; min-width: 0;
  }
  .artifact-tile .symbol { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-weight: 650; overflow-wrap: anywhere; }
  .artifact-tile .file { color: var(--fg-muted); font-size: 0.76rem; overflow-wrap: anywhere; }
  .artifact-tile .receipt { font-size: 0.7rem; color: var(--fg-muted); }
  .artifact-more { font-size: 0.72rem; color: var(--fg-muted); margin: 0.4rem 0 0; }
  .chip-popover {
    position: fixed; z-index: 1000; background: var(--bg-card); color: var(--fg);
    border: 1px solid var(--border); border-radius: 8px; box-shadow: 0 12px 32px rgba(0, 0, 0, 0.18);
    padding: 0.5rem 0.65rem; font-size: 0.76rem; max-width: min(320px, calc(100vw - 24px));
    pointer-events: none; overflow-wrap: anywhere;
  }
  [data-popover] { cursor: default; border-bottom: 1px dotted var(--fg-muted); }
  [hidden] { display: none !important; }
  .totals-inline { overflow-wrap: anywhere; }
  .omission-summary { display: flex; justify-content: center; gap: 1rem; color: var(--fg-muted); font-size: 0.75rem; margin-top: 1rem; }
  @media (max-width: 760px) {
    .mailbox { grid-template-columns: 1fr; }
    .index-pane { border-right: none; border-bottom: 1px solid var(--border); }
    .stage-rail { padding-left: 1.6rem; }
    .stage-glyph { left: -1.6rem; }
  }
"#;

const MAILBOX_SCRIPT: &str = r#"
(() => {
  const indexRows = Array.from(document.querySelectorAll(".index-row"));
  const detailPanes = Array.from(document.querySelectorAll(".detail-pane"));
  const popover = document.querySelector(".chip-popover");
  // The server renders one pane visible with no hash in the URL at all —
  // capture which session that is now, before any click can move it, so
  // Back can return to it even though "no hash" is not itself a session id.
  const defaultSession = detailPanes.find(pane => !pane.hidden)?.dataset.session;

  // --- selection / pane swap -----------------------------------------
  const selectSession = (id, updateHash) => {
    if (!id) return;
    let matched = false;
    detailPanes.forEach(pane => {
      const isMatch = pane.dataset.session === id;
      pane.hidden = !isMatch;
      if (isMatch) matched = true;
    });
    if (!matched) return;
    indexRows.forEach(row => {
      const isMatch = row.dataset.session === id;
      row.classList.toggle("selected", isMatch);
      row.setAttribute("aria-selected", isMatch ? "true" : "false");
    });
    if (updateHash !== false) location.hash = id;
  };

  indexRows.forEach(row => {
    row.addEventListener("click", () => selectSession(row.dataset.session));
  });

  // Back past the first click lands on the pre-click, no-hash history
  // entry — restore the server's default selection there instead of
  // no-op'ing on an empty id.
  window.addEventListener("hashchange", () => {
    selectSession(location.hash.slice(1) || defaultSession, false);
  });

  if (location.hash) {
    selectSession(location.hash.slice(1), false);
  }

  // --- sort -------------------------------------------------------------
  const container = document.querySelector(".session-rows");
  const OUTCOME_ORDER = ["success", "partial", "failed", "noted"];
  const OUTCOME_LABEL = { success: "SUCCESS", partial: "PARTIAL", failed: "FAILED", noted: "NOTED" };

  const dayLabel = (dayStr, today) => {
    const parts = dayStr.split("-").map(Number);
    const date = new Date(Date.UTC(parts[0], parts[1] - 1, parts[2]));
    const diffDays = Math.round((today - date) / 86400000);
    const month = date.toLocaleString("en-US", { month: "short", timeZone: "UTC" }).toUpperCase();
    const formatted = `${date.getUTCDate()} ${month}`;
    if (diffDays === 0) return `TODAY · ${formatted}`;
    if (diffDays === 1) return `YESTERDAY · ${formatted}`;
    return formatted;
  };

  const headerFor = (row, mode, today) => {
    if (mode === "recency") return dayLabel(row.dataset.day, today);
    if (mode === "group") return row.dataset.project;
    return OUTCOME_LABEL[row.dataset.outcome] || row.dataset.outcome.toUpperCase();
  };

  const order = (mode) => {
    if (mode === "recency") return indexRows.slice();
    if (mode === "group") {
      const seen = [];
      indexRows.forEach(row => {
        if (!seen.includes(row.dataset.project)) seen.push(row.dataset.project);
      });
      return seen.flatMap(project => indexRows.filter(row => row.dataset.project === project));
    }
    return OUTCOME_ORDER.flatMap(slug => indexRows.filter(row => row.dataset.outcome === slug));
  };

  const render = (mode) => {
    const ordered = order(mode);
    const today = (() => {
      const now = new Date();
      return new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()));
    })();
    container.innerHTML = "";
    let lastHeader = null;
    ordered.forEach(row => {
      const header = headerFor(row, mode, today);
      if (header !== lastHeader) {
        const heading = document.createElement("p");
        heading.className = "group-header";
        heading.textContent = header;
        container.append(heading);
        lastHeader = header;
      }
      container.append(row);
    });
  };

  document.querySelectorAll(".sort-btn").forEach(btn => {
    btn.addEventListener("click", () => {
      document.querySelectorAll(".sort-btn").forEach(other => other.classList.toggle("active", other === btn));
      render(btn.dataset.sort);
    });
  });

  // --- stage card pin (aria sync on top of native <details>) ------------
  document.querySelectorAll(".stage-card").forEach(card => {
    const sync = () => card.setAttribute("aria-expanded", card.open ? "true" : "false");
    card.addEventListener("toggle", sync);
    sync();
  });

  // --- instrumentation chip popover --------------------------------------
  const positionPopover = (event) => {
    const gap = 14;
    const left = Math.max(12, Math.min(event.clientX + gap, window.innerWidth - popover.offsetWidth - 12));
    const top = Math.max(12, Math.min(event.clientY + gap, window.innerHeight - popover.offsetHeight - 12));
    popover.style.left = `${left}px`;
    popover.style.top = `${top}px`;
  };
  document.querySelectorAll("[data-popover]").forEach(chip => {
    chip.addEventListener("pointerenter", event => {
      popover.textContent = chip.dataset.popover;
      popover.hidden = false;
      positionPopover(event);
    });
    chip.addEventListener("pointermove", positionPopover);
    chip.addEventListener("pointerleave", () => { popover.hidden = true; });
  });
})();
"#;

const MAX_STORY_CARDS: usize = 50;

#[derive(Debug, Clone, Serialize)]
struct StoryArtifactView {
    label: String,
    file: String,
    /// Always present, never omitted: "⌗ superseded <oid8>" or the honest
    /// "no receipt" — an artifact tile with a silently missing receipt line
    /// would read as "not yet checked" when it actually means "checked, and
    /// there is none."
    receipt_line: String,
}

#[derive(Debug, Clone, Serialize)]
struct StoryTodoView {
    glyph: &'static str,
    slug: &'static str,
    content: String,
}

#[derive(Debug, Clone, Serialize)]
struct SegmentView {
    /// e.g. "✓1 ○1 tasks", "2 artifacts", "3 files", "12 errors", "~3 errors".
    text: String,
    /// Short preview shown on hover; empty means the chip carries no popover.
    popover: String,
    /// Native `title` attribute — used for the degraded-fallback disclosure
    /// ("pattern-matched, not tool-verified") and the R8 "(parent only)"
    /// note. `None` means no `title` attribute is rendered at all.
    title: Option<String>,
    /// True for the tilde-marked degraded (`error_signatures.len()`)
    /// fallback — never a tool-verified count. Kept as an explicit flag
    /// (rather than inferring degradation from the `~` in `text`) so the
    /// template and any future consumer never have to string-sniff it.
    degraded: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SteerLineView {
    turn: u32,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
struct StageCardView {
    id: &'static str,
    label: &'static str,
    glyph: String,
    /// "ask" | "deliberation" | "steer" | "outcome" — the template
    /// discriminator; also the CSS hook.
    kind: &'static str,
    quote: Option<String>,
    narrative: Option<String>,
    investigated: Vec<String>,
    investigated_more: usize,
    todos: Vec<StoryTodoView>,
    outcome_text: Option<String>,
    outcome_stats: Option<String>,
    /// STEER stage only. `None` -> no count line at all; `Some(0)` also
    /// renders no count line (plan §3.3b: a session with zero steers is a
    /// real fact, but it earns no line in this phase) — `Some(n>0)` renders
    /// "N mid-flight steers" plus up to 3 `steers` children.
    steer_count: Option<u32>,
    steers: Vec<SteerLineView>,
    /// OUTCOME stage only: tool-verified/degraded error count line and the
    /// single highest-byte-size error's "tool: preview".
    error_line: Option<String>,
    top_error: Option<String>,
    /// OUTCOME stage only: `next_steps`/`blockers`, fused into one trailing
    /// "still open" line.
    still_open: Option<String>,
}

/// Intermediate, curation-facing representation of one rich session. This is
/// the type `curate_sentences` mutates (`summary`/`description`) — it is
/// deliberately unchanged in shape from the pre-mailbox renderer so the
/// caching/curation code below needs no edits. `index_row`/`detail_pane`
/// project it into the view types the template actually walks.
#[derive(Debug, Clone)]
struct StoryCardView {
    /// The mailbox's one-line fused ask->outcome sentence. Index row and
    /// detail-pane header both render this; nothing else.
    summary: String,
    /// Written for the `journal_headlines` cache-hit predicate
    /// (`description != ''`); rendered as the detail-pane subtitle under
    /// `summary`, omitted entirely when empty. Never shown in the index.
    description: String,
    project: String,
    date: String,
    timestamp: String,
    session_short: String,
    /// Full session id — never displayed (the mailbox only ever shows
    /// `session_short`), kept so the report-time instrumentation backfill
    /// can resolve this card's transcript file and correlate the
    /// `session_instrumentation` cache row.
    session_id: String,
    outcome_slug: &'static str,
    outcome_badge: Option<String>,
    request_text: Option<String>,
    narrative: Option<String>,
    investigated: Vec<String>,
    /// `investigated` ∪ `files_modified`, deduped — the index row's `F
    /// files` count (plan §6 Phase 2). The DELIBERATION stage's file list
    /// keeps showing `investigated` alone; this union only feeds the count.
    files_touched: Vec<String>,
    todos: Vec<StoryTodoView>,
    outcome_text: Option<String>,
    completed: Option<String>,
    next_steps: Option<String>,
    blockers: Option<String>,
    artifacts: Vec<StoryArtifactView>,
    /// Hash payload input only in Phase 1 — no longer rendered as a page
    /// script tag inline in the index (still embedded once per detail pane,
    /// see `DetailPaneView::episode_json`).
    episode_json: String,
    /// The pre-existing, over/under-counting substring-matched fallback
    /// (see `transcript::instrumentation` module doc). Eligible as the
    /// degraded error segment only when non-empty AND `instrumentation` is
    /// still `None` after the backfill runs.
    error_signatures: Vec<String>,
    /// Resolution order steps 1-2 from `StorySession` (episode/cache),
    /// mutated in place by the report-time backfill (step 3, live scan) if
    /// still `None` when the backfill runs.
    instrumentation: Option<SessionInstrumentation>,
    /// Set by the backfill when a transcript was located on disk but did
    /// not end up contributing measured instrumentation (oversized, budget
    /// exhausted, kill switch, or a read/parse failure) — drives the "⚠ not
    /// scanned" detail-header chip. Never set when no transcript resolves
    /// at all (plan §3.4: that case shows no chip, only an absent segment).
    unscanned: bool,
    /// Set by the backfill when the resolved transcript's directory has a
    /// `<session>/subagents/agent-*.jsonl` sibling — the parent-only
    /// disclosure required by plan §8 R8.
    has_subagents: bool,
}

#[derive(Debug, Clone, Serialize)]
struct IndexRowView {
    session_short: String,
    glyph: &'static str,
    outcome_slug: &'static str,
    project: String,
    sentence: String,
    segments: Vec<SegmentView>,
    /// Full ISO timestamp — sort key for `recency`.
    ts: String,
    /// `YYYY-MM-DD` — sort/group key for the client-side re-sort.
    day: String,
    selected: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DetailPaneView {
    session_short: String,
    sentence: String,
    /// Detail-pane subtitle under `sentence` (plan §5.2); empty means the
    /// template omits the element entirely rather than rendering an empty
    /// `<p>`.
    description: String,
    project: String,
    date: String,
    outcome_slug: &'static str,
    outcome_badge: Option<String>,
    stages: Vec<StageCardView>,
    artifacts: Vec<StoryArtifactView>,
    artifacts_more: usize,
    episode_json: String,
    /// Plan §3.4 / §6 Phase 2: transcript located but did not contribute
    /// measured instrumentation (oversized, budget-exhausted, kill switch,
    /// or a read/parse failure).
    unscanned: bool,
}

#[derive(Debug, Clone, Serialize)]
struct DayGroupView {
    label: String,
    rows: Vec<IndexRowView>,
}

#[derive(Debug, Clone, Serialize)]
struct ThinRowView {
    time: String,
    excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
struct ThinGroupView {
    project: String,
    count: usize,
    rows: Vec<ThinRowView>,
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

/// `●` success / `◐` partial / `✗` failed / `○` noted — the same glyph used
/// for the index-row status marker and the OUTCOME stage's rail node.
fn outcome_glyph(slug: &str) -> &'static str {
    match slug {
        "success" => "●",
        "partial" => "◐",
        "failed" => "✗",
        _ => "○",
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
    day_groups: Vec<DayGroupView>,
    detail_panes: Vec<DetailPaneView>,
    thin_groups: Vec<ThinGroupView>,
    thin_total: usize,
    /// Sessions with some signal but not enough to be RICH: they never went
    /// through curation, only counted and grouped into `thin_groups`.
    omitted: usize,
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

fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
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

/// Map a free-text todo status onto a checklist glyph slug.
fn todo_glyph(status: &str) -> (&'static str, &'static str) {
    let lower = status.to_lowercase();
    if lower.contains("complete") || lower.contains("done") {
        ("done", "✓")
    } else if lower.contains("progress") || lower.contains("doing") {
        ("doing", "→")
    } else if lower.contains("pending") || lower.contains("open") || lower.contains("todo") {
        ("open", "○")
    } else {
        ("unknown", "?")
    }
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

fn iso_date(timestamp: &str) -> String {
    timestamp.get(..10).unwrap_or(timestamp).to_string()
}

fn iso_time(timestamp: &str) -> String {
    timestamp.get(11..16).unwrap_or("").to_string()
}

/// `TODAY · 10 AUG` / `YESTERDAY · 9 AUG` / `8 AUG` — Q3's default grouping
/// key (calendar day). Uses the UTC calendar day: the binary has no reliable
/// local-timezone source to draw on, so "local time" per Q3 degrades to UTC
/// rather than guessing an offset.
fn day_label(date_str: &str, today: chrono::NaiveDate) -> String {
    let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
        return date_str.to_string();
    };
    let formatted = format!("{} {}", date.format("%-d"), date.format("%b")).to_uppercase();
    if date == today {
        format!("TODAY · {formatted}")
    } else if date == today - chrono::Duration::days(1) {
        format!("YESTERDAY · {formatted}")
    } else {
        formatted
    }
}

/// Q2's default RICH/THIN/OMITTED split: RICH needs an ask (request or
/// registry `first_prompt`) AND at least one of (narrative, outcome,
/// artifacts). Everything else with any signal at all is THIN — including
/// registry-only sessions, which the old tier system silently dropped.
/// Everything with zero signal is OMITTED (counted, never rendered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionClass {
    Rich,
    Thin,
    Omitted,
}

fn has_text(value: &Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty())
}

fn classify_session(session: &StorySession) -> SessionClass {
    let request = session.request.as_deref().map(strip_scaffold);
    let first_prompt = session.first_prompt.as_deref().map(strip_scaffold);
    let narrative = session.narrative.as_deref().map(strip_scaffold);
    let outcome = session.outcome.as_deref().map(strip_scaffold);
    let has_ask = has_text(&request) || has_text(&first_prompt);
    let has_narrative = has_text(&narrative);
    let has_outcome = has_text(&outcome);
    let has_artifacts = !session.artifacts.is_empty();

    if has_ask && (has_narrative || has_outcome || has_artifacts) {
        return SessionClass::Rich;
    }
    let has_any_signal = has_ask
        || has_narrative
        || has_outcome
        || has_artifacts
        || !session.investigated.is_empty()
        || !session.todos.is_empty();
    if has_any_signal {
        SessionClass::Thin
    } else {
        SessionClass::Omitted
    }
}

/// Build the curation-facing card for a session already classified RICH.
fn build_story_card(session: &StorySession) -> StoryCardView {
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
    let input_text = request.or_else(|| {
        session
            .first_prompt
            .as_deref()
            .map(strip_scaffold)
            .filter(|value| !value.trim().is_empty())
    });

    let artifacts: Vec<StoryArtifactView> = session
        .artifacts
        .iter()
        .map(|artifact| {
            let receipt_line = artifact
                .superseded_receipt
                .as_deref()
                .map(|receipt| format!("⌗ superseded {}", short_oid(receipt)))
                .unwrap_or_else(|| "no receipt".to_string());
            StoryArtifactView {
                label: artifact
                    .symbol
                    .clone()
                    .unwrap_or_else(|| basename(&artifact.file)),
                file: artifact.file.clone(),
                receipt_line,
            }
        })
        .collect();

    let todos: Vec<StoryTodoView> = session
        .todos
        .iter()
        .map(|todo| {
            let (slug, glyph) = todo_glyph(&todo.status);
            StoryTodoView {
                glyph,
                slug,
                content: truncate_chars(&plain_text(&todo.content), 140),
            }
        })
        .collect();

    // A handful of sessions in the maintainer's own corpus carry 100+
    // artifacts or 150+ investigated files (a hot symbol touched across many
    // sessions, or a long-running refactor session). The bento/DELIBERATION
    // stage already cap what they *display* (see `ARTIFACT_CAP` and
    // `INVESTIGATED_CAP` below) — this JSON blob is debug-only payload never
    // read by Phase 1's JS, so it is capped the same way rather than
    // embedding hundreds of entries nothing on the page shows.
    let episode_json = json_for_html_script(&EpisodeDataView {
        request: input_text,
        narrative,
        investigated: &session.investigated
            [..session.investigated.len().min(EPISODE_JSON_LIST_CAP)],
        artifacts: &session.artifacts[..session.artifacts.len().min(EPISODE_JSON_LIST_CAP)],
        outcome,
        todos: &session.todos,
    });

    let (outcome_slug, outcome_badge) = outcome_badge(outcome);

    // Fused ask->outcome sentence (plan §5.1/§5.2): the deterministic,
    // zero-AI baseline that AI curation overwrites when its reply agrees
    // with the outcome slug. Never wrong about the outcome — it is what
    // every card shows when curation is off, fails, or its reply's first
    // word disagrees with `outcome_slug` (`verb_agrees`).
    let summary = fallback_sentence(outcome_slug, narrative.or(input_text));

    // Deterministic subtitle: the narrative (or request) *beyond* what the
    // fused sentence's first clause already says — empty for the common
    // single-clause ask ("fix the flaky test"), non-empty only when there is
    // a genuine second sentence of detail. Written unconditionally (even
    // when empty) so a cached row still hits (`description != ''`, see
    // `cached_sentence`); the detail pane renders it as the subtitle under
    // the fused sentence, omitting the element entirely when it is empty.
    let description = {
        let source = narrative.or(input_text).map(plain_text).unwrap_or_default();
        let head = first_sentence(&source);
        let remainder = source
            .strip_prefix(head.as_str())
            .unwrap_or(&source)
            .trim_start();
        truncate_chars(remainder, 260)
    };

    // Index row `F files` count (plan §6 Phase 2): investigated ∪
    // files_modified, deduped. The DELIBERATION stage keeps showing
    // `investigated` alone — this union only feeds the count/popover.
    let mut files_touched = session.investigated.clone();
    for file in &session.files_modified {
        if !file.trim().is_empty() && !files_touched.contains(file) {
            files_touched.push(file.clone());
        }
    }

    let completed = session
        .completed
        .as_deref()
        .map(strip_scaffold)
        .filter(|value| !value.trim().is_empty())
        .map(|text| truncate_chars(&plain_text(text), 320));
    let next_steps = session
        .next_steps
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|text| truncate_chars(&plain_text(text), 200));
    let blockers = session
        .blockers
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(|text| truncate_chars(&plain_text(text), 200));

    StoryCardView {
        summary,
        description,
        project: if session.project.is_empty() {
            "unknown".to_string()
        } else {
            session.project.clone()
        },
        date: iso_date(&session.timestamp),
        timestamp: session.timestamp.clone(),
        session_short: short_oid(&session.session_id),
        session_id: session.session_id.clone(),
        outcome_slug,
        outcome_badge,
        request_text: input_text.map(|text| truncate_chars(&plain_text(text), 240)),
        narrative: narrative.map(|text| truncate_chars(&plain_text(text), 500)),
        investigated: session.investigated.clone(),
        files_touched,
        todos,
        outcome_text: outcome.map(|text| truncate_chars(&plain_text(text), 320)),
        completed,
        next_steps,
        blockers,
        artifacts,
        episode_json,
        error_signatures: session.error_signatures.clone(),
        instrumentation: session.instrumentation.clone(),
        unscanned: false,
        has_subagents: false,
    }
}

/// A session with only artifacts/registry signal — never curated (§5.3),
/// shown only as one compact line in the thin rollup.
fn build_thin_entry(session: &StorySession) -> (String, String, ThinRowView) {
    let project = if session.project.is_empty() {
        "unknown".to_string()
    } else {
        session.project.clone()
    };
    let request = session
        .request
        .as_deref()
        .map(strip_scaffold)
        .filter(|value| !value.trim().is_empty());
    let first_prompt = session
        .first_prompt
        .as_deref()
        .map(strip_scaffold)
        .filter(|value| !value.trim().is_empty());
    let excerpt = if let Some(text) = request.or(first_prompt) {
        format!("\"{}\"", truncate_chars(&plain_text(text), 60))
    } else if !session.artifacts.is_empty() {
        pluralize(session.artifacts.len(), "artifact")
    } else {
        "(no evidence)".to_string()
    };
    (
        project,
        session.timestamp.clone(),
        ThinRowView {
            time: iso_time(&session.timestamp),
            excerpt,
        },
    )
}

const INVESTIGATED_CAP: usize = 4;
const CHIP_PREVIEW_CAP: usize = 5;
/// Cap on artifact tiles drawn in the detail pane's bento — a hot symbol
/// touched across many sessions can carry 100+ artifacts, which would
/// otherwise render as an unreadable wall of tiles. The instrumentation
/// line's `K artifacts` count (computed from the uncapped list) still
/// reports the true total; this only bounds what the bento *draws*, with an
/// honest "+N more" note for the remainder — never a silent drop.
const ARTIFACT_CAP: usize = 24;
/// Cap on the per-session debug JSON blob's investigated/artifacts arrays.
/// Nothing in Phase 1's JS reads this script tag; it exists for future
/// reuse and for the hostile-payload regression test, so it is capped to
/// the same order of magnitude as what the page actually displays rather
/// than embedding hundreds of entries no UI surface shows.
const EPISODE_JSON_LIST_CAP: usize = 20;

/// Resolved error evidence for one card, computed once the backfill has had
/// its chance to fill `instrumentation` (plan §3.3a resolution order, final
/// two branches). `Absent` — never a segment/line at all — for a session
/// with neither a tool-verified pass nor any pattern-matched
/// `error_signatures` (an episode that never carried the field defaults to
/// the same empty `Vec` as one that genuinely found zero matches, so an
/// empty `error_signatures` is never distinguishable from "not collected"
/// and must never render `~0 errors`).
enum ErrorDisplay {
    Absent,
    Measured { count: u32, top: Vec<ErrorEvent> },
    Degraded { count: u32 },
}

fn error_display(card: &StoryCardView) -> ErrorDisplay {
    if let Some(instrumentation) = &card.instrumentation {
        return ErrorDisplay::Measured {
            count: instrumentation.error_count,
            top: instrumentation.top_errors.clone(),
        };
    }
    if !card.error_signatures.is_empty() {
        return ErrorDisplay::Degraded {
            count: card.error_signatures.len() as u32,
        };
    }
    ErrorDisplay::Absent
}

/// A short, single/no-space outcome phrase ("partial", "done") that reads
/// as a bare slug rather than real prose — the OUTCOME card falls back to
/// `completed` text as its body in this case (plan §6 Phase 2).
fn is_bare_slug(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed.split_whitespace().count() <= 1 && trimmed.chars().count() <= 24
}

/// Only the stages with real evidence are drawn — a session with no todos
/// and no steers gets a shorter rail, never a padded-out four-node one.
fn build_stage_cards(card: &StoryCardView) -> Vec<StageCardView> {
    let mut stages = Vec::with_capacity(4);

    if let Some(quote) = &card.request_text {
        stages.push(StageCardView {
            id: "S_INPUT",
            label: "ASK",
            glyph: "●".to_string(),
            kind: "ask",
            quote: Some(quote.clone()),
            narrative: None,
            investigated: Vec::new(),
            investigated_more: 0,
            todos: Vec::new(),
            outcome_text: None,
            outcome_stats: None,
            steer_count: None,
            steers: Vec::new(),
            error_line: None,
            top_error: None,
            still_open: None,
        });
    }

    if card.narrative.is_some() || !card.investigated.is_empty() {
        let shown: Vec<String> = card
            .investigated
            .iter()
            .take(INVESTIGATED_CAP)
            .cloned()
            .collect();
        let more = card.investigated.len().saturating_sub(INVESTIGATED_CAP);
        stages.push(StageCardView {
            id: "S_DELIB",
            label: "DELIBERATION",
            glyph: "◆".to_string(),
            kind: "deliberation",
            quote: None,
            narrative: card.narrative.clone(),
            investigated: shown,
            investigated_more: more,
            todos: Vec::new(),
            outcome_text: None,
            outcome_stats: None,
            steer_count: None,
            steers: Vec::new(),
            error_line: None,
            top_error: None,
            still_open: None,
        });
    }

    let steer_count = card.instrumentation.as_ref().map(|i| i.steer_count);
    let has_steers = steer_count.is_some_and(|n| n > 0);
    if !card.todos.is_empty() || has_steers {
        let steers: Vec<SteerLineView> = card
            .instrumentation
            .as_ref()
            .map(|i| {
                i.steers
                    .iter()
                    .take(3)
                    .map(|s| SteerLineView {
                        turn: s.turn,
                        text: s.text.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        stages.push(StageCardView {
            id: "S_STEER",
            label: "STEER",
            glyph: "✎".to_string(),
            kind: "steer",
            quote: None,
            narrative: None,
            investigated: Vec::new(),
            investigated_more: 0,
            todos: card.todos.clone(),
            outcome_text: None,
            outcome_stats: None,
            steer_count,
            steers,
            error_line: None,
            top_error: None,
            still_open: None,
        });
    }

    if card.outcome_text.is_some() || !card.artifacts.is_empty() {
        let outcome_stats = (!card.todos.is_empty()).then(|| {
            let done = card.todos.iter().filter(|todo| todo.slug == "done").count();
            format!("{done} of {} tasks done", card.todos.len())
        });
        // Plan §6 Phase 2: when the outcome text is a bare slug ("partial",
        // "done"), show `completed`'s real prose as the body instead.
        let outcome_text = match &card.outcome_text {
            Some(text) if is_bare_slug(text) => {
                card.completed.clone().or(card.outcome_text.clone())
            }
            other => other.clone(),
        };
        let (error_line, top_error) = match error_display(card) {
            ErrorDisplay::Absent => (None, None),
            ErrorDisplay::Measured { count, top } => (
                Some(pluralize(count as usize, "error")),
                top.first()
                    .map(|error| format!("{}: {}", error.tool, error.preview)),
            ),
            ErrorDisplay::Degraded { count } => (
                Some(format!("~{}", pluralize(count as usize, "error"))),
                None,
            ),
        };
        let still_open = match (&card.next_steps, &card.blockers) {
            (Some(next), Some(blockers)) => Some(format!("{next} — blocked by: {blockers}")),
            (Some(next), None) => Some(next.clone()),
            (None, Some(blockers)) => Some(format!("Blocked by: {blockers}")),
            (None, None) => None,
        };
        stages.push(StageCardView {
            id: "S_OUT",
            label: "OUTCOME",
            glyph: outcome_glyph(card.outcome_slug).to_string(),
            kind: "outcome",
            quote: None,
            narrative: None,
            investigated: Vec::new(),
            investigated_more: 0,
            todos: Vec::new(),
            outcome_text,
            outcome_stats,
            steer_count: None,
            steers: Vec::new(),
            error_line,
            top_error,
            still_open,
        });
    }

    stages
}

/// The instrumentation line's segments: `✓N ○M tasks · K artifacts · F
/// files · E errors`. Every segment drops independently when its evidence is
/// absent — an empty todo list never renders `✓0 ○0`, an empty
/// artifact/file list never renders `0 artifacts`/`0 files`, and an
/// unmeasured error count renders no `errors` segment at all
/// (`ErrorDisplay::Absent`). A tool-verified `Some(0)` DOES render `0
/// errors` — a real measured zero is not the same as "never measured".
fn instrumentation_segments(
    todos: &[StoryTodoView],
    artifacts: &[StoryArtifactView],
    files_touched: &[String],
    error: &ErrorDisplay,
    has_subagents: bool,
) -> Vec<SegmentView> {
    let mut segments = Vec::with_capacity(4);
    if !todos.is_empty() {
        let done = todos.iter().filter(|todo| todo.slug == "done").count();
        let open = todos.len() - done;
        let popover = todos
            .iter()
            .take(CHIP_PREVIEW_CAP)
            .map(|todo| format!("{} {}", todo.glyph, todo.content))
            .collect::<Vec<_>>()
            .join(" • ");
        segments.push(SegmentView {
            text: format!("✓{done} ○{open} tasks"),
            popover,
            title: None,
            degraded: false,
        });
    }
    if !artifacts.is_empty() {
        let popover = artifacts
            .iter()
            .take(CHIP_PREVIEW_CAP)
            .map(|artifact| artifact.label.clone())
            .collect::<Vec<_>>()
            .join(" • ");
        segments.push(SegmentView {
            text: pluralize(artifacts.len(), "artifact"),
            popover,
            title: None,
            degraded: false,
        });
    }
    if !files_touched.is_empty() {
        let popover = files_touched
            .iter()
            .take(CHIP_PREVIEW_CAP)
            .map(|file| basename(file))
            .collect::<Vec<_>>()
            .join(" • ");
        segments.push(SegmentView {
            text: pluralize(files_touched.len(), "file"),
            popover,
            title: None,
            degraded: false,
        });
    }
    match error {
        ErrorDisplay::Absent => {}
        ErrorDisplay::Measured { count, top } => {
            let popover = top
                .iter()
                .take(CHIP_PREVIEW_CAP)
                .map(|error| format!("{}: {}", error.tool, error.preview))
                .collect::<Vec<_>>()
                .join(" • ");
            let title = has_subagents.then(|| "(parent only)".to_string());
            segments.push(SegmentView {
                text: pluralize(*count as usize, "error"),
                popover,
                title,
                degraded: false,
            });
        }
        ErrorDisplay::Degraded { count } => {
            let mut title = "pattern-matched, not tool-verified".to_string();
            if has_subagents {
                title.push_str(" (parent only)");
            }
            segments.push(SegmentView {
                text: format!("~{}", pluralize(*count as usize, "error")),
                popover: String::new(),
                title: Some(title),
                degraded: true,
            });
        }
    }
    segments
}

fn index_row(card: &StoryCardView, selected: bool) -> IndexRowView {
    IndexRowView {
        session_short: card.session_short.clone(),
        glyph: outcome_glyph(card.outcome_slug),
        outcome_slug: card.outcome_slug,
        project: card.project.clone(),
        sentence: card.summary.clone(),
        segments: instrumentation_segments(
            &card.todos,
            &card.artifacts,
            &card.files_touched,
            &error_display(card),
            card.has_subagents,
        ),
        ts: card.timestamp.clone(),
        day: card.date.clone(),
        selected,
    }
}

fn detail_pane(card: &StoryCardView) -> DetailPaneView {
    let artifacts_more = card.artifacts.len().saturating_sub(ARTIFACT_CAP);
    DetailPaneView {
        session_short: card.session_short.clone(),
        sentence: card.summary.clone(),
        description: card.description.clone(),
        project: card.project.clone(),
        date: card.date.clone(),
        outcome_slug: card.outcome_slug,
        outcome_badge: card.outcome_badge.clone(),
        stages: build_stage_cards(card),
        artifacts: card.artifacts.iter().take(ARTIFACT_CAP).cloned().collect(),
        artifacts_more,
        episode_json: card.episode_json.clone(),
        unscanned: card.unscanned,
    }
}

/// Raw, uncurated projection — gathered once from storage, curated in place
/// (`curate_sentences`), then projected into `DreamReportData` by
/// `finalize_report_data`.
struct RawReportData {
    has_run: bool,
    last_run_head_short: Option<String>,
    last_run_at: Option<String>,
    totals: VerdictTotals,
    rich_cards: Vec<StoryCardView>,
    thin_entries: Vec<(String, String, ThinRowView)>,
    omitted: usize,
    older_omitted: usize,
}

/// Kill switch (plan §3.3a/§6 Phase 2): when set, the report-time backfill
/// never parses a transcript or writes a `session_instrumentation` row.
/// Threaded as a parameter with the env read happening at this single call
/// site, rather than read again inside the backfill loop, so tests never
/// race the process-global variable the way parallel `cargo test` threads
/// would if the loop itself called `std::env::var`.
const CSR_JOURNAL_NO_SCAN: &str = "CSR_JOURNAL_NO_SCAN";

fn journal_scan_enabled() -> bool {
    std::env::var(CSR_JOURNAL_NO_SCAN).as_deref() != Ok("1")
}

/// Wall-clock budget for the whole report-time backfill loop (plan §3.3a
/// cost table): checked between sessions, never mid-parse — a session
/// already in flight finishes, but no new one starts once the budget is
/// spent. Left-over sessions stay uninstrumented (honest partial), not an
/// error.
const BACKFILL_WALL_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);

/// True when `transcript_path`'s sibling `<session_id>/subagents/` directory
/// exists and contains at least one `agent-*.jsonl` file — the layout
/// `import::derive_conversation_attribution` already recognizes for
/// sidechains. Used only for the R8 "(parent only)" disclosure; sidechain
/// transcripts themselves are never scanned (plan §8 R8/§9 Q9, out of
/// scope).
fn has_subagent_transcripts(transcript_path: &Path, session_id: &str) -> bool {
    let Some(parent) = transcript_path.parent() else {
        return false;
    };
    let subagents_dir = parent.join(session_id).join("subagents");
    let Ok(entries) = std::fs::read_dir(&subagents_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        name.starts_with("agent-") && name.ends_with(".jsonl")
    })
}

/// Report-time instrumentation backfill (plan §3.3a resolution order, step
/// 3 — live scan). Mutates `cards` in place: any card whose
/// `instrumentation` is still `None` after the episode/cache resolution in
/// `StorySession` gets one attempt at a fresh scan, bounded by the wall
/// budget and the existing `MAX_STORY_CARDS` cap on `cards.len()` itself
/// (the plan's "at most 50 sessions per run"). A transcript over
/// `MAX_TRANSCRIPT_SCAN_BYTES` is never parsed; it only sets `unscanned`. A
/// cache hit found here (a prior run's backfill result the DB-only
/// `load_story_sessions` join could not see freshness for) does not count
/// against the wall budget — only an actual `parse_transcript` call does.
fn backfill_instrumentation(storage: &Storage, projects_dir: &Path, cards: &mut [StoryCardView]) {
    let scan_enabled = journal_scan_enabled();
    let start = std::time::Instant::now();
    for card in cards.iter_mut() {
        if card.instrumentation.is_some() {
            continue;
        }
        let lookup = crate::transcript::resolve_session_path(
            projects_dir,
            &card.session_id,
            Some(&card.project),
        );
        let path = match lookup {
            crate::mcp::tools::ConversationLookup::Found(path, _) => path,
            _ => continue, // no transcript at all -> no chip, segment stays Absent/Degraded.
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        card.has_subagents = has_subagent_transcripts(&path, &card.session_id);
        if metadata.len() > crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES {
            card.unscanned = true;
            continue;
        }
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // A cache row from a prior run may already be fresh even though the
        // DB-only join in `load_story_sessions` could not validate it
        // without this filesystem stat. Reusing it here costs one read, no
        // parse, and does not count against the wall budget.
        let cached = storage
            .with_connection(|conn| {
                dream_report::read_instrumentation_cache(
                    conn,
                    &card.session_id,
                    metadata.len(),
                    mtime,
                )
            })
            .ok()
            .flatten();
        if let Some(instrumentation) = cached {
            card.instrumentation = Some(instrumentation);
            continue;
        }

        if !scan_enabled {
            card.unscanned = true;
            continue;
        }
        if start.elapsed() > BACKFILL_WALL_BUDGET {
            card.unscanned = true;
            continue; // budget spent — stop honestly, leave the rest uninstrumented.
        }
        let Ok(parsed) = crate::transcript::parse_transcript(&path) else {
            card.unscanned = true;
            continue;
        };
        let instrumentation = crate::transcript::instrumentation::from_parsed(&parsed);
        let _ = storage.with_connection(|conn| {
            dream_report::write_instrumentation_cache(
                conn,
                &card.session_id,
                metadata.len(),
                mtime,
                &instrumentation,
            )
        });
        card.instrumentation = Some(instrumentation);
    }
}

/// Gather every piece of data through read-only storage projections.
fn gather_report_data(storage: &Storage, projects_dir: &Path) -> Result<RawReportData> {
    gather_report_data_with(
        storage,
        projects_dir,
        crate::storage::recap_feeds::dream_consumption_mode(),
    )
}

fn gather_report_data_with(
    storage: &Storage,
    projects_dir: &Path,
    _consumption_mode: crate::storage::recap_feeds::ConsumptionMode,
) -> Result<RawReportData> {
    let (obsolete, superseded, reinstated) = storage.dream_event_totals()?;
    let last_run = storage.last_dream_run()?;
    let (last_run_head_short, last_run_at) = match &last_run {
        Some((oid, at)) => (Some(short_oid(oid)), Some(at.clone())),
        None => (None, None),
    };

    let sessions = storage.with_connection(crate::storage::dream_report::load_story_sessions)?;
    let mut rich_cards = Vec::new();
    let mut thin_entries = Vec::new();
    let mut omitted = 0usize;
    for session in &sessions {
        match classify_session(session) {
            SessionClass::Rich => rich_cards.push(build_story_card(session)),
            SessionClass::Thin => thin_entries.push(build_thin_entry(session)),
            SessionClass::Omitted => omitted += 1,
        }
    }
    let older_omitted = rich_cards.len().saturating_sub(MAX_STORY_CARDS);
    rich_cards.truncate(MAX_STORY_CARDS);
    backfill_instrumentation(storage, projects_dir, &mut rich_cards);

    let total = obsolete + superseded + reinstated;
    Ok(RawReportData {
        has_run: last_run.is_some(),
        last_run_head_short,
        last_run_at,
        totals: VerdictTotals {
            obsolete,
            superseded,
            reinstated,
            total,
        },
        rich_cards,
        thin_entries,
        omitted,
        older_omitted,
    })
}

/// Project curated raw data into the view types the template walks: day
/// groups (recency-sorted, server-rendered), detail panes (one per rich
/// card, same order), and thin groups (by project, count desc).
fn finalize_report_data(raw: RawReportData) -> DreamReportData {
    let today = chrono::Utc::now().date_naive();

    let mut day_groups: Vec<DayGroupView> = Vec::new();
    let mut detail_panes = Vec::with_capacity(raw.rich_cards.len());
    for (index, card) in raw.rich_cards.iter().enumerate() {
        let selected = index == 0;
        let row = index_row(card, selected);
        detail_panes.push(detail_pane(card));
        match day_groups.last_mut() {
            Some(group) if group.label == day_label(&card.date, today) => group.rows.push(row),
            _ => day_groups.push(DayGroupView {
                label: day_label(&card.date, today),
                rows: vec![row],
            }),
        }
    }

    let mut by_project: std::collections::BTreeMap<String, Vec<(String, ThinRowView)>> =
        std::collections::BTreeMap::new();
    for (project, timestamp, row) in raw.thin_entries {
        by_project
            .entry(project)
            .or_default()
            .push((timestamp, row));
    }
    let thin_total = by_project.values().map(Vec::len).sum();
    let mut thin_groups: Vec<ThinGroupView> = by_project
        .into_iter()
        .map(|(project, mut rows)| {
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            ThinGroupView {
                project,
                count: rows.len(),
                rows: rows.into_iter().map(|(_, row)| row).collect(),
            }
        })
        .collect();
    thin_groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| a.project.cmp(&b.project))
    });

    // Only the true zero-signal case (no rich session, no thin session
    // either) falls back to the plain "no story signal" empty state — a
    // corpus that is all-thin still gets the mailbox shell so its rollup is
    // visible (Q2 explicitly promotes registry-only/artifact-only sessions
    // into view instead of hiding them).
    let is_empty = day_groups.is_empty() && thin_groups.is_empty();

    DreamReportData {
        has_run: raw.has_run,
        last_run_head_short: raw.last_run_head_short,
        last_run_at: raw.last_run_at,
        totals: raw.totals,
        day_groups,
        detail_panes,
        thin_groups,
        thin_total,
        omitted: raw.omitted,
        older_omitted: raw.older_omitted,
        is_empty,
    }
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
        &format!("{MAILBOX_CSS}</style>"),
        "</style> (MAILBOX_CSS insertion point)",
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
    let scripts = format!("<script>{MAILBOX_SCRIPT}</script>\n</body>");
    Ok(rendered.replacen("</body>", &scripts, 1))
}

// --- AI-curated fused sentences ---------------------------------------------
//
// Raw first-prompts make poor headlines ("pull the latest data."). When AI
// narratives are enabled, one batched `claude -p` call rewrites the summary
// line of every uncached card with a single verb-first ask->outcome sentence
// (plan §5.1); results are cached in `journal_headlines` by a content hash,
// so an unchanged corpus re-renders at zero cost. The kill switch
// (`CSR_NO_AI_NARRATIVES=1`) suppresses the invocation but still applies
// previously cached sentences — they are already paid for.
//
// No schema change (plan §5.2): the cache table and its `headline`/
// `description` columns are reused verbatim — `headline` now holds the fused
// sentence, `description` holds the detail-pane subtitle. Migration is by
// `CURATION_PROMPT_VERSION` folded into the content hash, not by ALTER or
// backfill: every row cached under the old two-field prompt misses exactly
// once and is overwritten.

/// Bumped whenever the curation prompt or its output contract changes.
/// Folded into `sentence_hash` so every cached row from an older prompt
/// misses deterministically — no ALTER, no backfill script, no dual-read.
const CURATION_PROMPT_VERSION: u32 = 2;

/// Asked for in the prompt text itself (90); the cache/parse ceiling below
/// carries a little slack before truncation kicks in.
const SENTENCE_MAX_CHARS: usize = 96;

/// Number of misses batched into one `claude -p` call. A 50-card batch
/// overruns the model's output cap (measured ~1.2k output tokens per card on
/// haiku), truncating the JSON so the whole reply fails to parse and NOTHING
/// caches — which made every render re-pay the full batch. Small chunks keep
/// each reply well inside the cap, and a failed chunk no longer sinks the
/// others. Unchanged from Phase 1/2 (plan: "chunk size stays 10").
const SENTENCE_CHUNK: usize = 10;

/// Structural chunk-count bound (plan §7.3 test 7): the number of `claude -p`
/// calls a given miss-count would spend, `ceil(misses / SENTENCE_CHUNK)`.
fn curation_chunk_plan(misses: usize) -> usize {
    misses.div_ceil(SENTENCE_CHUNK)
}

/// Deterministic, zero-AI sentence (plan §5.2) used when curation is off,
/// fails, or its reply's first word disagrees with `outcome_slug`
/// (`verb_agrees`). Format: `"{prefix}: {first_sentence(source)}"`, clamped
/// to `SENTENCE_MAX_CHARS`. Never as good as a curated line; never wrong
/// about the outcome, which matters more.
fn fallback_sentence(outcome_slug: &str, source: Option<&str>) -> String {
    let prefix = match outcome_slug {
        "success" => "Shipped",
        "partial" => "Partly done",
        "failed" => "Failed",
        _ => "Noted",
    };
    let body = source
        .map(first_sentence)
        .filter(|body| !body.is_empty())
        .unwrap_or_else(|| "session recorded".to_string());
    let body = body.trim_end_matches(['.', '!', '?']);
    truncate_chars(&format!("{prefix}: {body}"), SENTENCE_MAX_CHARS)
}

/// Verb-agreement table (plan §5.1): a curated reply whose opening words
/// contradict the outcome slug is rejected outright rather than displayed —
/// a model saying "Shipped" over a session recorded as `failed` is worse
/// than the plain deterministic fallback.
fn verb_agrees(slug: &str, sentence: &str) -> bool {
    let lower = sentence.trim().to_lowercase();
    let starts_with_any = |verbs: &[&str]| verbs.iter().any(|verb| lower.starts_with(verb));
    match slug {
        "success" => starts_with_any(&["shipped", "fixed", "built", "landed", "closed", "merged"]),
        "partial" => {
            lower.starts_with("partly ")
                || lower.starts_with("got as far as")
                || lower.starts_with("half-")
                || lower.starts_with("half ")
        }
        "failed" => starts_with_any(&["failed", "gave up on", "broke"]),
        _ => starts_with_any(&["noted", "looked at", "recorded"]),
    }
}

/// Folds `CURATION_PROMPT_VERSION` into the content hash so every cached row
/// from an older prompt misses deterministically (plan §5.2 code block).
/// Deliberately independent of `card.summary` (which the previous curation
/// pass may already have overwritten) — the hash is a pure function of
/// evidence: the outcome slug, the deterministic fallback derived from
/// narrative/request, and the episode JSON.
fn sentence_hash(card: &StoryCardView) -> String {
    let fallback = fallback_sentence(
        card.outcome_slug,
        card.narrative.as_deref().or(card.request_text.as_deref()),
    );
    let payload = format!(
        "{CURATION_PROMPT_VERSION}\u{1}{}\u{1}{}\u{1}{}",
        card.outcome_slug, fallback, card.episode_json
    );
    format!("{:016x}", crate::narrative::fnv1a_64(payload.as_bytes()))
}

/// Parse the model's batch response: a JSON object mapping session-short ids
/// to a bare sentence string, or (defensively) to an object carrying a
/// `sentence` or `headline` key — possibly wrapped in code fences or prose.
/// Fail-open: any shape problem yields an empty map and the cards keep their
/// deterministic fallback sentences.
fn parse_sentence_batch(text: &str) -> std::collections::BTreeMap<String, String> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Default::default();
    };
    if end < start {
        return Default::default();
    }
    let clean = |value: &str| -> String {
        truncate_chars(
            &value.split_whitespace().collect::<Vec<_>>().join(" "),
            SENTENCE_MAX_CHARS,
        )
    };
    serde_json::from_str::<std::collections::BTreeMap<String, serde_json::Value>>(
        &text[start..=end],
    )
    .unwrap_or_default()
    .into_iter()
    .filter_map(|(id, value)| {
        let sentence = match value {
            serde_json::Value::String(sentence) => sentence,
            serde_json::Value::Object(fields) => fields
                .get("sentence")
                .or_else(|| fields.get("headline"))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            _ => return None,
        };
        let sentence = clean(&sentence);
        (!sentence.is_empty()).then_some((id, sentence))
    })
    .collect()
}

/// A cached row is only a hit when it carries a description too — rows
/// written by a description-less shape re-curate once and are then
/// complete (plan §5.2 / test `description_still_written_...`).
fn cached_sentence(storage: &Storage, session_id: &str, hash: &str) -> Option<(String, String)> {
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

/// No schema change (plan §5.2): `headline` holds the fused sentence,
/// `description` holds the detail-pane subtitle. Column names unchanged.
fn store_sentence(
    storage: &Storage,
    session_id: &str,
    hash: &str,
    sentence: &str,
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
            rusqlite::params![session_id, hash, sentence, description, model],
        )?;
        Ok(())
    });
}

/// Plan §5.1's prompt, verbatim, plus the batched session payload (which now
/// carries an `outcome` field — the deterministic slug from `outcome_badge`
/// — so the model's verb has something to agree with).
fn sentence_prompt(misses: &[(&StoryCardView, String)]) -> String {
    let sessions = misses
        .iter()
        .map(|(card, _)| {
            serde_json::json!({
                "id": card.session_short,
                "project": card.project,
                "outcome": card.outcome_slug,
                "text": truncate_chars(&card.summary, 300),
                "detail": truncate_chars(&card.episode_json, 600),
            })
        })
        .collect::<Vec<_>>();
    format!(
        "You are writing one line per session for a developer's private journal.\n\
         For each session below write ONE sentence, max 90 characters, that fuses what\n\
         was asked with how it ended.\n\
         \n\
         Rules:\n\
         - Start with a past-tense verb that carries the outcome. The outcome slug is\n\
         \u{20}\u{20}given for each session and your verb MUST agree with it:\n\
         \u{20}\u{20}\u{20}\u{20}success -> Shipped / Fixed / Built / Landed / Closed / Merged\n\
         \u{20}\u{20}\u{20}\u{20}partial -> Partly <verb> / Got as far as / Half-<verb>\n\
         \u{20}\u{20}\u{20}\u{20}failed  -> Failed at / Gave up on / Broke\n\
         \u{20}\u{20}\u{20}\u{20}noted   -> Noted / Looked at / Recorded\n\
         - Name the concrete thing: the feature, file, bug, or artifact. Never\n\
         \u{20}\u{20}\"worked on the project\", never \"made progress\".\n\
         - No trailing period. No quotes. No markdown.\n\
         - Use ONLY the given text/detail as evidence. Do not invent an outcome the\n\
         \u{20}\u{20}evidence does not support.\n\
         \n\
         Return ONLY a JSON object mapping each id to a bare string.\n\
         Sessions: {}",
        serde_json::to_string(&sessions).unwrap_or_default()
    )
}

/// Applies one parsed reply map to `cards`/`storage` for `chunk`'s miss
/// indices. Split out from `curate_sentences_with` so the verb-agreement /
/// caching contract (plan §5.1: a disagreeing reply is rejected and NOT
/// cached) is unit-testable without spawning `claude`.
fn apply_sentence_replies(
    storage: &Storage,
    cards: &mut [StoryCardView],
    chunk: &[usize],
    hashes: &[String],
    replies: &std::collections::BTreeMap<String, String>,
    model: &str,
) {
    for &index in chunk {
        let (session_short, hash) = (cards[index].session_short.clone(), hashes[index].clone());
        let Some(reply) = replies.get(&session_short) else {
            continue;
        };
        let slug = cards[index].outcome_slug;
        if verb_agrees(slug, reply) {
            // Store a non-empty description unconditionally so the row
            // counts as a cache hit next run: the card's deterministic
            // subtitle when it has one, else the sentence itself. Without
            // this, sentence-only replies made the same cards re-curate
            // forever.
            let stored_description = if !cards[index].description.is_empty() {
                cards[index].description.clone()
            } else {
                reply.clone()
            };
            store_sentence(
                storage,
                &session_short,
                &hash,
                reply,
                &stored_description,
                model,
            );
            cards[index].summary = reply.clone();
            cards[index].description = if stored_description == *reply {
                String::new()
            } else {
                stored_description
            };
        } else {
            tracing::warn!(
                session = %session_short,
                reply = %reply,
                slug,
                "curated sentence disagreed with the outcome slug — using the \
                 deterministic fallback, not caching the model's line"
            );
            let narrative_or_request = cards[index]
                .narrative
                .clone()
                .or_else(|| cards[index].request_text.clone());
            cards[index].summary = fallback_sentence(slug, narrative_or_request.as_deref());
        }
    }
}

/// Apply cached sentences and, unless narratives are disabled, batch-generate
/// the missing ones. Every failure path leaves the deterministic fallback
/// sentence in place.
fn curate_sentences(storage: &Storage, cards: &mut [StoryCardView]) {
    curate_sentences_with(storage, cards, !crate::narrative::narratives_disabled())
}

/// `allow_invoke` is threaded as a parameter (not read from the environment
/// here) so tests can exercise the cache paths without racing other tests on
/// the process-global `CSR_NO_AI_NARRATIVES` variable.
fn curate_sentences_with(storage: &Storage, cards: &mut [StoryCardView], allow_invoke: bool) {
    let hashes: Vec<String> = cards.iter().map(sentence_hash).collect();
    let mut misses: Vec<usize> = Vec::new();
    for (index, card) in cards.iter_mut().enumerate() {
        match cached_sentence(storage, &card.session_short, &hashes[index]) {
            Some((sentence, description)) => {
                // A description equal to the sentence is a convergence
                // placeholder, not display content.
                card.description = if description == sentence {
                    String::new()
                } else {
                    description
                };
                card.summary = sentence;
            }
            None => misses.push(index),
        }
    }
    if misses.is_empty() || !allow_invoke {
        return;
    }
    tracing::debug!(
        misses = misses.len(),
        chunks = curation_chunk_plan(misses.len()),
        "journal sentence curation: claude -p calls about to be spent"
    );

    for chunk in misses.chunks(SENTENCE_CHUNK) {
        let batch: Vec<(&StoryCardView, String)> = chunk
            .iter()
            .map(|&index| (&cards[index] as &StoryCardView, hashes[index].clone()))
            .collect();
        let prompt = sentence_prompt(&batch);
        let started = std::time::Instant::now();
        let parsed = match crate::hooks::session_briefing::invoke_narrative_briefing(&prompt) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(error = %err, "journal sentence chunk failed — keeping deterministic fallbacks");
                continue;
            }
        };
        // `call_site` stays "journal_headline" (plan §6 Phase 3: renaming it
        // would reset the maintainer's cost history in `narrative_usage`).
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

        let replies = parse_sentence_batch(&parsed.text);
        if replies.is_empty() {
            tracing::warn!("journal sentence chunk parsed to nothing — reply likely malformed");
            continue;
        }
        apply_sentence_replies(storage, cards, chunk, &hashes, &replies, &parsed.model);
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
pub fn run_report(
    storage: &Storage,
    projects_dir: &Path,
    out: Option<PathBuf>,
    no_open: bool,
) -> Result<PathBuf> {
    let mut raw = gather_report_data(storage, projects_dir)?;
    curate_sentences(storage, &mut raw.rich_cards);
    let data = finalize_report_data(raw);
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

    const EPISODE_DATA_OPEN: &str = r#"<script type="application/json" class="episode-data">"#;

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
        for forbidden in [
            r#"src="http"#,
            r#"href="http"#,
            "cdn.",
            "integrity=",
            "<link",
            "@import",
            "url(http",
        ] {
            assert!(
                !lower.contains(forbidden),
                "report must not contain external resource reference {forbidden:?}"
            );
        }
    }

    /// All occurrences of `data-session="..."` (or any `needle`-prefixed
    /// attribute), in document order, within `html`.
    fn attr_values<'a>(html: &'a str, needle: &str) -> Vec<&'a str> {
        html.match_indices(needle)
            .filter_map(|(idx, _)| {
                let start = idx + needle.len();
                html[start..].find('"').map(|end| &html[start..start + end])
            })
            .collect()
    }

    /// Split the rendered report into (index-pane markup, detail-pane-wrap
    /// markup) so tests can assert per-section counts without the two
    /// panes' `data-session` attributes colliding.
    fn split_panes(html: &str) -> (&str, &str) {
        let marker = r#"<div class="detail-pane-wrap">"#;
        let idx = html
            .find(marker)
            .expect("detail-pane-wrap marker must be present in a rendered mailbox");
        (&html[..idx], &html[idx..])
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

    /// Like `seed_episode`, but merges `extra` object keys into the episode
    /// JSON — for tests exercising the Phase 2 fields (`error_count`,
    /// `steer_count`, `top_errors`, `steers`, `files_modified`,
    /// `error_signatures`, `completed`, `next_steps`, `blockers`) that
    /// `seed_episode`'s fixed shape doesn't cover.
    fn seed_episode_with_extra(
        storage: &Storage,
        session_id: &str,
        timestamp: &str,
        request: &str,
        outcome: &str,
        extra: serde_json::Value,
    ) {
        let mut content = serde_json::json!({
            "schema": "v2",
            "session_id": session_id,
            "project": "proj",
            "timestamp": timestamp,
            "request": request,
            "outcome": outcome,
            "investigated": [],
            "todos": [],
        });
        if let (Some(map), Some(extra_map)) = (content.as_object_mut(), extra.as_object()) {
            for (key, value) in extra_map {
                map.insert(key.clone(), value.clone());
            }
        }
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

    /// Full pipeline (gather -> curate with no AI invocation -> finalize ->
    /// render), the shape every test below needs. Curation always runs with
    /// `allow_invoke=false` so tests never spawn `claude` or race the
    /// process-global `CSR_NO_AI_NARRATIVES` variable. Uses a fresh, empty
    /// tempdir as `projects_dir` — the report-time backfill resolves no
    /// transcript against it and simply no-ops, which is what every test
    /// that doesn't care about instrumentation backfill wants. Tests that
    /// DO care use `render_with_projects_dir` directly.
    fn render(storage: &Storage, mode: crate::storage::recap_feeds::ConsumptionMode) -> String {
        let empty_dir = tempfile::tempdir().unwrap();
        render_with_projects_dir(storage, empty_dir.path(), mode)
    }

    fn render_with_projects_dir(
        storage: &Storage,
        projects_dir: &Path,
        mode: crate::storage::recap_feeds::ConsumptionMode,
    ) -> String {
        let mut raw = gather_report_data_with(storage, projects_dir, mode).unwrap();
        curate_sentences_with(storage, &mut raw.rich_cards, false);
        let data = finalize_report_data(raw);
        render_html(&data).unwrap()
    }

    fn seed_rich_sessions(storage: &Storage, count: usize) {
        for index in 0..count {
            let session = format!("rich-session-{index:03}");
            let timestamp = format!("2026-04-{:02}T0{}:00:00Z", (index % 27) + 1, index % 9);
            seed_episode(
                storage,
                &session,
                &timestamp,
                &format!("request number {index}"),
                "done",
                &[],
                &[],
            );
        }
    }

    /// Returns the indices of `cards` that would still need a `claude -p`
    /// call: those whose `sentence_hash` has no matching, non-empty-
    /// description row in `journal_headlines`. Test-only projection of the
    /// same predicate `curate_sentences_with`'s cache-application loop uses.
    fn curation_misses(storage: &Storage, cards: &[StoryCardView]) -> Vec<usize> {
        cards
            .iter()
            .enumerate()
            .filter_map(|(index, card)| {
                let hash = sentence_hash(card);
                cached_sentence(storage, &card.session_short, &hash)
                    .is_none()
                    .then_some(index)
            })
            .collect()
    }

    /// Test 1 (replaces `parse_headline_batch_tolerates_fences_prose_and_clamps_length`).
    #[test]
    fn parse_sentence_batch_accepts_bare_strings_and_objects_and_clamps() {
        // Bare string shape — the new prompt's actual contract.
        let bare = "{\"abc12345\": \"Fixed the relevance gate in guardrails\"}";
        assert_eq!(
            parse_sentence_batch(bare)
                .get("abc12345")
                .map(String::as_str),
            Some("Fixed the relevance gate in guardrails")
        );

        // Defensive object shapes: {"sentence": ...} and {"headline": ...}.
        let sentence_obj =
            "```json\n{\"id1\": {\"sentence\": \"Shipped the queue retry path\"}}\n```";
        assert_eq!(
            parse_sentence_batch(sentence_obj)
                .get("id1")
                .map(String::as_str),
            Some("Shipped the queue retry path")
        );
        let headline_obj = "{\"id2\": {\"headline\": \"Failed at the render step\"}}";
        assert_eq!(
            parse_sentence_batch(headline_obj)
                .get("id2")
                .map(String::as_str),
            Some("Failed at the render step")
        );

        // Prose wrapper + code fence + whitespace collapse.
        let prose = "Sure, here you go:\n```json\n{\"id3\": \"line one\\n   line two\"}\n```\nHope that helps!";
        assert_eq!(
            parse_sentence_batch(prose).get("id3").map(String::as_str),
            Some("line one line two")
        );

        // A 400-char reply clamps to SENTENCE_MAX_CHARS.
        let long = format!("{{\"id4\": \"{}\"}}", "x".repeat(400));
        let clamped = parse_sentence_batch(&long);
        assert!(clamped.get("id4").unwrap().chars().count() <= SENTENCE_MAX_CHARS);

        assert!(parse_sentence_batch("no json here").is_empty());
        assert!(parse_sentence_batch("{\"id\": \"   \"}").is_empty());
    }

    /// Test 2.
    #[test]
    fn verb_disagreeing_with_the_outcome_slug_is_rejected() {
        let storage = Storage::open_memory().unwrap();
        let session = StorySession {
            session_id: "failed-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("create the Ekadashi podcast episode".into()),
            outcome: Some("failed: render aborted".into()),
            ..Default::default()
        };
        let mut cards = vec![build_story_card(&session)];
        assert_eq!(cards[0].outcome_slug, "failed");
        let hash = sentence_hash(&cards[0]);
        let hashes = vec![hash.clone()];

        let mut replies = std::collections::BTreeMap::new();
        replies.insert(
            cards[0].session_short.clone(),
            "Shipped the podcast".to_string(),
        );
        apply_sentence_replies(&storage, &mut cards, &[0], &hashes, &replies, "test-model");

        assert!(
            cards[0].summary.starts_with("Failed:"),
            "got: {}",
            cards[0].summary
        );
        assert!(
            cached_sentence(&storage, &cards[0].session_short, &hash).is_none(),
            "a verb-disagreeing reply must not be cached"
        );
    }

    /// Test 3.
    #[test]
    fn deterministic_fallback_sentence_carries_the_outcome_verb() {
        assert!(
            fallback_sentence("success", Some("ship the mailbox layout")).starts_with("Shipped:")
        );
        assert!(
            fallback_sentence("partial", Some("fix the cache miss")).starts_with("Partly done:")
        );
        assert!(fallback_sentence("failed", Some("debug the flaky test")).starts_with("Failed:"));
        assert!(fallback_sentence("noted", Some("look at the alert")).starts_with("Noted:"));
        // No source text at all still produces a non-empty, correctly
        // prefixed sentence rather than panicking or returning empty.
        assert!(fallback_sentence("failed", None).starts_with("Failed:"));
    }

    /// Test 4.
    #[test]
    fn prompt_version_bump_invalidates_every_cached_row() {
        let storage = Storage::open_memory().unwrap();
        let session = StorySession {
            session_id: "version-bump-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("wire the sort controls".into()),
            outcome: Some("shipped".into()),
            ..Default::default()
        };
        let card = build_story_card(&session);

        // A row cached under a hash that is not today's `sentence_hash`
        // formula — stands in for "an older prompt version's hash", however
        // it was actually produced.
        let stale_hash = format!(
            "{:016x}",
            crate::narrative::fnv1a_64(
                format!("1\u{1}{}\u{1}{}", card.summary, card.episode_json).as_bytes()
            )
        );
        store_sentence(
            &storage,
            &card.session_short,
            &stale_hash,
            "Stale sentence from an older prompt",
            "Stale description.",
            "test-model",
        );
        assert!(
            cached_sentence(&storage, &card.session_short, &sentence_hash(&card)).is_none(),
            "a stale-prompt-version hash must miss"
        );

        // A row cached under today's `sentence_hash` formula hits.
        let current_hash = sentence_hash(&card);
        store_sentence(
            &storage,
            &card.session_short,
            &current_hash,
            "Shipped the sort controls",
            "Wired recency, group, and type sort.",
            "test-model",
        );
        assert_eq!(
            cached_sentence(&storage, &card.session_short, &current_hash),
            Some((
                "Shipped the sort controls".to_string(),
                "Wired recency, group, and type sort.".to_string()
            ))
        );
    }

    /// Test 5 (adapts `cached_headlines_apply_without_any_ai_invocation`).
    #[test]
    fn cached_sentences_apply_without_any_ai_invocation() {
        // Invocation disabled: curate must never spawn claude, yet a
        // previously cached sentence (already paid for) still applies, and
        // an uncached card keeps its deterministic fallback summary.
        let storage = Storage::open_memory().unwrap();
        let session = StorySession {
            session_id: "cached-sentence-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("pull the latest data.".into()),
            outcome: Some("done".into()),
            ..Default::default()
        };
        let raw_card = build_story_card(&session);
        let hash = sentence_hash(&raw_card);
        store_sentence(
            &storage,
            &raw_card.session_short,
            &hash,
            "Noted the overnight metrics pull",
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
        let mut cards = vec![raw_card, build_story_card(&uncached)];
        let raw_summary = cards[1].summary.clone();

        curate_sentences_with(&storage, &mut cards, false);

        assert_eq!(cards[0].summary, "Noted the overnight metrics pull");
        assert_eq!(
            cards[0].description,
            "Pulled Meta spend and account balances; MRR snapshot confirmed at $25k."
        );
        assert_eq!(cards[1].summary, raw_summary);
    }

    /// Test 6.
    #[test]
    fn curation_converges_second_pass_has_zero_misses() {
        let storage = Storage::open_memory().unwrap();
        let mut cards: Vec<StoryCardView> = (0..6)
            .map(|index| {
                // `session_short` is the first 8 chars of `session_id` (no
                // hashing) — the index must land inside that prefix or every
                // card collides onto the same `journal_headlines` row.
                build_story_card(&StorySession {
                    session_id: format!("cvg{index}-converge-session"),
                    project: "proj".into(),
                    timestamp: "2026-08-10T00:00:00Z".into(),
                    request: Some(format!("do task number {index}")),
                    outcome: Some("shipped".into()),
                    ..Default::default()
                })
            })
            .collect();
        assert_eq!(curation_misses(&storage, &cards).len(), cards.len());

        // Simulate one curation pass: the model agrees with every outcome
        // slug, so `apply_sentence_replies` caches every card (the same
        // store path `curate_sentences_with` uses on a real reply).
        let hashes: Vec<String> = cards.iter().map(sentence_hash).collect();
        let mut replies = std::collections::BTreeMap::new();
        for card in &cards {
            replies.insert(
                card.session_short.clone(),
                format!("Shipped task {}", card.session_short),
            );
        }
        let all_indices: Vec<usize> = (0..cards.len()).collect();
        apply_sentence_replies(
            &storage,
            &mut cards,
            &all_indices,
            &hashes,
            &replies,
            "test-model",
        );
        assert!(curation_misses(&storage, &cards).is_empty());

        // Second pass, allow_invoke=false: everything already hits, so this
        // must spawn nothing and leave zero misses (the unit-level form of
        // "rerun costs zero").
        curate_sentences_with(&storage, &mut cards, false);
        assert!(curation_misses(&storage, &cards).is_empty());
    }

    /// Test 7.
    #[test]
    fn curation_chunk_plan_is_ceil_over_ten_and_capped() {
        assert_eq!(curation_chunk_plan(50), 5);
        assert_eq!(curation_chunk_plan(1), 1);
        assert_eq!(curation_chunk_plan(0), 0);
        assert!(curation_chunk_plan(MAX_STORY_CARDS) <= 5);
    }

    /// Test 8.
    #[test]
    fn thin_rollup_rows_are_never_sent_to_curation() {
        let storage = Storage::open_memory().unwrap();
        seed_rich_sessions(&storage, 5);
        // Artifact-only sessions classify as THIN (Q2) — never curated.
        for index in 0..40 {
            let session_id = format!("thin-session-{index:03}");
            let timestamp = format!("2026-05-{:02}T0{}:00:00Z", (index % 27) + 1, index % 9);
            seed_anchor(
                &storage,
                &session_id,
                &timestamp,
                &format!("thin_fn_{index}"),
            );
        }
        let empty_dir = tempfile::tempdir().unwrap();
        let raw = gather_report_data_with(
            &storage,
            empty_dir.path(),
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        )
        .unwrap();
        assert_eq!(raw.rich_cards.len(), 5);
        assert!(raw.thin_entries.len() >= 40);
        assert!(curation_misses(&storage, &raw.rich_cards).len() <= 5);
    }

    /// Test 9.
    #[test]
    fn description_still_written_so_cache_hit_predicate_holds() {
        let storage = Storage::open_memory().unwrap();
        let session = StorySession {
            session_id: "sentence-only-reply-session".into(),
            project: "proj".into(),
            timestamp: "2026-08-10T00:00:00Z".into(),
            request: Some("ship the mailbox layout".into()),
            outcome: Some("shipped and merged".into()),
            ..Default::default()
        };
        let mut cards = vec![build_story_card(&session)];
        let hash = sentence_hash(&cards[0]);
        let hashes = vec![hash.clone()];

        // A reply carrying only a bare sentence — the new prompt's actual
        // contract has no description field at all.
        let mut replies = std::collections::BTreeMap::new();
        replies.insert(
            cards[0].session_short.clone(),
            "Shipped the mailbox layout".to_string(),
        );
        apply_sentence_replies(&storage, &mut cards, &[0], &hashes, &replies, "test-model");

        let stored: Option<(String, String)> = storage
            .with_connection(|conn| {
                Ok(conn
                    .query_row(
                        "SELECT headline, description FROM journal_headlines WHERE session_id = ?1",
                        rusqlite::params![cards[0].session_short],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .ok())
            })
            .unwrap();
        let (stored_headline, stored_description) = stored.expect("row must exist after apply");
        assert_eq!(stored_headline, "Shipped the mailbox layout");
        assert!(
            !stored_description.is_empty(),
            "description must be non-empty so the row hits next run"
        );

        // Next run, same hash: must be a cache hit, not a miss — the exact
        // regression the description fallback (`else { reply.clone() }`)
        // guards against.
        assert!(cached_sentence(&storage, &cards[0].session_short, &hash).is_some());
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
        let card = build_story_card(&session);
        store_sentence(
            &storage,
            &card.session_short,
            "0000000000000000",
            "Sentence for content that no longer exists",
            "Description for content that no longer exists.",
            "test-model",
        );
        let mut cards = vec![card];
        let raw_summary = cards[0].summary.clone();
        curate_sentences_with(&storage, &mut cards, false);
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
    fn signature_scaffold_never_reaches_summary_or_request_text() {
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
        let card = build_story_card(&session);
        assert!(
            !card.summary.contains("Signature"),
            "summary leaked scaffold: {}",
            card.summary
        );
        assert!(
            !card
                .request_text
                .as_deref()
                .unwrap_or_default()
                .contains("Signature"),
            "request text leaked scaffold"
        );
        assert!(!card.episode_json.contains("completion_status"));
        // Phase 3's fused sentence prefixes the scrubbed text with the
        // outcome verb ("Partly done: ...") rather than starting with it
        // verbatim.
        assert!(card.summary.contains("generate a cover photo"));
    }

    #[test]
    fn empty_db_renders_no_session_story_signal() {
        let storage = Storage::open_memory().unwrap();
        let html = render(&storage, crate::storage::recap_feeds::ConsumptionMode::Off);
        assert!(html.contains("No session has enough story signal"));
        assert!(html.contains("<!doctype html>"));
        assert!(html.contains("CSR Dream Journal"));
        assert_no_external_resources(&html);
        assert!(
            !html.contains("class=\"index-row"),
            "an empty corpus must not render any index rows"
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
        let html = render(&storage, crate::storage::recap_feeds::ConsumptionMode::Off);
        assert!(html.contains("Show the session story"));
        assert_eq!(html.matches("class=\"index-row").count(), 1);
        assert_eq!(html.matches("class=\"detail-pane\"").count(), 1);
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
        // An anchor alone (no request/narrative) is THIN under Q2 — pair it
        // with a request+outcome so this session is RICH and its artifact
        // bento is reachable in the detail pane.
        seed_episode(
            &storage,
            "event-session",
            "2026-01-02T01:00:00Z",
            "touch the foo symbol",
            "done",
            &[],
            &[],
        );
        seed_anchor(&storage, "event-session", "2026-01-02T01:00:00Z", "foo");

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert!(html.contains("foo"));
        assert!(html.contains("⌗ superseded bbb"));
        assert!(html.contains("lib.rs"));
        assert!(html.contains("no receipt") || html.matches("⌗ superseded").count() >= 1);
    }

    /// Symbol names are user code identifiers interpolated into HTML; minijinja
    /// auto-escaping depends on TEMPLATE_NAME ending in `.html`. This regression
    /// test fails if that wiring is ever broken (e.g. the template gets renamed).
    /// Extended (finding: server-rendering user text is new attack surface once
    /// the mailbox stopped going through mermaid's own escaping-free SVG path):
    /// the ASK stage's request quote and the thin-rollup's prompt excerpt are
    /// new sinks and must escape identically.
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
        seed_episode(
            &storage,
            "hostile-session",
            "2026-01-03T01:00:00Z",
            payload,
            "done",
            &[],
            &[],
        );
        seed_anchor(&storage, "hostile-session", "2026-01-03T01:00:00Z", payload);
        // New sink: a thin (registry-only) session whose prompt is hostile.
        seed_registry(&storage, "hostile-thin", "2026-01-03T02:00:00Z", payload);

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert!(!html.contains(payload), "raw script tag leaked into HTML");
        // minijinja HTML-escapes `/` as well: </script> -> &lt;&#x2f;script&gt;
        assert!(html.contains("&lt;script&gt;alert(1)&lt;&#x2f;script&gt;"));
        assert!(
            html.contains("\\u003cscript\\u003ealert(1)\\u003c/script\\u003e"),
            "JSON script context must neutralize HTML delimiters"
        );
        let episode_data = episode_data_blocks(&html);
        assert_eq!(episode_data.len(), 1);
        assert_eq!(episode_data[0]["artifacts"][0]["symbol"], payload);
        // The thin-rollup excerpt must escape too.
        assert!(html.contains("class=\"thin-excerpt\""));
    }

    #[test]
    fn report_no_open_writes_file_to_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        let out_path = dir.path().join("dream-test.html");

        let written = run_report(&storage, dir.path(), Some(out_path.clone()), true).unwrap();
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

        let written = run_report(&storage, dir.path(), Some(out_path.clone()), true).unwrap();
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
    fn session_with_request_and_outcome_only_renders_ask_and_outcome_stages() {
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

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert!(html.contains(r#"data-stage="S_INPUT""#));
        assert!(html.contains(r#"data-stage="S_OUT""#));
        assert!(
            !html.contains(r#"data-stage="S_DELIB""#),
            "an outcome with no narrative and no investigated files must not draw a DELIBERATION card"
        );
        assert!(
            !html.contains(r#"data-stage="S_STEER""#),
            "a session with no todos must not draw a STEER card"
        );
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
                &format!("no-signal-{index}"),
                &format!("2026-01-0{}T01:00:00Z", index + 1),
                "",
            );
        }

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );

        assert_eq!(html.matches("class=\"index-row").count(), 50);
        assert!(html.contains("2 older sessions omitted"));
        assert!(html.contains("2 sessions omitted — no prompt, no artifact, no episode"));
    }

    // ---- §7.1 pinned Phase 1 tests ---------------------------------------

    /// Pinned test 1: replaces the old "the vendored Mermaid runtime must be
    /// embedded exactly once" assertion — the runtime, and everything that
    /// rendered it, is gone.
    #[test]
    fn mermaid_runtime_is_gone_and_report_is_small() {
        let storage = Storage::open_memory().unwrap();
        seed_rich_sessions(&storage, 50);

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let lower = html.to_ascii_lowercase();
        assert!(!lower.contains("statediagram"));
        assert!(!lower.contains("__esbuild_esm_mermaid_nm"));
        assert!(!lower.contains("mermaid"));
        assert!(
            html.len() < 2_000_000,
            "50-session mailbox report is {} bytes, expected < 2MB",
            html.len()
        );
        assert_no_external_resources(&html);
    }

    /// Pinned test 2.
    #[test]
    fn index_rows_and_detail_panes_are_one_to_one() {
        let storage = Storage::open_memory().unwrap();
        seed_rich_sessions(&storage, 6);

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert_eq!(html.matches("class=\"index-row").count(), 6);
        assert_eq!(html.matches("class=\"detail-pane\"").count(), 6);

        let (index_html, detail_html) = split_panes(&html);
        let mut index_ids = attr_values(index_html, r#"data-session=""#);
        let mut detail_ids = attr_values(detail_html, r#"data-session=""#);
        index_ids.sort_unstable();
        detail_ids.sort_unstable();
        assert_eq!(index_ids.len(), 6);
        assert_eq!(
            index_ids, detail_ids,
            "every index row must have exactly one matching detail pane"
        );
    }

    /// Pinned test 3.
    #[test]
    fn thin_sessions_are_grouped_below_and_never_in_the_main_index() {
        let storage = Storage::open_memory().unwrap();
        // Artifacts only, no request/narrative -> THIN (Q2).
        seed_anchor(
            &storage,
            "artifact-only-session",
            "2026-02-04T01:00:00Z",
            "retired_symbol",
        );
        // Registry-only with a first_prompt -> THIN (Q2 explicitly promotes
        // this out of "omitted" and into the rollup).
        seed_registry(
            &storage,
            "registry-only-session",
            "2026-02-04T02:00:00Z",
            "check the logs",
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert!(!html.contains("class=\"index-row"));
        assert!(html.contains("class=\"thin-group\""));
        assert!(html.contains("2 thin sessions"));
    }

    /// Pinned test 4.
    #[test]
    fn sessions_with_no_evidence_at_all_are_counted_not_rendered() {
        let storage = Storage::open_memory().unwrap();
        seed_registry(&storage, "no-evidence-session", "2026-02-05T01:00:00Z", "");

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert!(!html.contains("class=\"index-row"));
        assert!(!html.contains("class=\"thin-group\""));
        assert!(html.contains("1 sessions omitted"));
    }

    /// Pinned test 5.
    #[test]
    fn index_rows_carry_the_four_sort_keys() {
        let storage = Storage::open_memory().unwrap();
        seed_episode(
            &storage,
            "sort-key-success",
            "2026-02-06T01:00:00Z",
            "ship the feature",
            "shipped to production",
            &[],
            &[],
        );
        seed_episode(
            &storage,
            "sort-key-failed",
            "2026-02-07T01:00:00Z",
            "ship the other feature",
            "failed at the gate",
            &[],
            &[],
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let (index_html, _) = split_panes(&html);
        let ts_values = attr_values(index_html, r#"data-ts=""#);
        let project_values = attr_values(index_html, r#"data-project=""#);
        let outcome_values = attr_values(index_html, r#"data-outcome=""#);
        let day_values = attr_values(index_html, r#"data-day=""#);
        assert_eq!(ts_values.len(), 2);
        assert_eq!(project_values.len(), 2);
        assert_eq!(outcome_values.len(), 2);
        assert_eq!(day_values.len(), 2);
        for value in &ts_values {
            assert!(!value.is_empty());
        }
        for value in &project_values {
            assert!(!value.is_empty());
        }
        for value in &day_values {
            assert!(!value.is_empty());
        }
        for value in &outcome_values {
            assert!(matches!(*value, "success" | "partial" | "failed" | "noted"));
        }
    }

    /// Pinned test 6 — the single most load-bearing honest-degradation
    /// assertion in this phase.
    #[test]
    fn instrumentation_line_drops_unknown_segments() {
        let storage = Storage::open_memory().unwrap();
        seed_episode(
            &storage,
            "todos-and-artifacts",
            "2026-02-08T01:00:00Z",
            "wire the two things",
            "done",
            &[],
            &[
                ("wire thing one", "completed"),
                ("wire thing two", "pending"),
            ],
        );
        seed_anchor(
            &storage,
            "todos-and-artifacts",
            "2026-02-08T01:00:00Z",
            "sym_a",
        );
        seed_anchor(
            &storage,
            "todos-and-artifacts",
            "2026-02-08T01:00:00Z",
            "sym_b",
        );

        seed_episode(
            &storage,
            "no-todos-session",
            "2026-02-09T01:00:00Z",
            "just note this",
            "noted for later",
            &[],
            &[],
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let (index_html, _) = split_panes(&html);

        let short_a = short_oid("todos-and-artifacts");
        let row_a_start = index_html
            .find(&format!(r#"data-session="{short_a}""#))
            .unwrap();
        let row_a_end = index_html[row_a_start..]
            .find("</button>")
            .map(|offset| row_a_start + offset)
            .unwrap();
        let row_a = &index_html[row_a_start..row_a_end];
        assert!(
            row_a.contains("✓1 ○1 tasks"),
            "expected done/open task counts, got: {row_a}"
        );
        assert!(row_a.contains("2 artifacts"));
        assert!(!row_a.contains("file"), "no investigated evidence: {row_a}");
        assert!(
            !row_a.contains("error"),
            "no error evidence in phase 1: {row_a}"
        );

        let short_b = short_oid("no-todos-session");
        let row_b_start = index_html
            .find(&format!(r#"data-session="{short_b}""#))
            .unwrap();
        let row_b_end = index_html[row_b_start..]
            .find("</button>")
            .map(|offset| row_b_start + offset)
            .unwrap();
        let row_b = &index_html[row_b_start..row_b_end];
        assert!(
            !row_b.contains("tasks"),
            "an empty todo list must render no tasks segment: {row_b}"
        );
        assert!(
            !row_b.contains("✓0") && !row_b.contains("○0"),
            "an empty todo list must never render ✓0 ○0: {row_b}"
        );
        let instrumentation = row_b
            .split(r#"<span class="instrumentation">"#)
            .nth(1)
            .and_then(|tail| tail.split("</span>").next())
            .unwrap_or_default();
        assert!(
            instrumentation.is_empty(),
            "a session with no todos/artifacts/files must render an empty \
             instrumentation line, got: {instrumentation:?}"
        );
    }

    /// Pinned test 7.
    #[test]
    fn day_headers_render_server_side_in_recency_order() {
        let storage = Storage::open_memory().unwrap();
        let today = chrono::Utc::now();
        let yesterday = today - chrono::Duration::days(1);
        let today_a = today.format("2026-%m-%dT08:00:00Z").to_string();
        let today_b = today.format("2026-%m-%dT18:00:00Z").to_string();
        // Force onto the *current* month/day so the "TODAY" comparison in
        // `day_label` (which compares against real `chrono::Utc::now()`)
        // actually lands on today, without depending on which year this
        // suite happens to run in.
        let today_a = format!("{}-{}", chrono::Utc::now().format("%Y"), &today_a[5..]);
        let today_b = format!("{}-{}", chrono::Utc::now().format("%Y"), &today_b[5..]);
        let yesterday_ts = format!(
            "{}-{}",
            yesterday.format("%Y"),
            yesterday.format("%m-%dT09:00:00Z")
        );

        seed_episode(
            &storage,
            "today-session-a",
            &today_a,
            "ask a",
            "done",
            &[],
            &[],
        );
        seed_episode(
            &storage,
            "today-session-b",
            &today_b,
            "ask b",
            "done",
            &[],
            &[],
        );
        seed_episode(
            &storage,
            "yesterday-session",
            &yesterday_ts,
            "ask c",
            "done",
            &[],
            &[],
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert_eq!(html.matches("class=\"group-header\"").count(), 2);
        let first_header_start = html.find("class=\"group-header\"").unwrap();
        let first_header = &html[first_header_start..first_header_start + 200];
        assert!(
            first_header.contains("TODAY ·"),
            "newest header must read TODAY, got: {first_header}"
        );
        let today_pos = html.find("TODAY ·").unwrap();
        let yesterday_pos = html.find("YESTERDAY ·").unwrap();
        assert!(
            today_pos < yesterday_pos,
            "TODAY must render before YESTERDAY"
        );
    }

    /// Pinned test 9 (helper extension only — `assert_no_external_resources`
    /// above now also rejects `<link`, `@import`, `url(http`; exercised by
    /// every test in this module that calls it).
    #[test]
    fn assert_no_external_resources_rejects_link_import_and_css_url() {
        let clean = "<html><body>hi</body></html>";
        assert_no_external_resources(clean);
    }

    /// Pinned test 10.
    #[test]
    fn render_is_deterministic() {
        let storage = Storage::open_memory().unwrap();
        seed_rich_sessions(&storage, 5);
        seed_anchor(
            &storage,
            "rich-session-000",
            "2026-04-01T05:00:00Z",
            "det_sym",
        );

        let html_a = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let html_b = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        assert_eq!(
            html_a, html_b,
            "rendering the same fixture twice must be byte-identical"
        );
    }

    // ---- §7.2 pinned Phase 2 tests ---------------------------------------

    /// Pinned test 15 — the single most load-bearing test in this phase.
    #[test]
    fn errors_segment_absent_when_unmeasured_and_zero_when_measured() {
        let storage = Storage::open_memory().unwrap();
        seed_episode_with_extra(
            &storage,
            "measured-zero-session",
            "2026-03-01T01:00:00Z",
            "ship the thing",
            "done",
            serde_json::json!({
                "error_count": 0,
                "top_errors": [],
                "steer_count": 0,
                "steers": [],
            }),
        );
        seed_episode(
            &storage,
            "unmeasured-session",
            "2026-03-02T01:00:00Z",
            "note the other thing",
            "noted",
            &[],
            &[],
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let (index_html, _) = split_panes(&html);

        let short_measured = short_oid("measured-zero-session");
        let row_start = index_html
            .find(&format!(r#"data-session="{short_measured}""#))
            .unwrap();
        let row_end = index_html[row_start..]
            .find("</button>")
            .map(|offset| row_start + offset)
            .unwrap();
        let row_measured = &index_html[row_start..row_end];
        assert!(
            row_measured.contains("0 errors"),
            "a tool-verified zero must render, got: {row_measured}"
        );

        let short_unmeasured = short_oid("unmeasured-session");
        let row_start = index_html
            .find(&format!(r#"data-session="{short_unmeasured}""#))
            .unwrap();
        let row_end = index_html[row_start..]
            .find("</button>")
            .map(|offset| row_start + offset)
            .unwrap();
        let row_unmeasured = &index_html[row_start..row_end];
        assert!(
            !row_unmeasured.contains("error"),
            "an unmeasured session must render no errors segment at all, got: {row_unmeasured}"
        );
    }

    /// Pinned test 16.
    #[test]
    fn degraded_error_count_is_tilde_marked() {
        let storage = Storage::open_memory().unwrap();
        seed_episode_with_extra(
            &storage,
            "degraded-session",
            "2026-03-03T01:00:00Z",
            "chase the flaky test",
            "partial",
            serde_json::json!({
                "error_signatures": ["Error: boom", "Error: again", "panic: whoops"],
            }),
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let (index_html, _) = split_panes(&html);
        let short = short_oid("degraded-session");
        let row_start = index_html
            .find(&format!(r#"data-session="{short}""#))
            .unwrap();
        let row_end = index_html[row_start..]
            .find("</button>")
            .map(|offset| row_start + offset)
            .unwrap();
        let row = &index_html[row_start..row_end];
        assert!(
            row.contains("~3 errors"),
            "the degraded fallback must be tilde-marked, never a bare count: {row}"
        );
        assert!(
            !row.contains(">3 errors<"),
            "a bare, non-tilde '3 errors' must never render for the degraded fallback: {row}"
        );
        assert!(
            row.contains("pattern-matched, not tool-verified"),
            "the degraded disclosure title must be present: {row}"
        );
    }

    /// Pinned test 17.
    #[test]
    fn steer_lines_are_capped_at_three_with_a_count_line() {
        let storage = Storage::open_memory().unwrap();
        seed_episode_with_extra(
            &storage,
            "steer-session",
            "2026-03-04T01:00:00Z",
            "wire the feature end to end",
            "done",
            serde_json::json!({
                "todos": [{"content": "wire it", "status": "completed"}],
                "error_count": 0,
                "top_errors": [],
                "steer_count": 5,
                "steers": [
                    {"turn": 3, "text": "no, hindi not english"},
                    {"turn": 7, "text": "stop, show me the profile"},
                    {"turn": 11, "text": "use the october script"},
                ],
            }),
        );

        let html = render(
            &storage,
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let (_, detail_html) = split_panes(&html);
        let short = short_oid("steer-session");
        let pane_start = detail_html
            .find(&format!(r#"data-session="{short}""#))
            .unwrap();
        let pane_end = detail_html[pane_start..]
            .find("</article>")
            .map(|offset| pane_start + offset)
            .unwrap();
        let pane = &detail_html[pane_start..pane_end];

        assert!(
            pane.contains("5 mid-flight steers"),
            "the count line must show the true total, not the capped list length: {pane}"
        );
        assert_eq!(
            pane.matches("class=\"steer-line\"").count(),
            3,
            "at most 3 steer lines must render regardless of steer_count: {pane}"
        );
    }

    /// Pinned test 18.
    #[test]
    fn unscanned_session_shows_the_warning_chip() {
        let storage = Storage::open_memory().unwrap();
        let projects_dir = tempfile::tempdir().unwrap();
        let project_dir = projects_dir.path().join("proj");
        std::fs::create_dir_all(&project_dir).unwrap();

        // A transcript that exists but is oversized: never parsed, must
        // surface the warning chip.
        let oversized_path = project_dir.join("oversized-sess.jsonl");
        std::fs::write(&oversized_path, "").unwrap();
        {
            let f = std::fs::OpenOptions::new()
                .write(true)
                .open(&oversized_path)
                .unwrap();
            f.set_len(crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES + 1)
                .unwrap();
        }
        seed_episode(
            &storage,
            "oversized-sess",
            "2026-03-05T01:00:00Z",
            "process the huge transcript",
            "noted",
            &[],
            &[],
        );

        // A session with no transcript on disk at all: plan §3.4 row 1 —
        // absent segment, but no chip (there is nothing to disclose "not
        // scanned" about).
        seed_episode(
            &storage,
            "no-transcript-sess",
            "2026-03-06T01:00:00Z",
            "a session whose transcript moved away",
            "noted",
            &[],
            &[],
        );

        let html = render_with_projects_dir(
            &storage,
            projects_dir.path(),
            crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
        );
        let (_, detail_html) = split_panes(&html);

        let oversized_short = short_oid("oversized-sess");
        let pane_start = detail_html
            .find(&format!(r#"data-session="{oversized_short}""#))
            .unwrap();
        let pane_end = detail_html[pane_start..]
            .find("</article>")
            .map(|offset| pane_start + offset)
            .unwrap();
        assert!(
            detail_html[pane_start..pane_end].contains("not scanned"),
            "an oversized, never-parsed transcript must show the warning chip"
        );

        let missing_short = short_oid("no-transcript-sess");
        let pane_start = detail_html
            .find(&format!(r#"data-session="{missing_short}""#))
            .unwrap();
        let pane_end = detail_html[pane_start..]
            .find("</article>")
            .map(|offset| pane_start + offset)
            .unwrap();
        assert!(
            !detail_html[pane_start..pane_end].contains("not scanned"),
            "a session with no transcript on disk at all must show no warning chip"
        );
    }
}
