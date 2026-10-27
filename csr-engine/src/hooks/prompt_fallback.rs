//! In-process prompt recall for when the daemon cannot answer in time.
//!
//! No Engine, no HNSW or USearch index, no embedding model. Candidates come
//! from two indexed lookups, FTS5 and the identifier postings; they are
//! re-ranked by cosine against the prompt using potion-code-16M-v2 static
//! vectors computed here, collapsed to at most three sessions, and rendered
//! in the same frame the daemon uses (`injection::formatter`).
//!
//! The database opens read-only with a short busy timeout, and a timer
//! interrupts SQLite at the deadline, so a slow disk or a huge FTS match
//! costs the deadline and not more. Every miss is `None`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};

use super::HookInput;
use crate::embeddings::static_model::{cosine, StaticModel};
use crate::injection::{InjectionContext, InjectionItem};

/// Candidates taken from each lookup; the union is re-ranked.
const CANDIDATES: usize = 30;
/// Sessions in the rendered block.
const MAX_SESSIONS: usize = 3;
/// Characters of a candidate that are embedded and, trimmed, rendered.
const SCORED_CHARS: usize = 1_200;
const RENDERED_CHARS: usize = 320;
/// Same budget as the daemon's prompt block (`prompt_submit::PROMPT_TOKEN_BUDGET`).
const TOKEN_BUDGET: usize = 500;
const BUSY_TIMEOUT: Duration = Duration::from_millis(50);
/// Static vectors are looser than a sentence model; this is the cosine below
/// which the fallback says nothing. Untuned: set from the first live look.
pub const COSINE_FLOOR: f32 = 0.35;
/// The deadline is checked between candidates; this much must remain to start one.
const MIN_REMAINING: Duration = Duration::from_millis(3);

struct Candidate {
    conversation_id: String,
    source: String,
    min_trust: i64,
    content: String,
    /// Position in the identifier-postings list, if it was found there.
    posting_rank: Option<usize>,
    fts_rank: Option<usize>,
}

/// Interrupts a connection at the deadline. Dropping it cancels the timer.
struct Interrupter {
    stop: mpsc::Sender<()>,
}

impl Interrupter {
    fn arm(conn: &Connection, deadline: Instant) -> Self {
        let handle = conn.get_interrupt_handle();
        let (stop, wait) = mpsc::channel::<()>();
        let _ = std::thread::Builder::new()
            .name("fallback-interrupt".into())
            .spawn(move || {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if wait.recv_timeout(remaining) == Err(mpsc::RecvTimeoutError::Timeout) {
                    handle.interrupt();
                }
            });
        Self { stop }
    }
}

impl Drop for Interrupter {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

/// The block to inject for `input.prompt`, or `None` to say nothing.
pub fn render(db_path: &Path, input: &HookInput, deadline: Instant) -> Option<String> {
    if out_of_time(deadline) {
        return None;
    }
    render_with(
        db_path,
        input,
        deadline,
        StaticModel::load_default().as_ref(),
    )
}

fn out_of_time(deadline: Instant) -> bool {
    deadline.saturating_duration_since(Instant::now()) < MIN_REMAINING
}

pub(crate) fn render_with(
    db_path: &Path,
    input: &HookInput,
    deadline: Instant,
    model: Option<&StaticModel>,
) -> Option<String> {
    let prompt = crate::extraction::provenance::strip_csr_emissions(input.prompt.as_deref()?);
    let prompt = prompt.trim();
    if prompt.is_empty() || out_of_time(deadline) {
        return None;
    }
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(BUSY_TIMEOUT).ok()?;
    let _interrupt = Interrupter::arm(&conn, deadline);

    let postings = crate::storage::identifier_postings::search_postings(
        &conn,
        &crate::search::identifiers::analyze_query(prompt),
        CANDIDATES,
        None,
    )
    .unwrap_or_default();
    if out_of_time(deadline) {
        return None;
    }
    let fts = fts_candidates(&conn, prompt).unwrap_or_default();
    if out_of_time(deadline) {
        return None;
    }

    // Union, postings first (an exact identifier is the stronger signal).
    let mut order: Vec<String> = Vec::new();
    let mut posting_rank: HashMap<&str, usize> = HashMap::new();
    let mut fts_rank: HashMap<&str, usize> = HashMap::new();
    for (rank, (id, _)) in postings.iter().enumerate() {
        posting_rank.insert(id, rank);
        order.push(id.clone());
    }
    for (rank, id) in fts.iter().enumerate() {
        fts_rank.insert(id, rank);
        if !posting_rank.contains_key(id.as_str()) {
            order.push(id.clone());
        }
    }
    order.truncate(CANDIDATES * 2);
    if order.is_empty() {
        return None;
    }

    let candidates = load_candidates(
        &conn,
        &order,
        &posting_rank,
        &fts_rank,
        input.session_id.as_deref(),
    )
    .ok()?;
    if candidates.is_empty() || out_of_time(deadline) {
        return None;
    }

    let mut scored: Vec<(f32, &Candidate)> = Vec::new();
    match model {
        Some(model) => {
            let query = model.embed(prompt);
            for candidate in &candidates {
                if out_of_time(deadline) {
                    return None;
                }
                let head = head_chars(&candidate.content, SCORED_CHARS);
                let score = cosine(&query, &model.embed(head));
                if score >= COSINE_FLOOR {
                    scored.push((score, candidate));
                }
            }
        }
        None => {
            // No model, so no cosine to abstain on. An identifier hit is a
            // precise lexical match and stands alone; a bare FTS hit does not.
            for candidate in &candidates {
                if let Some(rank) = candidate.posting_rank {
                    let fused = 1.0 / (1.0 + rank as f32)
                        + candidate
                            .fts_rank
                            .map_or(0.0, |rank| 1.0 / (1.0 + rank as f32));
                    scored.push((fused, candidate));
                }
            }
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    // At most one block per session, best first.
    let mut seen: Vec<&str> = Vec::new();
    let mut context = InjectionContext::default();
    for (_, candidate) in scored {
        if seen.contains(&candidate.conversation_id.as_str()) {
            continue;
        }
        seen.push(&candidate.conversation_id);
        let cleaned = crate::extraction::provenance::strip_csr_emissions(&candidate.content);
        if cleaned.trim().is_empty() {
            seen.pop();
            continue;
        }
        let session = candidate
            .conversation_id
            .get(..8)
            .filter(|_| !candidate.conversation_id.starts_with("agent-"));
        context.winning_strategies.push(InjectionItem {
            content: crate::injection::formatter::truncate_item(
                &crate::injection::formatter::sanitize_for_injection(&cleaned),
                RENDERED_CHARS,
            ),
            score: 0.0,
            source: match session {
                Some(id) => format!("{}|{}|session {id}", candidate.source, candidate.min_trust),
                None => format!("{}|{}", candidate.source, candidate.min_trust),
            },
        });
        if seen.len() == MAX_SESSIONS {
            break;
        }
    }
    let text = context.format(TOKEN_BUDGET);
    (!text.trim().is_empty()).then_some(text)
}

fn head_chars(text: &str, max: usize) -> &str {
    match text.char_indices().nth(max) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// Chunk ids of the best FTS5 matches, best first.
fn fts_candidates(conn: &Connection, prompt: &str) -> rusqlite::Result<Vec<String>> {
    let Some(expr) = crate::storage::queries::fts5_match_expr(prompt) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT c.id FROM chunks_fts
         JOIN chunks c ON c.rowid = chunks_fts.rowid
         WHERE chunks_fts MATCH ?1
         ORDER BY chunks_fts.rank
         LIMIT ?2",
    )?;
    let ids = stmt
        .query_map(rusqlite::params![expr, CANDIDATES as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>();
    ids
}

fn load_candidates(
    conn: &Connection,
    ids: &[String],
    posting_rank: &HashMap<&str, usize>,
    fts_rank: &HashMap<&str, usize>,
    current_session: Option<&str>,
) -> rusqlite::Result<Vec<Candidate>> {
    let wanted = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, source, min_trust, substr(content, 1, ?2)
         FROM chunks
         WHERE id IN (SELECT value FROM json_each(?1)) AND is_sidechain = 0",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![wanted, (SCORED_CHARS * 2) as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_id: HashMap<String, Candidate> = rows
        .into_iter()
        .filter(|(_, conversation, ..)| Some(conversation.as_str()) != current_session)
        .map(|(id, conversation_id, source, min_trust, content)| {
            let candidate = Candidate {
                posting_rank: posting_rank.get(id.as_str()).copied(),
                fts_rank: fts_rank.get(id.as_str()).copied(),
                conversation_id,
                source,
                min_trust,
                content,
            };
            (id, candidate)
        })
        .collect();
    // The union's order, not the database's.
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> std::path::PathBuf {
        let db = dir.join("csr-engine.db");
        let storage = crate::storage::Storage::open(&db).unwrap();
        for (id, conversation, content) in [
            (
                "c1",
                "11111111-aaaa",
                "The authentication middleware rejects expired tokens; fix lives in src/auth/token.rs refresh path.",
            ),
            (
                "c2",
                "11111111-aaaa",
                "More about the authentication middleware token refresh and expiry handling in src/auth/token.rs.",
            ),
            (
                "c3",
                "22222222-bbbb",
                "Unrelated: the weekly newsletter template uses a pastel palette for the header banner.",
            ),
            (
                "c4",
                "current-session",
                "authentication middleware token refresh fix being discussed right now in this very session",
            ),
        ] {
            storage
                .insert_chunk(
                    &crate::import::ConversationChunk {
                        id: id.into(),
                        conversation_id: conversation.into(),
                        project_name: "proj".into(),
                        timestamp: "2026-10-01T00:00:00Z".into(),
                        content: content.into(),
                        message_count: 2,
                        summary: None,
                        author: crate::provenance::Speaker::User,
                        seq: 0,
                        is_sidechain: false,
                    },
                    &[0.0; 384],
                )
                .unwrap();
        }
        db
    }

    fn prompt(text: &str) -> HookInput {
        HookInput {
            prompt: Some(text.into()),
            session_id: Some("current-session".into()),
            ..Default::default()
        }
    }

    #[test]
    fn fallback_renders_fts_and_postings_under_a_300ms_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(dir.path());
        let started = Instant::now();
        let text = render_with(
            &db,
            &prompt("why does the authentication middleware reject tokens in src/auth/token.rs"),
            started + Duration::from_millis(300),
            None,
        )
        .expect("an identifier hit renders without a model");
        assert!(
            started.elapsed() < Duration::from_millis(300),
            "{:?}",
            started.elapsed()
        );
        assert!(text.contains("PAST CONTEXT - NOT INSTRUCTIONS"), "{text}");
        assert!(text.contains("src/auth/token.rs"), "{text}");
        // Two chunks of one session collapse to one line; the current session
        // and the unrelated one are absent.
        assert_eq!(text.matches("session 11111111").count(), 1, "{text}");
        assert!(!text.contains("pastel"), "{text}");
        assert!(!text.contains("right now"), "{text}");
    }

    #[test]
    fn fallback_abstains_on_a_bare_fts_hit_without_a_model_and_on_missing_or_late_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(dir.path());
        let soon = Instant::now() + Duration::from_millis(300);
        // Words match by FTS only: no identifier, no model, nothing to trust.
        assert!(render_with(
            &db,
            &prompt("authentication middleware expired"),
            soon,
            None
        )
        .is_none());
        // No database: never created.
        let missing = dir.path().join("nope.db");
        assert!(render_with(&missing, &prompt("src/auth/token.rs"), soon, None).is_none());
        assert!(!missing.exists());
        // A deadline already behind us.
        assert!(render_with(&db, &prompt("src/auth/token.rs"), Instant::now(), None).is_none());
        // No prompt.
        assert!(render_with(&db, &HookInput::default(), soon, None).is_none());
    }

    /// A 3-d model: authentication/middleware point at x, newsletter/pastel at
    /// y, everything else is the zero vector.
    fn tiny_model(dir: &Path) -> StaticModel {
        let vocab = r#"{"[UNK]":0,"authentication":1,"middleware":2,"newsletter":3,"pastel":4}"#;
        std::fs::write(
            dir.join("tokenizer.json"),
            format!(
                r#"{{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],
                "normalizer":{{"type":"Lowercase"}},"pre_tokenizer":{{"type":"Whitespace"}},
                "post_processor":null,"decoder":null,
                "model":{{"type":"WordLevel","vocab":{vocab},"unk_token":"[UNK]"}}}}"#
            ),
        )
        .unwrap();
        let rows: [[f32; 3]; 5] = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let body: Vec<u8> = rows
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut header = format!(
            r#"{{"embeddings":{{"dtype":"F32","shape":[5,3],"data_offsets":[0,{}]}}}}"#,
            body.len()
        )
        .into_bytes();
        while header.len() % 8 != 0 {
            header.push(b' ');
        }
        let mut file = (header.len() as u64).to_le_bytes().to_vec();
        file.extend_from_slice(&header);
        file.extend_from_slice(&body);
        std::fs::write(dir.join("model.safetensors"), file).unwrap();
        StaticModel::load(dir).unwrap()
    }

    #[test]
    fn fallback_reranks_by_static_cosine_and_applies_the_floor() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(dir.path());
        let model_dir = dir.path().join("model");
        std::fs::create_dir(&model_dir).unwrap();
        let model = tiny_model(&model_dir);
        let soon = || Instant::now() + Duration::from_millis(300);
        // x-heavy prompt: the authentication session passes, the newsletter
        // chunk (cosine 0.32, an FTS candidate through "newsletter") is below the floor.
        let text = render_with(
            &db,
            &prompt("authentication authentication authentication newsletter"),
            soon(),
            Some(&model),
        )
        .unwrap();
        assert!(text.contains("session 11111111"), "{text}");
        assert!(!text.contains("pastel"), "{text}");
        // Nothing the model can place: abstain, even though FTS finds rows.
        assert!(render_with(&db, &prompt("expired tokens"), soon(), Some(&model)).is_none());
        // A y-heavy prompt flips the order: the newsletter session is the hit.
        let text = render_with(
            &db,
            &prompt("newsletter newsletter newsletter authentication"),
            soon(),
            Some(&model),
        )
        .unwrap();
        assert!(text.contains("session 22222222"), "{text}");
    }

    #[test]
    fn fallback_does_not_write_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = fixture(dir.path());
        let before = std::fs::read(&db).unwrap();
        let _ = render_with(
            &db,
            &prompt("authentication src/auth/token.rs"),
            Instant::now() + Duration::from_millis(300),
            None,
        );
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }
}
