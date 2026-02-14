pub mod decay;

use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::prelude::DistCosine;

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
}

// HNSW parameters
const MAX_NB_CONNECTION: usize = 16; // M
const EF_CONSTRUCTION: usize = 200;
const EF_SEARCH: usize = 100;
const MAX_LAYER: usize = 16;

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
        }
    }

    pub fn insert_chunk(&mut self, id: String, embedding: Vec<f32>) {
        let idx = self.chunk_id_map.len();
        self.chunk_id_map.push(id);
        self.chunk_index.insert((&embedding, idx));
    }

    pub fn insert_reflection(&mut self, id: String, embedding: Vec<f32>) {
        let idx = self.reflection_id_map.len();
        self.reflection_id_map.push(id);
        self.reflection_index.insert((&embedding, idx));
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
                if score >= min_score && n.d_id < id_map.len() {
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
        self.reflection_id_map.len()
    }
}
