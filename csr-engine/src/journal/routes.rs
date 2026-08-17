//! The journal's route table.
//!
//! Security posture, restated where it is enforced:
//!
//! * Every read route is `GET`. The **only** mutating routes are
//!   `POST /dream/{id}/resolve` and `POST /dream/{id}/dismiss`
//!   (locked decision 4: resolve + dismiss from the UI, loopback only, every
//!   write logged with origin `journal_ui`). `no_other_mutating_route_exists`
//!   probes the whole surface to keep it that way.
//! * **No CORS header is ever set.** Not `Access-Control-Allow-Origin`, not
//!   `-Credentials`, not a preflight handler. A browser on this machine is
//!   the only intended client and it needs none of them.
//! * The two write routes enforce, in order and before any write:
//!   1. **same-origin** — `Sec-Fetch-Site` must be absent or `same-origin`,
//!      and `Origin`, when present, must match the request's own `Host`;
//!   2. a bounded body — `Content-Length` over [`MAX_FORM_BYTES`], or a body
//!      that exceeds it while streaming, is a **413**, read no further;
//!   3. a **per-render CSRF token** bound to the target id; a missing or
//!      wrong one is a **403**;
//!   4. the target id **validated against stored rows** — the write is
//!      performed against the row the feed returned, never against the path
//!      parameter.
//! * The `{id}` path parameter is compared against ids that came out of the
//!   feed. It is never interpolated into SQL, a path, or a command — an
//!   unknown id can only produce a 404.
//! * Responses carry `Content-Security-Policy: default-src 'none'` plus
//!   `nosniff`, `frame-ancestors 'none'` and `base-uri 'none'`. The pages
//!   ship no JavaScript and load no external asset. `form-action 'self'` is
//!   the one grant the write forms need; it still forbids posting anywhere
//!   but this origin.
//! * Feed queries run on `spawn_blocking`. The daemon hosts this server;
//!   a SQLite mutex must never be taken on a runtime thread.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;

use super::render::{self, NoticeView};
use super::state::{self, JournalAction, JournalState, ResolveError};
use crate::storage::dream_items::DreamItem;

/// `default-src 'none'` is accurate, not aspirational: these documents load
/// nothing. `style-src 'unsafe-inline'` covers the single inlined `<style>`
/// block; there is no `script-src` grant because there is no script.
/// `form-action 'self'` is the minimum the resolve/dismiss forms need — it
/// permits posting to this origin and nowhere else.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src data:; \
                   form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Hard cap on a write request's body. The forms send two short fields; a
/// kilobyte is already generous, and anything larger is refused with 413
/// before it is parsed.
pub const MAX_FORM_BYTES: usize = 1024;

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
        .route("/", get(dreams_home))
        .route("/board", get(board))
        // axum 0.8 spells path parameters `{id}`, not `:id` (0.7 syntax).
        // The served URL is `/dream/<id>` either way.
        .route("/dream/{id}", get(detail))
        .route("/dream/{id}/resolve", post(resolve))
        .route("/dream/{id}/dismiss", post(dismiss))
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

type LoadedItems = (Vec<DreamItem>, Option<(String, String)>);

/// Load the per-item feed off the runtime. Errors are surfaced as strings so
/// both the HTML and JSON routes can report the real reason instead of an
/// empty page that would read as "nothing on record".
async fn load_items(app_state: &JournalState) -> Result<LoadedItems, String> {
    let feed = app_state.feed();
    let joined = tokio::task::spawn_blocking(move || (feed.load(), feed.last_pass())).await;
    match joined {
        Err(error) => Err(format!("feed task failed: {error}")),
        Ok((Err(error), _)) => Err(error.to_string()),
        Ok((Ok(items), last_pass)) => Ok((items, last_pass)),
    }
}

type LoadedBoard = (state::BoardFeed, Option<(String, String)>);

/// Load the cluster feed off the runtime.
async fn load_board(app_state: &JournalState) -> Result<LoadedBoard, String> {
    let feed = app_state.feed();
    let joined = tokio::task::spawn_blocking(move || (feed.load_board(), feed.last_pass())).await;
    match joined {
        Err(error) => Err(format!("feed task failed: {error}")),
        Ok((Err(error), _)) => Err(error.to_string()),
        Ok((Ok(board), last_pass)) => Ok((board, last_pass)),
    }
}

async fn dreams_home(State(app_state): State<JournalState>) -> Response {
    let now = Utc::now();
    let loaded = tokio::task::spawn_blocking(move || app_state.feed().load_week_dreams(now)).await;
    match loaded {
        Ok(Ok(dreams)) => render_or_500(render::dreams_home(&render::DreamsHomeView::from_week(
            dreams,
        ))),
        Ok(Err(error)) => notice(
            StatusCode::INTERNAL_SERVER_ERROR,
            NoticeView::feed_error(&error.to_string()),
        ),
        Err(error) => notice(
            StatusCode::INTERNAL_SERVER_ERROR,
            NoticeView::feed_error(&format!("week-dream task failed: {error}")),
        ),
    }
}

async fn board(State(app_state): State<JournalState>) -> Response {
    let (feed, last_pass) = match load_board(&app_state).await {
        Ok(loaded) => loaded,
        Err(reason) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::feed_error(&reason),
            )
        }
    };
    let now = Utc::now();
    let built =
        tokio::task::spawn_blocking(move || state::build_board(&feed, last_pass, now)).await;
    match built {
        Ok(view) => render_or_500(render::board(&view)),
        Err(error) => notice(
            StatusCode::INTERNAL_SERVER_ERROR,
            NoticeView::feed_error(&format!("board projection failed: {error}")),
        ),
    }
}

async fn detail(State(app_state): State<JournalState>, Path(id): Path<String>) -> Response {
    let (items, _) = match load_items(&app_state).await {
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
    let composed = item.clone();
    let context = match tokio::task::spawn_blocking(move || feed.detail_context(&composed)).await {
        // A read FAILURE is a degraded page, never an empty one: rendering
        // "no request, no completion, no night thread on record" off a locked
        // database would be evidence manufactured from an error
        // (codex X4 finding 9).
        Ok(Ok(context)) => context,
        Ok(Err(error)) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::detail_error(&item.item, &error.to_string()),
            )
        }
        Err(error) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::detail_error(&item.item, &format!("detail task failed: {error}")),
            )
        }
    };
    let token = app_state.csrf().token(&item.id);
    render_or_500(render::detail(&state::build_detail(&item, &context, token)))
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
    let (feed, _) = match load_board(&app_state).await {
        Ok(loaded) => loaded,
        Err(reason) => {
            let body = Json(serde_json::json!({ "error": reason }));
            return secure((StatusCode::INTERNAL_SERVER_ERROR, body).into_response());
        }
    };
    let limit = query.limit();
    let now = Utc::now();
    let built = tokio::task::spawn_blocking(move || {
        state::build_page(&state::ranked_active_cards(&feed, now), offset, limit)
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

// --- write path ---------------------------------------------------------------

/// Same-origin gate. Returns the reason when the request must be refused.
///
/// `Sec-Fetch-Site` is the modern signal and is sent by every current
/// browser; `Origin` is the fallback and is always present on a cross-origin
/// form POST. Either one disagreeing is fatal. Both absent is accepted only
/// because the CSRF token — which a cross-origin document cannot read — is
/// still required below.
fn same_origin_violation(headers: &HeaderMap) -> Option<String> {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if site != "same-origin" && site != "none" {
            return Some(format!(
                "the request declared Sec-Fetch-Site: {site}. This surface accepts same-origin \
                 writes only."
            ));
        }
    }
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())?;
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    // `Origin: http://127.0.0.1:7373` must match `Host: 127.0.0.1:7373`.
    let origin_authority = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    if origin_authority.eq_ignore_ascii_case(host) && !host.is_empty() {
        return None;
    }
    Some(format!(
        "the request came from origin {origin}, which is not this server. Cross-origin writes \
         are refused."
    ))
}

/// Read at most [`MAX_FORM_BYTES`]. A declared or actual overrun is a 413 and
/// the body is not parsed.
async fn bounded_body(headers: &HeaderMap, body: Body) -> Result<Bytes, Response> {
    if let Some(declared) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<usize>().ok())
    {
        if declared > MAX_FORM_BYTES {
            return Err(notice(
                StatusCode::PAYLOAD_TOO_LARGE,
                NoticeView::too_large(MAX_FORM_BYTES),
            ));
        }
    }
    axum::body::to_bytes(body, MAX_FORM_BYTES)
        .await
        .map_err(|_| {
            notice(
                StatusCode::PAYLOAD_TOO_LARGE,
                NoticeView::too_large(MAX_FORM_BYTES),
            )
        })
}

/// Pull one field out of an `application/x-www-form-urlencoded` body without
/// pulling in a parser: the forms have two short fields and the value set is
/// hex.
fn form_field(body: &str, name: &str) -> Option<String> {
    for pair in body.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            return Some(value.replace('+', " "));
        }
    }
    None
}

async fn resolve(
    state_: State<JournalState>,
    path: Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    write_verdict(state_, path, headers, body, JournalAction::Resolve).await
}

async fn dismiss(
    state_: State<JournalState>,
    path: Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    write_verdict(state_, path, headers, body, JournalAction::Dismiss).await
}

/// The single mutating handler both write routes share, so the four checks
/// cannot drift apart between them.
async fn write_verdict(
    State(app_state): State<JournalState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
    action: JournalAction,
) -> Response {
    // 1. Same-origin, before anything is read.
    if let Some(reason) = same_origin_violation(&headers) {
        return notice(StatusCode::FORBIDDEN, NoticeView::forbidden(&reason));
    }
    // 2. Bounded body.
    let bytes = match bounded_body(&headers, body).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    // 3. CSRF token, bound to this exact id.
    let form = String::from_utf8_lossy(&bytes);
    let presented = form_field(&form, "csrf").unwrap_or_default();
    if !app_state.csrf().verify(&id, &presented) {
        return notice(
            StatusCode::FORBIDDEN,
            NoticeView::forbidden(
                "the form did not carry a valid CSRF token for this dream. Reload the dream page \
                 and submit the form on it.",
            ),
        );
    }
    // 4. Target validated against stored rows — the write uses the row the
    //    feed returned, never the path parameter.
    let (items, _) = match load_items(&app_state).await {
        Ok(loaded) => loaded,
        Err(reason) => {
            return notice(
                StatusCode::INTERNAL_SERVER_ERROR,
                NoticeView::feed_error(&reason),
            )
        }
    };
    let Some(item) = state::find_item(&items, &id).cloned() else {
        return notice(StatusCode::NOT_FOUND, NoticeView::unknown_dream(&id));
    };

    let feed = app_state.feed();
    let written = tokio::task::spawn_blocking(move || feed.record_verdict(&item, action)).await;
    match written {
        Ok(Ok(receipt)) => notice(StatusCode::OK, NoticeView::recorded(&receipt)),
        Ok(Err(ResolveError::NoChunks { session })) => notice(
            StatusCode::CONFLICT,
            NoticeView::write_refused(&ResolveError::NoChunks { session }.to_string()),
        ),
        Ok(Err(error)) => notice(
            StatusCode::INTERNAL_SERVER_ERROR,
            NoticeView::write_refused(&error.to_string()),
        ),
        Err(error) => notice(
            StatusCode::INTERNAL_SERVER_ERROR,
            NoticeView::write_refused(&format!("the verdict task failed: {error}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::state::{
        sample_cluster, sample_item, BoardFeed, DreamFeed, StaticDreamFeed,
    };
    use crate::storage::dream_clusters::DreamClusterFeed;
    use crate::storage::dream_items::DreamItemGrade;
    use axum::http::Request;
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn items(n: usize) -> Vec<DreamItem> {
        (0..n)
            .map(|i| sample_item(&format!("id{i:02}"), "csr", &format!("open item {i}")))
            .collect()
    }

    fn board_feed(clusters: Vec<crate::storage::dream_clusters::DreamCluster>) -> BoardFeed {
        let projects: Vec<String> = clusters
            .iter()
            .map(|c| c.project.clone())
            .collect::<BTreeSet<String>>()
            .into_iter()
            .collect();
        BoardFeed {
            clusters: DreamClusterFeed {
                total_active: clusters.len(),
                active: clusters,
                settled: Vec::new(),
                archive: Vec::new(),
                total_settled: 0,
                total_archive: 0,
                projects,
            },
            open_items: Vec::new(),
            verified_plan_items: BTreeSet::new(),
        }
    }

    fn adverse_cluster(id: &str, project: &str) -> crate::storage::dream_clusters::DreamCluster {
        sample_cluster(
            id,
            project,
            DreamItemGrade::ItemGrade,
            "anchor_obsolete",
            Some("abcdef1234567890"),
            "2026-08-09T12:00:00Z",
            "todo",
            "2026-08-01T09:00:00Z",
            1,
        )
    }

    fn state_with(n: usize) -> JournalState {
        let clusters: Vec<_> = (0..n)
            .map(|i| adverse_cluster(&format!("c{i:02}"), "csr"))
            .collect();
        JournalState::new(Arc::new(
            StaticDreamFeed::new(items(n)).with_board(board_feed(clusters)),
        ))
    }

    struct BrokenFeed;
    impl DreamFeed for BrokenFeed {
        fn load(&self) -> anyhow::Result<Vec<DreamItem>> {
            Err(anyhow::anyhow!("database is locked"))
        }
        fn load_board(&self) -> anyhow::Result<BoardFeed> {
            Err(anyhow::anyhow!("database is locked"))
        }
    }

    async fn send(
        app_state: JournalState,
        request: Request<Body>,
    ) -> (StatusCode, Vec<(String, String)>, String) {
        let response = router(app_state)
            .oneshot(request)
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

    async fn get_path(
        app_state: JournalState,
        uri: &str,
    ) -> (StatusCode, Vec<(String, String)>, String) {
        send(
            app_state,
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    #[tokio::test]
    async fn the_home_route_is_the_week_dreams_page_not_the_log() {
        let (status, _, body) = get_path(state_with(3), "/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("No next move is dreamed yet"));
        assert!(body.contains("abstention"));
        assert!(body.contains("href=\"/board\""));
        assert!(
            !body.contains("id=\"observations\""),
            "the log board must not be the home"
        );
        assert!(
            !body.to_lowercase().contains("all clear"),
            "zero week-dreams must never read as an all-clear"
        );
        assert!(!body.contains("<script"), "home must work with JS off");
    }

    #[tokio::test]
    async fn the_landing_route_renders_the_cluster_board_not_a_legacy_item_list() {
        let (status, headers, body) = get_path(state_with(3), "/board").await;
        assert_eq!(status, StatusCode::OK);
        // All three evidence-maturity columns, with their gates stated.
        assert!(body.contains("Proposals"));
        assert!(body.contains("Observations"));
        assert!(body.contains("Outdated claims"));
        assert!(body.contains("item-grade binding"));
        // The off-board lane and the two ledger partitions are present.
        assert!(body.contains("Unexamined"));
        assert!(body.contains("Settled"));
        assert!(body.contains("Archive"));
        // Conclusions, not one card per item.
        assert!(body.contains("3 conclusions"));
        assert!(
            body.starts_with("<!doctype html>"),
            "document must be complete"
        );
        assert!(!body.contains("<script"), "the board must work with JS off");
        let content_type = headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert_eq!(content_type, "text/html; charset=utf-8");
    }

    /// Finding 2's ordering requirement, asserted on the LIVE page: the tier
    /// fixture is served in reverse and must come back in tier order, with
    /// the alphabetically-first project and the largest item count on the
    /// LAST card.
    #[tokio::test]
    async fn live_board_order_survives_shuffled_input_churn_and_project_names() {
        let mut clusters = vec![
            sample_cluster(
                "t4-oldest",
                "aaa",
                DreamItemGrade::SessionGrade,
                "anchor_reinstated",
                None,
                "2026-08-01T00:00:00Z",
                "todo",
                "2026-07-30T00:00:00Z",
                99,
            ),
            sample_cluster(
                "t3-blocker",
                "bbb",
                DreamItemGrade::SessionGrade,
                "anchor_reinstated",
                None,
                "2026-08-01T00:00:00Z",
                "blocker",
                "2026-07-31T00:00:00Z",
                50,
            ),
            sample_cluster(
                "t2-newer",
                "ccc",
                DreamItemGrade::SessionGrade,
                "anchor_reinstated",
                None,
                "2026-08-05T00:00:00Z",
                "todo",
                "2026-07-31T00:00:00Z",
                20,
            ),
            sample_cluster(
                "t1-item-grade",
                "zzz",
                DreamItemGrade::ItemGrade,
                "superseded_by",
                Some("beef000000000000"),
                "2026-08-05T00:00:00Z",
                "todo",
                "2026-07-31T00:00:00Z",
                1,
            ),
        ];
        clusters.reverse();
        let expected = ["t1-item-grade", "t2-newer", "t3-blocker", "t4-oldest"];

        let app_state = JournalState::new(Arc::new(
            StaticDreamFeed::new(Vec::new()).with_board(board_feed(clusters)),
        ));
        let (_, _, body) = get_path(app_state.clone(), "/api/dreams").await;
        let page: serde_json::Value = serde_json::from_str(&body).expect("json");
        let seen: Vec<String> = page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["id"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(seen, expected);

        // And the rendered page agrees. Cards are laid out per column, so the
        // tier order is asserted WITHIN the column the three session-grade
        // conclusions share; the item-grade one sits in Outdated claims.
        let (_, _, html) = get_path(app_state, "/board").await;
        let observations = &html[html
            .find(r#"id="observations""#)
            .expect("observations column")
            ..html
                .find(r#"id="outdated-claims""#)
                .expect("outdated column")];
        let mut last = 0usize;
        for id in ["t2-newer", "t3-blocker", "t4-oldest"] {
            let at = observations
                .find(&format!("dream-{id}"))
                .unwrap_or_else(|| panic!("card {id} missing from Observations"));
            assert!(at > last, "card {id} rendered out of tier order");
            last = at;
        }
        assert!(
            html[html
                .find(r#"id="outdated-claims""#)
                .expect("outdated column")..]
                .contains("dream-t1-item-grade"),
            "the item-grade adverse conclusion belongs in Outdated claims"
        );
        // Rank numerals restart per column and are the tier positions.
        assert!(observations.contains(">01<"));
    }

    #[tokio::test]
    async fn an_unexamined_item_never_appears_in_a_column() {
        use crate::storage::dream_clusters::OpenItem;

        let mut feed = board_feed(vec![adverse_cluster("c00", "csr")]);
        feed.open_items = vec![OpenItem {
            id: "bare".into(),
            project: "csr".into(),
            item: "never examined todo".into(),
            kind: "todo".into(),
            origin_session: "sess-9".into(),
            origin_ts: "2026-08-01T09:00:00Z".into(),
            origin_date: "2026-08-01".into(),
            completed: None,
            examined: false,
        }];
        let app_state =
            JournalState::new(Arc::new(StaticDreamFeed::new(Vec::new()).with_board(feed)));
        let (_, _, body) = get_path(app_state, "/board").await;

        let lane = body
            .find("id=\"unexamined\"")
            .expect("unexamined lane must render");
        let item = body
            .find("never examined todo")
            .expect("the unexamined item must be shown, not hidden");
        assert!(item > lane, "the item must sit inside the off-board lane");
        assert!(body.contains("concluded NOTHING about these"));
    }

    #[tokio::test]
    async fn board_on_an_empty_feed_is_honest() {
        let app_state = JournalState::new(Arc::new(
            StaticDreamFeed::new(Vec::new()).with_board(BoardFeed::default()),
        ));
        let (status, _, body) = get_path(app_state, "/board").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Nothing on record yet."));
        assert!(body.contains("This is an empty feed, not a failure"));
        assert!(
            !body.to_lowercase().contains("all clear"),
            "zero conclusions must never read as an all-clear"
        );
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
        assert!(
            csp.contains("form-action 'self'"),
            "the write forms need exactly this grant and no more"
        );
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

    /// Codex X4 finding 9, both halves, through the live route.
    #[tokio::test]
    async fn a_failing_detail_read_degrades_instead_of_claiming_absence() {
        let item = sample_item("id00", "csr", "open item 0");
        let app_state = JournalState::new(Arc::new(
            StaticDreamFeed::new(vec![item]).with_detail_error("the detail read failed hard"),
        ));
        let (status, _, body) = get_path(app_state, "/dream/id00").await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("the detail read failed hard"));
        assert!(
            body.contains("Nothing below is rendered from this failed read"),
            "the page must say the sections are missing because of the error"
        );
        for absence in [
            "The brief is empty because the",
            "No churn was counted",
            "AST comparison abstained",
        ] {
            assert!(
                !body.contains(absence),
                "a failed read must not render {absence:?} as evidence of absence"
            );
        }
    }

    #[tokio::test]
    async fn a_successful_empty_detail_read_renders_the_absence_states() {
        let item = sample_item("id00", "csr", "open item 0");
        let app_state = JournalState::new(Arc::new(StaticDreamFeed::new(vec![item])));
        let (status, _, body) = get_path(app_state, "/dream/id00").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("The brief is empty because the"));
        assert!(body.contains("No churn was counted"));
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
        assert_eq!(first["items"][0]["id"], "c00");

        let (_, _, body) = get_path(app_state.clone(), "/api/dreams?cursor=2&limit=2").await;
        let second: serde_json::Value = serde_json::from_str(&body).expect("json");
        assert_eq!(second["items"][0]["id"], "c02");
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

    /// The links the server-rendered board emits must actually resolve
    /// through the router with JavaScript off — that is the whole point of
    /// rendering the page complete in the DOM.
    #[tokio::test]
    async fn board_links_resolve_when_followed() {
        // One cluster whose lead item id matches a real detail row.
        let mut cluster = adverse_cluster("c00", "csr");
        cluster.items[0].id = "id00".to_string();
        let app_state = JournalState::new(Arc::new(
            StaticDreamFeed::new(items(1)).with_board(board_feed(vec![cluster])),
        ));

        let (_, _, body) = get_path(app_state.clone(), "/board").await;
        let card = first_href_after(&body, "class=\"card-conclusion\"");
        assert_eq!(card, "/dream/id00");
        let (status, _, detail_body) = get_path(app_state.clone(), &card).await;
        assert_eq!(status, StatusCode::OK);
        assert!(detail_body.contains("open item 0"));

        // The micro copy icon is the card's only action, and it points at the
        // detail page's copy block.
        let copy = first_href_after(&body, "class=\"card-copy\"");
        assert_eq!(copy, "/dream/id00#copy");
    }

    #[tokio::test]
    async fn a_failing_feed_reports_the_reason_instead_of_an_empty_page() {
        let app_state = JournalState::new(Arc::new(BrokenFeed));
        let (status, _, body) = get_path(app_state.clone(), "/board").await;
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

    // --- write surface: positive path ---------------------------------------

    fn writable_state() -> (JournalState, Arc<StaticDreamFeed>) {
        let feed = Arc::new(StaticDreamFeed::new(items(2)).writable());
        (JournalState::new(feed.clone()), feed)
    }

    fn post(uri: &str, token: &str) -> axum::http::request::Builder {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7373")
            .header(header::ORIGIN, "http://127.0.0.1:7373")
            .header("sec-fetch-site", "same-origin")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header("x-journal-test-token", token)
    }

    fn form(token: &str) -> Body {
        Body::from(format!("csrf={token}"))
    }

    #[tokio::test]
    async fn resolve_and_dismiss_write_through_tagged_journal_ui() {
        for (action, status_written) in [("resolve", "resolved"), ("dismiss", "still_open")] {
            let (app_state, feed) = writable_state();
            let token = app_state.csrf().token("id00");
            let (status, _, body) = send(
                app_state,
                post(&format!("/dream/id00/{action}"), &token)
                    .body(form(&token))
                    .expect("request"),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{action} must be accepted");
            assert!(body.contains("Verdict recorded"));
            assert!(body.contains("journal_ui"));

            let writes = feed.writes();
            assert_eq!(writes.len(), 1, "{action} must write exactly once");
            assert_eq!(writes[0].action, action);
            assert_eq!(writes[0].status, status_written);
            assert_eq!(writes[0].origin, "journal_ui");
            assert_eq!(writes[0].item_id, "id00");
        }
    }

    /// The token rendered on the detail page is the token the form accepts —
    /// asserted by pulling it out of the page and posting it back.
    #[tokio::test]
    async fn the_token_rendered_on_the_page_is_the_one_the_write_accepts() {
        let (app_state, feed) = writable_state();
        let (_, _, page) = get_path(app_state.clone(), "/dream/id00").await;
        let marker = "name=\"csrf\" value=\"";
        let start = page.find(marker).expect("csrf field on the detail page") + marker.len();
        let end = page[start..].find('"').expect("end of value") + start;
        let token = page[start..end].to_string();
        assert_eq!(token.len(), 64, "sha256 hex");

        let (status, _, _) = send(
            app_state,
            post("/dream/id00/resolve", &token)
                .body(form(&token))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(feed.writes().len(), 1);
    }

    // --- write surface: negative security tests ------------------------------

    #[tokio::test]
    async fn a_cross_origin_write_is_refused_and_writes_nothing() {
        for (name, value) in [
            (header::ORIGIN.as_str(), "http://evil.example"),
            ("sec-fetch-site", "cross-site"),
        ] {
            let (app_state, feed) = writable_state();
            let token = app_state.csrf().token("id00");
            let request = Request::builder()
                .method("POST")
                .uri("/dream/id00/resolve")
                .header(header::HOST, "127.0.0.1:7373")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(name, value)
                .body(form(&token))
                .expect("request");
            let (status, _, body) = send(app_state, request).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{name}: {value}");
            assert!(body.contains("refused") || body.contains("same-origin"));
            assert!(
                feed.writes().is_empty(),
                "{name}: a refused write must write nothing"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_or_wrong_csrf_token_is_refused_and_writes_nothing() {
        for body_text in [
            "".to_string(),
            "csrf=".to_string(),
            format!("csrf={}", "0".repeat(64)),
            "note=hello".to_string(),
        ] {
            let (app_state, feed) = writable_state();
            let (status, _, page) = send(
                app_state,
                post("/dream/id00/resolve", "")
                    .body(Body::from(body_text.clone()))
                    .expect("request"),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "body {body_text:?}");
            assert!(page.contains("CSRF token"));
            assert!(feed.writes().is_empty());
        }
    }

    /// A valid token for one dream must not authorise a write on another.
    #[tokio::test]
    async fn a_token_minted_for_another_dream_is_refused() {
        let (app_state, feed) = writable_state();
        let other = app_state.csrf().token("id01");
        let (status, _, _) = send(
            app_state,
            post("/dream/id00/resolve", &other)
                .body(form(&other))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(feed.writes().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_id_is_a_404_and_writes_nothing() {
        let (app_state, feed) = writable_state();
        let token = app_state.csrf().token("does-not-exist");
        let (status, _, body) = send(
            app_state,
            post("/dream/does-not-exist/resolve", &token)
                .body(form(&token))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body.contains("No dream with that id"));
        assert!(feed.writes().is_empty());
    }

    #[tokio::test]
    async fn an_oversized_body_is_413_and_is_never_parsed() {
        let (app_state, feed) = writable_state();
        let token = app_state.csrf().token("id00");
        let padding = "x".repeat(MAX_FORM_BYTES * 4);
        let (status, _, body) = send(
            app_state,
            post("/dream/id00/resolve", &token)
                .body(Body::from(format!("csrf={token}&pad={padding}")))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(body.contains("1024 bytes"));
        assert!(feed.writes().is_empty());
    }

    /// An understated `Content-Length` must not get past the streaming bound
    /// either — the declared length is a hint, the read is what is enforced.
    #[tokio::test]
    async fn a_lying_content_length_does_not_defeat_the_body_bound() {
        let (app_state, feed) = writable_state();
        let token = app_state.csrf().token("id00");
        let padding = "x".repeat(MAX_FORM_BYTES * 4);
        let (status, _, _) = send(
            app_state,
            post("/dream/id00/resolve", &token)
                .header(header::CONTENT_LENGTH, "12")
                .body(Body::from(format!("csrf={token}&pad={padding}")))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(feed.writes().is_empty());
    }

    /// A read-only feed must refuse rather than report a write it did not do.
    #[tokio::test]
    async fn a_read_only_feed_reports_the_refusal_instead_of_a_receipt() {
        let app_state = state_with(2);
        let token = app_state.csrf().token("id00");
        let (status, _, body) = send(
            app_state,
            post("/dream/id00/resolve", &token)
                .body(form(&token))
                .expect("request"),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.contains("records no verdicts"));
        assert!(!body.contains("Verdict recorded"));
    }

    /// The whole mutating surface, enumerated. Only the two write routes may
    /// accept a non-GET method; everything else must refuse.
    #[tokio::test]
    async fn no_other_mutating_route_exists() {
        const PATHS: [&str; 7] = [
            "/",
            "/dream/id00",
            "/dream/id00/",
            "/api/dreams",
            "/healthz",
            "/admin",
            "/dream/id00/delete",
        ];
        for path in PATHS {
            for method in ["POST", "PUT", "PATCH", "DELETE"] {
                let (app_state, feed) = writable_state();
                let token = app_state.csrf().token("id00");
                let request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::HOST, "127.0.0.1:7373")
                    .header(header::ORIGIN, "http://127.0.0.1:7373")
                    .body(form(&token))
                    .expect("request");
                let (status, _, _) = send(app_state, request).await;
                assert!(
                    status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
                    "{method} {path} answered {status}: no third mutating route may exist"
                );
                assert!(feed.writes().is_empty(), "{method} {path} wrote something");
            }
        }
        // And the two real write routes reject GET, so nothing mutates from a
        // link, a prefetch, or a bookmark.
        for path in ["/dream/id00/resolve", "/dream/id00/dismiss"] {
            let (app_state, feed) = writable_state();
            let (status, _, _) = get_path(app_state, path).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "GET {path}");
            assert!(feed.writes().is_empty());
        }
    }
}
