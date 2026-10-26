//! Episode channel of recall: bm25 over `episodes_fts` as one more lexical
//! channel, fused with the chunk channels at session level. A session's
//! standing in a channel is its single best unit (max, never a sum); a unit is
//! a chunk or an episode. `CSR_NO_EPISODES=1`, or an empty `episodes` table,
//! switches the channel off and recall is exactly the chunk-only path.

use rusqlite::{params_from_iter, types::Value, Connection};

use crate::episodes::store::has_episodes;
use crate::search::hybrid::collapse_to_sessions_by;
use crate::storage::Storage;

/// Episodes fetched from the FTS channel.
pub const EPISODE_POOL: usize = 100;

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
    let rows = stmt.query_map(params_from_iter(params), |row| {
        Ok(EpisodeHit {
            id: row.get(0)?,
            session: row.get(1)?,
            project: row.get(2)?,
            timestamp: row.get(3)?,
            text: row.get(4)?,
            echo_class: row.get::<_, i64>(5)?.clamp(0, 2) as u8,
        })
    })?;
    rows.collect()
}

/// The episode channel for a query: `None` when it is off (kill switch or an
/// empty table), else the top [`EPISODE_POOL`] hits, which may be none. A
/// failing query reads as no hits.
pub fn gather(storage: &Storage, query: &str, family: Option<&[&str]>) -> Option<Vec<EpisodeHit>> {
    if !channel_active(storage) {
        return None;
    }
    Some(
        storage
            .with_connection(|conn| Ok(episode_hits(conn, query, EPISODE_POOL, family)?))
            .unwrap_or_default(),
    )
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
}
