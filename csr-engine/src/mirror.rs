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

/// The import path leaves a transcript with more than this unmirrored to the
/// daemon pass: a backlog is minutes of compression, not a cheap side effect.
const OPPORTUNISTIC_MAX_PENDING: u64 = 8 << 20;
/// Mirror-wide lock, beside (not inside) the folder `purge` deletes.
const MIRROR_LOCK_NAME: &str = ".mirror.lock";

static OPEN_PRIVATE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<private>").expect("private tag pattern compiles"));

/// A JSON `\u00XX` escape that decodes to printable ASCII or a line break. A
/// pattern the scrubs look for can hide behind one (`\u0067hp_...`), so a line
/// holding one is decoded before the prefilter. Control characters and
/// non-ASCII escapes cannot spell a pattern; ANSI colour codes stay cheap.
static ASCII_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\\u00(?:[2-7][0-9a-fA-F]|0[aAdD])").expect("escape pattern compiles")
});

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

/// Why a mirrored transcript stopped decoding before its end, if it did.
pub type ReadFailure = std::sync::Arc<std::sync::Mutex<Option<String>>>;

/// A streaming reader over one mirrored transcript. Never holds the whole
/// decoded file; a decode error ends the stream instead of repeating forever.
pub fn open_transcript(path: &Path) -> io::Result<impl BufRead> {
    open_transcript_checked(path).map(|(reader, _)| reader)
}

/// [`open_transcript`], plus a slot that says why the stream ended early: a
/// torn or corrupt frame reads as a plain end of file otherwise, and the
/// caller would present half a transcript as the whole.
pub fn open_transcript_checked(path: &Path) -> io::Result<(impl BufRead, ReadFailure)> {
    let mut decoder = zstd::stream::read::Decoder::new(File::open(path)?)?;
    decoder.window_log_max(WINDOW_LOG)?;
    let failure = ReadFailure::default();
    let reader = BufReader::with_capacity(
        1 << 16,
        StopAtFirstError {
            inner: decoder,
            failed: false,
            failure: failure.clone(),
        },
    );
    Ok((reader, failure))
}

struct StopAtFirstError<R> {
    inner: R,
    failed: bool,
    failure: ReadFailure,
}

impl<R: Read> Read for StopAtFirstError<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Ok(0);
        }
        self.inner.read(buf).inspect_err(|error| {
            self.failed = true;
            if let Ok(mut slot) = self.failure.lock() {
                slot.get_or_insert_with(|| error.to_string());
            }
        })
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

/// Rewrite the strings of one valid JSON line in place, object keys included
/// (a token-to-account map keeps its credential in the key). Numbers, key
/// order and whitespace are left exactly as written; only a string that the
/// scrubs change is re-encoded. `None` means nothing changed.
fn rewrite_json_strings(line: &str) -> Result<Option<String>> {
    /// A string the scrubs changed, to be spliced over `line[start..=end]`.
    struct Edit {
        start: usize,
        end: usize,
        as_written: bool,
        scrubbed: String,
    }
    let bytes = line.as_bytes();
    let mut edits: Vec<Edit> = Vec::new();
    // Per object: the keys the scrubs left alone, and the edits that renamed one.
    let mut objects: Vec<(HashSet<Cow<str>>, Vec<usize>)> = Vec::new();
    let mut open: Vec<Option<usize>> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                objects.push((HashSet::new(), Vec::new()));
                open.push(Some(objects.len() - 1));
                i += 1;
                continue;
            }
            b'[' => {
                open.push(None);
                i += 1;
                continue;
            }
            b'}' | b']' => {
                open.pop();
                i += 1;
                continue;
            }
            b'"' => {}
            _ => {
                i += 1;
                continue;
            }
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
        let object = if bytes.get(after) == Some(&b':') {
            open.last().copied().flatten()
        } else {
            None
        };
        let inner = &line[start + 1..end];
        // A string that will not decode (a lone surrogate escape) is scrubbed
        // as written; the markers are JSON-safe, so it is spliced back as is.
        let mut as_written = false;
        let decoded: Cow<str> = if inner.contains('\\') {
            match serde_json::from_str::<String>(&line[start..=end]) {
                Ok(text) => Cow::Owned(text),
                Err(_) => {
                    as_written = true;
                    Cow::Borrowed(inner)
                }
            }
        } else {
            Cow::Borrowed(inner)
        };
        match scrub_string(&decoded) {
            Cow::Owned(scrubbed) => {
                if let Some(object) = object {
                    objects[object].1.push(edits.len());
                }
                edits.push(Edit {
                    start,
                    end,
                    as_written,
                    scrubbed,
                });
            }
            Cow::Borrowed(_) => {
                if let Some(object) = object {
                    objects[object].0.insert(decoded);
                }
            }
        }
    }
    if edits.is_empty() {
        return Ok(None);
    }
    // Two credentials redact to the same marker, and a duplicate key would drop
    // one value on the next parse. Every key the scrubs left alone is reserved
    // first, so a renamed key never lands on one of them, wherever it sits.
    for (reserved, renamed) in &mut objects {
        for &at in renamed.iter() {
            let base = edits[at].scrubbed.clone();
            let mut name = base.clone();
            let mut n = 1;
            while reserved.contains(name.as_str()) {
                n += 1;
                name = format!("{base} #{n}");
            }
            reserved.insert(Cow::Owned(name.clone()));
            edits[at].scrubbed = name;
        }
    }
    let mut out = String::new();
    let mut copied = 0usize;
    for edit in &edits {
        out.push_str(&line[copied..edit.start]);
        if edit.as_written {
            out.push('"');
            out.push_str(&edit.scrubbed);
            out.push('"');
        } else {
            out.push_str(&serde_json::to_string(&edit.scrubbed)?);
        }
        copied = edit.end + 1;
    }
    out.push_str(&line[copied..]);
    Ok(Some(out))
}

/// Scrub one line (no trailing newline). Clean lines come back borrowed and
/// byte-identical. A line that is JSON has every string value scrubbed; one
/// that does not parse is scrubbed as plain text.
pub fn scrub_line(line: &[u8]) -> Cow<'_, [u8]> {
    let text = String::from_utf8_lossy(line);
    if !ASCII_ESCAPE.is_match(&text)
        && !crate::redact::may_hold_secret(&text)
        && !OPEN_PRIVATE_TAG.is_match(&text)
    {
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

/// A cross-process lock on a sidecar file. Per transcript it is exclusive
/// (daemon, hooks and MCP servers all import); mirror-wide it is shared by
/// every sync and seal and exclusive for `purge`.
struct FileLock(File);

impl FileLock {
    /// `Ok(None)` when another process still holds it after `wait`
    /// (`Duration::ZERO` tries once).
    fn acquire(path: &Path, exclusive: bool, wait: Duration) -> io::Result<Option<Self>> {
        let file = private_file_options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            let locked = if exclusive {
                FileExt::try_lock_exclusive(&file)
            } else {
                FileExt::try_lock_shared(&file)
            };
            match locked {
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
    /// Another process held the transcript's lock (or `purge` held the whole
    /// mirror); the next pass catches up.
    Busy,
    /// Too much is unmirrored for the import path; the daemon pass takes it.
    Deferred,
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

/// How a sync was asked for: how long it waits on locks, and whether a big
/// backlog is left for later.
#[derive(Clone, Copy)]
struct SyncMode {
    wait: Duration,
    max_pending: Option<u64>,
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
    /// A transcript untouched this long is sealed. Seeded from
    /// `CSR_MIRROR_SEAL_SECS`.
    pub seal_after: Duration,
    /// Runs inside `sync_pass` right after the state table is read: the window
    /// in which another process can change a row the pass is holding.
    #[cfg(test)]
    after_snapshot: std::cell::RefCell<Option<Box<dyn Fn() + 'a>>>,
    /// Runs in `try_sync_file` right after the source is first opened, before
    /// any lock: the window in which the source can be replaced and another
    /// process can mirror the replacement.
    #[cfg(test)]
    after_source_open: std::cell::RefCell<Option<Box<dyn Fn() + 'a>>>,
}

impl<'a> Mirror<'a> {
    pub fn new(storage: &'a Storage, data_dir: &Path, projects_dir: &Path) -> Self {
        Self {
            storage,
            data_dir: data_dir.to_path_buf(),
            projects_dir: projects_dir.to_path_buf(),
            disabled: mirror_disabled(),
            seal_after: seal_after_from_env(),
            #[cfg(test)]
            after_snapshot: std::cell::RefCell::new(None),
            #[cfg(test)]
            after_source_open: std::cell::RefCell::new(None),
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
        self.sync_with(
            source,
            SyncMode {
                wait: LOCK_WAIT,
                max_pending: None,
            },
        )
    }

    /// [`sync_file`](Self::sync_file) for the import path: never waits on a
    /// lock another process holds, and leaves a large backlog (more than
    /// [`OPPORTUNISTIC_MAX_PENDING`] unmirrored) to the daemon pass.
    pub fn sync_file_opportunistic(&self, source: &Path) -> SyncOutcome {
        self.sync_with(
            source,
            SyncMode {
                wait: Duration::ZERO,
                max_pending: Some(OPPORTUNISTIC_MAX_PENDING),
            },
        )
    }

    fn sync_with(&self, source: &Path, mode: SyncMode) -> SyncOutcome {
        match self.try_sync_file(source, mode) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.note_error(&error);
                SyncOutcome::Failed
            }
        }
    }

    /// The mirror file and the source still match the state row: the `.zst` has
    /// the recorded length and the source's bytes up to `source_bytes` hash as
    /// recorded. A row that fails this needs the recovery in `try_sync_file`.
    fn row_intact(
        &self,
        rel: &str,
        row: &MirrorRow,
        source_file: &mut File,
        size: u64,
    ) -> io::Result<bool> {
        if size < row.source_bytes {
            return Ok(false);
        }
        let on_disk = match fs::metadata(self.zst_path(rel)) {
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        if on_disk != row.zst_bytes {
            return Ok(false);
        }
        Ok(tail_hash_at(source_file, row.source_bytes)? == row.tail_hash)
    }

    fn try_sync_file(&self, source: &Path, mode: SyncMode) -> Result<SyncOutcome> {
        if self.disabled {
            return Ok(SyncOutcome::Disabled);
        }
        let Some(rel) = self.rel_path(source) else {
            return Ok(SyncOutcome::NotMirrored);
        };
        let mut source_file = match File::open(source) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SyncOutcome::SourceGone)
            }
            Err(error) => return Err(error.into()),
        };
        #[cfg(test)]
        if let Some(hook) = self.after_source_open.borrow().as_ref() {
            hook();
        }
        let size = source_file.metadata()?.len();
        // The common call: nothing new since the last pass. One stat of the
        // source and of the mirror file, a 4 KB read, one lookup.
        let row = self.storage.mirror_row(&rel)?;
        let intact = match &row {
            Some(row) => self.row_intact(&rel, row, &mut source_file, size)?,
            None => false,
        };
        match &row {
            Some(row) if intact && row.source_bytes == size && row.source_gone_at.is_none() => {
                return Ok(SyncOutcome::UpToDate)
            }
            None if size == 0 => return Ok(SyncOutcome::UpToDate),
            _ => {}
        }
        if let Some(limit) = mode.max_pending {
            let pending = match &row {
                Some(row) if intact => size - row.source_bytes,
                _ => size,
            };
            if pending > limit {
                return Ok(SyncOutcome::Deferred);
            }
        }
        self.check_layout()?;
        let root = mirror_root(&self.data_dir);
        ensure_private_dir(&root)?;
        self.harden_root()?;
        // Shared with every other sync and seal, exclusive for purge.
        let Some(_mirror) = FileLock::acquire(&root.join(MIRROR_LOCK_NAME), false, mode.wait)?
        else {
            return Ok(SyncOutcome::Busy);
        };
        let zst = self.zst_path(&rel);
        let dir = zst
            .parent()
            .ok_or_else(|| anyhow!("mirror path has no parent"))?;
        ensure_private_dir(dir)?;
        let Some(_lock) = FileLock::acquire(&append_name(&zst, ".lock"), true, mode.wait)? else {
            return Ok(SyncOutcome::Busy);
        };

        // Everything below sees a stable mirror file and state row. The
        // descriptor from before the locks may point at a source that was
        // replaced while this call waited, and another process may have
        // mirrored the replacement: compare the state row against what the
        // path holds now, not against the old inode.
        let mut source_file = match File::open(source) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SyncOutcome::SourceGone)
            }
            Err(error) => return Err(error.into()),
        };
        let size = source_file.metadata()?.len();
        let had_row = self
            .storage
            .mirror_row(&rel)?
            .is_some_and(|row| row.source_bytes > 0);
        let mut row = self.recover(&rel, &zst)?;
        // A row that existed but could not be kept (the `.zst` lost bytes) is a
        // rebuild from scratch too.
        let mut rewrote = had_row && row.is_none();
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
            // One batch from a line boundary. A line is never held longer than
            // the batch: a window with no newline in it is either the unfinished
            // last line (it waits for its newline, however old the file is: a
            // credential cut in half would not match yet and could not be
            // scrubbed once the rest arrives) or a line past the cap.
            let want = BATCH_MAX_BYTES.min((size - offset) as usize);
            buf.clear();
            buf.resize(want, 0);
            source_file.seek(SeekFrom::Start(offset))?;
            source_file.read_exact(&mut buf)?;
            let Some(cut) = buf.iter().rposition(|byte| *byte == b'\n').map(|at| at + 1) else {
                if want >= BATCH_MAX_BYTES {
                    bail!(
                        "{rel}: a line over {} MiB at byte {offset}; not mirrored past it",
                        BATCH_MAX_BYTES >> 20
                    );
                }
                break;
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

    /// Both locks a writer of one transcript holds, mirror-wide (shared) first.
    /// `None` when either is busy, or when there is no mirror to lock.
    fn lock_transcript(&self, rel: &str, wait: Duration) -> Result<Option<(FileLock, FileLock)>> {
        let root = mirror_root(&self.data_dir);
        if !root.is_dir() {
            return Ok(None);
        }
        let Some(mirror) = FileLock::acquire(&root.join(MIRROR_LOCK_NAME), false, wait)? else {
            return Ok(None);
        };
        let lock = append_name(&self.zst_path(rel), ".lock");
        if !lock.parent().is_some_and(Path::is_dir) {
            return Ok(None);
        }
        let Some(transcript) = FileLock::acquire(&lock, true, wait)? else {
            return Ok(None);
        };
        Ok(Some((mirror, transcript)))
    }

    /// The source of a mirrored transcript is gone: say so on the row. Under
    /// the transcript's locks, and only that one column: a pass's snapshot of
    /// the row can be older than what another process has since written.
    fn mark_source_gone(&self, rel: &str) -> Result<bool> {
        let Some(_guards) = self.lock_transcript(rel, LOCK_WAIT)? else {
            return Ok(false);
        };
        self.storage.mirror_mark_gone(rel, &now_stamp())
    }

    /// Repack a many-frame transcript into one frame. Decodes and re-encodes
    /// as a stream, so the largest transcript never sits in memory. Order:
    /// finished `.part` fsynced, state updated, rename (see `settle_seal`).
    pub fn seal_file(&self, rel: &str) -> Result<bool> {
        if self.disabled {
            return Ok(false);
        }
        let Some(_guards) = self.lock_transcript(rel, LOCK_WAIT)? else {
            return Ok(false);
        };
        let zst = self.zst_path(rel);
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
        #[cfg(test)]
        if let Some(hook) = self.after_snapshot.borrow().as_ref() {
            hook();
        }
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
                Some(row) => {
                    row.source_bytes != meta.len()
                        || row.source_gone_at.is_some()
                        || !self.matches_disk(&rel, row, source, meta.len())
                }
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
                match self.mark_source_gone(rel) {
                    Ok(true) => stats.sources_gone += 1,
                    Ok(false) => {}
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

    /// [`row_intact`](Self::row_intact) for the pass: any error reads as "no".
    fn matches_disk(&self, rel: &str, row: &MirrorRow, source: &Path, size: u64) -> bool {
        File::open(source)
            .and_then(|mut file| self.row_intact(rel, row, &mut file, size))
            .unwrap_or(false)
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
        // Writers hold the mirror-wide lock shared; the delete holds it
        // exclusively, so nothing writes into the folder (or the table) while
        // it goes. The lock file sits outside the folder that is deleted.
        let _exclusive = if yes {
            let root = mirror_root(&self.data_dir);
            ensure_private_dir(&root)?;
            let held = FileLock::acquire(&root.join(MIRROR_LOCK_NAME), true, LOCK_WAIT)?;
            Some(held.ok_or_else(|| anyhow!("the mirror is busy (a sync is running); try again"))?)
        } else {
            None
        };
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

    // ─── U3: fix round after review ───

    #[test]
    fn a_stale_pass_snapshot_never_overwrites_a_sealed_mirror_of_a_gone_source() {
        let rig = Rig::new();
        let src = rig.source("gone.jsonl");
        let rel = "-tmp-proj/gone.jsonl";
        for n in 0..3 {
            append(&src, line(n, &format!("entry {n}")).as_bytes());
            rig.mirror().sync_file(&src);
        }
        let before = rig.decoded("gone.jsonl");
        assert_eq!(rig.storage.mirror_row(rel).unwrap().unwrap().frames, 3);

        // The pass reads its snapshot (three frames). Before it gets to the
        // gone-source step, another process seals the mirror and the source
        // disappears.
        let mirror = rig.mirror();
        {
            let src = src.clone();
            let rig = &rig;
            mirror.after_snapshot.replace(Some(Box::new(move || {
                assert!(rig.mirror().seal_file(rel).unwrap());
                fs::remove_file(&src).unwrap();
            })));
        }
        let mut mirror = mirror;
        mirror.seal_after = Duration::ZERO;
        let stats = mirror.sync_pass(None);

        assert_eq!(stats.sources_gone, 1);
        assert_eq!(stats.errors, 0);
        assert_eq!(rig.decoded("gone.jsonl"), before);
        let row = rig.storage.mirror_row(rel).unwrap().unwrap();
        assert!(row.source_gone_at.is_some());
        assert_eq!((row.frames, row.sealed), (1, true));
        assert_eq!(
            row.zst_bytes,
            fs::metadata(rig.zst("gone.jsonl")).unwrap().len()
        );
    }

    #[test]
    fn a_same_size_rewrite_of_the_source_is_caught_before_up_to_date() {
        let rig = Rig::new();
        let src = rig.source("same.jsonl");
        let first = format!("{}{}", line(1, "alpha alpha"), line(2, "beta beta"));
        fs::write(&src, &first).unwrap();
        rig.mirror().sync_file(&src);

        let second = first.replace("alpha", "gamma");
        assert_eq!(second.len(), first.len());
        fs::write(&src, &second).unwrap();
        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Rewritten { .. }
        ));
        assert_eq!(rig.decoded("same.jsonl"), second.as_bytes());

        // The daemon pass finds it too.
        let third = second.replace("beta", "delt");
        fs::write(&src, &third).unwrap();
        assert_eq!(rig.mirror().sync_pass(None).synced, 1);
        assert_eq!(rig.decoded("same.jsonl"), third.as_bytes());
    }

    #[test]
    fn a_missing_or_truncated_mirror_file_is_rebuilt_while_the_source_is_unchanged() {
        let rig = Rig::new();
        let src = rig.source("dmg.jsonl");
        let text = format!("{}{}", line(1, "one"), line(2, "two"));
        fs::write(&src, &text).unwrap();
        rig.mirror().sync_file(&src);

        let zst = rig.zst("dmg.jsonl");
        let full = fs::read(&zst).unwrap();
        fs::write(&zst, &full[..full.len() - 5]).unwrap();
        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Rewritten { .. }
        ));
        assert_eq!(rig.decoded("dmg.jsonl"), text.as_bytes());

        fs::remove_file(&zst).unwrap();
        assert_eq!(rig.mirror().sync_pass(None).synced, 1);
        assert_eq!(rig.decoded("dmg.jsonl"), text.as_bytes());
    }

    #[test]
    fn a_partial_last_line_is_never_flushed_even_when_the_file_is_idle() {
        let rig = Rig::new();
        let src = rig.source("part.jsonl");
        let first = line(1, "complete");
        let github = fake_github();
        // The writer stopped inside a credential: too short to match yet.
        let whole = line(2, &format!("token {github} end"));
        let cut = whole.find("ghp_").unwrap() + 10;
        fs::write(&src, format!("{first}{}", &whole[..cut])).unwrap();

        let mut mirror = rig.mirror();
        mirror.seal_after = Duration::ZERO;
        mirror.sync_file(&src);
        assert_eq!(rig.decoded("part.jsonl"), first.as_bytes());
        let rel = "-tmp-proj/part.jsonl";
        let row = rig.storage.mirror_row(rel).unwrap().unwrap();
        assert_eq!(row.source_bytes, first.len() as u64);

        // The writer resumes: the finished line is scrubbed as a whole.
        append(&src, &whole.as_bytes()[cut..]);
        mirror.sync_file(&src);
        let decoded = String::from_utf8(rig.decoded("part.jsonl")).unwrap();
        assert!(!decoded.contains("ghp_"));
        assert!(decoded.contains("[secret redacted: github_token]"));
        assert_eq!(decoded.lines().count(), 2);
    }

    #[test]
    fn json_unicode_escapes_cannot_hide_a_secret_or_a_private_span() {
        let rig = Rig::new();
        let src = rig.source("esc.jsonl");
        let body = "A".repeat(36);
        let secret_line = format!(
            "{{\"type\":\"user\",\"message\":{{\"content\":\"key \\u0067hp_{body} end\"}}}}\n"
        );
        let private_line = "{\"type\":\"user\",\"message\":{\"content\":\"a \\u003cprivate>hush hush\\u003c/private> b\"}}\n";
        let clean_escape =
            "{\"type\":\"user\",\"message\":{\"content\":\"caf\\u00e9 \\u001b[0m ok\"}}\n";
        fs::write(&src, format!("{secret_line}{private_line}{clean_escape}")).unwrap();
        rig.mirror().sync_file(&src);

        let decoded = String::from_utf8(rig.decoded("esc.jsonl")).unwrap();
        let mut lines = decoded.split_inclusive('\n');
        let first = lines.next().unwrap();
        assert!(!first.contains(&body), "{first}");
        assert!(first.contains("[secret redacted: github_token]"));
        let second = lines.next().unwrap();
        assert!(!second.contains("hush"), "{second}");
        // An escape that cannot spell a pattern leaves the line byte-identical.
        assert_eq!(lines.next().unwrap(), clean_escape);
    }

    #[test]
    fn object_keys_are_scrubbed_in_the_mirror() {
        let rig = Rig::new();
        let src = rig.source("keys.jsonl");
        let other = format!("{}{}", "ghp_", "B".repeat(36));
        let text = format!(
            "{{\"input\":{{\"{}\":\"acct-1\",\"{other}\":\"acct-2\",\"plain\":1}}}}\n",
            fake_github()
        );
        fs::write(&src, &text).unwrap();
        rig.mirror().sync_file(&src);
        let decoded = String::from_utf8(rig.decoded("keys.jsonl")).unwrap();
        assert!(!decoded.contains("ghp_"), "{decoded}");
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        let object = value["input"].as_object().unwrap();
        // Two secrets collapse to one marker; neither value is lost.
        assert_eq!(object.len(), 3, "{decoded}");
        assert!(object.values().any(|v| v == "acct-1"));
        assert!(object.values().any(|v| v == "acct-2"));
        assert_eq!(object["plain"], 1);
    }

    #[test]
    fn a_source_replaced_while_waiting_for_the_locks_does_not_clobber_the_newer_mirror() {
        let rig = Rig::new();
        let src = rig.source("swap.jsonl");
        let rel = "-tmp-proj/swap.jsonl";
        append(&src, line(1, "old one").as_bytes());
        rig.mirror().sync_file(&src);
        append(&src, line(2, "old two").as_bytes());

        // This call opens the old inode. Before it takes any lock, the source is
        // replaced and another process mirrors the replacement.
        let replacement = format!(
            "{}{}{}",
            line(7, "new seven"),
            line(8, "new eight"),
            line(9, "new nine")
        );
        let mirror = rig.mirror();
        {
            let src = src.clone();
            let replacement = replacement.clone();
            let rig = &rig;
            mirror.after_source_open.replace(Some(Box::new(move || {
                let staged = src.with_extension("new");
                fs::write(&staged, &replacement).unwrap();
                fs::rename(&staged, &src).unwrap();
                assert!(matches!(
                    rig.mirror().sync_file(&src),
                    SyncOutcome::Rewritten { .. }
                ));
            })));
        }
        mirror.sync_file(&src);
        // The replacement is later deleted: the mirror is its only copy.
        fs::remove_file(&src).unwrap();

        assert_eq!(rig.decoded("swap.jsonl"), replacement.as_bytes());
        let row = rig.storage.mirror_row(rel).unwrap().unwrap();
        assert_eq!(row.source_bytes, replacement.len() as u64);
    }

    #[test]
    fn a_renamed_key_never_lands_on_a_key_that_was_already_there() {
        let rig = Rig::new();
        let src = rig.source("collide.jsonl");
        let aws = format!("AKIA{}", "B".repeat(16));
        let marker = "[secret redacted: aws_access_key]";
        let text = format!(
            "{{\"input\":{{\"{aws}\":\"credential\",\"{marker}\":\"existing\",\"z\":1}},\"other\":{{\"{aws}\":\"alone\"}}}}\n"
        );
        fs::write(&src, &text).unwrap();
        rig.mirror().sync_file(&src);
        let decoded = String::from_utf8(rig.decoded("collide.jsonl")).unwrap();
        assert!(!decoded.contains("AKIA"), "{decoded}");
        let value: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        let input = value["input"].as_object().unwrap();
        assert_eq!(input.len(), 3, "{decoded}");
        assert_eq!(input[marker], "existing");
        assert!(input.values().any(|v| v == "credential"));
        // A different object is its own namespace: no suffix needed there.
        assert_eq!(value["other"][marker], "alone");
    }

    #[test]
    fn purge_waits_for_a_running_sync_and_a_sync_waits_for_purge() {
        let rig = Rig::new();
        let src = rig.source("lk.jsonl");
        fs::write(&src, line(1, "x")).unwrap();
        rig.mirror().sync_file(&src);

        let lock_path = mirror_root(&rig.data_dir).join(".mirror.lock");
        let open = || {
            private_file_options()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_path)
                .unwrap()
        };
        // A sync in flight holds the shared lock: purge must not delete under it.
        let syncing = open();
        syncing.lock_shared().unwrap();
        assert!(rig.mirror().purge(true).is_err());
        assert!(rig.zst("lk.jsonl").exists());
        assert_eq!(rig.storage.mirror_totals().unwrap().files, 1);
        FileExt::unlock(&syncing).unwrap();

        // A purge in flight holds it exclusively: a sync backs off.
        let purging = open();
        purging.lock_exclusive().unwrap();
        append(&src, line(2, "y").as_bytes());
        assert_eq!(rig.mirror().sync_file(&src), SyncOutcome::Busy);
        FileExt::unlock(&purging).unwrap();
        assert!(matches!(
            rig.mirror().sync_file(&src),
            SyncOutcome::Appended { .. }
        ));
    }

    #[test]
    fn a_line_over_the_cap_stops_the_file_and_is_never_mirrored() {
        let rig = Rig::new();
        let src = rig.source("huge.jsonl");
        let first = line(1, "small and fine");
        let github = fake_github();
        let mut huge = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{github} ");
        huge.push_str(&"y".repeat(BATCH_MAX_BYTES));
        huge.push_str("\"}}\n");
        let after = line(3, "after the huge line");
        fs::write(&src, format!("{first}{huge}{after}")).unwrap();

        rig.mirror().sync_file(&src);
        let decoded = rig.decoded("huge.jsonl");
        assert!(
            decoded == first.as_bytes(),
            "mirror holds {} bytes",
            decoded.len()
        );
        let row = rig
            .storage
            .mirror_row("-tmp-proj/huge.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(row.source_bytes, first.len() as u64);
        assert_eq!(
            rig.storage
                .get_meta_counter(MIRROR_ERRORS_KEY)
                .unwrap()
                .unwrap(),
            1
        );
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

    fn ignorable_line(bytes: usize) -> String {
        format!(
            "{{\"type\":\"file-history-snapshot\",\"blob\":\"{}\"}}\n",
            "x".repeat(bytes)
        )
    }

    #[tokio::test]
    async fn an_import_leaves_a_large_mirror_backlog_to_the_daemon_pass() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let src = rig.source("sess-big.jsonl");
        write_session(&src, &fake_github());
        let mut body = fs::read_to_string(&src).unwrap();
        for _ in 0..9 {
            body.push_str(&ignorable_line(1 << 20));
        }
        fs::write(&src, body).unwrap();

        assert!(engine.import_file(&src, "proj").await.unwrap() > 0);
        let rel = "-tmp-proj/sess-big.jsonl";
        assert!(engine.storage().mirror_row(rel).unwrap().is_none());

        let stats = engine.mirror().sync_pass(None);
        assert_eq!((stats.synced, stats.errors), (1, 0));
        assert_eq!(
            engine
                .storage()
                .mirror_row(rel)
                .unwrap()
                .unwrap()
                .source_bytes,
            fs::metadata(&src).unwrap().len()
        );
    }

    #[tokio::test]
    async fn a_busy_transcript_lock_does_not_stall_the_import() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let src = rig.source("sess-busy.jsonl");
        write_session(&src, &fake_stripe());
        engine.import_file(&src, "proj").await.unwrap();

        append(&src, line(9, "a later message").as_bytes());
        let lock = File::create(append_name(&rig.zst("sess-busy.jsonl"), ".lock")).unwrap();
        lock.lock_exclusive().unwrap();
        let started = std::time::Instant::now();
        engine.import_file(&src, "proj").await.unwrap();
        let took = started.elapsed();
        FileExt::unlock(&lock).unwrap();
        assert!(took < Duration::from_millis(1500), "import waited {took:?}");
        // The next pass catches the mirror up.
        let stats = engine.mirror().sync_pass(None);
        assert_eq!((stats.synced, stats.errors), (1, 0));
    }

    fn chunks_like(engine: &crate::engine::Engine, needle: &str) -> i64 {
        engine
            .storage()
            .with_connection(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM chunks WHERE content LIKE ?1",
                    [format!("%{needle}%")],
                    |row| row.get(0),
                )?)
            })
            .unwrap()
    }

    /// A conversation stored by a build without the scrubber: its first four
    /// messages each hold a private key block. Returns the conversation id.
    async fn legacy_conversation(engine: &crate::engine::Engine, src: &Path) -> String {
        let pem = format!(
            "{}{}\n{}\n{}{}",
            "-----BEGIN RSA PRIV",
            "ATE KEY-----",
            "Zm9vYmFy".repeat(200),
            "-----END RSA PRIV",
            "ATE KEY-----"
        );
        let _off = crate::redact::DisabledForTest::set(true);
        let mut body = String::new();
        for n in 0..4 {
            body.push_str(&session_message(n, &format!("dump {n}\n{pem}\nend {n}")));
        }
        fs::write(src, body).unwrap();
        assert!(engine.import_file(src, "proj").await.unwrap() >= 4);
        assert!(chunks_like(engine, "Zm9vYmFy") >= 4);
        src.file_stem().unwrap().to_string_lossy().to_string()
    }

    fn session_message(n: usize, text: &str) -> String {
        serde_json::json!({
            "type": "user", "uuid": format!("r-{n}"),
            "timestamp": format!("2026-10-14T10:00:{n:02}Z"),
            "message": {"content": text}
        })
        .to_string()
            + "\n"
    }

    /// Every column of every chunk row of `conv`, and its embedding, as text.
    fn stored_rows(engine: &crate::engine::Engine, conv: &str) -> Vec<String> {
        engine
            .storage()
            .with_connection(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT c.id, c.project_name, c.timestamp, c.content, c.message_count,
                            c.summary, c.seq, c.is_sidechain, c.source, e.embedding
                     FROM chunks c LEFT JOIN chunk_embeddings e ON e.chunk_id = c.id
                     WHERE c.conversation_id = ?1 ORDER BY c.id",
                )?;
                let rows = stmt
                    .query_map([conv], |row| {
                        let mut cells = Vec::new();
                        for i in 0..10 {
                            cells.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?));
                        }
                        Ok(cells.join("|"))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .unwrap()
    }

    fn bind_first_chunk_to_a_witness(engine: &crate::engine::Engine, conv: &str) -> Vec<String> {
        engine
            .storage()
            .with_connection(|conn| {
                conn.execute(
                    "INSERT INTO witness_ledger (project, file, symbol, stamp, tier, source_kind)
                     VALUES ('proj', '/repo/a.rs', 'foo', 'b3:1', 'committed', 'test')",
                    [],
                )?;
                let witness: i64 =
                    conn.query_row("SELECT id FROM witness_ledger", [], |r| r.get(0))?;
                conn.execute(
                    "INSERT INTO witness_chunk_bindings (witness_id, chunk_id)
                     SELECT ?1, id FROM chunks WHERE conversation_id = ?2",
                    rusqlite::params![witness, conv],
                )?;
                Ok(())
            })
            .unwrap();
        witness_bindings(engine)
    }

    fn witness_bindings(engine: &crate::engine::Engine) -> Vec<String> {
        engine
            .storage()
            .with_connection(|conn| {
                let mut stmt =
                    conn.prepare("SELECT chunk_id FROM witness_chunk_bindings ORDER BY chunk_id")?;
                let ids = stmt
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(ids)
            })
            .unwrap()
    }

    #[derive(Clone, Copy)]
    enum Via {
        /// `Engine::import_file`.
        Engine,
        /// The watcher's content-diff import.
        Watcher,
    }

    async fn import_via(engine: &crate::engine::Engine, rig: &Rig, src: &Path, via: Via) {
        match via {
            Via::Engine => {
                engine.import_file(src, "proj").await.unwrap();
            }
            Via::Watcher => {
                crate::import::watcher::import_file_content_diff(
                    engine.storage(),
                    engine.embeddings(),
                    engine.search(),
                    &rig.projects,
                    src,
                )
                .await
                .unwrap();
            }
        }
    }

    /// Rows stored before the scrubber are never touched; everything after them
    /// is imported scrubbed and found by search. Includes the original review
    /// finding: the scrub would cut the earlier private-key chunks short, and a
    /// count-based resume would then skip the new messages.
    async fn legacy_rows_survive_and_new_messages_land(via: Via) {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let src = rig.source("sess-legacy.jsonl");
        let conv = legacy_conversation(&engine, &src).await;
        let before = stored_rows(&engine, &conv);
        let old_secret_rows = chunks_like(&engine, "Zm9vYmFy");
        let bindings = bind_first_chunk_to_a_witness(&engine, &conv);
        assert_eq!(bindings.len(), before.len());

        let token = format!("{}{}", "ghp_", "Q".repeat(36));
        append(
            &src,
            session_message(10, &format!("freshly appended alpha {token}")).as_bytes(),
        );
        append(
            &src,
            session_message(11, "freshly appended bravo").as_bytes(),
        );
        filetime_touch(&src);
        import_via(&engine, &rig, &src, via).await;

        let after = stored_rows(&engine, &conv);
        for row in &before {
            assert!(after.contains(row), "a stored row changed or went: {row}");
        }
        assert!(after.len() > before.len());
        assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
        assert_eq!(chunks_like(&engine, "freshly appended bravo"), 1);
        assert_eq!(chunks_like(&engine, "ghp_"), 0);
        assert_eq!(chunks_like(&engine, "[secret redacted: github_token]"), 1);
        assert_eq!(chunks_like(&engine, "Zm9vYmFy"), old_secret_rows);
        assert_eq!(witness_bindings(&engine), bindings);
        assert!(engine.storage().legacy_frontier(&conv).unwrap().is_some());
        // Only what the scrub did to the new message is counted, not the
        // secrets the stored rows still hold.
        assert_eq!(engine.storage().get_secrets_redacted().unwrap().0, 1);

        // The next appends land too, and the stored rows stay as they were.
        let long = format!("freshly appended charlie {}", "lorem ipsum ".repeat(120));
        append(&src, session_message(12, &long).as_bytes());
        filetime_touch(&src);
        import_via(&engine, &rig, &src, via).await;
        import_via(&engine, &rig, &src, via).await;
        let later = stored_rows(&engine, &conv);
        for row in &before {
            assert!(later.contains(row), "a stored row changed or went: {row}");
        }
        assert_eq!(chunks_like(&engine, "freshly appended charlie"), 1);
        assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
        assert_eq!(witness_bindings(&engine), bindings);
    }

    #[tokio::test]
    async fn legacy_rows_stay_byte_identical_and_new_messages_land_scrubbed() {
        legacy_rows_survive_and_new_messages_land(Via::Engine).await;
    }

    #[tokio::test]
    async fn legacy_rows_stay_byte_identical_through_the_watcher_path() {
        legacy_rows_survive_and_new_messages_land(Via::Watcher).await;
    }

    #[tokio::test]
    async fn a_legacy_conversation_without_receipts_is_placed_by_its_text() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let src = rig.source("sess-bare.jsonl");
        let conv = legacy_conversation(&engine, &src).await;
        engine
            .storage()
            .with_connection(|conn| {
                conn.execute("DELETE FROM chunk_spans", [])?;
                Ok(())
            })
            .unwrap();
        let before = stored_rows(&engine, &conv);
        let old_tail = chunks_like(&engine, "end 3");

        append(
            &src,
            session_message(10, "freshly appended alpha").as_bytes(),
        );
        filetime_touch(&src);
        import_via(&engine, &rig, &src, Via::Engine).await;

        let after = stored_rows(&engine, &conv);
        for row in &before {
            assert!(after.contains(row), "a stored row changed or went: {row}");
        }
        assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
        // The messages the stored rows hold are not chunked a second time.
        assert_eq!(chunks_like(&engine, "end 3"), old_tail);
    }

    #[tokio::test]
    async fn a_legacy_conversation_that_cannot_be_placed_is_left_alone_until_it_can() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let src = rig.source("sess-lost.jsonl");
        let conv = legacy_conversation(&engine, &src).await;
        let original: (String, String) = engine
            .storage()
            .with_connection(|conn| {
                conn.execute("DELETE FROM chunk_spans", [])?;
                Ok(conn.query_row(
                    "SELECT id, content FROM chunks WHERE conversation_id = ?1
                     ORDER BY seq DESC, rowid DESC LIMIT 1",
                    [&conv],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        let set_tail = |content: &str| {
            engine
                .storage()
                .with_connection(|conn| {
                    conn.execute(
                        "UPDATE chunks SET content = ?2 WHERE id = ?1",
                        [&original.0, content],
                    )?;
                    Ok(())
                })
                .unwrap();
        };
        set_tail("text that is nowhere in the transcript");
        let before = stored_rows(&engine, &conv);

        append(
            &src,
            session_message(10, "freshly appended alpha").as_bytes(),
        );
        filetime_touch(&src);
        import_via(&engine, &rig, &src, Via::Engine).await;
        import_via(&engine, &rig, &src, Via::Watcher).await;

        assert_eq!(stored_rows(&engine, &conv), before);
        assert_eq!(chunks_like(&engine, "freshly appended alpha"), 0);
        assert!(engine.storage().legacy_frontier(&conv).unwrap().is_none());

        // Once the place can be found, the pending message lands.
        set_tail(&original.1);
        append(
            &src,
            session_message(11, "freshly appended bravo").as_bytes(),
        );
        filetime_touch(&src);
        import_via(&engine, &rig, &src, Via::Engine).await;
        assert_eq!(chunks_like(&engine, "freshly appended alpha"), 1);
        assert_eq!(chunks_like(&engine, "freshly appended bravo"), 1);
    }

    /// `status` reports how many legacy conversations import placed and how many
    /// it left alone because it could not: one of each here, and the waiting one
    /// moves to placed once its place can be found.
    #[tokio::test]
    async fn status_counts_one_placed_and_one_waiting_legacy_conversation() {
        let rig = Rig::new();
        let engine = engine_for(&rig).await;
        let counts = || crate::status::LegacyFrontierStatus::read(engine.storage());
        assert_eq!(counts(), Default::default());

        // One that can be placed: its stored rows carry transcript receipts.
        let placed = rig.source("sess-placed.jsonl");
        legacy_conversation(&engine, &placed).await;
        // One that cannot: no receipts, and its newest chunk is text the transcript lacks.
        let lost = rig.source("sess-lost-count.jsonl");
        let lost_conv = legacy_conversation(&engine, &lost).await;
        let (tail_id, tail_text): (String, String) = engine
            .storage()
            .with_connection(|conn| {
                conn.execute(
                    "DELETE FROM chunk_spans WHERE chunk_id IN
                     (SELECT id FROM chunks WHERE conversation_id = ?1)",
                    [&lost_conv],
                )?;
                Ok(conn.query_row(
                    "SELECT id, content FROM chunks WHERE conversation_id = ?1
                     ORDER BY seq DESC, rowid DESC LIMIT 1",
                    [&lost_conv],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();
        let set_tail = |content: &str| {
            engine
                .storage()
                .with_connection(|conn| {
                    conn.execute(
                        "UPDATE chunks SET content = ?2 WHERE id = ?1",
                        [&tail_id, content],
                    )?;
                    Ok(())
                })
                .unwrap();
        };
        set_tail("text that is nowhere in the transcript");

        // Nothing has tried to place either yet: nothing is waiting.
        assert_eq!(counts(), Default::default());
        for src in [&placed, &lost] {
            append(
                src,
                session_message(10, "freshly appended alpha").as_bytes(),
            );
            filetime_touch(src);
            import_via(&engine, &rig, src, Via::Engine).await;
        }
        assert_eq!(
            (counts().placed, counts().waiting),
            (1, 1),
            "one placed, one left alone"
        );

        // The watcher retrying the same lost conversation is still one waiting.
        import_via(&engine, &rig, &lost, Via::Watcher).await;
        assert_eq!((counts().placed, counts().waiting), (1, 1));

        // Its place can be found again: it is placed, and no longer waiting.
        set_tail(&tail_text);
        append(
            &lost,
            session_message(11, "freshly appended bravo").as_bytes(),
        );
        filetime_touch(&lost);
        import_via(&engine, &rig, &lost, Via::Engine).await;
        assert_eq!((counts().placed, counts().waiting), (2, 0));
    }

    /// The import-state check compares mtimes; make sure an append inside the
    /// same clock tick still reads as changed.
    fn filetime_touch(path: &Path) {
        let later = SystemTime::now() + Duration::from_secs(5);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(later)
            .unwrap();
    }

    #[test]
    fn archived_subagent_transcripts_are_found_in_the_mirror_by_id() {
        let rig = Rig::new();
        let nested = transcripts_dir(&rig.data_dir).join("-tmp-proj/sess-1/subagents");
        fs::create_dir_all(&nested).unwrap();
        let mut body = String::new();
        write_session(&rig.source("scratch.jsonl"), "nothing");
        body.push_str(&fs::read_to_string(rig.source("scratch.jsonl")).unwrap());
        fs::write(
            nested.join("agent-a1b2.jsonl.zst"),
            compress_frame(body.as_bytes()).unwrap(),
        )
        .unwrap();

        let pointer = crate::mcp::tools::get_full_conversation(
            &rig.projects,
            Some(&rig.data_dir),
            "agent-a1b2",
            None,
        );
        assert!(pointer.contains("<mirror_path>"), "got: {pointer}");
        assert!(pointer.contains("-tmp-proj"), "got: {pointer}");
        let request = crate::transcript::TranscriptRequest {
            session: "agent-a1b2".to_string(),
            view: crate::transcript::ViewKind::Stats,
            ..Default::default()
        };
        let stats =
            crate::transcript::run_with_mirror(&rig.projects, Some(&rig.data_dir), &request);
        assert!(stats.contains("<turns total=\"2\""), "got: {stats}");

        // The same id under a second project is still ambiguous.
        let other = transcripts_dir(&rig.data_dir).join("-tmp-other/sess-2/subagents");
        fs::create_dir_all(&other).unwrap();
        fs::write(
            other.join("agent-a1b2.jsonl.zst"),
            compress_frame(body.as_bytes()).unwrap(),
        )
        .unwrap();
        let pointer = crate::mcp::tools::get_full_conversation(
            &rig.projects,
            Some(&rig.data_dir),
            "agent-a1b2",
            None,
        );
        assert!(pointer.contains("ambiguous"), "got: {pointer}");
    }

    #[test]
    fn a_damaged_mirror_frame_is_reported_as_incomplete_not_as_a_clean_read() {
        let rig = Rig::new();
        let dir = transcripts_dir(&rig.data_dir).join("-tmp-proj");
        fs::create_dir_all(&dir).unwrap();
        let good = [line(1, "first prompt"), line(2, "second prompt")].concat();
        let torn_source = (3..400)
            .map(|n| line(n, "a later prompt in a torn frame"))
            .collect::<String>();
        let torn = compress_frame(torn_source.as_bytes()).unwrap();
        let mut bytes = compress_frame(good.as_bytes()).unwrap();
        bytes.extend_from_slice(&torn[..torn.len() / 2]);
        fs::write(dir.join("sess-torn.jsonl.zst"), bytes).unwrap();

        let ask = |view, json| {
            crate::transcript::run_with_mirror(
                &rig.projects,
                Some(&rig.data_dir),
                &crate::transcript::TranscriptRequest {
                    session: "sess-torn".to_string(),
                    view,
                    json,
                    ..Default::default()
                },
            )
        };
        let stats = ask(crate::transcript::ViewKind::Stats, false);
        assert!(stats.contains("INCOMPLETE"), "got: {stats}");
        let prompts = ask(crate::transcript::ViewKind::Prompts, false);
        assert!(prompts.contains("INCOMPLETE"), "got: {prompts}");
        assert!(prompts.contains("first prompt"));
        let json = ask(crate::transcript::ViewKind::Stats, true);
        assert!(json.contains("\"incomplete\":true"), "got: {json}");
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
