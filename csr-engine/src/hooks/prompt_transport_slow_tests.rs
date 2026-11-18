//! Slow prompt answers over the daemon socket: a slow answer leaves one line
//! of step times in the timing log, and an answer the daemon finishes before
//! the hook stops waiting is delivered.

use super::tests::{fixture, fixture_over, input, ready, warm};
use super::*;

/// The slow-answer lines in this process's timing log for `prompt_id`.
fn slow_lines(prompt_id: &str) -> Vec<String> {
    let Some(path) = crate::telemetry::default_log_path() else {
        return Vec::new();
    };
    let tail = format!(" prompt_id={prompt_id}");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(" CSR prompt-answer slow: ") && line.ends_with(&tail))
        .map(str::to_owned)
        .collect()
}

/// The render that writes the line can end after the hook stopped waiting.
async fn wait_for_slow_lines(prompt_id: &str) -> Vec<String> {
    for _ in 0..100 {
        let lines = slow_lines(prompt_id);
        if !lines.is_empty() {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Vec::new()
}

fn delayed(dir: &Path, delay_ms: u64, prompt_id: &str) -> HookInput {
    let mut input = input(dir);
    input.session_id = Some(format!("render-delay-ms={delay_ms}"));
    input.prompt_id = Some(prompt_id.to_owned());
    input
}

async fn ask_as(dir: &Path, input: HookInput, budget: Duration) -> Socket {
    exchange(
        &dir.join("csr-engine.db"),
        dir,
        Kind::Prompt,
        input,
        budget,
        |_| true,
    )
    .await
}

/// The value of `key=` in a slow-answer line.
fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let start = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("no {key} in {line}"))
        + key.len()
        + 2;
    line[start..].split(' ').next().unwrap()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn a_slow_answer_writes_one_line_of_step_times_and_a_fast_one_writes_none() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let dir = tempfile::tempdir().unwrap();
    let engine = fixture(dir.path());
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_for_daemon(engine, shutdown.clone());
    ready(dir.path()).await;
    warm(dir.path()).await;
    let fast = format!("fast-{}", std::process::id());
    let slow = format!("slow-{}", std::process::id());
    let budget = Duration::from_secs(2);
    let reply = ask_as(dir.path(), delayed(dir.path(), 0, &fast), budget).await;
    assert!(matches!(reply, Socket::Answered(..)), "{reply:?}");
    let reply = ask_as(dir.path(), delayed(dir.path(), 250, &slow), budget).await;
    assert!(matches!(reply, Socket::Answered(..)), "{reply:?}");

    let lines = wait_for_slow_lines(&slow).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    let body = line.split_once(" CSR prompt-answer slow: ").unwrap().1;
    let keys: Vec<&str> = body
        .split(' ')
        .map(|pair| pair.split_once('=').unwrap().0)
        .collect();
    assert_eq!(
        keys,
        [
            "outcome",
            "total",
            "slot_wait",
            "embed",
            "index_wait",
            "search",
            "db",
            "compose",
            "reply",
            "other",
            "deadline",
            "connected",
            "prompt_id"
        ],
        "{line}"
    );
    assert_eq!(field(line, "outcome"), "delivered");
    assert_eq!(field(line, "connected"), "1");
    let total = field(line, "total").strip_suffix("ms").unwrap();
    assert!(total.parse::<u64>().unwrap() >= 250, "{line}");
    for key in [
        "slot_wait",
        "embed",
        "index_wait",
        "search",
        "db",
        "compose",
        "reply",
        "other",
        "deadline",
    ] {
        let value = field(line, key).strip_suffix("ms").unwrap();
        assert!(value.parse::<u64>().is_ok(), "{key} in {line}");
    }
    // Numbers, the outcome and the prompt id only: no prompt text, no path,
    // and nothing a prompt-row or session-start reader would pick up.
    assert!(!line.contains("continue"), "{line}");
    assert!(!line.contains(&*dir.path().to_string_lossy()), "{line}");
    for marker in [
        "hook prompt-submit [",
        "session-start",
        "query=\"",
        "prompt=\"",
        "preview=\"",
    ] {
        assert!(!line.contains(marker), "{marker} in {line}");
    }
    assert!(slow_lines(&fast).is_empty(), "{:?}", slow_lines(&fast));
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn a_render_that_ends_after_the_hook_left_is_logged_late_and_disconnected() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let dir = tempfile::tempdir().unwrap();
    let engine = fixture(dir.path());
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_for_daemon(engine, shutdown.clone());
    ready(dir.path()).await;
    warm(dir.path()).await;
    let late = format!("late-{}", std::process::id());
    let reply = ask_as(
        dir.path(),
        delayed(dir.path(), 600, &late),
        Duration::from_millis(300),
    )
    .await;
    assert_eq!(reply, Socket::TimedOut);
    let lines = wait_for_slow_lines(&late).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(field(&lines[0], "outcome"), "late", "{}", lines[0]);
    assert_eq!(field(&lines[0], "connected"), "0", "{}", lines[0]);
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn a_full_render_times_its_steps_and_receipts_waiting_on_the_writer_are_not_counted() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let _gate = crate::injection::send_gate::Switch::off(); // one item is a thin pool
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csr-engine.db");
    let engine = fixture_over(dir.path(), crate::storage::Storage::open(&db).unwrap());
    let mut content = input(dir.path());
    content.prompt =
        Some("authentication timeout regression in token validation needs a fix".into());
    let vector = engine
        .embeddings()
        .embed_single(content.prompt.as_deref().unwrap())
        .unwrap();
    engine
        .storage()
        .insert_reflection(
            "solution",
            "Token validation regression was fixed by refreshing expired credentials before retry.",
            &["solution".into()],
            &vector,
        )
        .unwrap();
    engine
        .search()
        .write()
        .await
        .insert_reflection("solution".into(), vector);
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_for_daemon(engine, shutdown.clone());
    ready(dir.path()).await;
    warm(dir.path()).await;
    let budget = Duration::from_secs(2);

    // A slow content render: the line shows the embedding the render did.
    let timed = format!("timed-{}", std::process::id());
    let mut slow = content.clone();
    slow.session_id = Some("render-delay-ms=250".into());
    slow.prompt_id = Some(timed.clone());
    let reply = ask_as(dir.path(), slow, budget).await;
    assert!(matches!(reply, Socket::Answered(..)), "{reply:?}");
    let lines = wait_for_slow_lines(&timed).await;
    assert_eq!(lines.len(), 1, "{lines:?}");
    eprintln!("slow content render: {}", lines[0]);
    let embed = field(&lines[0], "embed").strip_suffix("ms").unwrap();
    assert!(embed.parse::<u64>().unwrap() >= 1, "{}", lines[0]);

    // A fast answer whose receipts then wait 600 ms for another writer: the
    // answer ended at the acknowledgement, so it is not a slow answer.
    let held = format!("held-{}", std::process::id());
    let mut fast = content.clone();
    fast.prompt_id = Some(held.clone());
    let writer = rusqlite::Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let reply = ask_as(dir.path(), fast, budget).await;
    assert!(matches!(reply, Socket::Answered(..)), "{reply:?}");
    tokio::time::sleep(Duration::from_millis(600)).await;
    writer.execute_batch("COMMIT").unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    for line in slow_lines(&held) {
        let total = field(&line, "total").strip_suffix("ms").unwrap();
        assert!(total.parse::<u64>().unwrap() < 500, "{line}");
    }
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn an_answer_ready_before_the_hook_stops_waiting_is_delivered() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let dir = tempfile::tempdir().unwrap();
    let engine = fixture(dir.path());
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_for_daemon(engine, shutdown.clone());
    ready(dir.path()).await;
    warm(dir.path()).await;
    // Done 425 ms after the request: past the daemon's own 420 ms, inside the
    // 520 ms the prompt hook waits.
    let mut input = input(dir.path());
    input.session_id = Some("render-delay-ms=425".into());
    let (text, transport, _) = resolve_decision(
        &dir.path().join("csr-engine.db"),
        dir.path(),
        input,
        Instant::now(),
        |_| true,
    )
    .await;
    assert_eq!(transport, Transport::Socket, "{text:?}");
    assert!(text.is_some_and(|text| !text.is_empty()));
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}

/// Build F's three composed answers that missed ended 482, 497 and 501 ms in,
/// past the 460 ms the hook then waited. One done 470 ms in is delivered.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn an_answer_done_470_ms_in_reaches_the_prompt() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let dir = tempfile::tempdir().unwrap();
    let engine = fixture(dir.path());
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_for_daemon(engine, shutdown.clone());
    ready(dir.path()).await;
    warm(dir.path()).await;
    let mut input = input(dir.path());
    input.session_id = Some("render-delay-ms=470".into());
    let (text, transport, _) = resolve_decision(
        &dir.path().join("csr-engine.db"),
        dir.path(),
        input,
        Instant::now(),
        |_| true,
    )
    .await;
    assert_eq!(transport, Transport::Socket, "{text:?}");
    assert!(text.is_some_and(|text| !text.is_empty()));
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}

/// A render answers someone who is waiting, so it runs user-initiated, above
/// the daemon's utility threads (all daemon threads ran at priority 20 in G).
#[cfg(target_os = "macos")]
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn a_prompt_render_runs_at_user_initiated_qos() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let dir = tempfile::tempdir().unwrap();
    let engine = fixture(dir.path());
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_for_daemon(engine, shutdown.clone());
    ready(dir.path()).await;
    warm(dir.path()).await;
    let mut input = input(dir.path());
    let session = format!("qos-readback-{}", std::process::id());
    input.session_id = Some(session.clone());
    let db = dir.path().join("csr-engine.db");
    let started = Instant::now();
    let (_, transport, _) = resolve_decision(&db, dir.path(), input, started, |_| true).await;
    assert_eq!(transport, Transport::Socket);
    let seen = super::server::RENDER_QOS.lock().unwrap().clone();
    let render = seen.iter().find(|(id, _)| *id == session);
    let expected = crate::runtime::Qos::UserInitiated;
    assert_eq!(render.map(|(_, qos)| *qos), Some(expected), "{seen:?}");
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}

/// A serve-only daemon runs no vocabulary refresh, so before this its first
/// render measured the vocabulary and read the dream join cold, holding the
/// render slot past the deadlines of the prompts behind it.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Serializes process-wide policy environment during this test.
async fn the_socket_opens_only_after_the_first_render_has_nothing_cold_to_read() {
    let _env = crate::daemon::dream_cadence::env_test_guard();
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csr-engine.db");
    let engine = fixture_over(dir.path(), crate::storage::Storage::open(&db).unwrap());
    let shutdown = Arc::new(AtomicBool::new(false));
    let server = spawn_serve_only(engine.clone(), shutdown.clone());
    ready(dir.path()).await;
    assert!(engine.storage().common_terms_are_cached());
    let conclusions = engine
        .storage()
        .with_connection(|conn| Ok(crate::storage::dream_delivery::conclusions_are_warm(conn)))
        .unwrap();
    assert!(conclusions);
    shutdown.store(true, Ordering::SeqCst);
    server.await.unwrap();
}
