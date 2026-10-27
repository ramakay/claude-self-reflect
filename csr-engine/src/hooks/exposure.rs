//! Best-effort exposure logging shared by injection hooks.

use crate::injection::formatter::{FormattedInjection, InjectionCategory};
use crate::storage::trained_rerank::{ExposureImpression, ExposureItem};
use crate::storage::Storage;

pub const FEATURE_SCHEMA: i64 = crate::search::trained_rerank::FEATURE_SCHEMA;

/// Map a formatter receipt back to the parallel stable-memory metadata kept by
/// the hook. Items without a stable ID are omitted, and persisted ranks are
/// dense output ranks rather than pre-budget candidate positions.
pub fn rendered_exposure_items(
    receipt: &FormattedInjection,
    anti_patterns: &[Option<ExposureItem>],
    relevant_context: &[Option<ExposureItem>],
    winning_strategies: &[Option<ExposureItem>],
) -> Vec<ExposureItem> {
    receipt
        .rendered_items
        .iter()
        .filter_map(|rendered| {
            let source = match rendered.category {
                InjectionCategory::AntiPattern => anti_patterns,
                InjectionCategory::RelevantContext => relevant_context,
                InjectionCategory::WinningStrategy => winning_strategies,
                InjectionCategory::ErrorMatch | InjectionCategory::IterationLearning => &[],
            };
            source.get(rendered.index).and_then(Clone::clone)
        })
        .enumerate()
        .map(|(rank, mut item)| {
            item.rank = rank as i64;
            item
        })
        .collect()
}

pub fn query_hash(query: &str) -> String {
    blake3::hash(query.as_bytes()).to_hex().to_string()
}

/// Side table that keys an exposure impression by the `prompt_id` of the
/// prompt it answered. It is separate from `rerank_exposure_impressions` so
/// that table's writer and schema stay untouched; the audit joins the two on
/// `impression_id`. Created here, on first use, by [`ensure_prompt_key_table`].
const PROMPT_KEY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS exposure_prompt_keys (
        impression_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        prompt_id TEXT NOT NULL,
        shown_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_exposure_prompt_keys_prompt
        ON exposure_prompt_keys(session_id, prompt_id);";

/// Create the prompt-key table and its index if they are missing. Idempotent.
pub fn ensure_prompt_key_table(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(PROMPT_KEY_SCHEMA)
}

/// Key one stored impression by its prompt, without ever waiting: the write
/// runs at a zero busy timeout and a busy database drops the key, as the
/// impression write itself does.
fn key_impression(
    storage: &Storage,
    impression_id: &str,
    session_id: &str,
    prompt_id: &str,
    shown_at: &str,
) {
    let wrote = storage.with_connection(|conn| {
        let previous_ms: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        conn.busy_timeout(std::time::Duration::ZERO)?;
        let result = ensure_prompt_key_table(conn).and_then(|()| {
            conn.execute(
                "INSERT OR IGNORE INTO exposure_prompt_keys
                 (impression_id, session_id, prompt_id, shown_at) VALUES (?1,?2,?3,?4)",
                rusqlite::params![impression_id, session_id, prompt_id, shown_at],
            )
            .map(|_| ())
        });
        let restore = conn.busy_timeout(std::time::Duration::from_millis(
            u64::try_from(previous_ms).unwrap_or(5_000),
        ));
        restore?;
        Ok(result?)
    });
    if let Err(error) = wrote {
        eprintln!("CSR: exposure prompt key dropped (non-fatal): {error}");
    }
}

/// The impressions stored for one prompt of one session, oldest first. The
/// consumption audit starts here to join an injection to its prompt.
pub fn impressions_for_prompt(
    storage: &Storage,
    session_id: &str,
    prompt_id: &str,
) -> anyhow::Result<Vec<String>> {
    storage.with_connection(|conn| {
        ensure_prompt_key_table(conn)?;
        let mut stmt = conn.prepare(
            "SELECT impression_id FROM exposure_prompt_keys
             WHERE session_id = ?1 AND prompt_id = ?2 ORDER BY shown_at, impression_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, prompt_id], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    })
}

/// Record a whole stdout emission without ever waiting on database contention.
/// Hooks call this only after they know the corresponding text was emitted.
/// A surface with no prompt (SessionStart) records no prompt key.
#[allow(clippy::too_many_arguments)]
pub fn record_impression(
    storage: &Storage,
    session_id: Option<&str>,
    project: &str,
    surface: &str,
    query: Option<&str>,
    query_embedding: Option<&[f32]>,
    intent: &str,
    items: Vec<ExposureItem>,
) {
    record_impression_keyed(
        storage,
        session_id,
        None,
        project,
        surface,
        query,
        query_embedding,
        intent,
        items,
    );
}

/// [`record_impression`] that also keys the row by the `prompt_id` of the
/// prompt it answered, when there is one.
#[allow(clippy::too_many_arguments)]
pub fn record_impression_keyed(
    storage: &Storage,
    session_id: Option<&str>,
    prompt_id: Option<&str>,
    project: &str,
    surface: &str,
    query: Option<&str>,
    query_embedding: Option<&[f32]>,
    intent: &str,
    items: Vec<ExposureItem>,
) {
    record_impression_inner(
        storage,
        session_id,
        prompt_id,
        project,
        surface,
        query,
        query_embedding,
        intent,
        items,
        false,
    );
}

/// [`record_impression_keyed`] for the serve-only daemon, whose connections are
/// `query_only`. A prompt-keyed impression and its key row are the one write it
/// makes: `query_only` is lifted for exactly those inserts and put back before
/// this returns, whether they landed or not. An impression with no
/// `prompt_id` stays blocked, as every other write does.
#[allow(clippy::too_many_arguments)]
pub fn record_impression_serve_only(
    storage: &Storage,
    session_id: Option<&str>,
    prompt_id: Option<&str>,
    project: &str,
    surface: &str,
    query: Option<&str>,
    query_embedding: Option<&[f32]>,
    intent: &str,
    items: Vec<ExposureItem>,
) {
    record_impression_inner(
        storage,
        session_id,
        prompt_id,
        project,
        surface,
        query,
        query_embedding,
        intent,
        items,
        true,
    );
}

/// Puts `PRAGMA query_only=ON` back when dropped.
struct QueryOnlyLift<'a>(&'a Storage);

impl<'a> QueryOnlyLift<'a> {
    fn new(storage: &'a Storage) -> Option<Self> {
        storage
            .with_connection(|conn| {
                conn.execute_batch("PRAGMA query_only=OFF")?;
                Ok(())
            })
            .ok()
            .map(|()| Self(storage))
    }
}

impl Drop for QueryOnlyLift<'_> {
    fn drop(&mut self) {
        let _ = self.0.with_connection(|conn| {
            conn.execute_batch("PRAGMA query_only=ON")?;
            Ok(())
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn record_impression_inner(
    storage: &Storage,
    session_id: Option<&str>,
    prompt_id: Option<&str>,
    project: &str,
    surface: &str,
    query: Option<&str>,
    query_embedding: Option<&[f32]>,
    intent: &str,
    items: Vec<ExposureItem>,
    serve_only: bool,
) {
    let Some(session_id) = session_id else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let prompt_id = prompt_id.map(str::trim).filter(|id| !id.is_empty());
    let _lift = if serve_only {
        // Keyed rows only; the guard restores `query_only` on every exit.
        if prompt_id.is_none() {
            return;
        }
        match QueryOnlyLift::new(storage) {
            Some(lift) => Some(lift),
            None => return,
        }
    } else {
        None
    };
    let impression = ExposureImpression {
        impression_id: uuid::Uuid::new_v4().to_string(),
        session_id: session_id.to_string(),
        project: project.to_string(),
        surface: surface.to_string(),
        query_hash: query.map(query_hash),
        query_embedding: query_embedding.map(ToOwned::to_owned),
        intent: intent.to_string(),
        shown_at: chrono::Utc::now().to_rfc3339(),
        feature_schema: FEATURE_SCHEMA,
        legacy: false,
        items,
    };
    match storage.try_record_rerank_exposure(&impression) {
        Ok(true) => {
            if let Some(prompt_id) = prompt_id {
                key_impression(
                    storage,
                    &impression.impression_id,
                    &impression.session_id,
                    prompt_id,
                    &impression.shown_at,
                );
            }
        }
        Ok(false) => eprintln!("CSR: exposure telemetry dropped (database busy)"),
        Err(error) => eprintln!("CSR: exposure telemetry error (non-fatal): {error}"),
    }
}

#[cfg(test)]
mod tests {
    use crate::injection::formatter::{
        FormattedInjection, InjectionCategory, RenderedInjectionItem,
    };
    use crate::storage::trained_rerank::ExposureItem;

    use super::{impressions_for_prompt, record_impression_keyed, rendered_exposure_items};
    use crate::storage::Storage;

    fn item(id: &str) -> ExposureItem {
        ExposureItem {
            rank: -1,
            memory_id: id.into(),
            conversation_id: None,
            source_type: "reflection".into(),
            baseline_score: None,
            cosine: None,
            recency: None,
            graph_proximity: None,
            author: None,
            is_scaffold: false,
            is_mechanic: false,
            supersedes: false,
        }
    }

    #[test]
    fn receipt_maps_only_rendered_memory_items_and_assigns_dense_ranks() {
        let receipt = FormattedInjection {
            text: "shown".into(),
            rendered_items: vec![
                RenderedInjectionItem {
                    category: InjectionCategory::AntiPattern,
                    index: 0,
                },
                RenderedInjectionItem {
                    category: InjectionCategory::RelevantContext,
                    index: 0,
                },
                RenderedInjectionItem {
                    category: InjectionCategory::WinningStrategy,
                    index: 1,
                },
            ],
        };
        let anti = vec![Some(item("anti"))];
        let relevant = vec![None];
        let winning = vec![Some(item("truncated-out")), Some(item("winner"))];

        let selected = rendered_exposure_items(&receipt, &anti, &relevant, &winning);

        assert_eq!(
            selected
                .iter()
                .map(|entry| (entry.rank, entry.memory_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(0, "anti"), (1, "winner")]
        );
    }

    #[test]
    fn an_impression_is_keyed_by_its_prompt_and_an_unkeyed_one_is_not() {
        let storage = Storage::open_memory().unwrap();
        record_impression_keyed(
            &storage,
            Some("sess"),
            Some("prompt-1"),
            "proj",
            "prompt_submit",
            Some("query"),
            None,
            "other",
            vec![ExposureItem {
                rank: 0,
                ..item("a")
            }],
        );
        record_impression_keyed(
            &storage,
            Some("sess"),
            None,
            "proj",
            "prompt_submit",
            Some("other query"),
            None,
            "other",
            vec![ExposureItem {
                rank: 0,
                ..item("b")
            }],
        );
        record_impression_keyed(
            &storage,
            Some("sess"),
            Some("   "),
            "proj",
            "prompt_submit",
            Some("third query"),
            None,
            "other",
            vec![ExposureItem {
                rank: 0,
                ..item("c")
            }],
        );
        let keyed = impressions_for_prompt(&storage, "sess", "prompt-1").unwrap();
        assert_eq!(keyed.len(), 1);
        assert!(impressions_for_prompt(&storage, "sess", "prompt-2")
            .unwrap()
            .is_empty());
        assert!(impressions_for_prompt(&storage, "other-sess", "prompt-1")
            .unwrap()
            .is_empty());
        // The key joins to the stored impression and its item.
        let joined: String = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT item.memory_id FROM exposure_prompt_keys key
                     JOIN rerank_exposure_items item ON item.impression_id = key.impression_id
                     WHERE key.prompt_id = 'prompt-1'",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(joined, "a");
        let total: i64 = storage
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM rerank_exposure_impressions",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(total, 3);
    }
}
