//! The journal's read-only route table.
//!
//! Security posture, restated where it is enforced:
//!
//! * Every route is `GET`. There is no write path in P1 — resolve/dismiss
//!   POSTs are P4/P5 work and land with origin tagging when they do.
//! * **No CORS header is ever set.** Not `Access-Control-Allow-Origin`, not
//!   `-Credentials`, not a preflight handler. A browser on this machine is
//!   the only intended client and it needs none of them.
//! * The `{id}` path parameter is compared against ids that came out of the
//!   feed. It is never interpolated into SQL, a path, or a command — an
//!   unknown id can only produce a 404.
//! * Responses carry `Content-Security-Policy: default-src 'none'` plus
//!   `nosniff`, `frame-ancestors 'none'` and `base-uri 'none'`. The pages
//!   ship no JavaScript and load no external asset, so the policy is not
//!   aspirational: it is what the documents already do.
//! * Feed queries run on `spawn_blocking`. The daemon hosts this server;
//!   a SQLite mutex must never be taken on a runtime thread.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use super::render::{self, NoticeView};
use super::state::{self, JournalState};
use crate::storage::dream_items::DreamItem;

/// `default-src 'none'` is accurate, not aspirational: these documents load
/// nothing. `style-src 'unsafe-inline'` covers the single inlined `<style>`
/// block; there is no `script-src` grant because there is no script.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; \
                   form-action 'none'; frame-ancestors 'none'; base-uri 'none'";

#[derive(Debug, Default, Deserialize)]
struct PageQuery {
    /// Decimal row offset. Anything else is a 400 — a broken link should be
    /// visible, not silently reset to page one.
    cursor: Option<String>,
    /// Page size. Kept as a string so a garbled value degrades to the
    /// default instead of producing an opaque extractor rejection: a page
    /// size is a display preference, not evidence.
    limit: Option<String>,
}

impl PageQuery {
    fn limit(&self) -> usize {
        state::clamp_limit(self.limit.as_deref().and_then(|v| v.trim().parse().ok()))
    }
}

/// Build the route table. Callers that must honour the kill switch use
/// [`super::router_if_enabled`] instead.
pub fn router(app_state: JournalState) -> Router {
    Router::new()
        .route("/", get(landing))
        // axum 0.8 spells path parameters `{id}`, not `:id` (0.7 syntax).
        // The served URL is `/dream/<id>` either way.
        .route("/dream/{id}", get(detail))
        .route("/api/dreams", get(api_dreams))
        .route("/healthz", get(healthz))
        .fallback(not_found)
        .with_state(app_state)
}

fn secure(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}

fn html(status: StatusCode, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    secure(response)
}

/// Render a notice page. If even the notice template fails, fall back to a
/// plain-text body rather than panicking inside a daemon-hosted task.
fn notice(status: StatusCode, view: NoticeView) -> Response {
    match render::notice(&view) {
        Ok(body) => html(status, body),
        Err(error) => {
            tracing::warn!(%error, "journal notice template failed");
            let mut response = Response::new(Body::from(format!(
                "{}\n{}\n(notice template failed: {error})\n",
                view.heading, view.detail
            )));
            *response.status_mut() = status;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            secure(response)
        }
    }
}

fn render_or_500(rendered: anyhow::Result<String>) -> Response {
    match rendered {
        Ok(body) => html(StatusCode::OK, body),
        Err(error) => {
            tracing::warn!(%error, "journal render failed");
            notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::feed_error(&error.to_string()),
            )
        }
    }
}

type Loaded = (Vec<DreamItem>, Option<(String, String)>);

/// Load the feed off the runtime. Errors are surfaced as strings so both the
/// HTML and JSON routes can report the real reason instead of an empty page
/// that would read as "nothing on record".
async fn load(app_state: &JournalState) -> Result<Loaded, String> {
    let feed = app_state.feed();
    let joined = tokio::task::spawn_blocking(move || (feed.load(), feed.last_pass())).await;
    match joined {
        Err(error) => Err(format!("feed task failed: {error}")),
        Ok((Err(error), _)) => Err(error.to_string()),
        Ok((Ok(items), last_pass)) => Ok((items, last_pass)),
    }
}

async fn landing(
    State(app_state): State<JournalState>,
    Query(query): Query<PageQuery>,
) -> Response {
    let offset = match state::parse_cursor(query.cursor.as_deref()) {
        Ok(offset) => offset,
        Err(bad) => {
            return notice(
                StatusCode::BAD_REQUEST,
                NoticeView::bad_request(&bad.to_string()),
            )
        }
    };
    let (items, last_pass) = match load(&app_state).await {
        Ok(loaded) => loaded,
        Err(reason) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::feed_error(&reason),
            )
        }
    };
    // `spend_for` takes the SQLite mutex per card, so the whole projection
    // runs on a blocking thread — the daemon hosts this server and a runtime
    // thread must never block on the database.
    let feed = app_state.feed();
    let limit = query.limit();
    let built = tokio::task::spawn_blocking(move || {
        state::build_landing(&items, last_pass, offset, limit, &|item| {
            feed.spend_for(item)
        })
    })
    .await;
    match built {
        Ok(view) => render_or_500(render::landing(&view)),
        Err(error) => notice(
            StatusCode::INTERNAL_SERVER_ERROR,
            NoticeView::feed_error(&format!("landing projection failed: {error}")),
        ),
    }
}

async fn detail(State(app_state): State<JournalState>, Path(id): Path<String>) -> Response {
    let (items, _) = match load(&app_state).await {
        Ok(loaded) => loaded,
        Err(reason) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::feed_error(&reason),
            )
        }
    };
    // Validation against stored rows, not against a pattern: the id has to
    // BE one of the ids the feed just produced.
    let Some(item) = state::find_item(&items, &id).cloned() else {
        return notice(StatusCode::NOT_FOUND, NoticeView::unknown_dream(&id));
    };
    // The composer reads git blobs and SQLite; both are blocking, and the
    // daemon hosts this server. Nothing here invokes a model — the plan is
    // read from `dream_plans`, never proposed on a GET.
    let feed = app_state.feed();
    let context = match tokio::task::spawn_blocking(move || feed.detail_context(&item)).await {
        Ok(context) => context,
        Err(error) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::feed_error(&format!("detail composition failed: {error}")),
            )
        }
    };
    let Some(item) = state::find_item(&items, &id) else {
        return notice(StatusCode::NOT_FOUND, NoticeView::unknown_dream(&id));
    };
    render_or_500(render::detail(&state::build_detail(item, &context)))
}

async fn api_dreams(
    State(app_state): State<JournalState>,
    Query(query): Query<PageQuery>,
) -> Response {
    let offset = match state::parse_cursor(query.cursor.as_deref()) {
        Ok(offset) => offset,
        Err(bad) => {
            let body = Json(serde_json::json!({ "error": bad.to_string() }));
            return secure((StatusCode::BAD_REQUEST, body).into_response());
        }
    };
    let (items, _) = match load(&app_state).await {
        Ok(loaded) => loaded,
        Err(reason) => {
            let body = Json(serde_json::json!({ "error": reason }));
            return secure((StatusCode::INTERNAL_SERVER_ERROR, body).into_response());
        }
    };
    let feed = app_state.feed();
    let limit = query.limit();
    let built = tokio::task::spawn_blocking(move || {
        state::build_page(&items, offset, limit, &|item| feed.spend_for(item))
    })
    .await;
    match built {
        Ok(page) => secure(Json(page).into_response()),
        Err(error) => {
            let body =
                Json(serde_json::json!({ "error": format!("page projection failed: {error}") }));
            secure((StatusCode::INTERNAL_SERVER_ERROR, body).into_response())
        }
    }
}

async fn healthz() -> Response {
    let mut response = Response::new(Body::from("ok\n"));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    secure(response)
}

async fn not_found(uri: Uri) -> Response {
    notice(StatusCode::NOT_FOUND, NoticeView::not_found(uri.path()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::state::{sample_item, StaticDreamFeed};
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn state_with(n: usize) -> JournalState {
        let items = (0..n)
            .map(|i| sample_item(&format!("id{i:02}"), "csr", &format!("open item {i}")))
            .collect();
        JournalState::new(Arc::new(StaticDreamFeed::new(items)))
    }

    struct BrokenFeed;
    impl crate::journal::state::DreamFeed for BrokenFeed {
        fn load(&self) -> anyhow::Result<Vec<DreamItem>> {
            Err(anyhow::anyhow!("database is locked"))
        }
    }

    async fn get_path(
        app_state: JournalState,
        uri: &str,
    ) -> (StatusCode, Vec<(String, String)>, String) {
        let response = router(app_state)
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router response");
        let status = response.status();
        let headers = response
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("body");
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    #[tokio::test]
    async fn landing_renders_items() {
        let (status, headers, body) = get_path(state_with(3), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("open item 0"));
        assert!(body.contains("open item 2"));
        assert!(body.contains("3 open items"));
        assert!(
            body.starts_with("<!doctype html>"),
            "document must be complete"
        );
        assert!(
            !body.contains("<script"),
            "first page must work with JS off"
        );
        let content_type = headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(content_type, "text/html; charset=utf-8");
    }

    #[tokio::test]
    async fn landing_on_empty_feed_is_honest() {
        let (status, _, body) = get_path(state_with(0), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Nothing on record yet."));
        assert!(body.contains("This is an empty feed, not a failure"));
    }

    #[tokio::test]
    async fn no_cors_header_is_ever_set() {
        for uri in ["/", "/dream/id00", "/api/dreams", "/healthz", "/nope"] {
            let (_, headers, _) = get_path(state_with(1), uri).await;
            for (name, _) in &headers {
                assert!(
                    !name.starts_with("access-control-"),
                    "{uri} returned a CORS header {name}"
                );
            }
        }
    }

    #[tokio::test]
    async fn responses_carry_the_no_network_csp() {
        let (_, headers, _) = get_path(state_with(1), "/").await;
        let csp = headers
            .iter()
            .find(|(k, _)| k == "content-security-policy")
            .map(|(_, v)| v.clone())
            .expect("csp header");
        assert!(csp.starts_with("default-src 'none'"));
        assert!(!csp.contains("script-src"));
    }

    #[tokio::test]
    async fn detail_renders_a_known_id() {
        let (status, _, body) = get_path(state_with(2), "/dream/id01").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("open item 1"));
        let contract = body.find("Evidence contract").expect("contract");
        let touch = body.find("Touch next").expect("touch next");
        assert!(contract < touch);
    }

    /// The route must actually *wire* the composer through, not merely be
    /// capable of it: this asserts the injected brief / copy block / plan /
    /// spend / AST / churn all reach the rendered document, in order.
    #[tokio::test]
    async fn detail_route_renders_the_composed_sections() {
        use crate::journal::composer::{
            build_brief, build_copy_block, DreamSpend, EpisodeFacts, PlanStep, StoredPlan,
        };
        use crate::journal::state::DetailContext;
        use crate::storage::dream_items::ChurnTile;
        use crate::storage::queries::NarrativeUsageByModel;

        let item = sample_item("id00", "csr", "open item 0");
        let episode = EpisodeFacts {
            request: Some("close the release gate".to_string()),
            completed: Some("wrote the gate but never ran it".to_string()),
            outcome: None,
            files: vec![],
        };
        let plan = StoredPlan {
            plan_hash: "hash-plan".into(),
            item_id: "id00".into(),
            context: "the gate exists but was never run".into(),
            steps: vec![PlanStep {
                action: "run the gate end to end".into(),
                files: vec!["csr-engine/src/dream/report.rs".into()],
                citation: "wrote the gate but never ran it".into(),
            }],
            files: vec!["csr-engine/src/dream/report.rs".into()],
            acceptance: Some("the gate exits zero".into()),
            dropped: 2,
            model: "sonnet-5".into(),
            created_at: "2026-08-10T00:00:00Z".into(),
        };
        let context = DetailContext {
            brief: build_brief(&item, &episode, &[]),
            copy_block: Some(build_copy_block(&item, &episode, &[], Some(&plan))),
            plan: Some(plan),
            spend: DreamSpend::from_rows(&[NarrativeUsageByModel {
                model: "claude-sonnet-5".into(),
                calls: 1,
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            }]),
            ast: None,
            ast_abstention: Some("AST comparison abstained: fixture has no repository.".into()),
            churn: vec![ChurnTile {
                file: "csr-engine/src/dream/report.rs".into(),
                touches: 7,
            }],
        };

        let app_state = JournalState::new(Arc::new(
            StaticDreamFeed::new(vec![item]).with_context(context),
        ));
        let (status, _, body) = get_path(app_state, "/dream/id00").await;
        assert_eq!(status, StatusCode::OK);

        // Brief + copy block, both composed from stored rows.
        assert!(body.contains("close the release gate"));
        assert!(body.contains("Copy block"));
        assert!(body.contains("Resume: open item 0"));
        // The no-night-pass gap is stated, not left blank.
        assert!(body.contains("No night-pass thread is on record for this session"));
        // Plan, labelled and citation-carrying.
        assert!(body.contains("proposed — not executed"));
        assert!(body.contains("run the gate end to end"));
        assert!(body.contains("traces to: wrote the gate but never ran it"));
        assert!(body.contains("2 drafted steps dropped by the verifier"));
        // Spend, on the detail view.
        assert!(body.contains("1,000,000 in · 1,000,000 out"));
        // AST abstention in the slot; churn measured and last.
        assert!(body.contains("AST comparison abstained: fixture has no repository."));
        assert!(body.contains("7 touches"));

        let proposal = body.find("Proposal").expect("proposal");
        let ast = body.find("AST before / after").expect("ast");
        let churn = body.find("Churn heat").expect("churn");
        assert!(proposal < ast && ast < churn);
    }

    #[tokio::test]
    async fn a_dream_with_no_recorded_usage_shows_no_spend_anywhere() {
        let app_state = state_with(2);
        let (_, _, landing_body) = get_path(app_state.clone(), "/").await;
        assert!(!landing_body.contains(r#"<p class="card-spend">"#));

        let (_, _, detail_body) = get_path(app_state.clone(), "/dream/id00").await;
        assert!(!detail_body.contains(r#"<p class="spend">"#));

        let (_, _, api_body) = get_path(app_state, "/api/dreams").await;
        let page: serde_json::Value = serde_json::from_str(&api_body).expect("json");
        assert!(
            page["items"][0]["spend"].is_null(),
            "the API must report absence as null, never as a zeroed object"
        );
    }

    #[tokio::test]
    async fn detail_404s_on_an_unknown_id() {
        let (status, _, body) = get_path(state_with(2), "/dream/does-not-exist").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("No dream with that id"));
    }

    #[tokio::test]
    async fn detail_404s_on_a_traversal_attempt_without_touching_disk() {
        let (status, _, _) = get_path(state_with(2), "/dream/..%2F..%2Fetc%2Fpasswd").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unknown_path_404s() {
        let (status, _, body) = get_path(state_with(1), "/admin").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("No such page"));
    }

    #[tokio::test]
    async fn healthz_is_plain_ok() {
        let (status, headers, body) = get_path(state_with(0), "/healthz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ok\n");
        assert!(headers
            .iter()
            .any(|(k, v)| k == "content-type" && v.starts_with("text/plain")));
    }

    #[tokio::test]
    async fn api_pagination_walks_the_cursor_to_the_end() {
        let app_state = state_with(5);

        let (status, _, body) = get_path(app_state.clone(), "/api/dreams?limit=2").await;
        assert_eq!(status, StatusCode::OK);
        let first: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(first["total"], 5);
        assert_eq!(first["items"].as_array().expect("items").len(), 2);
        assert_eq!(first["next_cursor"], "2");
        assert_eq!(first["items"][0]["id"], "id00");

        let (_, _, body) = get_path(app_state.clone(), "/api/dreams?cursor=2&limit=2").await;
        let second: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(second["items"][0]["id"], "id02");
        assert_eq!(second["next_cursor"], "4");

        let (_, _, body) = get_path(app_state, "/api/dreams?cursor=4&limit=2").await;
        let last: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(last["items"].as_array().expect("items").len(), 1);
        assert!(
            last["next_cursor"].is_null(),
            "the last page must not advertise another"
        );
    }

    #[tokio::test]
    async fn api_rejects_a_garbled_cursor_instead_of_resetting() {
        let (status, _, body) = get_path(state_with(3), "/api/dreams?cursor=abc").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid cursor"));
    }

    #[tokio::test]
    async fn api_limit_is_capped() {
        let (_, _, body) = get_path(state_with(60), "/api/dreams?limit=9999").await;
        let page: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(
            page["items"].as_array().expect("items").len(),
            state::MAX_PAGE_LIMIT
        );
        assert_eq!(page["total"], 60);
    }

    /// Undo minijinja's HTML auto-escape (OWASP set) on an attribute value,
    /// the way a browser does before it follows the link.
    fn unescape(value: &str) -> String {
        value
            .replace("&#x2f;", "/")
            .replace("&#x27;", "'")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    fn first_href_after(body: &str, marker: &str) -> String {
        let tail = &body[body.find(marker).unwrap_or_else(|| panic!("no {marker}"))..];
        let start = tail.find("href=\"").expect("href") + 6;
        let end = tail[start..].find('"').expect("href end") + start;
        unescape(&tail[start..end])
    }

    #[tokio::test]
    async fn landing_cursor_paginates_with_real_links_for_js_off() {
        let (_, _, body) = get_path(state_with(5), "/?limit=2").await;
        // Escaped form is what minijinja emits; `unescape` is what a browser
        // resolves it to. Both are asserted so a change to either the URL
        // builder or the escaping is visible here.
        assert!(body.contains(r#"href="&#x2f;?cursor=2&amp;limit=2""#));
        assert_eq!(
            first_href_after(&body, "class=\"pager\""),
            "/?cursor=2&limit=2"
        );
        assert!(body.contains("showing 1–2 of 5"));

        let (_, _, body) = get_path(state_with(5), "/?cursor=4&limit=2").await;
        assert!(body.contains("showing 5–5 of 5"));
        assert!(!body.contains("next →"));
    }

    /// The links the server-rendered first page emits must actually resolve
    /// through the router with JavaScript off — that is the whole point of
    /// rendering the page complete in the DOM.
    #[tokio::test]
    async fn landing_links_resolve_when_followed() {
        let app_state = state_with(5);

        let (_, _, body) = get_path(app_state.clone(), "/?limit=2").await;
        let card = first_href_after(&body, "class=\"card-title\"");
        assert_eq!(card, "/dream/id00");
        let (status, _, detail_body) = get_path(app_state.clone(), &card).await;
        assert_eq!(status, StatusCode::OK);
        assert!(detail_body.contains("open item 0"));

        let next = first_href_after(&body, "class=\"pager\"");
        let (status, _, next_body) = get_path(app_state, &next).await;
        assert_eq!(status, StatusCode::OK);
        assert!(next_body.contains("open item 2"));
        assert!(next_body.contains("showing 3–4 of 5"));
    }

    #[tokio::test]
    async fn landing_rejects_a_garbled_cursor() {
        let (status, _, body) = get_path(state_with(3), "/?cursor=nope").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("invalid cursor"));
    }

    #[tokio::test]
    async fn a_failing_feed_reports_the_reason_instead_of_an_empty_page() {
        let app_state = JournalState::new(Arc::new(BrokenFeed));
        let (status, _, body) = get_path(app_state.clone(), "/").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("database is locked"));
        assert!(
            !body.contains("Nothing on record yet."),
            "a read failure must never render as an empty feed"
        );

        let (status, _, body) = get_path(app_state, "/api/dreams").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("database is locked"));
    }
}
