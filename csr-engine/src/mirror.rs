//! Transcript mirror: every Claude Code transcript under the projects dir
//! (main sessions and subagent runs) kept as a scrubbed, zstd-compressed copy
//! in CSR's data folder, so `csr_transcript` still works after Claude Code
//! deletes the JSONL.
//!
//! Layout: `<data_dir>/mirror/transcripts/<path relative to projects_dir>.zst`,
//! directories 0700, files 0600. The mirror only ever grows by appending whole
//! zstd frames; the `transcript_mirror` state table says how many source bytes
//! the `.zst` covers, and every state update comes after the frame is fsynced,
//! so a crash leaves at most one extra frame that the next pass trims back.
//!
//! Each line goes through the same two scrubs import uses: secrets are
//! redacted ([`crate::redact`]), then `<private>` spans are stripped. Lines the
//! scrubs do not touch stay byte-identical. `CSR_NO_MIRROR=1` turns the mirror off.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use regex::Regex;

use crate::storage::transcript_mirror::{MirrorRow, MIRROR_ERRORS_KEY};
use crate::storage::Storage;

/// How much of the mirrored source's end the state hashes.
const TAIL_WINDOW: u64 = 4096;
/// One compressed frame covers at most this much source (more for one huge line).
const BATCH_MAX_BYTES: usize = 64 << 20;
/// Level 3 everywhere; long-distance matching only pays off on big inputs.
const ZSTD_LEVEL: i32 = 3;
const LDM_MIN_BYTES: usize = 1 << 20;
const WINDOW_LOG: u32 = 27;
/// Sealing repacks a file whose frames pile up: only worth LDM above this.
const SEAL_LDM_MIN_ZST_BYTES: u64 = 8 << 20;
const DEFAULT_SEAL_SECS: u64 = 3600;
/// How long a sync waits for another process holding the same transcript.
const LOCK_WAIT: Duration = Duration::from_secs(2);

static OPEN_PRIVATE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<private>").expect("private tag pattern compiles"));

static ERROR_LOGGED: AtomicBool = AtomicBool::new(false);

/// `CSR_NO_MIRROR=1` (or `true`) switches the mirror off.
pub fn mirror_disabled() -> bool {
    std::env::var("CSR_NO_MIRROR")
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
        .unwrap_or(false)
}

fn seal_after_from_env() -> Duration {
    let secs = std::env::var("CSR_MIRROR_SEAL_SECS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SEAL_SECS);
    Duration::from_secs(secs)
}

/// `<data_dir>/mirror`.
pub fn mirror_root(data_dir: &Path) -> PathBuf {
    data_dir.join("mirror")
}

/// Where the compressed transcripts live. The legacy one-time copy sits next
/// to it (`mirror/projects`, `mirror/dict`) and is never touched.
pub fn transcripts_dir(data_dir: &Path) -> PathBuf {
    mirror_root(data_dir).join("transcripts")
}

/// A streaming reader over one mirrored transcript. Never holds the whole
/// decoded file; a decode error ends the stream instead of repeating forever.
pub fn open_transcript(path: &Path) -> io::Result<impl BufRead> {
    let mut decoder = zstd::stream::read::Decoder::new(File::open(path)?)?;
    decoder.window_log_max(WINDOW_LOG)?;
    Ok(BufReader::with_capacity(
        1 << 16,
        StopAtFirstError {
            inner: decoder,
            failed: false,
        },
    ))
}

struct StopAtFirstError<R> {
    inner: R,
    failed: bool,
}

impl<R: Read> Read for StopAtFirstError<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Ok(0);
        }
        self.inner.read(buf).inspect_err(|_| self.failed = true)
    }
}

// ─── Line transform ───

/// One string from a transcript line: redact secrets, then strip private spans
/// (the same order import uses).
fn scrub_string(text: &str) -> Cow<'_, str> {
    let redacted = crate::redact::redact_secrets(text);
    if !OPEN_PRIVATE_TAG.is_match(&redacted) {
        return redacted;
    }
    Cow::Owned(crate::import::strip_private_tags(&redacted))
}

/// Rewrite the string values of one valid JSON line in place. Keys, numbers,
/// key order and whitespace are left exactly as written; only a string that
/// the scrubs change is re-encoded. `None` means nothing changed.
fn rewrite_json_strings(line: &str) -> Result<Option<String>> {
    let bytes = line.as_bytes();
    let mut out = String::new();
    let mut copied = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i;
        let mut end = i + 1;
        loop {
            match bytes.get(end) {
                Some(b'\\') => end += 2,
                Some(b'"') => break,
                Some(_) => end += 1,
                None => bail!("unterminated string"),
            }
        }
        i = end + 1;
        let mut after = i;
        while bytes.get(after).is_some_and(u8::is_ascii_whitespace) {
            after += 1;
        }
        if bytes.get(after) == Some(&b':') {
            continue; // an object key
        }
        let inner = &line[start + 1..end];
        let decoded: Cow<str> = if inner.contains('\\') {
            Cow::Owned(serde_json::from_str::<String>(&line[start..=end])?)
        } else {
            Cow::Borrowed(inner)
        };
        if let Cow::Owned(scrubbed) = scrub_string(&decoded) {
            out.push_str(&line[copied..start]);
            out.push_str(&serde_json::to_string(&scrubbed)?);
            copied = end + 1;
        }
    }
    if copied == 0 {
        return Ok(None);
    }
    out.push_str(&line[copied..]);
    Ok(Some(out))
}

/// Scrub one line (no trailing newline). Clean lines come back borrowed and
/// byte-identical. A line that is JSON has every string value scrubbed; one
/// that does not parse is scrubbed as plain text.
pub fn scrub_line(line: &[u8]) -> Cow<'_, [u8]> {
    let text = String::from_utf8_lossy(line);
    if !crate::redact::may_hold_secret(&text) && !OPEN_PRIVATE_TAG.is_match(&text) {
        return Cow::Borrowed(line);
    }
    let structured = serde_json::from_str::<serde::de::IgnoredAny>(&text)
        .is_ok()
        .then(|| rewrite_json_strings(&text).ok())
        .flatten();
    let rewritten = match structured {
        Some(done) => done,
        None => match scrub_string(&text) {
            Cow::Owned(scrubbed) => Some(scrubbed),
            Cow::Borrowed(_) => None,
        },
    };
    match rewritten {
        Some(scrubbed) => Cow::Owned(scrubbed.into_bytes()),
        None => Cow::Borrowed(line),
    }
}

/// Scrub a run of whole lines (each ending in `\n`, the last optionally not).
pub fn scrub_lines(batch: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(batch.len());
    for piece in batch.split_inclusive(|byte| *byte == b'\n') {
        match piece.split_last() {
            Some((b'\n', body)) => {
                out.extend_from_slice(&scrub_line(body));
                out.push(b'\n');
            }
            _ => out.extend_from_slice(&scrub_line(piece)),
        }
    }
    out
}

// ─── Compression ───

fn compress_frame(data: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
    encoder.include_checksum(true)?;
    encoder.set_pledged_src_size(Some(data.len() as u64))?;
    if data.len() >= LDM_MIN_BYTES {
        encoder.long_distance_matching(true)?;
        encoder.window_log(WINDOW_LOG)?;
    }
    encoder.write_all(data)?;
    encoder.finish()
}

/// Where each complete zstd frame of `path` ends (byte offsets), starting with
/// 0. Stops at the first torn frame.
fn frame_boundaries(path: &Path) -> io::Result<Vec<u64>> {
    let mut reader = BufReader::with_capacity(1 << 16, File::open(path)?);
    let mut ends = vec![0u64];
    while !reader.fill_buf()?.is_empty() {
        let mut decoder = zstd::stream::read::Decoder::with_buffer(&mut reader)?.single_frame();
        decoder.window_log_max(WINDOW_LOG)?;
        if io::copy(&mut decoder, &mut io::sink()).is_err() {
            break;
        }
        drop(decoder);
        ends.push(reader.stream_position()?);
    }
    Ok(ends)
}

// ─── Files ───

fn ensure_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(path)
}

fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

fn append_name(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Exclusive per-transcript lock: daemon, hooks and MCP servers all import.
struct FileLock(File);

impl FileLock {
    /// `Ok(None)` when another process still holds it after [`LOCK_WAIT`].
    fn acquire(path: &Path) -> io::Result<Option<Self>> {
        let file = private_file_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        let deadline = std::time::Instant::now() + LOCK_WAIT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(Some(Self(file))),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut buf)?;
    Ok(buf)
}

/// blake3 (hex) of the last up-to-[`TAIL_WINDOW`] source bytes before `end`.
fn tail_hash_at(file: &mut File, end: u64) -> io::Result<String> {
    let start = end.saturating_sub(TAIL_WINDOW);
    let tail = read_at(file, start, (end - start) as usize)?;
    Ok(blake3::hash(&tail).to_hex().to_string())
}

fn now_stamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn empty_row(rel_path: &str) -> MirrorRow {
    MirrorRow {
        rel_path: rel_path.to_string(),
        source_bytes: 0,
        tail_hash: blake3::hash(b"").to_hex().to_string(),
        zst_bytes: 0,
        frames: 0,
        sealed: false,
        updated_at: now_stamp(),
        source_gone_at: None,
    }
}

fn collect_transcripts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            collect_transcripts(&path, out);
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
}

// ─── The mirror ───

/// What one [`Mirror::sync_file`] call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// `CSR_NO_MIRROR=1`.
    Disabled,
    /// Not a transcript under the projects dir.
    NotMirrored,
    /// The source file is gone; the existing mirror stays.
    SourceGone,
    UpToDate,
    /// Another process held the transcript's lock; the next pass catches up.
    Busy,
    /// New source bytes were compressed and appended.
    Appended {
        source_bytes: u64,
    },
    /// The source shrank or changed under the mirror: rebuilt from scratch.
    Rewritten {
        source_bytes: u64,
    },
    /// An error was counted in `mirror_errors`.
    Failed,
}

/// Totals for one [`Mirror::sync_pass`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PassStats {
    pub files: usize,
    pub synced: usize,
    pub source_bytes: u64,
    pub sealed: usize,
    pub sources_gone: usize,
    pub errors: usize,
}

/// What `mirror purge` found (and, with `--yes`, removed).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PurgeReport {
    pub files: i64,
    pub zst_bytes: i64,
    pub deleted: bool,
}

pub struct Mirror<'a> {
    storage: &'a Storage,
    data_dir: PathBuf,
    projects_dir: PathBuf,
    /// Seeded from `CSR_NO_MIRROR`; public so tests need no process env.
    pub disabled: bool,
    /// A transcript untouched this long is sealed (and flushes a partial last
    /// line). Seeded from `CSR_MIRROR_SEAL_SECS`.
    pub seal_after: Duration,
}

impl<'a> Mirror<'a> {
    pub fn new(storage: &'a Storage, data_dir: &Path, projects_dir: &Path) -> Self {
        Self {
            storage,
            data_dir: data_dir.to_path_buf(),
            projects_dir: projects_dir.to_path_buf(),
            disabled: mirror_disabled(),
            seal_after: seal_after_from_env(),
        }
    }

    fn zst_path(&self, rel_path: &str) -> PathBuf {
        transcripts_dir(&self.data_dir).join(format!("{rel_path}.zst"))
    }

    /// `/`-joined path of `source` below the projects dir.
    fn rel_path(&self, source: &Path) -> Option<String> {
        let rel = source.strip_prefix(&self.projects_dir).ok()?;
        let mut parts = Vec::new();
        for part in rel.components() {
            match part {
                std::path::Component::Normal(name) => parts.push(name.to_str()?),
                _ => return None,
            }
        }
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// The mirror must never sit under the projects dir: import would find the
    /// copies and mirror its own mirror.
    fn check_layout(&self) -> Result<()> {
        let root = mirror_root(&self.data_dir);
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if root.starts_with(&self.projects_dir)
            || canonical(&self.data_dir)
                .join("mirror")
                .starts_with(canonical(&self.projects_dir))
        {
            bail!(
                "transcript mirror {} would sit under the projects dir {}",
                root.display(),
                self.projects_dir.display()
            );
        }
        Ok(())
    }

    /// Count an error and log the first one; never fails the caller.
    fn note_error(&self, error: &anyhow::Error) {
        let _ = self.storage.bump_meta_counter_by(MIRROR_ERRORS_KEY, 1);
        if ERROR_LOGGED.swap(true, Ordering::Relaxed) {
            tracing::debug!(%error, "transcript mirror error");
        } else {
            tracing::warn!(%error, "transcript mirror error (further ones count in mirror_errors)");
        }
    }

    /// Mirror the new bytes of one transcript. Never returns an error and
    /// never runs inside a DB transaction: a failure is counted and logged.
    pub fn sync_file(&self, source: &Path) -> SyncOutcome {
        match self.try_sync_file(source) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.note_error(&error);
                SyncOutcome::Failed
            }
        }
    }

    fn try_sync_file(&self, source: &Path) -> Result<SyncOutcome> {
        if self.disabled {
            return Ok(SyncOutcome::Disabled);
        }
        let Some(rel) = self.rel_path(source) else {
            return Ok(SyncOutcome::NotMirrored);
        };
        let size = match fs::metadata(source) {
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SyncOutcome::SourceGone)
            }
            Err(error) => return Err(error.into()),
        };
        // The common call: nothing new since the last pass. One stat, one lookup.
        match self.storage.mirror_row(&rel)? {
            Some(row) if row.source_bytes == size && row.source_gone_at.is_none() => {
                return Ok(SyncOutcome::UpToDate)
            }
            None if size == 0 => return Ok(SyncOutcome::UpToDate),
            _ => {}
        }
        self.check_layout()?;
        let zst = self.zst_path(&rel);
        let dir = zst
            .parent()
            .ok_or_else(|| anyhow!("mirror path has no parent"))?;
        ensure_private_dir(dir)?;
        self.harden_root()?;
        let Some(_lock) = FileLock::acquire(&append_name(&zst, ".lock"))? else {
            return Ok(SyncOutcome::Busy);
        };

        // Everything below sees a stable mirror file and state row.
        let meta = match fs::metadata(source) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SyncOutcome::SourceGone)
            }
            Err(error) => return Err(error.into()),
        };
        let size = meta.len();
        let idle = meta
            .modified()
            .ok()
            .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
            .is_some_and(|age| age >= self.seal_after);
        let mut source_file = File::open(source)?;

        let mut row = self.recover(&rel, &zst)?;
        let mut rewrote = false;
        if let Some(existing) = &row {
            let intact = size >= existing.source_bytes
                && tail_hash_at(&mut source_file, existing.source_bytes)? == existing.tail_hash;
            if !intact {
                self.reset(&rel, &zst)?;
                row = None;
                rewrote = true;
            }
        }
        let mut row = row.unwrap_or_else(|| empty_row(&rel));
        let before = row.source_bytes;

        let mut buf: Vec<u8> = Vec::new();
        while row.source_bytes < size {
            let offset = row.source_bytes;
            // Read a batch; a line longer than the batch keeps extending it.
            buf.clear();
            let mut cut = None;
            while cut.is_none() && offset + (buf.len() as u64) < size {
                let want = BATCH_MAX_BYTES.min((size - offset) as usize - buf.len());
                let from = buf.len();
                buf.resize(from + want, 0);
                source_file.seek(SeekFrom::Start(offset + from as u64))?;
                source_file.read_exact(&mut buf[from..])?;
                cut = buf.iter().rposition(|byte| *byte == b'\n').map(|at| at + 1);
            }
            let cut = match cut {
                Some(cut) => cut,
                // A last line with no newline waits, unless nothing has written
                // to the file for a while: then it is all there will be.
                None if idle && !buf.is_empty() => buf.len(),
                None => break,
            };
            let frame = compress_frame(&scrub_lines(&buf[..cut]))?;

            let mut zst_file = private_file_options()
                .create(true)
                .append(true)
                .open(&zst)?;
            let created = row.zst_bytes == 0;
            if zst_file.metadata()?.len() != row.zst_bytes {
                zst_file.set_len(row.zst_bytes)?;
            }
            zst_file.write_all(&frame)?;
            zst_file.sync_all()?;
            if created {
                sync_dir(dir);
            }

            row.source_bytes = offset + cut as u64;
            row.tail_hash = tail_hash_at(&mut source_file, row.source_bytes)?;
            row.zst_bytes += frame.len() as u64;
            row.frames += 1;
            row.sealed = false;
            row.updated_at = now_stamp();
            row.source_gone_at = None;
            self.storage.mirror_upsert(&row)?;
        }
        if row.source_gone_at.is_some() {
            row.source_gone_at = None;
            self.storage.mirror_upsert(&row)?;
        }
        let added = row.source_bytes - before;
        Ok(if rewrote {
            SyncOutcome::Rewritten {
                source_bytes: row.source_bytes,
            }
        } else if added > 0 {
            SyncOutcome::Appended {
                source_bytes: added,
            }
        } else {
            SyncOutcome::UpToDate
        })
    }

    /// The mirror root is ours: keep it private even when an older install
    /// created it looser.
    fn harden_root(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let root = mirror_root(&self.data_dir);
            let mode = fs::metadata(&root)?.permissions().mode() & 0o777;
            if mode != 0o700 {
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
            }
        }
        Ok(())
    }

    /// Bring the `.zst` and its state row back in step after a crash. Returns
    /// the row to continue from, or `None` for "start this transcript over".
    /// Caller holds the transcript's lock.
    fn recover(&self, rel: &str, zst: &Path) -> Result<Option<MirrorRow>> {
        let row = self.storage.mirror_row(rel)?;
        self.settle_seal(zst, row.as_ref())?;
        let on_disk = match fs::metadata(zst) {
            Ok(meta) => Some(meta.len()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let (row, len) = match (row, on_disk) {
            (None, None) => return Ok(None),
            (None, Some(_)) => {
                // A first append that died before its state row: start over.
                fs::remove_file(zst)?;
                return Ok(None);
            }
            (Some(row), None) if row.zst_bytes == 0 => return Ok(Some(row)),
            (Some(_), None) => {
                self.reset(rel, zst)?;
                return Ok(None);
            }
            (Some(row), Some(len)) => (row, len),
        };
        if len == row.zst_bytes {
            return Ok(Some(row));
        }
        // An append landed and its state update did not: the file is longer than
        // the state says. The recorded length must be a frame boundary; cut the
        // stray frame (or torn half-frame) off there. Anything else means the
        // mirror file lost bytes, so the transcript is rebuilt.
        if len > row.zst_bytes && frame_boundaries(zst)?.contains(&row.zst_bytes) {
            OpenOptions::new()
                .write(true)
                .open(zst)?
                .set_len(row.zst_bytes)?;
            return Ok(Some(row));
        }
        self.reset(rel, zst)?;
        Ok(None)
    }

    /// A seal records its new state before the rename, so a crash in between
    /// leaves a finished `.part` that the state already describes. Finish the
    /// rename if the `.part` is whole; otherwise it is a stale half-write.
    fn settle_seal(&self, zst: &Path, row: Option<&MirrorRow>) -> Result<()> {
        let part = append_name(zst, ".part");
        let Ok(meta) = fs::metadata(&part) else {
            return Ok(());
        };
        let whole = row.is_some_and(|row| {
            row.frames == 1
                && row.sealed
                && meta.len() == row.zst_bytes
                && frame_boundaries(&part).is_ok_and(|ends| ends == [0, row.zst_bytes])
        });
        if whole {
            fs::rename(&part, zst)?;
            if let Some(dir) = zst.parent() {
                sync_dir(dir);
            }
        } else {
            fs::remove_file(&part)?;
        }
        Ok(())
    }

    /// Forget what was mirrored of one transcript. State first: a crash after
    /// it leaves a longer file that `recover` trims to zero.
    fn reset(&self, rel: &str, zst: &Path) -> Result<()> {
        self.storage.mirror_upsert(&empty_row(rel))?;
        match OpenOptions::new().write(true).open(zst) {
            Ok(file) => {
                file.set_len(0)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Repack a many-frame transcript into one frame. Decodes and re-encodes
    /// as a stream, so the largest transcript never sits in memory. Order:
    /// finished `.part` fsynced, state updated, rename (see `settle_seal`).
    pub fn seal_file(&self, rel: &str) -> Result<bool> {
        if self.disabled {
            return Ok(false);
        }
        let zst = self.zst_path(rel);
        let Some(_lock) = FileLock::acquire(&append_name(&zst, ".lock"))? else {
            return Ok(false);
        };
        let Some(row) = self.recover(rel, &zst)? else {
            return Ok(false);
        };
        if row.frames <= 1 {
            return Ok(false);
        }
        let part = self.write_sealed_part(&zst, &row)?;
        self.storage.mirror_upsert(&MirrorRow {
            zst_bytes: fs::metadata(&part)?.len(),
            frames: 1,
            sealed: true,
            updated_at: now_stamp(),
            ..row
        })?;
        fs::rename(&part, &zst)?;
        if let Some(dir) = zst.parent() {
            sync_dir(dir);
        }
        Ok(true)
    }

    /// Stream every frame of `zst` into a single-frame `.part`, fsynced.
    fn write_sealed_part(&self, zst: &Path, row: &MirrorRow) -> Result<PathBuf> {
        let part = append_name(zst, ".part");
        let mut decoder = zstd::stream::read::Decoder::new(File::open(zst)?)?;
        decoder.window_log_max(WINDOW_LOG)?;
        let out = private_file_options()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&part)?;
        let mut encoder =
            zstd::stream::write::Encoder::new(BufWriter::with_capacity(1 << 20, out), ZSTD_LEVEL)?;
        encoder.include_checksum(true)?;
        if row.zst_bytes >= SEAL_LDM_MIN_ZST_BYTES {
            encoder.long_distance_matching(true)?;
            encoder.window_log(WINDOW_LOG)?;
        }
        io::copy(&mut decoder, &mut encoder)?;
        let out = encoder
            .finish()?
            .into_inner()
            .map_err(|error| error.into_error())?;
        out.sync_all()?;
        Ok(part)
    }

    /// Walk every transcript under the projects dir, sync what changed, seal
    /// what has gone quiet, and mark mirrors whose source is gone. This is the
    /// first-run backfill too. State is loaded once.
    pub fn sync_pass(&self, shutdown: Option<&AtomicBool>) -> PassStats {
        let mut stats = PassStats::default();
        if self.disabled {
            return stats;
        }
        let errors_before = self.error_count();
        let stopping = || shutdown.is_some_and(|flag| flag.load(Ordering::SeqCst));
        let rows = match self.storage.mirror_rows() {
            Ok(rows) => rows,
            Err(error) => {
                self.note_error(&error);
                stats.errors = 1;
                return stats;
            }
        };
        let mut sources = Vec::new();
        collect_transcripts(&self.projects_dir, &mut sources);
        let mut seen: HashSet<String> = HashSet::with_capacity(sources.len());
        for source in &sources {
            if stopping() {
                return self.finish_pass(stats, errors_before);
            }
            let Some(rel) = self.rel_path(source) else {
                continue;
            };
            let Ok(meta) = fs::metadata(source) else {
                continue;
            };
            stats.files += 1;
            let row = rows.get(&rel);
            let due = match row {
                Some(row) => row.source_bytes != meta.len() || row.source_gone_at.is_some(),
                None => meta.len() > 0,
            };
            let mut frames = row.map_or(0, |row| row.frames);
            if due {
                match self.sync_file(source) {
                    SyncOutcome::Appended { source_bytes }
                    | SyncOutcome::Rewritten { source_bytes } => {
                        stats.synced += 1;
                        stats.source_bytes += source_bytes;
                        frames = self
                            .storage
                            .mirror_row(&rel)
                            .ok()
                            .flatten()
                            .map_or(0, |row| row.frames);
                    }
                    _ => {}
                }
            }
            seen.insert(rel.clone());
            let idle = meta
                .modified()
                .ok()
                .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
                .is_some_and(|age| age >= self.seal_after);
            if frames > 1 && idle {
                match self.seal_file(&rel) {
                    Ok(true) => stats.sealed += 1,
                    Ok(false) => {}
                    Err(error) => self.note_error(&error),
                }
            }
        }
        // A mirror whose source has vanished is kept; it only gets flagged. An
        // unreadable projects dir proves nothing about its files, so skip then.
        if self.projects_dir.is_dir() {
            for (rel, row) in rows
                .iter()
                .filter(|(rel, row)| row.source_gone_at.is_none() && !seen.contains(rel.as_str()))
            {
                if stopping() {
                    break;
                }
                if self.projects_dir.join(rel).exists() {
                    continue;
                }
                let flagged = MirrorRow {
                    source_gone_at: Some(now_stamp()),
                    ..row.clone()
                };
                match self.storage.mirror_upsert(&flagged) {
                    Ok(()) => stats.sources_gone += 1,
                    Err(error) => self.note_error(&error),
                }
                if row.frames > 1 {
                    match self.seal_file(rel) {
                        Ok(true) => stats.sealed += 1,
                        Ok(false) => {}
                        Err(error) => self.note_error(&error),
                    }
                }
            }
        }
        self.finish_pass(stats, errors_before)
    }

    fn finish_pass(&self, mut stats: PassStats, errors_before: i64) -> PassStats {
        stats.errors += (self.error_count() - errors_before).max(0) as usize;
        let _ = self.storage.mirror_record_sync(&now_stamp());
        stats
    }

    fn error_count(&self) -> i64 {
        self.storage
            .get_meta_counter(MIRROR_ERRORS_KEY)
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    /// `mirror purge`: report what the mirror holds; with `yes`, delete the
    /// compressed transcripts and the state rows. The legacy one-time copy
    /// beside them is not this module's to delete.
    pub fn purge(&self, yes: bool) -> Result<PurgeReport> {
        let totals = self.storage.mirror_totals()?;
        let mut report = PurgeReport {
            files: totals.files,
            zst_bytes: totals.zst_bytes,
            deleted: false,
        };
        if !yes {
            return Ok(report);
        }
        match fs::remove_dir_all(transcripts_dir(&self.data_dir)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("removing the mirror directory"),
        }
        self.storage.mirror_clear()?;
        report.deleted = true;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixture secrets are assembled at runtime; no credential-shaped literal
    // sits in the source tree.
    fn fake_github() -> String {
        format!("{}{}", "ghp_", "A".repeat(36))
    }

    fn fake_stripe() -> String {
        format!("{}{}", "sk_live_", "Z".repeat(24))
    }

    struct Rig {
        _dir: tempfile::TempDir,
        storage: Storage,
        data_dir: PathBuf,
        projects: PathBuf,
    }

    impl Rig {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let data_dir = dir.path().join("data");
            let projects = dir.path().join("projects");
            fs::create_dir_all(&data_dir).unwrap();
            fs::create_dir_all(projects.join("-tmp-proj")).unwrap();
            Self {
                storage: Storage::open_memory().unwrap(),
                data_dir,
                projects,
                _dir: dir,
            }
        }

        fn mirror(&self) -> Mirror<'_> {
            let mut mirror = Mirror::new(&self.storage, &self.data_dir, &self.projects);
            mirror.disabled = false;
            mirror.seal_after = Duration::from_secs(3600);
            mirror
        }

        fn source(&self, name: &str) -> PathBuf {
            self.projects.join("-tmp-proj").join(name)
        }

        fn zst(&self, name: &str) -> PathBuf {
            transcripts_dir(&self.data_dir).join(format!("-tmp-proj/{name}.zst"))
        }

        fn decoded(&self, name: &str) -> Vec<u8> {
            let mut out = Vec::new();
            open_transcript(&self.zst(name))
                .unwrap()
                .read_to_end(&mut out)
                .unwrap();
            out
        }
    }

    fn append(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    fn line(n: usize, text: &str) -> String {
        serde_json::json!({"type": "user", "uuid": format!("u-{n}"), "message": {"content": text}})
            .to_string()
            + "\n"
    }

    #[test]
    fn clean_lines_stay_byte_identical_and_dirty_lines_hold_markers() {
        let rig = Rig::new();
        let src = rig.source("s1.jsonl");
        let clean = line(1, "just a normal prompt, with the word token in it");
        let dirty = line(2, &format!("use {} please", fake_github()));
        let escaped = format!(
            "{{\"z\":1,\"a\":{{\"b\":\"line one\\nAWS_SECRET_ACCESS_KEY={}\\nline \\u00e9\"}}}}\n",
            "q".repeat(8) + "Zk3mN9xT"
        );
        fs::write(&src, format!("{clean}{dirty}{escaped}")).unwrap();

        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Appended { .. }
        ));
        let decoded = String::from_utf8(rig.decoded("s1.jsonl")).unwrap();
        let mut lines = decoded.split_inclusive('\n');
        assert_eq!(lines.next().unwrap(), clean);
        let second = lines.next().unwrap();
        assert!(!second.contains(&fake_github()));
        assert!(second.contains("[secret redacted: github_token]"));
        let third = lines.next().unwrap();
        // Key order, the number and the escaped accent survive the rewrite.
        assert!(third.starts_with("{\"z\":1,\"a\":{\"b\":\"line one\\nAWS_SECRET_ACCESS_KEY="));
        assert!(third.contains("[secret redacted: env_secret]"));
        assert!(third.contains('\u{e9}') || third.contains("\\u00e9"));
        assert!(serde_json::from_str::<serde_json::Value>(third).is_ok());
    }

    #[test]
    fn private_spans_are_removed_and_text_lines_are_scrubbed_as_text() {
        let rig = Rig::new();
        let src = rig.source("s2.jsonl");
        let secret = fake_stripe();
        fs::write(
            &src,
            format!(
                "{}not json at all {secret} <PRIVATE>hush</PRIVATE> tail\n",
                line(1, "open <private>the vault code 1234</private> and go")
            ),
        )
        .unwrap();
        rig.mirror().sync_file(&src);
        let decoded = String::from_utf8(rig.decoded("s2.jsonl")).unwrap();
        assert!(!decoded.contains("vault code"));
        assert!(!decoded.contains("hush"));
        assert!(!decoded.contains(&secret));
        assert!(decoded.contains("[private]"));
        assert!(decoded.contains("[secret redacted: stripe_key]"));
    }

    #[test]
    fn three_appends_decode_to_the_one_shot_result() {
        let rig = Rig::new();
        let one_shot = Rig::new();
        let parts: Vec<String> = (0..3)
            .map(|n| line(n, &format!("message {n} with {}", fake_github())))
            .collect();
        let src = rig.source("s3.jsonl");
        for part in &parts {
            append(&src, part.as_bytes());
            assert!(matches!(
                rig.mirror().sync_file(&src),
                SyncOutcome::Appended { .. }
            ));
        }
        let whole = one_shot.source("s3.jsonl");
        fs::write(&whole, parts.concat()).unwrap();
        one_shot.mirror().sync_file(&whole);

        let row = rig
            .storage
            .mirror_row("-tmp-proj/s3.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(row.frames, 3);
        assert_eq!(rig.decoded("s3.jsonl"), one_shot.decoded("s3.jsonl"));
        assert_eq!(rig.mirror().sync_file(&src), SyncOutcome::UpToDate);
    }

    #[test]
    fn a_partial_last_line_waits_for_the_next_pass() {
        let rig = Rig::new();
        let src = rig.source("s4.jsonl");
        let first = line(1, "complete");
        let second = line(2, "arrives in two writes");
        let (head, tail) = second.split_at(second.len() / 2);
        fs::write(&src, format!("{first}{head}")).unwrap();
        rig.mirror().sync_file(&src);
        assert_eq!(rig.decoded("s4.jsonl"), first.as_bytes());
        let row = rig
            .storage
            .mirror_row("-tmp-proj/s4.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(row.source_bytes, first.len() as u64);

        append(&src, tail.as_bytes());
        rig.mirror().sync_file(&src);
        assert_eq!(
            rig.decoded("s4.jsonl"),
            format!("{first}{second}").as_bytes()
        );
    }

    #[test]
    fn an_old_file_with_no_final_newline_is_flushed_whole() {
        let rig = Rig::new();
        let src = rig.source("s4b.jsonl");
        let text = format!("{}{}", line(1, "one"), line(2, "two").trim_end());
        fs::write(&src, &text).unwrap();
        let mut mirror = rig.mirror();
        mirror.seal_after = Duration::ZERO;
        mirror.sync_file(&src);
        assert_eq!(rig.decoded("s4b.jsonl"), text.as_bytes());
    }

    #[test]
    fn a_truncated_or_rewritten_source_is_mirrored_again_from_scratch() {
        let rig = Rig::new();
        let src = rig.source("s5.jsonl");
        fs::write(&src, format!("{}{}", line(1, "alpha"), line(2, "beta"))).unwrap();
        rig.mirror().sync_file(&src);

        // Shorter than what was mirrored.
        fs::write(&src, line(9, "short")).unwrap();
        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Rewritten { .. }
        ));
        assert_eq!(rig.decoded("s5.jsonl"), line(9, "short").as_bytes());

        // Same prefix length but different bytes: the tail hash catches it.
        let longer = format!(
            "{}{}",
            line(8, "changed"),
            line(7, "and longer than before")
        );
        fs::write(&src, &longer).unwrap();
        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Rewritten { .. }
        ));
        assert_eq!(rig.decoded("s5.jsonl"), longer.as_bytes());
        let row = rig
            .storage
            .mirror_row("-tmp-proj/s5.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(row.frames, 1);
    }

    #[test]
    fn a_crash_between_append_and_state_update_leaves_no_duplicate_lines() {
        let rig = Rig::new();
        let src = rig.source("s6.jsonl");
        let one = line(1, "first");
        let two = line(2, "second");
        fs::write(&src, &one).unwrap();
        rig.mirror().sync_file(&src);
        let saved = rig
            .storage
            .mirror_row("-tmp-proj/s6.jsonl")
            .unwrap()
            .unwrap();

        append(&src, two.as_bytes());
        rig.mirror().sync_file(&src);
        // Roll the state back to before the second append: the frame is on
        // disk, the row does not know.
        rig.storage.mirror_upsert(&saved).unwrap();
        assert!(fs::metadata(rig.zst("s6.jsonl")).unwrap().len() > saved.zst_bytes);

        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Appended { .. }
        ));
        assert_eq!(rig.decoded("s6.jsonl"), format!("{one}{two}").as_bytes());
        let row = rig
            .storage
            .mirror_row("-tmp-proj/s6.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(row.frames, 2);
        assert_eq!(
            row.zst_bytes,
            fs::metadata(rig.zst("s6.jsonl")).unwrap().len()
        );
    }

    #[test]
    fn a_torn_frame_after_the_recorded_end_is_trimmed() {
        let rig = Rig::new();
        let src = rig.source("s6b.jsonl");
        fs::write(&src, line(1, "first")).unwrap();
        rig.mirror().sync_file(&src);
        let before = fs::metadata(rig.zst("s6b.jsonl")).unwrap().len();
        let stray = compress_frame(b"{\"stray\":1}\n").unwrap();
        append(&rig.zst("s6b.jsonl"), &stray[..stray.len() - 3]);

        append(&src, line(2, "second").as_bytes());
        rig.mirror().sync_file(&src);
        let row = rig
            .storage
            .mirror_row("-tmp-proj/s6b.jsonl")
            .unwrap()
            .unwrap();
        assert!(row.zst_bytes > before);
        let decoded = String::from_utf8(rig.decoded("s6b.jsonl")).unwrap();
        assert_eq!(
            decoded,
            format!("{}{}", line(1, "first"), line(2, "second"))
        );
    }

    #[test]
    fn sealing_keeps_the_bytes_and_leaves_one_frame() {
        let rig = Rig::new();
        let src = rig.source("s7.jsonl");
        for n in 0..4 {
            append(&src, line(n, &format!("entry {n}")).as_bytes());
            rig.mirror().sync_file(&src);
        }
        let before = rig.decoded("s7.jsonl");
        let rel = "-tmp-proj/s7.jsonl";
        assert_eq!(rig.storage.mirror_row(rel).unwrap().unwrap().frames, 4);

        assert!(rig.mirror().seal_file(rel).unwrap());
        assert_eq!(rig.decoded("s7.jsonl"), before);
        let row = rig.storage.mirror_row(rel).unwrap().unwrap();
        assert_eq!((row.frames, row.sealed), (1, true));
        assert_eq!(
            row.zst_bytes,
            fs::metadata(rig.zst("s7.jsonl")).unwrap().len()
        );
        assert!(!append_name(&rig.zst("s7.jsonl"), ".part").exists());
        assert!(!rig.mirror().seal_file(rel).unwrap());

        // A crash between the state update and the rename is healed too: the
        // finished .part is moved into place by the next sync.
        let mirror = rig.mirror();
        append(&src, line(4, "before the second seal").as_bytes());
        mirror.sync_file(&src);
        let row = rig.storage.mirror_row(rel).unwrap().unwrap();
        assert_eq!(row.frames, 2);
        let part = mirror
            .write_sealed_part(&rig.zst("s7.jsonl"), &row)
            .unwrap();
        rig.storage
            .mirror_upsert(&MirrorRow {
                zst_bytes: fs::metadata(&part).unwrap().len(),
                frames: 1,
                sealed: true,
                ..row
            })
            .unwrap();
        let expected = rig.decoded("s7.jsonl");
        append(&src, line(5, "after the seal").as_bytes());
        mirror.sync_file(&src);
        let mut expected = expected;
        expected.extend_from_slice(line(5, "after the seal").as_bytes());
        assert_eq!(rig.decoded("s7.jsonl"), expected);
        assert!(!part.exists());
        let row = rig.storage.mirror_row(rel).unwrap().unwrap();
        assert_eq!(row.frames, 2);
    }

    #[test]
    fn the_output_is_plain_zstd_and_big_batches_use_a_long_window() {
        let rig = Rig::new();
        let src = rig.source("s8.jsonl");
        let big = "x".repeat(2 << 20);
        fs::write(&src, line(1, &big)).unwrap();
        rig.mirror().sync_file(&src);
        // The reference decoder (no window cap beyond zstd's own default of
        // 128 MiB) reads the file without any CSR-specific setup.
        let plain = zstd::stream::decode_all(File::open(rig.zst("s8.jsonl")).unwrap()).unwrap();
        assert_eq!(plain, line(1, &big).as_bytes());
        // And the zstd command-line tool, when it is installed.
        if let Ok(output) = std::process::Command::new("zstd")
            .arg("-dc")
            .arg(rig.zst("s8.jsonl"))
            .output()
        {
            assert!(output.status.success());
            assert_eq!(output.stdout, line(1, &big).as_bytes());
        }
    }

    #[test]
    fn directories_are_0700_and_files_0600() {
        use std::os::unix::fs::PermissionsExt;
        let rig = Rig::new();
        let src = rig.source("s9.jsonl");
        fs::write(&src, line(1, "x")).unwrap();
        fs::create_dir_all(mirror_root(&rig.data_dir)).unwrap();
        fs::set_permissions(
            mirror_root(&rig.data_dir),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        rig.mirror().sync_file(&src);
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&mirror_root(&rig.data_dir)), 0o700);
        assert_eq!(mode(&transcripts_dir(&rig.data_dir)), 0o700);
        assert_eq!(mode(rig.zst("s9.jsonl").parent().unwrap()), 0o700);
        assert_eq!(mode(&rig.zst("s9.jsonl")), 0o600);
    }

    #[test]
    fn the_kill_switch_writes_nothing() {
        let rig = Rig::new();
        let src = rig.source("s10.jsonl");
        fs::write(&src, line(1, "x")).unwrap();
        let mut mirror = rig.mirror();
        mirror.disabled = true;
        assert_eq!(mirror.sync_file(&src), SyncOutcome::Disabled);
        assert_eq!(mirror.sync_pass(None), PassStats::default());
        assert!(!mirror_root(&rig.data_dir).exists());
        assert_eq!(rig.storage.mirror_totals().unwrap().files, 0);
    }

    #[test]
    fn an_unwritable_mirror_counts_an_error_and_returns_normally() {
        let rig = Rig::new();
        let src = rig.source("s11.jsonl");
        fs::write(&src, line(1, "x")).unwrap();
        // A plain file where the mirror directory belongs.
        fs::write(mirror_root(&rig.data_dir), b"in the way").unwrap();
        assert_eq!(rig.mirror().sync_file(&src), SyncOutcome::Failed);
        assert_eq!(
            rig.storage
                .get_meta_counter(MIRROR_ERRORS_KEY)
                .unwrap()
                .unwrap(),
            1
        );
    }

    #[test]
    fn a_mirror_under_the_projects_dir_is_refused() {
        let rig = Rig::new();
        let src = rig.source("s12.jsonl");
        fs::write(&src, line(1, "x")).unwrap();
        // data_dir == projects_dir is what `Engine::from_parts` builds in tests.
        let mirror = Mirror::new(&rig.storage, &rig.projects, &rig.projects);
        assert_eq!(mirror.sync_file(&src), SyncOutcome::Failed);
        assert!(!rig.projects.join("mirror").exists());
    }

    #[test]
    fn a_pass_backfills_subagents_flags_gone_sources_and_keeps_their_mirror() {
        let rig = Rig::new();
        let main = rig.source("sess.jsonl");
        fs::write(&main, line(1, &format!("key {}", fake_github()))).unwrap();
        let sub_dir = rig.projects.join("-tmp-proj/sess/subagents");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(sub_dir.join("agent-a1.jsonl"), line(2, "sub")).unwrap();
        fs::write(rig.source("empty.jsonl"), b"").unwrap();

        let stats = rig.mirror().sync_pass(None);
        assert_eq!((stats.files, stats.synced, stats.errors), (3, 2, 0));
        assert!(rig
            .zst("sess/subagents/agent-a1.jsonl")
            .parent()
            .unwrap()
            .exists());
        let totals = rig.storage.mirror_totals().unwrap();
        assert_eq!((totals.files, totals.sources_gone), (2, 0));
        assert_eq!(rig.mirror().sync_pass(None).synced, 0);

        fs::remove_file(&main).unwrap();
        let stats = rig.mirror().sync_pass(None);
        assert_eq!(stats.sources_gone, 1);
        let row = rig
            .storage
            .mirror_row("-tmp-proj/sess.jsonl")
            .unwrap()
            .unwrap();
        assert!(row.source_gone_at.is_some());
        let decoded = String::from_utf8(rig.decoded("sess.jsonl")).unwrap();
        assert!(decoded.contains("[secret redacted: github_token]"));
        assert_eq!(rig.storage.mirror_totals().unwrap().sources_gone, 1);
        assert!(rig.storage.get_meta("mirror_last_sync").unwrap().is_some());
    }

    #[test]
    fn a_pass_seals_quiet_multi_frame_transcripts_only() {
        let rig = Rig::new();
        let quiet = rig.source("quiet.jsonl");
        let busy = rig.source("busy.jsonl");
        for n in 0..3 {
            append(&quiet, line(n, "q").as_bytes());
            append(&busy, line(n, "b").as_bytes());
            rig.mirror().sync_file(&quiet);
            rig.mirror().sync_file(&busy);
        }
        // Nothing is old enough yet.
        assert_eq!(rig.mirror().sync_pass(None).sealed, 0);
        let mut mirror = rig.mirror();
        mirror.seal_after = Duration::ZERO;
        assert_eq!(mirror.sync_pass(None).sealed, 2);
        assert_eq!(rig.storage.mirror_totals().unwrap().sealed, 2);
    }

    #[test]
    fn purge_dry_run_deletes_nothing_and_yes_deletes() {
        let rig = Rig::new();
        let src = rig.source("p.jsonl");
        fs::write(&src, line(1, "x")).unwrap();
        rig.mirror().sync_file(&src);

        let dry = rig.mirror().purge(false).unwrap();
        assert_eq!((dry.files, dry.deleted), (1, false));
        assert!(dry.zst_bytes > 0);
        assert!(rig.zst("p.jsonl").exists());
        assert_eq!(rig.storage.mirror_totals().unwrap().files, 1);

        // The legacy copy beside the new tree is left alone.
        let legacy = mirror_root(&rig.data_dir).join("projects/keep.jsonl");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, b"old").unwrap();
        assert!(rig.mirror().purge(true).unwrap().deleted);
        assert!(!transcripts_dir(&rig.data_dir).exists());
        assert!(legacy.exists());
        assert_eq!(rig.storage.mirror_totals().unwrap().files, 0);
    }

    #[test]
    fn a_decode_error_ends_the_stream_instead_of_looping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.zst");
        let mut frame = compress_frame(&b"{\"a\":1}\n".repeat(1000)).unwrap();
        let cut = frame.len() / 2;
        frame.truncate(cut);
        fs::write(&path, &frame).unwrap();
        let reader = open_transcript(&path).unwrap();
        assert!(reader.lines().take(100_000).count() < 100_000);
    }

    fn write_session(path: &Path, secret: &str) {
        let lines = [
            serde_json::json!({
                "type": "user", "uuid": "u-1", "timestamp": "2026-10-14T10:00:00Z",
                "message": {"content": format!("please deploy with {secret} and then report back")}
            }),
            serde_json::json!({
                "type": "assistant", "uuid": "a-1", "timestamp": "2026-10-14T10:00:05Z",
                "message": {"content": [{"type": "text", "text": "Deployed. The staging rollout finished cleanly."}]}
            }),
        ];
        let body: String = lines.iter().map(|l| l.to_string() + "\n").collect();
        fs::write(path, body).unwrap();
    }

    async fn engine_for(rig: &Rig) -> crate::engine::Engine {
        use crate::embeddings::EmbeddingEngine;
        use crate::search::SearchEngine;
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let storage = Arc::new(Storage::open(&rig.data_dir.join("csr-engine.db")).unwrap());
        crate::engine::Engine::from_parts_with_index_dir(
            storage,
            Arc::new(EmbeddingEngine::new().unwrap()),
            Arc::new(RwLock::new(SearchEngine::new(64))),
            rig.projects.clone(),
            rig.data_dir.join("index-v3"),
        )
    }

    #[tokio::test]
    async fn import_mirrors_the_file_and_csr_transcript_reads_it_after_the_source_is_deleted() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let src = rig.source("sess-aaa.jsonl");
        let secret = fake_github();
        write_session(&src, &secret);

        assert!(engine.import_file(&src, "proj").await.unwrap() > 0);
        let row = engine
            .storage()
            .mirror_row("-tmp-proj/sess-aaa.jsonl")
            .unwrap()
            .expect("import mirrored the transcript");
        assert_eq!(row.source_bytes, fs::metadata(&src).unwrap().len());

        // Already imported and mirrored: the second call is a cheap no-op.
        assert_eq!(engine.import_file(&src, "proj").await.unwrap(), 0);
        assert_eq!(engine.mirror().sync_file(&src), SyncOutcome::UpToDate);

        // Imported but never mirrored (as on the first run after upgrade): the
        // early "already imported" return must not skip the mirror.
        engine.storage().mirror_clear().unwrap();
        fs::remove_dir_all(transcripts_dir(&rig.data_dir)).unwrap();
        assert_eq!(engine.import_file(&src, "proj").await.unwrap(), 0);
        assert!(rig.zst("sess-aaa.jsonl").exists());

        // Claude Code deletes the JSONL.
        fs::remove_file(&src).unwrap();
        let request = |view| crate::transcript::TranscriptRequest {
            session: "sess-aaa".to_string(),
            view,
            ..Default::default()
        };
        let without =
            crate::transcript::run(&rig.projects, &request(crate::transcript::ViewKind::Stats));
        assert!(without.contains("not found"), "got: {without}");

        let stats = crate::transcript::run_with_mirror(
            &rig.projects,
            Some(&rig.data_dir),
            &request(crate::transcript::ViewKind::Stats),
        );
        assert!(stats.contains("transcript mirror"), "got: {stats}");
        assert!(stats.contains("<turns total=\"2\""), "got: {stats}");
        let prompts = crate::transcript::run_with_mirror(
            &rig.projects,
            Some(&rig.data_dir),
            &request(crate::transcript::ViewKind::Prompts),
        );
        assert!(
            prompts.contains("[secret redacted: github_token]"),
            "got: {prompts}"
        );
        assert!(!prompts.contains(&secret));
        assert!(prompts.contains("source: transcript mirror"));

        let pointer = crate::mcp::tools::get_full_conversation(
            &rig.projects,
            Some(&rig.data_dir),
            "sess-aaa",
            None,
        );
        assert!(pointer.contains("<mirror_path>"), "got: {pointer}");
        assert!(pointer.contains(&rig.zst("sess-aaa.jsonl").to_string_lossy().to_string()));
        assert!(pointer.contains("csr_transcript(session=\"sess-aaa\""));
        // Without a data dir the old not-found answer is unchanged.
        assert!(
            crate::mcp::tools::get_full_conversation(&rig.projects, None, "sess-aaa", None)
                .contains("not found")
        );
    }

    #[tokio::test]
    async fn an_unwritable_mirror_does_not_fail_the_import() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        fs::write(mirror_root(&rig.data_dir), b"in the way").unwrap();
        let src = rig.source("sess-bbb.jsonl");
        write_session(&src, &fake_stripe());

        assert!(engine.import_file(&src, "proj").await.unwrap() > 0);
        assert_eq!(
            engine
                .storage()
                .get_meta_counter(MIRROR_ERRORS_KEY)
                .unwrap()
                .unwrap(),
            1
        );
        assert!(engine
            .storage()
            .mirror_row("-tmp-proj/sess-bbb.jsonl")
            .unwrap()
            .is_none());
    }

    /// A generated JSONL corpus of about `target` bytes: mostly clean lines,
    /// about 1 percent with fake secrets, a few with `<private>` spans. Every
    /// `noisy_every`-th line (0 = none) also carries words the prefilter
    /// trips on without being secrets, which forces the JSON-parse path.
    fn bench_corpus(target: usize, noisy_every: usize) -> Vec<u8> {
        const WORDS: [&str; 24] = [
            "the", "function", "returns", "a", "result", "after", "reading", "config", "value",
            "from", "disk", "and", "token", "stream", "of", "bytes", "while", "handling", "errors",
            "in", "the", "parser", "module", "path",
        ];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let github = fake_github();
        let anthropic = format!("{}{}", "sk-ant-", "aB3dE5gH7jK9mN1pQ3rS5tU7".repeat(2));
        let mut corpus = Vec::with_capacity(target + (64 << 10));
        let mut n = 0usize;
        while corpus.len() < target {
            let words = 40 + (next() % 600) as usize;
            let mut text = String::new();
            for _ in 0..words {
                text.push_str(WORDS[(next() % WORDS.len() as u64) as usize]);
                text.push(' ');
            }
            match n % 1000 {
                0..=9 => text.push_str(&format!("key {github} and {anthropic} end")),
                500 | 501 => text.push_str("see <private>the vault code</private> here"),
                _ => {}
            }
            if noisy_every > 0 && n.is_multiple_of(noisy_every) {
                text.push_str("\nTOKEN_BUDGET=high and PASSWORD policy, see sk-learn docs");
            }
            corpus.extend_from_slice(line(n, &text).as_bytes());
            n += 1;
        }
        corpus
    }

    /// `cargo test --release --lib mirror::tests::scrub_throughput -- --ignored --nocapture --test-threads=1`
    ///
    /// Runs the mirror's line transform (the redact + private-span scrub) over
    /// a generated ~100 MB JSONL corpus, then the whole `sync_file` path over
    /// the same bytes, so the second number includes compression, hashing and
    /// fsync. The third run is pessimistic: 1 line in 10 trips the prefilter
    /// without holding a secret.
    #[test]
    #[ignore = "throughput measurement; run in release mode with --nocapture"]
    fn scrub_throughput() {
        let github = fake_github();
        for (label, noisy_every) in [("typical", 0), ("noisy 1-in-10", 10)] {
            let corpus = bench_corpus(100 << 20, noisy_every);
            let mib = corpus.len() as f64 / (1 << 20) as f64;
            let lines = corpus.iter().filter(|byte| **byte == b'\n').count();

            let started = std::time::Instant::now();
            let scrubbed = scrub_lines(&corpus);
            let secs = started.elapsed().as_secs_f64();
            assert!(!String::from_utf8_lossy(&scrubbed).contains(&github));
            println!(
                "[{label}] scrub only: {mib:.1} MiB, {lines} lines in {secs:.2}s = {:.0} MiB/s",
                mib / secs
            );

            let rig = Rig::new();
            let src = rig.source("big.jsonl");
            fs::write(&src, &corpus).unwrap();
            let started = std::time::Instant::now();
            let outcome = rig.mirror().sync_file(&src);
            let secs = started.elapsed().as_secs_f64();
            assert!(matches!(outcome, SyncOutcome::Appended { .. }));
            let row = rig
                .storage
                .mirror_row("-tmp-proj/big.jsonl")
                .unwrap()
                .unwrap();
            println!(
                "[{label}] full sync:  {mib:.1} MiB in {secs:.2}s = {:.0} MiB/s (zstd {:.1} MiB, {} frames)",
                mib / secs,
                row.zst_bytes as f64 / (1 << 20) as f64,
                row.frames
            );
        }
    }

    /// `cargo test --release --lib mirror::tests::per_file_overhead -- --ignored --nocapture`
    ///
    /// The backfill is also 20,000 small files: each costs a lock file, an
    /// fsynced frame, a directory fsync and a state row. Times a first pass
    /// over 2,000 files of about 20 KB, then an idle second pass.
    #[test]
    #[ignore = "throughput measurement; run in release mode with --nocapture"]
    fn per_file_overhead() {
        let rig = Rig::new();
        let corpus = bench_corpus(20 << 10, 0);
        for n in 0..2000 {
            let dir = rig.projects.join(format!("-tmp-proj-{}", n % 40));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("sess-{n}.jsonl")), &corpus).unwrap();
        }
        let started = std::time::Instant::now();
        let stats = rig.mirror().sync_pass(None);
        let secs = started.elapsed().as_secs_f64();
        assert_eq!((stats.files, stats.synced, stats.errors), (2000, 2000, 0));
        println!(
            "first pass: {} files, {:.1} MiB in {secs:.2}s = {:.0} files/s",
            stats.files,
            stats.source_bytes as f64 / (1 << 20) as f64,
            stats.files as f64 / secs
        );
        let started = std::time::Instant::now();
        let stats = rig.mirror().sync_pass(None);
        println!(
            "idle pass:  {} files in {:.3}s ({} synced)",
            stats.files,
            started.elapsed().as_secs_f64(),
            stats.synced
        );
    }
}
