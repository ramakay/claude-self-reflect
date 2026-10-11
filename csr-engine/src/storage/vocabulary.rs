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
        let mut cache = self
            .common_terms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((at, terms)) = cache.as_ref() {
            if !remeasure && at.elapsed() < REMEASURE_AFTER {
                return terms.clone();
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
                let stored = Stored {
                    measured_at: now.to_rfc3339(),
                    excerpts: measured.excerpts,
                    terms: measured.terms().map(str::to_owned).collect(),
                };
                if let Ok(raw) = serde_json::to_string(&stored) {
                    let _ = self.set_meta(META_KEY, &raw);
                }
                measured
            }
        };
        *cache = Some((Instant::now(), terms.clone()));
        terms
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
}
