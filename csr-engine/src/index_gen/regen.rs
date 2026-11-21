//! Keeping the exact-scan delta of a generation-served engine bounded.
//!
//! The daemon builds a new generation from the SQLite vectors in a child
//! `csr-engine vindex regen` process (macOS never returns a process's freed
//! pages, and the build peaks at about 2.6 GB), then follows the new watermark
//! (see `engine::follow_generation`). This module holds the pieces that do not
//! need a search engine: thresholds, the trigger predicate, the single-process
//! build, old-generation cleanup, the child spawn and the status file.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::{
    basename, build, ids_path, manifest_path, read_watermark, usearch_path, write_watermark,
    GenManifest, Generation, DIMS,
};
use crate::storage::Storage;

pub const DEFAULT_REGEN_ROWS: usize = 10_000;
pub const DEFAULT_MIN_SECS: u64 = 30 * 60;
/// First retry after a failed build; doubles per consecutive failure.
const BACKOFF_BASE_SECS: i64 = 60;
const BACKOFF_CAP_SECS: i64 = 30 * 60;
pub const STATE_FILE: &str = "regen-state.json";

/// `CSR_NO_VECTOR_GEN=1` is the off switch for everything generation related.
pub fn vector_gen_enabled() -> bool {
    !std::env::var("CSR_NO_VECTOR_GEN").is_ok_and(|v| v == "1")
}

/// Pending rows (changed since the watermark) that trigger a regeneration.
pub fn regen_rows() -> usize {
    std::env::var("CSR_GEN_REGEN_ROWS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_REGEN_ROWS)
}

/// Floor between two regenerations.
pub fn regen_min_secs() -> u64 {
    std::env::var("CSR_GEN_REGEN_MIN_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MIN_SECS)
}

/// Whether a build is due. The first generation (`has_gen == false`) is never
/// held back by the floor; a failed build holds everything back until its
/// backoff has passed.
pub fn should_regen(
    has_gen: bool,
    pending: usize,
    since_last: Option<Duration>,
    threshold: usize,
    floor: Duration,
    backoff_left: Option<Duration>,
) -> bool {
    if backoff_left.is_some_and(|left| !left.is_zero()) {
        return false;
    }
    if !has_gen {
        return true;
    }
    if pending < threshold {
        return false;
    }
    since_last.is_none_or(|elapsed| elapsed >= floor)
}

pub fn backoff_secs(failures: u32) -> i64 {
    if failures == 0 {
        return 0;
    }
    BACKOFF_BASE_SECS
        .saturating_mul(1i64 << (failures - 1).min(10))
        .min(BACKOFF_CAP_SECS)
}

// ---------------------------------------------------------------- state

/// What `csr-engine status` shows about regeneration; written by whoever runs
/// the tick (the daemon), read by anyone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegenState {
    pub generation: u64,
    /// Rows in the exact-scan delta of the serving engine.
    pub delta_rows: usize,
    /// Ids changed since the watermark (delta rows plus deletes).
    pub pending_rows: usize,
    pub threshold: usize,
    pub last_regen_unix: i64,
    pub last_error: Option<String>,
    pub failures: u32,
    pub next_attempt_unix: i64,
    pub updated_unix: i64,
    /// Last full blake3 check of the serving generation's files (maintenance).
    #[serde(default)]
    pub last_verify_unix: i64,
    /// The published generation failed to open or failed its full check. Kept
    /// across ticks and restarts until a build succeeds, so a repair that hits
    /// backoff is retried instead of forgotten.
    #[serde(default)]
    pub repair_pending: bool,
    /// When `generation` began serving (0: not yet seen). The previous
    /// generation is kept for a grace period from here (`retire::sweep`).
    #[serde(default)]
    pub serving_since_unix: i64,
}

pub fn read_state(index_dir: &Path) -> Option<RegenState> {
    serde_json::from_str(&std::fs::read_to_string(index_dir.join(STATE_FILE)).ok()?).ok()
}

pub fn write_state(index_dir: &Path, state: &RegenState) -> Result<()> {
    std::fs::create_dir_all(index_dir)?;
    let path = index_dir.join(STATE_FILE);
    let tmp = index_dir.join(format!("{STATE_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(state)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- build

/// Generation numbers that have a manifest in `index_dir`.
pub fn list_generations(index_dir: &Path) -> Vec<u64> {
    let mut found: Vec<u64> = std::fs::read_dir(index_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.strip_prefix("chunks-g")?
                .strip_suffix(".json")?
                .parse::<u64>()
                .ok()
        })
        .collect();
    found.sort_unstable();
    found
}

/// Generation numbers that have any chunk-generation file in `index_dir`
/// (manifest, data, ids, stamp, or a leftover `.tmp`). Pruning uses this, so a
/// removal that stopped after the manifest still gets its data files retried.
pub(crate) fn generations_with_files(index_dir: &Path) -> Vec<u64> {
    let mut found: Vec<u64> = std::fs::read_dir(index_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let rest = name.strip_prefix("chunks-g")?;
            let digits = rest.split('.').next()?;
            let kind = &rest[digits.len()..];
            let known = [".json", ".usearch", ".ids", ".verified"]
                .iter()
                .any(|ext| kind == *ext || kind == format!("{ext}.tmp"));
            known.then(|| digits.parse::<u64>().ok()).flatten()
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Remove every generation but `current` and the one before it. A reader that
/// still maps a removed file keeps a valid mapping (the inode lives until it
/// unmaps); a reader that read the previous watermark can still open it.
/// Callers hold the build lock, so no other build has files in flight.
pub fn prune_old_generations(index_dir: &Path, current: u64) -> Vec<u64> {
    let previous = list_generations(index_dir)
        .into_iter()
        .filter(|g| *g < current)
        .max();
    let mut removed = Vec::new();
    for generation in generations_with_files(index_dir) {
        if generation == current || Some(generation) == previous {
            continue;
        }
        remove_generation(index_dir, generation);
        removed.push(generation);
    }
    removed
}

/// Remove every generation older than `current`, the previous one included.
/// `retire::sweep` calls this once `current` has served, verified, for the
/// grace period; until then the previous generation is the rollback.
/// Callers hold the build lock, as for [`prune_old_generations`].
pub fn prune_generations_below(index_dir: &Path, current: u64) -> Vec<u64> {
    let mut removed = Vec::new();
    for generation in generations_with_files(index_dir) {
        if generation < current {
            remove_generation(index_dir, generation);
            removed.push(generation);
        }
    }
    removed
}

fn remove_generation(index_dir: &Path, generation: u64) {
    let base = basename(generation);
    // Manifest first: a half-removed generation must never look servable.
    for path in [
        manifest_path(index_dir, &base),
        usearch_path(index_dir, &base),
        ids_path(index_dir, &base),
        super::stamp_path(index_dir, &base),
    ] {
        let tmp = {
            let mut name = path.file_name().unwrap_or_default().to_os_string();
            name.push(".tmp");
            path.with_file_name(name)
        };
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(tmp);
    }
}

pub const BUILD_LOCK_FILE: &str = "regen.lock";

/// Exclusive lock for one generation build per index directory, across
/// processes (the daemon's child, a `rebuild-index` child, a test). Held from
/// picking the generation number through publication and pruning; released
/// when the file closes, including when the holder dies.
pub fn try_build_lock(index_dir: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    std::fs::create_dir_all(index_dir)?;
    let path = index_dir.join(BUILD_LOCK_FILE);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    if FileExt::try_lock_exclusive(&file).is_err() {
        bail!("another generation build holds {}", path.display());
    }
    Ok(file)
}

/// Build the next generation from `storage`'s vectors, verify it, publish the
/// watermark last and prune old generations. The seq is read in the same
/// snapshot as the vectors, so no row is ever older than the watermark claims.
pub fn build_next_generation(
    storage: &Storage,
    index_dir: &Path,
    threads: usize,
) -> Result<GenManifest> {
    let _lock = try_build_lock(index_dir)?;
    let previous = read_watermark(storage)?.map(|m| m.generation).unwrap_or(0);
    let on_disk = list_generations(index_dir).into_iter().max().unwrap_or(0);
    let generation = previous.max(on_disk) + 1;
    let (ids, vectors, seq, skipped) = storage.with_connection(|conn| {
        let tx = conn.unchecked_transaction()?;
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM embedding_changes",
            [],
            |r| r.get(0),
        )?;
        let n: i64 = tx.query_row("SELECT COUNT(*) FROM chunk_embeddings", [], |r| r.get(0))?;
        let mut ids: Vec<String> = Vec::with_capacity(n as usize);
        let mut vectors: Vec<f32> = Vec::with_capacity(n as usize * DIMS);
        let mut skipped = 0usize;
        let mut stmt = tx.prepare("SELECT chunk_id, embedding FROM chunk_embeddings")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            if blob.len() != DIMS * 4 || id.contains('\n') {
                skipped += 1;
                continue;
            }
            vectors.extend(super::decode_f32(&blob));
            ids.push(id);
        }
        Ok((ids, vectors, seq, skipped))
    })?;
    if ids.is_empty() {
        bail!("no chunk vectors to build a generation from");
    }
    if skipped > 0 {
        eprintln!("regen skipped {skipped} rows with a malformed vector or id");
    }
    let manifest = build(&vectors, DIMS, &ids, index_dir, generation, seq, threads)?;
    drop(vectors);
    drop(ids);
    // Verify what landed on disk before the watermark names it.
    Generation::open(index_dir, &manifest)
        .context("built generation fails the blake3 check; watermark not written")?;
    write_watermark(storage, &manifest)?;
    prune_old_generations(index_dir, manifest.generation);
    Ok(manifest)
}

/// Body of `csr-engine vindex regen` (the child process): build, print the
/// manifest as the only stdout line.
pub fn child_main(db_path: &Path, threads: usize) -> Result<()> {
    exit_when_parent_dies();
    let index_dir = db_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("index-v3");
    let storage = Storage::open(db_path)?;
    let manifest = build_next_generation(&storage, &index_dir, threads)?;
    println!("{}", serde_json::to_string(&manifest)?);
    Ok(())
}

/// The build child must not outlive the daemon or CLI that spawned it: a
/// parent that exits mid-build (shutdown timeout, SIGKILL, Ctrl+C) would leave
/// a ~2.6 GB builder publishing under a replacement daemon. A watcher thread
/// exits the child once it is reparented. Files in flight are `.tmp` names the
/// next build overwrites; the build lock is released with the process.
/// The launcher passes its own pid in [`PARENT_PID_ENV`]: a parent that died
/// before the child sampled `parent_id()` would otherwise be missed, because
/// the child would record its adoptive parent.
fn exit_when_parent_dies() {
    let parent = std::env::var(PARENT_PID_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or_else(std::os::unix::process::parent_id);
    let gone = move || {
        let now = std::os::unix::process::parent_id();
        if now != parent {
            eprintln!("regeneration child: parent {parent} exited (now {now}), stopping the build");
            std::process::exit(3);
        }
    };
    gone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(500));
        gone();
    });
}

/// Pid of the process that launched a build child (`nice` execs, so it is the
/// child's real parent).
pub const PARENT_PID_ENV: &str = "CSR_REGEN_PARENT_PID";

/// Threads the child builds with: a share of the machine, never all of it.
pub fn build_threads() -> usize {
    std::env::var("CSR_GEN_BUILD_THREADS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| (n.get() / 3).clamp(1, 6))
                .unwrap_or(2)
        })
}

/// `nice`, and on macOS under a utility QoS clamp: a child of a launchd
/// `ProcessType Interactive` job starts its threads at default class (31),
/// above the daemon's own utility threads (20), and `nice` does not lower a
/// thread that has a class. `taskpolicy -c utility` holds every thread at 20.
fn build_child_launcher() -> Command {
    const TASKPOLICY: &str = "/usr/sbin/taskpolicy";
    if cfg!(target_os = "macos") && Path::new(TASKPOLICY).exists() {
        let mut cmd = Command::new(TASKPOLICY);
        cmd.args(["-c", "utility", "nice"]);
        cmd
    } else {
        Command::new("nice")
    }
}

/// Test seam: while set, [`run_build_child`] calls this instead of spawning a
/// child (a test binary cannot run `vindex regen`).
#[cfg(test)]
pub(crate) type BuildChildHook = Box<dyn Fn(&Path) -> Result<GenManifest> + Send + Sync>;
#[cfg(test)]
pub(crate) static BUILD_CHILD_HOOK: std::sync::Mutex<Option<BuildChildHook>> =
    std::sync::Mutex::new(None);

/// Run the build in a niced child `csr-engine` and return its manifest. The
/// child's memory (about 2.6 GB at 1M vectors) goes back to the OS when it
/// exits; the daemon's own footprint does not move.
pub fn run_build_child(db_path: &Path) -> Result<GenManifest> {
    #[cfg(test)]
    {
        if let Some(hook) = BUILD_CHILD_HOOK.lock().unwrap().as_ref() {
            return hook(db_path);
        }
    }
    let exe: PathBuf = std::env::current_exe().context("current_exe")?;
    let mut cmd = build_child_launcher();
    cmd.args(["-n", "10"])
        .arg(exe)
        .arg("--db-path")
        .arg(db_path)
        .args(["vindex", "regen", "--threads"])
        .arg(build_threads().to_string())
        .env(PARENT_PID_ENV, std::process::id().to_string())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let output = cmd.output().context("spawn regeneration child")?;
    if !output.status.success() {
        bail!("regeneration child exited with {}", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .context("regeneration child printed no manifest")?;
    Ok(serde_json::from_str(line)?)
}

/// Upgrade and fresh-install path: the daemon calls this before it loads any
/// index. A machine with chunk vectors and no watermark gets its first
/// generation here, so the daemon never builds or holds the hnsw_rs chunk
/// graph. Failure is logged and the daemon starts on the old path; the tick
/// retries with backoff.
pub fn bootstrap_first_generation(db_path: &Path) -> Result<Option<GenManifest>> {
    if !vector_gen_enabled() {
        return Ok(None);
    }
    let storage = Storage::open(db_path)?;
    if read_watermark(&storage)?.is_some() || storage.count_chunk_embeddings()? == 0 {
        return Ok(None);
    }
    drop(storage);
    Ok(Some(run_build_child(db_path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_floor_and_backoff_decide_when_a_build_is_due() {
        let floor = Duration::from_secs(1800);
        let long = Some(Duration::from_secs(3600));
        // Below the threshold: no build, however old the generation.
        assert!(!should_regen(true, 9_999, long, 10_000, floor, None));
        // At the threshold and past the floor: build.
        assert!(should_regen(true, 10_000, long, 10_000, floor, None));
        // At the threshold inside the floor: wait.
        let soon = Some(Duration::from_secs(60));
        assert!(!should_regen(true, 50_000, soon, 10_000, floor, None));
        // No generation yet: build now, floor or not.
        assert!(should_regen(false, 0, soon, 10_000, floor, None));
        // A failed build holds everything back until its backoff passes.
        let left = Some(Duration::from_secs(5));
        assert!(!should_regen(true, 50_000, long, 10_000, floor, left));
        assert!(!should_regen(false, 0, None, 10_000, floor, left));
        assert_eq!(backoff_secs(0), 0);
        assert_eq!(backoff_secs(1), 60);
        assert_eq!(backoff_secs(3), 240);
        assert_eq!(backoff_secs(40), 1800);
    }

    #[test]
    fn prune_keeps_the_current_and_the_previous_generation() {
        let dir = tempfile::tempdir().unwrap();
        for g in 1..=4u64 {
            let base = basename(g);
            for path in [
                manifest_path(dir.path(), &base),
                usearch_path(dir.path(), &base),
                ids_path(dir.path(), &base),
            ] {
                std::fs::write(path, b"x").unwrap();
            }
        }
        assert_eq!(list_generations(dir.path()), vec![1, 2, 3, 4]);
        let removed = prune_old_generations(dir.path(), 4);
        assert_eq!(removed, vec![1, 2]);
        assert_eq!(list_generations(dir.path()), vec![3, 4]);
        assert!(!usearch_path(dir.path(), "chunks-g1").exists());
        assert!(usearch_path(dir.path(), "chunks-g3").exists());
    }

    #[test]
    fn prune_retries_data_files_whose_manifest_is_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        for g in 1..=3u64 {
            let base = basename(g);
            for path in [
                manifest_path(dir.path(), &base),
                usearch_path(dir.path(), &base),
                ids_path(dir.path(), &base),
            ] {
                std::fs::write(path, b"x").unwrap();
            }
        }
        // An earlier prune of g1 stopped after the manifest; g0 left a tmp.
        std::fs::remove_file(manifest_path(dir.path(), "chunks-g1")).unwrap();
        std::fs::write(dir.path().join("chunks-g0.usearch.tmp"), b"x").unwrap();
        // Other families and unrelated names stay.
        std::fs::write(dir.path().join("episodes-g1.usearch"), b"x").unwrap();
        std::fs::write(dir.path().join("chunks-g1.usearch.bak"), b"x").unwrap();
        let removed = prune_old_generations(dir.path(), 3);
        assert_eq!(removed, vec![0, 1]);
        assert!(!usearch_path(dir.path(), "chunks-g1").exists());
        assert!(!ids_path(dir.path(), "chunks-g1").exists());
        assert!(!dir.path().join("chunks-g0.usearch.tmp").exists());
        assert!(usearch_path(dir.path(), "chunks-g2").exists());
        assert!(usearch_path(dir.path(), "chunks-g3").exists());
        assert!(dir.path().join("episodes-g1.usearch").exists());
        assert!(dir.path().join("chunks-g1.usearch.bak").exists());
    }

    #[test]
    fn a_second_builder_is_refused_while_the_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let held = try_build_lock(dir.path()).unwrap();
        let err = try_build_lock(dir.path()).unwrap_err().to_string();
        assert!(err.contains("another generation build"), "{err}");
        drop(held);
        assert!(try_build_lock(dir.path()).is_ok());
    }

    #[test]
    fn state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_state(dir.path()).is_none());
        let state = RegenState {
            generation: 3,
            delta_rows: 12,
            last_error: Some("boom".into()),
            failures: 2,
            ..Default::default()
        };
        write_state(dir.path(), &state).unwrap();
        assert_eq!(read_state(dir.path()), Some(state));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_build_child_runs_under_a_utility_clamp_and_nice() {
        let cmd = build_child_launcher();
        assert_eq!(cmd.get_program(), "/usr/sbin/taskpolicy");
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, ["-c", "utility", "nice"]);
    }
}
