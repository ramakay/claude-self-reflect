//! `csr-engine vindex <action>`: the offline tools S5 measures the USearch
//! generation with. None of them touch the serving path.

use anyhow::{anyhow, bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{
    build, decode_f32, delta_since, ids_path, manifest_path, read_watermark, usearch_path,
    write_watermark, GenManifest, Generation, DIMS,
};
use crate::search::SearchEngine;
use crate::storage::Storage;

#[derive(Subcommand, Debug)]
pub enum VindexAction {
    /// Dump every chunk embedding to vectors.f32 + ids.txt + export.json (one read transaction).
    Export {
        #[arg(long)]
        out: PathBuf,
    },
    /// Exact top-k neighbours (f32 cosine) for sampled query rows.
    Truth {
        #[arg(long = "vec")]
        vec_dir: PathBuf,
        #[arg(long, default_value_t = 10_000)]
        queries: usize,
        #[arg(long, default_value_t = 20_261_026)]
        seed: u64,
        #[arg(long, default_value_t = 10)]
        k: usize,
        #[arg(long, default_value_t = 6)]
        threads: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build a USearch f16 generation from an export.
    Build {
        #[arg(long = "vec")]
        vec_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 1)]
        generation: u64,
        #[arg(long, default_value_t = 6)]
        threads: usize,
    },
    /// Copy a built generation into <db parent>/index-v3, verify, write the watermark last.
    Activate {
        #[arg(long)]
        gen_dir: PathBuf,
        #[arg(long, default_value_t = 1)]
        generation: u64,
    },
    /// Recall@k of the activated generation or today's hnsw_rs index against a truth file.
    Recall {
        #[arg(long)]
        truth: PathBuf,
        #[arg(long = "vec")]
        vec_dir: PathBuf,
        #[arg(long, value_enum)]
        side: Side,
        #[arg(long, default_value_t = 10)]
        k: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// Open-time of the generation vs the hnsw_rs load, alternating.
    LoadBench {
        #[arg(long, default_value_t = 5)]
        runs: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// Drive a stdio MCP server through the probe + audit queries and record its memory.
    ServeRss {
        #[arg(long)]
        server_bin: PathBuf,
        #[arg(long)]
        probes: PathBuf,
        #[arg(long)]
        audit: PathBuf,
        #[arg(long, default_value_t = 50)]
        audit_limit: usize,
        /// Extra environment for the server, KEY=VALUE (repeatable).
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Gen,
    Hnsw,
}

pub fn run(db_path: &Path, action: &VindexAction) -> Result<()> {
    match action {
        VindexAction::Export { out } => export(db_path, out),
        VindexAction::Truth {
            vec_dir,
            queries,
            seed,
            k,
            threads,
            out,
        } => truth(vec_dir, *queries, *seed, *k, *threads, out),
        VindexAction::Build {
            vec_dir,
            out,
            generation,
            threads,
        } => build_cmd(vec_dir, out, *generation, *threads),
        VindexAction::Activate {
            gen_dir,
            generation,
        } => activate(db_path, gen_dir, *generation),
        VindexAction::Recall {
            truth,
            vec_dir,
            side,
            k,
            out,
        } => recall(db_path, truth, vec_dir, *side, *k, out),
        VindexAction::LoadBench { runs, out } => load_bench(db_path, *runs, out),
        VindexAction::ServeRss {
            server_bin,
            probes,
            audit,
            audit_limit,
            env,
            out,
        } => serve_rss(db_path, server_bin, probes, audit, *audit_limit, env, out),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("write {}", path.display()))
}

fn index_dir_of(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("index-v3")
}

/// Resident set size of a process in bytes, via `ps` (0 when unreadable).
fn rss_bytes(pid: u32) -> u64 {
    Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    crate::eval::probe::nearest_rank_percentile(sorted, p)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------- export

#[derive(Serialize, Deserialize)]
struct ExportMeta {
    n: usize,
    dims: usize,
    seq: i64,
    sha256: String,
}

fn export(db_path: &Path, out: &Path) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let storage = Storage::open_read_only(db_path)
        .with_context(|| format!("open {} read-only", db_path.display()))?;
    let started = Instant::now();
    let vectors_path = out.join("vectors.f32");
    let ids_file = out.join("ids.txt");
    let meta = storage.with_connection(|conn| {
        // One read transaction: the snapshot starts at the first read, so the
        // seq below is never newer than any vector exported after it.
        let tx = conn.unchecked_transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM embedding_changes",
            [],
            |r| r.get(0),
        )?;
        let mut vw = BufWriter::with_capacity(1 << 22, File::create(&vectors_path)?);
        let mut iw = BufWriter::new(File::create(&ids_file)?);
        let mut hasher = Sha256::new();
        let mut stmt = tx.prepare("SELECT chunk_id, embedding FROM chunk_embeddings")?;
        let mut rows = stmt.query([])?;
        let mut n = 0usize;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            if blob.len() != DIMS * 4 {
                bail!(
                    "chunk {id}: embedding is {} bytes, expected {}",
                    blob.len(),
                    DIMS * 4
                );
            }
            if id.contains('\n') {
                bail!("chunk id contains a newline: {id:?}");
            }
            hasher.update(&blob);
            vw.write_all(&blob)?;
            iw.write_all(id.as_bytes())?;
            iw.write_all(b"\n")?;
            n += 1;
            if n.is_multiple_of(200_000) {
                eprintln!(
                    "export progress rows={n} secs={:.1}",
                    started.elapsed().as_secs_f64()
                );
            }
        }
        vw.flush()?;
        iw.flush()?;
        Ok(ExportMeta {
            n,
            dims: DIMS,
            seq,
            sha256: hex(&hasher.finalize()),
        })
    })?;
    write_json(&out.join("export.json"), &serde_json::to_value(&meta)?)?;
    println!(
        "n={} dims={} seq={} sha256={} secs={:.1}",
        meta.n,
        meta.dims,
        meta.seq,
        meta.sha256,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

struct VecDir {
    meta: ExportMeta,
    vectors: Vec<f32>,
    ids: Vec<String>,
}

fn load_vec_dir(dir: &Path) -> Result<VecDir> {
    let meta: ExportMeta = serde_json::from_str(
        &std::fs::read_to_string(dir.join("export.json"))
            .with_context(|| format!("read {}/export.json", dir.display()))?,
    )?;
    let ids: Vec<String> = BufReader::new(File::open(dir.join("ids.txt"))?)
        .lines()
        .collect::<std::io::Result<_>>()?;
    if ids.len() != meta.n {
        bail!("ids.txt has {} ids, export.json says {}", ids.len(), meta.n);
    }
    let mut file = File::open(dir.join("vectors.f32"))?;
    let expected = (meta.n * meta.dims * 4) as u64;
    if file.metadata()?.len() != expected {
        bail!(
            "vectors.f32 is {} bytes, expected {expected}",
            file.metadata()?.len()
        );
    }
    let mut vectors = Vec::with_capacity(meta.n * meta.dims);
    let mut buf = vec![0u8; meta.dims * 4 * 1024];
    let mut left = expected as usize;
    while left > 0 {
        let take = left.min(buf.len());
        file.read_exact(&mut buf[..take])?;
        vectors.extend(decode_f32(&buf[..take]));
        left -= take;
    }
    Ok(VecDir { meta, vectors, ids })
}

// ----------------------------------------------------------------- truth

/// SplitMix64: fixed, dependency-free, identical on every machine.
struct SplitMix(u64);
impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// `count` distinct rows out of `n`, partial Fisher-Yates on a seeded stream.
fn sample_rows(n: usize, count: usize, seed: u64) -> Vec<usize> {
    let count = count.min(n);
    let mut rng = SplitMix(seed);
    let mut pool: Vec<u32> = (0..n as u32).collect();
    for i in 0..count {
        let j = i + (rng.next() % (n - i) as u64) as usize;
        pool.swap(i, j);
    }
    pool[..count].iter().map(|&r| r as usize).collect()
}

/// Best-first list of at most `k` (score, row). Rows arrive in ascending
/// order, so on equal scores the lower row stays: results do not depend on threads.
struct TopK {
    k: usize,
    items: Vec<(f32, u32)>,
}
impl TopK {
    fn new(k: usize) -> Self {
        TopK {
            k,
            items: Vec::with_capacity(k + 1),
        }
    }
    fn push(&mut self, score: f32, row: u32) {
        if self.items.len() == self.k && score <= self.items[self.k - 1].0 {
            return;
        }
        let at = self.items.partition_point(|x| x.0 >= score);
        self.items.insert(at, (score, row));
        self.items.truncate(self.k);
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    for (x, y) in a.as_chunks::<8>().0.iter().zip(b.as_chunks::<8>().0.iter()) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum()
}

fn normalise_rows(vectors: &mut [f32], dims: usize) {
    for row in vectors.chunks_mut(dims) {
        let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            row.iter_mut().for_each(|x| *x /= norm);
        }
    }
}

const QUERY_BATCH: usize = 8;

/// (cosine, row) of one neighbour.
type Hit = (f32, u32);

fn truth(
    vec_dir: &Path,
    queries: usize,
    seed: u64,
    k: usize,
    threads: usize,
    out: &Path,
) -> Result<()> {
    let started = Instant::now();
    let mut data = load_vec_dir(vec_dir)?;
    let (n, dims) = (data.meta.n, data.meta.dims);
    normalise_rows(&mut data.vectors, dims);
    let rows = sample_rows(n, queries, seed);
    eprintln!(
        "truth start rows={n} queries={} k={k} threads={threads} load_secs={:.1}",
        rows.len(),
        started.elapsed().as_secs_f64()
    );
    let scan_started = Instant::now();
    let vectors = &data.vectors;
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = rows.len();
    let results: Mutex<Vec<Option<Vec<Hit>>>> = Mutex::new(vec![None; total]);
    std::thread::scope(|scope| {
        for _ in 0..threads.max(1) {
            scope.spawn(|| loop {
                let first = next.fetch_add(QUERY_BATCH, Ordering::Relaxed);
                if first >= total {
                    return;
                }
                let last = (first + QUERY_BATCH).min(total);
                let batch = &rows[first..last];
                let mut tops: Vec<TopK> = batch.iter().map(|_| TopK::new(k)).collect();
                for row in 0..n {
                    let r = &vectors[row * dims..(row + 1) * dims];
                    for (qi, &qrow) in batch.iter().enumerate() {
                        if row == qrow {
                            continue;
                        }
                        let q = &vectors[qrow * dims..(qrow + 1) * dims];
                        tops[qi].push(dot(r, q), row as u32);
                    }
                }
                if let Ok(mut slot) = results.lock() {
                    for (qi, top) in tops.into_iter().enumerate() {
                        slot[first + qi] = Some(top.items);
                    }
                }
                let before = done.fetch_add(last - first, Ordering::Relaxed);
                let after = before + (last - first);
                if after / 500 != before / 500 {
                    eprintln!(
                        "truth progress queries={after}/{total} secs={:.1}",
                        scan_started.elapsed().as_secs_f64()
                    );
                }
            });
        }
    });
    let results = results.into_inner().map_err(|e| anyhow!("lock: {e}"))?;
    let mut w = BufWriter::new(File::create(out)?);
    for (qrow, items) in rows.iter().zip(results) {
        let items = items.ok_or_else(|| anyhow!("query row {qrow} was not computed"))?;
        let ids: Vec<&String> = items.iter().map(|(_, r)| &data.ids[*r as usize]).collect();
        let scores: Vec<f32> = items.iter().map(|(s, _)| *s).collect();
        writeln!(
            w,
            "{}",
            json!({"qid": qrow, "truth": ids, "scores": scores})
        )?;
    }
    w.flush()?;
    println!(
        "queries={} k={k} secs={:.1}",
        total,
        scan_started.elapsed().as_secs_f64()
    );
    Ok(())
}

// ----------------------------------------------------------------- build

fn build_cmd(vec_dir: &Path, out: &Path, generation: u64, threads: usize) -> Result<()> {
    let data = load_vec_dir(vec_dir)?;
    eprintln!("build start rows={} threads={threads}", data.meta.n);
    let started = Instant::now();
    let m = build(
        &data.vectors,
        data.meta.dims,
        &data.ids,
        out,
        generation,
        data.meta.seq,
        threads,
    )?;
    let bytes = std::fs::metadata(usearch_path(out, &m.basename))?.len()
        + std::fs::metadata(ids_path(out, &m.basename))?.len();
    println!(
        "count={} secs={:.1} bytes={bytes}",
        m.count,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

// -------------------------------------------------------------- activate

fn list_dir(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        names.push(format!(
            "{} ({} bytes)",
            entry.file_name().to_string_lossy(),
            entry.metadata().map(|m| m.len()).unwrap_or(0)
        ));
    }
    names.sort();
    Ok(names)
}

fn activate(db_path: &Path, gen_dir: &Path, generation: u64) -> Result<()> {
    let base = super::basename(generation);
    let manifest: GenManifest = serde_json::from_str(
        &std::fs::read_to_string(manifest_path(gen_dir, &base))
            .with_context(|| format!("read manifest in {}", gen_dir.display()))?,
    )?;
    let index_dir = index_dir_of(db_path);
    std::fs::create_dir_all(&index_dir)?;
    println!("index dir before: {}", index_dir.display());
    for line in list_dir(&index_dir)? {
        println!("  {line}");
    }
    for path in [
        usearch_path(gen_dir, &base),
        ids_path(gen_dir, &base),
        manifest_path(gen_dir, &base),
    ] {
        let name = path.file_name().ok_or_else(|| anyhow!("no file name"))?;
        let target = index_dir.join(name);
        let mut tmp_name = name.to_os_string();
        tmp_name.push(".tmp");
        let tmp = index_dir.join(tmp_name);
        std::fs::copy(&path, &tmp)?;
        File::open(&tmp)?.sync_all()?;
        std::fs::rename(&tmp, &target)?;
    }
    // Verify what landed, not what was copied from.
    let opened = Generation::open(&index_dir, &manifest)
        .context("activated files fail the blake3 check; watermark not written")?;
    let storage = Storage::open(db_path)?;
    write_watermark(&storage, &manifest)?;
    println!(
        "activated generation={} count={} seq={} blake3_ok=true",
        manifest.generation,
        opened.len(),
        manifest.seq
    );
    println!("index dir after:");
    for line in list_dir(&index_dir)? {
        println!("  {line}");
    }
    Ok(())
}

// ---------------------------------------------------------------- recall

#[derive(Deserialize)]
struct TruthLine {
    qid: usize,
    truth: Vec<String>,
    #[serde(default)]
    scores: Vec<f32>,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let d = dot(a, b);
    let na = dot(a, a).sqrt();
    let nb = dot(b, b).sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        d / (na * nb)
    }
}

fn recall(
    db_path: &Path,
    truth_path: &Path,
    vec_dir: &Path,
    side: Side,
    k: usize,
    out: &Path,
) -> Result<()> {
    let data = load_vec_dir(vec_dir)?;
    let dims = data.meta.dims;
    let truths: Vec<TruthLine> = BufReader::new(File::open(truth_path)?)
        .lines()
        .map(|l| Ok(serde_json::from_str(&l?)?))
        .collect::<Result<_>>()?;
    let row_of: HashMap<&str, usize> = data
        .ids
        .iter()
        .enumerate()
        .map(|(i, id)| (id.as_str(), i))
        .collect();
    let index_dir = index_dir_of(db_path);
    let storage = Storage::open_read_only(db_path)?;

    // Each side answers "top k+1 ids" for a query vector and says if it holds an id.
    type Searcher = Box<dyn Fn(&[f32]) -> Vec<String>>;
    type Holds = Box<dyn Fn(&str) -> bool>;
    let (search, holds): (Searcher, Holds) = match side {
        Side::Gen => {
            let manifest = read_watermark(&storage)?.ok_or_else(|| {
                anyhow!(
                    "no {} watermark: run `vindex activate` first",
                    super::WATERMARK_KEY
                )
            })?;
            let generation = Arc::new(Generation::open(&index_dir, &manifest)?);
            let present: Arc<HashSet<String>> =
                Arc::new(generation.ids().iter().cloned().collect());
            let g = generation.clone();
            (
                Box::new(move |q| g.search(q, k + 1).into_iter().map(|(id, _)| id).collect()),
                Box::new(move |id| present.contains(id)),
            )
        }
        Side::Hnsw => {
            let engine = SearchEngine::load_from_disk(
                &index_dir,
                storage.count_chunk_embeddings()?,
                storage.count_reflection_embeddings()?,
            )?
            .ok_or_else(|| {
                anyhow!(
                    "hnsw cache in {} is missing or refused",
                    index_dir.display()
                )
            })?;
            let engine = Arc::new(engine);
            let e = engine.clone();
            let e2 = engine.clone();
            (
                Box::new(move |q| {
                    e.search_chunks(q, k + 1, 0.0)
                        .into_iter()
                        .map(|r| r.id)
                        .collect()
                }),
                Box::new(move |id| e2.has_chunk(id)),
            )
        }
    };

    let (mut hits, mut tie_hits, mut total, mut covered, mut truth_total) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut micros = Vec::with_capacity(truths.len());
    for t in &truths {
        let qvec = &data.vectors[t.qid * dims..(t.qid + 1) * dims];
        let self_id = &data.ids[t.qid];
        let started = Instant::now();
        let found = search(qvec);
        micros.push(started.elapsed().as_secs_f64() * 1e6);
        let truth_set: HashSet<&str> = t.truth.iter().map(String::as_str).collect();
        // Cosine of the k-th truth neighbour: an equal-scoring duplicate is as right as the listed id.
        let floor = t.scores.last().copied();
        truth_total += t.truth.len();
        covered += t.truth.iter().filter(|id| holds(id.as_str())).count();
        for id in found.iter().filter(|id| *id != self_id).take(k) {
            total += 1;
            if truth_set.contains(id.as_str()) {
                hits += 1;
                tie_hits += 1;
            } else if let (Some(floor), Some(&row)) = (floor, row_of.get(id.as_str())) {
                // Truth scores are cosine over normalised rows.
                let c = cosine(qvec, &data.vectors[row * dims..(row + 1) * dims]);
                if c >= floor - 1e-5 {
                    tie_hits += 1;
                }
            }
        }
    }
    micros.sort_by(|a, b| a.total_cmp(b));
    let n = truths.len();
    let denom = (n * k).max(1) as f64;
    let report = json!({
        "side": match side { Side::Gen => "gen", Side::Hnsw => "hnsw" },
        "n": n,
        "k": k,
        "returned": total,
        "recall_at_10": hits as f64 / denom,
        "recall_tie_aware": tie_hits as f64 / denom,
        "truth_coverage": covered as f64 / truth_total.max(1) as f64,
        "recall_over_covered": hits as f64 / covered.max(1) as f64,
        "p50_us": round3(percentile(&micros, 50.0)),
        "p95_us": round3(percentile(&micros, 95.0)),
    });
    write_json(out, &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

// ------------------------------------------------------------ load-bench

fn median(sorted: &[f64]) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    }
}

fn load_bench(db_path: &Path, runs: usize, out: &Path) -> Result<()> {
    let index_dir = index_dir_of(db_path);
    let storage = Storage::open_read_only(db_path)?;
    let manifest = read_watermark(&storage)?
        .ok_or_else(|| anyhow!("no watermark: run `vindex activate` first"))?;
    let chunks = storage.count_chunk_embeddings()?;
    let reflections = storage.count_reflection_embeddings()?;
    let pid = std::process::id();
    let baseline_rss = rss_bytes(pid);
    let (mut gen_ms, mut gen_rss, mut hnsw_ms, mut hnsw_rss) = (vec![], vec![], vec![], vec![]);
    let mut delta_rows = 0usize;
    for run in 0..runs {
        let t = Instant::now();
        let generation = Generation::open(&index_dir, &manifest)?;
        let (up, del) = delta_since(&storage, manifest.seq)?;
        gen_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        gen_rss.push(rss_bytes(pid));
        delta_rows = up.len() + del.len();
        drop(generation);

        let t = Instant::now();
        let engine = SearchEngine::load_from_disk(&index_dir, chunks, reflections)?
            .ok_or_else(|| anyhow!("hnsw cache missing or refused"))?;
        hnsw_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        hnsw_rss.push(rss_bytes(pid));
        drop(engine);
        eprintln!(
            "load-bench run={run} gen_ms={:.1} hnsw_ms={:.1}",
            gen_ms[run], hnsw_ms[run]
        );
    }
    let summary = |ms: &[f64], rss: &[u64]| {
        let mut sorted = ms.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        json!({
            "runs_ms": ms.iter().map(|x| round3(*x)).collect::<Vec<_>>(),
            "median_ms": round3(median(&sorted)),
            "min_ms": round3(sorted.first().copied().unwrap_or(0.0)),
            "rss_after_bytes": rss,
        })
    };
    let report = json!({
        "runs": runs,
        "note": "page cache warm; alternating gen then hnsw; rss read while the loaded object is alive",
        "baseline_rss_bytes": baseline_rss,
        "generation_count": manifest.count,
        "delta_rows": delta_rows,
        "gen": summary(&gen_ms, &gen_rss),
        "hnsw": summary(&hnsw_ms, &hnsw_rss),
    });
    write_json(out, &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

// ------------------------------------------------------------- serve-rss

#[derive(Deserialize)]
struct ProbeFileLite {
    #[serde(default = "default_limit")]
    limit: usize,
    probes: Vec<ProbeLite>,
}
#[derive(Deserialize)]
struct ProbeLite {
    query: String,
}
#[derive(Deserialize)]
struct AuditLite {
    queries: Vec<String>,
}
fn default_limit() -> usize {
    5
}

/// The `min_score` the probe bench passes (src/eval/probe.rs MCP_MIN_SCORE).
const MCP_MIN_SCORE: f64 = 0.3;
const CALL_TIMEOUT: Duration = Duration::from_secs(180);

fn serve_rss(
    db_path: &Path,
    server_bin: &Path,
    probes: &Path,
    audit: &Path,
    audit_limit: usize,
    env: &[String],
    out: &Path,
) -> Result<()> {
    let probe_file: ProbeFileLite =
        serde_json::from_str(&std::fs::read_to_string(probes)?).context("parse probes json")?;
    let audit_file: AuditLite =
        serde_json::from_str(&std::fs::read_to_string(audit)?).context("parse audit json")?;
    let mut queries: Vec<String> = probe_file.probes.iter().map(|p| p.query.clone()).collect();
    queries.extend(audit_file.queries.iter().take(audit_limit).cloned());
    let limit = probe_file.limit;

    let version = Command::new(server_bin)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let mut cmd = Command::new(server_bin);
    cmd.arg("--db-path")
        .arg(db_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut env_pairs = Vec::new();
    for pair in env {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| anyhow!("--env wants KEY=VALUE, got {pair:?}"))?;
        cmd.env(k, v);
        env_pairs.push(json!({k: v}));
    }
    let spawned = Instant::now();
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {}", server_bin.display()))?;
    let pid = child.id();
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;

    let (tx, rx) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if tx.send(v).is_err() {
                    return;
                }
            }
        }
    });
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicU64::new(0));
    let poller = {
        let (stop, peak) = (stop.clone(), peak.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(rss_bytes(pid), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };

    let outcome = (|| -> Result<Value> {
        let mut send = |v: Value| -> Result<()> {
            writeln!(stdin, "{v}")?;
            stdin.flush()?;
            Ok(())
        };
        let await_id = |id: u64| -> Result<Value> {
            let deadline = Instant::now() + CALL_TIMEOUT;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let v = rx
                    .recv_timeout(left)
                    .map_err(|e| anyhow!("waiting for response {id}: {e}"))?;
                if v.get("id").and_then(Value::as_u64) == Some(id) && v.get("method").is_none() {
                    return Ok(v);
                }
            }
        };
        send(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"vindex-serve-rss","version":"1"}}}),
        )?;
        await_id(1)?;
        send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))?;

        let (mut ms, mut errors, mut cold_start_ms) = (Vec::new(), 0usize, 0.0f64);
        for (i, q) in queries.iter().enumerate() {
            let id = 100 + i as u64;
            let t = Instant::now();
            send(
                json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
                "name":"csr_reflect_on_past",
                "arguments":{"query":q,"limit":limit,"min_score":MCP_MIN_SCORE}}}),
            )?;
            let reply = await_id(id)?;
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            if i == 0 {
                cold_start_ms = spawned.elapsed().as_secs_f64() * 1000.0;
            }
            let failed = reply.get("error").is_some()
                || reply["result"]["isError"].as_bool().unwrap_or(false);
            if failed {
                errors += 1;
                eprintln!(
                    "call {i} failed: {}",
                    reply.to_string().chars().take(300).collect::<String>()
                );
            }
        }
        let rss_after_warm = rss_bytes(pid);
        let mut sorted = ms.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        Ok(json!({
            "calls": ms.len(),
            "errors": errors,
            "cold_start_ms": round3(cold_start_ms),
            "p50_ms": round3(percentile(&sorted, 50.0)),
            "p95_ms": round3(percentile(&sorted, 95.0)),
            "rss_after_warm_bytes": rss_after_warm,
        }))
    })();

    stop.store(true, Ordering::Relaxed);
    let _ = child.kill();
    let _ = child.wait();
    let _ = poller.join();
    let mut report = outcome?;
    report["peak_rss_bytes"] = json!(peak.load(Ordering::Relaxed));
    report["server_bin"] = json!(server_bin.display().to_string());
    report["server_version"] = json!(version);
    report["env"] = json!(env_pairs);
    report["db_path"] = json!(db_path.display().to_string());
    write_json(out, &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_deterministic_distinct_and_in_range() {
        let a = sample_rows(1000, 100, 20_261_026);
        assert_eq!(a, sample_rows(1000, 100, 20_261_026));
        assert_ne!(a, sample_rows(1000, 100, 1));
        let set: HashSet<_> = a.iter().collect();
        assert_eq!(set.len(), 100);
        assert!(a.iter().all(|&r| r < 1000));
        assert_eq!(sample_rows(5, 50, 3).len(), 5);
    }

    #[test]
    fn topk_keeps_best_first_and_lower_row_on_ties() {
        let mut t = TopK::new(3);
        for (s, r) in [(0.5, 1), (0.9, 2), (0.5, 3), (0.7, 4), (0.5, 5), (0.1, 6)] {
            t.push(s, r);
        }
        let rows: Vec<u32> = t.items.iter().map(|x| x.1).collect();
        assert_eq!(rows, [2, 4, 1]);
    }

    #[test]
    fn brute_force_truth_matches_naive_cosine() {
        let dims = 16;
        let n = 60;
        let mut rng = SplitMix(9);
        let mut v: Vec<f32> = (0..n * dims)
            .map(|_| (rng.next() % 2000) as f32 / 1000.0 - 1.0)
            .collect();
        normalise_rows(&mut v, dims);
        let q = 7;
        let mut top = TopK::new(5);
        for row in 0..n {
            if row != q {
                top.push(
                    dot(
                        &v[row * dims..(row + 1) * dims],
                        &v[q * dims..(q + 1) * dims],
                    ),
                    row as u32,
                );
            }
        }
        let mut naive: Vec<(f32, usize)> = (0..n)
            .filter(|&r| r != q)
            .map(|r| {
                (
                    dot(&v[r * dims..(r + 1) * dims], &v[q * dims..(q + 1) * dims]),
                    r,
                )
            })
            .collect();
        naive.sort_by(|a, b| b.0.total_cmp(&a.0));
        let want: Vec<u32> = naive.iter().take(5).map(|x| x.1 as u32).collect();
        let got: Vec<u32> = top.items.iter().map(|x| x.1).collect();
        assert_eq!(got, want);
    }
}
