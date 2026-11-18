//! Where import picks up a conversation whose chunk rows predate the secret
//! scrubber. Those rows are never rewritten or deleted; see
//! `storage::legacy_frontier` for the rule and the stored frontier.

use std::path::Path;

use anyhow::Result;

use super::{
    provenance_matcher::{MessageIndex, Occurrence},
    ResumePoint,
};
use crate::storage::legacy_frontier::{FrontierCommit, LegacyRows, LegacyState};
use crate::storage::Storage;

/// How the next import of one conversation proceeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resume {
    /// No chunk row predates the scrubber: import as always.
    Fresh,
    /// Chunk only what follows the stored rows.
    At(ResumePoint),
    /// Stored rows exist but the transcript position they end at cannot be
    /// found. No chunk is written and no frontier is recorded, so the next pass
    /// tries again; an import that guessed could rewrite stored rows or skip
    /// messages. Only a note that the conversation waits is kept, for `status`.
    Unplaced,
}

/// Where `resolve` can be interrupted in a test: the two places a second
/// importer on the same database can get in between its steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gap {
    /// After the quick look found no frontier, before the stored rows are read.
    AfterNoFrontier,
    /// After the candidate frontier was computed (slow: the transcript is
    /// parsed), before it is written.
    BeforeCommit,
}

/// Decide, and on first sight of a legacy conversation record, the resume
/// point. Call it under the operation lock, before parsing. The operation lock
/// is shared, so the watcher and `Engine::import_file` can both be here for the
/// same conversation; every decision below is made against the stored state in
/// one read, and the write re-checks it in one short transaction.
pub(crate) fn resolve(
    storage: &Storage,
    file_path: &Path,
    conversation_id: &str,
) -> Result<Resume> {
    resolve_with(storage, file_path, conversation_id, &mut |_| {})
}

/// [`resolve`] with `between` called at each [`Gap`], so a test can run a
/// second importer there.
pub(crate) fn resolve_with(
    storage: &Storage,
    file_path: &Path,
    conversation_id: &str,
    between: &mut dyn FnMut(Gap),
) -> Result<Resume> {
    let placed = |after_seq, first_chunk| {
        Resume::At(ResumePoint {
            after_seq,
            first_chunk,
        })
    };
    if let Some((after_seq, first_chunk)) = storage.legacy_frontier(conversation_id)? {
        return Ok(placed(after_seq, first_chunk));
    }
    between(Gap::AfterNoFrontier);
    // Preservation does not depend on `CSR_NO_SECRET_SCRUB`: a row stored before
    // the scrubber is as unrewritable with the scrub off as with it on.
    // A few rounds: only a changed row set sends the loop round again.
    for _ in 0..3 {
        let rows = match storage.legacy_state(file_path, conversation_id)? {
            // A frontier recorded since the look above is the answer, never "fresh".
            LegacyState::Placed(after_seq, first_chunk) => {
                return Ok(placed(after_seq, first_chunk));
            }
            LegacyState::Fresh => return Ok(Resume::Fresh),
            LegacyState::Legacy(rows) => rows,
        };
        let Some(after_seq) = place(storage, file_path, conversation_id, &rows) else {
            tracing::warn!(
                file = %file_path.display(),
                "stored chunks predate the secret scrub and their place in the transcript is unknown; \
                 import leaves them alone and retries on the next change"
            );
            storage.record_legacy_unplaced(conversation_id)?;
            return Ok(Resume::Unplaced);
        };
        between(Gap::BeforeCommit);
        match storage.commit_legacy_frontier(file_path, conversation_id, &rows, after_seq)? {
            FrontierCommit::Stored(after_seq, first_chunk) => {
                return Ok(placed(after_seq, first_chunk));
            }
            FrontierCommit::NoLongerLegacy => return Ok(Resume::Fresh),
            FrontierCommit::RowsChanged => continue,
        }
    }
    // The rows kept changing under us; nothing was written, try again next change.
    storage.record_legacy_unplaced(conversation_id)?;
    Ok(Resume::Unplaced)
}

/// The highest message sequence the stored rows cover, or `None` when it cannot
/// be said for certain.
///
/// A receipt-derived sequence is a candidate, never a fact. Legacy receipts were
/// backfilled by `provenance_matcher`, which picks among repeated copies of a
/// text by trust, so one can sit at a later copy than the stored chunk came
/// from; a frontier taken from it would skip the messages in between. The
/// candidate is accepted only when the newest stored chunk's text, placed in the
/// transcript by position (`place_by_text`: unanimous, or the one place the
/// earlier stored chunks lead up to), ends at that very message. When the
/// position says otherwise the position wins, and when it cannot say, nothing
/// is placed: a receipt that points at one of several equally likely copies
/// does not break the tie.
fn place(
    storage: &Storage,
    file_path: &Path,
    conversation_id: &str,
    rows: &LegacyRows,
) -> Option<usize> {
    let by_position = place_by_text(storage, file_path, conversation_id, rows);
    if let Some(receipt) = rows.receipt_seq {
        if by_position != Some(receipt) {
            tracing::warn!(
                file = %file_path.display(),
                receipt,
                by_position = ?by_position,
                "a stored chunk's transcript receipt does not match where its text sits; \
                 the receipt is not used"
            );
        }
    }
    by_position
}

fn place_by_text(
    storage: &Storage,
    file_path: &Path,
    conversation_id: &str,
    rows: &LegacyRows,
) -> Option<usize> {
    let (chunk_id, content) = &rows.last_chunk;
    let index =
        MessageIndex::read_as_imported(file_path, conversation_id, &[content.as_str()]).ok()?;
    let last = index.occurrences(chunk_id, content);
    if let Some(seq) = unanimous(&last) {
        return Some(seq);
    }
    if last.is_empty() {
        return None;
    }
    // The newest chunk's text occurs more than once, at places that end at
    // different messages. The stored rows, in order, say which: the chunk before
    // it must end just where this one starts, and so back.
    let stored = storage.legacy_chunk_texts(conversation_id).ok()?;
    let texts: Vec<&str> = stored.iter().map(|(_, text)| text.as_str()).collect();
    let index = MessageIndex::read_as_imported(file_path, conversation_id, &texts).ok()?;
    follow_the_chain(&index, &stored, rows.from_the_start)
}

/// The one message sequence every occurrence ends at, if they agree.
fn unanimous(occurrences: &[Occurrence]) -> Option<usize> {
    let mut seqs = occurrences.iter().map(ends_at);
    let first = seqs.next()?;
    seqs.all(|seq| seq == first).then_some(first)
}

fn ends_at(occurrence: &Occurrence) -> usize {
    occurrence
        .evidence
        .events
        .iter()
        .map(|event| event.seq)
        .max()
        .unwrap_or(0)
}

/// Keep, of the places the newest stored chunk could be, those the earlier
/// stored chunks lead up to without a gap. Stop at the first step where the
/// survivors agree. When the rows run out first, only a chain that also starts
/// at the head of the transcript (and rows that hold the conversation from its
/// first chunk) settles it. Anything else is ambiguous: `None`.
fn follow_the_chain(
    index: &MessageIndex,
    stored: &[(String, String)],
    from_the_start: bool,
) -> Option<usize> {
    let newest = stored.len().checked_sub(1)?;
    // (message the candidate ends at, document, where the chunk walked back to starts)
    let mut live: Vec<(usize, usize, usize)> = index
        .occurrences(&stored[newest].0, &stored[newest].1)
        .iter()
        .map(|o| (ends_at(o), o.document, o.start))
        .collect();
    for back in 1..=newest {
        if let Some(seq) = agreed(&live) {
            return Some(seq);
        }
        let (id, text) = &stored[newest - back];
        let before = index.occurrences(id, text);
        live = live
            .iter()
            .flat_map(|&(seq, document, start)| {
                before
                    .iter()
                    .filter(move |o| {
                        o.document == document
                            && o.end <= start
                            && index.blank_between(document, o.end, start)
                    })
                    .map(move |o| (seq, document, o.start))
            })
            .collect();
        if live.is_empty() {
            return None;
        }
    }
    if let Some(seq) = agreed(&live) {
        return Some(seq);
    }
    if !from_the_start {
        return None;
    }
    live.retain(|&(_, document, start)| index.blank_between(document, 0, start));
    agreed(&live)
}

fn agreed(live: &[(usize, usize, usize)]) -> Option<usize> {
    let first = live.first()?.0;
    live.iter().all(|c| c.0 == first).then_some(first)
}
