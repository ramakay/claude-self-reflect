//! Chunk vectors once a vector generation serves them: retiring the index-v3
//! chunk graph, and what an engine loads when the generation will not open.
//!
//! The chunk graph (`chunks-<pid>-<n>.hnsw.*`, with the manifest's
//! `chunk_id_map`) is the pre-generation way to serve chunks. A generation-served
//! engine never opens it, so after a generation has been built, verified and
//! served it is dead weight: about 2 GB beside the 1 GB generation on a
//! million-chunk corpus, and a 43 MB id map parsed on every load.
//!
//! Retirement writes the manifest first and unlinks second, both under the
//! exclusive `index.lock` that every dump and every load already uses. A crash
//! between the two leaves graph files no manifest names; `cleanup_stale_index_files`
//! removes them at the next start. The manifest keeps `"chunk_id_map": []` and
//! the default `chunk_basename` instead of dropping the keys, so a binary from
//! before this change still parses it: such a reader serves the generation, and
//! if its own generation open fails it finds an empty chunk side, which it
//! backfills from SQLite.

use super::*;
use std::path::PathBuf;

use crate::index_gen::{self, GenManifest, Generation};
use crate::storage::Storage;

/// A `chunks.hnsw.*` or `chunks-<pid>-<n>.hnsw.*` graph file. A generation's own
/// files (`chunks-g<N>.usearch`, `.ids`, `.json`, `.verified`) never match.
fn is_chunk_graph_file(name: &str) -> bool {
    (name.starts_with("chunks.") || name.starts_with("chunks-"))
        && !name.starts_with("chunks-g")
        && (name.ends_with(".hnsw.data") || name.ends_with(".hnsw.graph"))
}

/// Every chunk graph file in `dir`, data before graph for each basename.
pub fn chunk_graph_files(dir: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| is_chunk_graph_file(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// What one retirement removed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Retired {
    pub files: usize,
    pub bytes: u64,
    /// Entries the manifest's `chunk_id_map` held.
    pub id_map_entries: usize,
}

/// Where a retirement stops early (tests simulate a crash there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // the early stops are constructed by tests only
pub(crate) enum StopAt {
    Nowhere,
    BeforeManifest,
    AfterManifest,
    AfterFirstFile,
}

/// Whether the manifest still describes a chunk graph.
fn describes_a_chunk_graph(manifest: &IndexManifest) -> bool {
    !manifest.chunk_id_map.is_empty()
        || manifest.chunk_basename != default_chunk_basename()
        || manifest.chunk_embeddings_expected != 0
        || manifest.files.keys().any(|name| is_chunk_graph_file(name))
}

/// Remove the index-v3 chunk graph: rewrite the manifest without it, then unlink
/// the files. The caller has verified the generation that now serves chunks.
/// Safe to run again after any interruption.
pub fn retire_chunk_graph(dir: &Path) -> Result<Retired> {
    retire_chunk_graph_until(dir, StopAt::Nowhere)
}

pub(crate) fn retire_chunk_graph_until(dir: &Path, stop: StopAt) -> Result<Retired> {
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("index.lock"))
        .with_context(|| format!("opening {}", dir.join("index.lock").display()))?;
    lock_index(&lock, true, INDEX_LOCK_TIMEOUT)?;
    let mut retired = Retired::default();
    let manifest = std::fs::read_to_string(dir.join("manifest.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<IndexManifest>(&raw).ok())
        .filter(|manifest| manifest.version == MANIFEST_VERSION);
    if let Some(mut manifest) = manifest.filter(describes_a_chunk_graph) {
        if stop == StopAt::BeforeManifest {
            return Ok(retired);
        }
        retired.id_map_entries = manifest.chunk_id_map.len();
        manifest.chunk_id_map = Vec::new();
        manifest.chunk_basename = default_chunk_basename();
        manifest.chunk_embeddings_expected = 0;
        manifest.files.retain(|name, _| !is_chunk_graph_file(name));
        write_manifest_atomically(dir, &manifest)?;
    }
    if stop == StopAt::AfterManifest {
        return Ok(retired);
    }
    for path in chunk_graph_files(dir) {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                retired.files += 1;
                retired.bytes += bytes;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if stop == StopAt::AfterFirstFile {
            return Ok(retired);
        }
    }
    Ok(retired)
}

/// Remove stale numbered HNSW files from the index directory.
/// hnsw_rs creates numbered files (e.g. `chunks-7905.hnsw.data`) when mmap is active.
/// The files the current manifest names are retained; other numbered files are
/// stragglers from crashes, concurrent processes, old sessions, or a chunk graph
/// retired after a vector generation took over. The chunk pair is named only
/// while the manifest still describes one (a non-empty `chunk_id_map`, or a
/// numbered `chunk_basename`). A vector generation's own files (`chunks-g<N>.*`)
/// are never touched here.
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
    let chunk_graph_named =
        !manifest.chunk_id_map.is_empty() || manifest.chunk_basename != default_chunk_basename();
    let mut keep: HashSet<String> = HashSet::new();
    for (named, basename) in [
        (chunk_graph_named, &manifest.chunk_basename),
        (true, &manifest.reflection_basename),
    ] {
        if named {
            keep.insert(format!("{basename}.hnsw.data"));
            keep.insert(format!("{basename}.hnsw.graph"));
        }
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("chunks-g") {
            continue;
        }
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

impl SearchEngine {
    /// Open `watermark`'s generation and load everything that changed after
    /// its `seq` as the exact-scan delta.
    fn open_serve(
        index_dir: &Path,
        storage: &Storage,
        watermark: &GenManifest,
    ) -> Result<GenServe> {
        // Generation files are written once under unique names and never
        // modified, so opening (and hashing) one needs no index lock.
        let generation = Generation::open(index_dir, watermark)?;
        let (upserts, deleted) = index_gen::delta_since(storage, watermark.seq)?;
        Ok(GenServe::new(generation, upserts, deleted))
    }

    fn with_generation(mut self, serve: GenServe, seq: i64) -> Self {
        self.chunk_cursor = seq;
        self.chunk_gen = Some(serve);
        self
    }

    /// `watermark`'s generation serving chunks, and the reflection side from
    /// the manifest.
    pub(super) fn load_generation(
        index_dir: &Path,
        storage: &Storage,
        watermark: &GenManifest,
        expected_reflections: usize,
        budget: std::time::Duration,
    ) -> Result<Self> {
        let serve = Self::open_serve(index_dir, storage, watermark)?;
        let engine = Self::reflections_only(index_dir, expected_reflections, budget)?;
        Ok(engine.with_generation(serve, watermark.seq))
    }

    /// An engine holding the reflection side of the manifest in `index_dir`
    /// and no chunk vectors at all.
    fn reflections_only(
        index_dir: &Path,
        expected_reflections: usize,
        budget: std::time::Duration,
    ) -> Result<Self> {
        let mut engine = Self::new(16);
        let lock = match std::fs::File::open(index_dir.join("index.lock")) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(file) = &lock {
            lock_index(file, false, budget)?;
        }
        engine.load_reflection_side(index_dir, expected_reflections, lock.is_some())?;
        Ok(engine)
    }

    /// The engine to serve after [`Self::load_generation`] failed with `error`.
    /// No chunk graph is read, built or written: the chunk graph is retired
    /// once a generation serves, and rebuilding it here would write about
    /// 1.5 GB for a failure that a new generation repairs.
    ///
    /// 0. `None` for a lock timeout: a publisher holds `index.lock`, and the
    ///    caller's own load reports that instead of serving a degraded engine.
    /// 1. The failure may not be the generation's (a damaged reflection file
    ///    errs after the generation opened): the watermark's generation serves
    ///    and the reflection side starts empty, for reconciliation to backfill
    ///    from SQLite.
    /// 2. Roll back: the newest older generation that opens serves, with
    ///    everything changed after its `seq` as the delta.
    /// 3. Otherwise chunk search is empty (the keyword channels still answer)
    ///    until the daemon's next tick builds a generation (`regen_tick` sees
    ///    the engine is not serving the watermark and repairs it).
    pub(crate) fn load_after_failed_open(
        index_dir: &Path,
        storage: &Storage,
        expected_reflections: usize,
        budget: std::time::Duration,
        error: &anyhow::Error,
    ) -> Option<Self> {
        if format!("{error:#}").contains("timed out acquiring index.lock") {
            return None;
        }
        let watermark = index_gen::read_watermark(storage).ok().flatten();
        let reflections = || {
            Self::reflections_only(index_dir, expected_reflections, budget).unwrap_or_else(|e| {
                eprintln!("CSR: reflection index not loaded, rebuilding it from SQLite: {e:#}");
                Self::new(16)
            })
        };
        if let Some(watermark) = &watermark {
            if let Ok(serve) = Self::open_serve(index_dir, storage, watermark) {
                return Some(reflections().with_generation(serve, watermark.seq));
            }
        }
        let failed = watermark.map(|manifest| manifest.generation);
        for generation in index_gen::regen::list_generations(index_dir)
            .into_iter()
            .rev()
        {
            if failed.is_some_and(|failed| generation >= failed) {
                continue;
            }
            let Some(manifest) = read_generation_manifest(index_dir, generation) else {
                continue;
            };
            if let Ok(serve) = Self::open_serve(index_dir, storage, &manifest) {
                eprintln!("CSR: serving the previous vector generation g{generation} until a new one is built");
                return Some(reflections().with_generation(serve, manifest.seq));
            }
        }
        eprintln!("CSR: chunk vectors are unavailable until a new vector generation is built; search is keyword-only until then");
        let mut engine = reflections();
        engine.chunks_awaiting_generation = true;
        Some(engine)
    }

    /// Chunk kNN is, or is about to be, served from vector generations: the
    /// engine serves one, or is waiting for the next one to be built. The
    /// chunk graph is not in use and is neither backfilled nor rebuilt.
    pub fn is_generation_mode(&self) -> bool {
        self.chunk_gen.is_some() || self.chunks_awaiting_generation
    }

    /// No generation serves chunks and none could be opened: chunk search is
    /// empty until the regeneration tick builds one.
    pub fn is_awaiting_generation(&self) -> bool {
        self.chunks_awaiting_generation
    }
}

fn read_generation_manifest(index_dir: &Path, generation: u64) -> Option<GenManifest> {
    let path = index_gen::manifest_path(index_dir, &index_gen::basename(generation));
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}
