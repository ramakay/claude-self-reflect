use super::*;

// Fixture secrets are assembled at runtime; no credential-shaped literal
// sits in the source tree.
fn fake_github() -> String {
    format!("{}{}", "ghp_", "A".repeat(36))
}

fn fake_stripe() -> String {
    format!("{}{}", "sk_live_", "Z".repeat(24))
}

struct Rig {
    _dir: tempfile::TempDir,
    storage: Storage,
    data_dir: PathBuf,
    projects: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let projects = dir.path().join("projects");
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(projects.join("-tmp-proj")).unwrap();
        Self {
            storage: Storage::open_memory().unwrap(),
            data_dir,
            projects,
            _dir: dir,
        }
    }

    fn mirror(&self) -> Mirror<'_> {
        let mut mirror = Mirror::new(&self.storage, &self.data_dir, &self.projects);
        mirror.disabled = false;
        mirror.seal_after = Duration::from_secs(3600);
        mirror
    }

    fn source(&self, name: &str) -> PathBuf {
        self.projects.join("-tmp-proj").join(name)
    }

    fn zst(&self, name: &str) -> PathBuf {
        transcripts_dir(&self.data_dir).join(format!("-tmp-proj/{name}.zst"))
    }

    fn decoded(&self, name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        open_transcript(&self.zst(name))
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }
}

fn append(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn line(n: usize, text: &str) -> String {
    serde_json::json!({"type": "user", "uuid": format!("u-{n}"), "message": {"content": text}})
        .to_string()
        + "\n"
}

fn write_session(path: &Path, secret: &str) {
    let lines = [
        serde_json::json!({
            "type": "user", "uuid": "u-1", "timestamp": "2026-10-14T10:00:00Z",
            "message": {"content": format!("please deploy with {secret} and then report back")}
        }),
        serde_json::json!({
            "type": "assistant", "uuid": "a-1", "timestamp": "2026-10-14T10:00:05Z",
            "message": {"content": [{"type": "text", "text": "Deployed. The staging rollout finished cleanly."}]}
        }),
    ];
    let body: String = lines.iter().map(|l| l.to_string() + "\n").collect();
    fs::write(path, body).unwrap();
}

async fn engine_for(rig: &Rig) -> crate::engine::Engine {
    use crate::embeddings::EmbeddingEngine;
    use crate::search::SearchEngine;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    let storage = Arc::new(Storage::open(&rig.data_dir.join("csr-engine.db")).unwrap());
    crate::engine::Engine::from_parts_with_index_dir(
        storage,
        Arc::new(EmbeddingEngine::new().unwrap()),
        Arc::new(RwLock::new(SearchEngine::new(64))),
        rig.projects.clone(),
        rig.data_dir.join("index-v3"),
    )
}

fn ignorable_line(bytes: usize) -> String {
    format!(
        "{{\"type\":\"file-history-snapshot\",\"blob\":\"{}\"}}\n",
        "x".repeat(bytes)
    )
}

fn chunks_like(engine: &crate::engine::Engine, needle: &str) -> i64 {
    engine
        .storage()
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM chunks WHERE content LIKE ?1",
                [format!("%{needle}%")],
                |row| row.get(0),
            )?)
        })
        .unwrap()
}

/// A conversation stored by a build without the scrubber: its first four
/// messages each hold a private key block. Returns the conversation id.
async fn legacy_conversation(engine: &crate::engine::Engine, src: &Path) -> String {
    let pem = format!(
        "{}{}\n{}\n{}{}",
        "-----BEGIN RSA PRIV",
        "ATE KEY-----",
        "Zm9vYmFy".repeat(200),
        "-----END RSA PRIV",
        "ATE KEY-----"
    );
    let texts: Vec<String> = (0..4)
        .map(|n| format!("dump {n}\n{pem}\nend {n}"))
        .collect();
    let conv = legacy_conversation_of(engine, src, &texts).await;
    assert!(chunks_like(engine, "Zm9vYmFy") >= 4);
    conv
}

/// A conversation stored by a build without the scrubber, one user message
/// per entry of `texts`.
async fn legacy_conversation_of(
    engine: &crate::engine::Engine,
    src: &Path,
    texts: &[String],
) -> String {
    let _off = crate::redact::DisabledForTest::set(true);
    let mut body = String::new();
    for (n, text) in texts.iter().enumerate() {
        body.push_str(&session_message(n, text));
    }
    fs::write(src, body).unwrap();
    assert!(engine.import_file(src, "proj").await.unwrap() >= texts.len());
    src.file_stem().unwrap().to_string_lossy().to_string()
}

fn session_message(n: usize, text: &str) -> String {
    serde_json::json!({
        "type": "user", "uuid": format!("r-{n}"),
        "timestamp": format!("2026-10-14T10:00:{n:02}Z"),
        "message": {"content": text}
    })
    .to_string()
        + "\n"
}

/// Every column of every chunk row of `conv`, and its embedding, as text.
fn stored_rows(engine: &crate::engine::Engine, conv: &str) -> Vec<String> {
    engine
        .storage()
        .with_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT c.id, c.project_name, c.timestamp, c.content, c.message_count,
                            c.summary, c.seq, c.is_sidechain, c.source, e.embedding
                     FROM chunks c LEFT JOIN chunk_embeddings e ON e.chunk_id = c.id
                     WHERE c.conversation_id = ?1 ORDER BY c.id",
            )?;
            let rows = stmt
                .query_map([conv], |row| {
                    let mut cells = Vec::new();
                    for i in 0..10 {
                        cells.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?));
                    }
                    Ok(cells.join("|"))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

fn bind_first_chunk_to_a_witness(engine: &crate::engine::Engine, conv: &str) -> Vec<String> {
    engine
        .storage()
        .with_connection(|conn| {
            conn.execute(
                "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, source_kind)
                     VALUES ('proj', '/repo/a.rs', 'foo', 'b3:1', 'committed', 'test')",
                [],
            )?;
            let witness: i64 = conn.query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))?;
            conn.execute(
                "INSERT INTO witness_chunk_bindings (witness_id, chunk_id)
                     SELECT ?1, id FROM chunks WHERE conversation_id = ?2",
                rusqlite::params![witness, conv],
            )?;
            Ok(())
        })
        .unwrap();
    witness_bindings(engine)
}

fn witness_bindings(engine: &crate::engine::Engine) -> Vec<String> {
    engine
        .storage()
        .with_connection(|conn| {
            let mut stmt =
                conn.prepare("SELECT chunk_id FROM witness_chunk_bindings ORDER BY chunk_id")?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(ids)
        })
        .unwrap()
}

#[derive(Clone, Copy)]
enum Via {
    /// `Engine::import_file`.
    Engine,
    /// The watcher's content-diff import.
    Watcher,
}

async fn import_via(engine: &crate::engine::Engine, rig: &Rig, src: &Path, via: Via) {
    match via {
        Via::Engine => {
            engine.import_file(src, "proj").await.unwrap();
        }
        Via::Watcher => {
            crate::import::watcher::import_file_content_diff(
                engine.storage(),
                engine.embeddings(),
                engine.search(),
                &rig.projects,
                src,
            )
            .await
            .unwrap();
        }
    }
}

/// Rows stored before the scrubber are never touched; everything after them
/// is imported scrubbed and found by search. Includes the original review
/// finding: the scrub would cut the earlier private-key chunks short, and a
/// count-based resume would then skip the new messages.
async fn legacy_rows_survive_and_new_messages_land(via: Via) {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-legacy.jsonl");
    let conv = legacy_conversation(&engine, &src).await;
    let before = stored_rows(&engine, &conv);
    let old_secret_rows = chunks_like(&engine, "Zm9vYmFy");
    let bindings = bind_first_chunk_to_a_witness(&engine, &conv);
    assert_eq!(bindings.len(), before.len());

    let token = format!("{}{}", "ghp_", "Q".repeat(36));
    append(
        &src,
        session_message(10, &format!("freshly appended alpha {token}")).as_bytes(),
    );
    append(
        &src,
        session_message(11, "freshly appended bravo").as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, via).await;

    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert!(after.len() > before.len());
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    assert_eq!(chunks_like(&engine, "freshly appended bravo"), 1);
    assert_eq!(chunks_like(&engine, "ghp_"), 0);
    assert_eq!(chunks_like(&engine, "[secret redacted: github_token]"), 1);
    assert_eq!(chunks_like(&engine, "Zm9vYmFy"), old_secret_rows);
    assert_eq!(witness_bindings(&engine), bindings);
    assert!(engine.storage().legacy_frontier(&conv).unwrap().is_some());
    // Only what the scrub did to the new message is counted, not the
    // secrets the stored rows still hold.
    assert_eq!(engine.storage().get_secrets_redacted().unwrap().0, 1);

    // The next appends land too, and the stored rows stay as they were.
    let long = format!("freshly appended charlie {}", "lorem ipsum ".repeat(120));
    append(&src, session_message(12, &long).as_bytes());
    filetime_touch(&src);
    import_via(&engine, &rig, &src, via).await;
    import_via(&engine, &rig, &src, via).await;
    let later = stored_rows(&engine, &conv);
    for row in &before {
        assert!(later.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(chunks_like(&engine, "freshly appended charlie"), 1);
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    assert_eq!(witness_bindings(&engine), bindings);
}

/// A second handle on the engine's database file: another importer.
fn other_importer(rig: &Rig) -> Storage {
    Storage::open(&rig.data_dir.join("csr-engine.db")).unwrap()
}

fn drop_receipts(engine: &crate::engine::Engine) {
    engine
        .storage()
        .with_connection(|conn| {
            conn.execute("DELETE FROM chunk_spans", [])?;
            Ok(())
        })
        .unwrap();
}

fn frontier_rows(engine: &crate::engine::Engine) -> i64 {
    engine
        .storage()
        .with_connection(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM legacy_import_frontier", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap()
}

fn stamp_of(engine: &crate::engine::Engine, src: &Path) -> Option<String> {
    engine
        .storage()
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT secrets_redacted FROM import_state WHERE file_path = ?1",
                [src.to_string_lossy().to_string()],
                |r| r.get(0),
            )?)
        })
        .unwrap()
}

/// What the importer does to a conversation once it has its resume point:
/// stamp the import state as written by a scrubbing build.
fn finish_import(other: &Storage, src: &Path) {
    other
        .mark_file_imported_with_suppression(src, 0, crate::import::CsrSuppressionStats::default())
        .unwrap();
}

use crate::import::legacy_resume::{resolve, resolve_with, Gap, Resume};
use crate::storage::legacy_frontier::{FrontierCommit, LegacyState};

fn repeated(word: &str) -> String {
    format!("{word} ").repeat(600 / (word.len() + 1))
}

/// Point the receipts of the newest stored chunk of `conv` at message
/// `seq`, as a trust-picked backfill does when the text repeats.
fn point_newest_receipt_at(engine: &crate::engine::Engine, conv: &str, seq: i64) {
    engine
        .storage()
        .with_connection(|conn| {
            conn.execute(
                "UPDATE provenance_events SET seq = ?2 WHERE event_id IN
                     (SELECT s.event_id FROM chunk_spans s WHERE s.chunk_id =
                        (SELECT id FROM chunks WHERE conversation_id = ?1
                         ORDER BY seq DESC, rowid DESC LIMIT 1))",
                rusqlite::params![conv, seq],
            )?;
            Ok(())
        })
        .unwrap();
}

/// Every stored chunk has a receipt, so the receipt branch used to decide.
/// The newest chunk's text also occurs later in the transcript, and the
/// backfill put its receipt at that later copy. The frontier must land where
/// the stored rows end, not at the receipt: the message in between is new.
async fn a_receipt_at_a_later_copy_does_not_move_the_frontier(via: Via) {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-late-receipt.jsonl");
    let (p, q) = (repeated("alphaword"), repeated("bravoword"));
    let conv = legacy_conversation_of(&engine, &src, &[p.clone(), q.clone(), p.clone()]).await;
    let before = stored_rows(&engine, &conv);
    assert_eq!(before.len(), 3);

    // Messages 3 and 4 arrive: something new, then another copy of P.
    append(
        &src,
        session_message(10, "freshly appended alpha").as_bytes(),
    );
    append(&src, session_message(11, &p).as_bytes());
    point_newest_receipt_at(&engine, &conv, 4);
    filetime_touch(&src);
    import_via(&engine, &rig, &src, via).await;

    assert_eq!(
        engine.storage().legacy_frontier(&conv).unwrap(),
        Some((2, 3)),
        "placed where the stored rows end, not at the receipt's later copy"
    );
    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    // Both new messages are short enough to share one new chunk: the one
    // after the receipt's copy was not skipped.
    assert_eq!(after.len(), 4, "the new messages land once, as one chunk");
    assert_eq!(chunks_like(&engine, "alphaword"), 3, "P, P, and the new P");
}

/// `CSR_NO_SECRET_SCRUB` does not switch preservation off: with the scrub
/// off the stored rows are still not rewritten (the last one is short, so a
/// fresh re-chunk would grow it), and the conversation is not stamped as
/// scrub-aware, so turning the scrub back on does not rewrite it either.
async fn legacy_rows_survive_the_scrub_switch(via: Via) {
    let rig = Rig::new();
    let engine = engine_for(&rig).await;
    let src = rig.source("sess-switch.jsonl");
    let pem = format!(
        "{}{}\n{}\n{}{}",
        "-----BEGIN RSA PRIV",
        "ATE KEY-----",
        "Zm9vYmFy".repeat(200),
        "-----END RSA PRIV",
        "ATE KEY-----"
    );
    let conv = legacy_conversation_of(
        &engine,
        &src,
        &[pem.clone(), pem, "a short last message".to_string()],
    )
    .await;
    let before = stored_rows(&engine, &conv);
    assert!(stamp_of(&engine, &src).is_none());

    {
        let _off = crate::redact::DisabledForTest::set(true);
        append(
            &src,
            session_message(10, "freshly appended alpha").as_bytes(),
        );
        filetime_touch(&src);
        import_via(&engine, &rig, &src, via).await;
    }
    let during = stored_rows(&engine, &conv);
    for row in &before {
        assert!(during.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    assert!(engine.storage().legacy_frontier(&conv).unwrap().is_some());
    assert_eq!(
        stamp_of(&engine, &src),
        None,
        "a scrub-off import must not stamp a legacy conversation scrub-aware"
    );

    // The scrub is on again.
    let token = format!("{}{}", "ghp_", "Q".repeat(36));
    append(
        &src,
        session_message(11, &format!("freshly appended bravo {token}")).as_bytes(),
    );
    filetime_touch(&src);
    import_via(&engine, &rig, &src, via).await;
    let after = stored_rows(&engine, &conv);
    for row in &before {
        assert!(after.contains(row), "a stored row changed or went: {row}");
    }
    assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
    assert_eq!(chunks_like(&engine, "ghp_"), 0);
    assert_eq!(chunks_like(&engine, "[secret redacted: github_token]"), 1);
}

/// The import-state check compares mtimes; make sure an append inside the
/// same clock tick still reads as changed.
fn filetime_touch(path: &Path) {
    let later = SystemTime::now() + Duration::from_secs(5);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(later)
        .unwrap();
}

/// A generated JSONL corpus of about `target` bytes: mostly clean lines,
/// about 1 percent with fake secrets, a few with `<private>` spans. Every
/// `noisy_every`-th line (0 = none) also carries words the prefilter
/// trips on without being secrets, which forces the JSON-parse path.
fn bench_corpus(target: usize, noisy_every: usize) -> Vec<u8> {
    const WORDS: [&str; 24] = [
        "the", "function", "returns", "a", "result", "after", "reading", "config", "value", "from",
        "disk", "and", "token", "stream", "of", "bytes", "while", "handling", "errors", "in",
        "the", "parser", "module", "path",
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let github = fake_github();
    let anthropic = format!("{}{}", "sk-ant-", "aB3dE5gH7jK9mN1pQ3rS5tU7".repeat(2));
    let mut corpus = Vec::with_capacity(target + (64 << 10));
    let mut n = 0usize;
    while corpus.len() < target {
        let words = 40 + (next() % 600) as usize;
        let mut text = String::new();
        for _ in 0..words {
            text.push_str(WORDS[(next() % WORDS.len() as u64) as usize]);
            text.push(' ');
        }
        match n % 1000 {
            0..=9 => text.push_str(&format!("key {github} and {anthropic} end")),
            500 | 501 => text.push_str("see <private>the vault code</private> here"),
            _ => {}
        }
        if noisy_every > 0 && n.is_multiple_of(noisy_every) {
            text.push_str("\nTOKEN_BUDGET=high and PASSWORD policy, see sk-learn docs");
        }
        corpus.extend_from_slice(line(n, &text).as_bytes());
        n += 1;
    }
    corpus
}

#[path = "mirror_tests/mixed_tests.rs"]
mod mixed_tests;

#[path = "mirror_tests/legacy_conversation_tests.rs"]
mod legacy_conversation_tests;
