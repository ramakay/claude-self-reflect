//! The measured frame vocabulary of a store's reflections, shared by every
//! process through the meta table and cached in each `Storage`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::injection::specificity::CommonTerms;

/// Frame terms drift slowly (they are the writers' templates), so a
/// measurement stands this long before any process re-measures it.
const REMEASURE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Characters of each reflection measured: the excerpt the prompt hook renders.
const EXCERPT_CHARS: i64 = 300;

/// Meta key holding the last measurement, so a hook process reads ~300 terms
/// instead of re-scanning every reflection (~120 ms on 10.7k rows).
const META_KEY: &str = "specificity_common_terms_v1";

pub(super) type CommonTermsCache = Mutex<Option<(Instant, Arc<CommonTerms>)>>;

#[derive(serde::Serialize, serde::Deserialize)]
struct Stored {
    measured_at: String,
    excerpts: usize,
    terms: Vec<String>,
}

impl super::Storage {
    /// Frame terms of this store's renderable reflections (episodes are JSON
    /// and never rendered as text, so they are left out). Read from this
    /// store's cache, else from the shared measurement in meta, else measured
    /// and shared; a measurement older than [`REMEASURE_AFTER`] is redone. A
    /// failed read measures nothing, which leaves only the structural rules.
    pub fn reflection_common_terms(&self) -> Arc<CommonTerms> {
        self.common_terms_cached_or_measured(false)
    }

    /// Measure now and share the result, whatever the caches hold. The
    /// daemon calls this on a timer inside the validity window, so a prompt
    /// render only ever reads a cached copy.
    pub fn refresh_reflection_common_terms(&self) -> Arc<CommonTerms> {
        self.common_terms_cached_or_measured(true)
    }

    fn common_terms_cached_or_measured(&self, remeasure: bool) -> Arc<CommonTerms> {
        // The cache lock covers the lookup and the publish only. The reads and
        // the write below can wait for SQLite, and a render reading the cache
        // must never queue behind them.
        if !remeasure {
            if let Some((at, terms)) = self.cached_terms() {
                if at.elapsed() < REMEASURE_AFTER {
                    return terms;
                }
            }
        }
        let now = chrono::Utc::now();
        let shared = (!remeasure)
            .then(|| self.get_meta(META_KEY).ok().flatten())
            .flatten()
            .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok())
            .filter(|stored| {
                chrono::DateTime::parse_from_rfc3339(&stored.measured_at).is_ok_and(|at| {
                    (now - at.with_timezone(&chrono::Utc))
                        .to_std()
                        .is_ok_and(|age| age < REMEASURE_AFTER)
                })
            });
        let terms = match shared {
            Some(stored) => Arc::new(CommonTerms::from_measurement(stored.terms, stored.excerpts)),
            None => {
                let measured = Arc::new(self.measure_common_terms());
                if self.vocabulary_writes {
                    let stored = Stored {
                        measured_at: now.to_rfc3339(),
                        excerpts: measured.excerpts,
                        terms: measured.terms().map(str::to_owned).collect(),
                    };
                    if let Ok(raw) = serde_json::to_string(&stored) {
                        let _ = self.set_meta(META_KEY, &raw);
                    }
                }
                measured
            }
        };
        *self
            .common_terms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((Instant::now(), terms.clone()));
        terms
    }

    fn cached_terms(&self) -> Option<(Instant, Arc<CommonTerms>)> {
        self.common_terms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn measure_common_terms(&self) -> CommonTerms {
        let excerpts: Vec<String> = self
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT substr(content, 1, ?1) FROM reflections
                     WHERE tags NOT LIKE '%\"session_episode\"%'",
                )?;
                let rows = stmt
                    .query_map([EXCERPT_CHARS], |r| r.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .collect();
                Ok(rows)
            })
            .unwrap_or_default();
        CommonTerms::measure(excerpts.iter().map(String::as_str))
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::Storage;
    use std::time::Instant;

    #[test]
    fn one_measurement_is_shared_through_meta() {
        let storage = Storage::open_memory().unwrap();
        for i in 0..250 {
            storage
                .insert_reflection(
                    &format!("r{i}"),
                    &format!("Session in demo project using Bash, Read, Edit about item{i}"),
                    &["session_story".to_string()],
                    &[0.0; 4],
                )
                .unwrap();
        }
        let first = storage.reflection_common_terms();
        assert_eq!(first.excerpts, 250);
        assert!(first.contains("session"));
        assert!(storage.get_meta(super::META_KEY).unwrap().is_some());

        // A second store over the same rows would read meta; here a fresh
        // cache on the same store must agree with the shared copy.
        *storage.common_terms.lock().unwrap() = None;
        let again = storage.reflection_common_terms();
        assert_eq!(again.excerpts, 250);
        assert!(again.contains("session") && again.contains("project"));

        // A refresh re-measures: a new row shows up at once.
        storage
            .insert_reflection("r-new", "fresh words here", &[], &[0.0; 4])
            .unwrap();
        assert_eq!(storage.reflection_common_terms().excerpts, 250);
        assert_eq!(storage.refresh_reflection_common_terms().excerpts, 251);
        assert_eq!(storage.reflection_common_terms().excerpts, 251);
    }

    fn file_store_with_reflections(dir: &std::path::Path, n: usize) -> Storage {
        let storage = Storage::open(&dir.join("csr-engine.db")).unwrap();
        for i in 0..n {
            storage
                .insert_reflection(
                    &format!("r{i}"),
                    &format!("Session in demo project using Bash, Read, Edit about item{i}"),
                    &["session_story".to_string()],
                    &[0.0; 4],
                )
                .unwrap();
        }
        storage
    }

    /// Holds the database write lock from another connection for `hold`.
    fn hold_writer(
        dir: &std::path::Path,
        hold: std::time::Duration,
    ) -> std::thread::JoinHandle<()> {
        let path = dir.join("csr-engine.db");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(path).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            ready_tx.send(()).unwrap();
            std::thread::sleep(hold);
            conn.execute_batch("COMMIT").unwrap();
        });
        ready_rx.recv().unwrap();
        handle
    }

    #[test]
    fn the_prompt_lane_never_waits_on_a_writer_to_publish_the_vocabulary() {
        let dir = tempfile::tempdir().unwrap();
        let storage = file_store_with_reflections(dir.path(), 250);
        let lane = storage.open_prompt_lane().unwrap().unwrap();
        let writer = hold_writer(dir.path(), std::time::Duration::from_millis(2500));

        // A render on a cold cache measures from reads. Writing the result to
        // meta would wait for the held transaction, with the lane's mutex held.
        let started = Instant::now();
        let terms = lane.reflection_common_terms();
        let took = started.elapsed();
        // The mutex is free for the next render's own statements.
        let again = Instant::now();
        lane.set_busy_timeout(Storage::PROMPT_LANE_BUSY_TIMEOUT)
            .unwrap();
        let mutex_wait = again.elapsed();
        writer.join().unwrap();

        assert_eq!(terms.excerpts, 250);
        assert!(took < std::time::Duration::from_millis(1000), "{took:?}");
        assert!(
            mutex_wait < std::time::Duration::from_millis(500),
            "{mutex_wait:?}"
        );
        assert!(
            storage.get_meta(super::META_KEY).unwrap().is_none(),
            "the lane must not write the shared measurement"
        );
    }

    #[test]
    fn a_refresh_on_the_daemon_connection_publishes_to_the_lane() {
        let dir = tempfile::tempdir().unwrap();
        let storage = file_store_with_reflections(dir.path(), 250);
        let lane = storage.open_prompt_lane().unwrap().unwrap();
        assert_eq!(storage.refresh_reflection_common_terms().excerpts, 250);

        // The refresh also wrote meta, so rows added afterwards are not what
        // the lane reads: it serves the published copy.
        storage
            .insert_reflection("late", "fresh words here", &[], &[0.0; 4])
            .unwrap();
        let writer = hold_writer(dir.path(), std::time::Duration::from_millis(1500));
        let started = Instant::now();
        let terms = lane.reflection_common_terms();
        let took = started.elapsed();
        writer.join().unwrap();
        assert_eq!(terms.excerpts, 250);
        assert!(took < std::time::Duration::from_millis(300), "{took:?}");
    }

    #[test]
    fn the_prompt_lane_waits_a_bounded_time_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let storage = file_store_with_reflections(dir.path(), 1);
        let lane = storage.open_prompt_lane().unwrap().unwrap();
        let timeout: i64 = lane
            .with_connection(|conn| Ok(conn.query_row("PRAGMA busy_timeout", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(
            timeout,
            Storage::PROMPT_LANE_BUSY_TIMEOUT.as_millis() as i64
        );
    }
}
