pub mod cross_project;
pub mod decay;

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use fs2::FileExt;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use hnsw_rs::prelude::DistCosine;
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
        self.search_index(&self.chunk_index, &self.chunk_id_map, query_vec, limit, min_score)
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
        let neighbours = index.search(query_vec, limit, EF_SEARCH);

        let mut results: Vec<SearchResult> = neighbours
            .into_iter()
            .filter_map(|n| {
                // hnsw_rs DistCosine returns distance = 1.0 - cosine_similarity
                let score = 1.0 - n.distance;
                if score >= min_score
                    && n.d_id < id_map.len()
                    && !id_map[n.d_id].is_empty()
                {
                    Some(SearchResult {
                        id: id_map[n.d_id].clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
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

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
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
        lock_file.lock_exclusive().map_err(|e| {
            anyhow::anyhow!("failed to acquire index lock for dump: {}", e)
        })?;

        // Dump HNSW graph + data files (skip empty indices — file_dump fails on empty)
        if !self.chunk_id_map.is_empty() {
            self.chunk_index
                .file_dump(dir, "chunks")
                .map_err(|e| anyhow::anyhow!("chunk index dump failed: {}", e))?;
        }

        if !self.reflection_id_map.is_empty() {
            self.reflection_index
                .file_dump(dir, "reflections")
                .map_err(|e| anyhow::anyhow!("reflection index dump failed: {}", e))?;
        }

        // Write manifest atomically (tmp + rename)
        let manifest = IndexManifest {
            version: MANIFEST_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
            chunk_id_map: self.chunk_id_map.clone(),
            reflection_id_map: self.reflection_id_map.clone(),
            chunk_embeddings_expected: db_chunk_count,
            reflection_embeddings_expected: db_reflection_count,
            active_reflection_count: self.active_reflection_count,
        };

        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        let tmp_path = dir.join("manifest.json.tmp");
        let final_path = dir.join("manifest.json");
        std::fs::write(&tmp_path, &manifest_json)?;
        std::fs::rename(&tmp_path, &final_path)?;

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

        if manifest.chunk_embeddings_expected != expected_chunks {
            tracing::info!(
                cached = manifest.chunk_embeddings_expected,
                db = expected_chunks,
                "index cache stale (chunk count)"
            );
            return None;
        }

        if manifest.reflection_embeddings_expected != expected_reflections {
            tracing::info!(
                cached = manifest.reflection_embeddings_expected,
                db = expected_reflections,
                "index cache stale (reflection count)"
            );
            return None;
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
            Hnsw::new(MAX_NB_CONNECTION, 10_000, MAX_LAYER, EF_CONSTRUCTION, DistCosine {})
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
            Hnsw::new(MAX_NB_CONNECTION, 1_000, MAX_LAYER, EF_CONSTRUCTION, DistCosine {})
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

        Some(Self {
            chunk_index: chunk_hnsw,
            reflection_index: refl_hnsw,
            chunk_id_map: manifest.chunk_id_map,
            reflection_id_map: manifest.reflection_id_map,
            chunk_id_set,
            reflection_id_set,
            active_reflection_count: manifest.active_reflection_count,
            dirty: false,
        })
    }
}
