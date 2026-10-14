//! Where import picks up a conversation whose chunk rows predate the secret
//! scrubber. Those rows are never rewritten or deleted; see
//! `storage::legacy_frontier` for the rule and the stored frontier.

use std::path::Path;

use anyhow::Result;

use super::{provenance_matcher::MessageIndex, ParentContext, ResumePoint};
use crate::storage::Storage;

/// How the next import of one conversation proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resume {
    /// No chunk row predates the scrubber: import as always.
    Fresh,
    /// Chunk only what follows the stored rows.
    At(ResumePoint),
    /// Stored rows exist but the transcript position they end at cannot be
    /// found. Nothing is written and nothing is recorded, so the next pass tries
    /// again; an import that guessed could rewrite stored rows or skip messages.
    Unplaced,
}

/// Decide, and on first sight of a legacy conversation record, the resume
/// point. Call it under the operation lock, before parsing.
pub(crate) fn resolve(
    storage: &Storage,
    file_path: &Path,
    conversation_id: &str,
    parent: Option<&ParentContext>,
) -> Result<Resume> {
    if let Some((after_seq, first_chunk)) = storage.legacy_frontier(conversation_id)? {
        return Ok(Resume::At(ResumePoint {
            after_seq,
            first_chunk,
        }));
    }
    // With the scrub off nothing differs from how the rows were cut.
    if crate::redact::scrub_disabled() {
        return Ok(Resume::Fresh);
    }
    let Some(rows) = storage.legacy_rows(file_path, conversation_id)? else {
        return Ok(Resume::Fresh);
    };
    let after_seq = match rows.receipt_seq {
        Some(seq) => Some(seq),
        // A chunk stored before receipts existed, or whose backfill found no
        // match: find the newest chunk's text in the transcript itself.
        None => {
            let (chunk_id, content) = &rows.last_chunk;
            MessageIndex::read(
                file_path,
                conversation_id,
                false,
                false,
                parent,
                &[content.as_str()],
            )
            .ok()
            .and_then(|index| index.locate(chunk_id, content))
            .and_then(|evidence| evidence.events.iter().map(|event| event.seq).max())
        }
    };
    let Some(after_seq) = after_seq else {
        tracing::warn!(
            file = %file_path.display(),
            "stored chunks predate the secret scrub and their place in the transcript is unknown; \
             import leaves them alone and retries on the next change"
        );
        return Ok(Resume::Unplaced);
    };
    storage.record_legacy_frontier(conversation_id, after_seq, rows.first_chunk)?;
    Ok(Resume::At(ResumePoint {
        after_seq,
        first_chunk: rows.first_chunk,
    }))
}
