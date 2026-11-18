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
use rusqlite::Connection;
use serde::Serialize;

use crate::storage::dream_items::{self, DreamEvidence, DreamItem, DreamItemGrade};
use crate::storage::dream_report::{self, StoryArtifact, StorySession};
use crate::storage::witness_verdicts::{self, DayDigest, SinceThenConclusion, VerdictKind};
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
        {% if group.digest %}<p class="dream-digest" title="{{ group.digest_title }}">{{ group.digest }}</p>{% endif %}
        {% for row in group.rows %}
        <button type="button" class="index-row{% if row.selected %} selected{% endif %}" data-session="{{ row.session_short }}" data-ts="{{ row.ts }}" data-project="{{ row.project }}" data-outcome="{{ row.outcome_slug }}" data-day="{{ row.day }}" role="option" aria-selected="{% if row.selected %}true{% else %}false{% endif %}">
          <span class="row-head"><span class="glyph {{ row.outcome_slug }}">{{ row.glyph }}</span><span class="project-pill">{{ row.project }}</span></span>
          <span class="sentence">{{ row.sentence }}</span>
          <span class="instrumentation">{% for segment in row.segments %}<span class="chip{% if segment.degraded %} degraded{% endif %}"{% if segment.popover %} data-popover="{{ segment.popover }}"{% endif %}{% if segment.title %} title="{{ segment.title }}"{% endif %}>{{ segment.text }}</span>{% if not loop.last %} · {% endif %}{% endfor %}</span>
          {% if row.has_todo_bar %}<span class="microchart" aria-label="todo completion" style="--pct: {{ row.todo_pct }}%;"><span></span></span>{% endif %}
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
          <p class="detail-meta"><span class="project-pill">{{ pane.project }}</span><span>{{ pane.date }}</span><span>{{ pane.session_short }}</span>{% if pane.outcome_badge %}<span class="outcome-badge {{ pane.outcome_slug }}" title="{{ pane.date }}">ENDED: {{ pane.outcome_badge }}</span>{% endif %}{% if pane.now_badge %}<span class="now-badge {{ pane.now_badge_slug }}" title="derived from SINCE THEN evidence, not a health check">{{ pane.now_badge }}</span>{% endif %}{% if pane.unscanned %}<span class="warn-chip" title="transcript not scanned for instrumentation">&#9888; not scanned</span>{% endif %}</p>
        </header>
        {% if pane.touch_next %}
        <div class="touch-next" aria-label="Touch next">
          <p class="touch-next-head">Touch next</p>
          <ul class="touch-next-list">
            {% for line in pane.touch_next %}<li>{{ line }}</li>{% endfor %}
          </ul>
        </div>
        {% else %}
        <p class="touch-next-empty">nothing evidenced to touch — no open todos, blockers, or adverse verdicts</p>
        {% endif %}
        <div class="stage-rail">
          {% for stage in pane.stages %}
          <details class="stage-card {{ stage.kind }}" data-stage="{{ stage.id }}"{% if stage.open %} open{% endif %}>
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
                  {% for todo in stage.todos_open %}<li class="task {{ todo.slug }}"><span class="glyph">{{ todo.glyph }}</span>{{ todo.content }}</li>{% endfor %}
                  {% if stage.todos_completed_fold %}
                  <li class="task-fold"><details class="completed-fold"><summary>{{ stage.todos_completed | length }} completed</summary><ul class="task-list nested">{% for todo in stage.todos_completed %}<li class="task {{ todo.slug }}"><span class="glyph">{{ todo.glyph }}</span>{{ todo.content }}</li>{% endfor %}</ul></details></li>
                  {% else %}
                    {% for todo in stage.todos_completed %}<li class="task {{ todo.slug }}"><span class="glyph">{{ todo.glyph }}</span>{{ todo.content }}</li>{% endfor %}
                  {% endif %}
                </ul>
                {% if stage.steer_count %}
                <p class="steer-count">{{ stage.steer_count }} mid-flight steer{% if stage.steer_count != 1 %}s{% endif %}</p>
                {% for steer in stage.steers %}<p class="steer-line post-it">&#8627; turn {{ steer.turn }} &ldquo;{{ steer.text }}&rdquo;</p>{% endfor %}
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
        {% if pane.since_then %}
        <div class="since-then" aria-label="Since then">
          <p class="since-then-head">Since then</p>
          <p class="since-then-kpi">
            <span>{{ pane.since_then.kpi.symbols }} symbol{% if pane.since_then.kpi.symbols != 1 %}s{% endif %} touched</span>
            {% if pane.since_then.kpi.superseded %}<span>{{ pane.since_then.kpi.superseded }} superseded since</span>{% endif %}
            {% if pane.since_then.kpi.witnessed_unverified %}<span>{{ pane.since_then.kpi.witnessed_unverified }} witnessed, unverified</span>{% endif %}
            {% if pane.since_then.kpi.unverified %}<span>{{ pane.since_then.kpi.unverified }} unverified</span>{% endif %}
          </p>
          <div class="since-then-rows">
          {% for row in pane.since_then.rows %}
            <div class="since-then-row">
              <span class="symbol">{{ row.symbol }}</span>{% if row.anchor_only %} <span class="anchor-only-tag">(anchor-only)</span>{% endif %}
              <span class="file">{{ row.file }}</span>
              <p class="conclusion {{ row.conclusion_slug }}">{{ row.conclusion_label }}</p>
              {% if row.so_clause %}<p class="so-clause">{{ row.so_clause }}</p>{% endif %}
              <code class="why-hint">{{ row.why_hint }}</code>
            </div>
          {% endfor %}
          </div>
          {% if pane.since_then.more %}<p class="since-then-more">&#9656; +{{ pane.since_then.more }} more symbols</p>{% endif %}
        </div>
        {% endif %}
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

// --- Journal v3 "Dreams Gateway" Phase 2 — home + detail view markup -------
//
// Spliced into the document right after `<div class="wrap">`, ahead of the
// existing hero/mailbox markup (which becomes `#session-log-view`, wrapped
// unchanged below in `render_html`). Home is the new default view (no
// `hidden` attribute); detail starts hidden, one `<article>` per qualifying
// item, first unhidden as the same fallback-selection idiom the mailbox's
// own detail panes already use (`{% if not loop.first %} hidden{% endif %}`)
// — relevant only if JS never runs to correct it via the hash route.

const DREAMS_HOME_TEMPLATE: &str = r#"<div id="dreams-home-view" class="csr-view">
    <div class="hero-title-row"><h1>☾ Dreams</h1></div>
    {% if data.dream_groups %}
    <p class="dreams-sub">Incomplete work the night pass concluded on. {{ data.dream_total_label }} · {{ data.dream_project_label }}. <a class="session-log-link" href='#/log'>session log &rarr;</a></p>
    {% for group in data.dream_groups %}
    <div class="dream-project-group">
      <p class="dream-project-eyebrow">&#9660; <b>{{ group.project }}</b> — {{ group.count_label }}</p>
      <div class="dream-cards">
        {% for card in group.cards %}
        <a class="dream-card" href='#/item/{{ card.id }}' data-item="{{ card.id }}">
          <span class="dream-grade {{ card.grade_slug }}">{{ card.grade_label }}</span>
          <div class="dream-item-text">{{ card.item }}</div>
          {% for line in card.dream_lines %}
          <div class="dream-line">{{ line }}</div>
          {% endfor %}
          <div class="dream-meta">{{ card.meta }}</div>
        </a>
        {% endfor %}
      </div>
    </div>
    {% endfor %}
    {% else %}
    <p class="dreams-empty">No dreams matched open work — <a class="session-log-link" href='#/log'>session log &rarr;</a></p>
    {% endif %}
  </div>
  "#;

const DREAM_DETAIL_TEMPLATE: &str = r#"<div id="dream-detail-view" class="csr-view" hidden>
    {% for item in data.dream_details %}
    <article class="dream-detail-item" data-item="{{ item.id }}"{% if not loop.first %} hidden{% endif %}>
      <a class="dream-back" href='#/'>&larr; dreams</a>
      <div class="dream-detail-head touch-next">
        <p class="touch-next-head">&#9733; TOUCH NEXT</p>
        <p class="dream-detail-title">{{ item.title }}</p>
      </div>
      <p class="dream-detail-project">{{ item.project }} · {{ item.grade_label }}</p>
      <div class="dream-evidence-list" aria-label="Evidence">
        {% for line in item.evidence_lines %}
        <p class="dream-evidence-row">{{ line }}</p>
        {% endfor %}
      </div>
      <!-- Phase 3 mounts the AST change tree (solid witnessed / dotted candidate nodes) here. -->
      <div class="dream-ast-slot" data-slot="ast-tree" aria-hidden="true"></div>
      <!-- Phase 3 mounts the churn heat map (measured touch density) here. -->
      <div class="dream-heat-slot" data-slot="heat-map" aria-hidden="true"></div>
    </article>
    {% endfor %}
  </div>
  "#;

const MAILBOX_CSS: &str = r#"
  .mailbox {
    display: grid; grid-template-columns: 340px minmax(0, 1fr);
    gap: 0; border: 1px solid var(--border); border-radius: 12px; overflow: hidden;
    align-items: stretch;
  }
  /* Finding 2 (pane visibility on selection): both panes are independently
     scrollable and bounded to the viewport minus the hero above the
     mailbox, so selecting a session never requires scrolling the whole page
     to see it. 260px is a conservative estimate of the hero's rendered
     height (wrap top padding + title row + subtitle + meta-row + hero
     margin) — a sticky-friendly approach rather than a JS measurement, so it
     degrades gracefully (a little extra/unused scroll room) if the hero
     wraps to an extra line on a narrow viewport. Without JS the panes still
     render stacked/scrollable at <=760px below — no content is ever gated
     on script execution. */
  .index-pane, .detail-pane-wrap {
    max-height: calc(100vh - 260px);
    overflow-y: auto;
  }
  .index-pane {
    border-right: 1px solid var(--border); background: var(--bg-card);
    display: flex; flex-direction: column; min-width: 0; overflow-x: hidden;
  }
  .sort-controls {
    display: flex; align-items: center; gap: 0.4rem; padding: 0.75rem 0.9rem;
    border-bottom: 1px solid var(--border); font-size: 0.75rem;
    position: sticky; top: 0; z-index: 2; background: var(--bg-card);
    backdrop-filter: var(--glass-blur); -webkit-backdrop-filter: var(--glass-blur);
  }
  .sort-label { color: var(--fg-muted); text-transform: uppercase; letter-spacing: 0.05em; font-size: 0.68rem; }
  .sort-btn {
    background: none; border: 1px solid var(--border); border-radius: 999px;
    color: var(--fg-muted); font: inherit; font-size: 0.72rem; padding: 0.2rem 0.6rem;
    cursor: pointer;
  }
  .sort-btn.active { color: var(--accent); border-color: var(--accent); font-weight: 700; }
  /* flex-shrink: 0 is load-bearing: `overflow-x: hidden` flips this flex
     child's min-height:auto to 0, and inside the height-capped index column
     it would otherwise shrink to a sliver while the thin rollup (min-height
     intact) keeps its full height — rendering the rollup as the visible
     "top" of the index. */
  .session-rows { padding: 0.4rem 0; overflow-x: hidden; flex-shrink: 0; }
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
    overflow: hidden; overflow-wrap: anywhere; font-size: 0.875rem; line-height: 1.35; font-weight: 550;
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
  /* Finding 3: NOW states what the dream has concluded SINCE the session
     ended — evidence-gated, never a health claim. "superseded" reuses the
     same adverse-red as a superseded outcome; "unverified" is deliberately
     neutral (no verdict type proves current health, so it never earns the
     positive/green treatment). */
  .now-badge {
    border-radius: 999px; font-size: 0.62rem; font-weight: 800; padding: 0.1rem 0.45rem;
    text-transform: uppercase; letter-spacing: 0.04em; border: 1px solid var(--border);
  }
  .now-badge.superseded { color: var(--red); background: var(--red-bg); border-color: transparent; }
  .now-badge.unverified { color: var(--fg-muted); }
  .warn-chip {
    color: var(--amber); border: 1px solid var(--amber); border-radius: 999px;
    padding: 0.05rem 0.5rem; font-size: 0.68rem; font-weight: 700;
  }
  .chip.degraded { border-bottom: 1px dotted var(--amber); }
  /* Finding 1: the page's answer to "what should I do next?" — quiet but
     first, directly below the detail header, before the stage rail. Muted
     purple (--accent/--purple) marks it as the dream's own voice, same
     register as the SINCE THEN panel's conclusion text, distinct from the
     session's own editorial-black narrative. */
  .touch-next { margin: 0 0 1rem; }
  .touch-next-head {
    font-size: 0.68rem; font-weight: 800; letter-spacing: 0.06em;
    text-transform: uppercase; color: var(--purple); margin: 0 0 0.3rem;
  }
  .touch-next-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 0.25rem; }
  .touch-next-list li {
    font-size: 0.875rem; color: var(--ink); line-height: 1.4; overflow-wrap: anywhere;
    padding-left: 0.9rem; position: relative;
  }
  .touch-next-list li::before {
    content: "\2192"; position: absolute; left: 0; color: var(--purple);
  }
  /* The honest empty state (abstention-first): quiet, small, never styled
     as an invitation to invent advice. */
  .touch-next-empty {
    font-size: 0.76rem; color: var(--fg-muted); font-style: italic; margin: 0 0 1rem;
  }
  .steer-count { font-weight: 700; margin: 0.5rem 0 0.2rem; }
  .steer-line { margin: 0 0 0.2rem; color: var(--fg-muted); overflow-wrap: anywhere; }
  .outcome-errors { font-weight: 700; margin: 0 0 0.35rem; }
  .top-error {
    color: var(--fg-muted); margin: 0 0 0.35rem; overflow-wrap: anywhere;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.78rem;
    display: -webkit-box; -webkit-line-clamp: 3; -webkit-box-orient: vertical;
    overflow: hidden;
  }
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
  .stage-body { padding: 0 0.85rem 0.75rem; font-size: 0.875rem; line-height: 1.5; overflow-wrap: anywhere; }
  .stage-body .quote { font-style: italic; margin: 0; }
  .stage-body .narrative { margin: 0 0 0.5rem; }
  .file-list, .task-list { list-style: none; margin: 0; padding: 0; display: grid; gap: 0.3rem; }
  .file-list li, .task-list li { font-size: 0.875rem; overflow-wrap: anywhere; }
  .file-list li.more { color: var(--fg-muted); }
  .task { display: flex; gap: 0.45rem; }
  .task .glyph { font-weight: 800; flex: none; width: 1.1em; text-align: center; }
  .task.done .glyph { color: var(--green); }
  .task.doing .glyph { color: var(--amber); }
  .task.open .glyph { color: var(--fg-muted); }
  .task.unknown .glyph { color: var(--fg-muted); }
  .task.done { color: var(--fg-muted); }
  /* Finding 4: completed todos beyond the fold threshold nest behind a
     closed <details> — same visual register as a task line, not a whole new
     component. All rows stay in the DOM either way (no-JS readable). */
  .task-fold { list-style: none; }
  .completed-fold summary {
    list-style: none; cursor: pointer; font-size: 0.875rem; color: var(--fg-muted);
  }
  .completed-fold summary::-webkit-details-marker { display: none; }
  .completed-fold .task-list.nested { margin-top: 0.3rem; padding-left: 1.1em; }
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

  /* ---- Journal v2 Phase 4: glassmorphic brand pass ----
     Chrome + cards register (landing glass, docs-site DESIGN_SYSTEM.md):
     translucent blur, soft radius, gentle hover lift. Long-form text inside
     cards (the detail-pane headline) borrows the DocPage editorial register
     — serif headline over quiet prose. Additive only: every rule above still
     stands, these just layer the polish on top via the later cascade
     position (this block is the tail of MAILBOX_CSS, spliced in right
     before the document's closing style tag — literally naming that tag
     here would truncate the HTML raw-text element at parse time, hiding
     every rule after it; that is not hypothetical, it is exactly the bug
     this comment replaced). */
  .totals .stat, .event, .forgotten-row, .empty-state, .meta-row code,
  .index-pane, .stage-card, .artifact-tile, .chip-popover, .thin-group {
    backdrop-filter: var(--glass-blur);
    -webkit-backdrop-filter: var(--glass-blur);
  }
  .mailbox {
    border-radius: var(--radius-pane);
    box-shadow: 0 4px 30px rgba(42, 38, 66, 0.1);
  }
  .stage-card, .artifact-tile, .thin-group,
  .totals .stat, .event, .forgotten-row, .empty-state {
    border-radius: var(--radius-tile);
  }
  .stage-card, .artifact-tile {
    transition: transform 180ms cubic-bezier(0.16, 1, 0.3, 1), border-color 180ms ease-out;
  }
  .stage-card:hover, .artifact-tile:hover {
    transform: translateY(-2px);
    border-color: var(--highlight);
  }
  .index-row.selected { background: var(--highlight); }

  /* Editorial small-caps kicker labels — same muted-purple-adjacent grey the
     docs-site DocPage register uses for breadcrumbs/section labels. */
  .group-header, .sort-label, .stage-label, .thin-rollup-header, .thin-count {
    color: var(--label);
  }

  /* DocPage editorial register: serif headline for the one piece of
     long-form text a card carries — the fused ask->outcome sentence. Index
     rows stay in the landing-glass chrome register (dense, scannable, sans)
     — only the detail pane's headline gets the editorial treatment. */
  header.hero h1, .detail-pane .sentence,
  .hero-title-row h1, .dream-detail-title {
    font-family: var(--serif);
  }
  .detail-pane .sentence {
    font-weight: 700;
    color: var(--ink);
  }

  /* Steer quotes as post-it notes (plan: "steers ARE handwritten human
     interjections; the metaphor is exact"). Alternating lavender/rose tints
     and rotation so a run of three doesn't read as one flat block. */
  .steer-line.post-it {
    display: inline-block;
    max-width: 100%;
    font-family: var(--cursive);
    font-size: 1.05rem;
    line-height: 1.35;
    color: var(--ink);
    background: var(--postit-lavender);
    border-radius: 4px;
    padding: 0.45rem 0.75rem;
    margin: 0 0 0.6rem;
    transform: rotate(-3deg);
    box-shadow: 2px 3px 8px rgba(42, 38, 66, 0.15);
    transition: transform 160ms ease-out;
  }
  .steer-line.post-it:nth-of-type(even) {
    background: var(--postit-rose);
    transform: rotate(2deg);
  }
  .steer-line.post-it:hover { transform: rotate(0deg) scale(1.03); }

  /* Todo-completion microchart (real data only — server omits the element
     entirely when a session has no todos; never a fabricated 0%). */
  .microchart {
    display: block;
    margin-top: 0.3rem;
    height: 4px;
    border-radius: 999px;
    background: rgba(26, 26, 46, 0.08);
    overflow: hidden;
  }
  .microchart > span {
    display: block;
    height: 100%;
    width: var(--pct, 0%);
    background: var(--sage);
    border-radius: 999px;
  }

  /* Journal v2 Phase 5: SINCE THEN payback panel — the dream's
     re-deliberation, not a session log. Reuses the Phase 4 glass tile
     register (--bg-card/--border/--radius-tile, same as .artifact-tile);
     the "dream voice" text (conclusion + so-clause) takes the muted-purple
     accent already used for --accent, distinguishing it from the plain
     editorial black of the session's own narrative. */
  .since-then { margin-top: 1rem; }
  .since-then-head {
    font-size: 0.72rem; font-weight: 800; letter-spacing: 0.06em;
    text-transform: uppercase; color: var(--fg-muted); margin: 0 0 0.4rem;
  }
  .since-then-kpi {
    display: flex; flex-wrap: wrap; gap: 0.4rem; margin: 0 0 0.6rem;
    font-size: 0.72rem; color: var(--fg-muted);
  }
  .since-then-kpi span {
    border: 1px solid var(--border); border-radius: 999px; padding: 0.08rem 0.55rem;
  }
  .since-then-rows { display: grid; gap: 0.5rem; }
  .since-then-row {
    border: 1px solid var(--border); border-radius: var(--radius-tile);
    background: var(--bg-card); padding: 0.6rem 0.75rem; font-size: 0.875rem;
    line-height: 1.5; min-width: 0;
  }
  .since-then-row .symbol {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-weight: 650; overflow-wrap: anywhere;
  }
  .since-then-row .anchor-only-tag { color: var(--fg-muted); font-size: 0.7rem; }
  .since-then-row .file {
    display: block; color: var(--fg-muted); font-size: 0.74rem; overflow-wrap: anywhere;
  }
  .since-then-row .conclusion {
    margin: 0.35rem 0 0; font-weight: 650; color: var(--purple); overflow-wrap: anywhere;
  }
  .since-then-row .conclusion.superseded,
  .since-then-row .conclusion.obsolete { color: var(--red); }
  .since-then-row .conclusion.unverified { color: var(--fg-muted); font-style: italic; font-weight: 550; }
  /* F1: witnessed-but-unverified is deliberately NOT the purple "dream
     concluded something" accent — it is neutral, distinct from both the
     italic "unverified" (no evidence at all) and any positive/live
     treatment, since a witness with no later verdict is not proof of
     liveness at HEAD. */
  .since-then-row .conclusion.witnessed_unverified { color: var(--fg-muted); font-weight: 650; }
  .since-then-row .so-clause { margin: 0.3rem 0 0; color: var(--ink); overflow-wrap: anywhere; }
  .since-then-row .why-hint {
    display: block; margin-top: 0.35rem; font-size: 0.68rem; color: var(--fg-muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; overflow-wrap: anywhere;
  }
  .since-then-more { font-size: 0.72rem; color: var(--fg-muted); margin: 0.4rem 0 0; }
  /* Delta-review MED: at 0.85 opacity the dark-mode purple fell to ≈4.16:1
     against the index-pane glass — below the 4.5:1 floor. Full opacity
     restores the raw token ratios (4.89:1 dark, higher in light). */
  /* The night bar. Dreams happen BETWEEN sessions, so the digest renders as
     a thin nocturnal band under each day label — the one place the page
     spends its boldness. The ☾ is CSS-drawn so the DOM text (and every
     assertion against it) stays byte-identical. */
  .dream-digest {
    margin: 0.1rem 0 0.45rem; padding: 0.28rem 0.9rem;
    font-size: 0.72rem; color: var(--purple);
    background: linear-gradient(90deg, var(--postit-lavender), transparent 78%);
    border-left: 3px solid var(--purple);
  }
  .dream-digest::before { content: "☾ "; }
  /* The hero's verdict line is a ledger of the mind changing — numbers align
     as figures, not prose. */
  .meta-row { font-variant-numeric: tabular-nums; }

  /* Staggered reveal (CSS-only — no data attribute, no JS, so the rendered
     HTML bytes are identical run to run; the determinism test asserts the
     markup, not the paint). `:nth-of-type` counts only sibling <button>s
     inside `.session-rows` (the interspersed `.group-header` <p>s are a
     different tag), so it stays a stable per-row stagger across every
     client-side re-sort too — the JS re-sort only moves existing button
     nodes, it never creates new ones. */
  @keyframes csrRowReveal {
    from { opacity: 0; transform: translateY(6px); }
    to { opacity: 1; transform: translateY(0); }
  }
  .index-row { animation: csrRowReveal 360ms cubic-bezier(0.16, 1, 0.3, 1) both; }
  .index-row:nth-of-type(1) { animation-delay: 0ms; }
  .index-row:nth-of-type(2) { animation-delay: 18ms; }
  .index-row:nth-of-type(3) { animation-delay: 36ms; }
  .index-row:nth-of-type(4) { animation-delay: 54ms; }
  .index-row:nth-of-type(5) { animation-delay: 72ms; }
  .index-row:nth-of-type(6) { animation-delay: 90ms; }
  .index-row:nth-of-type(7) { animation-delay: 108ms; }
  .index-row:nth-of-type(8) { animation-delay: 126ms; }
  .index-row:nth-of-type(9) { animation-delay: 144ms; }
  .index-row:nth-of-type(10) { animation-delay: 162ms; }
  .index-row:nth-of-type(11) { animation-delay: 180ms; }
  .index-row:nth-of-type(12) { animation-delay: 198ms; }
  .index-row:nth-of-type(13) { animation-delay: 216ms; }
  .index-row:nth-of-type(14) { animation-delay: 234ms; }
  .index-row:nth-of-type(15) { animation-delay: 252ms; }
  .index-row:nth-of-type(16) { animation-delay: 270ms; }
  .index-row:nth-of-type(17) { animation-delay: 288ms; }
  .index-row:nth-of-type(18) { animation-delay: 306ms; }
  .index-row:nth-of-type(19) { animation-delay: 324ms; }
  .index-row:nth-of-type(20) { animation-delay: 342ms; }
  @media (prefers-reduced-motion: reduce) {
    .index-row { animation: none; }
  }

  /* ---- Journal v3 "Dreams Gateway" Phase 2 ----
     The three top-level views share `.csr-view`; `[hidden]` (already
     declared above) is the only visibility mechanism — no separate
     display rule needed here. Card/tile chrome reuses the same
     glass-tile register as `.stage-card`/`.artifact-tile` (backdrop-filter,
     --radius-tile, hover lift) rather than inventing a new one. */
  .dreams-home-head, #dreams-home-view > .hero-title-row { margin-bottom: 0.4rem; }
  .dreams-sub, .dreams-empty {
    color: var(--fg-muted); font-size: 0.9rem; margin: 0 0 2rem;
  }
  .dreams-empty {
    background: var(--bg-card); border: 1px dashed var(--border);
    border-radius: var(--radius-tile); padding: 1.4rem 1.2rem; text-align: center;
  }
  .session-log-link, .dream-back {
    color: var(--fg-muted); text-decoration: underline dotted; font-size: 0.82rem;
  }
  .dream-back { display: inline-block; margin-bottom: 1rem; }
  .dream-project-group { margin-bottom: 1.8rem; }
  .dream-project-eyebrow {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.72rem; letter-spacing: 0.08em; text-transform: uppercase;
    color: var(--label); margin: 0 0 0.6rem;
  }
  .dream-project-eyebrow b { color: var(--purple); text-transform: none; letter-spacing: 0; }
  .dream-cards {
    display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: 0.9rem;
  }
  .dream-card {
    display: block; background: var(--bg-card); border: 1px solid var(--border);
    border-radius: var(--radius-tile); padding: 1rem 1.1rem; color: inherit;
    text-decoration: none; min-width: 0;
    backdrop-filter: var(--glass-blur); -webkit-backdrop-filter: var(--glass-blur);
    transition: transform 180ms cubic-bezier(0.16, 1, 0.3, 1), border-color 180ms ease-out;
  }
  .dream-card:hover { transform: translateY(-2px); border-color: var(--highlight); }
  .dream-grade {
    float: right; margin-left: 0.5rem;
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.6rem; font-weight: 700; letter-spacing: 0.05em;
    border-radius: 999px; padding: 0.1rem 0.5rem; border: 1px solid var(--border);
    color: var(--fg-muted); white-space: nowrap;
  }
  /* Item-grade is the visually stronger chip (plan: "item-grade visually
     stronger") — filled purple vs. session-grade's plain outline. */
  .dream-grade.item-grade {
    color: var(--purple); background: var(--purple-bg); border-color: transparent;
    font-weight: 800;
  }
  .dream-item-text {
    font-size: 1rem; line-height: 1.4; font-weight: 600; color: var(--ink);
    margin: 0 0 0.5rem; overflow-wrap: anywhere;
  }
  .dream-line {
    font-size: 0.8rem; color: var(--fg-muted); line-height: 1.5;
    margin: 0 0 0.25rem; overflow-wrap: anywhere;
  }
  .dream-line::before { content: "\263E "; color: var(--purple); }
  .dream-meta {
    font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.7rem; color: var(--fg-muted); margin-top: 0.5rem;
    font-variant-numeric: tabular-nums;
  }
  .dream-detail-item { max-width: 720px; }
  .dream-detail-head.touch-next {
    border: 1px solid var(--purple); border-radius: var(--radius-tile);
    padding: 0.9rem 1.1rem; margin: 0 0 1.2rem; background: var(--purple-bg);
  }
  .dream-detail-title { font-size: 1.05rem; font-weight: 700; margin: 0; overflow-wrap: anywhere; }
  .dream-detail-project {
    color: var(--fg-muted); font-size: 0.8rem; margin: 0 0 1.1rem;
  }
  .dream-evidence-list { display: grid; gap: 0.4rem; margin-bottom: 1rem; }
  .dream-evidence-row {
    background: var(--bg-card); border: 1px solid var(--border);
    border-radius: var(--radius-tile); padding: 0.5rem 0.7rem;
    font-size: 0.82rem; color: var(--ink); margin: 0; overflow-wrap: anywhere;
  }
  /* Phase 3 mount points — no visual footprint until that phase fills
     them; an empty visible tile would read as a broken panel, not a
     future feature. */
  .dream-ast-slot, .dream-heat-slot { display: none; }
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
  // Finding 2 (pane visibility on selection): the detail pane scrolls
  // independently of the index pane (see the CSS `.detail-pane-wrap`
  // max-height/overflow-y rule), so swapping to a different session's pane
  // must also reset that scroll position to the top — otherwise a reader
  // who scrolled deep into one session's stage rail would land mid-scroll
  // on the next one. `prefers-reduced-motion: reduce` gets the instant
  // "auto" behavior instead of a smooth scroll.
  const detailWrap = document.querySelector(".detail-pane-wrap");
  const prefersReducedMotion =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
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
    if (detailWrap) {
      if (typeof detailWrap.scrollTo === "function") {
        detailWrap.scrollTo({ top: 0, behavior: prefersReducedMotion ? "auto" : "smooth" });
      } else {
        detailWrap.scrollTop = 0;
      }
    }
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

  // --- theme toggle (Journal v2 Phase 6) ---------------------------------
  // auto -> dark -> light -> auto, never persisted: every reload starts
  // back at "auto" (no data-theme attribute), so the rendered HTML bytes
  // stay identical run to run — this only ever touches the live DOM.
  const themeToggle = document.querySelector(".theme-toggle");
  if (themeToggle) {
    const THEME_STATES = ["auto", "dark", "light"];
    const GLYPH = { auto: "◐", dark: "●", light: "○" };
    let themeIndex = 0;
    themeToggle.addEventListener("click", () => {
      themeIndex = (themeIndex + 1) % THEME_STATES.length;
      const state = THEME_STATES[themeIndex];
      if (state === "auto") document.documentElement.removeAttribute("data-theme");
      else document.documentElement.setAttribute("data-theme", state);
      themeToggle.dataset.themeState = state;
      themeToggle.textContent = GLYPH[state];
      themeToggle.setAttribute("aria-label", `Theme: ${state} (click to change)`);
    });
  }

  // --- Journal v3 "Dreams Gateway" routing (Phase 2) ---------------------
  // Three top-level views (`#dreams-home-view` default, `#dream-detail-view`,
  // `#session-log-view` wrapping the unchanged mailbox above) toggled purely
  // by `location.hash`. Deliberately independent of the mailbox's own
  // hashchange listener above: the two hash namespaces never collide
  // (mailbox ids are bare session-short strings; this namespace always
  // starts with "/"), so both listeners can run on every hashchange with no
  // coordination — the mailbox one simply finds no matching pane for a
  // "/"-prefixed hash and no-ops.
  const dreamsHomeView = document.getElementById("dreams-home-view");
  const dreamDetailView = document.getElementById("dream-detail-view");
  const sessionLogView = document.getElementById("session-log-view");
  if (dreamsHomeView && dreamDetailView && sessionLogView) {
    const dreamDetailItems = Array.from(
      dreamDetailView.querySelectorAll(".dream-detail-item")
    );

    const showDreamView = (name) => {
      dreamsHomeView.hidden = name !== "home";
      dreamDetailView.hidden = name !== "detail";
      sessionLogView.hidden = name !== "log";
    };

    const selectDreamItem = (id) => {
      let matched = false;
      dreamDetailItems.forEach(item => {
        const isMatch = item.dataset.item === id;
        item.hidden = !isMatch;
        if (isMatch) matched = true;
      });
      return matched;
    };

    const routeDreams = () => {
      const hash = location.hash;
      if (hash.indexOf('#/item/') === 0) {
        const id = decodeURIComponent(hash.slice('#/item/'.length));
        if (selectDreamItem(id)) {
          showDreamView("detail");
          return;
        }
      }
      if (hash.indexOf('#/log') === 0) {
        showDreamView("log");
        return;
      }
      showDreamView("home");
    };

    routeDreams();
    window.addEventListener("hashchange", routeDreams);
  }
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

/// One SINCE THEN row (journal v2 Phase 5): "this happened" (`symbol`/
/// `file`) -> "the dream concluded" (`conclusion_label`) -> "so"
/// (`so_clause`, rendered only when real evidence exists).
#[derive(Debug, Clone, Serialize)]
struct SinceThenRowView {
    symbol: String,
    file: String,
    /// True when `symbol` came from the `episode_anchors` fallback rather
    /// than trusted `code_node_attribution` — the template's "(anchor-only)"
    /// disclosure.
    anchor_only: bool,
    /// Always present: "superseded ⌗oid8 · date", "obsolete, anchor gone ·
    /// date", "reinstated · date", "witnessed <date> · unverified since", or
    /// "unverified" — never omitted, never a fabricated "still live at
    /// HEAD" (F1: a `witness_ledger` row proves the symbol was intact when
    /// witnessed, not that it still is — no dream verdict re-checked it
    /// since).
    conclusion_label: String,
    /// CSS hook / KPI bucket: "superseded" | "obsolete" | "reinstated" |
    /// "witnessed_unverified" | "unverified".
    conclusion_slug: &'static str,
    /// `csr_why("<symbol>")` — rendered as copyable `<code>` text, never a
    /// hyperlink (this is a static file, no MCP transport from HTML).
    why_hint: String,
    /// ONE evidenced next move — `None` means the row ends at the
    /// conclusion with no advice (abstention-first: no evidence, no "so").
    so_clause: Option<String>,
    /// Short receipt oid when `conclusion_slug == "superseded"`, `None`
    /// otherwise (including a superseded verdict with no receipt at all).
    /// Kept as its own field — rather than re-parsing it out of
    /// `conclusion_label` — so the TOUCH NEXT strip (adversarial-review
    /// finding 1) can quote the receipt without string-sniffing rendered
    /// prose.
    receipt_oid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SinceThenKpiView {
    symbols: usize,
    /// Only segments with evidence are ever non-zero; the template drops a
    /// zero segment entirely rather than rendering "0 superseded".
    superseded: usize,
    /// Count of `witnessed_unverified` rows (F1: "witnessed" is the honest
    /// KPI word — there is no "still live" count, since a witness with no
    /// subsequent verdict is not proof of liveness at HEAD).
    witnessed_unverified: usize,
    unverified: usize,
}

#[derive(Debug, Clone, Serialize)]
struct SinceThenPanelView {
    kpi: SinceThenKpiView,
    rows: Vec<SinceThenRowView>,
    /// Honest "+N more symbols" count for rows beyond `SINCE_THEN_CAP`.
    more: usize,
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
    /// STEER stage only: open/doing todos, in original order, rendered
    /// BEFORE any completed ones (adversarial-review finding 4).
    todos_open: Vec<StoryTodoView>,
    /// STEER stage only: completed todos, in original order.
    todos_completed: Vec<StoryTodoView>,
    /// True when `todos_completed.len() > 5` — the template wraps the
    /// completed list in a nested closed `<details>` ("N completed") instead
    /// of listing every one inline; all rows still land in the DOM either
    /// way (no-JS readable).
    todos_completed_fold: bool,
    /// Whether this stage's `<details>` renders `open` by default
    /// (adversarial-review finding 4): ASK and DELIBERATION start closed,
    /// STEER and OUTCOME start open. All content is always in the DOM
    /// regardless — this only sets the initial disclosure state.
    open: bool,
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
    /// F4: which step produced `instrumentation` — tells the backfill
    /// whether it may skip this card outright (`Episode`, authoritative) or
    /// must validate it against the current transcript stat before trusting
    /// it (`Cache`). See [`crate::storage::dream_report::InstrumentationSource`].
    instrumentation_source: crate::storage::dream_report::InstrumentationSource,
    /// F2 (delta review): whether the stored steer total came from the
    /// current filter generation (version-stamped episode, post-wipe cache
    /// row, or a live scan this run). When false the displayed count comes
    /// from surviving persisted quotes only.
    steer_total_trusted: bool,
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
    /// The SINCE THEN payback panel (journal v2 Phase 5) — `None` when the
    /// session has no attributed symbols and no anchor fallback (panel
    /// absent beats panel empty). Filled in by `build_since_then_panel`
    /// after `build_story_card` returns, since that needs `Storage` access
    /// this pure projection function does not take.
    since_then: Option<SinceThenPanelView>,
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
    /// Todo-completion microchart (plan Phase 4): `true` only when this
    /// session has at least one todo — real data only, absent (no bar at
    /// all) when there is nothing to measure. A bare `Option<u8>` risks a
    /// minijinja truthiness footgun (`Some(0)` reads falsy same as `None`
    /// under `{% if %}`), so presence and value are two separate fields.
    has_todo_bar: bool,
    /// 0-100, meaningful only when `has_todo_bar` is true.
    todo_pct: u8,
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
    /// Adversarial-review finding 3: what the dream has concluded SINCE the
    /// session ended, derived only from SINCE THEN evidence — `None` when
    /// the session touched no symbols at all (no evidence, no claim).
    now_badge: Option<String>,
    now_badge_slug: Option<&'static str>,
    stages: Vec<StageCardView>,
    artifacts: Vec<StoryArtifactView>,
    artifacts_more: usize,
    episode_json: String,
    /// Plan §3.4 / §6 Phase 2: transcript located but did not contribute
    /// measured instrumentation (oversized, budget-exhausted, kill switch,
    /// or a read/parse failure).
    unscanned: bool,
    /// The SINCE THEN payback panel (journal v2 Phase 5) — see
    /// `StoryCardView::since_then`.
    since_then: Option<SinceThenPanelView>,
    /// TOUCH NEXT strip (adversarial-review finding 1) — empty means no
    /// evidenced next move anywhere; the template renders the explicit
    /// abstention sentence in that case, never an empty element.
    touch_next: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DayGroupView {
    label: String,
    rows: Vec<IndexRowView>,
    /// "while away: N anchors retired, M proposals open" (journal v2 Phase
    /// 5) — `None` when both counts are zero for this calendar day; never
    /// an empty digest line.
    digest: Option<String>,
    /// Full commit oids backing `digest`, for the digest's `title`
    /// attribute — receipts, not the visible chip text. Empty string when
    /// `digest` is `None`.
    digest_title: String,
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

// --- Journal v3 "Dreams Gateway" Phase 2 — home + detail view models -------
//
// These project `dream_items::load_dream_items` (Phase 1) into the template
// shapes for View 1 (spare card gallery, grouped by project) and View 2
// (per-item detail shell). Nothing here re-derives evidence — every rendered
// clause traces back to a `DreamEvidence` row already gated by the
// two-channel qualification in `dream_items.rs`; a `DreamItem` with no
// evidence never reaches these builders (the query itself only returns
// qualified items).

/// One dream card on the home gallery (plan View 1).
#[derive(Debug, Clone, Serialize)]
struct DreamCardView {
    id: String,
    /// Verb-first item text, verbatim from the open todo/blocker.
    item: String,
    /// `"item-grade"` | `"session-grade"` — CSS hook, distinct styling per
    /// the plan (item-grade visually stronger).
    grade_slug: &'static str,
    grade_label: &'static str,
    /// Up to 2 lines: `"☾ <symbol> <verdict phrase> in <file> ⌗oid"`,
    /// session-grade prefixed with the ground-shifted framing. Never
    /// includes proposal evidence (no file/symbol to report against).
    dream_lines: Vec<String>,
    /// `"left open <date> · witnessed <date>[ · +N more]"`.
    meta: String,
}

#[derive(Debug, Clone, Serialize)]
struct DreamProjectGroupView {
    project: String,
    /// `"1 dream"` / `"N dreams"` — the eyebrow header's count clause.
    count_label: String,
    cards: Vec<DreamCardView>,
}

/// View 2 shell (Phase 2 scope): TOUCH NEXT-style header + the full,
/// receipt-bearing evidence list. Phase 3 adds the AST tree and heat map
/// into the marked slots this view also renders.
#[derive(Debug, Clone, Serialize)]
struct DreamDetailView {
    id: String,
    title: String,
    project: String,
    grade_label: &'static str,
    /// Every evidence row (verdict rows AND proposals), each rendered with
    /// its own receipt clause — `"no receipt"` when none is stored, never a
    /// silently dropped receipt.
    evidence_lines: Vec<String>,
}

/// The plan's verdict-phrasing map — the only sanctioned wording for a
/// `witness_verdicts.verdict` value or the synthetic `"proposal"` kind on a
/// dream card. Deliberately excludes any "still live"/"re-verified" wording
/// (F1 precedent carried into v3): a witness/verdict row proves a past
/// state, never a current one.
fn dream_verdict_phrase(verdict: &str) -> &'static str {
    match verdict {
        "superseded_by" => "superseded",
        "anchor_obsolete" => "anchor obsolete",
        "anchor_reinstated" => "reinstated",
        "proposal" => "proposal",
        _ => "noted",
    }
}

const DREAM_SESSION_GRADE_PREFIX: &str = "ground shifted under this session — ";

/// Session-grade cards get the ground-shifted framing prefix (the evidence
/// is the origin session's touched files overlapping witnessed ground, not
/// the item's own symbol); item-grade cards attribute directly.
fn dream_grade_prefix(grade: DreamItemGrade) -> &'static str {
    match grade {
        DreamItemGrade::ItemGrade => "",
        DreamItemGrade::SessionGrade => DREAM_SESSION_GRADE_PREFIX,
    }
}

pub(crate) fn dream_grade_slug_label(grade: DreamItemGrade) -> (&'static str, &'static str) {
    match grade {
        DreamItemGrade::ItemGrade => ("item-grade", "item-grade"),
        DreamItemGrade::SessionGrade => ("session-grade", "session-grade"),
    }
}

/// One evidence row rendered as `"<prefix><subject> in <file><receipt>"` —
/// `subject` is `"<symbol> <verdict phrase>"` when a symbol is attached, or
/// the bare verdict phrase for a whole-file witness. The receipt clause is
/// `" ⌗<oid8>"` when a receipt is stored, otherwise omitted entirely (never
/// a fabricated placeholder).
fn dream_evidence_subject_line(evidence: &DreamEvidence, prefix: &str) -> String {
    let verdict_phrase = dream_verdict_phrase(&evidence.verdict);
    let file = basename(&evidence.file);
    let subject = match &evidence.symbol {
        Some(symbol) => format!("{symbol} {verdict_phrase}"),
        None => verdict_phrase.to_string(),
    };
    let receipt = match &evidence.receipt_oid {
        Some(oid) => format!(" ⌗{}", short_oid(oid)),
        None => String::new(),
    };
    format!("{prefix}{subject} in {file}{receipt}")
}

/// Up to 2 card dream-lines from the item's non-proposal evidence (already
/// newest-witnessed-first from `dream_items::dedup_and_cap_evidence`).
/// Proposal rows never appear here — they carry no file/symbol to attribute
/// a change against.
pub(crate) fn dream_card_lines(evidence: &[DreamEvidence], grade: DreamItemGrade) -> Vec<String> {
    let prefix = dream_grade_prefix(grade);
    evidence
        .iter()
        .filter(|e| e.verdict != "proposal")
        .take(2)
        .map(|e| dream_evidence_subject_line(e, prefix))
        .collect()
}

/// `"left open <date> · witnessed <date>[ · +N more]"` — `+N more` counts
/// the non-proposal evidence rows beyond the 2 shown as dream-lines, so the
/// number is always traceable to stored evidence, never an estimate.
pub(crate) fn dream_card_meta(item: &DreamItem) -> String {
    let mut meta = format!("left open {}", iso_date(&item.origin_ts));
    if let Some(latest) = item.evidence.first() {
        meta.push_str(" · witnessed ");
        meta.push_str(&iso_date(&latest.witnessed_at));
    }
    let non_proposal = item
        .evidence
        .iter()
        .filter(|e| e.verdict != "proposal")
        .count();
    if non_proposal > 2 {
        meta.push_str(&format!(" · +{} more", non_proposal - 2));
    }
    meta
}

fn build_dream_card(item: &DreamItem) -> DreamCardView {
    let (grade_slug, grade_label) = dream_grade_slug_label(item.grade);
    DreamCardView {
        id: item.id.clone(),
        item: item.item.clone(),
        grade_slug,
        grade_label,
        dream_lines: dream_card_lines(&item.evidence, item.grade),
        meta: dream_card_meta(item),
    }
}

/// Every evidence row for the detail pane, each carrying its own witnessed
/// date; proposal rows included (unlike the card's capped dream-lines). A
/// non-proposal row with no stored receipt says so explicitly (`"no
/// receipt"`) rather than silently omitting the clause — the card's compact
/// dream-lines can afford the omission, but the detail pane's whole point is
/// to be the honest, complete evidence list.
pub(crate) fn dream_detail_evidence_lines(
    evidence: &[DreamEvidence],
    grade: DreamItemGrade,
) -> Vec<String> {
    let prefix = dream_grade_prefix(grade);
    evidence
        .iter()
        .map(|e| {
            if e.verdict == "proposal" {
                format!(
                    "{prefix}proposal recorded · witnessed {}",
                    iso_date(&e.witnessed_at)
                )
            } else {
                let subject = dream_evidence_subject_line(e, prefix);
                let receipt_note = if e.receipt_oid.is_none() {
                    ", no receipt"
                } else {
                    ""
                };
                format!(
                    "{subject}{receipt_note} · witnessed {}",
                    iso_date(&e.witnessed_at)
                )
            }
        })
        .collect()
}

fn build_dream_detail(item: &DreamItem) -> DreamDetailView {
    let (_, grade_label) = dream_grade_slug_label(item.grade);
    DreamDetailView {
        id: item.id.clone(),
        title: item.item.clone(),
        project: item.project.clone(),
        grade_label,
        evidence_lines: dream_detail_evidence_lines(&item.evidence, item.grade),
    }
}

/// Project `load_dream_items`'s flat, globally-ranked list into the home
/// gallery's project groups (first-seen project order — a stable function of
/// the already-deterministic item order, so re-renders on a frozen corpus
/// stay byte-identical) plus the flat detail-view list (same order as
/// `items`, one `<article>` per qualifying item).
fn build_dream_home(
    items: &[DreamItem],
) -> (
    Vec<DreamProjectGroupView>,
    usize,
    usize,
    Vec<DreamDetailView>,
) {
    let mut groups: Vec<DreamProjectGroupView> = Vec::new();
    for item in items {
        let card = build_dream_card(item);
        match groups.iter_mut().find(|g| g.project == item.project) {
            Some(group) => group.cards.push(card),
            None => groups.push(DreamProjectGroupView {
                project: item.project.clone(),
                count_label: String::new(),
                cards: vec![card],
            }),
        }
    }
    for group in &mut groups {
        group.count_label = pluralize(group.cards.len(), "dream");
    }
    let project_count = groups.len();
    let total = items.len();
    let details = items.iter().map(build_dream_detail).collect();
    (groups, total, project_count, details)
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
    /// Journal v3 Phase 2 — dreams home gallery, grouped by project.
    dream_groups: Vec<DreamProjectGroupView>,
    /// `"N item"`/`"N items"` — the home sub-header's count clause.
    dream_total_label: String,
    /// `"N project"`/`"N projects"` — the home sub-header's group clause.
    dream_project_label: String,
    /// One entry per qualifying item, same order as `dream_groups`
    /// flattens to — the detail view's stacked `<article>`s.
    dream_details: Vec<DreamDetailView>,
}

/// First 8 hex chars of a commit oid.
pub(crate) fn short_oid(oid: &str) -> String {
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

pub(crate) fn pluralize(count: usize, noun: &str) -> String {
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

pub(crate) fn iso_date(timestamp: &str) -> String {
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
        instrumentation_source: session.instrumentation_source.clone(),
        steer_total_trusted: session.steer_total_trusted,
        unscanned: false,
        has_subagents: false,
        since_then: None,
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

/// F2: the FULL provenance chain a persisted steer text must survive to
/// render as a genuine mid-flight human correction. `is_noisy_steer_text`
/// alone (the render path's pre-F2 re-filter) only catches harness-shaped
/// plumbing (`<task-notification>`, `[SYSTEM NOTIFICATION`, …) — it does NOT
/// catch a persisted CSR self-emission such as a `"CSR CONTINUUM [...]"`
/// echo, which is harness-shaped fine but is CSR's own voice, not the
/// human's. `is_csr_emission` and `extractable` are the same guards
/// `instrumentation::from_parsed`'s forward path already runs on the raw
/// entry text before a steer is ever persisted (module doc, "Steer detection
/// reuses the exact same self-contamination guard"); re-running them here on
/// the *persisted* text closes the gap for episodes stored before a filter
/// fix landed, or before `is_noisy_steer_text` grew a new pattern.
fn steer_survives(text: &str) -> bool {
    !crate::extraction::provenance::is_csr_emission(text)
        && crate::extraction::provenance::extractable(text).is_some()
        && !crate::transcript::instrumentation::is_noisy_steer_text(text)
}

/// STEER stage (adversarial-review finding 4): above this many completed
/// todos, the completed list folds behind a nested closed `<details>`
/// ("N completed") instead of listing every one inline.
const COMPLETED_TODO_FOLD_THRESHOLD: usize = 5;

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
            todos_open: Vec::new(),
            todos_completed: Vec::new(),
            todos_completed_fold: false,
            open: false,
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
            todos_open: Vec::new(),
            todos_completed: Vec::new(),
            todos_completed_fold: false,
            open: false,
            outcome_text: None,
            outcome_stats: None,
            steer_count: None,
            steers: Vec::new(),
            error_line: None,
            top_error: None,
            still_open: None,
        });
    }

    // F2: `steer_count`/`steers` are the *stored* cache, computed once at
    // episode-persist time. An episode persisted before a provenance-filter
    // fix landed (or before `is_noisy_steer_text` grew a new pattern) can
    // carry a stale "steer" that is actually a CSR self-emission or harness
    // notification. Re-running only `is_noisy_steer_text` (the old fix) left
    // a gap: a persisted `"CSR CONTINUUM [...]"` steer is harness-shaped
    // fine but IS a CSR emission, so it survived. The render path must not
    // trust the cache at all — it re-runs the FULL provenance chain
    // (`is_csr_emission`, `extractable`, `is_noisy_steer_text`) on every
    // persisted steer text, and the *displayed* count is derived from how
    // many of the (at most `MAX_STORED_STEERS`) persisted quotes survive,
    // not blindly reused from the stored `steer_count` — otherwise a steer
    // count of "1" can render with zero surviving quotes underneath it.
    let stored_steers: &[crate::transcript::instrumentation::SteerEvent] = card
        .instrumentation
        .as_ref()
        .map(|i| i.steers.as_slice())
        .unwrap_or(&[]);
    let surviving_count = stored_steers
        .iter()
        .filter(|s| steer_survives(&s.text))
        .count() as u32;
    // Delta-review F2: whether the stored TOTAL may be displayed depends on
    // which filter generation measured it. A trusted total (version-stamped
    // episode, post-wipe cache row, live scan) was computed by the same
    // chain `steer_survives` applies, so its uninspectable remainder beyond
    // the persisted-quote cap is clean by construction and the total stands.
    // An untrusted (legacy) total may count harness noise we cannot inspect
    // — the previous raw-minus-provable-noise arithmetic produced "2
    // mid-flight steers" over an empty quote list (steer_count=5, 3 stored
    // quotes all noise). The only number evidence supports there is the
    // surviving-quote count, so that is what renders — and when it is zero,
    // no steer line at all.
    let steer_count = card.instrumentation.as_ref().map(|i| {
        if card.steer_total_trusted {
            i.steer_count
        } else {
            surviving_count
        }
    });
    let has_steers = steer_count.is_some_and(|n| n > 0);
    if !card.todos.is_empty() || has_steers {
        let steers: Vec<SteerLineView> = stored_steers
            .iter()
            .filter(|s| steer_survives(&s.text))
            .take(3)
            .map(|s| SteerLineView {
                turn: s.turn,
                text: s.text.clone(),
            })
            .collect();
        // Finding 4: open (○/→) todos render before completed ones; a
        // completed list beyond the fold threshold nests behind a closed
        // <details> rather than listing every one inline. Order within each
        // bucket is preserved from the episode's own todo order.
        let mut todos_open = Vec::new();
        let mut todos_completed = Vec::new();
        for todo in &card.todos {
            if todo.slug == "done" {
                todos_completed.push(todo.clone());
            } else {
                todos_open.push(todo.clone());
            }
        }
        let todos_completed_fold = todos_completed.len() > COMPLETED_TODO_FOLD_THRESHOLD;
        stages.push(StageCardView {
            id: "S_STEER",
            label: "STEER",
            glyph: "✎".to_string(),
            kind: "steer",
            quote: None,
            narrative: None,
            investigated: Vec::new(),
            investigated_more: 0,
            todos_open,
            todos_completed,
            todos_completed_fold,
            open: true,
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
            todos_open: Vec::new(),
            todos_completed: Vec::new(),
            todos_completed_fold: false,
            open: true,
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

/// Real-data-only todo-completion fraction (plan Phase 4 microchart): `None`
/// when the session carries no todos at all — never a fabricated 0%.
fn todo_completion_pct(todos: &[StoryTodoView]) -> Option<u8> {
    if todos.is_empty() {
        return None;
    }
    let done = todos.iter().filter(|todo| todo.slug == "done").count();
    Some(((done * 100) / todos.len()) as u8)
}

fn index_row(card: &StoryCardView, selected: bool) -> IndexRowView {
    let todo_pct = todo_completion_pct(&card.todos);
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
        has_todo_bar: todo_pct.is_some(),
        todo_pct: todo_pct.unwrap_or(0),
    }
}

fn detail_pane(card: &StoryCardView) -> DetailPaneView {
    let artifacts_more = card.artifacts.len().saturating_sub(ARTIFACT_CAP);
    let (now_badge, now_badge_slug) = build_now_badge(card.since_then.as_ref());
    DetailPaneView {
        session_short: card.session_short.clone(),
        sentence: card.summary.clone(),
        description: card.description.clone(),
        project: card.project.clone(),
        date: card.date.clone(),
        outcome_slug: card.outcome_slug,
        outcome_badge: card.outcome_badge.clone(),
        now_badge,
        now_badge_slug,
        stages: build_stage_cards(card),
        artifacts: card.artifacts.iter().take(ARTIFACT_CAP).cloned().collect(),
        artifacts_more,
        episode_json: card.episode_json.clone(),
        unscanned: card.unscanned,
        since_then: card.since_then.clone(),
        touch_next: build_touch_next(card),
    }
}

// --- SINCE THEN payback panel (journal v2 Phase 5) --------------------------
//
// The panel renders the dream's RE-DELIBERATION about the symbols a session
// touched, not a session log — see dreaming-subconscious-principles.md's
// "double deliberation" frame. Every row is happened -> concluded -> so, and
// every clause drops independently without evidence (abstention-first).

/// Cap on rendered SINCE THEN rows — same discipline as `ARTIFACT_CAP`: an
/// honest "+N more" line covers the remainder, never a silent drop.
const SINCE_THEN_CAP: usize = 20;

/// Render one symbol's conclusion into its always-present label, CSS slug,
/// and adverse-sort rank (0 = worst news, sorts first). Wording choices:
/// "obsolete, anchor gone" is the literal phrase from the Phase 5 spec's
/// dream-voice section; "superseded" and "reinstated" follow the same
/// "verdict · event date" shape. F1: there is no "still live, re-verified at
/// HEAD" state — a witness with no subsequent verdict is NOT evidence the
/// symbol still holds at HEAD, only that it held when last witnessed, so
/// that case renders as "witnessed <date> · unverified since" (slug
/// `witnessed_unverified`) with neutral styling, never the positive
/// treatment a genuine re-verification would earn.
fn since_then_conclusion_display(conclusion: &SinceThenConclusion) -> (String, &'static str, u8) {
    match (&conclusion.verdict, conclusion.witnessed) {
        (Some(VerdictKind::SupersededBy), _) => {
            let oid = conclusion
                .receipt_oid
                .as_deref()
                .map(short_oid)
                .unwrap_or_else(|| "no receipt".to_string());
            let date = conclusion
                .event_date
                .as_deref()
                .map(iso_date)
                .unwrap_or_default();
            (format!("superseded ⌗{oid} · {date}"), "superseded", 0)
        }
        (Some(VerdictKind::AnchorObsolete), _) => {
            let date = conclusion
                .event_date
                .as_deref()
                .map(iso_date)
                .unwrap_or_default();
            (format!("obsolete, anchor gone · {date}"), "obsolete", 0)
        }
        (Some(VerdictKind::AnchorReinstated), _) => {
            let date = conclusion
                .event_date
                .as_deref()
                .map(iso_date)
                .unwrap_or_default();
            (format!("reinstated · {date}"), "reinstated", 1)
        }
        (None, true) => {
            let date = conclusion
                .witnessed_at
                .as_deref()
                .map(iso_date)
                .unwrap_or_default();
            (
                format!("witnessed {date} · unverified since"),
                "witnessed_unverified",
                2,
            )
        }
        (None, false) => ("unverified".to_string(), "unverified", 3),
    }
}

/// The ONE evidenced "so" move for a row, or `None` (abstention-first: no
/// evidence, no advice, row ends at the conclusion). Priority order per the
/// Phase 5 spec: a superseded symbol ALWAYS gets the deterministic
/// receipt-based advice (the receipt itself is the evidence); everything
/// else checks, in order, an open todo naming the symbol, an open
/// `resolution_proposals` claim bound to one of its witnesses, then the
/// session's own episode blocker text.
fn since_then_so_clause(
    conn: &Connection,
    session: &StorySession,
    project: &str,
    file: &str,
    symbol: &str,
    conclusion: &SinceThenConclusion,
) -> Option<String> {
    if matches!(conclusion.verdict, Some(VerdictKind::SupersededBy)) {
        let oid = conclusion
            .receipt_oid
            .as_deref()
            .map(short_oid)
            .unwrap_or_else(|| "unknown".to_string());
        return Some(format!(
            "read the superseding change before touching this again (⌗{oid})"
        ));
    }
    let symbol_lower = symbol.to_lowercase();
    if let Some(todo) = session.todos.iter().find(|todo| {
        !matches!(todo.status.as_str(), "completed" | "done")
            && todo.content.to_lowercase().contains(&symbol_lower)
    }) {
        return Some(format!("open todo: \"{}\"", plain_text(&todo.content)));
    }
    if let Ok(Some(claim)) =
        witness_verdicts::open_proposal_claim_for_symbol(conn, project, file, symbol)
    {
        return Some(format!(
            "open proposal: \"{}\" — csr_resolve to confirm or reject",
            plain_text(&claim)
        ));
    }
    if let Some(blockers) = session
        .blockers
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(format!("blocked: {}", plain_text(blockers)));
    }
    None
}

/// Build the SINCE THEN panel for one session, or `None` when it has no
/// attributed symbols and no anchor fallback — panel absent beats panel
/// empty (the plan's binding trap, carried from the payback-panel spec).
/// One connection lock for the whole panel (symbols + every row's
/// conclusion + so-clause lookup) rather than one lock per query.
fn build_since_then_panel(storage: &Storage, session: &StorySession) -> Option<SinceThenPanelView> {
    let project = if session.project.is_empty() {
        "unknown"
    } else {
        session.project.as_str()
    };

    struct Built {
        row: SinceThenRowView,
        adverse_rank: u8,
        touched_at: String,
    }

    let built: Vec<Built> = storage
        .with_connection(|conn| {
            let symbols = dream_report::load_since_then_symbols(conn, &session.session_id)?;
            let mut built = Vec::with_capacity(symbols.len());
            for sym in symbols {
                let conclusion =
                    witness_verdicts::since_then_conclusion(conn, project, &sym.file, &sym.symbol)?;
                let (label, slug, rank) = since_then_conclusion_display(&conclusion);
                let so_clause = since_then_so_clause(
                    conn,
                    session,
                    project,
                    &sym.file,
                    &sym.symbol,
                    &conclusion,
                );
                let receipt_oid = matches!(conclusion.verdict, Some(VerdictKind::SupersededBy))
                    .then(|| conclusion.receipt_oid.as_deref().map(short_oid))
                    .flatten();
                built.push(Built {
                    row: SinceThenRowView {
                        symbol: sym.symbol.clone(),
                        file: sym.file,
                        anchor_only: sym.anchor_only,
                        conclusion_label: label,
                        conclusion_slug: slug,
                        why_hint: format!("csr_why(\"{}\")", sym.symbol),
                        so_clause,
                        receipt_oid,
                    },
                    adverse_rank: rank,
                    touched_at: sym.touched_at,
                });
            }
            Ok(built)
        })
        .unwrap_or_default();

    if built.is_empty() {
        return None;
    }

    let mut built = built;
    built.sort_by(|a, b| {
        a.adverse_rank
            .cmp(&b.adverse_rank)
            .then_with(|| b.touched_at.cmp(&a.touched_at))
            .then_with(|| a.row.symbol.cmp(&b.row.symbol))
    });

    let total = built.len();
    let mut kpi = SinceThenKpiView {
        symbols: total,
        superseded: 0,
        witnessed_unverified: 0,
        unverified: 0,
    };
    for entry in &built {
        match entry.row.conclusion_slug {
            "superseded" => kpi.superseded += 1,
            "witnessed_unverified" => kpi.witnessed_unverified += 1,
            "unverified" => kpi.unverified += 1,
            _ => {}
        }
    }

    let more = total.saturating_sub(SINCE_THEN_CAP);
    let rows = built
        .into_iter()
        .take(SINCE_THEN_CAP)
        .map(|b| b.row)
        .collect();

    Some(SinceThenPanelView { kpi, rows, more })
}

// --- TOUCH NEXT strip (adversarial-review finding 1) -----------------------
//
// The page's answer to "what should I do next?", rendered directly below the
// detail header, before the stage rail. Abstention-first: every line traces
// to evidence already computed for this session — an open/doing todo, the
// episode's own `next_steps`, a `blockers` clause, or a SINCE THEN so-clause
// for a symbol the dream concluded was superseded (the receipt IS the
// evidence). No source anywhere -> an empty list, and the template renders
// the honest "nothing evidenced" sentence rather than inventing advice.

/// Cap on the TOUCH NEXT strip — worst/most-actionable evidence first:
/// blockers > open todos > next_steps > superseded receipts.
const TOUCH_NEXT_CAP: usize = 3;

fn build_touch_next(card: &StoryCardView) -> Vec<String> {
    let mut lines: Vec<String> = Vec::with_capacity(TOUCH_NEXT_CAP);
    // An open todo and the episode's next_steps often carry the same text
    // (the Stop hook derives one from the other) — the strip must never say
    // the same thing twice, so every push is deduped case-insensitively.
    let push_unique = |lines: &mut Vec<String>, line: String| {
        let key = line.trim().to_lowercase();
        if !lines.iter().any(|l| l.trim().to_lowercase() == key) {
            lines.push(line);
        }
    };

    if let Some(blockers) = &card.blockers {
        push_unique(&mut lines, format!("blocked: {blockers}"));
    }

    if lines.len() < TOUCH_NEXT_CAP {
        for todo in &card.todos {
            if lines.len() >= TOUCH_NEXT_CAP {
                break;
            }
            if matches!(todo.slug, "open" | "doing") {
                push_unique(&mut lines, todo.content.clone());
            }
        }
    }

    if lines.len() < TOUCH_NEXT_CAP {
        if let Some(next) = &card.next_steps {
            push_unique(&mut lines, next.clone());
        }
    }

    if lines.len() < TOUCH_NEXT_CAP {
        if let Some(since_then) = &card.since_then {
            for row in &since_then.rows {
                if lines.len() >= TOUCH_NEXT_CAP {
                    break;
                }
                if row.conclusion_slug == "superseded" {
                    let oid = row.receipt_oid.as_deref().unwrap_or("unknown");
                    push_unique(
                        &mut lines,
                        format!("read ⌗{oid} before touching {}", row.symbol),
                    );
                }
            }
        }
    }

    lines.truncate(TOUCH_NEXT_CAP);
    lines
}

// --- ENDED / NOW badges (adversarial-review finding 3) ----------------------
//
// The outcome badge ("ENDED: FAILED", etc.) states what the session itself
// concluded, dated. The NOW badge states what the dream has concluded SINCE
// — evidence-gated, never a health claim: there is no verdict type that
// proves a symbol is still fine, so "NOW: fine/ok/current" never renders.

/// `(badge text, CSS slug)` for the NOW badge, or `(None, None)` when the
/// session has no attributed symbols at all (no evidence, no claim — never a
/// fabricated "NOW: fine").
fn build_now_badge(
    since_then: Option<&SinceThenPanelView>,
) -> (Option<String>, Option<&'static str>) {
    let Some(panel) = since_then else {
        return (None, None);
    };
    if panel.rows.is_empty() {
        return (None, None);
    }
    let adverse = panel
        .rows
        .iter()
        .filter(|row| matches!(row.conclusion_slug, "superseded" | "obsolete"))
        .count();
    if adverse > 0 {
        return (
            Some(format!("NOW: {adverse} superseded since")),
            Some("superseded"),
        );
    }
    let all_unverified = panel
        .rows
        .iter()
        .all(|row| matches!(row.conclusion_slug, "witnessed_unverified" | "unverified"));
    if all_unverified {
        return (Some("NOW: unverified".to_string()), Some("unverified"));
    }
    // Mixed reinstated/witnessed evidence with no adverse verdict proves
    // nothing about current health either way (no verdict type means "still
    // fine") — abstention-first: no badge rather than an invented claim.
    (None, None)
}

/// "while away: N anchors retired, M proposals open" (journal v2 Phase 5) —
/// `None` when both counts are zero. Each clause drops independently
/// (segment discipline): a day with retirements but no open proposals shows
/// only the retirement clause, and vice versa.
fn day_digest_line(digest: &DayDigest) -> Option<(String, String)> {
    if digest.retired_count == 0 && digest.proposal_count == 0 {
        return None;
    }
    let mut clauses = Vec::new();
    if digest.retired_count > 0 {
        clauses.push(format!(
            "{} retired",
            pluralize(digest.retired_count as usize, "anchor")
        ));
    }
    if digest.proposal_count > 0 {
        clauses.push(format!(
            "{} open",
            pluralize(digest.proposal_count as usize, "proposal")
        ));
    }
    let title = digest
        .receipts
        .iter()
        .map(|oid| format!("⌗{oid}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some((format!("while away: {}", clauses.join(", ")), title))
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
    /// Journal v2 Phase 5: one [`DayDigest`] per DISTINCT calendar day among
    /// `rich_cards`, keyed by `StoryCardView::date` — `finalize_report_data`
    /// attaches the matching digest when it opens a new `DayGroupView`.
    day_digests: std::collections::BTreeMap<String, DayDigest>,
    /// Journal v3 Phase 2: dream-gated open items — see
    /// `storage::dream_items::load_dream_items`. Fail-open: a storage error
    /// here becomes an empty vec, never a report-generation failure.
    dream_items: Vec<DreamItem>,
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
/// 3 — live scan). Mutates `cards` in place, bounded by the wall budget and
/// the existing `MAX_STORY_CARDS` cap on `cards.len()` itself (the plan's
/// "at most 50 sessions per run"). A transcript over
/// `MAX_TRANSCRIPT_SCAN_BYTES` is never parsed; it only sets `unscanned`. A
/// cache hit found here (a prior run's backfill result the DB-only
/// `load_story_sessions` join could not see freshness for) does not count
/// against the wall budget — only an actual `parse_transcript` call does.
///
/// F4: `instrumentation_source` gates what this loop does per card.
/// `Episode`-sourced instrumentation is the Stop hook's own tool-verified
/// scan — authoritative, skipped outright, exactly as before. `Cache`-sourced
/// instrumentation (a `session_instrumentation` row `load_story_sessions`
/// attached without ever statting the transcript) is no longer trusted
/// blindly: this loop resolves the path, stats the file, and compares
/// against the `(transcript_size, transcript_mtime)` the row was cached
/// under. A match is a real fact ("this transcript has not changed since it
/// was scanned") and costs one stat, zero rescans. A mismatch means the
/// transcript has since grown/shrunk — the stale value is discarded and the
/// card falls through to the same resolve-cache-then-scan path a never-cached
/// (`None`-sourced) card already took, so a transcript cached at "0 errors"
/// can no longer render "0 errors" forever after it starts producing real
/// ones. An unresolvable path or failed stat proves nothing either way, so
/// the cached value is left exactly as loaded — F4's "cannot stat → keep,
/// disclosed nothing".
fn backfill_instrumentation(storage: &Storage, projects_dir: &Path, cards: &mut [StoryCardView]) {
    let scan_enabled = journal_scan_enabled();
    let start = std::time::Instant::now();
    for card in cards.iter_mut() {
        let cached_stat = match card.instrumentation_source {
            dream_report::InstrumentationSource::Episode => continue,
            dream_report::InstrumentationSource::Cache {
                transcript_size,
                transcript_mtime,
            } => Some((transcript_size, transcript_mtime)),
            dream_report::InstrumentationSource::None => None,
        };

        let lookup = crate::transcript::resolve_session_path(
            projects_dir,
            // Live transcripts only: the stat below is the JSONL's.
            None,
            &card.session_id,
            Some(&card.project),
        );
        let path = match lookup {
            crate::mcp::tools::ConversationLookup::Found(path, _) => path,
            // No transcript resolvable at all: a `None`-sourced card stays
            // Absent/Degraded exactly as before; a `Cache`-sourced card's
            // stat cannot be checked, so its existing value is left as-is
            // (cannot prove staleness -> not discarded).
            _ => continue,
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            // Same reasoning: a failed stat proves nothing, so any
            // already-loaded cached value is kept untouched.
            continue;
        };
        card.has_subagents = has_subagent_transcripts(&path, &card.session_id);
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        if let Some((cached_size, cached_mtime)) = cached_stat {
            if cached_size == metadata.len() && cached_mtime == mtime {
                // Fresh: the transcript has not changed since this row was
                // computed. Zero rescans, keep the loaded value as-is.
                continue;
            }
            // Stale: discard so the logic below re-resolves it exactly like
            // a session that was never cached.
            card.instrumentation = None;
        }

        if metadata.len() > crate::transcript::instrumentation::MAX_TRANSCRIPT_SCAN_BYTES {
            card.unscanned = true;
            continue;
        }

        // A cache row from a prior run — or the same row just proven stale
        // above via a size/mtime mismatch — may still turn out fresh here
        // (e.g. a concurrent writer refreshed it since); reusing it here
        // costs one read, no parse, and does not count against the wall
        // budget.
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
            // F2: cache rows postdate the steer_noise_filter_v2 wipe —
            // current filter generation, total trustworthy.
            card.steer_total_trusted = true;
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
        // F2: a live scan this run uses the current filter by definition.
        card.steer_total_trusted = true;
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
            SessionClass::Rich => {
                let mut card = build_story_card(session);
                card.since_then = build_since_then_panel(storage, session);
                rich_cards.push(card);
            }
            SessionClass::Thin => thin_entries.push(build_thin_entry(session)),
            SessionClass::Omitted => omitted += 1,
        }
    }
    let older_omitted = rich_cards.len().saturating_sub(MAX_STORY_CARDS);
    rich_cards.truncate(MAX_STORY_CARDS);
    backfill_instrumentation(storage, projects_dir, &mut rich_cards);

    // Journal v2 Phase 5 day-header digest: one `day_digest` lookup per
    // DISTINCT calendar day actually rendered (not per card) — the shared
    // storage lock is cheap to reacquire relative to the query itself.
    let mut day_digests: std::collections::BTreeMap<String, DayDigest> =
        std::collections::BTreeMap::new();
    {
        let mut dates: Vec<String> = rich_cards.iter().map(|card| card.date.clone()).collect();
        dates.sort();
        dates.dedup();
        for date in dates {
            let digest = storage
                .with_connection(|conn| witness_verdicts::day_digest(conn, &date))
                .unwrap_or_default();
            day_digests.insert(date, digest);
        }
    }

    let total = obsolete + superseded + reinstated;

    // Journal v3 Phase 2: fail-open per plan §2 ("error → empty vec") — a
    // storage error on this read-only feed degrades the dreams home to its
    // honest empty state, it never fails report generation.
    let dream_items = storage
        .with_connection(dream_items::load_dream_items)
        .unwrap_or_default();

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
        day_digests,
        dream_items,
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
            _ => {
                let (digest, digest_title) = raw
                    .day_digests
                    .get(&card.date)
                    .and_then(day_digest_line)
                    .map(|(text, title)| (Some(text), title))
                    .unwrap_or((None, String::new()));
                day_groups.push(DayGroupView {
                    label: day_label(&card.date, today),
                    rows: vec![row],
                    digest,
                    digest_title,
                })
            }
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

    let (dream_groups, dream_total, dream_project_count, dream_details) =
        build_dream_home(&raw.dream_items);

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
        dream_groups,
        dream_total_label: pluralize(dream_total, "item"),
        dream_project_label: pluralize(dream_project_count, "project"),
        dream_details,
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
    // Journal v3 Phase 2: the dreams home + detail views become the default
    // landing content; the existing hero+mailbox markup is demoted into
    // `#session-log-view` (hidden by default), reachable via the "session
    // log" link and unchanged otherwise — same certified v2 surface, just a
    // new hidden parent around it.
    let template = replace_exactly_once(
        &template,
        "<div class=\"wrap\">\n\n  <header class=\"hero\">",
        &format!(
            "<div class=\"wrap\">\n\n  {DREAMS_HOME_TEMPLATE}\n  {DREAM_DETAIL_TEMPLATE}\n  <div id=\"session-log-view\" class=\"csr-view\" hidden>\n  <header class=\"hero\">"
        ),
        "<div class=\"wrap\"> (dreams-home insertion point)",
    );
    let template = replace_exactly_once(
        &template,
        "  {% endif %}\n\n  <footer>",
        "  {% endif %}\n\n  </div>\n\n  <footer>",
        "{% endif %} (session-log-view close point)",
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
// (`CSR_NO_AI_NARRATIVES=1`, `allow_invoke: false` below) applies previously
// cached sentences exactly as if the switch were off — they are already
// paid for, so suppressing them would waste money already spent — and
// forces fallbacks only for the misses: it never invokes the model, and a
// miss keeps the deterministic `fallback_sentence` `build_story_card`
// already set, uncached, forever (nothing to converge — no spend was made,
// so there is no purchased result to preserve).
//
// F5 (AI-spend convergence by construction): when the switch IS off and the
// model IS invoked, `narrative_usage` is recorded for the whole batch the
// instant a reply comes back — before anyone knows whether every requested
// card actually produced an acceptable sentence. A missing id, unparseable
// JSON, or a verb-disagreeing reply must therefore never leave a card
// uncached: `apply_sentence_replies` caches that card's deterministic
// fallback instead (tagged `FALLBACK_MODEL_TAG` in `journal_headlines.model`
// — the model's own rejected/missing LINE is still never cached, only the
// fallback is). Without this, a batch that pays once but parses badly kept
// re-paying every subsequent run for the same misses, forever.
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

/// Tag stored in `journal_headlines.model` for a row cached with the
/// deterministic fallback sentence rather than an accepted model line (F5)
/// — disambiguates "the model never produced an acceptable sentence for
/// this card, so its fallback was cached instead" from a genuine model
/// attribution, for anyone inspecting the cache directly.
const FALLBACK_MODEL_TAG: &str = "fallback";

/// Applies one parsed reply map to `cards`/`storage` for `chunk`'s miss
/// indices. Split out from `curate_sentences_with` so the verb-agreement /
/// caching contract (plan §5.1: a disagreeing reply is rejected and NOT
/// cached with the MODEL'S line) is unit-testable without spawning `claude`.
///
/// F5 (AI-spend convergence by construction): `curate_sentences_with`
/// already recorded `narrative_usage` for this batch by the time this runs
/// — the call was paid for regardless of what the model returned. So EVERY
/// card in `chunk` must leave this function cached in `journal_headlines`
/// under the current hash: an accepted model sentence when one exists and
/// agrees with the outcome slug, otherwise the deterministic
/// `fallback_sentence` — a card missing from `replies` entirely (unparsed/
/// malformed JSON, or an id the model simply dropped) or verb-disagreeing is
/// no longer left uncached. Without this, a malformed or partial reply made
/// the same cards re-purchase a `claude -p` call every single run, forever,
/// for money already spent once.
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
        let slug = cards[index].outcome_slug;
        let reply = replies.get(&session_short);
        let accepted = reply.filter(|reply| verb_agrees(slug, reply));

        if let Some(reply) = accepted {
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
            continue;
        }

        if let Some(reply) = reply {
            tracing::warn!(
                session = %session_short,
                reply = %reply,
                slug,
                "curated sentence disagreed with the outcome slug — using the \
                 deterministic fallback and caching IT, not the model's line"
            );
        } else {
            tracing::warn!(
                session = %session_short,
                slug,
                "no curated sentence for this session in the batch reply (missing id or \
                 unparseable JSON) — using the deterministic fallback and caching it"
            );
        }

        // F5: the fallback line — never the rejected/missing model text —
        // is what gets cached, so this card converges to zero spend next
        // run exactly like an accepted sentence would.
        let narrative_or_request = cards[index]
            .narrative
            .clone()
            .or_else(|| cards[index].request_text.clone());
        let fallback = fallback_sentence(slug, narrative_or_request.as_deref());
        let stored_description = if !cards[index].description.is_empty() {
            cards[index].description.clone()
        } else {
            fallback.clone()
        };
        store_sentence(
            storage,
            &session_short,
            &hash,
            &fallback,
            &stored_description,
            FALLBACK_MODEL_TAG,
        );
        cards[index].summary = fallback.clone();
        cards[index].description = if stored_description == fallback {
            String::new()
        } else {
            stored_description
        };
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
            tracing::warn!(
                "journal sentence chunk parsed to nothing — reply likely malformed; every card \
                 in this already-paid-for batch gets its deterministic fallback cached (F5)"
            );
        }
        // F5: run unconditionally, even on a fully malformed reply —
        // `apply_sentence_replies` now caches a fallback for every card that
        // did not receive an accepted model sentence, so the usage just
        // recorded above converges to zero next run regardless of reply
        // quality.
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
#[path = "report_tests.rs"]
mod tests;
