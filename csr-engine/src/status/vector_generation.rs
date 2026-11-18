//! The `vector_generation` block of `csr-engine status`: how chunk vectors are
//! served, the regeneration that keeps a generation's delta bounded, and what
//! the `index-v3` directory holds by purpose (`storage`).

use std::path::Path;

use serde::Serialize;

use crate::storage::Storage;

/// Chunk-vector serving: generation or hnsw_rs, and the regeneration that keeps
/// a generation's delta bounded. The regeneration fields come from the file the
/// daemon writes after every tick (`index-v3/regen-state.json`).
#[derive(Serialize, Debug, Default, PartialEq, Eq)]
pub struct VectorGenerationStatus {
    /// `generation`, `hnsw` (no watermark yet) or `off` (`CSR_NO_VECTOR_GEN=1`).
    pub mode: String,
    pub generation: Option<u64>,
    pub watermark_seq: Option<i64>,
    pub delta_rows: usize,
    pub pending_rows: usize,
    pub threshold: usize,
    pub last_regen_unix: i64,
    pub last_error: Option<String>,
    pub failures: u32,
    pub next_attempt_unix: i64,
    /// `index-v3` by purpose, one line each: total, the serving generation (or
    /// why none serves, and that chunk search is keyword-only meanwhile), the
    /// previous generation and when it goes, reflections, and dead files.
    /// Empty in the compact gather: the statusline prints none of it, and the
    /// lines read the directory and `manifest.json`.
    pub storage: Vec<String>,
}

impl VectorGenerationStatus {
    /// `with_storage` false skips the `storage` lines (the compact gather).
    pub fn read(storage: &Storage, index_dir: &Path, with_storage: bool) -> Self {
        let mut out = Self {
            mode: if crate::index_gen::regen::vector_gen_enabled() {
                "hnsw".into()
            } else {
                "off".into()
            },
            storage: if with_storage {
                crate::index_gen::retire::storage_lines(index_dir)
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        if let Ok(Some(manifest)) = crate::index_gen::read_watermark(storage) {
            if out.mode != "off" {
                out.mode = "generation".into();
            }
            out.generation = Some(manifest.generation);
            out.watermark_seq = Some(manifest.seq);
        }
        if let Some(state) = crate::index_gen::regen::read_state(index_dir) {
            out.delta_rows = state.delta_rows;
            out.pending_rows = state.pending_rows;
            out.threshold = state.threshold;
            out.last_regen_unix = state.last_regen_unix;
            out.last_error = state.last_error;
            out.failures = state.failures;
            out.next_attempt_unix = state.next_attempt_unix;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::{gather_status_with, test_run_dir, test_settings, Scope};
    use crate::index_gen::regen::{write_state, RegenState};
    use crate::storage::Storage;

    /// The `vector_generation.storage` array of the full status JSON for a
    /// fresh database whose `index-v3` holds `files` and `state`.
    fn storage_array(files: &[(&str, usize)], state: &RegenState) -> Vec<String> {
        storage_array_in(files, state, Scope::Full)
    }

    fn storage_array_in(files: &[(&str, usize)], state: &RegenState, scope: Scope) -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let projects_dir = dir.path().join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        Storage::open(&db_path).unwrap();
        let index_dir = dir.path().join("index-v3");
        std::fs::create_dir_all(&index_dir).unwrap();
        for (name, bytes) in files {
            std::fs::write(index_dir.join(name), vec![0u8; *bytes]).unwrap();
        }
        write_state(&index_dir, state).unwrap();
        let report = gather_status_with(
            &db_path,
            &projects_dir,
            false,
            scope,
            Some(&test_run_dir(&db_path)),
            Some(&test_settings(&db_path)),
        )
        .unwrap();
        let value = serde_json::to_value(&report).unwrap();
        let array = value["vector_generation"]["storage"]
            .as_array()
            .unwrap_or_else(|| panic!("no storage array in {}", value["vector_generation"]));
        array
            .iter()
            .map(|line| line.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn status_lists_index_storage_by_purpose_for_a_healthy_generation() {
        let state = RegenState {
            generation: 2,
            ..Default::default()
        };
        let lines = storage_array(
            &[
                ("chunks-g2.usearch", 3_000_000),
                ("chunks-g2.ids", 1_000_000),
                ("reflections.hnsw.data", 2_000_000),
                ("reflections.hnsw.graph", 1_000_000),
            ],
            &state,
        );
        let text = lines.join("\n");
        assert!(lines[0].starts_with("index-v3 storage: 7 MB"), "{text}");
        assert!(text.contains("serving: g2, 4 MB"), "{text}");
        assert!(text.contains("previous: none kept"), "{text}");
        assert!(text.contains("reflections: 3 MB"), "{text}");
        assert!(text.contains("dead: none"), "{text}");
        assert!(!text.contains("keyword-only"), "{text}");
    }

    #[test]
    fn status_says_chunk_search_is_keyword_only_while_a_repair_builds_a_generation() {
        let state = RegenState {
            generation: 0,
            repair_pending: true,
            ..Default::default()
        };
        let lines = storage_array(
            &[
                ("chunks-g1.usearch", 3_000_000),
                ("chunks-g1.ids", 1_000_000),
                ("reflections.hnsw.data", 1_000_000),
            ],
            &state,
        );
        let text = lines.join("\n");
        assert!(text.contains("serving: none; g1 will not open"), "{text}");
        assert!(
            text.contains("chunk search is keyword-only until it serves"),
            "{text}"
        );
    }

    /// The statusline's compact gather prints no storage line, so it does not
    /// read the directory or the manifest for them.
    #[test]
    fn the_compact_gather_reads_no_storage_lines() {
        let state = RegenState {
            generation: 2,
            ..Default::default()
        };
        let files = [("chunks-g2.usearch", 1_000_000)];
        assert!(storage_array_in(&files, &state, Scope::Compact).is_empty());
        assert!(!storage_array_in(&files, &state, Scope::Full).is_empty());
    }
}
