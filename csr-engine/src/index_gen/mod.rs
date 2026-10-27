//! USearch chunk-vector generation: an immutable, mmap-viewable f16 index of
//! every chunk embedding, published as three files beside the hnsw_rs cache
//! (`chunks-g<N>.usearch`, `.ids`, `.json`) plus a watermark row in `meta`.
//!
//! Keys are row positions in the id sidecar. The generation never changes after
//! it is written; rows that changed since `seq` come from `delta_since`.
//! Reflections stay on hnsw_rs. The offline tools live in [`cli`].

pub mod cli;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::storage::Storage;

/// Meta row that names the active generation (JSON of [`GenManifest`]).
pub const WATERMARK_KEY: &str = "vector_generation";
/// Meta row that names the active episode-vector generation. A separate key so
/// the chunk watermark is never touched by the episode family.
pub const EPISODE_WATERMARK_KEY: &str = "episode_vector_generation";
pub const DIMS: usize = 384;
pub const CONNECTIVITY: usize = 16;
pub const EXPANSION_ADD: usize = 128;
/// Equals `EF_SEARCH` in src/search/mod.rs so the two engines are compared at the same effort.
pub const EXPANSION_SEARCH: usize = 100;
const SCALAR: &str = "f16";
/// Points handed to a builder thread at a time.
const ADD_BATCH: usize = 2048;
const PROGRESS_EVERY: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenManifest {
    pub generation: u64,
    pub basename: String,
    pub count: usize,
    pub dims: usize,
    pub scalar: String,
    pub m: usize,
    /// `embedding_changes.seq` the generation covers; `delta_since` resumes after it.
    pub seq: i64,
    pub usearch_blake3: String,
    pub ids_blake3: String,
}

fn options(dims: usize) -> IndexOptions {
    IndexOptions {
        dimensions: dims,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F16,
        connectivity: CONNECTIVITY,
        expansion_add: EXPANSION_ADD,
        expansion_search: EXPANSION_SEARCH,
        multi: false,
    }
}

pub fn basename(generation: u64) -> String {
    format!("chunks-g{generation}")
}

/// Episode family basename (`episodes-g<N>`); same file layout as the chunk family.
pub fn episode_basename(generation: u64) -> String {
    format!("episodes-g{generation}")
}

pub fn usearch_path(dir: &Path, basename: &str) -> PathBuf {
    dir.join(format!("{basename}.usearch"))
}

pub fn ids_path(dir: &Path, basename: &str) -> PathBuf {
    dir.join(format!("{basename}.ids"))
}

pub fn manifest_path(dir: &Path, basename: &str) -> PathBuf {
    dir.join(format!("{basename}.json"))
}

pub fn blake3_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(file)?;
    Ok(hasher.finalize().to_hex().to_string())
}

fn sync_file(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn tmp_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Build a generation from row-major `vectors` (`ids.len() * dims` floats) and
/// write it to `out_dir`. Every file is written under a tmp name, fsynced and
/// renamed; the manifest goes last, so a reader never sees a half generation.
pub fn build(
    vectors: &[f32],
    dims: usize,
    ids: &[String],
    out_dir: &Path,
    generation: u64,
    seq: i64,
    threads: usize,
) -> Result<GenManifest> {
    build_family(
        basename(generation),
        vectors,
        dims,
        ids,
        out_dir,
        generation,
        seq,
        threads,
    )
}

/// [`build`] with the file basename chosen by the caller (the chunk and
/// episode families share every other step).
#[allow(clippy::too_many_arguments)]
fn build_family(
    base: String,
    vectors: &[f32],
    dims: usize,
    ids: &[String],
    out_dir: &Path,
    generation: u64,
    seq: i64,
    threads: usize,
) -> Result<GenManifest> {
    let n = ids.len();
    if dims == 0 || vectors.len() != n * dims {
        bail!(
            "vectors hold {} floats, expected {} ids x {} dims",
            vectors.len(),
            n,
            dims
        );
    }
    if let Some(bad) = ids.iter().find(|id| id.contains('\n')) {
        bail!("id contains a newline: {bad:?}");
    }
    std::fs::create_dir_all(out_dir)?;
    let threads = threads.max(1);

    let index = Index::new(&options(dims)).map_err(|e| anyhow!("usearch new: {e}"))?;
    index
        .reserve_capacity_and_threads(n.max(1), threads)
        .map_err(|e| anyhow!("usearch reserve: {e}"))?;

    let started = Instant::now();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let start = next.fetch_add(ADD_BATCH, Ordering::Relaxed);
                if start >= n {
                    return;
                }
                let end = (start + ADD_BATCH).min(n);
                for row in start..end {
                    let v = &vectors[row * dims..(row + 1) * dims];
                    if let Err(e) = index.add(row as u64, v) {
                        stop.store(true, Ordering::Relaxed);
                        if let Ok(mut slot) = failure.lock() {
                            slot.get_or_insert_with(|| format!("add row {row}: {e}"));
                        }
                        return;
                    }
                }
                let after = done.fetch_add(end - start, Ordering::Relaxed) + (end - start);
                if after / PROGRESS_EVERY != (after - (end - start)) / PROGRESS_EVERY {
                    eprintln!(
                        "build progress added={after}/{n} secs={:.1}",
                        started.elapsed().as_secs_f64()
                    );
                }
            });
        }
    });
    if let Some(message) = failure.into_inner().ok().flatten() {
        bail!("usearch {message}");
    }
    if index.size() != n {
        bail!("usearch holds {} points after adding {}", index.size(), n);
    }

    let usearch_final = usearch_path(out_dir, &base);
    let usearch_tmp = tmp_name(&usearch_final);
    index
        .save(&usearch_tmp.to_string_lossy())
        .map_err(|e| anyhow!("usearch save: {e}"))?;
    sync_file(&usearch_tmp)?;
    let usearch_blake3 = blake3_file(&usearch_tmp)?;

    let ids_final = ids_path(out_dir, &base);
    let ids_tmp = tmp_name(&ids_final);
    {
        let mut w = BufWriter::new(File::create(&ids_tmp)?);
        for id in ids {
            w.write_all(id.as_bytes())?;
            w.write_all(b"\n")?;
        }
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    let ids_blake3 = blake3_file(&ids_tmp)?;

    let manifest = GenManifest {
        generation,
        basename: base.clone(),
        count: n,
        dims,
        scalar: SCALAR.to_string(),
        m: CONNECTIVITY,
        seq,
        usearch_blake3,
        ids_blake3,
    };
    let manifest_final = manifest_path(out_dir, &base);
    let manifest_tmp = tmp_name(&manifest_final);
    {
        let mut w = BufWriter::new(File::create(&manifest_tmp)?);
        serde_json::to_writer_pretty(&mut w, &manifest)?;
        w.flush()?;
        w.get_ref().sync_all()?;
    }
    std::fs::rename(&usearch_tmp, &usearch_final)?;
    std::fs::rename(&ids_tmp, &ids_final)?;
    std::fs::rename(&manifest_tmp, &manifest_final)?;
    sync_file(out_dir)?;
    Ok(manifest)
}

/// An opened generation: the usearch file is memory-mapped, not copied.
pub struct Generation {
    index: Index,
    ids: Vec<String>,
    manifest: GenManifest,
}

impl Generation {
    /// Verify both files against the manifest (blake3), then mmap the index.
    pub fn open(index_dir: &Path, manifest: &GenManifest) -> Result<Generation> {
        if manifest.scalar != SCALAR {
            bail!("generation scalar {:?} is not {SCALAR}", manifest.scalar);
        }
        let usearch_file = usearch_path(index_dir, &manifest.basename);
        let ids_file = ids_path(index_dir, &manifest.basename);
        let ids_hash = blake3_file(&ids_file)?;
        if ids_hash != manifest.ids_blake3 {
            bail!(
                "{} blake3 {ids_hash} does not match manifest {}",
                ids_file.display(),
                manifest.ids_blake3
            );
        }
        let usearch_hash = blake3_file(&usearch_file)?;
        if usearch_hash != manifest.usearch_blake3 {
            bail!(
                "{} blake3 {usearch_hash} does not match manifest {}",
                usearch_file.display(),
                manifest.usearch_blake3
            );
        }
        let text = std::fs::read_to_string(&ids_file)?;
        let ids: Vec<String> = text.lines().map(str::to_string).collect();
        if ids.len() != manifest.count {
            bail!(
                "ids file holds {} ids, manifest says {}",
                ids.len(),
                manifest.count
            );
        }
        let index = Index::new(&options(manifest.dims)).map_err(|e| anyhow!("usearch new: {e}"))?;
        index
            .view(&usearch_file.to_string_lossy())
            .map_err(|e| anyhow!("usearch view: {e}"))?;
        index.change_expansion_search(EXPANSION_SEARCH);
        if index.size() != manifest.count {
            bail!(
                "usearch holds {} points, manifest says {}",
                index.size(),
                manifest.count
            );
        }
        Ok(Generation {
            index,
            ids,
            manifest: manifest.clone(),
        })
    }

    pub fn manifest(&self) -> &GenManifest {
        &self.manifest
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Nearest `k` chunks by cosine, best first. Score is cosine similarity.
    pub fn search(&self, q: &[f32], k: usize) -> Vec<(String, f32)> {
        if q.len() != self.manifest.dims || k == 0 {
            return Vec::new();
        }
        let Ok(found) = self.index.search::<f32>(q, k) else {
            return Vec::new();
        };
        found
            .keys
            .iter()
            .zip(found.distances.iter())
            .filter_map(|(key, dist)| {
                self.ids
                    .get(*key as usize)
                    .map(|id| (id.clone(), 1.0 - *dist))
            })
            .collect()
    }
}

pub fn read_watermark(storage: &Storage) -> Result<Option<GenManifest>> {
    match storage.get_meta(WATERMARK_KEY)? {
        Some(text) => Ok(Some(
            serde_json::from_str(&text).context("parse vector_generation watermark")?,
        )),
        None => Ok(None),
    }
}

pub fn write_watermark(storage: &Storage, manifest: &GenManifest) -> Result<()> {
    storage.set_meta(WATERMARK_KEY, &serde_json::to_string(manifest)?)
}

// ------------------------------------------------------- episode family

/// Counts the episode build reports next to the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeBuildReport {
    /// Vectors kept in the generation.
    pub count: usize,
    /// Live episodes (rows in `episodes`) that have no vector.
    pub missing: usize,
    /// Distinct vector ids dropped because no live episode has them.
    pub stale: usize,
}

pub fn read_episode_watermark(storage: &Storage) -> Result<Option<GenManifest>> {
    match storage.get_meta(EPISODE_WATERMARK_KEY)? {
        Some(text) => Ok(Some(
            serde_json::from_str(&text).context("parse episode_vector_generation watermark")?,
        )),
        None => Ok(None),
    }
}

pub fn write_episode_watermark(storage: &Storage, manifest: &GenManifest) -> Result<()> {
    storage.set_meta(EPISODE_WATERMARK_KEY, &serde_json::to_string(manifest)?)
}

/// Open an episode generation: same blake3 check and mmap as the chunk family,
/// but only for a manifest that names an `episodes-g<N>` file set.
pub fn open_episode_generation(index_dir: &Path, manifest: &GenManifest) -> Result<Generation> {
    if manifest.basename != episode_basename(manifest.generation) {
        bail!(
            "manifest basename {:?} is not an episode generation",
            manifest.basename
        );
    }
    Generation::open(index_dir, manifest)
}

/// Ids of every episode in the DB's episodes table (built with `--since-days 90`).
pub fn live_episode_ids(storage: &Storage) -> Result<HashSet<String>> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare("SELECT id FROM episodes")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for id in rows {
            out.insert(id?);
        }
        Ok(out)
    })
}

/// One embed-sample prefix: `<prefix>.d384.f32`, `<prefix>.ids`, `<prefix>.meta.json`.
fn read_vec_prefix(prefix: &Path) -> Result<(Vec<String>, Vec<f32>)> {
    let with = |ext: &str| {
        let mut name = prefix.as_os_str().to_os_string();
        name.push(ext);
        PathBuf::from(name)
    };
    let meta_file = with(".meta.json");
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_file).with_context(|| {
            format!(
                "read {} (written last; is the run finished?)",
                meta_file.display()
            )
        })?)
        .with_context(|| format!("parse {}", meta_file.display()))?;
    let dims: Vec<u64> = match meta.get("dims") {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_u64()).collect(),
        Some(v) => v.as_u64().into_iter().collect(),
        None => Vec::new(),
    };
    if dims != [DIMS as u64] {
        bail!("{}: dims {dims:?}, expected [{DIMS}]", meta_file.display());
    }
    let ids_file = with(".ids");
    let ids: Vec<String> = std::fs::read_to_string(&ids_file)
        .with_context(|| format!("read {}", ids_file.display()))?
        .lines()
        .map(str::to_string)
        .collect();
    let f32_file = with(&format!(".d{DIMS}.f32"));
    let bytes = std::fs::read(&f32_file).with_context(|| format!("read {}", f32_file.display()))?;
    let want = ids.len() * DIMS * 4;
    if bytes.len() != want {
        bail!(
            "{}: {} bytes, expected {} ids x {DIMS} x 4 = {want}",
            f32_file.display(),
            bytes.len(),
            ids.len()
        );
    }
    Ok((ids, decode_f32(&bytes)))
}

/// Merge prefixes (last one wins per id), keep live ids only, in sorted id order.
pub fn merge_episode_vectors(
    prefixes: &[PathBuf],
    live: &HashSet<String>,
) -> Result<(Vec<String>, Vec<f32>, EpisodeBuildReport)> {
    if prefixes.is_empty() {
        bail!("no vector prefix given");
    }
    let mut winner: std::collections::HashMap<String, (usize, usize)> =
        std::collections::HashMap::new();
    let mut loaded: Vec<Vec<f32>> = Vec::with_capacity(prefixes.len());
    for (pi, prefix) in prefixes.iter().enumerate() {
        let (ids, vectors) = read_vec_prefix(prefix)?;
        for (row, id) in ids.into_iter().enumerate() {
            winner.insert(id, (pi, row));
        }
        loaded.push(vectors);
    }
    let stale = winner.keys().filter(|id| !live.contains(*id)).count();
    let mut keep: Vec<&String> = winner.keys().filter(|id| live.contains(*id)).collect();
    keep.sort_unstable();
    let missing = live.len() - keep.len();
    let mut ids = Vec::with_capacity(keep.len());
    let mut vectors = Vec::with_capacity(keep.len() * DIMS);
    for id in keep {
        let (pi, row) = winner[id];
        vectors.extend_from_slice(&loaded[pi][row * DIMS..(row + 1) * DIMS]);
        ids.push(id.clone());
    }
    let report = EpisodeBuildReport {
        count: ids.len(),
        missing,
        stale,
    };
    Ok((ids, vectors, report))
}

/// Build `episodes-g<generation>` into `out_dir` from embed-sample prefixes,
/// keeping only episodes live in the DB. Does not write the watermark.
pub fn build_episode_generation(
    storage: &Storage,
    prefixes: &[PathBuf],
    out_dir: &Path,
    generation: u64,
    threads: usize,
) -> Result<(GenManifest, EpisodeBuildReport)> {
    let live = live_episode_ids(storage)?;
    let (ids, vectors, report) = merge_episode_vectors(prefixes, &live)?;
    eprintln!(
        "episodes build live={} kept={} missing={} stale={}",
        live.len(),
        report.count,
        report.missing,
        report.stale
    );
    let manifest = build_family(
        episode_basename(generation),
        &vectors,
        DIMS,
        &ids,
        out_dir,
        generation,
        0,
        threads,
    )?;
    Ok((manifest, report))
}

/// Chunk embeddings that changed after `seq`: current vectors for rows that
/// still exist, ids for rows that were deleted (no embedding row any more).
#[allow(clippy::type_complexity)]
pub fn delta_since(
    storage: &Storage,
    seq: i64,
) -> Result<(Vec<(String, Vec<f32>)>, HashSet<String>)> {
    storage.with_connection(|conn| {
        let mut stmt = conn.prepare(
            "SELECT d.id, c.embedding
             FROM embedding_changes d
             LEFT JOIN chunk_embeddings c ON c.chunk_id = d.id
             WHERE d.kind = 0 AND d.seq > ?1
             ORDER BY d.seq",
        )?;
        let mut rows = stmt.query([seq])?;
        let mut upserts = Vec::new();
        let mut deleted = HashSet::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let blob: Option<Vec<u8>> = row.get(1)?;
            match blob {
                Some(bytes) => upserts.push((id, decode_f32(&bytes))),
                None => {
                    deleted.insert(id);
                }
            }
        }
        Ok((upserts, deleted))
    })
}

pub fn decode_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64: a fixed, dependency-free generator so every run sees the same data.
    pub(crate) struct Rng(pub u64);
    impl Rng {
        pub(crate) fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        /// Roughly standard normal: sum of 12 uniforms minus 6.
        pub(crate) fn gauss(&mut self) -> f32 {
            let mut s = 0.0f32;
            for _ in 0..12 {
                s += (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32;
            }
            s - 6.0
        }
    }

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("chunk-{i}")).collect()
    }

    fn gaussian(n: usize, seed: u64) -> Vec<f32> {
        let mut rng = Rng(seed);
        (0..n * DIMS).map(|_| rng.gauss()).collect()
    }

    fn normalise(v: &mut [f32]) {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            v.iter_mut().for_each(|x| *x /= norm);
        }
    }

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn build_then_view_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let n = 500;
        let vectors = gaussian(n, 1);
        let ids = ids(n);
        let m = build(&vectors, DIMS, &ids, dir.path(), 7, 42, 4).unwrap();
        assert_eq!((m.generation, m.count, m.seq, m.dims), (7, n, 42, DIMS));
        assert_eq!(m.basename, "chunks-g7");
        for ext in ["usearch", "ids", "json"] {
            assert!(dir.path().join(format!("chunks-g7.{ext}")).exists());
        }
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "tmp files left behind");
        let on_disk: GenManifest = serde_json::from_str(
            &std::fs::read_to_string(manifest_path(dir.path(), &m.basename)).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk, m);

        let generation = Generation::open(dir.path(), &m).unwrap();
        assert_eq!(generation.len(), n);
        for row in [0usize, 17, 499] {
            let hits = generation.search(&vectors[row * DIMS..(row + 1) * DIMS], 3);
            assert_eq!(hits[0].0, format!("chunk-{row}"));
            assert!(hits[0].1 > 0.99, "self cosine {}", hits[0].1);
        }
        assert!(generation.search(&vectors[..10], 3).is_empty());
    }

    #[test]
    fn blake3_mismatch_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let vectors = gaussian(100, 2);
        let m = build(&vectors, DIMS, &ids(100), dir.path(), 1, 0, 2).unwrap();
        assert!(Generation::open(dir.path(), &m).is_ok());

        let ids_file = ids_path(dir.path(), &m.basename);
        let original = std::fs::read(&ids_file).unwrap();
        let mut bad = original.clone();
        bad[0] = b'X';
        std::fs::write(&ids_file, &bad).unwrap();
        let err = Generation::open(dir.path(), &m).err().unwrap().to_string();
        assert!(err.contains("blake3"), "{err}");
        std::fs::write(&ids_file, &original).unwrap();

        let index_file = usearch_path(dir.path(), &m.basename);
        let mut bytes = std::fs::read(&index_file).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&index_file, &bytes).unwrap();
        let err = Generation::open(dir.path(), &m).err().unwrap().to_string();
        assert!(err.contains("blake3"), "{err}");
    }

    fn sample_manifest() -> GenManifest {
        GenManifest {
            generation: 3,
            basename: "chunks-g3".into(),
            count: 10,
            dims: DIMS,
            scalar: "f16".into(),
            m: CONNECTIVITY,
            seq: 99,
            usearch_blake3: "aa".into(),
            ids_blake3: "bb".into(),
        }
    }

    #[test]
    fn watermark_writes_and_reads_back() {
        let storage = Storage::open_memory().unwrap();
        assert!(read_watermark(&storage).unwrap().is_none());
        let m = sample_manifest();
        write_watermark(&storage, &m).unwrap();
        assert_eq!(read_watermark(&storage).unwrap(), Some(m.clone()));
        let newer = GenManifest {
            generation: 4,
            seq: 120,
            ..m
        };
        write_watermark(&storage, &newer).unwrap();
        assert_eq!(read_watermark(&storage).unwrap(), Some(newer));
    }

    #[test]
    fn delta_since_returns_upserts_and_deletes() {
        let storage = Storage::open_memory().unwrap();
        let blob = |x: f32| {
            let mut b = Vec::new();
            for i in 0..DIMS {
                b.extend_from_slice(&(x + i as f32).to_le_bytes());
            }
            b
        };
        let mark = storage
            .with_connection(|conn| {
                for id in ["a", "b", "c"] {
                    conn.execute(
                        "INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES(?1,'conv','project','2026-09-26','content',1)",
                        [id],
                    )?;
                    conn.execute(
                        "INSERT INTO chunk_embeddings(chunk_id,embedding) VALUES(?1,?2)",
                        rusqlite::params![id, blob(1.0)],
                    )?;
                }
                Ok(conn.query_row(
                    "SELECT COALESCE(MAX(seq),0) FROM embedding_changes",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap();
        let (up, del) = delta_since(&storage, mark).unwrap();
        assert!(up.is_empty() && del.is_empty());
        assert_eq!(delta_since(&storage, 0).unwrap().0.len(), 3);

        storage
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE chunk_embeddings SET embedding=?1 WHERE chunk_id='a'",
                    [blob(5.0)],
                )?;
                conn.execute(
                    "INSERT INTO chunks(id,conversation_id,project_name,timestamp,content,message_count) VALUES('d','conv','project','2026-09-26','content',1)",
                    [],
                )?;
                conn.execute(
                    "INSERT INTO chunk_embeddings(chunk_id,embedding) VALUES('d',?1)",
                    [blob(9.0)],
                )?;
                conn.execute("DELETE FROM chunk_embeddings WHERE chunk_id='b'", [])?;
                Ok(())
            })
            .unwrap();
        let (up, del) = delta_since(&storage, mark).unwrap();
        let mut up_ids: Vec<&str> = up.iter().map(|(id, _)| id.as_str()).collect();
        up_ids.sort_unstable();
        assert_eq!(up_ids, ["a", "d"]);
        assert_eq!(del, HashSet::from(["b".to_string()]));
        let a = up.iter().find(|(id, _)| id == "a").unwrap();
        assert_eq!(a.1.len(), DIMS);
        assert_eq!(a.1[0], 5.0);
    }

    fn write_prefix(dir: &Path, name: &str, ids: &[&str], vectors: &[f32]) -> PathBuf {
        let prefix = dir.join(name);
        let with = |ext: &str| PathBuf::from(format!("{}{ext}", prefix.display()));
        let mut bytes = Vec::new();
        for x in vectors {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        std::fs::write(with(".d384.f32"), bytes).unwrap();
        std::fs::write(with(".ids"), ids.join("\n") + "\n").unwrap();
        std::fs::write(
            with(".meta.json"),
            format!("{{\"dims\":[384],\"n\":{}}}", ids.len()),
        )
        .unwrap();
        prefix
    }

    fn insert_episodes(storage: &Storage, ids: &[&str]) {
        storage
            .with_connection(|conn| {
                for (i, id) in ids.iter().enumerate() {
                    conn.execute(
                        "INSERT INTO episodes(id,conversation_id,project_name,ordinal,first_uuid,last_uuid,timestamp,text,token_est) VALUES(?1,'conv','project',?2,'a','b','2026-10-01T00:00:00Z','text',1)",
                        rusqlite::params![id, i as i64],
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn episode_watermark_is_independent_of_chunk_watermark() {
        let storage = Storage::open_memory().unwrap();
        assert!(read_episode_watermark(&storage).unwrap().is_none());
        let chunk = sample_manifest();
        write_watermark(&storage, &chunk).unwrap();
        let before = storage.get_meta(WATERMARK_KEY).unwrap();

        let episode = GenManifest {
            generation: 2,
            basename: episode_basename(2),
            count: 5,
            seq: 0,
            ..sample_manifest()
        };
        write_episode_watermark(&storage, &episode).unwrap();
        assert_eq!(read_episode_watermark(&storage).unwrap(), Some(episode));
        assert_eq!(storage.get_meta(WATERMARK_KEY).unwrap(), before);
        assert_eq!(read_watermark(&storage).unwrap(), Some(chunk));
        assert_eq!(basename(2), "chunks-g2");
        assert_eq!(EPISODE_WATERMARK_KEY, "episode_vector_generation");
    }

    #[test]
    fn stale_id_dropped_and_missing_id_counted() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open_memory().unwrap();
        insert_episodes(&storage, &["ep:a", "ep:b", "ep:c"]);
        let mut v = gaussian(3, 11);
        v.chunks_mut(DIMS).for_each(normalise);
        let prefix = write_prefix(dir.path(), "base", &["ep:a", "ep:b", "ep:gone"], &v);
        let out = dir.path().join("index-v3");
        let (m, report) = build_episode_generation(&storage, &[prefix], &out, 1, 2).unwrap();
        assert_eq!(
            report,
            EpisodeBuildReport {
                count: 2,
                missing: 1,
                stale: 1
            }
        );
        assert_eq!(m.basename, "episodes-g1");
        assert_eq!(m.count, 2);
        assert!(out.join("episodes-g1.usearch").exists());
        assert!(!out.join("chunks-g1.usearch").exists());
        let generation = open_episode_generation(&out, &m).unwrap();
        assert_eq!(generation.ids(), ["ep:a", "ep:b"]);
        assert!(open_episode_generation(
            &out,
            &GenManifest {
                basename: "chunks-g1".into(),
                ..m
            }
        )
        .is_err());
    }

    #[test]
    fn last_prefix_wins() {
        let dir = tempfile::tempdir().unwrap();
        let mut v = gaussian(3, 21);
        v.chunks_mut(DIMS).for_each(normalise);
        let base = write_prefix(dir.path(), "base", &["ep:a", "ep:b"], &v[..2 * DIMS]);
        let delta = write_prefix(dir.path(), "delta", &["ep:a"], &v[2 * DIMS..]);
        let live: HashSet<String> = ["ep:a", "ep:b"].iter().map(|s| s.to_string()).collect();

        let (ids, vectors, report) =
            merge_episode_vectors(&[base.clone(), delta.clone()], &live).unwrap();
        assert_eq!(ids, ["ep:a", "ep:b"]);
        assert_eq!(&vectors[..DIMS], &v[2 * DIMS..]);
        assert_eq!(&vectors[DIMS..], &v[DIMS..2 * DIMS]);
        assert_eq!((report.count, report.missing, report.stale), (2, 0, 0));

        let (_, vectors, _) = merge_episode_vectors(&[delta, base], &live).unwrap();
        assert_eq!(&vectors[..DIMS], &v[..DIMS]);
    }

    #[test]
    fn size_mismatch_and_wrong_dims_refused() {
        let dir = tempfile::tempdir().unwrap();
        let v = gaussian(2, 31);
        let prefix = write_prefix(dir.path(), "short", &["ep:a", "ep:b"], &v);
        let f32_file = PathBuf::from(format!("{}.d384.f32", prefix.display()));
        let mut bytes = std::fs::read(&f32_file).unwrap();
        bytes.truncate(bytes.len() - 4);
        std::fs::write(&f32_file, bytes).unwrap();
        let live: HashSet<String> = ["ep:a"].iter().map(|s| s.to_string()).collect();
        let err = merge_episode_vectors(std::slice::from_ref(&prefix), &live)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("expected 2 ids x 384 x 4"), "{err}");

        let prefix = write_prefix(dir.path(), "dims", &["ep:a"], &v[..DIMS]);
        std::fs::write(
            format!("{}.meta.json", prefix.display()),
            "{\"dims\":[768]}",
        )
        .unwrap();
        let err = merge_episode_vectors(&[prefix], &live)
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("dims"), "{err}");
    }

    /// Unit vectors drawn around `centers` cluster centres, the shape real
    /// embeddings have (low intrinsic dimension). Queries are perturbed points.
    fn clustered(n: usize, centers: usize, spread: f32, seed: u64) -> Vec<f32> {
        let mut rng = Rng(seed);
        let cs: Vec<Vec<f32>> = (0..centers)
            .map(|_| (0..DIMS).map(|_| rng.gauss()).collect())
            .collect();
        let mut out = Vec::with_capacity(n * DIMS);
        for i in 0..n {
            let c = &cs[(rng.next_u64() as usize ^ i) % centers];
            let mut v: Vec<f32> = c.iter().map(|x| x + spread * rng.gauss()).collect();
            normalise(&mut v);
            out.extend_from_slice(&v);
        }
        out
    }

    fn recall_of(vectors: &[f32], n: usize, queries: usize, dir: &Path) -> f32 {
        let ids = ids(n);
        let m = build(vectors, DIMS, &ids, dir, 1, 0, 4).unwrap();
        let generation = Generation::open(dir, &m).unwrap();
        let mut rng = Rng(77);
        let mut hits = 0usize;
        for _ in 0..queries {
            let row = (rng.next_u64() as usize) % n;
            let mut q: Vec<f32> = vectors[row * DIMS..(row + 1) * DIMS]
                .iter()
                .map(|x| x + 0.01 * rng.gauss())
                .collect();
            normalise(&mut q);
            let mut scored: Vec<(f32, usize)> = (0..n)
                .map(|r| (dot(&q, &vectors[r * DIMS..(r + 1) * DIMS]), r))
                .collect();
            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
            let truth: HashSet<String> = scored
                .iter()
                .take(10)
                .map(|(_, r)| format!("chunk-{r}"))
                .collect();
            hits += generation
                .search(&q, 10)
                .iter()
                .filter(|(id, _)| truth.contains(id))
                .count();
        }
        hits as f32 / (queries * 10) as f32
    }

    #[test]
    fn recall_at_10_over_20k_vectors() {
        let n = 20_000;
        let mut unit = gaussian(n, 5);
        unit.chunks_mut(DIMS).for_each(normalise);
        let dir = tempfile::tempdir().unwrap();
        let isotropic = recall_of(&unit, n, 200, dir.path());
        println!("recall_at_10_isotropic_gaussian={isotropic:.4}");

        let structured = clustered(n, 200, 0.7, 6);
        let dir = tempfile::tempdir().unwrap();
        let recall = recall_of(&structured, n, 200, dir.path());
        println!("recall_at_10={recall:.4}");
        assert!(recall >= 0.95, "recall_at_10={recall}");
    }
}
