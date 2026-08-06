use std::path::{Component, Path, PathBuf};

use crate::anchor::to_git_path;
use crate::verdict::SupersededReceipt;
use crate::{causal, Anchor, CausalOrder, Error, StampKind, Tier, Verdict, Witness};

/// Wraps a [`gix::Repository`] to produce and audit [`Witness`]es.
///
/// All I/O (reading blobs, reading worktree files, walking commit ancestry)
/// happens synchronously and locally — no network access, no LLM calls, no
/// wall-clock time is ever consulted.
pub struct Auditor {
    repo: gix::Repository,
}

impl Auditor {
    /// Wrap an already-open repository.
    pub fn new(repo: gix::Repository) -> Self {
        Self { repo }
    }

    /// Open the repository at exactly `path` (no upward search).
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let repo = gix::open(&path).map_err(|source| Error::Open {
            path: path.clone(),
            source: Box::new(source),
        })?;
        Ok(Self::new(repo))
    }

    /// Open the repository containing `path`, searching upward through
    /// parent directories the way `git` itself does.
    pub fn discover(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let repo = gix::discover(&path).map_err(|source| Error::Discover {
            path: path.clone(),
            source: Box::new(source),
        })?;
        Ok(Self::new(repo))
    }

    /// The wrapped repository, e.g. to pass into [`crate::causal::compare`].
    pub fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    // ---- stamping ---------------------------------------------------

    /// Stamp `anchor` from the live worktree, using a raw (whitespace-sensitive)
    /// content hash. Tier is [`Tier::Worktree`].
    pub fn stamp(&self, anchor: &Anchor) -> Result<Witness, Error> {
        self.stamp_with_kind(anchor, StampKind::Raw)
    }

    /// Like [`Self::stamp`], but whitespace-insensitive
    /// ([`crate::stamp_normalized`]).
    pub fn stamp_normalized(&self, anchor: &Anchor) -> Result<Witness, Error> {
        self.stamp_with_kind(anchor, StampKind::Normalized)
    }

    /// Stamp `anchor` as it existed in `commit`, using a raw content hash.
    /// Tier is [`Tier::Committed`] — this is the only tier from which a
    /// [`Verdict::Superseded`] receipt may later be minted.
    pub fn stamp_at(&self, anchor: &Anchor, commit: gix::ObjectId) -> Result<Witness, Error> {
        self.stamp_at_with_kind(anchor, commit, StampKind::Raw)
    }

    /// Like [`Self::stamp_at`], but whitespace-insensitive.
    pub fn stamp_normalized_at(
        &self,
        anchor: &Anchor,
        commit: gix::ObjectId,
    ) -> Result<Witness, Error> {
        self.stamp_at_with_kind(anchor, commit, StampKind::Normalized)
    }

    fn stamp_with_kind(&self, anchor: &Anchor, kind: StampKind) -> Result<Witness, Error> {
        let bytes = self.read_worktree_bytes(anchor)?;
        let stamp = kind.compute(&bytes);
        let at = self.repo.head_id()?.detach();
        Ok(Witness::new(anchor.clone(), stamp, at))
    }

    fn stamp_at_with_kind(
        &self,
        anchor: &Anchor,
        commit: gix::ObjectId,
        kind: StampKind,
    ) -> Result<Witness, Error> {
        let bytes = self.read_committed_bytes(anchor, commit)?;
        let stamp = kind.compute(&bytes);
        Ok(Witness::new_committed(anchor.clone(), stamp, commit))
    }

    // ---- auditing -----------------------------------------------------

    /// Check whether `witness` still holds against the current worktree.
    ///
    /// Only ever returns [`Verdict::Intact`], [`Verdict::Drifted`], or
    /// [`Verdict::Vanished`] — minting [`Verdict::Superseded`] requires
    /// explicit successor evidence, see [`Self::audit_against_successor`].
    ///
    /// This is the crate's only audit entry point: it is fallible on
    /// purpose. Operational failures (permission denied, a corrupt
    /// repository, an unreadable object) are *not* the same fact as
    /// "the anchor is gone" and must not be silently folded into
    /// [`Verdict::Vanished`] — a caller who can't tell "genuinely gone"
    /// from "I couldn't check" cannot make a sound decision either way.
    /// Only [`Error::AnchorMissing`] — the anchor's path genuinely does
    /// not resolve — becomes `Vanished`; every other [`Error`] propagates.
    pub fn try_audit(&self, witness: &Witness) -> Result<Verdict, Error> {
        match self.read_worktree_bytes(witness.anchor()) {
            Ok(bytes) => {
                let current = witness.stamp().kind().compute(&bytes);
                Ok(if current == *witness.stamp() {
                    Verdict::Intact
                } else {
                    Verdict::Drifted
                })
            }
            Err(Error::AnchorMissing { .. }) => Ok(Verdict::Vanished {
                last_seen: witness.at(),
            }),
            Err(other) => Err(other),
        }
    }

    /// Check `witness` against the current worktree, additionally
    /// considering `successor` as a candidate replacement.
    ///
    /// # Errors
    ///
    /// Like [`Self::try_audit`], operational failures are never folded
    /// into a verdict: an error auditing `witness` itself (anything
    /// `try_audit` propagates), and any repository-access failure while
    /// re-deriving the successor's claimed evidence (a commit that cannot
    /// be found or decoded, an unreadable tree, a merge-base failure),
    /// propagate as `Err`. Only a successor that was *successfully
    /// checked* and found wanting — wrong tier, a re-derived stamp that
    /// doesn't match its claim, causally backwards — degrades to the
    /// plain verdict. "I couldn't check" is not the same fact as "checked
    /// and rejected", and must never become a deterministic verdict.
    ///
    /// # Verdict logic
    ///
    /// - If current content still matches `witness.stamp()`, the resurrection
    ///   rule wins: [`Verdict::Intact`], regardless of `successor` — content
    ///   identity beats any history claim, and a witness whose content is
    ///   still intact is never eligible for supersession in the first
    ///   place.
    /// - Otherwise, [`Verdict::Superseded`] is minted **only** if all of the
    ///   following hold; if any fails (as a checked rejection, not an
    ///   operational error — see above), this returns whatever
    ///   [`Self::try_audit`] returned for `witness` alone (typically
    ///   [`Verdict::Drifted`], or [`Verdict::Vanished`] if the anchor no
    ///   longer resolves):
    ///   1. `successor.tier() == Tier::Committed` — a dirty worktree isn't
    ///      part of the causal graph yet and can't supersede anything.
    ///   2. `successor.at()` resolves to a real commit in this repository.
    ///   3. `successor`'s stamp is **re-derived** from `successor.anchor()`'s
    ///      content at `successor.at()` and compared byte-for-byte against
    ///      `successor.stamp()` — a caller-supplied `Witness` claiming a
    ///      stamp that commit doesn't actually contain is rejected. (This
    ///      also transitively re-checks #2: re-deriving requires looking the
    ///      commit up.)
    ///   4. **Causal precedence**, checked only when `witness.tier()` is
    ///      itself `Committed` (for `Worktree`-tier witnesses, `witness.at()`
    ///      is a provenance marker only, not a reproducible claim to order
    ///      against — see [`Tier::Worktree`]): `successor.at()` must not be
    ///      a (transitive) *ancestor* of `witness.at()`, and must not be
    ///      *equal* to it. A successor that causally precedes (or is) the
    ///      claim it supposedly replaces cannot be its supersession.
    ///      [`CausalOrder::DescendantOf`] is the ordinary case.
    ///      [`CausalOrder::Incomparable`] (squash, rebase, cherry-pick —
    ///      see `tests/adversarial.rs`) is accepted *only* because check #3
    ///      already proved the successor's content by direct re-derivation
    ///      from the object database; the commit graph gives no help in
    ///      that case, and content re-derivation is why none is needed.
    pub fn audit_against_successor(
        &self,
        witness: &Witness,
        successor: &Witness,
    ) -> Result<Verdict, Error> {
        let fallback = self.try_audit(witness)?;
        if fallback.is_intact() {
            return Ok(fallback);
        }

        if self.successor_is_valid(witness, successor)? {
            Ok(Verdict::Superseded(SupersededReceipt::new(
                successor.anchor().clone(),
                successor.at(),
            )))
        } else {
            Ok(fallback)
        }
    }

    /// All of [`Self::audit_against_successor`]'s supersession checks.
    /// `Ok(false)` means the successor was fully checked and *rejected*
    /// (wrong tier, stamp mismatch, causally backwards); `Err` means a
    /// repository-access failure prevented checking at all (missing or
    /// undecodable commit, unreadable tree, merge-base error) — the two
    /// must stay distinguishable, because only the former is a
    /// deterministic fact about the evidence.
    fn successor_is_valid(&self, witness: &Witness, successor: &Witness) -> Result<bool, Error> {
        // (1) Tier gate.
        if successor.tier() != Tier::Committed {
            return Ok(false);
        }

        // (2) + (3): re-derive the successor's stamp from the commit it
        // claims — this also proves the commit exists (`read_committed_bytes`
        // errors otherwise, and that error propagates: a successor whose
        // claimed commit can't even be read is unverifiable, not
        // verified-and-rejected).
        let bytes = self.read_committed_bytes(successor.anchor(), successor.at())?;
        let recomputed = successor.stamp().kind().compute(&bytes);
        if recomputed != *successor.stamp() {
            return Ok(false);
        }

        // (4) Causal precedence, only meaningful when the original witness
        // is itself pinned to a real commit.
        if witness.tier() == Tier::Committed {
            let order = causal::compare(&self.repo, successor.at(), witness.at())?;
            if matches!(order, CausalOrder::AncestorOf | CausalOrder::Equal) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    // ---- content access -------------------------------------------------

    fn read_worktree_bytes(&self, anchor: &Anchor) -> Result<Vec<u8>, Error> {
        let workdir = self
            .repo
            .workdir()
            .ok_or_else(|| Error::BareRepository(self.repo.path().to_path_buf()))?;
        let full_path = resolve_worktree_path(workdir, &anchor.path)?;
        let bytes = std::fs::read(&full_path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::AnchorMissing {
                    path: anchor.path.clone(),
                    commit: None,
                }
            } else {
                Error::WorktreeRead {
                    path: full_path.clone(),
                    source,
                }
            }
        })?;
        apply_span(bytes, anchor.span, &anchor.path)
    }

    fn read_committed_bytes(
        &self,
        anchor: &Anchor,
        commit: gix::ObjectId,
    ) -> Result<Vec<u8>, Error> {
        let bytes = self.blob_bytes_at_path(&anchor.path, commit)?;
        apply_span(bytes, anchor.span, &anchor.path)
    }

    /// Full, unspanned content of `path` (repo-relative) as it existed at
    /// `commit` — the historical-mode counterpart to reading a live worktree
    /// file with [`std::fs::read`]. Public so callers extracting
    /// function/type/const spans from historical source text (rather than
    /// auditing a single already-known span) don't have to duplicate
    /// [`Self::stamp_at`]'s blob-lookup internals — see `csr-engine`'s
    /// `codegraph stamp-spans --at <rev>`.
    ///
    /// Same failure modes as [`Self::stamp_at`]'s own lookup: [`Error::AnchorMissing`]
    /// if `path` isn't present in `commit`'s tree, `Error::NotABlob` if it names a
    /// tree/submodule/symlink instead of a regular file.
    pub fn file_content_at(&self, path: &Path, commit: gix::ObjectId) -> Result<Vec<u8>, Error> {
        self.blob_bytes_at_path(path, commit)
    }

    /// Every blob path present in `commit`'s tree, relative to the
    /// repository root (forward-slash separated, matching [`to_git_path`]'s
    /// convention) — directories, submodule commit
    /// pointers, and symlinks are excluded; only plain and executable blobs
    /// are returned. Used by historical `stamp-spans --at <rev>` to
    /// discover which files existed at a past commit: that set can differ
    /// from both the live working tree and the code graph's own node list
    /// (a historical commit may contain files the graph never saw, or omit
    /// files the graph still remembers).
    pub fn files_at(&self, commit: gix::ObjectId) -> Result<Vec<PathBuf>, Error> {
        let tree = self.tree_at(commit)?;
        let entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|source| Error::TreeWalk {
                commit,
                source: Box::new(source),
            })?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.mode.is_blob())
            .map(|entry| PathBuf::from(entry.filepath.to_string()))
            .collect())
    }

    /// Resolve `rev` (branch, tag, short/long SHA, `HEAD~2`, ... — anything
    /// `git rev-parse` accepts) to the commit it names, following annotated
    /// tags. Every failure mode — the spec doesn't parse, the object it
    /// names can't be found, or it resolves to something that isn't (and
    /// doesn't peel to) a commit, e.g. a bare tree or blob SHA — folds into
    /// a single [`Error::RevParse`]: historical `stamp-spans --at <rev>`
    /// treats all of them identically ("this repo doesn't have that
    /// revision, skip it"), never guesses a substitute commit.
    pub fn resolve_commit(&self, rev: &str) -> Result<gix::ObjectId, Error> {
        let wrap = |source: Box<dyn std::error::Error + Send + Sync + 'static>| Error::RevParse {
            rev: rev.to_string(),
            source,
        };
        let id = self
            .repo
            .rev_parse_single(rev)
            .map_err(|source| wrap(Box::new(source)))?;
        let object = id.object().map_err(|source| wrap(Box::new(source)))?;
        let commit = object
            .peel_to_commit()
            .map_err(|source| wrap(Box::new(source)))?;
        Ok(commit.id)
    }

    fn tree_at(&self, commit: gix::ObjectId) -> Result<gix::Tree<'_>, Error> {
        let found = self
            .repo
            .find_commit(commit)
            .map_err(|source| Error::Commit {
                commit,
                source: Box::new(source),
            })?;
        found.tree().map_err(|source| Error::Tree {
            commit,
            source: Box::new(source),
        })
    }

    fn blob_bytes_at_path(&self, path: &Path, commit: gix::ObjectId) -> Result<Vec<u8>, Error> {
        reject_escaping_anchor(path)?;
        let tree = self.tree_at(commit)?;
        let entry = tree
            .lookup_entry_by_path(git_path_as_std_path(&to_git_path(path)))
            .map_err(|source| Error::TreeLookup {
                commit,
                path: path.to_path_buf(),
                source: Box::new(source),
            })?
            .ok_or_else(|| Error::AnchorMissing {
                path: path.to_path_buf(),
                commit: Some(commit),
            })?;
        let object = entry.object().map_err(|source| Error::TreeLookup {
            commit,
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        let mut blob = object.try_into_blob().map_err(|source| Error::NotABlob {
            commit,
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        // `Blob` implements `Drop`, so `blob.data` cannot be moved out
        // directly (E0509) — `take_data()` exists precisely for this.
        Ok(blob.take_data())
    }
}

/// `lookup_entry_by_path` wants a `Path`; callers already normalize to
/// forward slashes internally via [`to_git_path`], so this is a cheap
/// re-wrap, not a second normalization pass.
fn git_path_as_std_path(git_path: &str) -> &Path {
    Path::new(git_path)
}

/// Lexical rejection shared by both the worktree and committed-tree read
/// paths: an anchor path must be relative and must not contain a `..`
/// component. Committed-tree lookups (`lookup_entry_by_path`) don't walk
/// the filesystem, so a `..` component can't actually escape the repo
/// there — but treating it as a literal tree-entry name isn't meaningful
/// either, and rejecting it uniformly keeps `Anchor`'s contract identical
/// across both tiers instead of tier-dependent.
fn reject_escaping_anchor(path: &Path) -> Result<(), Error> {
    if path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(Error::PathEscape {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Resolve `anchor_path` against `workdir`, refusing to return any path
/// that escapes it — via an absolute anchor path, a `..` component, or a
/// symlink (anywhere along the path, including a not-yet-existing final
/// component's parent) that resolves outside `workdir`.
fn resolve_worktree_path(workdir: &Path, anchor_path: &Path) -> Result<PathBuf, Error> {
    reject_escaping_anchor(anchor_path)?;
    let full_path = workdir.join(anchor_path);

    let canonical_workdir = workdir
        .canonicalize()
        .map_err(|source| Error::WorktreeRead {
            path: workdir.to_path_buf(),
            source,
        })?;

    match full_path.canonicalize() {
        Ok(canonical_full) if canonical_full.starts_with(&canonical_workdir) => Ok(full_path),
        Ok(_) => Err(Error::PathEscape {
            path: anchor_path.to_path_buf(),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            // The path (or some component of it) doesn't exist yet. There's
            // nothing to canonicalize all the way down, but a symlinked
            // *directory* earlier in the path could still point outside
            // `workdir` even though the final component is missing — walk
            // up to the nearest existing ancestor and check that instead.
            // The genuinely-missing case is reported as `AnchorMissing` by
            // the caller's own `fs::read`, not here.
            let mut ancestor = full_path.parent();
            while let Some(dir) = ancestor {
                match dir.canonicalize() {
                    Ok(canonical_ancestor) => {
                        if !canonical_ancestor.starts_with(&canonical_workdir) {
                            return Err(Error::PathEscape {
                                path: anchor_path.to_path_buf(),
                            });
                        }
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        ancestor = dir.parent();
                    }
                    Err(source) => {
                        return Err(Error::WorktreeRead {
                            path: dir.to_path_buf(),
                            source,
                        })
                    }
                }
            }
            Ok(full_path)
        }
        Err(source) => Err(Error::WorktreeRead {
            path: full_path,
            source,
        }),
    }
}

/// Cut `bytes` down to the 1-based inclusive line `span` (identity when
/// `span` is `None`).
///
/// Reconstruction appends `\n` after EVERY selected line, including the
/// last — so a span that ends on the file's final line produces the same
/// bytes whether or not that line had a terminator on disk. Both the
/// worktree and committed paths share this function, so spanned stamps
/// stay comparable across tiers; the trade-off is that a spanned stamp is
/// deliberately insensitive to the presence of a final newline (unlike a
/// whole-file [`crate::Stamp::from_bytes`], which normalizes nothing).
fn apply_span(bytes: Vec<u8>, span: Option<(u32, u32)>, path: &Path) -> Result<Vec<u8>, Error> {
    let Some((start, end)) = span else {
        return Ok(bytes);
    };
    if start == 0 || start > end {
        return Err(Error::InvalidSpan { start, end });
    }

    let ends_with_newline = bytes.ends_with(b"\n");
    // `split` on empty input yields one empty slice, but an empty file has
    // zero lines — no span may resolve against it (`available: 0` below).
    let mut lines: Vec<&[u8]> = if bytes.is_empty() {
        Vec::new()
    } else {
        bytes.split(|&b| b == b'\n').collect()
    };
    if ends_with_newline {
        lines.pop();
    }
    let available = lines.len();
    let start_idx = (start - 1) as usize;
    let end_idx = (end - 1) as usize;
    if start_idx >= available || end_idx >= available {
        return Err(Error::SpanOutOfRange {
            path: path.to_path_buf(),
            start,
            end,
            available,
        });
    }

    let mut out = Vec::new();
    for line in &lines[start_idx..=end_idx] {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    Ok(out)
}
