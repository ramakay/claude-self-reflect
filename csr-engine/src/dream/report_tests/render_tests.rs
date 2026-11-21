use super::*;

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

/// Journal v2 Phase 6 (dark mode): the report must ship a dark variant
/// of the SAME token set reachable through both the OS-level
/// `prefers-color-scheme: dark` media query and the client-side
/// `[data-theme="dark"]` attribute override — one block covers a
/// dark-OS visitor who never touches the toggle, the other covers the
/// toggle winning regardless of OS preference. Both must define every
/// token that changes for dark mode, or the two forms would silently
/// drift out of sync.
#[test]
fn dark_tokens_present_in_both_media_query_and_attribute_forms() {
    let storage = Storage::open_memory().unwrap();
    seed_rich_sessions(&storage, 1);
    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );

    let media_start = html
        .find(r#"@media (prefers-color-scheme: dark)"#)
        .expect("prefers-color-scheme: dark media query must be present");
    let media_end = html[media_start..]
        .find(r#":root[data-theme="dark"]"#)
        .map(|offset| media_start + offset)
        .expect(r#"[data-theme="dark"] block must follow the media query block"#);
    let media_block = &html[media_start..media_end];
    assert!(
        media_block.contains(r#":not([data-theme="light"])"#),
        "the media-query block must be guarded so an explicit light toggle can still win"
    );

    let attr_start = media_end;
    let attr_end = html[attr_start..]
        .find("* { box-sizing: border-box; }")
        .map(|offset| attr_start + offset)
        .expect("attribute block must be followed by the base stylesheet");
    let attr_block = &html[attr_start..attr_end];

    let dark_tokens = [
        "--sky-top: #1a1a2e",
        "--sky-bottom: #26263e",
        "--ink: #e8e6f0",
        "--slate: #9a98b5",
        "--label: #9694ae",
        "--purple: #9d8cc4",
        "--rose: #d9a3ac",
        "--sage: #a8c29c",
        "--amber-cream: #e0b988",
        "--glass: rgba(30, 30, 46, 0.55)",
        "--glass-border: rgba(255, 255, 255, 0.12)",
        "--bg: rgba(30, 30, 46, 0.35)",
        "--red: #f0a9b2",
        "--amber: #f0c58c",
        "--green: #a9dd98",
    ];
    for token in dark_tokens {
        assert!(
            media_block.contains(token),
            "prefers-color-scheme block missing dark token {token:?}"
        );
        assert!(
            attr_block.contains(token),
            "[data-theme=\"dark\"] block missing dark token {token:?}"
        );
    }
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
            // Version-stamped: measured by the current filter chain, so
            // the total beyond the persisted-quote cap is trustworthy
            // and "5" may render. An unstamped episode with this same
            // payload shows the surviving-quote count instead — pinned
            // by `legacy_untrusted_steer_total_never_renders`.
            "instrumentation_version":
                crate::transcript::instrumentation::STEER_FILTER_VERSION,
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
        pane.matches("class=\"steer-line post-it\"").count(),
        3,
        "at most 3 steer lines must render regardless of steer_count: {pane}"
    );
}

/// F2 regression: a persisted episode whose stored steers mix a CSR
/// self-emission (`"CSR CONTINUUM [...]"`, caught only by
/// `is_csr_emission`/`extractable`, not by `is_noisy_steer_text`), a
/// harness task-notification (caught by `is_noisy_steer_text`), and one
/// genuine human steer — the render path must re-filter with the FULL
/// chain and show exactly the one surviving quote with a count of 1, not
/// the stale stored count of 3.
#[test]
fn steer_render_reapplies_full_provenance_chain_not_just_noise_filter() {
    let storage = Storage::open_memory().unwrap();
    seed_episode_with_extra(
        &storage,
        "mixed-steer-session",
        "2026-03-05T01:00:00Z",
        "wire the feature end to end",
        "done",
        serde_json::json!({
            "error_count": 0,
            "top_errors": [],
            "steer_count": 3,
            "steers": [
                {"turn": 2, "text": "CSR CONTINUUM [2m ago]: stale context echo"},
                {"turn": 4, "text": "<task-notification>queued job finished</task-notification>"},
                {"turn": 6, "text": "no, use the october script instead"},
            ],
        }),
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (_, detail_html) = split_panes(&html);
    let short = short_oid("mixed-steer-session");
    let pane_start = detail_html
        .find(&format!(r#"data-session="{short}""#))
        .unwrap();
    let pane_end = detail_html[pane_start..]
        .find("</article>")
        .map(|offset| pane_start + offset)
        .unwrap();
    let pane = &detail_html[pane_start..pane_end];

    assert!(
        pane.contains("1 mid-flight steer") && !pane.contains("1 mid-flight steers"),
        "the recomputed, surviving-only count must be 1 (singular), not the stale stored 3: {pane}"
    );
    assert_eq!(
        pane.matches("class=\"steer-line post-it\"").count(),
        1,
        "only the one genuine steer may render as a quote: {pane}"
    );
    assert!(
        pane.contains("october script"),
        "the surviving genuine steer's text must still render: {pane}"
    );
    assert!(
        !pane.contains("CSR CONTINUUM") && !pane.contains("task-notification"),
        "neither the CSR self-emission nor the harness notification may render as a steer: {pane}"
    );
}

/// F2 regression: when EVERY persisted steer is noise (by any of the
/// three provenance guards), the STEER card renders no count line and no
/// steer quotes at all — never "N mid-flight steers" with an empty list
/// underneath it.
#[test]
fn steer_render_shows_no_line_when_all_persisted_steers_are_noise() {
    let storage = Storage::open_memory().unwrap();
    seed_episode_with_extra(
        &storage,
        "all-noise-steer-session",
        "2026-03-06T01:00:00Z",
        "clean up the queue",
        "done",
        serde_json::json!({
            "error_count": 0,
            "top_errors": [],
            "steer_count": 1,
            "steers": [
                {"turn": 2, "text": "CSR CONTINUUM [5m ago]: stale context echo"},
            ],
        }),
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (_, detail_html) = split_panes(&html);
    let short = short_oid("all-noise-steer-session");
    let pane_start = detail_html
        .find(&format!(r#"data-session="{short}""#))
        .unwrap();
    let pane_end = detail_html[pane_start..]
        .find("</article>")
        .map(|offset| pane_start + offset)
        .unwrap();
    let pane = &detail_html[pane_start..pane_end];

    assert!(
        !pane.contains("mid-flight steer"),
        "an all-noise stored steer list must render no count line at all: {pane}"
    );
    assert!(
        !pane.contains("class=\"steer-line post-it\""),
        "an all-noise stored steer list must render no steer quotes: {pane}"
    );
    assert!(
        !pane.contains("CSR CONTINUUM"),
        "the filtered CSR emission must never leak into the rendered pane: {pane}"
    );
}

/// Delta-review F2: a legacy episode (no `instrumentation_version` stamp)
/// whose raw total exceeds its persisted quotes cannot have that total
/// repaired by subtracting provable noise — `steer_count=5` with three
/// stored all-noise quotes used to render "2 mid-flight steers" over an
/// empty quote list. An untrusted total never renders: all-noise shows no
/// steer line, and a mixed list shows exactly the surviving-quote count.
#[test]
fn legacy_untrusted_steer_total_never_renders() {
    let storage = Storage::open_memory().unwrap();
    seed_episode_with_extra(
        &storage,
        "legacy-noise-total",
        "2026-03-07T01:00:00Z",
        "pull the overnight data",
        "done",
        serde_json::json!({
            "error_count": 0,
            "top_errors": [],
            "steer_count": 5,
            "steers": [
                {"turn": 2, "text": "<task-notification><task-id>x</task-id></task-notification>"},
                {"turn": 4, "text": "[SYSTEM NOTIFICATION - NOT USER INPUT] scheduled event"},
                {"turn": 6, "text": "CSR CONTINUUM [2h ago]: stale echo"},
            ],
        }),
    );
    seed_episode_with_extra(
        &storage,
        "legacy-mixed-total",
        "2026-03-07T02:00:00Z",
        "rerender the promo",
        "done",
        serde_json::json!({
            "error_count": 0,
            "top_errors": [],
            "steer_count": 5,
            "steers": [
                {"turn": 2, "text": "no — hindi, not english"},
                {"turn": 4, "text": "[SYSTEM NOTIFICATION - NOT USER INPUT] scheduled event"},
                {"turn": 6, "text": "stop, show me the profile"},
            ],
        }),
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (_, detail_html) = split_panes(&html);

    let noise_short = short_oid("legacy-noise-total");
    let noise_start = detail_html
        .find(&format!(r#"data-session="{noise_short}""#))
        .unwrap();
    let noise_end = noise_start + detail_html[noise_start..].find("</article>").unwrap();
    let noise_pane = &detail_html[noise_start..noise_end];
    assert!(
        !noise_pane.contains("mid-flight steer"),
        "an untrusted total over an all-noise quote list must render no steer line: {noise_pane}"
    );

    let mixed_short = short_oid("legacy-mixed-total");
    let mixed_start = detail_html
        .find(&format!(r#"data-session="{mixed_short}""#))
        .unwrap();
    let mixed_end = mixed_start + detail_html[mixed_start..].find("</article>").unwrap();
    let mixed_pane = &detail_html[mixed_start..mixed_end];
    assert!(
        mixed_pane.contains("2 mid-flight steers"),
        "an untrusted total renders the surviving-quote count, never the raw 5: {mixed_pane}"
    );
    assert_eq!(
        mixed_pane.matches("class=\"steer-line post-it\"").count(),
        2,
        "exactly the two genuine quotes survive: {mixed_pane}"
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

/// F4 regression: a `session_instrumentation` cache row whose
/// `(transcript_size, transcript_mtime)` still matches the transcript on
/// disk must be reused with ZERO rescans — the pre-existing cost
/// property the backfill relies on to stay cheap. Proven by caching a
/// deliberately wrong error count against the file's REAL stat: if the
/// backfill ever re-parsed the transcript, the wrong count would be
/// overwritten with the real one (0), so seeing the wrong count survive
/// is direct evidence no rescan happened.
#[test]
fn backfill_reuses_fresh_cache_row_with_zero_rescans() {
    let storage = Storage::open_memory().unwrap();
    let projects_dir = tempfile::tempdir().unwrap();
    let project_dir = projects_dir.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    let session_id = "fresh-cache-sess";
    let transcript_path = project_dir.join(format!("{session_id}.jsonl"));
    std::fs::write(
        &transcript_path,
        r#"{"type":"user","message":{"content":"do a small task"}}"#,
    )
    .unwrap();
    let metadata = std::fs::metadata(&transcript_path).unwrap();
    let mtime = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Episode carries NO instrumentation fields -> resolution falls to
    // the cache row below, tagging this session `InstrumentationSource::
    // Cache` (not `Episode`).
    seed_episode(
        &storage,
        session_id,
        "2026-03-07T01:00:00Z",
        "do a small task",
        "done",
        &[],
        &[],
    );
    storage
        .with_connection(|conn| {
            crate::storage::dream_report::write_instrumentation_cache(
                conn,
                session_id,
                metadata.len(),
                mtime,
                &SessionInstrumentation {
                    error_count: 77,
                    top_errors: vec![],
                    steer_count: 0,
                    steers: vec![],
                    turn_count: 1,
                },
            )
        })
        .unwrap();

    let html = render_with_projects_dir(
        &storage,
        projects_dir.path(),
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (index_html, _) = split_panes(&html);
    let short = short_oid(session_id);
    let row_start = index_html
        .find(&format!(r#"data-session="{short}""#))
        .unwrap();
    let row_end = index_html[row_start..]
        .find("</button>")
        .map(|offset| row_start + offset)
        .unwrap();
    let row = &index_html[row_start..row_end];
    assert!(
        row.contains("77 errors"),
        "a stat-matched cache row must be reused verbatim, not rescanned: {row}"
    );
}

/// F4 regression: a `session_instrumentation` cache row whose stat no
/// longer matches the transcript on disk (it grew) must be discarded and
/// rescanned — never left rendering forever-stale numbers. The
/// transcript is rewritten with real `is_error` tool_result blocks after
/// the stale row is cached, guaranteeing the file size (not relying on
/// mtime, which can share the same wall-clock second in a fast test run)
/// differs from what the cache row was keyed to.
#[test]
fn backfill_rescans_stale_cache_row_when_transcript_grows() {
    let storage = Storage::open_memory().unwrap();
    let projects_dir = tempfile::tempdir().unwrap();
    let project_dir = projects_dir.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    let session_id = "stale-cache-sess";
    let transcript_path = project_dir.join(format!("{session_id}.jsonl"));
    std::fs::write(
        &transcript_path,
        r#"{"type":"user","message":{"content":"do a small task"}}"#,
    )
    .unwrap();
    let stale_metadata = std::fs::metadata(&transcript_path).unwrap();
    let stale_mtime = stale_metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    seed_episode(
        &storage,
        session_id,
        "2026-03-08T01:00:00Z",
        "do a small task",
        "done",
        &[],
        &[],
    );
    storage
        .with_connection(|conn| {
            crate::storage::dream_report::write_instrumentation_cache(
                conn,
                session_id,
                stale_metadata.len(),
                stale_mtime,
                &SessionInstrumentation {
                    error_count: 1,
                    top_errors: vec![],
                    steer_count: 0,
                    steers: vec![],
                    turn_count: 1,
                },
            )
        })
        .unwrap();

    // The transcript "grows": real content with two tool-verified errors
    // now on disk, guaranteed larger than the single-line fixture the
    // cache row above was keyed to.
    std::fs::write(
        &transcript_path,
        concat!(
            r#"{"type":"user","message":{"content":"do a small task"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ffmpeg"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","is_error":true,"content":"ffmpeg exit 1"}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu2","name":"Bash","input":{"command":"ffmpeg2"}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu2","is_error":true,"content":"ffmpeg exit 2"}]}}"#,
            "\n",
        ),
    )
    .unwrap();
    assert_ne!(
        std::fs::metadata(&transcript_path).unwrap().len(),
        stale_metadata.len(),
        "fixture must actually grow for this test to mean anything"
    );

    let html = render_with_projects_dir(
        &storage,
        projects_dir.path(),
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (index_html, _) = split_panes(&html);
    let short = short_oid(session_id);
    let row_start = index_html
        .find(&format!(r#"data-session="{short}""#))
        .unwrap();
    let row_end = index_html[row_start..]
        .find("</button>")
        .map(|offset| row_start + offset)
        .unwrap();
    let row = &index_html[row_start..row_end];
    assert!(
        row.contains("2 errors"),
        "a size-mismatched cache row must trigger a rescan producing the real, current \
             count, within the existing wall budget: {row}"
    );
    assert!(
        !row.contains(">1 error<"),
        "the stale cached count must never survive the rescan: {row}"
    );
}

// ---- Codex adversarial-review findings (journal v2) -------------------

/// Finding 1: TOUCH NEXT sources in priority order (blockers > open
/// todos > next_steps > superseded receipts) and the 3-line cap. One
/// session carries all four candidate sources (blocker, one open todo,
/// next_steps, a superseded symbol) — the cap must render exactly the
/// top three and drop the fourth. A second session carries five open
/// todos and nothing else, proving the cap also holds within a single
/// tier and preserves the episode's own todo order. A third session
/// carries ONLY a superseded symbol, proving tier 4 fires when nothing
/// higher-priority exists.
#[test]
fn touch_next_strip_sources_and_priority() {
    let storage = Storage::open_memory().unwrap();
    seed_episode_with_extra(
        &storage,
        "touch-priority",
        "2026-05-01T09:00:00Z",
        "wire the retry path",
        "partial",
        serde_json::json!({
            "todos": [{"content": "wire the retry queue", "status": "pending"}],
            "next_steps": "polish the release notes",
            "blockers": "clear the migration lock first",
        }),
    );
    seed_attribution(
        &storage,
        "touch-priority",
        "/repo/src/retry.rs",
        "old_helper",
        "2026-05-01T09:00:00Z",
    );
    let witness_id = seed_witness(
        &storage,
        "proj",
        "/repo/src/retry.rs",
        "old_helper",
        "aoid-tn1",
    );
    seed_verdict_event(
        &storage,
        witness_id,
        "superseded_by",
        Some("1234567890abcdef"),
        "2026-05-01 10:00:00",
    );

    seed_episode_with_extra(
        &storage,
        "touch-cap",
        "2026-05-02T09:00:00Z",
        "clear the backlog",
        "done",
        serde_json::json!({
            "todos": [
                {"content": "task one", "status": "pending"},
                {"content": "task two", "status": "pending"},
                {"content": "task three", "status": "pending"},
                {"content": "task four", "status": "pending"},
                {"content": "task five", "status": "pending"},
            ],
        }),
    );

    seed_episode(
        &storage,
        "touch-superseded-only",
        "2026-05-03T09:00:00Z",
        "touch the legacy helper",
        "done",
        &[],
        &[],
    );
    seed_attribution(
        &storage,
        "touch-superseded-only",
        "/repo/src/legacy.rs",
        "legacy_fn",
        "2026-05-03T09:00:00Z",
    );
    let witness_id2 = seed_witness(
        &storage,
        "proj",
        "/repo/src/legacy.rs",
        "legacy_fn",
        "aoid-tn2",
    );
    seed_verdict_event(
        &storage,
        witness_id2,
        "superseded_by",
        Some("abcdef9876543210"),
        "2026-05-03 10:00:00",
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (_, detail_html) = split_panes(&html);

    let pane_for = |session_id: &str| {
        let short = short_oid(session_id);
        let start = detail_html
            .find(&format!(r#"data-session="{short}""#))
            .unwrap();
        let end = start + detail_html[start..].find("</article>").unwrap();
        detail_html[start..end].to_string()
    };
    let strip_of = |pane: &str| -> String {
        let start = pane
            .find(r#"class="touch-next-list""#)
            .expect("touch-next strip must render when evidence exists");
        let end = start + pane[start..].find("</ul>").unwrap();
        pane[start..end].to_string()
    };

    let priority_pane = pane_for("touch-priority");
    let priority_strip = strip_of(&priority_pane);
    assert_eq!(
        priority_strip.matches("<li>").count(),
        3,
        "the cap must hold at 3 lines even with 4 candidate sources: {priority_strip}"
    );
    let blocker_pos = priority_strip
        .find("blocked: clear the migration lock first")
        .expect("blocker must be the first source");
    let todo_pos = priority_strip
        .find("wire the retry queue")
        .expect("open todo must be the second source");
    let next_pos = priority_strip
        .find("polish the release notes")
        .expect("next_steps must be the third source");
    assert!(
        blocker_pos < todo_pos && todo_pos < next_pos,
        "priority order must be blockers > open todos > next_steps: {priority_strip}"
    );
    assert!(
        !priority_strip.contains("old_helper"),
        "the superseded-receipt source must not render in the TOUCH NEXT strip once the \
             cap is already full (it may still legitimately appear in the SINCE THEN panel \
             below): {priority_strip}"
    );

    let cap_pane = pane_for("touch-cap");
    let cap_strip = strip_of(&cap_pane);
    assert_eq!(
        cap_strip.matches("<li>").count(),
        3,
        "5 open todos and nothing else must still cap at 3: {cap_strip}"
    );
    let t1 = cap_strip.find("task one").unwrap();
    let t2 = cap_strip.find("task two").unwrap();
    let t3 = cap_strip.find("task three").unwrap();
    assert!(
        t1 < t2 && t2 < t3,
        "the surviving 3 lines must preserve the episode's own todo order: {cap_strip}"
    );
    assert!(
        !cap_strip.contains("task four") && !cap_strip.contains("task five"),
        "todos beyond the cap must not render: {cap_strip}"
    );

    let superseded_only_pane = pane_for("touch-superseded-only");
    assert!(
        superseded_only_pane.contains("read ⌗abcdef98 before touching legacy_fn"),
        "with no higher-priority evidence, tier 4 (superseded receipt) fills the strip: \
             {superseded_only_pane}"
    );
}

/// Finding 1: a session with no open todos, no blockers, no next_steps,
/// and no superseded symbols renders the explicit abstention sentence —
/// never an empty `<ul>`, never invented advice.
#[test]
fn touch_next_abstains_honestly() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "touch-abstain",
        "2026-05-04T09:00:00Z",
        "read the logs, nothing to change",
        "done",
        &[],
        &[],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (_, detail_html) = split_panes(&html);
    let short = short_oid("touch-abstain");
    let pane_start = detail_html
        .find(&format!(r#"data-session="{short}""#))
        .unwrap();
    let pane_end = pane_start + detail_html[pane_start..].find("</article>").unwrap();
    let pane = &detail_html[pane_start..pane_end];

    assert!(
        pane.contains(
            "class=\"touch-next-empty\">nothing evidenced to touch — no open todos, \
                 blockers, or adverse verdicts</p>"
        ),
        "no evidence anywhere must render the explicit abstention sentence, not silence: {pane}"
    );
    assert!(
        !pane.contains("class=\"touch-next-list\""),
        "no evidence must never render an empty touch-next list: {pane}"
    );
}

/// Finding 3: the outcome badge always carries the ENDED prefix and the
/// session date as its title. The NOW badge is evidence-gated: adverse
/// verdicts (superseded + obsolete, NOT reinstated) render "NOW: N
/// superseded since"; a session with no attributed symbols at all
/// renders no NOW badge; and there is no verdict type anywhere that
/// earns "NOW: ok/fine/current".
#[test]
fn ended_and_now_badges_are_evidence_gated() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "ended-adverse",
        "2026-05-04T09:00:00Z",
        "chase the flaky release",
        "failed at the gate",
        &[],
        &[],
    );
    for symbol in ["sym_super", "sym_obs", "sym_reinst"] {
        seed_attribution(
            &storage,
            "ended-adverse",
            "/repo/src/lib.rs",
            symbol,
            "2026-05-04T09:00:00Z",
        );
    }
    let w_super = seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "sym_super",
        "aoid-eb1",
    );
    seed_verdict_event(
        &storage,
        w_super,
        "superseded_by",
        Some("aaaaaaaaaaaaaaaa"),
        "2026-05-04 10:00:00",
    );
    let w_obs = seed_witness(&storage, "proj", "/repo/src/lib.rs", "sym_obs", "aoid-eb2");
    seed_verdict_event(
        &storage,
        w_obs,
        "anchor_obsolete",
        None,
        "2026-05-04 11:00:00",
    );
    let w_reinst = seed_witness(
        &storage,
        "proj",
        "/repo/src/lib.rs",
        "sym_reinst",
        "aoid-eb3",
    );
    seed_verdict_event(
        &storage,
        w_reinst,
        "anchor_reinstated",
        None,
        "2026-05-04 12:00:00",
    );

    seed_episode(
        &storage,
        "ended-no-symbols",
        "2026-05-05T09:00:00Z",
        "ship the calm feature",
        "shipped to production",
        &[],
        &[],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );

    assert!(
        !html.contains("NOW: ok") && !html.contains("NOW: fine") && !html.contains("NOW: current"),
        "no verdict type proves current health — 'NOW: ok/fine/current' must never render"
    );

    let (_, detail_html) = split_panes(&html);

    let adverse_short = short_oid("ended-adverse");
    let adverse_start = detail_html
        .find(&format!(r#"data-session="{adverse_short}""#))
        .unwrap();
    let adverse_end = adverse_start + detail_html[adverse_start..].find("</article>").unwrap();
    let adverse_pane = &detail_html[adverse_start..adverse_end];
    assert!(
        adverse_pane
            .contains(r#"class="outcome-badge failed" title="2026-05-04">ENDED: failed</span>"#),
        "the outcome badge must carry the ENDED prefix and the session date as its title: \
             {adverse_pane}"
    );
    assert!(
        adverse_pane.contains(r#"class="now-badge superseded""#)
            && adverse_pane.contains("NOW: 2 superseded since"),
        "two adverse verdicts (superseded + obsolete, reinstated excluded) must render \
             'NOW: 2 superseded since': {adverse_pane}"
    );

    let clean_short = short_oid("ended-no-symbols");
    let clean_start = detail_html
        .find(&format!(r#"data-session="{clean_short}""#))
        .unwrap();
    let clean_end = clean_start + detail_html[clean_start..].find("</article>").unwrap();
    let clean_pane = &detail_html[clean_start..clean_end];
    assert!(
        clean_pane
            .contains(r#"class="outcome-badge success" title="2026-05-05">ENDED: success</span>"#),
        "a clean success session still gets the ENDED badge: {clean_pane}"
    );
    assert!(
        !clean_pane.contains("class=\"now-badge"),
        "a session with no attributed symbols at all must render no NOW badge: {clean_pane}"
    );
}

/// Finding 4: ASK and DELIBERATION stage cards render closed by default
/// (no `open` attribute); STEER and OUTCOME stay open.
#[test]
fn ask_and_deliberation_render_closed() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "stage-open-test",
        "2026-05-06T09:00:00Z",
        "investigate the queue",
        "done",
        &["/repo/src/queue.rs"],
        &[("finish the migration", "pending")],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );

    assert!(
        html.contains(r#"data-stage="S_INPUT">"#),
        "ASK must render with no `open` attribute: {html}"
    );
    assert!(!html.contains(r#"data-stage="S_INPUT" open>"#));
    assert!(
        html.contains(r#"data-stage="S_DELIB">"#),
        "DELIBERATION must render with no `open` attribute"
    );
    assert!(!html.contains(r#"data-stage="S_DELIB" open>"#));
    assert!(
        html.contains(r#"data-stage="S_STEER" open>"#),
        "STEER must render open by default"
    );
    assert!(
        html.contains(r#"data-stage="S_OUT" open>"#),
        "OUTCOME must render open by default"
    );
}

/// Finding 4: open todos always render before completed ones, in the
/// episode's own order; a completed list of more than 5 folds behind a
/// nested closed `<details>` ("N completed"), but every completed todo
/// still lands in the DOM. A session with exactly 5 completed todos (at
/// the threshold, not over it) must NOT fold.
#[test]
fn completed_todos_fold_when_more_than_five() {
    let storage = Storage::open_memory().unwrap();
    seed_episode(
        &storage,
        "fold-over-five",
        "2026-05-07T09:00:00Z",
        "clean up the backlog",
        "done",
        &[],
        &[
            ("open task", "pending"),
            ("t1", "completed"),
            ("t2", "completed"),
            ("t3", "completed"),
            ("t4", "completed"),
            ("t5", "completed"),
            ("t6", "completed"),
        ],
    );
    seed_episode(
        &storage,
        "fold-at-five",
        "2026-05-08T09:00:00Z",
        "clean up a smaller backlog",
        "done",
        &[],
        &[
            ("u1", "completed"),
            ("u2", "completed"),
            ("u3", "completed"),
            ("u4", "completed"),
            ("u5", "completed"),
        ],
    );

    let html = render(
        &storage,
        crate::storage::recap_feeds::ConsumptionMode::AnnotateOnly,
    );
    let (_, detail_html) = split_panes(&html);

    let over_short = short_oid("fold-over-five");
    let over_start = detail_html
        .find(&format!(r#"data-session="{over_short}""#))
        .unwrap();
    let over_end = over_start + detail_html[over_start..].find("</article>").unwrap();
    let over_pane = &detail_html[over_start..over_end];
    assert!(
        over_pane.contains("class=\"completed-fold\""),
        "6 completed todos must fold behind a nested closed <details>: {over_pane}"
    );
    assert!(
        over_pane.contains("<summary>6 completed</summary>"),
        "the fold summary must state the true completed count: {over_pane}"
    );
    for label in ["t1", "t2", "t3", "t4", "t5", "t6"] {
        assert!(
            over_pane.contains(label),
            "every completed todo must still land in the DOM even when folded: {over_pane}"
        );
    }
    let open_pos = over_pane
        .find("open task")
        .expect("the open todo must render");
    let fold_pos = over_pane
        .find("class=\"completed-fold\"")
        .expect("the fold must render");
    assert!(
        open_pos < fold_pos,
        "open todos must render before completed ones: {over_pane}"
    );

    let at_five_short = short_oid("fold-at-five");
    let at_five_start = detail_html
        .find(&format!(r#"data-session="{at_five_short}""#))
        .unwrap();
    let at_five_end = at_five_start + detail_html[at_five_start..].find("</article>").unwrap();
    let at_five_pane = &detail_html[at_five_start..at_five_end];
    assert!(
        !at_five_pane.contains("class=\"completed-fold\""),
        "exactly 5 completed todos (at the threshold, not over it) must not fold: {at_five_pane}"
    );
    assert_eq!(
        at_five_pane.matches("class=\"task done\"").count(),
        5,
        "all 5 completed todos must render inline: {at_five_pane}"
    );
}
