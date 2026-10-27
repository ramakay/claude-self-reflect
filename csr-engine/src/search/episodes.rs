//! Episode channel of recall: bm25 over `episodes_fts` fused with a dense
//! nearest-neighbour list over the episode vector generation, the pair merged
//! by RRF into one list, then fused with the chunk channels at session level.
//! A session's standing in a channel is its single best unit (max, never a
//! sum); a unit is a chunk or an episode. `CSR_NO_EPISODES=1`, or an empty
//! `episodes` table, switches the channel off and recall is exactly the
//! chunk-only path. `CSR_NO_EPISODE_VECTORS=1`, no episode watermark, a
//! missing model file or a failed embed leaves the lexical list alone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use rusqlite::{params_from_iter, types::Value, Connection};

use crate::embeddings::onnx_embed::{Kind, OnnxEmbedder};
use crate::episodes::store::has_episodes;
use crate::index_gen::{GenManifest, Generation};
use crate::search::hybrid::{collapse_to_sessions_by, fuse_rrf_weighted, RRF_K};
use crate::storage::Storage;

/// Episodes fetched from the FTS channel.
pub const EPISODE_POOL: usize = 100;

/// Neighbours fetched from the episode vector generation.
pub const DENSE_POOL: usize = 24;

/// Model folder under the model root and graph file inside it: the Granite
/// fp32 graph `embed sample` and `embed query-bench --model granite-fp32` use.
const MODEL_DIR: &str = "granite-embedding-97m-multilingual-r2";
const MODEL_FILE: &str = "onnx/model.onnx";

/// Prefix of the `summary` of a result row whose excerpt is an episode; the
/// rest is the episode id. The result page reads it to print `eid`.
pub const EID_MARK: &str = "episode-excerpt:";

/// One episode reached by the keyword query.
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodeHit {
    pub id: String,
    /// The conversation the episode belongs to: its session.
    pub session: String,
    pub project: String,
    pub timestamp: String,
    pub text: String,
    /// Standing of the episode as an answer: 0 answer, 1 question echo,
    /// 2 CSR's own output. The same scale as `non_answer_class` for chunks.
    pub echo_class: u8,
}

/// Serializes tests that set `CSR_NO_EPISODES` against tests that need the
/// channel on.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Kill switch: `CSR_NO_EPISODES` set to anything but empty or `0`.
pub fn disabled() -> bool {
    std::env::var("CSR_NO_EPISODES").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Kill switch for the dense half: `CSR_NO_EPISODE_VECTORS` set to anything
/// but empty or `0`. The channel is then exactly the lexical one.
pub fn dense_disabled() -> bool {
    std::env::var("CSR_NO_EPISODE_VECTORS").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Whether the episode channel runs: not switched off and the table holds rows.
pub fn channel_active(storage: &Storage) -> bool {
    !disabled()
        && storage
            .with_connection(|conn| Ok(has_episodes(conn)))
            .unwrap_or(false)
}

/// Episode FTS hits for `query`, best bm25 first, at most `limit`. The match
/// expression is the one `keyword_chunk_hits` uses, so a query sanitizes the
/// same way in both channels. `family` restricts to those projects.
pub fn episode_hits(
    conn: &Connection,
    query: &str,
    limit: usize,
    family: Option<&[&str]>,
) -> rusqlite::Result<Vec<EpisodeHit>> {
    let Some(expr) = crate::storage::queries::fts5_match_expr(query) else {
        return Ok(Vec::new());
    };
    let mut params: Vec<Value> = vec![Value::Text(expr)];
    let scope = match family {
        Some(projects) if !projects.is_empty() => {
            let marks = vec!["?"; projects.len()].join(",");
            params.extend(projects.iter().map(|p| Value::Text((*p).to_string())));
            format!("AND e.project_name IN ({marks})")
        }
        _ => String::new(),
    };
    params.push(Value::Integer(limit as i64));
    let sql = format!(
        "SELECT e.id, e.conversation_id, e.project_name, e.timestamp, e.text, e.echo_class
         FROM episodes e
         JOIN episodes_fts f ON f.rowid = e.rowid
         WHERE episodes_fts MATCH ? {scope}
         ORDER BY f.rank, e.id
         LIMIT ?"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), hit_of_row)?;
    rows.collect()
}

fn hit_of_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EpisodeHit> {
    Ok(EpisodeHit {
        id: row.get(0)?,
        session: row.get(1)?,
        project: row.get(2)?,
        timestamp: row.get(3)?,
        text: row.get(4)?,
        echo_class: row.get::<_, i64>(5)?.clamp(0, 2) as u8,
    })
}

/// The episodes named by `ids` that pass the same project-family scope as
/// [`episode_hits`], in no particular order. An id with no live episode (a
/// stale vector) is simply absent.
pub fn episodes_by_id(
    conn: &Connection,
    ids: &[String],
    family: Option<&[&str]>,
) -> rusqlite::Result<Vec<EpisodeHit>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut params: Vec<Value> = ids.iter().map(|i| Value::Text(i.clone())).collect();
    let marks = vec!["?"; ids.len()].join(",");
    let scope = match family {
        Some(projects) if !projects.is_empty() => {
            let pm = vec!["?"; projects.len()].join(",");
            params.extend(projects.iter().map(|p| Value::Text((*p).to_string())));
            format!("AND e.project_name IN ({pm})")
        }
        _ => String::new(),
    };
    let sql = format!(
        "SELECT e.id, e.conversation_id, e.project_name, e.timestamp, e.text, e.echo_class
         FROM episodes e
         WHERE e.id IN ({marks}) {scope}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), hit_of_row)?;
    rows.collect()
}

// ------------------------------------------------------------ dense half

static EMBEDDER: OnceLock<Result<OnnxEmbedder, String>> = OnceLock::new();
type CachedGeneration = Option<(PathBuf, GenManifest, Arc<Generation>)>;
static GENERATION: Mutex<CachedGeneration> = Mutex::new(None);
static LAST_REPORT: Mutex<Option<String>> = Mutex::new(None);

/// One stderr line per distinct state of the dense half per process. Never
/// stdout: that is the JSON-RPC channel.
fn report(line: String) {
    let mut last = LAST_REPORT.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_deref() != Some(line.as_str()) {
        eprintln!("{line}");
        *last = Some(line);
    }
}

fn report_off(reason: &str) {
    report(format!("episode-vectors: off ({reason})"));
}

/// `<db parent>/index-v3`, from the open connection's main database file.
fn index_dir_of(storage: &Storage) -> Option<PathBuf> {
    let file: String = storage
        .with_connection(|conn| {
            Ok(conn.query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .ok()?;
    let db = PathBuf::from(file);
    db.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.join("index-v3"))
}

/// The generation the watermark names, opened once per manifest.
fn cached_generation(index_dir: PathBuf, manifest: GenManifest) -> Result<Arc<Generation>, String> {
    let mut slot = GENERATION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((dir, cached, generation)) = slot.as_ref() {
        if *dir == index_dir && *cached == manifest {
            return Ok(Arc::clone(generation));
        }
    }
    let generation = Arc::new(
        crate::index_gen::open_episode_generation(&index_dir, &manifest)
            .map_err(|e| format!("open episode generation: {e:#}"))?,
    );
    *slot = Some((index_dir, manifest, Arc::clone(&generation)));
    Ok(generation)
}

/// The Granite fp32 embedder, loaded once per process.
fn embedder() -> Result<&'static OnnxEmbedder, String> {
    EMBEDDER
        .get_or_init(|| {
            let dir = crate::eval::embed_sample::default_model_root().join(MODEL_DIR);
            if !dir.join(MODEL_FILE).is_file() {
                return Err(format!(
                    "model file missing: {}",
                    dir.join(MODEL_FILE).display()
                ));
            }
            OnnxEmbedder::load(&dir, MODEL_FILE, 0).map_err(|e| format!("model load: {e:#}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Ids of the [`DENSE_POOL`] nearest episodes to `query`, best first. `Err`
/// carries the reason the dense half is off; the caller fails open.
fn dense_ids(storage: &Storage, query: &str) -> Result<Vec<String>, String> {
    let manifest = crate::index_gen::read_episode_watermark(storage)
        .map_err(|e| format!("episode watermark unreadable: {e:#}"))?
        .ok_or_else(|| "no episode watermark".to_string())?;
    let index_dir = index_dir_of(storage).ok_or_else(|| "no index dir".to_string())?;
    let generation = cached_generation(index_dir, manifest)?;
    let embedder = embedder()?;
    let mut vectors = embedder
        .embed(&[query], Kind::Query)
        .map_err(|e| format!("embed failed: {e:#}"))?;
    let vector = vectors
        .pop()
        .ok_or_else(|| "embed returned no vector".to_string())?;
    let manifest = generation.manifest();
    report(format!(
        "episode-vectors: generation={} count={}",
        manifest.generation, manifest.count
    ));
    Ok(generation
        .search(&vector, DENSE_POOL)
        .into_iter()
        .map(|(id, _)| id)
        .collect())
}

/// RRF (k=60) of the lexical list and the dense list into one hit list, at
/// most [`EPISODE_POOL`] long. A dense id with no hit (out of scope or stale)
/// is already gone from `dense`; every id keeps the hit it was fetched as.
fn merge_lists(lexical: Vec<EpisodeHit>, dense: Vec<EpisodeHit>) -> Vec<EpisodeHit> {
    if dense.is_empty() {
        return lexical;
    }
    let lexical_ids: Vec<String> = lexical.iter().map(|h| h.id.clone()).collect();
    let dense_ids: Vec<String> = dense.iter().map(|h| h.id.clone()).collect();
    let mut by_id: HashMap<String, EpisodeHit> = HashMap::new();
    for hit in lexical.into_iter().chain(dense) {
        by_id.entry(hit.id.clone()).or_insert(hit);
    }
    fuse_rrf_weighted(&[(&lexical_ids, 1.0), (&dense_ids, 1.0)], RRF_K)
        .into_iter()
        .take(EPISODE_POOL)
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

/// The episode channel for a query: `None` when it is off (kill switch or an
/// empty table), else the top [`EPISODE_POOL`] hits, which may be none. A
/// failing query reads as no hits. The list is the bm25 hits merged by RRF with
/// the dense neighbours of the query, unless the dense half is off (kill switch
/// or any failure), where it is the bm25 list alone.
pub fn gather(storage: &Storage, query: &str, family: Option<&[&str]>) -> Option<Vec<EpisodeHit>> {
    gather_with(storage, query, family, dense_ids)
}

fn gather_with(
    storage: &Storage,
    query: &str,
    family: Option<&[&str]>,
    dense: impl FnOnce(&Storage, &str) -> Result<Vec<String>, String>,
) -> Option<Vec<EpisodeHit>> {
    if !channel_active(storage) {
        return None;
    }
    let lexical = storage
        .with_connection(|conn| Ok(episode_hits(conn, query, EPISODE_POOL, family)?))
        .unwrap_or_default();
    if dense_disabled() {
        report_off("CSR_NO_EPISODE_VECTORS=1");
        return Some(lexical);
    }
    if query.trim().is_empty() {
        return Some(lexical);
    }
    let ids = match dense(storage, query) {
        Ok(ids) => ids,
        Err(reason) => {
            report_off(&reason);
            return Some(lexical);
        }
    };
    let mut fetched: HashMap<String, EpisodeHit> = storage
        .with_connection(|conn| Ok(episodes_by_id(conn, &ids, family)?))
        .unwrap_or_default()
        .into_iter()
        .map(|h| (h.id.clone(), h))
        .collect();
    let dense_hits: Vec<EpisodeHit> = ids.iter().filter_map(|id| fetched.remove(id)).collect();
    Some(merge_lists(lexical, dense_hits))
}

/// An episode's text under the preview cap, cut on a character boundary.
pub fn excerpt(text: &str) -> String {
    let cap = crate::format::PREVIEW_CHARS;
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let cut: String = text.chars().take(cap).collect();
    format!("{cut}...")
}

/// Session-level fusion over units (chunks and episodes alike): each channel
/// ranks a session by its best unit, the session scores the weighted sum of
/// those per-channel maxima, and a session is represented by its unit with
/// the best fused unit score. `class_of` is the standing of a unit: only a
/// session's lowest-class units count, so an echo cannot lift a session.
pub fn fuse_units(
    channels: &[(&[String], f64)],
    session_of: impl Fn(&str) -> String,
    class_of: impl Fn(&str) -> u8,
    k: usize,
) -> Vec<String> {
    collapse_to_sessions_by(channels, session_of, class_of, k)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::hybrid::RRF_K;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn session_of(id: &str) -> String {
        // "a-1" and "ep:a:0" both belong to session "a".
        let id = id.strip_prefix("ep:").unwrap_or(id);
        id.split(['-', ':']).next().unwrap().to_string()
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::migrations::run(&conn).unwrap();
        conn
    }

    fn put(conn: &Connection, conv: &str, ordinal: i64, text: &str, echo: i64) {
        conn.execute(
            "INSERT INTO episodes (id, conversation_id, project_name, ordinal, first_uuid,
                 last_uuid, timestamp, text, token_est, echo_class)
             VALUES (?1, ?2, 'p', ?3, 'f', 'l', '2026-10-01T00:00:00Z', ?4, 10, ?5)",
            rusqlite::params![format!("ep:{conv}:{ordinal}"), conv, ordinal, text, echo],
        )
        .unwrap();
    }

    #[test]
    fn session_score_is_max_not_sum() {
        // Session "w" has five weak units behind the single stronger unit of
        // "s" in the one channel that holds them: summed, w would win.
        let channel = ids(&["s-1", "w-1", "w-2", "w-3", "w-4", "w-5"]);
        let order = fuse_units(&[(&channel, 1.0)], session_of, |_| 0, RRF_K);
        assert_eq!(order.len(), 2);
        assert_eq!(session_of(&order[0]), "s");
    }

    #[test]
    fn episode_hit_lifts_right_session() {
        // The answer is split across two chunks of "a" (neither strong in any
        // chunk channel) while "b" holds one mid-ranked chunk in two channels.
        let semantic = ids(&["b-1", "x-1", "y-1", "a-1"]);
        let keyword = ids(&["b-1", "z-1", "a-2"]);
        let without = fuse_units(
            &[(&semantic, 1.0), (&keyword, 1.0)],
            session_of,
            |_| 0,
            RRF_K,
        );
        assert_eq!(session_of(&without[0]), "b");
        // One episode of "a" holds both halves and tops the episode channel.
        let episodes = ids(&["ep:a:0"]);
        let with = fuse_units(
            &[(&semantic, 1.0), (&keyword, 1.0), (&episodes, 1.0)],
            session_of,
            |_| 0,
            RRF_K,
        );
        assert_eq!(session_of(&with[0]), "a");
    }

    #[test]
    fn episode_echo_cannot_lift_a_session() {
        let chunks = ids(&["b-1"]);
        let episodes = ids(&["ep:a:0", "ep:a:1"]);
        let class = |id: &str| u8::from(id.starts_with("ep:a:0"));
        let order = fuse_units(
            &[(&chunks, 1.0), (&episodes, 1.0)],
            session_of,
            class,
            RRF_K,
        );
        // "a" is represented by the answer episode (class 0) at its own rank 1,
        // not by the echo at rank 0.
        let a = order.iter().find(|id| session_of(id) == "a").unwrap();
        assert_eq!(a, "ep:a:1");
        assert_eq!(session_of(&order[0]), "b");
    }

    #[test]
    fn episodes_fts_hits_rank_by_bm25_and_filter_by_project() {
        let conn = db();
        put(
            &conn,
            "a",
            0,
            "the widget parser failed on nested braces",
            0,
        );
        put(&conn, "b", 0, "unrelated lunch plans", 0);
        put(&conn, "c", 0, "widget widget widget parser parser", 2);
        let hits = episode_hits(&conn, "widget parser", 10, None).unwrap();
        let sessions: Vec<&str> = hits.iter().map(|h| h.session.as_str()).collect();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"a") && sessions.contains(&"c"));
        let c = hits.iter().find(|h| h.session == "c").unwrap();
        assert_eq!(c.echo_class, 2);
        assert!(episode_hits(&conn, "widget", 10, Some(&["other"]))
            .unwrap()
            .is_empty());
        assert_eq!(
            episode_hits(&conn, "widget", 10, Some(&["p"]))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(episode_hits(&conn, "widget", 1, None).unwrap().len(), 1);
        assert!(episode_hits(&conn, "!!! ???", 10, None).unwrap().is_empty());
    }

    #[test]
    fn excerpt_is_capped_on_a_char_boundary() {
        let long = "é".repeat(crate::format::PREVIEW_CHARS + 20);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), crate::format::PREVIEW_CHARS + 3);
        assert_eq!(excerpt("short"), "short");
    }

    #[test]
    fn kill_switch_values() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CSR_NO_EPISODES", "1");
        assert!(disabled());
        std::env::set_var("CSR_NO_EPISODES", "0");
        assert!(!disabled());
        std::env::remove_var("CSR_NO_EPISODES");
        assert!(!disabled());
    }

    fn memory_with(rows: &[(&str, i64, &str, i64)]) -> Storage {
        let storage = Storage::open_memory().unwrap();
        storage
            .with_connection(|conn| {
                for (conv, ordinal, text, echo) in rows {
                    put(conn, conv, *ordinal, text, *echo);
                }
                Ok(())
            })
            .unwrap();
        storage
    }

    #[test]
    fn dense_only_hit_lifts_right_session() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_EPISODES");
        std::env::remove_var("CSR_NO_EPISODE_VECTORS");
        let storage = memory_with(&[
            ("a", 0, "the answer split across two halves", 0),
            ("b", 0, "unrelated lunch plans", 0),
        ]);
        // No word of the query is in any episode: the lexical list is empty and
        // only the dense neighbour of "a" reaches the channel.
        let query = "zzzquux";
        let lexical = gather_with(&storage, query, None, |_, _| Ok(Vec::new())).unwrap();
        assert!(lexical.is_empty());
        let hits = gather_with(&storage, query, None, |_, _| {
            Ok(vec!["ep:a:0".to_string(), "ep:gone:0".to_string()])
        })
        .unwrap();
        assert_eq!(hits.len(), 1, "a stale dense id drops out");
        assert_eq!(hits[0].id, "ep:a:0");
        let semantic = ids(&["b-1", "x-1", "y-1", "a-1"]);
        let keyword = ids(&["b-1", "z-1", "a-2"]);
        let episodes: Vec<String> = hits.iter().map(|h| h.id.clone()).collect();
        let order = fuse_units(
            &[(&semantic, 1.0), (&keyword, 1.0), (&episodes, 1.0)],
            session_of,
            |_| 0,
            RRF_K,
        );
        assert_eq!(session_of(&order[0]), "a");
    }

    #[test]
    fn dense_hits_respect_family_scope_and_merge_by_rrf() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_EPISODES");
        std::env::remove_var("CSR_NO_EPISODE_VECTORS");
        let storage = memory_with(&[
            ("a", 0, "widget parser failed", 0),
            ("c", 0, "dense neighbour only", 2),
        ]);
        let all = gather_with(&storage, "widget", None, |_, _| {
            Ok(vec!["ep:c:0".to_string(), "ep:a:0".to_string()])
        })
        .unwrap();
        // "a" is in both lists, so it outranks the dense-only "c".
        let order: Vec<&str> = all.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(order, ["ep:a:0", "ep:c:0"]);
        assert_eq!(all[1].echo_class, 2);
        let scoped = gather_with(&storage, "widget", Some(&["other"]), |_, _| {
            Ok(vec!["ep:c:0".to_string()])
        })
        .unwrap();
        assert!(scoped.is_empty());
    }

    #[test]
    fn dense_kill_switch_matches_lexical_only() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_EPISODES");
        let storage = memory_with(&[
            ("a", 0, "widget parser failed", 0),
            ("c", 0, "dense neighbour only", 0),
        ]);
        let lexical = storage
            .with_connection(|conn| Ok(episode_hits(conn, "widget", EPISODE_POOL, None)?))
            .unwrap();
        std::env::set_var("CSR_NO_EPISODE_VECTORS", "1");
        assert!(dense_disabled());
        let got = gather_with(&storage, "widget", None, |_, _| {
            panic!("the dense half must not run under the kill switch")
        });
        std::env::remove_var("CSR_NO_EPISODE_VECTORS");
        assert_eq!(got, Some(lexical));
        std::env::set_var("CSR_NO_EPISODE_VECTORS", "0");
        assert!(!dense_disabled());
        std::env::remove_var("CSR_NO_EPISODE_VECTORS");
        assert!(!dense_disabled());
    }

    #[test]
    fn no_watermark_fails_open_to_lexical() {
        let _guard = ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("CSR_NO_EPISODES");
        std::env::remove_var("CSR_NO_EPISODE_VECTORS");
        let storage = memory_with(&[
            ("a", 0, "widget parser failed", 0),
            ("b", 0, "unrelated lunch plans", 0),
        ]);
        let lexical = storage
            .with_connection(|conn| Ok(episode_hits(conn, "widget", EPISODE_POOL, None)?))
            .unwrap();
        assert_eq!(lexical.len(), 1);
        // The real dense path: an in-memory store has no episode watermark.
        assert_eq!(
            dense_ids(&storage, "widget").unwrap_err(),
            "no episode watermark"
        );
        assert_eq!(gather(&storage, "widget", None), Some(lexical.clone()));
        // A failing dense half reads the same.
        let failed = gather_with(&storage, "widget", None, |_, _| Err("embed failed".into()));
        assert_eq!(failed, Some(lexical));
    }
}
