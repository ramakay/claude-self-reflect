//! Template compilation and HTML rendering for the journal server.
//!
//! Templates are compiled into the binary with `include_str!` (same
//! precedent as `dream::report`'s `report_template.html.jinja`), so the
//! server has no runtime asset directory to find, ship, or be tricked into
//! reading from.
//!
//! Every template is registered under a `.html` name, which is what makes
//! minijinja's default auto-escape callback treat every `{{ }}` as HTML and
//! escape it. That is the whole XSS story for this surface: item text,
//! symbol names, file paths and project names all originate in a real
//! codebase and a real transcript, and none of them are trusted.
//!
//! Nothing here panics. Template compilation happens once behind a
//! `OnceLock` and a failure is returned as an error the routes turn into a
//! 500 — the daemon hosts this server, and a panicking render must never be
//! able to take a background loop with it.

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use minijinja::Environment;
use serde::Serialize;

use super::state::{BoardView, DetailView, ResolveReceipt};

const LAYOUT: &str = include_str!("layout.html.jinja");
const LANDING: &str = include_str!("landing.html.jinja");
const DETAIL: &str = include_str!("detail.html.jinja");
const NOTICE: &str = include_str!("notice.html.jinja");

const LAYOUT_NAME: &str = "layout.html";
const LANDING_NAME: &str = "landing.html";
const DETAIL_NAME: &str = "detail.html";
const NOTICE_NAME: &str = "notice.html";

/// A 404 / 500 / kill-switch page. Kept in the same visual register as the
/// real pages so an error never looks like a broken deploy.
#[derive(Debug, Clone, Serialize)]
pub struct NoticeView {
    pub status: String,
    pub heading: String,
    pub detail: String,
}

impl NoticeView {
    pub fn not_found(path: &str) -> Self {
        Self {
            status: "404".to_string(),
            heading: "No such page".to_string(),
            // `path` is escaped by the template; it is never used to touch
            // the filesystem, a query, or a process.
            detail: format!("Nothing is served at {path}."),
        }
    }

    pub fn unknown_dream(id: &str) -> Self {
        Self {
            status: "404".to_string(),
            heading: "No dream with that id".to_string(),
            detail: format!(
                "{id} does not match any item currently in the feed. Ids are recomputed from the \
                 stored item text, so a link can go stale when the item is completed or reworded."
            ),
        }
    }

    pub fn feed_error(reason: &str) -> Self {
        Self {
            status: "500".to_string(),
            heading: "The feed could not be read".to_string(),
            detail: format!(
                "The dream feed query failed: {reason}. Nothing is rendered from a partial read."
            ),
        }
    }

    pub fn bad_request(reason: &str) -> Self {
        Self {
            status: "400".to_string(),
            heading: "That request does not parse".to_string(),
            detail: reason.to_string(),
        }
    }

    /// A detail page whose composed sections could not be read.
    ///
    /// This is the degraded page codex X4 finding 9 requires: the failure is
    /// named, and the page says explicitly that the missing sections are
    /// missing *because of the failure*. Rendering the ordinary detail
    /// template here would assert that no request, completion or night thread
    /// is on record — a claim a failed read has proved nothing about.
    pub fn detail_error(item: &str, reason: &str) -> Self {
        Self {
            status: "500".to_string(),
            heading: "This dream could not be composed".to_string(),
            detail: format!(
                "Reading the stored rows behind {item:?} failed: {reason}. Nothing below is \
                 rendered from this failed read — no brief, no plan, no AST comparison and no \
                 churn are shown, and their absence here is the error, not evidence that the \
                 rows are missing."
            ),
        }
    }

    pub fn forbidden(reason: &str) -> Self {
        Self {
            status: "403".to_string(),
            heading: "That write was refused".to_string(),
            detail: format!("{reason} Nothing was written."),
        }
    }

    pub fn too_large(limit: usize) -> Self {
        Self {
            status: "413".to_string(),
            heading: "That request body is too large".to_string(),
            detail: format!(
                "This surface accepts at most {limit} bytes on a write. The body was not parsed \
                 and nothing was written."
            ),
        }
    }

    /// A write that did not happen, with the reason. Never dressed up as a
    /// success.
    pub fn write_refused(reason: &str) -> Self {
        Self {
            status: "409".to_string(),
            heading: "No verdict was recorded".to_string(),
            detail: reason.to_string(),
        }
    }

    /// A write that DID happen, reported from its receipt. Every number here
    /// is a count of rows actually written.
    pub fn recorded(receipt: &ResolveReceipt) -> Self {
        Self {
            status: "200".to_string(),
            heading: "Verdict recorded".to_string(),
            detail: format!(
                "{} recorded as {} on {:?} — {} written against session {}, origin {}. \
                 This records what you asserted; no automated check verified the code.",
                receipt.action,
                receipt.status,
                receipt.item,
                projection_pluralize(receipt.chunks, "resolution row"),
                receipt.origin_session,
                receipt.origin,
            ),
        }
    }
}

fn projection_pluralize(count: usize, noun: &str) -> String {
    crate::dream::report::pluralize(count, noun)
}

static ENV: OnceLock<std::result::Result<Environment<'static>, String>> = OnceLock::new();

fn build_env() -> std::result::Result<Environment<'static>, String> {
    let mut env = Environment::new();
    for (name, source) in [
        (LAYOUT_NAME, LAYOUT),
        (LANDING_NAME, LANDING),
        (DETAIL_NAME, DETAIL),
        (NOTICE_NAME, NOTICE),
    ] {
        env.add_template(name, source)
            .map_err(|e| format!("compiling journal template {name}: {e}"))?;
    }
    Ok(env)
}

fn env() -> Result<&'static Environment<'static>> {
    match ENV.get_or_init(build_env) {
        Ok(env) => Ok(env),
        Err(message) => Err(anyhow!("{message}")),
    }
}

fn render<T: Serialize>(template: &str, view: &T) -> Result<String> {
    let tmpl = env()?
        .get_template(template)
        .map_err(|e| anyhow!("loading journal template {template}: {e}"))?;
    tmpl.render(minijinja::context! { view => minijinja::Value::from_serialize(view) })
        .map_err(|e| anyhow!("rendering journal template {template}: {e}"))
}

pub fn board(view: &BoardView) -> Result<String> {
    render(LANDING_NAME, view)
}

pub fn detail(view: &DetailView) -> Result<String> {
    render(DETAIL_NAME, view)
}

pub fn notice(view: &NoticeView) -> Result<String> {
    render(NOTICE_NAME, view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::state::{
        board_feed_of, build_board, build_detail, sample_cluster, sample_item, test_now, BoardFeed,
        DetailContext, ResolveReceipt,
    };
    use crate::storage::dream_items::{DreamItem, DreamItemGrade};

    fn adverse(id: &str) -> crate::storage::dream_clusters::DreamCluster {
        sample_cluster(
            id,
            "csr",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        )
    }

    fn board_html(feed: &BoardFeed) -> String {
        board(&build_board(feed, None, test_now())).expect("render")
    }

    fn detail_html(item: &DreamItem, context: &DetailContext) -> String {
        detail(&build_detail(item, context, "token-abc".into())).expect("render")
    }

    #[test]
    fn every_template_compiles() {
        env().expect("journal templates must compile");
    }

    #[test]
    fn the_board_renders_a_complete_document_with_all_three_columns() {
        let html = board_html(&board_feed_of(vec![adverse("c00")]));
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("</html>"));
        for column in ["Proposals", "Observations", "Outdated claims"] {
            assert!(html.contains(column), "missing column {column}");
        }
        // minijinja escapes `/` as `&#x2f;` (OWASP set), so the file path in
        // the conclusion sentence is asserted in its escaped form.
        assert!(html.contains(
            "run_report in csr-engine&#x2f;src&#x2f;dream&#x2f;report.rs was witnessed anchor obsolete."
        ));
        // minijinja's HTML auto-escape follows the OWASP set, which includes
        // `/` → `&#x2f;`. That is a character reference in an attribute
        // value, so the browser resolves it back to `/dream/…`; the escape is
        // not weakened here just to make the raw bytes prettier.
        // `routes::tests::board_links_resolve_when_followed` proves the link
        // actually works by following it through the router.
        assert!(html.contains(r#"href="&#x2f;dream&#x2f;c00-item0""#));
        assert!(html.contains("1 conclusion · 1 affected item"));
        // Server-rendered and complete in the DOM: no script tag at all, so
        // there is no JS-off degradation to reason about.
        assert!(!html.contains("<script"), "the board must ship zero JS");
    }

    /// A three-line index card and nothing more: caps, conclusion, mono meta,
    /// plus exactly one micro copy icon. No worded copy button, no spend, no
    /// affected-item rows, no absolute dates.
    #[test]
    fn an_index_card_is_three_lines_with_one_micro_copy_icon() {
        let html = board_html(&board_feed_of(vec![adverse("c00")]));
        let card_at = html.find(r#"id="dream-c00""#).expect("card");
        let card = &html[card_at..card_at + html[card_at..].find("</li>").expect("card end")];

        assert_eq!(card.matches(r#"class="card-caps""#).count(), 1);
        assert_eq!(card.matches(r#"class="card-conclusion""#).count(), 1);
        assert_eq!(card.matches(r#"class="card-meta""#).count(), 1);
        assert_eq!(
            card.matches(r#"class="card-copy""#).count(),
            1,
            "exactly one micro copy icon, top right"
        );
        assert!(!card.contains("<button"), "no worded action on the index");
        assert!(
            !card.contains("card-spend"),
            "spend belongs on the detail page"
        );
        assert!(
            !card.contains("2026-08-09"),
            "absolute dates move to detail"
        );
        assert!(card.contains("1d ago"));
        assert!(card.contains("⌗abcdef12"));
    }

    #[test]
    fn a_session_grade_card_gets_no_saturated_rail() {
        let session = sample_cluster(
            "s00",
            "csr",
            DreamItemGrade::SessionGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        let html = board_html(&board_feed_of(vec![session]));
        assert!(html.contains(r#"class="card session-grade"#));
        assert!(
            html.contains(r#"<span class="grade">session-grade</span>"#),
            "the grade must be stated explicitly instead of implied by colour"
        );
        // The stylesheet is where the rule lives; assert the rule exists so a
        // later edit cannot quietly give session-grade the same 3px rail.
        assert!(html.contains(".card.session-grade { border-left-width: 1px;"));
    }

    #[test]
    fn an_unreceipted_card_is_neutral_dashed_not_live_and_unranked() {
        let unverified = sample_cluster(
            "u00",
            "csr",
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            None,
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        );
        let html = board_html(&board_feed_of(vec![unverified]));
        assert!(html.contains("not-live"));
        assert!(html.contains("NOT LIVE · EVIDENCE UNVERIFIED"));
        assert!(html.contains("hue-neutral"));
        assert!(html.contains("receipt unavailable"));
        for hue in ["hue-obsolete", "hue-superseded", "hue-reinstated"] {
            let card_at = html.find(r#"id="dream-u00""#).expect("card");
            let card = &html[card_at..card_at + html[card_at..].find("</li>").expect("end")];
            assert!(
                !card.contains(hue),
                "unverified evidence must never borrow the {hue} semantic colour"
            );
        }
    }

    #[test]
    fn the_board_empty_state_is_explicit_and_never_an_all_clear() {
        let html = board_html(&BoardFeed::default());
        assert!(html.contains("Nothing on record yet."));
        assert!(html.contains("This is an empty feed, not a failure"));
        assert!(!html.contains("<li class=\"card"));
        assert!(!html.to_lowercase().contains("all clear"));
    }

    /// Ban 2 in the P6 spec: an active nav item is brighter text plus a small
    /// leading dot, never a bar, pill, tab shape or background block.
    #[test]
    fn the_nav_marks_the_current_item_with_a_dot_and_never_with_a_bar() {
        let html = board_html(&board_feed_of(vec![adverse("c00")]));
        let style_end = html.find("</style>").expect("stylesheet");
        let css = &html[..style_end];
        let nav_at = css.find(".nav a.current").expect("current-item rule");
        let nav_rules = &css[nav_at..nav_at + 220];
        assert!(
            nav_rules.contains(r#"content: "· ""#),
            "dot marker required"
        );
        for banned in ["border-left", "background", "border-radius"] {
            assert!(
                !nav_rules.contains(banned),
                "the current nav item must not be marked with {banned}"
            );
        }
        assert!(html.contains(r#"<a class="current" href="/">Board</a>"#));
    }

    /// Ban 1, 3 and 4: no manila fill, no rounded floating frame, nothing
    /// non-flat.
    #[test]
    fn the_stylesheet_obeys_the_four_design_bans() {
        let html = board_html(&board_feed_of(vec![adverse("c00")]));
        let css = &html[..html.find("</style>").expect("stylesheet")];
        for banned in [
            "linear-gradient",
            "radial-gradient",
            "box-shadow",
            "backdrop-filter",
            "text-shadow",
        ] {
            assert!(!css.contains(banned), "flat only: {banned} is banned");
        }
        // Radius <= 3px everywhere: the two radius tokens are the only source.
        assert!(css.contains("--radius-pane: 3px;"));
        assert!(css.contains("--radius-tile: 2px;"));
        for rem_radius in [
            "border-radius: 999px",
            "border-radius: 1.25rem",
            "border-radius: 0.75rem",
        ] {
            assert!(
                !css.contains(rem_radius),
                "radius must stay <= 3px: {rem_radius}"
            );
        }
        // No manila / beige / tan / cream fill anywhere in the palette.
        for manila in [
            "#f5f0e1", "#faf3e0", "#f0e6d2", "#e8dcc0", "bisque", "wheat", "tan;",
        ] {
            assert!(
                !css.to_lowercase().contains(manila),
                "manila-family fill {manila} is banned on every surface"
            );
        }
    }

    #[test]
    fn html_is_escaped_so_item_text_cannot_inject_markup() {
        let mut cluster = adverse("c00");
        cluster.conclusion.symbol = Some("<script>alert('x')</script>".to_string());
        let html = board_html(&board_feed_of(vec![cluster]));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn both_themes_are_defined_and_light_is_defined_on_bare_root() {
        let html = board_html(&BoardFeed::default());
        let bare = html.find("  :root {").expect("bare :root light palette");
        let media = html
            .find("@media (prefers-color-scheme: dark)")
            .expect("dark media query");
        let attr = html
            .find(r#":root[data-theme="dark"]"#)
            .expect("data-theme dark block");
        assert!(bare < media && media < attr, "token blocks out of order");
        assert!(
            html[media..attr].contains(r#":not([data-theme="light"])"#),
            "the dark media query must yield to an explicit light choice"
        );

        // Semantic tokens are aliases defined ONCE, on bare `:root`, over a
        // base palette. Both dark blocks re-point the bases, so the aliases
        // follow without being restated. Asserting the aliases appear inside
        // the dark blocks would be asserting duplication, not correctness —
        // what has to hold is: every alias is defined on bare `:root`, and
        // every base it resolves to is redefined in BOTH dark blocks.
        const ALIASES: [(&str, &str); 5] = [
            ("--bg-card:", "--glass:"),
            ("--fg:", "--ink:"),
            ("--fg-muted:", "--slate:"),
            ("--border:", "--glass-border:"),
            ("--accent:", "--purple:"),
        ];
        for (alias, base) in ALIASES {
            assert!(
                html[..media].contains(alias),
                "alias {alias} must be defined on bare :root"
            );
            assert!(
                html[..media].contains(base),
                "light palette missing base {base}"
            );
            assert!(
                html[media..attr].contains(base),
                "prefers-color-scheme dark block missing base {base}"
            );
            assert!(
                html[attr..].contains(base),
                "[data-theme=dark] block missing base {base}"
            );
        }

        // Every semantic verdict hue exists in BOTH modes too — a card in
        // light mode must not fall back to an undefined colour.
        for hue in ["--red:", "--amber:", "--green:", "--neutral:"] {
            assert!(html[..media].contains(hue), "light palette missing {hue}");
            assert!(html[media..attr].contains(hue), "dark media missing {hue}");
            assert!(
                html[attr..].contains(hue),
                "[data-theme=dark] missing {hue}"
            );
        }
    }

    #[test]
    fn detail_leads_with_the_evidence_contract_not_the_imperative() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail_html(&item, &DetailContext::default());
        let contract = html.find("Evidence contract").expect("contract heading");
        let touch = html.find("Touch next").expect("touch next heading");
        assert!(
            contract < touch,
            "codex IA correction 3: evidence contract must precede TOUCH NEXT"
        );
        assert!(html.contains("⌗abcdef12"));
    }

    /// Section order is the codex IA memo's binding correction 3. Asserting
    /// the *rendered byte offsets* (not the struct field order) is what makes
    /// this a real guard: a template edit that moves churn above AST, or puts
    /// the imperative first, fails here.
    #[test]
    fn detail_sections_render_in_the_mandated_order() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail_html(&item, &DetailContext::default());

        let at = |needle: &str| {
            html.find(needle)
                .unwrap_or_else(|| panic!("missing section heading: {needle}\n{html}"))
        };
        let contract = at("Evidence contract");
        let touch = at("Touch next");
        let description = at("What changed, and why this surfaced");
        let proposal = at("Proposal");
        let ast = at("AST before / after");
        let churn = at("Churn heat");

        assert!(contract < touch, "the imperative must never lead");
        assert!(touch < description);
        assert!(description < proposal);
        assert!(proposal < ast, "the description must precede the AST slot");
        assert!(ast < churn, "churn is LAST, as secondary context");
        assert_eq!(
            churn,
            *[contract, touch, description, proposal, ast, churn]
                .iter()
                .max()
                .expect("non-empty"),
            "nothing may render after churn"
        );
    }

    /// The two write forms, on the detail page, carrying the token and
    /// posting same-origin. There is no control that moves a card rightward.
    #[test]
    fn the_detail_page_carries_both_write_forms_and_the_token() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail_html(&item, &DetailContext::default());
        // The path literal is template text; only `{{ view.id }}` passes
        // through minijinja's escape, so the action reads plainly here.
        assert!(html.contains(r#"action="/dream/id00/resolve""#));
        assert!(html.contains(r#"action="/dream/id00/dismiss""#));
        assert_eq!(
            html.matches(r#"name="csrf" value="token-abc""#).count(),
            2,
            "each form must carry the per-render token"
        );
        assert_eq!(
            html.matches(r#"method="post""#).count(),
            2,
            "exactly two write forms, and no third"
        );
        assert!(html.contains("a dismissal is never stored as a resolution"));
    }

    #[test]
    fn churn_is_captioned_as_context_and_never_as_importance() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail_html(&item, &DetailContext::default());
        assert!(html.contains("CONTEXT · NOT USED FOR RANKING"));
        assert!(
            html.contains("measurement, not a measurement of zero activity"),
            "an unmeasured churn window must say so"
        );
        // Scan the churn section itself, not the whole document — the
        // stylesheet's own comment explains *why* warm bars are banned and
        // is allowed to use the word.
        let section = &html[html.find("Churn heat").expect("churn heading")..];
        for banned in ["most important", "priority", "top file", "matters most"] {
            assert!(
                !section.to_lowercase().contains(banned),
                "churn must never be framed as importance: {banned}"
            );
        }
    }

    /// Churn must not appear on the landing surface at ALL — warm or not, it
    /// would read as justifying the ranking.
    #[test]
    fn churn_never_reaches_the_board() {
        let html = board_html(&board_feed_of(vec![adverse("c00")]));
        let body = &html[html.find("</style>").expect("stylesheet")..];
        assert!(!body.to_lowercase().contains("churn"));
        assert!(!body.contains("touches"));
    }

    /// An AST comparison that cannot be resolved renders its abstention
    /// sentence **in the same slot** the trees would have occupied — never a
    /// missing section, never an error-red empty tree, never invented nodes.
    #[test]
    fn an_unresolvable_ast_renders_an_abstention_in_the_same_slot() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let context = DetailContext {
            ast_abstention: Some(
                "AST comparison abstained: no deterministic node at current HEAD matched \
                 witnessed anchor ⌗abcdef12."
                    .to_string(),
            ),
            ..DetailContext::default()
        };
        let html = detail_html(&item, &context);

        let ast = html.find("AST before / after").expect("ast heading");
        let abstention = html.find("AST comparison abstained").expect("abstention");
        let churn = html.find("Churn heat").expect("churn heading");
        assert!(
            ast < abstention && abstention < churn,
            "the abstention must sit between the AST heading and the next section"
        );
        assert!(html.contains(r#"class="abstained""#));
        assert!(
            !html.contains(r#"class="ast-grid""#),
            "no tree markup may be emitted when nothing resolved"
        );
    }

    #[test]
    fn spend_markup_is_absent_entirely_when_nothing_was_recorded() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail_html(&item, &DetailContext::default());
        assert!(!html.contains(r#"<p class="spend">"#));
        assert!(
            !html.contains("$0.00"),
            "a zero would read as 'this dream was free'"
        );
    }

    #[test]
    fn spend_renders_on_the_detail_when_recorded() {
        use crate::storage::queries::NarrativeUsageByModel;

        let item = sample_item("id00", "csr", "finish the release gate");
        let spend = crate::journal::composer::DreamSpend::from_rows(&[NarrativeUsageByModel {
            model: "claude-sonnet-5".into(),
            calls: 2,
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }])
        .expect("rows");

        let context = DetailContext {
            spend: Some(spend),
            ..DetailContext::default()
        };
        let html = detail_html(&item, &context);
        assert!(html.contains(r#"<p class="spend">"#));
        assert!(html.contains("1,000,000 in · 1,000,000 out"));
        assert!(html.contains("≈$18.0000 at list price"));
        assert!(html.contains("2 calls · claude-sonnet-5"));
    }

    #[test]
    fn the_brief_gets_its_own_scroll_context_and_states_its_truncation() {
        use crate::journal::composer::{build_brief, EpisodeFacts};

        let item = sample_item("id00", "csr", "finish the release gate");
        let episode = EpisodeFacts {
            request: Some("close the v10.1 release gate".to_string()),
            completed: Some("wrote the gate but never ran it".to_string()),
            outcome: Some("partial".to_string()),
            files: vec![],
        };
        let context = DetailContext {
            brief: build_brief(&item, &episode, &[]),
            ..DetailContext::default()
        };
        let html = detail_html(&item, &context);
        assert!(html.contains(r#"class="brief" tabindex="0""#));
        assert!(html.contains("close the v10.1 release gate"));

        let empty = detail_html(&item, &DetailContext::default());
        assert!(empty.contains("The brief is empty because the"));
    }

    #[test]
    fn notice_renders_without_leaking_markup_from_the_path() {
        let html = notice(&NoticeView::not_found("/<img onerror=x>")).expect("render");
        assert!(html.contains("No such page"));
        assert!(!html.contains("<img onerror"));
    }

    /// The degraded detail page must say the sections are missing BECAUSE of
    /// the failure, and must not carry any of the ordinary absence sentences
    /// (codex X4 finding 9).
    #[test]
    fn the_degraded_detail_notice_never_reads_as_absence_of_evidence() {
        let html = notice(&NoticeView::detail_error(
            "finish the release gate",
            "database is locked",
        ))
        .expect("render");
        assert!(html.contains("database is locked"));
        assert!(html.contains("Nothing below is rendered from this failed read"));
        assert!(html.contains("their absence here is the error"));
        for absence in ["The brief is empty because", "No churn was counted"] {
            assert!(!html.contains(absence));
        }
    }

    #[test]
    fn a_recorded_verdict_notice_reports_only_measured_facts() {
        let receipt = ResolveReceipt {
            action: "dismiss",
            item_id: "id00".into(),
            item: "finish the release gate".into(),
            project: "csr".into(),
            origin_session: "sess-1".into(),
            status: "still_open",
            chunks: 3,
            origin: "journal_ui",
        };
        let html = notice(&NoticeView::recorded(&receipt)).expect("render");
        assert!(html.contains("Verdict recorded"));
        assert!(html.contains("3 resolution rows"));
        assert!(html.contains("journal_ui"));
        assert!(html.contains("no automated check verified the code"));
    }

    #[test]
    fn a_refused_write_notice_never_reads_as_a_success() {
        let html =
            notice(&NoticeView::forbidden("Cross-origin writes are refused.")).expect("render");
        assert!(html.contains("That write was refused"));
        assert!(html.contains("Nothing was written."));
        assert!(!html.contains("Verdict recorded"));
    }
}
