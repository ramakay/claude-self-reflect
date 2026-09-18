pub mod code_rank;
pub mod cross_project;
pub mod decay;
pub mod reinstatement;
pub mod rerank;

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use fs2::FileExt;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use hnsw_rs::prelude::DistCosine;
use hnsw_rs::prelude::Distance;
use serde::{Deserialize, Serialize};

/// A search result with score and ID.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: String,
    pub score: f32,
}

/// HNSW-backed search engine for chunks and reflections.
///
/// Uses `hnsw_rs` with cosine distance. Scores are converted from
/// distance (1.0 - cosine_similarity) back to similarity (1.0 - distance).
pub struct SearchEngine {
    chunk_index: Hnsw<'static, f32, DistCosine>,
    reflection_index: Hnsw<'static, f32, DistCosine>,
    chunk_id_map: Vec<String>,
    reflection_id_map: Vec<String>,
    chunk_id_set: HashSet<String>,
    reflection_id_set: HashSet<String>,
    active_reflection_count: usize,
    /// Chunk index dirty since the last `dump_to_disk`. Split from a single
    /// `dirty` flag so a reflection-only mutation (e.g. `store_reflection`,
    /// the `stop` hook) dumps only `reflections.hnsw.*` and leaves the
    /// (usually far larger) `chunks.hnsw.*` files untouched — see
    /// `dump_to_disk`. `is_dirty()` still reports "either" for callers that
    /// only need a yes/no before bothering to call `dump_to_disk` at all.
    chunk_dirty: bool,
    /// Reflection index dirty since the last `dump_to_disk`. See `chunk_dirty`.
    reflection_dirty: bool,
    /// Whether this engine's in-memory maps are currently known to
    /// correspond to the manifest that is (or, the instant this becomes
    /// true, was just made to be) on disk — i.e. an UNDIRTIED side's
    /// in-memory content can be trusted to describe real, already-written
    /// bytes on disk, not just this process's private state. Two things
    /// establish that: successfully loading a manifest (`load_from_disk`
    /// sets this `true` on construction), and successfully PUBLISHING one
    /// (`dump_to_disk` sets this `true` at the very end of its success
    /// path, after the manifest rename lands — never on an early `?`
    /// return, since a half-finished dump has established nothing). A
    /// freshly rebuilt engine (`SearchEngine::new`, populated from SQLite by
    /// the caller — `Engine::new`'s cache-miss/rebuild path is the real
    /// example) starts `false` and stays `false` until ITS first successful
    /// dump; before that point it is authoritative for both sides
    /// (including one that is legitimately empty) and must never carry
    /// anything forward — see `manifest_only_carries_forward_on_a_manifest_backed_engine`.
    /// After that first dump, it's exactly as trustworthy as a
    /// `load_from_disk` engine for future untouched-side carry-forward.
    ///
    /// `dump_to_disk` uses this to decide whether an UNDIRTIED side may
    /// carry its manifest fields forward from the pre-existing on-disk
    /// manifest (safe only when `manifest_backed`) or must describe that
    /// side from memory (required when not: carrying forward a stale
    /// on-disk count for a side with zero DB rows would publish a manifest
    /// permanently describing an empty side as non-empty, which
    /// `load_from_disk`'s negative-drift check would then always reject,
    /// forcing a full rebuild on every single startup). See `dump_to_disk`'s
    /// doc comment for the full reasoning.
    manifest_backed: bool,
}

// HNSW parameters
const MAX_NB_CONNECTION: usize = 16; // M
const EF_CONSTRUCTION: usize = 200;
const EF_SEARCH: usize = 100;
const MAX_LAYER: usize = 16;

// Below this many points, bypass HNSW and scan exactly. HNSW is approximate and
// has misbehaved on near-empty indexes (CI: 1-point search returned no neighbours);
// exact cosine over ≤256 384-dim vectors is well under a millisecond anyway.
const EXACT_SCAN_THRESHOLD: usize = 256;

const MANIFEST_VERSION: u32 = 1;

/// Metadata for validating a cached HNSW index.
/// The `_expected` counts are DB row counts passed in from storage,
/// used for staleness detection on reload.
#[derive(Debug, Serialize, Deserialize)]
struct IndexManifest {
    version: u32,
    created_at: String,
    chunk_id_map: Vec<String>,
    reflection_id_map: Vec<String>,
    chunk_embeddings_expected: usize,
    reflection_embeddings_expected: usize,
    active_reflection_count: usize,
}

impl SearchEngine {
    pub fn new(estimated_size: usize) -> Self {
        Self {
            chunk_index: Hnsw::new(
                MAX_NB_CONNECTION,
                estimated_size,
                MAX_LAYER,
                EF_CONSTRUCTION,
                DistCosine {},
            ),
            reflection_index: Hnsw::new(
                MAX_NB_CONNECTION,
                estimated_size / 10,
                MAX_LAYER,
                EF_CONSTRUCTION,
                DistCosine {},
            ),
            chunk_id_map: Vec::new(),
            reflection_id_map: Vec::new(),
            chunk_id_set: HashSet::new(),
            reflection_id_set: HashSet::new(),
            active_reflection_count: 0,
            chunk_dirty: false,
            reflection_dirty: false,
            manifest_backed: false,
        }
    }

    pub fn insert_chunk(&mut self, id: String, embedding: Vec<f32>) {
        if !self.chunk_id_set.insert(id.clone()) {
            return; // Already indexed — skip duplicate
        }
        let idx = self.chunk_id_map.len();
        self.chunk_id_map.push(id);
        self.chunk_index.insert((&embedding, idx));
        self.chunk_dirty = true;
    }

    pub fn insert_reflection(&mut self, id: String, embedding: Vec<f32>) {
        if !self.reflection_id_set.insert(id.clone()) {
            return; // Already indexed — skip duplicate
        }
        let idx = self.reflection_id_map.len();
        self.reflection_id_map.push(id);
        self.reflection_index.insert((&embedding, idx));
        self.active_reflection_count += 1;
        self.reflection_dirty = true;
    }

    /// Remove a reflection from search results.
    /// Note: HNSW doesn't support true deletion, but we remove the ID mapping
    /// so search results won't include this reflection. The vector stays in the
    /// index but maps to nothing.
    pub fn remove_reflection(&mut self, id: &str) {
        let removed = if let Some(pos) = self.reflection_id_map.iter().position(|x| x == id) {
            self.reflection_id_map[pos] = String::new(); // Blank out the mapping
            self.active_reflection_count = self.active_reflection_count.saturating_sub(1);
            true
        } else {
            false
        };
        let removed_from_set = self.reflection_id_set.remove(id);
        if removed || removed_from_set {
            self.reflection_dirty = true;
        }
    }

    /// Remove a chunk from search results — same blank-the-mapping mechanism as
    /// [`Self::remove_reflection`] (HNSW has no true deletion). Needed by plan
    /// reimport: deleting the SQLite rows alone left the old vectors live, and
    /// re-inserting a reused deterministic id was skipped as a duplicate (Codex
    /// HIGH), so stale plan content kept matching forever.
    pub fn remove_chunk(&mut self, id: &str) {
        let removed = if let Some(pos) = self.chunk_id_map.iter().position(|x| x == id) {
            self.chunk_id_map[pos] = String::new();
            true
        } else {
            false
        };
        let removed_from_set = self.chunk_id_set.remove(id);
        if removed || removed_from_set {
            self.chunk_dirty = true;
        }
    }

    /// Check if a reflection ID exists in the index (non-blanked).
    pub fn has_reflection(&self, id: &str) -> bool {
        self.reflection_id_set.contains(id)
    }

    /// Check if a chunk ID is already present in the index.
    pub fn has_chunk(&self, id: &str) -> bool {
        self.chunk_id_set.contains(id)
    }

    /// Blank reflection IDs in the map that are not present in the given DB ID set.
    /// Returns the number of entries blanked. Marks index dirty if any were removed.
    pub fn blank_orphan_reflections(&mut self, db_ids: &std::collections::HashSet<&str>) -> usize {
        let mut blanked = 0;
        for entry in &mut self.reflection_id_map {
            if !entry.is_empty() && !db_ids.contains(entry.as_str()) {
                self.reflection_id_set.remove(entry.as_str());
                *entry = String::new();
                blanked += 1;
            }
        }
        if blanked > 0 {
            self.active_reflection_count = self.active_reflection_count.saturating_sub(blanked);
            self.reflection_dirty = true;
        }
        blanked
    }

    /// Search chunk index. Returns results sorted by descending score.
    pub fn search_chunks(
        &self,
        query_vec: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        if self.chunk_id_map.is_empty() {
            return Vec::new();
        }
        self.search_index(
            &self.chunk_index,
            &self.chunk_id_map,
            query_vec,
            limit,
            min_score,
        )
    }

    /// Search reflection index. Returns results sorted by descending score.
    pub fn search_reflections(
        &self,
        query_vec: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        if self.reflection_id_map.is_empty() {
            return Vec::new();
        }
        self.search_index(
            &self.reflection_index,
            &self.reflection_id_map,
            query_vec,
            limit,
            min_score,
        )
    }

    fn search_index(
        &self,
        index: &Hnsw<'static, f32, DistCosine>,
        id_map: &[String],
        query_vec: &[f32],
        limit: usize,
        min_score: f32,
    ) -> Vec<SearchResult> {
        if id_map.len() <= EXACT_SCAN_THRESHOLD {
            return Self::exact_scan(index, id_map, query_vec, limit, min_score, None);
        }
        let neighbours = index.search(query_vec, limit, EF_SEARCH);

        let mut results: Vec<SearchResult> = neighbours
            .into_iter()
            .filter_map(|n| {
                // hnsw_rs DistCosine returns distance = 1.0 - cosine_similarity
                let score = 1.0 - n.distance;
                if score >= min_score && n.d_id < id_map.len() && !id_map[n.d_id].is_empty() {
                    Some(SearchResult {
                        id: id_map[n.d_id].clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    /// Exact cosine scan over every point in the index. Used below
    /// EXACT_SCAN_THRESHOLD where HNSW's approximation isn't worth its
    /// nondeterminism and an exhaustive pass is effectively free.
    fn exact_scan(
        index: &Hnsw<'static, f32, DistCosine>,
        id_map: &[String],
        query_vec: &[f32],
        limit: usize,
        min_score: f32,
        allowed_ids: Option<&HashSet<String>>,
    ) -> Vec<SearchResult> {
        let mut results: Vec<SearchResult> = index
            .get_point_indexation()
            .into_iter()
            .filter_map(|point| {
                let d_id = point.get_origin_id();
                if d_id >= id_map.len() || id_map[d_id].is_empty() {
                    return None;
                }
                if let Some(allowed) = allowed_ids {
                    if !allowed.contains(&id_map[d_id]) {
                        return None;
                    }
                }
                let score = 1.0 - DistCosine {}.eval(point.get_v(), query_vec);
                if score >= min_score {
                    Some(SearchResult {
                        id: id_map[d_id].clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    pub fn chunk_count(&self) -> usize {
        self.chunk_id_map.len()
    }

    pub fn reflection_count(&self) -> usize {
        self.active_reflection_count
    }

    /// Whether either index has been modified since its last dump. Existing
    /// callers (daemon shutdown, watcher batch flush, MCP `flush_index`) use
    /// this as a cheap "is there anything to write at all" gate before
    /// calling `dump_to_disk`, which then only rewrites whichever side(s)
    /// are actually dirty — see `is_chunk_dirty`/`is_reflection_dirty` for
    /// callers that need to distinguish (e.g. the hook persistence gate).
    pub fn is_dirty(&self) -> bool {
        self.chunk_dirty || self.reflection_dirty
    }

    /// Whether the chunk index specifically needs a dump.
    pub fn is_chunk_dirty(&self) -> bool {
        self.chunk_dirty
    }

    /// Whether the reflection index specifically needs a dump.
    pub fn is_reflection_dirty(&self) -> bool {
        self.reflection_dirty
    }

    /// Search chunk index but only return results whose IDs are in `allowed_ids`.
    /// Used for project-scoped and time-range-scoped searches.
    pub fn search_chunks_filtered(
        &self,
        query_vec: &[f32],
        limit: usize,
        min_score: f32,
        allowed_ids: &HashSet<String>,
    ) -> Vec<SearchResult> {
        if self.chunk_id_map.is_empty() || allowed_ids.is_empty() {
            return Vec::new();
        }
        if self.chunk_id_map.len() <= EXACT_SCAN_THRESHOLD {
            return Self::exact_scan(
                &self.chunk_index,
                &self.chunk_id_map,
                query_vec,
                limit,
                min_score,
                Some(allowed_ids),
            );
        }

        // Adaptive over-fetch: start at 5x, escalate to full index if sparse
        let max_elements = self.chunk_id_map.len();
        let mut fetch_limit = (limit * 5).min(max_elements);

        let mut results = loop {
            let neighbours = self.chunk_index.search(query_vec, fetch_limit, EF_SEARCH);

            let found: Vec<SearchResult> = neighbours
                .into_iter()
                .filter_map(|n| {
                    let score = 1.0 - n.distance;
                    if score >= min_score && n.d_id < self.chunk_id_map.len() {
                        let id = &self.chunk_id_map[n.d_id];
                        if allowed_ids.contains(id) {
                            Some(SearchResult {
                                id: id.clone(),
                                score,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();

            // If we got enough results or already searched the full index, stop
            if found.len() >= limit || fetch_limit >= max_elements {
                break found;
            }
            // Escalate: try full index
            fetch_limit = max_elements;
        };

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    // ─── Persistence ───

    /// Serialize both HNSW indices and ID maps to disk.
    /// Uses atomic write (tmp + rename) for the manifest to prevent corruption.
    /// Advisory file lock (fs2) prevents concurrent dump/load from multiple processes.
    ///
    /// `db_chunk_count` and `db_reflection_count` are the current DB row counts
    /// from `count_chunk_embeddings()` / `count_reflection_embeddings()`.
    /// These are stored in the manifest for staleness detection on reload.
    ///
    /// Only rewrites the HNSW graph/data files for whichever side(s) are
    /// actually dirty (`chunk_dirty` / `reflection_dirty`) — a reflection-only
    /// mutation (e.g. `store_reflection`, the `stop` hook) leaves
    /// `chunks.hnsw.*` byte-identical on disk instead of paying to rewrite a
    /// multi-hundred-MB file that didn't change.
    ///
    /// The manifest is still rewritten in full every call — it is one JSON
    /// file describing BOTH indices — but a side this call did NOT dump may
    /// be described from the manifest that was already on disk instead of
    /// from this process's in-memory copy. That carry-forward requires BOTH:
    /// (1) this call didn't just dump that side, AND (2) `self.manifest_backed`
    /// — this engine came from `load_from_disk`, so its in-memory knowledge
    /// of an undirtied side traces back to a manifest, not to an independent
    /// rebuild. Without condition (2), carrying forward is unsound: `csr-engine`
    /// is multi-process (daemon, MCP server, hook processes share one on-disk
    /// cache directory), and a long-lived manifest-backed process — the MCP
    /// server is the standing example — loads the cache once and, in
    /// production, may only ever mutate the reflection side
    /// (`store_reflection`). If the daemon later imports more chunks and
    /// dumps a bigger chunk graph, that process's in-memory `chunk_id_map` is
    /// now stale-but-smaller than what's actually on disk; publishing it from
    /// memory (paired with a freshly queried `db_chunk_count` reflecting the
    /// daemon's larger DB state) would pair a smaller id map with a bigger
    /// on-disk graph, and the next `Engine::new` load's backfill would insert
    /// at indices the real graph already has points at — `hnsw_rs` does not
    /// dedupe by the caller's origin id, so two distinct vectors collapse
    /// onto one `d_id` and search can silently return the wrong chunk with no
    /// error, no log line, nothing (see
    /// `dump_does_not_regress_an_untouched_sides_manifest_across_processes`).
    /// Carrying forward fixes that — but ONLY for a manifest-backed engine.
    /// An engine from `SearchEngine::new` (`Engine::new`'s cache-miss/rebuild
    /// path is the real example) is authoritative for BOTH sides after being
    /// populated from SQLite, including a side that is legitimately empty
    /// (e.g. zero reflections in the DB). If such an engine carried forward a
    /// stale non-empty on-disk `reflection_embeddings_expected` for that
    /// empty side instead of publishing the true (zero) count from memory,
    /// the manifest would permanently overstate that side; the next
    /// `load_from_disk` would then always see `expected_reflections (0) <
    /// manifest.reflection_embeddings_expected (stale, nonzero)`, take the
    /// negative-drift branch, return `None`, and force a full HNSW rebuild —
    /// on every single startup, forever. So a rebuilt (non-manifest-backed)
    /// engine always describes both sides from memory, dumped or not — see
    /// `manifest_only_carries_forward_on_a_manifest_backed_engine`.
    pub fn dump_to_disk(
        &mut self,
        dir: &Path,
        db_chunk_count: usize,
        db_reflection_count: usize,
    ) -> Result<()> {
        std::fs::create_dir_all(dir)?;

        // Advisory lock prevents concurrent dump/load from separate processes (H-2)
        let lock_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("index.lock"))?;
        lock_file
            .lock_exclusive()
            .map_err(|e| anyhow::anyhow!("failed to acquire index lock for dump: {}", e))?;

        // Snapshot whatever manifest is on disk BEFORE this call writes
        // anything, while still holding the exclusive lock — no other
        // process's dump_to_disk can be mid-write concurrently, so this is a
        // consistent read of "whatever the last writer actually published".
        // `None` covers three cases uniformly: no `manifest.json` yet
        // (first-ever dump), a manifest that fails to parse, and a manifest
        // whose `version` doesn't match `MANIFEST_VERSION` (a format we
        // don't understand). These are NOT "nothing on disk to disagree
        // with" — an absent/corrupt/unreadable manifest says nothing about
        // whether the actual `chunks.hnsw.*`/`reflections.hnsw.*` files out
        // there are current, stale, or from some other process's newer
        // dump. `force_full` below turns `None` into "rewrite both sides
        // from memory now, so whatever this call publishes is guaranteed to
        // match what's actually on disk" rather than risking the same
        // id-map/graph mismatch this whole fix exists to prevent.
        let existing_manifest: Option<IndexManifest> =
            std::fs::read_to_string(dir.join("manifest.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<IndexManifest>(&raw).ok())
                .filter(|m| m.version == MANIFEST_VERSION);
        let force_full = existing_manifest.is_none();

        // Dump HNSW graph + data files (skip empty indices — file_dump fails on empty;
        // skip untouched indices — nothing changed since the file on disk was last
        // written, so re-dumping would just burn I/O to produce the same bytes; but
        // NEVER skip a side under `force_full` — see the comment on `existing_manifest`
        // above. hnsw_rs may return a numbered basename (e.g. "chunks-7905") when mmap
        // is active (after load_from_disk). We promote it to the canonical name so
        // load_from_disk always finds "chunks.hnsw.data" / "reflections.hnsw.data".
        //
        // `chunk_dirty && chunk_id_map.is_empty()` can't actually happen —
        // `insert_chunk` only sets `chunk_dirty` after a non-empty push, and
        // `remove_chunk` only sets it after finding an existing (so
        // already-non-empty) entry — but the empty check stays as a guard
        // against `file_dump`'s documented failure on an empty index, and
        // `chunk_actually_dumped` names the guarded condition once so the
        // manifest logic below can ask "did we really just write this side"
        // without repeating it.
        let chunk_actually_dumped =
            (self.chunk_dirty || force_full) && !self.chunk_id_map.is_empty();
        if chunk_actually_dumped {
            let basename = self
                .chunk_index
                .file_dump(dir, "chunks")
                .map_err(|e| anyhow::anyhow!("chunk index dump failed: {}", e))?;
            promote_to_canonical(dir, &basename, "chunks")?;
        }

        let reflection_actually_dumped =
            (self.reflection_dirty || force_full) && !self.reflection_id_map.is_empty();
        if reflection_actually_dumped {
            let basename = self
                .reflection_index
                .file_dump(dir, "reflections")
                .map_err(|e| anyhow::anyhow!("reflection index dump failed: {}", e))?;
            promote_to_canonical(dir, &basename, "reflections")?;
        }

        // Chunk side of the manifest: from memory if we just dumped it (now
        // provably matching what's on disk), OR if this engine isn't
        // manifest-backed (a rebuild is authoritative for both sides —
        // carrying forward would be unsound, see the doc comment above).
        // Only a manifest-backed engine that did NOT dump this side carries
        // its fields forward from the pre-existing on-disk manifest. The
        // final `else let None` arm is reachable only when `chunk_id_map`
        // is genuinely empty — `force_full` already made `chunk_actually_dumped`
        // true for any non-empty map whenever `existing_manifest` was
        // untrustworthy, so there's no remaining path where we describe a
        // real (non-empty), unwritten side from an untrusted manifest.
        let (chunk_id_map, chunk_embeddings_expected) =
            if chunk_actually_dumped || !self.manifest_backed {
                (self.chunk_id_map.clone(), db_chunk_count)
            } else if let Some(prev) = &existing_manifest {
                (prev.chunk_id_map.clone(), prev.chunk_embeddings_expected)
            } else {
                (self.chunk_id_map.clone(), db_chunk_count)
            };

        // Same reasoning for the reflection side, including
        // `active_reflection_count` (also a description of on-disk state,
        // not a live counter — see its field doc).
        let (reflection_id_map, reflection_embeddings_expected, active_reflection_count) =
            if reflection_actually_dumped || !self.manifest_backed {
                (
                    self.reflection_id_map.clone(),
                    db_reflection_count,
                    self.active_reflection_count,
                )
            } else if let Some(prev) = &existing_manifest {
                (
                    prev.reflection_id_map.clone(),
                    prev.reflection_embeddings_expected,
                    prev.active_reflection_count,
                )
            } else {
                (
                    self.reflection_id_map.clone(),
                    db_reflection_count,
                    self.active_reflection_count,
                )
            };

        // Write manifest atomically (tmp + rename)
        let manifest = IndexManifest {
            version: MANIFEST_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
            chunk_id_map,
            reflection_id_map,
            chunk_embeddings_expected,
            reflection_embeddings_expected,
            active_reflection_count,
        };

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        let tmp_path = dir.join("manifest.json.tmp");
        let final_path = dir.join("manifest.json");
        std::fs::write(&tmp_path, &manifest_json)?;
        std::fs::rename(&tmp_path, &final_path)?;

        // Clean stale numbered HNSW files from previous dumps.
        // hnsw_rs creates numbered files (e.g. chunks-7905.hnsw.data) when mmap is active
        // on the old files (after load_from_disk). These are orphaned after each new dump.
        cleanup_stale_index_files(dir);

        // Lock is released when lock_file is dropped
        self.chunk_dirty = false;
        self.reflection_dirty = false;
        // The manifest we just published (`std::fs::rename` above already
        // succeeded — this line only runs on that success path, never on an
        // early `?` return) now describes this engine's in-memory maps for
        // BOTH sides: the side(s) we actually dumped this call by
        // construction, and any side we carried forward we copied verbatim
        // from a manifest that WAS trustworthy (see `existing_manifest`/
        // `force_full` above) — so after this write, `self.chunk_id_map`/
        // `self.reflection_id_map` are exactly what the manifest on disk
        // says, regardless of whether this engine started out manifest-backed.
        // A rebuilt (`SearchEngine::new`) engine that has never dumped is
        // NOT yet in this state — that's `manifest_only_carries_forward_on_a_manifest_backed_engine`
        // — but one that just published successfully is, from this point on,
        // exactly as trustworthy for a future untouched-side carry-forward as
        // one that loaded a manifest at construction. See the field doc.
        self.manifest_backed = true;
        Ok(())
    }

    /// Load HNSW indices from a cached directory.
    /// Returns `None` on any failure — caller falls back to full rebuild.
    ///
    /// Validates:
    /// - Manifest version matches
    /// - Chunk/reflection counts match current DB state (staleness detection)
    /// - All required files exist and load successfully
    ///
    /// **WARNING**: This function uses `Box::leak` for `HnswIo` objects to satisfy
    /// the `'static` lifetime requirement. Each call leaks ~200 bytes. Only call
    /// this once per process (at startup).
    pub fn load_from_disk(
        dir: &Path,
        expected_chunks: usize,
        expected_reflections: usize,
    ) -> Option<Self> {
        // Acquire shared advisory lock to avoid reading while dump_to_disk is writing.
        // The File is held as _lock_guard for the duration of the load.

        let lock_path = dir.join("index.lock");
        let _lock_guard = std::fs::OpenOptions::new()
            .read(true)
            .open(&lock_path)
            .ok()
            .and_then(|f| f.lock_shared().ok().map(|_| f));

        // Read and validate manifest
        let manifest_path = dir.join("manifest.json");
        let manifest_data = std::fs::read_to_string(&manifest_path).ok()?;
        let manifest: IndexManifest = serde_json::from_str(&manifest_data).ok()?;

        if manifest.version != MANIFEST_VERSION {
            tracing::info!(
                expected = MANIFEST_VERSION,
                found = manifest.version,
                "index cache version mismatch"
            );
            return None;
        }

        // Staleness is asymmetric. Chunks and reflections normally only grow, so an
        // ADDITIVE drift (db > cached) is cheap to reconcile: Engine::new loads this
        // cache and incrementally inserts the few new rows (~ms) instead of rebuilding
        // the whole HNSW (~tens of seconds). This avoids cache thrash when several
        // csr-engine processes import transcripts concurrently.
        //
        // A NEGATIVE drift (db < cached) means rows were deleted, so the cache holds
        // orphan vectors — fall back to a full rebuild for correctness.
        if expected_chunks < manifest.chunk_embeddings_expected {
            tracing::info!(
                cached = manifest.chunk_embeddings_expected,
                db = expected_chunks,
                "index cache stale (chunks removed) — rebuilding"
            );
            return None;
        }
        if expected_reflections < manifest.reflection_embeddings_expected {
            tracing::info!(
                cached = manifest.reflection_embeddings_expected,
                db = expected_reflections,
                "index cache stale (reflections removed) — rebuilding"
            );
            return None;
        }
        if expected_chunks > manifest.chunk_embeddings_expected
            || expected_reflections > manifest.reflection_embeddings_expected
        {
            tracing::info!(
                cached_chunks = manifest.chunk_embeddings_expected,
                db_chunks = expected_chunks,
                cached_reflections = manifest.reflection_embeddings_expected,
                db_reflections = expected_reflections,
                "index cache behind DB — loading + incremental backfill"
            );
        }

        // Load chunk index.
        // Box::leak gives the HnswIo a 'static lifetime required by load_hnsw.
        // Leaked memory is ~200 bytes total — negligible for process-lifetime objects.
        // Empty indices aren't dumped to disk, so create fresh ones if count is 0.
        let chunk_hnsw = if manifest.chunk_embeddings_expected > 0 {
            let chunk_io = Box::leak(Box::new(HnswIo::new(dir, "chunks")));
            match chunk_io.load_hnsw::<f32, DistCosine>() {
                Ok(hnsw) => hnsw,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load chunk HNSW from cache");
                    return None;
                }
            }
        } else {
            Hnsw::new(
                MAX_NB_CONNECTION,
                10_000,
                MAX_LAYER,
                EF_CONSTRUCTION,
                DistCosine {},
            )
        };

        let refl_hnsw = if manifest.reflection_embeddings_expected > 0 {
            let refl_io = Box::leak(Box::new(HnswIo::new(dir, "reflections")));
            match refl_io.load_hnsw::<f32, DistCosine>() {
                Ok(hnsw) => hnsw,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load reflection HNSW from cache");
                    return None;
                }
            }
        } else {
            Hnsw::new(
                MAX_NB_CONNECTION,
                1_000,
                MAX_LAYER,
                EF_CONSTRUCTION,
                DistCosine {},
            )
        };

        // Rebuild ID sets from the loaded maps
        let chunk_id_set: HashSet<String> = manifest
            .chunk_id_map
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        let reflection_id_set: HashSet<String> = manifest
            .reflection_id_map
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();

        // Use non-blank map count — orphans will be blanked by Engine::new reconciliation
        let active_count = reflection_id_set.len();

        Some(Self {
            chunk_index: chunk_hnsw,
            reflection_index: refl_hnsw,
            chunk_id_map: manifest.chunk_id_map,
            reflection_id_map: manifest.reflection_id_map,
            chunk_id_set,
            reflection_id_set,
            active_reflection_count: active_count,
            chunk_dirty: false,
            reflection_dirty: false,
            manifest_backed: true,
        })
    }
}

/// If `file_dump` returned a numbered basename (e.g. "chunks-7905"), rename its files
/// to the canonical name (e.g. "chunks.hnsw.data") so `load_from_disk` can find them.
/// This happens when the Hnsw was loaded via `load_hnsw` (mmap active → `overwrite=false`).
fn promote_to_canonical(dir: &Path, returned_basename: &str, canonical: &str) -> Result<()> {
    if returned_basename == canonical {
        return Ok(()); // Already canonical, nothing to do
    }
    for ext in &[".hnsw.data", ".hnsw.graph"] {
        let src = dir.join(format!("{}{}", returned_basename, ext));
        let dst = dir.join(format!("{}{}", canonical, ext));
        if src.exists() {
            std::fs::rename(&src, &dst).map_err(|e| {
                anyhow::anyhow!(
                    "failed to promote {} to {}: {}",
                    src.display(),
                    dst.display(),
                    e
                )
            })?;
        }
    }
    Ok(())
}

/// Remove stale numbered HNSW files from the index directory.
/// hnsw_rs creates numbered files (e.g. `chunks-7905.hnsw.data`) when mmap is active.
/// After `dump_to_disk` promotes them to canonical names, the numbered copies are gone.
/// This function catches any stragglers from crashes, concurrent processes, or old sessions.
///
/// Called after every `dump_to_disk` and at engine startup.
/// Must be called while holding `index.lock` (dump_to_disk) or at startup before serving.
pub fn cleanup_stale_index_files(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Match pattern: chunks-NNN.hnsw.data/graph or reflections-NNN.hnsw.data/graph
        // Keep: chunks.hnsw.data, reflections.hnsw.graph, manifest.json, index.lock
        if (name.starts_with("chunks-") || name.starts_with("reflections-"))
            && (name.ends_with(".hnsw.data") || name.ends_with(".hnsw.graph"))
        {
            let _ = std::fs::remove_file(entry.path());
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "cleaned stale numbered HNSW files");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only file identity check, mirroring `import::registry::file_identity`
    /// (the portable-identity pattern this repo already uses for "did this file
    /// get replaced" checks — see PR #271). Used below to prove a reflection-only
    /// dump does not rewrite `chunks.hnsw.*`, not just leave its byte content
    /// coincidentally equal.
    #[cfg(unix)]
    fn test_file_identity(metadata: &std::fs::Metadata) -> u64 {
        use std::os::unix::fs::MetadataExt;
        metadata.ino()
    }
    #[cfg(windows)]
    fn test_file_identity(metadata: &std::fs::Metadata) -> u64 {
        use std::os::windows::fs::MetadataExt;
        metadata.creation_time()
    }

    #[test]
    fn reflection_only_dump_leaves_chunk_files_untouched() {
        // Regression for the split-dirty-flag fix: before this, a single
        // `dirty` bool covering both indices meant any mutation — even a
        // reflection-only insert from `store_reflection` or the `stop` hook —
        // rewrote the (usually far larger) chunk HNSW files too.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        let mut engine = SearchEngine::new(100);
        for i in 0..5 {
            engine.insert_chunk(format!("c{i}"), vec![(i as f32) / 5.0; 384]);
        }
        engine.insert_reflection("r0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 5, 1).unwrap();

        let chunk_data_path = dir.join("chunks.hnsw.data");
        let chunk_graph_path = dir.join("chunks.hnsw.graph");
        let before_data = std::fs::metadata(&chunk_data_path).unwrap();
        let before_graph = std::fs::metadata(&chunk_graph_path).unwrap();
        let before_data_snapshot = (
            before_data.len(),
            before_data.modified().unwrap(),
            test_file_identity(&before_data),
        );
        let before_graph_snapshot = (
            before_graph.len(),
            before_graph.modified().unwrap(),
            test_file_identity(&before_graph),
        );

        // Give the filesystem clock room to move — some filesystems have
        // coarse mtime granularity, and a false "unchanged" would make this
        // test vacuous.
        std::thread::sleep(std::time::Duration::from_millis(20));

        engine.insert_reflection("r1".into(), vec![0.6; 384]);
        assert!(
            !engine.is_chunk_dirty(),
            "a reflection insert must not mark the chunk index dirty"
        );
        assert!(engine.is_reflection_dirty());
        engine.dump_to_disk(dir, 5, 2).unwrap();

        let after_data = std::fs::metadata(&chunk_data_path).unwrap();
        let after_graph = std::fs::metadata(&chunk_graph_path).unwrap();
        assert_eq!(
            before_data_snapshot,
            (
                after_data.len(),
                after_data.modified().unwrap(),
                test_file_identity(&after_data)
            ),
            "chunks.hnsw.data was rewritten by a reflection-only dump"
        );
        assert_eq!(
            before_graph_snapshot,
            (
                after_graph.len(),
                after_graph.modified().unwrap(),
                test_file_identity(&after_graph)
            ),
            "chunks.hnsw.graph was rewritten by a reflection-only dump"
        );

        // The index must still round-trip correctly: both the untouched
        // chunk side and the freshly-dumped reflection side load and search.
        let loaded = SearchEngine::load_from_disk(dir, 5, 2)
            .expect("reload after a partial (reflection-only) dump must succeed");
        assert!(loaded.has_chunk("c0") && loaded.has_chunk("c4"));
        assert!(loaded.has_reflection("r0") && loaded.has_reflection("r1"));

        let chunk_query = vec![0.2f32; 384]; // == c1's exact embedding
        let chunk_hits = loaded.search_chunks(&chunk_query, 5, -1.0);
        assert!(
            chunk_hits.iter().any(|r| r.id == "c1"),
            "chunk index unreadable/incomplete after reflection-only dump: {chunk_hits:?}"
        );

        let refl_query = vec![0.6f32; 384]; // == r1's exact embedding
        let refl_hits = loaded.search_reflections(&refl_query, 5, -1.0);
        assert!(
            refl_hits.iter().any(|r| r.id == "r1"),
            "newly-dumped reflection r1 not searchable after reload: {refl_hits:?}"
        );
    }

    /// Distinguishable, deterministic per-index embedding — every vector
    /// must be far enough from every other that each chunk's own vector is
    /// unambiguously its own nearest neighbour, so a `d_id` collision shows
    /// up as "resolved to the WRONG chunk" rather than a tie.
    fn distinct_embedding(i: usize) -> Vec<f32> {
        (0..384)
            .map(|j| (((i * 97 + j) as f32) * 0.013).sin())
            .collect()
    }

    #[test]
    fn dump_does_not_regress_an_untouched_sides_manifest_across_processes() {
        // Regression for a cross-process manifest-divergence bug (adversarial
        // review on this fix): `csr-engine` is multi-process — daemon, MCP
        // server, and hook processes all read/write the SAME on-disk cache
        // directory. A long-lived process (the MCP server is the standing
        // example) loads the chunk graph once and, in production, may only
        // ever mutate the reflection side. If the daemon later dumps a
        // BIGGER chunk graph, and that long-lived process then flushes for
        // an unrelated reflection-only reason, it must NOT publish its own
        // stale (smaller) in-memory `chunk_id_map` into the manifest — that
        // would pair a smaller id map with the real bigger on-disk graph,
        // and the next `Engine::new` load's additive backfill would insert
        // at indices the graph already has real points at (`hnsw_rs` does
        // not dedupe by caller-supplied origin id), collapsing two distinct
        // vectors onto one `d_id` — a silent wrong search result with no
        // error and no log line.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // "Process A" (e.g. the daemon) writes the first chunk generation.
        let mut proc_a = SearchEngine::new(100);
        for i in 0..5 {
            proc_a.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        proc_a.dump_to_disk(dir, 5, 0).unwrap();

        // "Process B" (e.g. a long-lived MCP server) loads that 5-chunk
        // state and never touches the chunk side again.
        let mut proc_b = SearchEngine::load_from_disk(dir, 5, 0).unwrap();

        // Meanwhile "process A" imports 3 more chunks and dumps again — the
        // on-disk graph now has 8 points, but `proc_b`'s in-memory
        // `chunk_id_map` is still stuck at 5.
        for i in 5..8 {
            proc_a.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        proc_a.dump_to_disk(dir, 8, 0).unwrap();

        // "Process B" does something reflection-only (e.g. `store_reflection`)
        // and flushes. Its chunk side is not dirty, so the chunk graph FILE
        // must not be rewritten — but the manifest it publishes must
        // describe the REAL 8-point graph, not `proc_b`'s stale 5-entry view.
        // `db_chunk_count = 8` here stands in for a freshly queried live DB
        // count, exactly as `Engine::flush_index` supplies it — the whole
        // bug was that count disagreeing with a stale in-memory id map.
        proc_b.insert_reflection("r0".into(), distinct_embedding(1000));
        assert!(!proc_b.is_chunk_dirty());
        proc_b.dump_to_disk(dir, 8, 1).unwrap();

        let manifest_raw = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
        let published_chunk_map = manifest["chunk_id_map"].as_array().unwrap();
        assert_eq!(
            published_chunk_map.len(),
            8,
            "process B's reflection-only flush regressed the manifest's chunk_id_map \
             to its own stale 5-entry view instead of carrying forward the real 8-entry \
             on-disk state"
        );

        // Reload fresh and prove every original AND new chunk id still
        // resolves to ITS OWN vector — this is the assertion that would
        // have caught the silent wrong-answer bug.
        let reloaded = SearchEngine::load_from_disk(dir, 8, 1).unwrap();
        assert_eq!(reloaded.chunk_count(), 8);
        for i in 0..8 {
            let q = distinct_embedding(i);
            let hits = reloaded.search_chunks(&q, 1, -1.0);
            assert_eq!(
                hits.first().map(|h| h.id.as_str()),
                Some(format!("c{i}").as_str()),
                "chunk c{i} did not resolve to itself after reload — \
                 possible d_id collision from a regressed manifest"
            );
        }
    }

    #[test]
    fn manifest_only_carries_forward_on_a_manifest_backed_engine() {
        // Regression for CodeRabbit's finding on the fix above: the
        // carry-forward must NOT apply to a freshly rebuilt (non
        // manifest-backed) engine. `Engine::new`'s cache-miss path builds a
        // `SearchEngine::new`, inserts whatever SQLite has, and dumps
        // unconditionally. If the DB genuinely has zero reflections, nothing
        // is inserted for that side, so `reflection_dirty` stays false — but
        // that engine is still AUTHORITATIVE for the reflection side (it is
        // empty, on purpose), not merely "didn't touch it this call" the way
        // a manifest-backed engine would be. Carrying forward a stale
        // nonzero on-disk count here would publish a manifest claiming
        // reflections that don't exist, and every subsequent
        // `load_from_disk` would see `expected (0) < manifest (stale,
        // nonzero)`, take the negative-drift branch, return `None`, and
        // force a full rebuild — forever, on every single startup.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Seed an on-disk manifest describing a populated reflection side
        // (simulates a prior generation of the cache that had reflections).
        let mut seed = SearchEngine::new(100);
        assert!(
            !seed.manifest_backed,
            "SearchEngine::new must never be manifest-backed at construction"
        );
        for i in 0..5 {
            seed.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        for i in 0..3 {
            seed.insert_reflection(format!("r{i}"), distinct_embedding(1000 + i));
        }
        // `seed` itself becomes manifest-backed after this — expected, per
        // `manifest_becomes_backed_after_a_rebuilt_engines_own_dump` — but
        // `seed` is discarded after this point; only `rebuilt` below (which
        // has NOT yet dumped) is the engine under test for this case.
        seed.dump_to_disk(dir, 5, 3).unwrap();

        // A freshly constructed (NOT manifest-backed) engine — standing in
        // for `Engine::new`'s rebuild path — populated from a DB that has
        // the same 5 chunks but genuinely zero reflections now.
        let mut rebuilt = SearchEngine::new(100);
        for i in 0..5 {
            rebuilt.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        // No insert_reflection calls — reflection_dirty stays false, exactly
        // like a DB with zero reflection rows.
        assert!(!rebuilt.is_reflection_dirty());
        assert!(!rebuilt.manifest_backed);
        rebuilt.dump_to_disk(dir, 5, 0).unwrap();

        let manifest_raw = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
        assert_eq!(
            manifest["reflection_embeddings_expected"].as_u64(),
            Some(0),
            "a rebuilt (non manifest-backed) engine must publish the TRUE (zero) \
             reflection count, not carry forward the stale on-disk 3 from the seed manifest"
        );
        assert_eq!(
            manifest["reflection_id_map"].as_array().map(Vec::len),
            Some(0),
            "rebuilt engine's empty reflection_id_map must not be replaced by the \
             seed manifest's 3-entry map"
        );

        // The assertion that proves the permanent-rebuild-loop bug is gone:
        // load_from_disk must ACCEPT this manifest (0 == 0, no drift) rather
        // than reject it as negative drift (0 < stale-3) and force a rebuild.
        assert!(
            SearchEngine::load_from_disk(dir, 5, 0).is_some(),
            "load_from_disk rejected a correctly-published zero-reflection manifest as \
             negative drift — this is the permanent full-rebuild-on-every-startup bug"
        );

        // A rebuilt engine becomes manifest-backed the instant its own dump
        // publishes successfully — it is now exactly as trustworthy for a
        // future untouched-side carry-forward as one that loaded a manifest
        // at construction. See `manifest_becomes_backed_after_a_rebuilt_engines_own_dump`
        // for the cross-process regression this enables.
        assert!(
            rebuilt.manifest_backed,
            "a rebuilt engine must become manifest-backed after its own successful dump"
        );
    }

    #[test]
    fn manifest_becomes_backed_after_a_rebuilt_engines_own_dump() {
        // Regression for a second CodeRabbit finding on the same fix:
        // `manifest_backed` must flip true not just when LOADED from a
        // manifest, but also the instant this engine PUBLISHES one
        // successfully. Without that, a rebuilt engine (`Engine::new`'s
        // cache-miss path is the real example) that stays alive for the
        // life of the process — a long-lived MCP server that just happened
        // to start with a cold cache — would NEVER become trustworthy for
        // carry-forward, and every later reflection-only flush would keep
        // regressing the chunk side to its own first-dump snapshot forever,
        // exactly the cross-process bug
        // `dump_does_not_regress_an_untouched_sides_manifest_across_processes`
        // fixed for a `load_from_disk`-constructed engine — this is the same
        // bug through the other constructor.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // "Process B", but rebuilt rather than loaded (e.g. Engine::new hit
        // a cache miss and built fresh from SQLite): 5 chunks, 1 reflection,
        // NOT manifest-backed until its own dump below.
        let mut proc_b = SearchEngine::new(100);
        for i in 0..5 {
            proc_b.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        proc_b.insert_reflection("r0".into(), distinct_embedding(500));
        assert!(!proc_b.manifest_backed);
        proc_b.dump_to_disk(dir, 5, 1).unwrap();
        assert!(proc_b.manifest_backed);

        // "Process A" (e.g. the daemon) grows the chunk graph to 8 points
        // and publishes its own newer generation — proc_b never sees this.
        let mut proc_a = SearchEngine::new(100);
        for i in 0..8 {
            proc_a.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        proc_a.dump_to_disk(dir, 8, 0).unwrap();

        // "Process B" does a reflection-only mutation and flushes. Its
        // chunk side is not dirty; this must NOT regress the manifest's
        // chunk map to proc_b's stale 5-entry view over process A's real
        // 8-point graph.
        proc_b.insert_reflection("r1".into(), distinct_embedding(999));
        assert!(!proc_b.is_chunk_dirty());
        proc_b.dump_to_disk(dir, 8, 2).unwrap();

        let manifest_raw = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
        assert_eq!(
            manifest["chunk_id_map"].as_array().unwrap().len(),
            8,
            "proc_b's reflection-only flush regressed the manifest's chunk_id_map to its \
             own stale 5-entry view — manifest_backed did not carry forward from its own \
             prior successful dump"
        );

        let reloaded = SearchEngine::load_from_disk(dir, 8, 2).unwrap();
        assert_eq!(reloaded.chunk_count(), 8);
        for i in 0..8 {
            let hits = reloaded.search_chunks(&distinct_embedding(i), 1, -1.0);
            assert_eq!(
                hits.first().map(|h| h.id.as_str()),
                Some(format!("c{i}").as_str()),
                "chunk c{i} did not resolve to itself after reload — \
                 possible d_id collision from a regressed manifest"
            );
        }
    }

    /// Runs the FIX-2 scenario (untrusted manifest forces a full re-dump)
    /// against one way `existing_manifest` can end up `None`: `corrupt`
    /// mutates `manifest.json` in place after a trustworthy 8-chunk/2-reflection
    /// generation is already on disk.
    fn assert_untrusted_manifest_forces_full_dump(corrupt: impl FnOnce(&Path)) {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // "Process A" writes a trustworthy first generation (5 chunks, 2
        // reflections) — "process B" below loads exactly this and becomes
        // manifest-backed from it.
        let mut proc_a = SearchEngine::new(100);
        for i in 0..5 {
            proc_a.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        for i in 0..2 {
            proc_a.insert_reflection(format!("r{i}"), distinct_embedding(500 + i));
        }
        proc_a.dump_to_disk(dir, 5, 2).unwrap();

        let mut proc_b = SearchEngine::load_from_disk(dir, 5, 2).unwrap();
        assert!(proc_b.manifest_backed);

        // "Process A" grows the chunk graph to 8 points and dumps again —
        // this newer generation, with its own trustworthy manifest, is what
        // ends up "on disk" before we make the manifest untrustworthy below.
        for i in 5..8 {
            proc_a.insert_chunk(format!("c{i}"), distinct_embedding(i));
        }
        proc_a.dump_to_disk(dir, 8, 2).unwrap();

        // Make the manifest untrustworthy (absent / unparseable / wrong
        // version, depending on `corrupt`) while the newer 8-point chunk
        // graph FILES are still sitting on disk, unreadable-manifest and all.
        corrupt(dir);

        // "Process B" (still only aware of its own 5-chunk state — it never
        // saw process A's second dump) does a reflection-only mutation and
        // flushes. Its chunk side is not dirty, but the manifest it's about
        // to publish can't be trusted (per `corrupt`), so this must NOT
        // publish proc_b's stale 5-entry chunk map against the newer 8-point
        // graph file left on disk — it must rewrite the chunk file from
        // memory too, so file and manifest agree.
        proc_b.insert_reflection("r-new".into(), distinct_embedding(999));
        assert!(!proc_b.is_chunk_dirty());
        proc_b.dump_to_disk(dir, 8, 3).unwrap();

        let manifest_raw = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
        let published_len = manifest["chunk_id_map"].as_array().unwrap().len();
        assert_eq!(
            published_len,
            proc_b.chunk_id_map.len(),
            "published chunk_id_map must match what proc_b actually just wrote to disk \
             (its own memory), not the stale/unwritten newer graph"
        );
        assert_eq!(
            published_len, 5,
            "under an untrusted manifest, the chunk side must be forced to re-dump from \
             proc_b's own memory (5 chunks), not silently left as process A's 8"
        );

        // The chunk graph FILE itself must now actually hold proc_b's 5
        // points — not just the manifest number — proving a real re-dump
        // happened rather than merely publishing a smaller claimed count
        // over untouched bigger files. Reload with `expected_chunks = 8`
        // (matching the `db_chunk_count` proc_b's dump was called with,
        // which is stored in the manifest independently of `chunk_id_map`'s
        // length — that gap is exactly what `Engine::new`'s additive
        // backfill exists to reconcile on the next real load) so this
        // reload takes the normal "cache behind DB" path rather than being
        // rejected as negative drift for an unrelated reason.
        let reloaded = SearchEngine::load_from_disk(dir, 8, 3)
            .expect("manifest and chunk graph file must agree after the forced re-dump");
        assert_eq!(reloaded.chunk_count(), 5);
        for i in 0..5 {
            let hits = reloaded.search_chunks(&distinct_embedding(i), 1, -1.0);
            assert_eq!(
                hits.first().map(|h| h.id.as_str()),
                Some(format!("c{i}").as_str())
            );
        }
    }

    #[test]
    fn untrusted_manifest_forces_full_dump_when_absent() {
        assert_untrusted_manifest_forces_full_dump(|dir| {
            std::fs::remove_file(dir.join("manifest.json")).unwrap();
        });
    }

    #[test]
    fn untrusted_manifest_forces_full_dump_when_unparseable() {
        assert_untrusted_manifest_forces_full_dump(|dir| {
            std::fs::write(dir.join("manifest.json"), b"{ not valid json at all").unwrap();
        });
    }

    #[test]
    fn untrusted_manifest_forces_full_dump_when_version_mismatched() {
        assert_untrusted_manifest_forces_full_dump(|dir| {
            let raw = std::fs::read_to_string(dir.join("manifest.json")).unwrap();
            let mut v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            v["version"] = serde_json::json!(MANIFEST_VERSION + 1);
            std::fs::write(dir.join("manifest.json"), v.to_string()).unwrap();
        });
    }

    #[test]
    fn tiny_index_search_never_empty() {
        // Regression: CI flake in test_session_end_v3_extraction — HNSW search on a
        // 1-point index intermittently returned no neighbours. Same degenerate case
        // hits fresh installs: first reflection stored, first search finds nothing.
        for i in 0..500 {
            let mut engine = SearchEngine::new(100);
            let v: Vec<f32> = (0..384)
                .map(|j| (((i * 384 + j) as f32) * 0.01).sin())
                .collect();
            engine.insert_reflection(format!("r{i}"), v.clone());
            let results = engine.search_reflections(&v, 5, 0.1);
            assert!(
                !results.is_empty(),
                "iteration {i}: self-query on 1-point reflection index returned empty"
            );
            let mut engine2 = SearchEngine::new(100);
            engine2.insert_chunk(format!("c{i}"), v.clone());
            let results2 = engine2.search_chunks(&v, 5, 0.1);
            assert!(
                !results2.is_empty(),
                "iteration {i}: self-query on 1-point chunk index returned empty"
            );
        }
    }

    #[test]
    fn tiny_index_exact_scan_skips_blanked_and_respects_threshold() {
        let mut engine = SearchEngine::new(100);
        let a: Vec<f32> = (0..384).map(|j| (j as f32 * 0.01).sin()).collect();
        let b: Vec<f32> = (0..384).map(|j| (j as f32 * 0.01).cos()).collect();
        engine.insert_reflection("keep".into(), a.clone());
        engine.insert_reflection("gone".into(), b.clone());
        engine.remove_reflection("gone");

        let results = engine.search_reflections(&a, 5, 0.1);
        assert!(results.iter().any(|r| r.id == "keep"));
        assert!(
            !results.iter().any(|r| r.id.is_empty() || r.id == "gone"),
            "blanked entries must not surface in exact-scan path"
        );
        // Impossible threshold → empty, threshold still respected on exact path.
        assert!(engine.search_reflections(&a, 5, 1.01).is_empty());
    }

    #[test]
    fn test_load_allows_additive_drift_rebuilds_on_deletion() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("index.lock"), "").unwrap();

        let mut engine = SearchEngine::new(100);
        for i in 0..5 {
            engine.insert_chunk(format!("c{i}"), vec![i as f32 / 5.0; 384]);
        }
        engine.insert_reflection("r0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 5, 1).unwrap();

        // Exact match → loads.
        assert!(SearchEngine::load_from_disk(dir, 5, 1).is_some());
        // Additive drift (DB grew since dump) → loads; Engine::new backfills the new rows.
        assert!(
            SearchEngine::load_from_disk(dir, 8, 3).is_some(),
            "additive drift must load the cache, not rebuild"
        );
        // Negative drift (rows deleted) → None so the caller does a clean full rebuild.
        assert!(
            SearchEngine::load_from_disk(dir, 4, 1).is_none(),
            "chunk deletion must force a rebuild"
        );
        assert!(
            SearchEngine::load_from_disk(dir, 5, 0).is_none(),
            "reflection deletion must force a rebuild"
        );
    }

    #[test]
    fn test_cleanup_removes_numbered_keeps_base() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Create stale numbered files
        std::fs::write(dir.join("chunks-100.hnsw.data"), "old").unwrap();
        std::fs::write(dir.join("chunks-100.hnsw.graph"), "old").unwrap();
        std::fs::write(dir.join("chunks-200.hnsw.data"), "old").unwrap();
        std::fs::write(dir.join("reflections-50.hnsw.data"), "old").unwrap();
        std::fs::write(dir.join("reflections-50.hnsw.graph"), "old").unwrap();

        // Create base files that should be kept
        std::fs::write(dir.join("chunks.hnsw.data"), "current").unwrap();
        std::fs::write(dir.join("chunks.hnsw.graph"), "current").unwrap();
        std::fs::write(dir.join("reflections.hnsw.data"), "current").unwrap();
        std::fs::write(dir.join("reflections.hnsw.graph"), "current").unwrap();
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        std::fs::write(dir.join("index.lock"), "").unwrap();

        cleanup_stale_index_files(dir);

        // Numbered files should be gone
        assert!(!dir.join("chunks-100.hnsw.data").exists());
        assert!(!dir.join("chunks-200.hnsw.data").exists());
        assert!(!dir.join("reflections-50.hnsw.data").exists());

        // Base files should remain
        assert!(dir.join("chunks.hnsw.data").exists());
        assert!(dir.join("chunks.hnsw.graph").exists());
        assert!(dir.join("reflections.hnsw.data").exists());
        assert!(dir.join("manifest.json").exists());
        assert!(dir.join("index.lock").exists());
    }

    #[test]
    fn test_promote_renames_numbered_to_canonical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Simulate hnsw_rs creating numbered files
        std::fs::write(dir.join("chunks-4567.hnsw.data"), "new_data").unwrap();
        std::fs::write(dir.join("chunks-4567.hnsw.graph"), "new_graph").unwrap();
        // Old canonical files exist
        std::fs::write(dir.join("chunks.hnsw.data"), "old_data").unwrap();
        std::fs::write(dir.join("chunks.hnsw.graph"), "old_graph").unwrap();

        let _ = promote_to_canonical(dir, "chunks-4567", "chunks");

        // Canonical files should have the new content
        assert_eq!(
            std::fs::read_to_string(dir.join("chunks.hnsw.data")).unwrap(),
            "new_data"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("chunks.hnsw.graph")).unwrap(),
            "new_graph"
        );
        // Numbered files should be gone (renamed)
        assert!(!dir.join("chunks-4567.hnsw.data").exists());
        assert!(!dir.join("chunks-4567.hnsw.graph").exists());
    }

    #[test]
    fn test_promote_noop_when_already_canonical() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("chunks.hnsw.data"), "data").unwrap();

        let _ = promote_to_canonical(dir, "chunks", "chunks"); // should not panic or corrupt
        assert_eq!(
            std::fs::read_to_string(dir.join("chunks.hnsw.data")).unwrap(),
            "data"
        );
    }

    #[test]
    fn test_cleanup_no_panic_on_empty_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        cleanup_stale_index_files(tmp.path()); // should not panic
    }

    #[test]
    fn test_cleanup_no_panic_on_nonexistent_dir() {
        cleanup_stale_index_files(Path::new("/nonexistent/path")); // should not panic
    }
}
