pub mod code_rank;
pub mod cross_project;
pub mod decay;
pub mod reinstatement;
pub mod rerank;

use std::collections::HashSet;
use std::io::Write;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use fs2::FileExt;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use hnsw_rs::hnswio::ReloadOptions;
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

// hnsw_rs has no true deletion: `remove_chunk`/`remove_reflection`/
// `blank_orphan_reflections` only blank the id-map slot, so a rewritten chunk
// leaves a dead point in the graph that can still win a nearest-neighbour
// slot. `search_index` overfetches by the number of dead points (capped here)
// so tombstones near the query don't eat live result slots. The cap bounds the
// cost, so it also bounds the promise: a query with more than this many dead
// points nearer than its live results can still come back short.
const TOMBSTONE_OVERFETCH_CAP: usize = 256;

/// Neighbours to ask hnsw_rs for so that `limit` live results survive the
/// dead-id filter. Exactly `limit` when nothing is dead.
fn fetch_budget(limit: usize, dead: usize) -> usize {
    limit + dead.min(TOMBSTONE_OVERFETCH_CAP)
}

const MANIFEST_VERSION: u32 = 2;
const LEGACY_MANIFEST_VERSION: u32 = 1;
static INDEX_GENERATION: AtomicU64 = AtomicU64::new(0);

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
    #[serde(default = "default_chunk_basename")]
    chunk_basename: String,
    #[serde(default = "default_reflection_basename")]
    reflection_basename: String,
}

fn default_chunk_basename() -> String {
    "chunks".to_string()
}

fn default_reflection_basename() -> String {
    "reflections".to_string()
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
        // O(1): chunk_id_set holds exactly the live (non-blanked) ids — kept in
        // sync by insert_chunk/remove_chunk and rebuilt from the non-empty
        // entries of the loaded manifest on `load_from_disk`. There is no
        // orphan-blanking pass for chunks (unlike reflections), so this stays
        // accurate without a dedicated counter.
        let dead = self
            .chunk_id_map
            .len()
            .saturating_sub(self.chunk_id_set.len());
        self.search_index(
            &self.chunk_index,
            &self.chunk_id_map,
            query_vec,
            limit,
            min_score,
            dead,
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
        // O(1): active_reflection_count is already the accurate live count,
        // kept correct across insert/remove/blank_orphan_reflections and load.
        let dead = self
            .reflection_id_map
            .len()
            .saturating_sub(self.active_reflection_count);
        self.search_index(
            &self.reflection_index,
            &self.reflection_id_map,
            query_vec,
            limit,
            min_score,
            dead,
        )
    }

    /// `dead` is the number of blanked (tombstoned) entries in `id_map`. When
    /// `dead == 0` this asks hnsw_rs for exactly `limit` neighbours — same
    /// call, same result set as before tombstone overfetch existed — so a
    /// freshly built/loaded index with no rewrites is byte-identical.
    fn search_index(
        &self,
        index: &Hnsw<'static, f32, DistCosine>,
        id_map: &[String],
        query_vec: &[f32],
        limit: usize,
        min_score: f32,
        dead: usize,
    ) -> Vec<SearchResult> {
        if id_map.len() <= EXACT_SCAN_THRESHOLD {
            return Self::exact_scan(index, id_map, query_vec, limit, min_score, None);
        }
        let neighbours = index.search(query_vec, fetch_budget(limit, dead), EF_SEARCH);

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
        // whether the generations out there are current, stale, or from some
        // other process's newer dump. `force_full` below turns `None` into
        // "rewrite both sides from memory now, so whatever this call
        // publishes is guaranteed to match what's actually on disk" rather
        // than risking the id-map/graph mismatch this fix exists to prevent.
        let existing_manifest: Option<IndexManifest> =
            std::fs::read_to_string(dir.join("manifest.json"))
                .ok()
                .and_then(|raw| serde_json::from_str::<IndexManifest>(&raw).ok())
                .filter(|m| m.version == MANIFEST_VERSION);
        let force_full = existing_manifest.is_none();

        // Dump a side to a NEW generation only when it actually changed (skip empty
        // indices — the generation dump fails on empty; skip untouched indices —
        // nothing changed since the generation on disk was last written, so
        // re-dumping would just burn I/O to produce the same bytes), but NEVER skip a
        // side under `force_full` — see the comment on `existing_manifest` above.
        // Neither fresh nor mmap-backed indexes may overwrite files referenced by the
        // currently committed manifest, which is why each write goes to a new
        // generation rather than over the existing one.
        //
        // The `!id_map.is_empty()` guard remains the authoritative "a generation
        // exists" signal — `load_from_disk` MUST gate its load on the same persisted
        // id map, not on the DB counts recorded below, or it can load a stale
        // generation this dump never wrote. Keep the two in lockstep. The basename
        // must likewise track the SAME source as the id map chosen below, or the
        // manifest would point at one generation while describing another: a side we
        // just wrote takes its fresh basename, a side we skipped keeps whatever
        // generation the previous manifest referenced so `cleanup_stale_index_files`
        // still sees it as referenced.
        let chunk_actually_dumped =
            (self.chunk_dirty || force_full) && !self.chunk_id_map.is_empty();
        let chunk_basename = if chunk_actually_dumped {
            dump_hnsw_generation(&self.chunk_index, dir, "chunks")
                .map_err(|e| anyhow::anyhow!("chunk index dump failed: {}", e))?
        } else if self.manifest_backed {
            existing_manifest
                .as_ref()
                .map(|prev| prev.chunk_basename.clone())
                .unwrap_or_else(default_chunk_basename)
        } else {
            default_chunk_basename()
        };

        let reflection_actually_dumped =
            (self.reflection_dirty || force_full) && !self.reflection_id_map.is_empty();
        let reflection_basename = if reflection_actually_dumped {
            dump_hnsw_generation(&self.reflection_index, dir, "reflections")
                .map_err(|e| anyhow::anyhow!("reflection index dump failed: {}", e))?
        } else if self.manifest_backed {
            existing_manifest
                .as_ref()
                .map(|prev| prev.reflection_basename.clone())
                .unwrap_or_else(default_reflection_basename)
        } else {
            default_reflection_basename()
        };

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
            chunk_basename,
            reflection_basename,
        };

        write_manifest_atomically(dir, &manifest)?;

        // Clean numbered generations not referenced by the newly committed manifest.
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
    /// **WARNING**: On full success the `HnswIo` objects are leaked to satisfy
    /// the `'static` lifetime requirement (their mmaps must outlive the returned
    /// indices). Failed or partially failed loads reclaim them instead. Only
    /// call this once per process (at startup).
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

        if manifest.version != MANIFEST_VERSION && manifest.version != LEGACY_MANIFEST_VERSION {
            tracing::info!(
                expected = "1 or 2",
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

        // Hnsw borrows its HnswIo mmap. Keep each allocation owned while loading so
        // failures can reclaim it; leak both only after both loads have succeeded.
        //
        // Gate the load on the SAME emptiness signal `dump_to_disk` used to decide
        // whether it wrote a generation: the persisted id map, NOT the DB row count.
        // `dump_to_disk` writes a generation pair iff `!chunk_id_map.is_empty()`, and
        // serializes that exact map here. `chunk_embeddings_expected` is a DB count
        // captured independently; under concurrent ingestion a dump can record a
        // positive count while its in-memory id map was still empty, so no generation
        // was written. Gating on the DB count would then load whatever legacy canonical
        // `chunks.hnsw.*` files a prior generation left behind and map fresh ids onto
        // those stale vectors. Gating on the id map treats that manifest as empty and
        // rebuilds from the DB instead.
        let lock_held = _lock_guard.is_some();
        let chunk_use_mmap = should_mmap_generation(
            dir,
            &manifest.chunk_basename,
            &default_chunk_basename(),
            lock_held,
        );
        let mut chunk_io = (!manifest.chunk_id_map.is_empty())
            .then(|| PendingHnswIo::new(dir, &manifest.chunk_basename, chunk_use_mmap));
        let chunk_hnsw = if let Some(io) = chunk_io.as_mut() {
            match io.load() {
                Ok(hnsw) => hnsw,
                Err(failure) => {
                    log_hnsw_load_failure("chunk", &failure);
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

        // Same reasoning as the chunk index above: gate on the persisted id map, which
        // is what `dump_to_disk` keyed the generation write on, not the DB count.
        let refl_use_mmap = should_mmap_generation(
            dir,
            &manifest.reflection_basename,
            &default_reflection_basename(),
            lock_held,
        );
        let mut refl_io = (!manifest.reflection_id_map.is_empty())
            .then(|| PendingHnswIo::new(dir, &manifest.reflection_basename, refl_use_mmap));
        let refl_hnsw = if let Some(io) = refl_io.as_mut() {
            match io.load() {
                Ok(hnsw) => hnsw,
                Err(failure) => {
                    log_hnsw_load_failure("reflection", &failure);
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

        // These allocations own mmaps referenced by the returned Hnsw values and
        // therefore intentionally live for the process lifetime after full success.
        if let Some(io) = chunk_io.take() {
            io.leak();
        }
        if let Some(io) = refl_io.take() {
            io.leak();
        }

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

fn next_generation_basename(dir: &Path, prefix: &str) -> String {
    loop {
        let generation = INDEX_GENERATION.fetch_add(1, Ordering::Relaxed);
        let basename = format!("{prefix}-{}-{generation}", std::process::id());
        let data_path = dir.join(format!("{basename}.hnsw.data"));
        let graph_path = dir.join(format!("{basename}.hnsw.graph"));
        if !data_path.exists() && !graph_path.exists() {
            return basename;
        }
    }
}

fn dump_hnsw_generation(
    index: &Hnsw<'static, f32, DistCosine>,
    dir: &Path,
    prefix: &str,
) -> Result<String> {
    let requested_basename = next_generation_basename(dir, prefix);
    let actual_basename = index.file_dump(dir, &requested_basename)?;
    sync_index_pair(dir, &actual_basename)?;
    Ok(actual_basename)
}

fn sync_index_pair(dir: &Path, basename: &str) -> Result<()> {
    for extension in [".hnsw.data", ".hnsw.graph"] {
        std::fs::File::open(dir.join(format!("{basename}{extension}")))?.sync_all()?;
    }
    Ok(())
}

fn write_manifest_atomically(dir: &Path, manifest: &IndexManifest) -> Result<()> {
    let manifest_json = serde_json::to_vec_pretty(manifest)?;
    let tmp_path = dir.join("manifest.json.tmp");
    let final_path = dir.join("manifest.json");
    let mut tmp_file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path)?;
    tmp_file.write_all(&manifest_json)?;
    tmp_file.sync_all()?;
    drop(tmp_file);
    std::fs::rename(&tmp_path, &final_path)?;
    if let Err(error) = std::fs::File::open(dir).and_then(|directory| directory.sync_all()) {
        tracing::warn!(%error, path = %dir.display(), "failed to fsync index directory");
    }
    Ok(())
}

/// Decide whether a HNSW generation is safe to mmap rather than heap-load.
///
/// Two conditions, both required:
///
/// 1. **Not the legacy canonical basename.** A dump from a pre-9.5.4 process
///    (before numbered generations) writes the canonical `chunks.hnsw.data` /
///    `reflections.hnsw.data` in place with `overwrite=true`, truncating it. If
///    this process had that file mapped, the truncation would SIGBUS it. Numbered
///    generations (`chunks-<pid>-<n>`) are written once to a unique name and never
///    overwritten or truncated, so only they are safe to map while other processes
///    (possibly older builds) share the directory. A canonical generation is loaded
///    heap-backed; the mmap win arrives once any dump migrates the index to a
///    numbered generation (every post-9.5.4 dump does).
///
/// 2. **The files are actually mappable.** `hnsw_rs`'s `from_hnswdump` calls
///    `std::process::exit(1)` — not a recoverable error — if the graph file cannot
///    be opened, the data file cannot be stat'd/opened, or the data file cannot be
///    mapped. Turning mmap on would therefore let a transient mapping failure kill
///    the MCP server or a hook instead of falling back to a heap rebuild. Probing
///    the exact operations here (open the graph, map the data, unmap) means a
///    failure returns `false` and the caller heap-loads, preserving the pre-mmap
///    fallback behaviour.
fn should_mmap_generation(
    dir: &Path,
    basename: &str,
    default_basename: &str,
    lock_held: bool,
) -> bool {
    // Only map with the shared load lock held. Without it, a concurrent dump could
    // publish a new generation and unlink these files between hnsw_rs's `init()` and
    // `from_hnswdump` reopening them, reaching a `process::exit(1)` path. The lock is
    // present in normal operation (dump/startup create `index.lock`); a copied cache
    // that lacks it is loaded heap-backed, which is always safe.
    if !lock_held {
        return false;
    }
    // Only map a NUMBERED generation (`<prefix>-...`), never the legacy canonical
    // `<prefix>.hnsw.data`: a pre-9.5.4 process can truncate the canonical file in
    // place, which would SIGBUS a process mapping it. Engine-written manifests only
    // ever name `<prefix>` or `<prefix>-<pid>-<n>`, so this prefix check is exact for
    // real inputs; it is not a general alias validator (a hand-crafted `<prefix>-x`
    // pointing elsewhere would pass), which real manifests never produce.
    let numbered_prefix = format!("{default_basename}-");
    if !basename.starts_with(&numbered_prefix) {
        return false;
    }
    // The graph file must open (from_hnswdump exits if it cannot) and the data file
    // must be mappable (else from_hnswdump exits). Probing up front lets a failure
    // fall back to a heap rebuild instead of killing the process. This narrows but
    // cannot fully close the exit window: hnsw_rs reopens the files during the real
    // load, so a resource failure that appears only then (e.g. EMFILE) can still hit
    // its exit path — `raise_fd_limit` removes that specific trigger at startup.
    if std::fs::File::open(dir.join(format!("{basename}.hnsw.graph"))).is_err() {
        return false;
    }
    data_file_is_mmappable(&dir.join(format!("{basename}.hnsw.data")))
}

/// Probe that a file can be memory-mapped read-only, then unmap it. Returns false
/// on any failure (missing, empty, unmappable filesystem, resource limit) so the
/// caller can heap-load instead of handing an un-mappable file to `hnsw_rs`, which
/// would `process::exit(1)`.
#[cfg(unix)]
fn data_file_is_mmappable(path: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    let len = meta.len();
    if len == 0 {
        return false;
    }
    let Ok(len) = usize::try_from(len) else {
        return false;
    };
    // SAFETY: a read-only probe map of an open regular file at offset 0. We never
    // dereference the returned pointer and unmap it immediately; on failure `mmap`
    // returns MAP_FAILED, which we check before calling `munmap`.
    unsafe {
        let addr = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        );
        if addr == libc::MAP_FAILED {
            return false;
        }
        libc::munmap(addr, len);
    }
    true
}

/// Non-unix has no reliable unlink-under-mmap guarantee (Windows can deny or defer
/// deletion of a mapped file), so never mmap there — heap-load is always correct.
#[cfg(not(unix))]
fn data_file_is_mmappable(_path: &Path) -> bool {
    false
}

struct PendingHnswIo {
    io: NonNull<HnswIo>,
}

impl PendingHnswIo {
    /// `use_mmap` maps the `.hnsw.data` vectors instead of reading them into the
    /// heap. The returned `Hnsw` then borrows point slices from this `HnswIo`'s
    /// mapping, which is why a fully successful load leaks the `HnswIo` (see `leak`)
    /// so the mapping outlives the process's use of the index. It must only be set
    /// for a numbered generation the caller has already confirmed is mappable — see
    /// `should_mmap_generation`.
    fn new(dir: &Path, basename: &str, use_mmap: bool) -> Self {
        let options = ReloadOptions::new(use_mmap);
        let io = Box::new(HnswIo::new_with_options(dir, basename, options));
        Self {
            io: NonNull::new(Box::into_raw(io)).expect("Box::into_raw never returns null"),
        }
    }

    fn load(&mut self) -> std::result::Result<Hnsw<'static, f32, DistCosine>, HnswLoadFailure> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: `self` uniquely owns this allocation until `leak`; the pointer
            // remains stable, and a failed/unwound call cannot return a borrowing Hnsw.
            unsafe { self.io.as_mut().load_hnsw::<f32, DistCosine>() }
        }))
        .map_err(HnswLoadFailure::Panic)?
        .map_err(HnswLoadFailure::Error)
    }

    fn leak(self) {
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: the allocation is still uniquely owned here. Both Hnsw loads have
        // succeeded, so converting it to a process-lifetime leak preserves their mmaps.
        unsafe {
            Box::leak(Box::from_raw(this.io.as_ptr()));
        }
    }
}

impl Drop for PendingHnswIo {
    fn drop(&mut self) {
        // SAFETY: no Hnsw escaped when a load returned Err or unwound. At a partial
        // failure, Rust drops any previously loaded Hnsw before its PendingHnswIo.
        unsafe {
            drop(Box::from_raw(self.io.as_ptr()));
        }
    }
}

enum HnswLoadFailure {
    Error(anyhow::Error),
    Panic(Box<dyn std::any::Any + Send>),
}

fn log_hnsw_load_failure(kind: &str, failure: &HnswLoadFailure) {
    match failure {
        HnswLoadFailure::Error(error) => {
            tracing::warn!(%error, index = kind, "failed to load HNSW from cache");
        }
        HnswLoadFailure::Panic(payload) => {
            tracing::warn!(
                panic = panic_payload_message(payload.as_ref()),
                index = kind,
                "panic while loading HNSW from cache"
            );
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    }
}

/// Remove stale numbered HNSW files from the index directory.
/// hnsw_rs creates numbered files (e.g. `chunks-7905.hnsw.data`) when mmap is active.
/// The generation referenced by the current manifest is retained; other numbered
/// generations are stragglers from crashes, concurrent processes, or old sessions.
///
/// Called after every `dump_to_disk` and at engine startup.
/// Must be called while holding `index.lock` (dump_to_disk) or at startup before serving.
pub fn cleanup_stale_index_files(dir: &Path) {
    let Some(manifest) = std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|data| serde_json::from_str::<IndexManifest>(&data).ok())
    else {
        return;
    };
    let keep: HashSet<String> = [
        format!("{}.hnsw.data", manifest.chunk_basename),
        format!("{}.hnsw.graph", manifest.chunk_basename),
        format!("{}.hnsw.data", manifest.reflection_basename),
        format!("{}.hnsw.graph", manifest.reflection_basename),
    ]
    .into_iter()
    .collect();

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
            && !keep.contains(&name)
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

        // This line writes numbered generations (`chunks-<pid>-<n>.hnsw.*`),
        // not the legacy canonical `chunks.hnsw.data` — resolve the real
        // paths from the manifest, exactly as
        // `consecutive_clean_dumps_reuse_the_same_generation` does.
        let first_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let chunk_data_path = dir.join(format!("{}.hnsw.data", first_manifest.chunk_basename));
        let chunk_graph_path = dir.join(format!("{}.hnsw.graph", first_manifest.chunk_basename));
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

        let second_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(
            first_manifest.chunk_basename, second_manifest.chunk_basename,
            "a reflection-only dump must not migrate the chunk side to a new generation"
        );
        assert_ne!(
            first_manifest.reflection_basename, second_manifest.reflection_basename,
            "a reflection-only dump must actually write a new reflection generation"
        );

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
    fn load_from_disk_returns_none_when_hnsw_pair_is_mismatched() {
        use std::io::{Seek, SeekFrom, Write};

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 1, 0).unwrap();

        let manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        // Retain the real graph but make its paired data file claim a different
        // dimension. hnsw_rs asserts that the graph and data dimensions agree.
        let mut data = std::fs::OpenOptions::new()
            .write(true)
            .open(dir.join(format!("{}.hnsw.data", manifest.chunk_basename)))
            .unwrap();
        data.seek(SeekFrom::Start(std::mem::size_of::<u32>() as u64))
            .unwrap();
        data.write_all(&999usize.to_ne_bytes()).unwrap();
        drop(data);

        assert!(SearchEngine::load_from_disk(dir, 1, 0).is_none());
    }

    #[test]
    fn positive_count_without_a_generation_does_not_map_ids_onto_stale_files() {
        // Regression: dump keys "a generation exists" on `!id_map.is_empty()`, but the
        // manifest independently records a DB row count. Under concurrent ingestion a
        // dump can commit a positive count while its in-memory id map was still empty,
        // so no generation pair was written — only whatever legacy canonical
        // `chunks.hnsw.*` files a prior generation left behind remain on disk. The
        // loader must NOT resurrect those stale vectors and map fresh ids onto them; it
        // must treat the index as empty and let the DB backfill rebuild it.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Produce real, loadable canonical chunk files from a throwaway generation, so
        // the buggy DB-count gate WOULD have loaded three stale vectors here.
        let mut seeded = SearchEngine::new(10);
        seeded.insert_chunk("stale0".into(), vec![0.1; 384]);
        seeded.insert_chunk("stale1".into(), vec![0.2; 384]);
        seeded.insert_chunk("stale2".into(), vec![0.3; 384]);
        seeded.dump_to_disk(dir, 3, 0).unwrap();
        let seeded_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        for extension in [".hnsw.data", ".hnsw.graph"] {
            std::fs::copy(
                dir.join(format!("{}{extension}", seeded_manifest.chunk_basename)),
                dir.join(format!("chunks{extension}")),
            )
            .unwrap();
        }

        // Rewrite the manifest to the bug shape: positive DB count, empty id map (no
        // generation written for THIS manifest), basename pointing at the stale canonical
        // files.
        let manifest = IndexManifest {
            version: MANIFEST_VERSION,
            created_at: "2026-08-19T00:00:00Z".into(),
            chunk_id_map: Vec::new(),
            reflection_id_map: Vec::new(),
            chunk_embeddings_expected: 3,
            reflection_embeddings_expected: 0,
            active_reflection_count: 0,
            chunk_basename: default_chunk_basename(),
            reflection_basename: default_reflection_basename(),
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let loaded = SearchEngine::load_from_disk(dir, 3, 0)
            .expect("empty-map manifest loads as an empty index, not None");
        // The stale canonical vectors must not have been mapped in.
        assert!(loaded.chunk_id_map.is_empty());
        assert_eq!(loaded.chunk_index.get_nb_point(), 0);
    }

    #[test]
    fn load_accepts_v1_manifest_with_canonical_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 1, 0).unwrap();

        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let object = manifest.as_object_mut().unwrap();
        let chunk_basename = object["chunk_basename"].as_str().unwrap().to_string();
        for extension in [".hnsw.data", ".hnsw.graph"] {
            std::fs::copy(
                dir.join(format!("{chunk_basename}{extension}")),
                dir.join(format!("chunks{extension}")),
            )
            .unwrap();
        }
        object.insert("version".into(), serde_json::json!(1));
        object.remove("chunk_basename");
        object.remove("reflection_basename");
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();

        assert!(SearchEngine::load_from_disk(dir, 1, 0).is_some());
    }

    /// Under WP3, `dump_to_disk` only rewrites a side's HNSW generation when
    /// that side is actually dirty (or `force_full` applies). A clean second
    /// dump therefore must NOT invent a new generation — it carries the
    /// previous manifest's basename forward untouched. This is split into
    /// two tests (clean vs. dirty) because they pin genuinely different
    /// behaviour: a clean dump reusing its generation, and a dirty dump
    /// being forbidden from reusing one.
    #[test]
    fn consecutive_clean_dumps_reuse_the_same_generation() {
        // Skipping a write can never violate the "never overwrite files the
        // committed manifest references" invariant — a skipped write is a
        // no-op on those files by definition. So a clean second dump
        // reusing the first generation's basename is safe, and is the whole
        // point of WP3: it stops a merely-reflection-adjacent flush from
        // paying to rewrite a multi-hundred-MB chunk graph that didn't
        // change.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 1, 0).unwrap();

        let first_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let first_data_path = dir.join(format!("{}.hnsw.data", first_manifest.chunk_basename));
        let first_graph_path = dir.join(format!("{}.hnsw.graph", first_manifest.chunk_basename));
        let first_data = std::fs::read(&first_data_path).unwrap();
        let first_graph = std::fs::read(&first_graph_path).unwrap();

        // Stage an orphan generation directly (bypassing dump_to_disk, so no
        // manifest ever references it) to prove cleanup still reclaims it
        // even when the following dump_to_disk call is itself clean.
        let staged_basename = dump_hnsw_generation(&engine.chunk_index, dir, "chunks").unwrap();
        assert_ne!(first_manifest.chunk_basename, staged_basename);
        assert!(dir.join(format!("{staged_basename}.hnsw.data")).exists());
        assert!(dir.join(format!("{staged_basename}.hnsw.graph")).exists());

        // No mutation happened between the two dumps.
        assert!(!engine.is_chunk_dirty());
        engine.dump_to_disk(dir, 1, 0).unwrap();

        let second_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(
            first_manifest.chunk_basename, second_manifest.chunk_basename,
            "a clean dump must reuse the previous generation's basename instead of \
             burning I/O to rewrite byte-identical HNSW files"
        );
        assert_eq!(
            std::fs::read(&first_data_path).unwrap(),
            first_data,
            "chunks.hnsw.data changed even though the chunk side was clean"
        );
        assert_eq!(
            std::fs::read(&first_graph_path).unwrap(),
            first_graph,
            "chunks.hnsw.graph changed even though the chunk side was clean"
        );

        // cleanup_stale_index_files runs at the end of every dump_to_disk
        // and reclaims any numbered generation the newly committed manifest
        // does not reference — including this staged orphan, even though
        // this particular dump was clean.
        assert!(
            !dir.join(format!("{staged_basename}.hnsw.data")).exists(),
            "orphaned staged generation was not cleaned up after a clean dump"
        );
        assert!(!dir.join(format!("{staged_basename}.hnsw.graph")).exists());
    }

    #[test]
    fn dirty_second_dump_writes_a_new_generation_without_touching_the_first() {
        // This is the case the original "consecutive dumps must never share
        // a basename" invariant protects: a second dump that actually
        // rewrites this side must never do so in place over generation N's
        // files — another process may have generation N mmap'd, and
        // truncating/rewriting it in place would corrupt that mapping.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 1, 0).unwrap();

        let first_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let first_data_path = dir.join(format!("{}.hnsw.data", first_manifest.chunk_basename));
        let first_graph_path = dir.join(format!("{}.hnsw.graph", first_manifest.chunk_basename));
        // Captured while generation N is still the only chunk generation on
        // disk — proves generation N+1 (below) is created as a distinct
        // file, not by truncating/reopening this inode.
        let first_data_ino = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&first_data_path).unwrap().ino()
        };

        engine.insert_chunk("c1".into(), vec![0.6; 384]);
        assert!(engine.is_chunk_dirty());
        engine.dump_to_disk(dir, 2, 0).unwrap();

        let second_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_ne!(
            first_manifest.chunk_basename, second_manifest.chunk_basename,
            "a dirty second dump must write a NEW generation, never reuse the previous one"
        );
        assert!(second_manifest.chunk_basename.starts_with("chunks-"));
        let second_data_path = dir.join(format!("{}.hnsw.data", second_manifest.chunk_basename));
        let second_graph_path = dir.join(format!("{}.hnsw.graph", second_manifest.chunk_basename));
        assert!(second_data_path.exists());
        assert!(second_graph_path.exists());

        // `next_generation_basename` only ever returns a name whose files do
        // not yet exist, so generation N+1 was necessarily written to a
        // brand-new inode while generation N's file was still present and
        // untouched — never overwritten in place.
        let second_data_ino = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&second_data_path).unwrap().ino()
        };
        assert_ne!(
            first_data_ino, second_data_ino,
            "generation N+1 was written to generation N's inode — proof of an in-place overwrite"
        );

        // Only AFTER the new generation is committed does
        // cleanup_stale_index_files reclaim the now-unreferenced generation
        // N files — a whole-file delete, never a truncate/rewrite of the
        // live file, and it only happens once nothing on disk still names
        // generation N.
        assert!(
            !first_data_path.exists() && !first_graph_path.exists(),
            "previous generation should have been reclaimed by cleanup, not left dangling"
        );
    }

    #[test]
    fn dumps_write_current_manifest_version() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.dump_to_disk(tmp.path(), 1, 0).unwrap();

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["version"], 2);
    }

    #[test]
    fn manifest_commit_replaces_file_and_removes_temp() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("manifest.json"), "old").unwrap();
        let manifest = IndexManifest {
            version: MANIFEST_VERSION,
            created_at: "2026-08-17T00:00:00Z".into(),
            chunk_id_map: Vec::new(),
            reflection_id_map: Vec::new(),
            chunk_embeddings_expected: 0,
            reflection_embeddings_expected: 0,
            active_reflection_count: 0,
            chunk_basename: default_chunk_basename(),
            reflection_basename: default_reflection_basename(),
        };

        write_manifest_atomically(dir, &manifest).unwrap();

        let committed: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(committed.version, MANIFEST_VERSION);
        assert!(!dir.join("manifest.json.tmp").exists());
    }

    #[test]
    fn reflection_panic_after_chunk_load_does_not_poison_later_load() {
        use std::io::{Seek, SeekFrom, Write};

        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.insert_reflection("r0".into(), vec![0.25; 384]);
        engine.dump_to_disk(dir, 1, 1).unwrap();

        let manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let reflection_data_path = dir.join(format!("{}.hnsw.data", manifest.reflection_basename));
        let valid_reflection_data = std::fs::read(&reflection_data_path).unwrap();
        let mut data = std::fs::OpenOptions::new()
            .write(true)
            .open(&reflection_data_path)
            .unwrap();
        data.seek(SeekFrom::Start(std::mem::size_of::<u32>() as u64))
            .unwrap();
        data.write_all(&999usize.to_ne_bytes()).unwrap();
        drop(data);

        assert!(SearchEngine::load_from_disk(dir, 1, 1).is_none());
        std::fs::write(&reflection_data_path, valid_reflection_data).unwrap();
        assert!(SearchEngine::load_from_disk(dir, 1, 1).is_some());
    }

    #[test]
    fn cleanup_keeps_manifest_generation_and_removes_other_numbered_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Create stale and current numbered generations.
        std::fs::write(dir.join("chunks-100.hnsw.data"), "old").unwrap();
        std::fs::write(dir.join("chunks-100.hnsw.graph"), "old").unwrap();
        std::fs::write(dir.join("chunks-200.hnsw.data"), "current").unwrap();
        std::fs::write(dir.join("chunks-200.hnsw.graph"), "current").unwrap();
        std::fs::write(dir.join("reflections-25.hnsw.data"), "old").unwrap();
        std::fs::write(dir.join("reflections-25.hnsw.graph"), "old").unwrap();
        std::fs::write(dir.join("reflections-50.hnsw.data"), "current").unwrap();
        std::fs::write(dir.join("reflections-50.hnsw.graph"), "current").unwrap();

        // Canonical files are from an older cache generation, but cleanup only
        // removes stale numbered generations.
        std::fs::write(dir.join("chunks.hnsw.data"), "current").unwrap();
        std::fs::write(dir.join("chunks.hnsw.graph"), "current").unwrap();
        std::fs::write(dir.join("reflections.hnsw.data"), "current").unwrap();
        std::fs::write(dir.join("reflections.hnsw.graph"), "current").unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": MANIFEST_VERSION,
                "created_at": "2026-08-17T00:00:00Z",
                "chunk_id_map": [],
                "reflection_id_map": [],
                "chunk_embeddings_expected": 0,
                "reflection_embeddings_expected": 0,
                "active_reflection_count": 0,
                "chunk_basename": "chunks-200",
                "reflection_basename": "reflections-50"
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("index.lock"), "").unwrap();

        cleanup_stale_index_files(dir);

        // Stale numbered generations should be gone.
        assert!(!dir.join("chunks-100.hnsw.data").exists());
        assert!(!dir.join("chunks-100.hnsw.graph").exists());
        assert!(!dir.join("reflections-25.hnsw.data").exists());
        assert!(!dir.join("reflections-25.hnsw.graph").exists());

        // The manifest's complete graph/data pairs must remain.
        assert!(dir.join("chunks-200.hnsw.data").exists());
        assert!(dir.join("chunks-200.hnsw.graph").exists());
        assert!(dir.join("reflections-50.hnsw.data").exists());
        assert!(dir.join("reflections-50.hnsw.graph").exists());

        // Canonical files and non-index metadata should remain.
        assert!(dir.join("chunks.hnsw.data").exists());
        assert!(dir.join("chunks.hnsw.graph").exists());
        assert!(dir.join("reflections.hnsw.data").exists());
        assert!(dir.join("manifest.json").exists());
        assert!(dir.join("index.lock").exists());
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

    // Build `n` deterministic 384-dim unit-ish vectors keyed by index.
    fn synthetic_vectors(n: usize) -> Vec<Vec<f32>> {
        // Well-separated deterministic vectors: a per-(i,j) hash decorrelates each
        // vector from every other one, so a query's exact self-match is an unambiguous
        // nearest neighbour. The earlier smooth-sinusoid scheme made consecutive
        // vectors near-collinear; approximate HNSW recall for a self-query then hinged
        // on float-summation order, which differs across platforms (top-1 self-match
        // held on macOS but not Linux CI). Values are zero-mean so the cosine
        // similarity between distinct vectors sits near 0.
        (0..n)
            .map(|i| {
                (0..384)
                    .map(|j| {
                        let h = ((i as f32 + 1.0) * 12.9898 + (j as f32 + 1.0) * 78.233).sin()
                            * 43_758.547;
                        (h - h.floor()) - 0.5
                    })
                    .collect()
            })
            .collect()
    }

    /// Verify a self-query against an index built from `synthetic_vectors`, in two
    /// parts, for indexes large enough (> `EXACT_SCAN_THRESHOLD`) that `search_chunks`
    /// walks the approximate HNSW graph rather than exact-scanning:
    ///
    ///  1. A DETERMINISTIC structural check on the real `search_chunks` (approximate)
    ///     path: non-empty, every returned id was actually inserted (`has_chunk`), every
    ///     score is a plausible cosine similarity. This still walks the graph traversal
    ///     end to end — including through the mmap where applicable — so it keeps
    ///     catching what approximate search needs to protect against: a failed/empty
    ///     load, a torn or truncated read, a panic/abort on corrupt graph data, or
    ///     garbage ids leaking through.
    ///  2. The exact top-1 IDENTITY check via the private `exact_scan` helper (the same
    ///     one `search_chunks` itself falls back to for small/filtered corpora),
    ///     bypassing HNSW's approximate graph traversal entirely.
    ///
    /// Part 2 exists, and part 1 is not itself an identity check, because of a measured
    /// property of this corpus: `synthetic_vectors` (see its doc comment) produces
    /// near-orthogonal vectors — every pair of DISTINCT points sits around cosine
    /// 0.1-0.15 — so there is no "getting warmer" gradient for HNSW's greedy descent to
    /// follow toward a query's own point once the walk starts elsewhere. hnsw_rs also
    /// seeds its level-assignment RNG from OS entropy per process (`StdRng::from_os_rng`
    /// in `hnsw_rs::hnsw`), so the graph topology — and therefore which points a
    /// bounded-width beam search actually reaches — differs every run. Measured directly
    /// on this corpus (300 independent build+reload trials of a 500-point self-query):
    /// ~3% of runs missed the exact self-match via `search_chunks`, by a wide score
    /// margin every time (true cosine ~1.0 vs ~0.1-0.15 for the wrong top-1) — never a
    /// near-duplicate tie, never wrong/stale/corrupted data (the `has_chunk` check in
    /// part 1 and the exact scan in part 2 both confirm the id map and vector are
    /// correct). That is bounded, expected ANN behaviour on this intentionally
    /// unstructured corpus, not a defect in the code under test, so the strict identity
    /// assertion belongs on the exact scan, not on `search_chunks`'s approximate result.
    fn assert_self_query_correct(
        engine: &SearchEngine,
        query: &[f32],
        expected_id: &str,
        limit: usize,
        min_score: f32,
    ) {
        let approx = engine.search_chunks(query, limit, min_score);
        assert!(
            !approx.is_empty(),
            "approximate search_chunks returned no results querying {expected_id}'s own vector"
        );
        for r in &approx {
            assert!(
                engine.has_chunk(&r.id),
                "search_chunks returned an id that was never inserted (or was removed): {}",
                r.id
            );
            assert!(
                (-1.0001..=1.0001).contains(&r.score),
                "search_chunks returned an implausible cosine score {} for id {}",
                r.score,
                r.id
            );
        }

        let exact = SearchEngine::exact_scan(
            &engine.chunk_index,
            &engine.chunk_id_map,
            query,
            limit,
            min_score,
            None,
        );
        assert_eq!(
            exact.first().map(|r| r.id.as_str()),
            Some(expected_id),
            "exact scan must find {expected_id} as its own nearest neighbour"
        );
    }

    // The canonical (legacy, pre-9.5.4) basename must NOT be mmapped, because an old
    // process can truncate it in place. `should_mmap_generation` returns false for it,
    // true for a numbered generation with real files, and false when the files are
    // missing. A canonical index still loads and searches correctly (heap-backed).
    #[test]
    fn canonical_generation_is_never_mmapped_but_still_loads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Canonical basename is refused regardless of whether files exist.
        assert!(!should_mmap_generation(
            dir,
            &default_chunk_basename(),
            &default_chunk_basename(),
            true
        ));
        // A numbered basename with no files on disk is refused (would exit in hnsw_rs).
        assert!(!should_mmap_generation(
            dir,
            "chunks-1-2",
            &default_chunk_basename(),
            true
        ));

        // Build an index, dump it, then rewrite the manifest to the LEGACY canonical
        // layout (as a pre-9.5.4 dump would have left it) and rename the files to match.
        let vecs = synthetic_vectors(300);
        let mut writer = SearchEngine::new(400);
        for (i, v) in vecs.iter().enumerate() {
            writer.insert_chunk(format!("c{i}"), v.clone());
        }
        writer.dump_to_disk(dir, 300, 0).unwrap();
        let manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let numbered = manifest.chunk_basename.clone();
        std::fs::rename(
            dir.join(format!("{numbered}.hnsw.data")),
            dir.join("chunks.hnsw.data"),
        )
        .unwrap();
        std::fs::rename(
            dir.join(format!("{numbered}.hnsw.graph")),
            dir.join("chunks.hnsw.graph"),
        )
        .unwrap();
        let mut legacy = serde_json::to_value(&manifest).unwrap();
        legacy["chunk_basename"] = serde_json::json!("chunks");
        legacy["version"] = serde_json::json!(1);
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        // Now the numbered generation is refused (files gone), the canonical layout is
        // refused by basename, and the index still loads heap-backed and searches.
        assert!(!should_mmap_generation(
            dir,
            "chunks",
            &default_chunk_basename(),
            true
        ));
        // A numbered generation with a missing lock is refused (no unlink protection).
        assert!(!should_mmap_generation(
            dir,
            "chunks-9-9",
            &default_chunk_basename(),
            false
        ));
        let loaded = SearchEngine::load_from_disk(dir, 300, 0).expect("canonical index loads");
        assert_self_query_correct(&loaded, &vecs[42], "c42", 3, 0.1);
    }

    // The core PR2 safety property: a process holding an mmap-backed generation keeps
    // serving correct results after ANOTHER process publishes a new generation and
    // `cleanup_stale_index_files` unlinks the older one. On unix the inode and its
    // pages survive until the last mapping is dropped, so the mapped reads stay valid.
    // This is what makes turning mmap on safe under concurrent csr-engine processes.
    #[cfg(unix)]
    #[test]
    fn mmap_backed_index_survives_concurrent_generation_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        // Generation 1: 300 chunks (> EXACT_SCAN_THRESHOLD, so search walks the HNSW
        // graph and reads vectors through the mmap rather than exact-scanning).
        let vecs = synthetic_vectors(300);
        let mut writer = SearchEngine::new(400);
        for (i, v) in vecs.iter().enumerate() {
            writer.insert_chunk(format!("c{i}"), v.clone());
        }
        writer.dump_to_disk(dir, 300, 0).unwrap();

        // A reader loads generation 1 mmap-backed (leaks an HnswIo holding the map).
        let reader = SearchEngine::load_from_disk(dir, 300, 0).expect("load generation 1");
        let gen1_data = {
            let manifest: IndexManifest =
                serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
            dir.join(format!("{}.hnsw.data", manifest.chunk_basename))
        };
        assert!(gen1_data.exists(), "generation 1 data file should exist");
        // The generation must be numbered so `should_mmap_generation` mapped it (a
        // canonical basename would be heap-loaded and this test would not exercise mmap).
        let gen1_basename = gen1_data
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap()
            .trim_end_matches(".hnsw.data")
            .to_string();
        assert!(
            gen1_basename.starts_with("chunks-"),
            "expected a numbered generation, got {gen1_basename}"
        );
        assert!(
            should_mmap_generation(dir, &gen1_basename, &default_chunk_basename(), true),
            "generation 1 should be mmap-backed"
        );

        // Sanity: the mmap-backed reader returns the self-match with a near-1.0 score.
        assert_self_query_correct(&reader, &vecs[142], "c142", 5, 0.1);

        // Another process publishes generation 2 (500 chunks). dump_to_disk commits the
        // new manifest and runs cleanup, which unlinks generation 1's numbered files.
        let vecs2 = synthetic_vectors(500);
        let mut publisher = SearchEngine::new(600);
        for (i, v) in vecs2.iter().enumerate() {
            publisher.insert_chunk(format!("c{i}"), v.clone());
        }
        publisher.dump_to_disk(dir, 500, 0).unwrap();
        assert!(
            !gen1_data.exists(),
            "cleanup should have unlinked generation 1's data file"
        );

        // The still-mapped reader must keep returning correct results after the unlink.
        assert_self_query_correct(&reader, &vecs[142], "c142", 5, 0.1);
        // A vector that only differs slightly should still resolve to its own id.
        assert_self_query_correct(&reader, &vecs[7], "c7", 3, 0.1);

        // And a fresh load now picks up generation 2, including the newer ids.
        let reloaded = SearchEngine::load_from_disk(dir, 500, 0).expect("load generation 2");
        assert!(reloaded.has_chunk("c499"));
        assert!(reloaded.has_chunk("c142"));
        // c499 is the boundary point most exposed to approximate-search recall variance
        // (see `assert_self_query_correct`'s doc comment for why and the measured rate).
        assert_self_query_correct(&reloaded, &vecs2[499], "c499", 3, 0.1);
    }

    // Additive backfill into an mmap-backed index: newly inserted points are heap-owned
    // Vecs while the loaded points remain mmap slices. Both must be searchable, and a
    // subsequent dump (which writes a fresh generation, never truncating the mapped
    // file) must round-trip every id.
    #[cfg(unix)]
    #[test]
    fn mmap_backed_index_accepts_new_inserts_and_redumps() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        let vecs = synthetic_vectors(300);
        let mut writer = SearchEngine::new(400);
        for (i, v) in vecs.iter().enumerate() {
            writer.insert_chunk(format!("c{i}"), v.clone());
        }
        writer.dump_to_disk(dir, 300, 0).unwrap();

        let mut reader = SearchEngine::load_from_disk(dir, 300, 0).expect("load generation 1");

        // Insert a new point (heap-owned) alongside the mmap-backed ones.
        let newv: Vec<f32> = (0..384).map(|j| ((j as f32) * 0.002).cos()).collect();
        reader.insert_chunk("c_new".into(), newv.clone());

        // newly inserted heap-owned point must be searchable
        assert_self_query_correct(&reader, &newv, "c_new", 3, 0.1);
        // mmap-backed points must remain searchable after a new insert
        assert_self_query_correct(&reader, &vecs[10], "c10", 3, 0.1);

        // Re-dump: writes a new generation, must not truncate the mapped file, and the
        // result must round-trip both the mmap-origin ids and the new one.
        reader.dump_to_disk(dir, 301, 0).unwrap();
        let reloaded = SearchEngine::load_from_disk(dir, 301, 0).expect("reload after redump");
        assert!(reloaded.has_chunk("c_new"));
        assert!(reloaded.has_chunk("c0"));
        assert!(reloaded.has_chunk("c299"));
        // Search must return the right vectors after the re-dump/reload, for both a
        // mmap-origin point and the point that was inserted heap-side before the dump.
        assert_self_query_correct(&reloaded, &vecs[0], "c0", 3, 0.1);
        assert_self_query_correct(&reloaded, &newv, "c_new", 3, 0.1);
    }

    // hnsw_rs has no true deletion: `remove_chunk` only blanks the id-map slot,
    // so a chunk rewritten repeatedly (the plan-reimport path: remove_chunk then
    // insert_chunk with the same deterministic id, see import/plans.rs) leaves one
    // dead point per rewrite sitting in the graph near the query. Below the fix,
    // `search_index` asked hnsw_rs for exactly `limit` neighbours, so those dead
    // points — being near-duplicates of the query — win result slots ahead of live
    // points. Above EXACT_SCAN_THRESHOLD (256) only, since the exact-scan path
    // already filters blanks with no slot budget to exhaust.
    #[test]
    fn tombstoned_rewrites_do_not_starve_live_results() {
        let mut vecs = synthetic_vectors(401);
        let seam = vecs.pop().unwrap(); // index 400: distinct from the 400 base points
        let mut engine = SearchEngine::new(500);
        for (i, v) in vecs.iter().enumerate() {
            engine.insert_chunk(format!("c{i}"), v.clone());
        }
        assert!(
            vecs.len() > EXACT_SCAN_THRESHOLD,
            "corpus must exceed EXACT_SCAN_THRESHOLD so search_index walks the HNSW path"
        );

        engine.insert_chunk("seam".to_string(), seam.clone());

        // Mirror the production rewrite path (import/plans.rs: remove_chunk then
        // insert_chunk under the SAME deterministic id) 10 times. Each cycle leaves
        // one dead point — a tiny perturbation of `seam`, so it sits right next to
        // the query — behind in the graph.
        for iter in 0..10u32 {
            engine.remove_chunk("seam");
            let perturbed: Vec<f32> = seam
                .iter()
                .enumerate()
                .map(|(j, v)| v + 1e-4 * ((iter as f32 + 1.0) * (j as f32 + 1.0)).sin())
                .collect();
            engine.insert_chunk("seam".to_string(), perturbed);
        }

        let dead = engine.chunk_id_map.len() - engine.chunk_id_set.len();
        assert_eq!(dead, 10, "10 rewrites must leave exactly 10 blanked slots");

        let results = engine.search_chunks(&seam, 5, 0.0);
        assert_eq!(
            results.len(),
            5,
            "5 dead tombstones near the query must not eat live result slots: {results:?}"
        );
        assert!(
            results.iter().all(|r| engine.has_chunk(&r.id)),
            "every returned id must be a live (non-blanked) chunk: {results:?}"
        );
        // Identity goes through the exact scan, not the approximate walk: on this
        // near-orthogonal corpus hnsw_rs misses a query's own neighbourhood in ~3%
        // of builds (see `assert_self_query_correct`). When that happens the walk
        // meets no dead point either, so the two assertions above hold regardless.
        let exact = SearchEngine::exact_scan(
            &engine.chunk_index,
            &engine.chunk_id_map,
            &seam,
            1,
            0.0,
            None,
        );
        assert_eq!(
            exact.first().map(|r| r.id.as_str()),
            Some("seam"),
            "the rewritten seam must stay the exact nearest live neighbour"
        );
    }

    // The fetch budget on its own, with no graph walk involved: unchanged with
    // no dead points, one extra neighbour per dead point, capped.
    #[test]
    fn fetch_budget_grows_with_dead_points_up_to_the_cap() {
        assert_eq!(fetch_budget(5, 0), 5);
        assert_eq!(fetch_budget(5, 10), 15);
        assert_eq!(
            fetch_budget(5, TOMBSTONE_OVERFETCH_CAP),
            5 + TOMBSTONE_OVERFETCH_CAP
        );
        assert_eq!(fetch_budget(5, 100_000), 5 + TOMBSTONE_OVERFETCH_CAP);
    }

    // Guards the byte-identical requirement: with zero dead points, tombstone
    // overfetch must not change the call to hnsw_rs or the returned results —
    // the upgrade rehearsal asserts byte-identical hook injections against 9.5.6.
    #[test]
    fn zero_dead_points_returns_exactly_the_direct_limit_sized_fetch() {
        let vecs = synthetic_vectors(300);
        let mut engine = SearchEngine::new(400);
        for (i, v) in vecs.iter().enumerate() {
            engine.insert_chunk(format!("c{i}"), v.clone());
        }
        assert_eq!(
            engine.chunk_id_map.len() - engine.chunk_id_set.len(),
            0,
            "no rewrites happened — there must be zero dead points"
        );

        let via_search_chunks = engine.search_chunks(&vecs[7], 5, 0.0);

        // Reproduce the pre-tombstone-overfetch call directly: exactly `limit`
        // neighbours from hnsw_rs, same filter/sort/truncate as search_index.
        let neighbours = engine.chunk_index.search(&vecs[7], 5, EF_SEARCH);
        let mut direct: Vec<SearchResult> = neighbours
            .into_iter()
            .filter_map(|n| {
                let score = 1.0 - n.distance;
                let id_map = &engine.chunk_id_map;
                if score >= 0.0 && n.d_id < id_map.len() && !id_map[n.d_id].is_empty() {
                    Some(SearchResult {
                        id: id_map[n.d_id].clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();
        direct.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        direct.truncate(5);

        assert_eq!(via_search_chunks.len(), direct.len());
        for (a, b) in via_search_chunks.iter().zip(direct.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.score, b.score);
        }
    }
}
