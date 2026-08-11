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

use super::state::{DetailView, LandingView};

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

pub fn landing(view: &LandingView) -> Result<String> {
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
    use crate::journal::composer::DreamSpend;
    use crate::journal::state::{build_detail, build_landing, sample_item, DetailContext};
    use crate::storage::dream_items::DreamItem;

    /// A feed with no recorded usage. Every template test runs through this
    /// so the "spend renders only when recorded" rule is the default here,
    /// not a special case.
    fn no_spend(_item: &DreamItem) -> Option<DreamSpend> {
        None
    }

    #[test]
    fn every_template_compiles() {
        env().expect("journal templates must compile");
    }

    #[test]
    fn landing_renders_a_complete_document_with_items() {
        let items = vec![sample_item("id00", "csr", "finish the release gate")];
        let html = landing(&build_landing(&items, None, 0, 20, &no_spend)).expect("render");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("</html>"));
        assert!(html.contains("finish the release gate"));
        // minijinja's HTML auto-escape follows the OWASP set, which includes
        // `/` → `&#x2f;`. That is a character reference in an attribute
        // value, so the browser resolves it back to `/dream/id00`; the
        // escape is not weakened here just to make the raw bytes prettier.
        // `routes::tests::landing_links_resolve_when_followed` proves the
        // link actually works by following it through the router.
        assert!(html.contains(r#"href="&#x2f;dream&#x2f;id00""#));
        assert!(html.contains("1 open item"));
        // First page complete in the DOM: no script tag is emitted at all,
        // so there is no JS-off degradation to reason about.
        assert!(!html.contains("<script"), "P1 landing must ship zero JS");
    }

    #[test]
    fn landing_empty_state_is_explicit() {
        let html = landing(&build_landing(&[], None, 0, 20, &no_spend)).expect("render");
        assert!(html.contains("Nothing on record yet."));
        assert!(html.contains("This is an empty feed, not a failure"));
        assert!(!html.contains("<li class=\"card\""));
    }

    #[test]
    fn detail_leads_with_the_evidence_contract_not_the_imperative() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail(&build_detail(&item, &DetailContext::default())).expect("render");
        let contract = html.find("Evidence contract").expect("contract heading");
        let touch = html.find("Touch next").expect("touch next heading");
        assert!(
            contract < touch,
            "codex IA correction 3: evidence contract must precede TOUCH NEXT"
        );
        assert!(html.contains("⌗abcdef12"));
    }

    #[test]
    fn html_is_escaped_so_item_text_cannot_inject_markup() {
        let items = vec![sample_item(
            "id00",
            "csr",
            "<script>alert('x')</script> fix the gate",
        )];
        let html = landing(&build_landing(&items, None, 0, 20, &no_spend)).expect("render");
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn both_themes_are_defined_and_light_is_defined_on_bare_root() {
        let html = landing(&build_landing(&[], None, 0, 20, &no_spend)).expect("render");
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
    }

    /// Section order is the codex IA memo's binding correction 3. Asserting
    /// the *rendered byte offsets* (not the struct field order) is what makes
    /// this a real guard: a template edit that moves churn above AST, or puts
    /// the imperative first, fails here.
    #[test]
    fn detail_sections_render_in_the_mandated_order() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail(&build_detail(&item, &DetailContext::default())).expect("render");

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

    #[test]
    fn churn_is_captioned_as_context_and_never_as_importance() {
        let item = sample_item("id00", "csr", "finish the release gate");
        let html = detail(&build_detail(&item, &DetailContext::default())).expect("render");
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
        let html = detail(&build_detail(&item, &context)).expect("render");

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
        let items = vec![sample_item("id00", "csr", "finish the release gate")];
        let landing_html = landing(&build_landing(&items, None, 0, 20, &no_spend)).expect("render");
        assert!(
            !landing_html.contains(r#"<p class="card-spend">"#),
            "an unmeasured dream must render no spend markup at all"
        );

        let detail_html =
            detail(&build_detail(&items[0], &DetailContext::default())).expect("render");
        assert!(!detail_html.contains(r#"<p class="spend">"#));
        assert!(
            !detail_html.contains("$0.00"),
            "a zero would read as 'this dream was free'"
        );
    }

    #[test]
    fn spend_renders_on_both_the_card_and_the_detail_when_recorded() {
        use crate::storage::queries::NarrativeUsageByModel;

        let items = vec![sample_item("id00", "csr", "finish the release gate")];
        let spend = crate::journal::composer::DreamSpend::from_rows(&[NarrativeUsageByModel {
            model: "claude-sonnet-5".into(),
            calls: 2,
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }])
        .expect("rows");

        let with_spend = |item: &DreamItem| {
            let _ = item;
            Some(spend.clone())
        };
        let landing_html =
            landing(&build_landing(&items, None, 0, 20, &with_spend)).expect("render");
        assert!(landing_html.contains(r#"<p class="card-spend">"#));
        assert!(landing_html.contains("1,000,000 in · 1,000,000 out"));
        assert!(landing_html.contains("≈$18.0000 at list price"));

        let context = DetailContext {
            spend: Some(spend),
            ..DetailContext::default()
        };
        let detail_html = detail(&build_detail(&items[0], &context)).expect("render");
        assert!(detail_html.contains(r#"<p class="spend">"#));
        assert!(detail_html.contains("2 calls · claude-sonnet-5"));
    }

    #[test]
    fn the_brief_gets_its_own_scroll_context_and_states_its_truncation() {
        use crate::journal::composer::{build_brief, EpisodeFacts};
        use crate::storage::dream_items::DreamItem as Item;

        let item: Item = sample_item("id00", "csr", "finish the release gate");
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
        let html = detail(&build_detail(&item, &context)).expect("render");
        assert!(html.contains(r#"class="brief" tabindex="0""#));
        assert!(html.contains("close the v10.1 release gate"));

        let empty = detail(&build_detail(&item, &DetailContext::default())).expect("render");
        assert!(empty.contains("The brief is empty because the"));
    }

    #[test]
    fn notice_renders_without_leaking_markup_from_the_path() {
        let html = notice(&NoticeView::not_found("/<img onerror=x>")).expect("render");
        assert!(html.contains("No such page"));
        assert!(!html.contains("<img onerror"));
    }
}
