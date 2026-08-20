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
    dirty: bool,
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
            dirty: false,
        }
    }

    pub fn insert_chunk(&mut self, id: String, embedding: Vec<f32>) {
        if !self.chunk_id_set.insert(id.clone()) {
            return; // Already indexed — skip duplicate
        }
        let idx = self.chunk_id_map.len();
        self.chunk_id_map.push(id);
        self.chunk_index.insert((&embedding, idx));
        self.dirty = true;
    }

    pub fn insert_reflection(&mut self, id: String, embedding: Vec<f32>) {
        if !self.reflection_id_set.insert(id.clone()) {
            return; // Already indexed — skip duplicate
        }
        let idx = self.reflection_id_map.len();
        self.reflection_id_map.push(id);
        self.reflection_index.insert((&embedding, idx));
        self.active_reflection_count += 1;
        self.dirty = true;
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
            self.dirty = true;
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
            self.dirty = true;
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
            self.dirty = true;
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

    /// Whether the index has been modified since last dump.
    pub fn is_dirty(&self) -> bool {
        self.dirty
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

        // Dump every non-empty index to a new generation. Neither fresh nor mmap-backed
        // indexes may overwrite files referenced by the currently committed manifest.
        //
        // The `!id_map.is_empty()` guard here is the authoritative "a generation exists"
        // signal — `load_from_disk` MUST gate its load on the same persisted id map, not
        // on the DB counts recorded below, or it can load a stale generation this dump
        // never wrote. Keep the two in lockstep.
        let chunk_basename = if !self.chunk_id_map.is_empty() {
            dump_hnsw_generation(&self.chunk_index, dir, "chunks")
                .map_err(|e| anyhow::anyhow!("chunk index dump failed: {}", e))?
        } else {
            default_chunk_basename()
        };

        let reflection_basename = if !self.reflection_id_map.is_empty() {
            dump_hnsw_generation(&self.reflection_index, dir, "reflections")
                .map_err(|e| anyhow::anyhow!("reflection index dump failed: {}", e))?
        } else {
            default_reflection_basename()
        };

        // Write manifest atomically (tmp + rename)
        let manifest = IndexManifest {
            version: MANIFEST_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
            chunk_id_map: self.chunk_id_map.clone(),
            reflection_id_map: self.reflection_id_map.clone(),
            chunk_embeddings_expected: db_chunk_count,
            reflection_embeddings_expected: db_reflection_count,
            active_reflection_count: self.active_reflection_count,
            chunk_basename,
            reflection_basename,
        };

        write_manifest_atomically(dir, &manifest)?;

        // Clean numbered generations not referenced by the newly committed manifest.
        cleanup_stale_index_files(dir);

        // Lock is released when lock_file is dropped
        self.dirty = false;
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
        let mut chunk_io = (!manifest.chunk_id_map.is_empty())
            .then(|| PendingHnswIo::new(dir, &manifest.chunk_basename));
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
        let mut refl_io = (!manifest.reflection_id_map.is_empty())
            .then(|| PendingHnswIo::new(dir, &manifest.reflection_basename));
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
            dirty: false,
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

struct PendingHnswIo {
    io: NonNull<HnswIo>,
}

impl PendingHnswIo {
    fn new(dir: &Path, basename: &str) -> Self {
        let io = Box::new(HnswIo::new(dir, basename));
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

    #[test]
    fn consecutive_dumps_without_reload_use_distinct_generations() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        let mut engine = SearchEngine::new(10);
        engine.insert_chunk("c0".into(), vec![0.5; 384]);
        engine.dump_to_disk(dir, 1, 0).unwrap();

        let first_manifest: IndexManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let first_manifest_bytes = std::fs::read(dir.join("manifest.json")).unwrap();
        let first_data_path = dir.join(format!("{}.hnsw.data", first_manifest.chunk_basename));
        let first_graph_path = dir.join(format!("{}.hnsw.graph", first_manifest.chunk_basename));
        let first_data = std::fs::read(&first_data_path).unwrap();
        let first_graph = std::fs::read(&first_graph_path).unwrap();

        // Stage the second same-process HNSW dump without publishing its manifest.
        let staged_basename = dump_hnsw_generation(&engine.chunk_index, dir, "chunks").unwrap();
        assert_ne!(first_manifest.chunk_basename, staged_basename);
        assert_eq!(
            std::fs::read(dir.join("manifest.json")).unwrap(),
            first_manifest_bytes
        );
        assert_eq!(std::fs::read(&first_data_path).unwrap(), first_data);
        assert_eq!(std::fs::read(&first_graph_path).unwrap(), first_graph);
        assert!(dir.join(format!("{staged_basename}.hnsw.data")).exists());
        assert!(dir.join(format!("{staged_basename}.hnsw.graph")).exists());

        engine.dump_to_disk(dir, 1, 0).unwrap();

        let second_manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let second_basename = second_manifest["chunk_basename"].as_str().unwrap();
        assert_ne!(first_manifest.chunk_basename, second_basename);
        assert!(second_basename.starts_with("chunks-"));
        assert!(dir.join(format!("{second_basename}.hnsw.data")).exists());
        assert!(dir.join(format!("{second_basename}.hnsw.graph")).exists());
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
}
