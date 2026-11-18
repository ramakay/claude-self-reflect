//! Import-side attribution scanner (Journal v4 P4b).
//!
//! When the user pastes a journal copy block into a fresh session, the block's
//! marker line travels with it into that session's transcript. This module is
//! the only place a marker becomes a binding.
//!
//! Three properties, all deliberate:
//!
//! * **Binding is evidence.** The scanner reads the chunks that were actually
//!   parsed for embedding — the same text the corpus keeps. No marker in that
//!   text, no binding. There is no heuristic fallback, no fuzzy title match,
//!   nothing that infers use from timing.
//! * **The marker is retained, not suppressed.** It is scanned from imported
//!   content precisely because the anti-contamination machinery lets it
//!   through; see `storage::dream_attribution` for why that is correct and
//!   for the standing regression that pins it.
//! * **Fail-soft.** A binding failure is logged and import continues. Losing
//!   an attribution costs a metric; failing an import costs the corpus.

use crate::import::ConversationChunk;
use crate::storage::{dream_attribution, Storage};

/// Scan one conversation's parsed chunks for attribution markers and bind
/// each distinct dream to this conversation. Returns the number of NEW
/// bindings written (re-imports write none).
///
/// `conversation_id` is the transcript's own id — the session that carried
/// the pasted prompt.
pub fn bind_markers(
    storage: &Storage,
    conversation_id: &str,
    chunks: &[ConversationChunk],
) -> usize {
    let mut dream_ids: Vec<String> = Vec::new();
    for chunk in chunks {
        for id in dream_attribution::scan_markers(&chunk.content) {
            if !dream_ids.contains(&id) {
                dream_ids.push(id);
            }
        }
    }
    if dream_ids.is_empty() {
        return 0;
    }

    let mut bound = 0usize;
    for dream_id in &dream_ids {
        match storage
            .with_connection(|conn| dream_attribution::bind_marker(conn, dream_id, conversation_id))
        {
            Ok(true) => bound += 1,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    %error,
                    dream = %dream_id,
                    conversation = %conversation_id,
                    "dream attribution binding failed (non-fatal, import continues)"
                );
            }
        }
        // Attach the outcome once the bound session has an episode. Attempted
        // on EVERY pass, not only the one that created the binding: at first
        // import the session is usually still running and has no episode yet,
        // so the outcome can only land on a later pass. A no-op when there is
        // nothing to attach.
        if let Err(error) =
            storage.with_connection(|conn| dream_attribution::refresh_outcome(conn, dream_id))
        {
            tracing::debug!(
                %error,
                dream = %dream_id,
                "dream outcome refresh unavailable (non-fatal)"
            );
        }
    }
    if bound > 0 {
        tracing::info!(
            conversation = %conversation_id,
            bound,
            "bound pasted dream prompt(s) to their originating dreams"
        );
    }
    bound
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provenance::Speaker;
    use crate::storage::dream_attribution::{load_attribution, marker_line, record_emission};

    fn chunk(content: &str) -> ConversationChunk {
        ConversationChunk {
            id: "chunk-0".into(),
            conversation_id: "sess-new".into(),
            project_name: "proj".into(),
            timestamp: "2026-08-11T00:00:00Z".into(),
            content: content.to_string(),
            message_count: 1,
            summary: None,
            author: Speaker::User,
            seq: 0,
            is_sidechain: false,
        }
    }

    #[test]
    fn a_pasted_prompt_binds_its_dream_to_the_new_session() {
        let storage = Storage::open_memory().expect("storage");
        storage
            .with_connection(|conn| record_emission(conn, "0123456789abcdef", "execution"))
            .expect("emission");

        let marker = marker_line("0123456789abcdef");
        let bound = bind_markers(
            &storage,
            "sess-new",
            &[chunk(&format!("## Resume: something\n\n{marker}"))],
        );
        assert_eq!(bound, 1);

        let attribution = storage
            .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
            .expect("query")
            .expect("bound");
        assert_eq!(attribution.bound_session_id, "sess-new");
        assert_eq!(attribution.kind.as_deref(), Some("execution"));
    }

    #[test]
    fn a_transcript_without_a_marker_binds_nothing_ever() {
        let storage = Storage::open_memory().expect("storage");
        storage
            .with_connection(|conn| record_emission(conn, "0123456789abcdef", "execution"))
            .expect("emission");

        // Text that is *about* the same work, at the same time, in the same
        // project — and carries no marker. Absence proves nothing, so nothing
        // is written.
        let bound = bind_markers(
            &storage,
            "sess-new",
            &[chunk(
                "picking the release gate back up, same item the journal listed",
            )],
        );
        assert_eq!(bound, 0);
        assert_eq!(
            storage
                .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
                .expect("query"),
            None
        );
    }

    #[test]
    fn re_importing_the_same_transcript_does_not_double_bind() {
        let storage = Storage::open_memory().expect("storage");
        let marker = marker_line("0123456789abcdef");
        let chunks = vec![chunk(&marker)];
        assert_eq!(bind_markers(&storage, "sess-new", &chunks), 1);
        assert_eq!(
            bind_markers(&storage, "sess-new", &chunks),
            0,
            "an incremental re-import must not re-bind"
        );
    }

    #[test]
    fn two_pasted_prompts_in_one_session_bind_both_dreams() {
        let storage = Storage::open_memory().expect("storage");
        let bound = bind_markers(
            &storage,
            "sess-new",
            &[
                chunk(&marker_line("aaaaaaaaaaaaaaaa")),
                chunk(&marker_line("bbbbbbbbbbbbbbbb")),
            ],
        );
        assert_eq!(bound, 2);
        for id in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb"] {
            assert!(storage
                .with_connection(|conn| load_attribution(conn, id))
                .expect("query")
                .is_some());
        }
    }

    #[test]
    fn the_outcome_lands_on_the_pass_after_the_session_stored_its_episode() {
        let storage = Storage::open_memory().expect("storage");
        let chunks = vec![chunk(&marker_line("0123456789abcdef"))];

        // Pass 1: the session is still running, no episode exists yet.
        bind_markers(&storage, "sess-new", &chunks);
        let after_first = storage
            .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
            .expect("query")
            .expect("bound");
        assert_eq!(
            after_first.outcome, None,
            "no episode on record yet — nothing may be claimed about the result"
        );

        // The session ends and writes its v2 episode.
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES ('ep-42', ?1, '[]', '2026-08-11T00:00:00Z')",
                    rusqlite::params![
                        r#"{"schema":"v2","session_id":"sess-new","outcome":"completed"}"#
                    ],
                )?;
                Ok(())
            })
            .expect("episode");

        // Pass 2 (the transcript grew, the watcher re-imports).
        bind_markers(&storage, "sess-new", &chunks);
        let after_second = storage
            .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
            .expect("query")
            .expect("bound");
        assert_eq!(after_second.outcome.as_deref(), Some("completed"));
        assert_eq!(after_second.outcome_episode_id.as_deref(), Some("ep-42"));
    }

    #[test]
    fn an_episode_that_recorded_no_outcome_leaves_the_outcome_empty() {
        let storage = Storage::open_memory().expect("storage");
        storage
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO reflections (id, content, tags, timestamp)
                     VALUES ('ep-43', ?1, '[]', '2026-08-11T00:00:00Z')",
                    rusqlite::params![r#"{"schema":"v2","session_id":"sess-new"}"#],
                )?;
                Ok(())
            })
            .expect("episode");
        bind_markers(
            &storage,
            "sess-new",
            &[chunk(&marker_line("0123456789abcdef"))],
        );
        let attribution = storage
            .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
            .expect("query")
            .expect("bound");
        assert_eq!(attribution.outcome, None);
        assert_eq!(
            attribution.outcome_episode_id, None,
            "an episode id without an outcome would render a dangling arrow"
        );
    }

    #[test]
    fn binding_never_fabricates_an_outcome() {
        let storage = Storage::open_memory().expect("storage");
        bind_markers(
            &storage,
            "sess-new",
            &[chunk(&marker_line("0123456789abcdef"))],
        );
        let attribution = storage
            .with_connection(|conn| load_attribution(conn, "0123456789abcdef"))
            .expect("query")
            .expect("bound");
        assert_eq!(attribution.outcome, None);
        assert_eq!(attribution.outcome_episode_id, None);
        assert!(attribution.receipts.is_empty());
    }
}
