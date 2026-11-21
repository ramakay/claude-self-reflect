//! Host-header gate: the journal's DNS-rebinding defence.
//!
//! Binding 127.0.0.1 keeps other machines out. It does not keep a web page
//! out. A page on `evil.example` can re-point its own DNS name at 127.0.0.1
//! after the browser has loaded it, and from then on the browser treats
//! `http://evil.example:7373/` as same-origin with the page that loaded it.
//! The page can read the journal, pull the CSRF token out of it, and post a
//! verdict. The `Origin == Host` write check cannot stop that, because both
//! carry the attacker's name.
//!
//! The one thing the attacker cannot change is the name the browser puts in
//! the `Host` header: it is the attacker's own domain. So every request, GET
//! included, must carry a `Host` that names this server's own loopback
//! address and the port it actually bound:
//!
//! * `127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`, compared
//!   case-insensitively and exactly (no suffix match, no userinfo, no
//!   trailing dot);
//! * the bare names only when the bound port is 80, the one port at which
//!   an HTTP client omits it.
//!
//! A refused request gets **421 Misdirected Request**: the request was
//! aimed at an authority this server does not answer for. 421 also keeps the
//! refusal distinct from the 403 the CSRF and same-origin gates return, so a
//! log line tells the two apart. The body names no request data and carries
//! no token.
//!
//! The port is the one the listener really bound, read by [`super::serve`]
//! from the listener itself, so the ephemeral-port fallback of
//! [`super::bind_loopback`] stays correct.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;

use super::routes;

/// The exact `Host` values this server answers to.
#[derive(Debug)]
pub struct AllowedHosts {
    port: u16,
    exact: Vec<String>,
}

impl AllowedHosts {
    pub fn for_port(port: u16) -> Self {
        let mut exact = vec![
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ];
        if port == 80 {
            exact.extend(["127.0.0.1", "localhost", "[::1]"].map(String::from));
        }
        Self { port, exact }
    }

    /// Whether `host` (a `Host` header value or a URI authority) is one of
    /// this server's own names. ASCII case-insensitive, otherwise exact.
    pub fn permits(&self, host: &str) -> bool {
        self.exact.iter().any(|ok| ok.eq_ignore_ascii_case(host))
    }

    /// Whether the whole request names this server and only this server.
    ///
    /// Exactly one `Host` header, or an absolute-form request URI (HTTP/2
    /// `:authority`, or `GET http://… HTTP/1.1`) with no `Host`. When both
    /// are present both must pass: RFC 9112 makes the URI authority win over
    /// `Host`, and honouring only one of them would let the other smuggle a
    /// foreign name through.
    fn permits_request(&self, request: &Request) -> bool {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        let host = hosts.next();
        if hosts.next().is_some() {
            return false;
        }
        let authority = request.uri().authority().map(|a| a.as_str());
        let host_ok = match host {
            Some(value) => value.to_str().is_ok_and(|v| self.permits(v)),
            None => authority.is_some(),
        };
        host_ok && authority.is_none_or(|a| self.permits(a))
    }
}

/// Wrap `router` so every route, the fallback included, answers only a
/// request addressed to `127.0.0.1`/`localhost`/`[::1]` on `port`.
pub fn guard(router: Router, port: u16) -> Router {
    let allowed = Arc::new(AllowedHosts::for_port(port));
    router.layer(middleware::from_fn_with_state(allowed, check))
}

async fn check(State(allowed): State<Arc<AllowedHosts>>, request: Request, next: Next) -> Response {
    if allowed.permits_request(&request) {
        return next.run(request).await;
    }
    routes::html(
        StatusCode::MISDIRECTED_REQUEST,
        format!(
            "<!doctype html><title>421 Misdirected Request</title>\
             <p>421 Misdirected Request. This server answers only at \
             http://127.0.0.1:{port}/ (or localhost:{port}).</p>\n",
            port = allowed.port
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::state::{sample_item, JournalState, StaticDreamFeed};
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;

    const PORT: u16 = 7373;

    fn writable() -> (JournalState, Arc<StaticDreamFeed>) {
        let items = (0..2)
            .map(|i| sample_item(&format!("id{i:02}"), "csr", &format!("open item {i}")))
            .collect();
        let feed = Arc::new(StaticDreamFeed::new(items).writable());
        (JournalState::new(feed.clone()), feed)
    }

    fn guarded(app_state: JournalState, port: u16) -> Router {
        guard(routes::router(app_state), port)
    }

    async fn call(
        router: Router,
        request: HttpRequest<Body>,
    ) -> (StatusCode, Vec<(String, String)>, String) {
        let response = router.oneshot(request).await.expect("router response");
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

    fn get(uri: &str, host: &str) -> HttpRequest<Body> {
        HttpRequest::builder()
            .uri(uri)
            .header(header::HOST, host)
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn a_get_with_a_foreign_host_is_refused_on_every_route() {
        let foreign = [
            "evil.example.com:7373",
            "evil.example.com",
            "127.0.0.1.evil.example:7373",
            "localhost.evil.example:7373",
            "evil.example:7373@127.0.0.1:7373",
            "127.0.0.1:7374",
            "127.0.0.1",
            "10.0.0.5:7373",
            "",
        ];
        for host in foreign {
            for uri in [
                "/",
                "/board",
                "/corrections",
                "/dream/id00",
                "/api/dreams",
                "/healthz",
                "/no-such-page",
            ] {
                let (app_state, _) = writable();
                let token = app_state.csrf().token("id00");
                let (status, headers, body) = call(guarded(app_state, PORT), get(uri, host)).await;
                assert_eq!(
                    status,
                    StatusCode::MISDIRECTED_REQUEST,
                    "GET {uri} with Host {host:?} must be refused"
                );
                assert!(
                    !body.contains(&token) && !body.contains("csrf"),
                    "{uri}: a refusal must not carry the form token"
                );
                assert!(
                    !headers
                        .iter()
                        .any(|(k, _)| k.starts_with("access-control-")),
                    "{uri}: a refusal must not set a CORS header"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_get_with_a_loopback_host_still_works() {
        for host in [
            "127.0.0.1:7373",
            "localhost:7373",
            "[::1]:7373",
            "LOCALHOST:7373",
            "Localhost:7373",
        ] {
            let (app_state, _) = writable();
            let (status, _, body) =
                call(guarded(app_state.clone(), PORT), get("/dream/id00", host)).await;
            assert_eq!(status, StatusCode::OK, "Host {host}");
            assert!(body.contains("name=\"csrf\""), "Host {host}: page served");

            let (status, _, body) =
                call(guarded(app_state.clone(), PORT), get("/healthz", host)).await;
            assert_eq!(status, StatusCode::OK, "Host {host}");
            assert_eq!(body, "ok\n");

            // A loopback Host reaches the fallback too, so a typo is a 404
            // and not a 421.
            let (status, _, _) = call(guarded(app_state, PORT), get("/no-such-page", host)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "Host {host}");
        }
    }

    #[tokio::test]
    async fn a_post_with_a_valid_token_but_a_foreign_host_is_refused() {
        // The rebinding shape: Origin and Host agree with each other, both
        // carry the attacker's name, and the token is the real one.
        for (host, origin) in [
            ("evil.example.com:7373", "http://evil.example.com:7373"),
            ("evil.example.com", "http://evil.example.com"),
        ] {
            for action in ["resolve", "dismiss"] {
                let (app_state, feed) = writable();
                let token = app_state.csrf().token("id00");
                let request = HttpRequest::builder()
                    .method("POST")
                    .uri(format!("/dream/id00/{action}"))
                    .header(header::HOST, host)
                    .header(header::ORIGIN, origin)
                    .header("sec-fetch-site", "same-origin")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf={token}")))
                    .expect("request");
                let (status, _, body) = call(guarded(app_state, PORT), request).await;
                assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{action} {host}");
                assert!(!body.contains("Verdict recorded"));
                assert!(
                    feed.writes().is_empty(),
                    "{action} {host}: a refused write must write nothing"
                );
            }
        }

        // Control: the same request addressed to the real host is accepted.
        let (app_state, feed) = writable();
        let token = app_state.csrf().token("id00");
        let request = HttpRequest::builder()
            .method("POST")
            .uri("/dream/id00/resolve")
            .header(header::HOST, "localhost:7373")
            .header(header::ORIGIN, "http://localhost:7373")
            .header("sec-fetch-site", "same-origin")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("csrf={token}")))
            .expect("request");
        let (status, _, _) = call(guarded(app_state, PORT), request).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(feed.writes().len(), 1);
    }

    #[tokio::test]
    async fn a_request_without_a_host_or_with_two_is_refused() {
        let (app_state, _) = writable();
        let none = HttpRequest::builder()
            .uri("/healthz")
            .body(Body::empty())
            .expect("request");
        let (status, _, _) = call(guarded(app_state.clone(), PORT), none).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "no Host at all");

        // One good name does not excuse a second, foreign one.
        let two = HttpRequest::builder()
            .uri("/healthz")
            .header(header::HOST, "127.0.0.1:7373")
            .header(header::HOST, "evil.example.com:7373")
            .body(Body::empty())
            .expect("request");
        let (status, _, _) = call(guarded(app_state, PORT), two).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "two Host headers");
    }

    #[tokio::test]
    async fn an_absolute_form_uri_must_agree_with_host() {
        let (app_state, _) = writable();
        let smuggled = get("http://evil.example.com:7373/healthz", "127.0.0.1:7373");
        let (status, _, _) = call(guarded(app_state.clone(), PORT), smuggled).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);

        let agreeing = get("http://127.0.0.1:7373/healthz", "127.0.0.1:7373");
        let (status, _, _) = call(guarded(app_state.clone(), PORT), agreeing).await;
        assert_eq!(status, StatusCode::OK);

        // HTTP/2 style: the authority is in the URI and there is no Host.
        let h2 = HttpRequest::builder()
            .uri("http://localhost:7373/healthz")
            .body(Body::empty())
            .expect("request");
        let (status, _, _) = call(guarded(app_state, PORT), h2).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[test]
    fn bare_loopback_names_are_accepted_only_at_port_80() {
        let high = AllowedHosts::for_port(7373);
        assert!(!high.permits("localhost"));
        assert!(!high.permits("127.0.0.1"));
        assert!(!high.permits("localhost:80"));
        assert!(high.permits("LocalHost:7373"));
        let http = AllowedHosts::for_port(80);
        assert!(http.permits("localhost"));
        assert!(http.permits("127.0.0.1:80"));
        assert!(!http.permits("evil.example"));
    }

    /// The wiring, over a real socket: `serve` applies the guard itself, for
    /// the port the listener actually bound (here an ephemeral one), so
    /// neither host (`journal serve`, the daemon) can serve without it.
    #[tokio::test]
    async fn serve_applies_the_guard_for_the_port_it_bound() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = crate::journal::bind_loopback(0).await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let (app_state, _) = writable();
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(crate::journal::serve(
            routes::router(app_state),
            listener,
            shutdown.clone(),
        ));

        async fn status_line(port: u16, host: &str) -> String {
            let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("connect");
            let request =
                format!("GET /healthz HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).await.expect("write");
            let mut raw = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                stream.read_to_end(&mut raw),
            )
            .await
            .expect("response within 5s")
            .expect("read");
            String::from_utf8_lossy(&raw)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        }

        let foreign = status_line(port, &format!("evil.example.com:{port}")).await;
        assert!(foreign.contains(" 421"), "foreign host: {foreign}");
        let wrong_port = status_line(port, &format!("127.0.0.1:{}", port.wrapping_add(1))).await;
        assert!(wrong_port.contains(" 421"), "wrong port: {wrong_port}");
        let numeric = status_line(port, &format!("127.0.0.1:{port}")).await;
        assert!(numeric.contains(" 200"), "127.0.0.1: {numeric}");
        let named = status_line(port, &format!("localhost:{port}")).await;
        assert!(named.contains(" 200"), "localhost: {named}");

        shutdown.store(true, Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("server stops within 5s")
            .expect("server task")
            .expect("serve result");
    }
}
