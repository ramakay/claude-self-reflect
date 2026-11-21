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

use crate::storage::transcript_mirror::{MirrorRow, MIRROR_ERRORS_KEY, MIRROR_LAST_ERROR_KEY};
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

    /// Source bytes the mirror has committed for `source`: always ends on a line boundary.
    pub fn committed_source_bytes(&self, source: &Path) -> Option<u64> {
        let rel = self.rel_path(source)?;
        Some(self.storage.mirror_row(&rel).ok()??.source_bytes)
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
        // The counter alone cannot explain itself later. Keep the last error's text and time.
        let _ = self.storage.set_meta(
            MIRROR_LAST_ERROR_KEY,
            &format!("{} {error:#}", chrono::Utc::now().to_rfc3339()),
        );
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
#[path = "mirror_tests.rs"]
mod tests;
