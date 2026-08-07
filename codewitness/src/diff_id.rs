//! Patch identity: a content-only fingerprint of a change, independent of
//! *where* in a file or *which commit* it was made in.
//!
//! gix 0.86 has no `patch-id` equivalent (git's own `patch-id` algorithm
//! lives in `git patch-id`, outside libgit2/gitoxide's scope), so this is
//! `codewitness`'s own primitive. It underlies squash- and
//! cherry-pick-equivalence: the same logical edit, applied in two different
//! commits with two different surrounding histories, produces the same
//! [`DiffId`] as long as the immediate context around the edit matches
//! (which a clean, non-conflicting cherry-pick guarantees by construction).

use std::fmt;

const PREFIX: &str = "b3d";
/// Prefix used when a size guard (see `MAX_LCS_CELLS`) diverts to whole-
/// buffer hashing instead of line-level diffing. Distinct from [`PREFIX`]
/// so a fallback id is never mistaken for (or accidentally collides with)
/// an ordinary line-diff id.
const FALLBACK_PREFIX: &str = "b3dfb";
/// Lines of unchanged context kept around each change, mirroring `diff -u`'s
/// default. Distant unrelated edits elsewhere in the file fall outside this
/// window and so cannot perturb the hash of a given change.
const CONTEXT: usize = 3;
/// Cap on the *product* `old_lines * new_lines` before [`diff_lines`]'s
/// O(n*m) LCS matrix (`(n+1)*(m+1)` `u32`s) is allowed to allocate — an
/// unbounded-allocation denial-of-service vector when `old`/`new` content
/// is not trusted. The product is what actually sizes the matrix: a
/// per-side cap of 20,000 lines would still admit a 20k x 20k matrix
/// (~1.6 GB of `u32`s), whereas 4,000,000 cells tops out around 16 MB.
/// When `old_lines.len() * new_lines.len()` exceeds this,
/// [`normalized_diff_id`] deterministically falls back to hashing the two
/// whole (line-ending-normalized) buffers directly (see
/// `hash_whole_buffers`) instead of line-diffing them at all.
const MAX_LCS_CELLS: usize = 4_000_000;
/// Per-side line cap, closing the gap `MAX_LCS_CELLS` alone leaves open:
/// the product collapses to 0 when one side has no lines (whole-file
/// creation or deletion — ordinary inputs for a witness backfill) and to
/// `n` when one side is a single line, so a multi-million-line side would
/// still reach [`diff_lines`] and allocate one DP row per old-side line.
/// Either side exceeding this cap diverts to `hash_whole_buffers`, exactly
/// as an oversized product does.
const MAX_LINES_PER_SIDE: usize = 100_000;

/// A content-only identity for a change between two byte buffers:
/// `"b3d:<64 hex chars>"`. Two diffs with the same [`DiffId`] made the same
/// edit, even if the commits that contain them are unrelated in the commit
/// graph (see [`crate::causal`] for that orthogonal axis).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DiffId(String);

impl DiffId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiffId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Compute the patch identity of the change from `old` to `new`.
///
/// Internally: line-diff the two buffers, keep only `CONTEXT` lines of
/// unchanged context around each changed region (exactly as `diff -u`
/// would draw hunk boundaries), then hash the concatenation of
/// `"-"`/`"+"`/`" "`-prefixed lines — deliberately omitting `@@ ... @@`
/// range headers (they encode absolute line position, which is exactly the
/// noise this primitive is designed to ignore) and any file/timestamp
/// header (there are no filenames at this layer).
///
/// Lines are split on `\n`, with any trailing `\r` stripped from each line
/// first — so `\n`- and `\r\n`-terminated content that is otherwise
/// identical produces the same [`DiffId`] (see the CRLF test below).
///
/// When the line-count product `old_lines * new_lines` exceeds
/// `MAX_LCS_CELLS`, or either side alone exceeds `MAX_LINES_PER_SIDE`,
/// this skips line-diffing entirely and falls back to hashing the two
/// whole buffers (see `hash_whole_buffers`) — see those constants' docs
/// for why.
pub fn normalized_diff_id(old: &[u8], new: &[u8]) -> DiffId {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);

    if old_lines.len() > MAX_LINES_PER_SIDE
        || new_lines.len() > MAX_LINES_PER_SIDE
        || old_lines.len().saturating_mul(new_lines.len()) > MAX_LCS_CELLS
    {
        return hash_whole_buffers(old, new);
    }

    let ops = diff_lines(&old_lines, &new_lines);
    let canonical = render_canonical(&ops, &old_lines, &new_lines);
    let hash = blake3::hash(&canonical);
    DiffId(format!("{PREFIX}:{}", hash.to_hex()))
}

/// Fallback for inputs too large for the O(n*m) LCS matrix to handle
/// safely (see `MAX_LCS_CELLS`): hash the two buffers directly, each
/// length-prefixed so `(old, new)` and e.g. `(old ++ new[..1], new[1..])`
/// can never collide. Both buffers are line-ending-normalized first (the
/// same `\r\n` -> `\n` rule as [`split_lines`]) so CRLF- vs LF-terminated
/// but otherwise identical inputs share a fallback id, exactly as they
/// share a line-diff id on the normal path. This is strictly coarser than
/// the line-level id — any change anywhere in an oversized buffer changes
/// the id, whether or not an equivalent edit elsewhere would otherwise
/// have shared a [`DiffId`] — but it stays fully deterministic, uses only
/// O(len(old) + len(new)) memory, and is clearly distinguishable from an
/// ordinary line-diff id via [`FALLBACK_PREFIX`].
fn hash_whole_buffers(old: &[u8], new: &[u8]) -> DiffId {
    let old = normalize_line_endings(old);
    let new = normalize_line_endings(new);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&(old.len() as u64).to_le_bytes());
    hasher.update(&old);
    hasher.update(&(new.len() as u64).to_le_bytes());
    hasher.update(&new);
    let hash = hasher.finalize();
    DiffId(format!("{FALLBACK_PREFIX}:{}", hash.to_hex()))
}

/// Whole-buffer form of the [`strip_trailing_cr`] rule used by
/// [`split_lines`]: drop each `\r` that immediately precedes a `\n`,
/// leaving lone `\r` bytes untouched. Borrows when there is nothing to do
/// (the overwhelmingly common case) so the fallback path stays
/// zero-copy for pure-LF input.
fn normalize_line_endings(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if !bytes.contains(&b'\r') {
        return std::borrow::Cow::Borrowed(bytes);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            i += 1; // skip the \r; the \n is emitted on the next iteration
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    std::borrow::Cow::Owned(out)
}

fn split_lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    if bytes.ends_with(b"\n") {
        lines.pop();
    }
    lines.iter().map(|line| strip_trailing_cr(line)).collect()
}

/// Strip a single trailing `\r`, treating `\r\n` as equivalent to `\n` for
/// line-splitting purposes (CRLF vs LF is a checkout/line-ending-config
/// artifact, not a meaningful content change).
fn strip_trailing_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Classic O(n*m) LCS-based line diff. Deliberately not the O(ND) Myers
/// algorithm — this crate targets code-review-sized hunks and files, not
/// bulk diffing, and the DP table is trivial to verify for correctness.
/// Only ever called when the line-count product `old * new` is at or
/// under `MAX_LCS_CELLS` — [`normalized_diff_id`] diverts larger inputs
/// to `hash_whole_buffers` before reaching here, so the `(n+1)*(m+1)`
/// allocation below is bounded.
fn diff_lines(old: &[&[u8]], new: &[&[u8]]) -> Vec<Op> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Equal(i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Delete(i));
            i += 1;
        } else {
            ops.push(Op::Insert(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Delete(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Insert(j));
        j += 1;
    }
    ops
}

fn render_canonical(ops: &[Op], old: &[&[u8]], new: &[&[u8]]) -> Vec<u8> {
    let is_change = |op: &Op| !matches!(op, Op::Equal(..));
    let keep: Vec<bool> = (0..ops.len())
        .map(|idx| {
            is_change(&ops[idx])
                || (idx.saturating_sub(CONTEXT)..=(idx + CONTEXT).min(ops.len().saturating_sub(1)))
                    .any(|k| is_change(&ops[k]))
        })
        .collect();

    let mut out = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        if !keep[idx] {
            continue;
        }
        match *op {
            Op::Equal(oi, _) => {
                out.push(b' ');
                out.extend_from_slice(old[oi]);
            }
            Op::Delete(oi) => {
                out.push(b'-');
                out.extend_from_slice(old[oi]);
            }
            Op::Insert(ni) => {
                out.push(b'+');
                out.extend_from_slice(new[ni]);
            }
        }
        out.push(b'\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_writes_the_complete_diff_id() {
        let id = DiffId("b3d:0123456789abcdef".to_owned());

        assert_eq!(id.to_string(), "b3d:0123456789abcdef");
    }

    #[test]
    fn identical_content_has_no_changed_lines() {
        let id = normalized_diff_id(b"a\nb\nc\n", b"a\nb\nc\n");
        // Still a valid, deterministic id (of the empty canonical diff).
        assert_eq!(id, normalized_diff_id(b"a\nb\nc\n", b"a\nb\nc\n"));
    }

    #[test]
    fn same_edit_far_apart_in_an_otherwise_different_file_matches() {
        // Same one-line change ("mid" -> "MID"), but surrounded by
        // unrelated padding of different lengths in each buffer.
        let old_a = b"h1\nh2\nh3\nmid\nt1\nt2\nt3\n".to_vec();
        let new_a = b"h1\nh2\nh3\nMID\nt1\nt2\nt3\n".to_vec();

        let old_b = b"x1\nx2\nh1\nh2\nh3\nmid\nt1\nt2\nt3\ny1\ny2\ny3\n".to_vec();
        let new_b = b"x1\nx2\nh1\nh2\nh3\nMID\nt1\nt2\nt3\ny1\ny2\ny3\n".to_vec();

        assert_eq!(
            normalized_diff_id(&old_a, &new_a),
            normalized_diff_id(&old_b, &new_b)
        );
    }

    #[test]
    fn different_edits_do_not_collide() {
        let a = normalized_diff_id(b"a\nb\nc\n", b"a\nB\nc\n");
        let b = normalized_diff_id(b"a\nb\nc\n", b"a\nb\nC\n");
        assert_ne!(a, b);
    }

    #[test]
    fn crlf_and_lf_line_endings_are_diff_equivalent() {
        let old_lf = b"a\nb\nc\n";
        let new_lf = b"a\nB\nc\n";
        let old_crlf = b"a\r\nb\r\nc\r\n";
        let new_crlf = b"a\r\nB\r\nc\r\n";

        // Sanity: the CRLF buffers really are byte-different from the LF
        // ones (otherwise this test would prove nothing).
        assert_ne!(old_lf.as_slice(), old_crlf.as_slice());

        assert_eq!(
            normalized_diff_id(old_lf, new_lf),
            normalized_diff_id(old_crlf, new_crlf),
        );
    }

    #[test]
    fn mixed_line_endings_within_one_buffer_still_match_pure_lf() {
        // A file with a stray CRLF line alongside LF ones (common after a
        // partial line-ending conversion) must diff identically to the
        // pure-LF equivalent.
        let mixed_old = b"a\r\nb\nc\n".to_vec();
        let mixed_new = b"a\r\nB\nc\n".to_vec();
        let lf_old = b"a\nb\nc\n".to_vec();
        let lf_new = b"a\nB\nc\n".to_vec();

        assert_eq!(
            normalized_diff_id(&mixed_old, &mixed_new),
            normalized_diff_id(&lf_old, &lf_new),
        );
    }

    /// Enough lines per side that `LINES * LINES` exceeds `MAX_LCS_CELLS`,
    /// tripping the product-cap guard without building huge fixtures.
    const OVERSIZED_LINES: usize = 2_500;
    // Compile-time proof the fixture actually exceeds the product cap.
    const _: () = assert!(OVERSIZED_LINES * OVERSIZED_LINES > MAX_LCS_CELLS);

    #[test]
    fn line_count_at_each_per_side_cap_stays_on_the_normal_path() {
        let at_cap = b"x\n".repeat(MAX_LINES_PER_SIDE);

        let old_at_cap = normalized_diff_id(&at_cap, b"");
        let new_at_cap = normalized_diff_id(b"", &at_cap);

        assert!(old_at_cap.as_str().starts_with(&format!("{PREFIX}:")));
        assert!(new_at_cap.as_str().starts_with(&format!("{PREFIX}:")));
    }

    #[test]
    fn new_side_above_line_cap_uses_fallback() {
        let above_cap = b"x\n".repeat(MAX_LINES_PER_SIDE + 1);

        let id = normalized_diff_id(b"", &above_cap);

        assert!(
            id.as_str().starts_with(&format!("{FALLBACK_PREFIX}:")),
            "new side above the line cap must use the fallback prefix, got {id}"
        );
    }

    #[test]
    fn lcs_product_at_cell_cap_stays_on_the_normal_path() {
        const SIDE: usize = 2_000;
        const _: () = assert!(SIDE * SIDE == MAX_LCS_CELLS);
        let old = b"old\n".repeat(SIDE);
        let new = b"new\n".repeat(SIDE);

        let id = normalized_diff_id(&old, &new);

        assert!(id.as_str().starts_with(&format!("{PREFIX}:")));
    }

    #[test]
    fn oversized_inputs_fall_back_to_deterministic_whole_buffer_hashing() {
        let mut old = Vec::new();
        let mut new = Vec::new();
        for i in 0..OVERSIZED_LINES {
            let line = format!("line-{i}\n");
            old.extend_from_slice(line.as_bytes());
            new.extend_from_slice(line.as_bytes());
        }
        // A single one-line edit near the end — under normal line-diffing
        // this would produce a small, localized canonical diff; the size
        // guard means it doesn't get the chance to.
        new.extend_from_slice(b"trailing-edit\n");

        let first = normalized_diff_id(&old, &new);
        let second = normalized_diff_id(&old, &new);
        assert_eq!(first, second, "fallback must still be deterministic");
        assert!(
            first.as_str().starts_with(&format!("{FALLBACK_PREFIX}:")),
            "oversized input must use the distinct fallback prefix, got {first}"
        );

        // A different edit over equally oversized input must not collide.
        let mut new2 = old.clone();
        new2.extend_from_slice(b"a-different-trailing-edit\n");
        let third = normalized_diff_id(&old, &new2);
        assert_ne!(first, third);
    }

    #[test]
    fn oversized_one_sided_deletion_falls_back_instead_of_allocating() {
        // Whole-file deletion: the old side is huge, the new side empty.
        // The cell product is 0, so the product cap alone would admit a
        // per-old-line DP row allocation — the per-side cap must divert
        // to the whole-buffer fallback instead.
        let mut old = Vec::new();
        for i in 0..(MAX_LINES_PER_SIDE + 1) {
            old.extend_from_slice(format!("line-{i}\n").as_bytes());
        }
        let id = normalized_diff_id(&old, b"");
        assert!(
            id.as_str().starts_with(&format!("{FALLBACK_PREFIX}:")),
            "one-sided oversized input must use the fallback prefix, got {id}"
        );
        // Deterministic, and distinct from the reverse direction (creation).
        assert_eq!(id, normalized_diff_id(&old, b""));
        assert_ne!(id, normalized_diff_id(b"", &old));
    }

    #[test]
    fn oversized_fallback_treats_crlf_and_lf_as_equivalent() {
        // Two buffer pairs, identical except for CRLF vs LF line endings,
        // both large enough to take the whole-buffer fallback path. They
        // must produce the same fallback DiffId, just as the normal
        // line-diff path treats them as equal.
        let mut old_lf = Vec::new();
        let mut old_crlf = Vec::new();
        for i in 0..OVERSIZED_LINES {
            old_lf.extend_from_slice(format!("line-{i}\n").as_bytes());
            old_crlf.extend_from_slice(format!("line-{i}\r\n").as_bytes());
        }
        let mut new_lf = old_lf.clone();
        new_lf.extend_from_slice(b"trailing-edit\n");
        let mut new_crlf = old_crlf.clone();
        new_crlf.extend_from_slice(b"trailing-edit\r\n");

        // Sanity: byte-different inputs, and the guard really fires.
        assert_ne!(old_lf, old_crlf);
        let id_lf = normalized_diff_id(&old_lf, &new_lf);
        let id_crlf = normalized_diff_id(&old_crlf, &new_crlf);
        assert!(
            id_lf.as_str().starts_with(&format!("{FALLBACK_PREFIX}:")),
            "fixture must take the fallback path, got {id_lf}"
        );

        assert_eq!(id_lf, id_crlf);
    }

    #[test]
    fn whole_buffer_normalization_only_removes_carriage_returns_before_newlines() {
        let normalized = normalize_line_endings(b"a\r\nb\rc\nd\r\n");

        assert_eq!(normalized.as_ref(), b"a\nb\rc\nd\n");
    }

    #[test]
    fn lcs_recurrence_uses_diagonal_and_adjacent_cells() {
        assert_eq!(
            diff_lines(&[b"a", b"a"], &[b"b", b"a", b"a"]),
            vec![Op::Insert(0), Op::Equal(0, 1), Op::Equal(1, 2)]
        );
        assert_eq!(
            diff_lines(&[b"a", b"a"], &[b"b", b"a"]),
            vec![Op::Delete(0), Op::Insert(0), Op::Equal(1, 1)]
        );
        assert_eq!(
            diff_lines(&[b"a", b"b", b"a"], &[b"c", b"a"]),
            vec![Op::Delete(0), Op::Delete(1), Op::Insert(0), Op::Equal(2, 1),]
        );
        assert_eq!(
            diff_lines(&[b"a"], &[b"b", b"b", b"a"]),
            vec![Op::Insert(0), Op::Insert(1), Op::Equal(0, 2)]
        );
    }

    #[test]
    fn diff_lines_stops_when_either_side_is_exhausted() {
        assert_eq!(
            diff_lines(&[b"same", b"old-tail"], &[b"same"]),
            vec![Op::Equal(0, 0), Op::Delete(1)]
        );
        assert_eq!(
            diff_lines(&[b"same"], &[b"same", b"new-tail"]),
            vec![Op::Equal(0, 0), Op::Insert(1)]
        );
    }

    #[test]
    fn deletion_tail_advances_to_completion() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            sender
                .send(diff_lines(&[b"same", b"old-tail"], &[b"same"]))
                .expect("test receiver must remain connected");
        });

        let ops = receiver
            .recv_timeout(std::time::Duration::from_millis(250))
            .expect("deletion tail must make forward progress");
        assert_eq!(ops, vec![Op::Equal(0, 0), Op::Delete(1)]);
    }

    #[test]
    fn disjoint_lines_prefer_deletion_on_an_lcs_tie() {
        assert_eq!(
            diff_lines(&[b"old"], &[b"new", b"tail"]),
            vec![Op::Delete(0), Op::Insert(0), Op::Insert(1)]
        );
    }

    #[test]
    fn canonical_render_keeps_unchanged_context_around_an_edit() {
        let old: &[&[u8]] = &[b"before", b"old", b"after"];
        let new: &[&[u8]] = &[b"before", b"new", b"after"];
        let ops = diff_lines(old, new);

        assert_eq!(
            render_canonical(&ops, old, new),
            b" before\n-old\n+new\n after\n"
        );
    }
}
