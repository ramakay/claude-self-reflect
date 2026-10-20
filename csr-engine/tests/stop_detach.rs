//! `csr-engine hook stop` returns at once and a detached worker does the
//! transcript work (see `hooks::stop_work`). These run the real binary against a
//! temp HOME and database, never live data.
#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use csr_engine::hooks::stop_work::{spool_dir, KeyLock};

struct World {
    _root: tempfile::TempDir,
    home: PathBuf,
    db: PathBuf,
    projects: PathBuf,
    timing: PathBuf,
}

impl World {
    fn new() -> Self {
        // macOS holds the first launch of a freshly built binary from a freshly
        // built parent in dyld for 1 to 3 s (measured: a `sample` of the hook
        // showed it parked in `_dyld_start`, before `main`). That is launch cost,
        // not hook time, and it would land on whichever test spawns first.
        static WARM: std::sync::Once = std::sync::Once::new();
        WARM.call_once(|| {
            for _ in 0..2 {
                let _ = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
                    .arg("--version")
                    .output();
            }
        });
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let home = root.path().join("home");
        // The binary resolves its model from $HOME: give it a clone of the one
        // this test process resolves, so nothing downloads.
        let source = dirs::cache_dir()
            .expect("a cache dir")
            .join("csr-engine")
            .join("fastembed");
        let target = home.join("Library/Caches/csr-engine");
        std::fs::create_dir_all(&target).unwrap();
        if source.exists() {
            let status = Command::new("cp")
                .arg("-Rc")
                .arg(&source)
                .arg(target.join("fastembed"))
                .status()
                .unwrap();
            assert!(status.success(), "could not clone the local model cache");
        }
        let data = root.path().join("data");
        let projects = root.path().join("projects");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        Self {
            timing: root.path().join("hook-timing.log"),
            db: data.join("csr-engine.db"),
            home,
            projects,
            _root: root,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_csr-engine"));
        cmd.arg("--db-path")
            .arg(&self.db)
            .arg("--projects-dir")
            .arg(&self.projects)
            .args(args)
            .env("HOME", &self.home)
            .env("CSR_TIMING_LOG", &self.timing)
            .env("CSR_NO_DREAMING", "1")
            .env("CSR_NO_AI_NARRATIVES", "1")
            .env("CSR_NO_CODEMAP", "1")
            .env_remove("CSR_STOP_INLINE")
            .env_remove("CSR_STOP_WORK_EXE")
            .env_remove("CSR_DISABLE_RECURSIVE_HOOKS");
        cmd
    }

    /// `hook stop` with `payload` on stdin; returns (wall time, output).
    fn stop(&self, payload: &serde_json::Value, env: &[(&str, &str)]) -> (Duration, Output) {
        let mut cmd = self.command(&["hook", "stop"]);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let started = Instant::now();
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        (started.elapsed(), output)
    }

    fn timing_log(&self) -> String {
        std::fs::read_to_string(&self.timing).unwrap_or_default()
    }

    /// Total `runs=` over every `stop-work` timing line.
    fn worker_runs(&self) -> usize {
        self.timing_log()
            .lines()
            .filter(|l| l.contains("CSR hook stop-work ["))
            .filter_map(|l| l.rsplit("runs=").next()?.trim().parse::<usize>().ok())
            .sum()
    }

    /// Wait (bounded) until `done` holds; panic with the timing log if it never does.
    fn wait_for(&self, what: &str, limit: Duration, done: impl Fn(&World) -> bool) {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if done(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "timed out waiting for {what}\n--- timing log\n{}",
            self.timing_log()
        );
    }

    fn query<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> T) -> Option<T> {
        if !self.db.exists() {
            return None;
        }
        let conn = rusqlite::Connection::open_with_flags(
            &self.db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;
        Some(f(&conn))
    }

    /// The stored episodes, parsed, oldest first.
    fn episodes(&self) -> Vec<serde_json::Value> {
        self.query(|conn| {
            let Ok(mut stmt) = conn.prepare(
                "SELECT content FROM reflections WHERE tags LIKE '%session_episode%' \
                 ORDER BY timestamp, id",
            ) else {
                return Vec::new();
            };
            stmt.query_map([], |r| r.get::<_, String>(0))
                .map(|rows| {
                    rows.flatten()
                        .filter_map(|c| serde_json::from_str(&c).ok())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
    }

    /// Everything a Stop stores, minus ids and timestamps.
    fn stored_rows(&self) -> serde_json::Value {
        self.query(|conn| {
            let chunks: Vec<(String, String, String, i64)> = conn
                .prepare(
                    "SELECT conversation_id, project_name, content, message_count \
                     FROM chunks ORDER BY conversation_id, content",
                )
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .flatten()
                .collect();
            let embeddings: i64 = conn
                .query_row("SELECT COUNT(*) FROM chunk_embeddings", [], |r| r.get(0))
                .unwrap();
            let reflections: Vec<(String, String)> = conn
                .prepare("SELECT tags, content FROM reflections ORDER BY tags, content")
                .unwrap()
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .flatten()
                .map(|(tags, content)| (tags, strip_times(&content)))
                .collect();
            let anchors: i64 = conn
                .query_row("SELECT COUNT(*) FROM session_anchors", [], |r| r.get(0))
                .unwrap_or(-1);
            serde_json::json!({
                "chunks": chunks,
                "chunk_embeddings": embeddings,
                "reflections": reflections,
                "session_anchors": anchors,
            })
        })
        .unwrap_or_default()
    }
}

/// An episode's `timestamp` is the run's wall clock; everything else must match.
fn strip_times(content: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(mut v) => {
            if let Some(o) = v.as_object_mut() {
                o.remove("timestamp");
            }
            v.to_string()
        }
        Err(_) => content.to_string(),
    }
}

fn line(kind: &str, text: &str, second: u32) -> String {
    format!(
        r#"{{"type":"{kind}","message":{{"content":[{{"type":"text","text":{}}}]}},"timestamp":"2026-10-20T10:00:{second:02}Z"}}"#,
        serde_json::to_string(text).unwrap()
    )
}

/// A small session: a request, an Edit, a reply.
fn small_transcript(request: &str) -> String {
    let edit = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Taking the lock before the dump."},{"type":"tool_use","name":"Edit","input":{"file_path":"/src/search.rs","old_string":"fn dump","new_string":"fn dump_locked"}}]},"timestamp":"2026-10-20T10:00:01Z"}"#;
    format!(
        "{}\n{edit}\n{}\n",
        line("user", request, 0),
        line(
            "assistant",
            "The dump now takes the advisory lock first.",
            2
        )
    )
}

/// A `file-history-snapshot` line: the bulk real transcripts carry, which the
/// importer skips but every pass over the file still reads and parses.
fn padding_line() -> String {
    let pad = "x".repeat(4000);
    format!(
        r#"{{"type":"file-history-snapshot","messageId":"m","snapshot":{{"trackedFileBackups":{{"f":"{pad}"}},"timestamp":"2026-10-20T10:00:00Z"}},"isSnapshotUpdate":false}}"#
    )
}

/// What fills a big transcript.
#[derive(Clone, Copy)]
enum Bulk {
    /// `file-history-snapshot` lines: cheap for the importer, still read and parsed by
    /// every pass over the file.
    Snapshots,
    /// Conversation turns of distinct text: what a real long session is made of, and
    /// what makes an inline Stop slow (every turn is chunked and embedded).
    Turns,
}

fn write_big_transcript(path: &Path, request: &str, min_bytes: u64, bulk: Bulk) -> u64 {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let head = small_transcript(request);
    out.write_all(head.as_bytes()).unwrap();
    let pad = padding_line();
    let mut written = head.len() as u64;
    let mut n = 0u64;
    while written < min_bytes {
        let row = match bulk {
            Bulk::Snapshots => pad.clone(),
            Bulk::Turns => {
                n += 1;
                let body = "the advisory lock guards dump number ".repeat(55);
                let text = format!("Turn {n}: {body}{n}");
                let kind = if n.is_multiple_of(2) {
                    "user"
                } else {
                    "assistant"
                };
                line(kind, &text, (n % 60) as u32)
            }
        };
        writeln!(out, "{row}").unwrap();
        written += row.len() as u64 + 1;
    }
    out.flush().unwrap();
    written
}

struct Session {
    dir: PathBuf,
    id: String,
    _tmp: tempfile::TempDir,
}

impl Session {
    fn new(id: &str) -> Self {
        let tmp = tempfile::tempdir_in("/private/tmp").unwrap();
        let dir = tmp.path().join("proj-stopdetach");
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            id: id.to_string(),
            _tmp: tmp,
        }
    }

    fn transcript(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.jsonl"))
    }

    fn payload(&self, transcript: &Path) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.id,
            "transcript_path": transcript,
            "cwd": self.dir,
        })
    }
}

fn spool_of(world: &World, sid: &str) -> Option<serde_json::Value> {
    let path = spool_dir(&world.db).join(format!("{sid}.json"));
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[test]
fn stop_on_a_50mb_transcript_returns_fast_and_leaves_the_input_in_the_spool() {
    let world = World::new();
    let session = Session::new("sid-fast");
    let transcript = session.transcript("big");
    let size = write_big_transcript(
        &transcript,
        "Take the dump lock first",
        50 * 1024 * 1024,
        Bulk::Turns,
    );
    assert!(size >= 50 * 1024 * 1024);

    // Hold the session's worker lock, so a worker that starts cannot run: the
    // spool is then still there to look at.
    let held = KeyLock::try_acquire(&spool_dir(&world.db), "sid-fast").unwrap();
    let (took, output) = world.stop(&session.payload(&transcript), &[]);
    println!("FAST_PATH_MS={} transcript_bytes={size}", took.as_millis());
    assert!(output.status.success(), "{output:?}");
    assert!(
        took < Duration::from_millis(500),
        "hook took {took:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        world.timing_log()
    );
    assert!(output.stdout.is_empty(), "Stop prints nothing to stdout");

    let spool = spool_of(&world, "sid-fast").expect("the input is spooled");
    assert_eq!(
        spool["input"]["transcript_path"],
        transcript.to_string_lossy().as_ref()
    );
    assert_eq!(spool["input"]["session_id"], "sid-fast");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(spool_dir(&world.db).join("sid-fast.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // It handed work off and did none: no import, no episode.
    assert!(world.episodes().is_empty());
    assert!(
        world
            .timing_log()
            .contains("CSR hook stop [proj-stopdetach]"),
        "{}",
        world.timing_log()
    );
    drop(held);
}

#[test]
fn a_detached_worker_stores_the_episode_after_stop_has_returned() {
    let world = World::new();
    let session = Session::new("sid-worker");
    let transcript = session.transcript("big");
    write_big_transcript(
        &transcript,
        "Take the dump lock first",
        50 * 1024 * 1024,
        Bulk::Snapshots,
    );

    let (took, output) = world.stop(&session.payload(&transcript), &[]);
    println!("DETACHED_STOP_MS={}", took.as_millis());
    assert!(output.status.success(), "{output:?}");
    assert!(
        took < Duration::from_millis(500),
        "hook took {took:?}\n{}\n{}",
        String::from_utf8_lossy(&output.stderr),
        world.timing_log()
    );
    assert!(output.stdout.is_empty());

    // The worker is observable by its log line and by the episode.
    world.wait_for("the stop-work line", Duration::from_secs(180), |w| {
        w.worker_runs() >= 1
    });
    let log = world.timing_log();
    let work = log
        .lines()
        .find(|l| l.contains("CSR hook stop-work [proj-stopdetach]: total="))
        .unwrap_or_else(|| panic!("no stop-work line in\n{log}"));
    assert!(work.ends_with("runs=1"), "{work}");
    let episodes = world.episodes();
    assert_eq!(episodes.len(), 1, "{episodes:?}");
    assert_eq!(episodes[0]["session_id"], "sid-worker");
    assert!(
        episodes[0]["request"]
            .as_str()
            .unwrap()
            .contains("dump lock"),
        "{}",
        episodes[0]
    );
    assert!(
        !spool_dir(&world.db).join("sid-worker.json").exists(),
        "the processed spool is deleted"
    );
}

#[test]
fn a_detached_stop_ends_with_the_rows_an_inline_stop_stores() {
    let session = Session::new("sid-equal");
    let transcript = session.transcript("t");
    std::fs::write(
        &transcript,
        small_transcript("Take the dump lock before the dump"),
    )
    .unwrap();
    let payload = session.payload(&transcript);

    let inline = World::new();
    let (_, output) = inline.stop(&payload, &[("CSR_STOP_INLINE", "1")]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    // Inline: everything is stored by the time the hook returns, and no worker ran.
    assert_eq!(inline.episodes().len(), 1);
    assert!(!spool_dir(&inline.db).exists(), "inline never spools");
    assert_eq!(inline.worker_runs(), 0);

    let detached = World::new();
    let (_, output) = detached.stop(&payload, &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    detached.wait_for("the worker", Duration::from_secs(120), |w| {
        w.worker_runs() >= 1
    });

    let want = inline.stored_rows();
    let got = detached.stored_rows();
    assert!(
        !want["chunks"].as_array().unwrap().is_empty(),
        "the comparison must cover imported chunks: {want}"
    );
    assert_eq!(got, want, "detached and inline Stop store the same rows");
}

#[test]
fn a_burst_of_stops_while_a_worker_holds_the_lock_is_one_run_on_the_last_input() {
    let world = World::new();
    let session = Session::new("sid-burst");
    let dir = spool_dir(&world.db);
    let held = KeyLock::try_acquire(&dir, "sid-burst").unwrap();
    for (name, request) in [
        ("a", "First: rename the dump"),
        ("b", "Second: lock the dump"),
        ("c", "Third: add the advisory lock to the dump"),
    ] {
        let transcript = session.transcript(name);
        std::fs::write(&transcript, small_transcript(request)).unwrap();
        let (took, output) = world.stop(&session.payload(&transcript), &[]);
        assert!(output.status.success(), "{output:?}");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }
    // Their workers were turned away by the lock; the spool is the third input.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(world.worker_runs(), 0);
    assert!(world.episodes().is_empty());
    let spool = spool_of(&world, "sid-burst").unwrap();
    assert_eq!(
        spool["input"]["transcript_path"],
        session.transcript("c").to_string_lossy().as_ref()
    );
    drop(held);

    // The next worker (here run in the foreground) takes the one input that is left.
    let output = world
        .command(&["hook", "stop-work", "--session", "sid-burst"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(world.worker_runs(), 1, "{}", world.timing_log());
    let episodes = world.episodes();
    assert_eq!(episodes.len(), 1);
    assert!(
        episodes[0]["request"]
            .as_str()
            .unwrap()
            .starts_with("Third:"),
        "the last input is the one processed: {}",
        episodes[0]
    );
    assert!(!dir.join("sid-burst.json").exists());
    assert!(!dir.join("sid-burst.work").exists());
}

#[test]
fn three_quick_stops_never_cost_more_runs_than_stops_and_the_last_input_wins() {
    let world = World::new();
    let session = Session::new("sid-quick");
    for (name, request) in [
        ("a", "First: rename the dump"),
        ("b", "Second: lock the dump"),
        ("c", "Third: add the advisory lock to the dump"),
    ] {
        let transcript = session.transcript(name);
        std::fs::write(&transcript, small_transcript(request)).unwrap();
        let (_, output) = world.stop(&session.payload(&transcript), &[]);
        assert!(output.status.success(), "{output:?}");
    }
    world.wait_for(
        "the last input to be stored",
        Duration::from_secs(120),
        |w| {
            w.episodes()
                .last()
                .and_then(|e| e["request"].as_str().map(|r| r.starts_with("Third:")))
                .unwrap_or(false)
                && !spool_dir(&w.db).join("sid-quick.json").exists()
                && !spool_dir(&w.db).join("sid-quick.work").exists()
        },
    );
    let runs = world.worker_runs();
    println!("QUICK_BURST_RUNS={runs}");
    assert!(
        (1..=3).contains(&runs),
        "runs={runs}\n{}",
        world.timing_log()
    );
    assert_eq!(world.episodes().len(), 1, "one episode per session");
}

#[test]
fn a_worker_that_cannot_start_runs_the_stop_inline() {
    let world = World::new();
    let session = Session::new("sid-nospawn");
    let transcript = session.transcript("t");
    std::fs::write(&transcript, small_transcript("Take the dump lock first")).unwrap();
    let (_, output) = world.stop(
        &session.payload(&transcript),
        &[("CSR_STOP_WORK_EXE", "/nonexistent/csr-engine-missing")],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    // Stored by the time the hook returned: it ran inline.
    let episodes = world.episodes();
    assert_eq!(
        episodes.len(),
        1,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(episodes[0]["session_id"], "sid-nospawn");
    assert!(
        !spool_dir(&world.db).join("sid-nospawn.json").exists(),
        "an input that ran inline leaves no spool"
    );
    assert!(
        !spool_dir(&world.db).join("sid-nospawn.work").exists(),
        "and no claim"
    );
    // The hook process drained the key itself, under the worker's lock and path.
    assert_eq!(world.worker_runs(), 1, "{}", world.timing_log());
}

#[test]
fn a_subagent_stop_is_processed_by_its_own_detached_worker() {
    let world = World::new();
    let session = Session::new("sid-agent");
    let transcript = session.transcript("t");
    std::fs::write(&transcript, small_transcript("Take the dump lock first")).unwrap();
    let mut payload = session.payload(&transcript);
    payload["agent_id"] = serde_json::json!("agent_7");
    let (_, output) = world.stop(&payload, &[]);
    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let dir = spool_dir(&world.db);
    world.wait_for("the subagent's worker", Duration::from_secs(120), |w| {
        w.worker_runs() >= 1
            && !dir.join("sid-agent.agent_7.json").exists()
            && !dir.join("sid-agent.agent_7.work").exists()
    });
}

#[test]
fn a_stop_with_no_session_id_runs_inline() {
    let world = World::new();
    let session = Session::new("unused");
    let transcript = session.transcript("t");
    std::fs::write(&transcript, small_transcript("Take the dump lock first")).unwrap();
    let (_, output) = world.stop(
        &serde_json::json!({"transcript_path": transcript, "cwd": session.dir}),
        &[],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(!spool_dir(&world.db).exists());
    assert_eq!(world.worker_runs(), 0);
}

#[test]
fn a_stale_spool_with_no_holder_is_processed_by_the_next_worker() {
    let world = World::new();
    let old = Session::new("sid-old");
    let transcript = old.transcript("t");
    std::fs::write(&transcript, small_transcript("Stale: take the dump lock")).unwrap();
    // A Stop from two days ago whose worker never ran.
    let dir = spool_dir(&world.db);
    csr_engine::hooks::stop_work::write_spool(
        &dir,
        "sid-old",
        &serde_json::from_value(old.payload(&transcript)).unwrap(),
    )
    .unwrap();
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(48 * 3600);
    std::fs::File::options()
        .write(true)
        .open(dir.join("sid-old.json"))
        .unwrap()
        .set_modified(two_days_ago)
        .unwrap();

    // A different session's Stop starts a worker; it sweeps the stale spool too.
    let fresh = Session::new("sid-new");
    let fresh_transcript = fresh.transcript("t");
    std::fs::write(&fresh_transcript, small_transcript("Fresh: lock the dump")).unwrap();
    let (_, output) = world.stop(&fresh.payload(&fresh_transcript), &[]);
    assert!(output.status.success(), "{output:?}");
    world.wait_for("both sessions' episodes", Duration::from_secs(120), |w| {
        w.episodes().len() == 2 && !dir.join("sid-old.json").exists()
    });
    assert_eq!(world.worker_runs(), 2);
}

#[test]
fn the_worker_writes_nothing_to_a_terminal_and_stdout_stays_empty() {
    // The worker is started with null stdio; run it in the foreground here and
    // check that nothing reaches stdout either.
    let world = World::new();
    let session = Session::new("sid-quiet");
    let transcript = session.transcript("t");
    std::fs::write(&transcript, small_transcript("Take the dump lock first")).unwrap();
    csr_engine::hooks::stop_work::write_spool(
        &spool_dir(&world.db),
        "sid-quiet",
        &serde_json::from_value(session.payload(&transcript)).unwrap(),
    )
    .unwrap();
    let output = world
        .command(&["hook", "stop-work", "--session", "sid-quiet"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(world.episodes().len(), 1);
}
