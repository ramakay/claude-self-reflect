//! One-time removal of prompt and session-start previews from timing logs.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use crate::storage::Storage;

const META_KEY: &str = "timing_query_scrub_v3";

pub(crate) fn query_fields(salt: Option<&[u8]>, query: &str) -> String {
    let Some(salt) = salt else {
        return format!("q_len={}", query.chars().count());
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(salt);
    hasher.update(query.as_bytes());
    format!(
        "q_len={} q_hash={}",
        query.chars().count(),
        &hasher.finalize().to_hex()[..8]
    )
}

/// Fail closed: if the salt cannot be read or created, omit the detail line.
pub(crate) fn load_salt(log: &Path) -> io::Result<Vec<u8>> {
    let path = log.with_file_name("hook-timing.salt");
    if let Some(salt) = read_salt(&path)? {
        return Ok(salt);
    }
    let temp = path.with_extension(format!("salt-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temp)?;
        file.write_all(uuid::Uuid::new_v4().as_bytes())?;
        file.sync_all()?;
        match fs::hard_link(&temp, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Recheck after publication races; never replace a valid salt.
                if let Some(salt) = read_salt(&path)? {
                    return Ok(salt);
                }
                fs::rename(&temp, &path)?;
            }
            Err(e) => return Err(e),
        }
        read_salt(&path)?.ok_or_else(|| io::Error::other("invalid timing salt length"))
    })();
    let _ = fs::remove_file(&temp);
    result
}

fn read_salt(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(io::Error::other("timing salt is not a regular file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let mut salt = Vec::new();
    options.open(path)?.take(17).read_to_end(&mut salt)?;
    if salt.len() != 16 {
        return Ok(None);
    }
    crate::private_files::set_mode(path, 0o600)?;
    Ok(Some(salt))
}

/// A scrub pass that loses a race with a hook appending to the log is retried
/// a few times in-process, then left for the next daemon start.
const SCRUB_ATTEMPTS: u32 = 3;
const SCRUB_RETRY_DELAY: Duration = Duration::from_millis(250);

/// Only mark completion after both files succeed; failures retry in-process,
/// then at next start. Every failed pass is logged as a warning.
pub(crate) fn scrub_once(storage: &Storage) {
    if let Some(log) = super::default_log_path() {
        let rotated = super::rotated_log_path();
        scrub_with_retry(SCRUB_ATTEMPTS, SCRUB_RETRY_DELAY, || {
            scrub_paths_once(storage, &log, rotated.as_deref())
        });
    }
}

/// Run `pass` up to `attempts` times, sleeping `delay * attempt` between tries.
/// Returns whether a pass succeeded. Errors name paths and OS causes only,
/// never log content.
fn scrub_with_retry(
    attempts: u32,
    delay: Duration,
    mut pass: impl FnMut() -> anyhow::Result<()>,
) -> bool {
    for attempt in 1..=attempts {
        match pass() {
            Ok(()) => return true,
            Err(error) if attempt < attempts => {
                tracing::warn!(attempt, attempts, %error, "timing log scrub failed, retrying");
                std::thread::sleep(delay * attempt);
            }
            Err(error) => {
                tracing::warn!(
                    attempt,
                    attempts,
                    %error,
                    "timing log scrub failed, prompt previews may remain until the next daemon start"
                );
            }
        }
    }
    false
}

fn scrub_paths_once(storage: &Storage, log: &Path, rotated: Option<&Path>) -> anyhow::Result<()> {
    if storage.get_meta(META_KEY)?.as_deref() == Some("1") {
        return Ok(());
    }
    let salt = load_salt(log).ok();
    let live_result = scrub_file(log, salt.as_deref());
    let rotated_result = rotated.map_or(Ok(()), |path| scrub_file(path, salt.as_deref()));
    // Evaluate both before propagating either error so rotation cannot retain prompts.
    live_result?;
    rotated_result?;
    storage.set_meta(META_KEY, "1")
}

fn scrub_bytes(input: &[u8], salt: Option<&[u8]>) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut record_start = 0;
    let mut line_start = 0;
    for line in input.split_inclusive(|b| *b == b'\n') {
        if line_start > record_start && starts_record(line) {
            scrub_record(&input[record_start..line_start], salt, &mut output);
            record_start = line_start;
        }
        line_start += line.len();
    }
    scrub_record(&input[record_start..], salt, &mut output);
    output
}

/// A record starts with the writer's exact `<timestamp>Z ` prefix followed by
/// `CSR `, which every producer of `append_timing_line` emits. A pasted
/// timestamp-shaped line inside a prompt preview is not a record start unless
/// it also reads `CSR `.
fn starts_record(line: &[u8]) -> bool {
    starts_timestamp(line) && line[21..].starts_with(b"CSR ")
}

fn starts_timestamp(line: &[u8]) -> bool {
    line.len() >= 21
        && line[4] == b'-'
        && line[7] == b'-'
        && line[10] == b'T'
        && line[13] == b':'
        && line[16] == b':'
        && line[19] == b'Z'
        && line[20] == b' '
        && [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]
            .iter()
            .all(|&i| line[i].is_ascii_digit())
}

fn scrub_record(record: &[u8], salt: Option<&[u8]>, output: &mut Vec<u8>) {
    const MARKER: &[u8] = b"inject: query=\"";
    const END: &[u8] = b"\" items=";
    const PREVIEW: &[u8] = b" preview=\"";
    let content_end = record.len() - usize::from(record.ends_with(b"\n"));
    let content = &record[..content_end];
    let inject = if starts_record(content) { 21 } else { 0 };
    if content[inject..].starts_with(b"CSR session-start inject [") {
        if let Some(preview) = content[inject..]
            .windows(PREVIEW.len())
            .position(|w| w == PREVIEW)
        {
            output.extend_from_slice(&content[..inject + preview]);
            output.extend_from_slice(&record[content_end..]);
            return;
        }
    }
    if let Some(marker) = content.windows(MARKER.len()).position(|w| w == MARKER) {
        let start = marker + MARKER.len();
        let end = content[start..]
            .windows(END.len())
            .rposition(|w| w == END)
            .map(|n| start + n);
        let query = String::from_utf8_lossy(&content[start..end.unwrap_or(content_end)]);
        output.extend_from_slice(&content[..start - b"query=\"".len()]);
        // Legacy logs hold only the preview: its full prompt length is unknown.
        output.extend_from_slice(query_fields(salt, &query).as_bytes());
        if let Some(end) = end {
            output.extend_from_slice(&content[end + 1..]);
        }
    } else {
        output.extend_from_slice(content);
    }
    output.extend_from_slice(&record[content_end..]);
}

/// How long to wait before the second look at the old file for lines a hook
/// appended through a descriptor it opened before the rename.
const TAIL_SETTLE: Duration = Duration::from_millis(20);

fn scrub_file(path: &Path, salt: Option<&[u8]>) -> io::Result<()> {
    scrub_file_with(path, salt, || {})
}

/// `before_rename` runs after the scrubbed copy is written and before the swap;
/// tests use it to play the part of a hook writing at that moment.
fn scrub_file_with(
    path: &Path,
    salt: Option<&[u8]>,
    before_rename: impl FnOnce(),
) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        // The writer follows a symlink, so the target would stay the live log.
        // Report it so the completion flag stays unset.
        Ok(_) => {
            return Err(io::Error::other(format!(
                "timing log is not a regular file: {}",
                path.display()
            )))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW);
    let mut old = options.open(path)?;
    let mut input = Vec::new();
    old.read_to_end(&mut input)?;
    let output = scrub_bytes(&input, salt);
    if output == input {
        crate::private_files::set_mode(path, 0o600)?;
        return Ok(());
    }
    let temp = path.with_extension(format!("scrub-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        file.write_all(&output)?;
        file.sync_all()?;
        before_rename();
        // A hook that rotated the log meanwhile owns a new file at this path;
        // renaming over it would drop its lines.
        let current = fs::symlink_metadata(path)?;
        if !current.file_type().is_file() || !same_file(&current, &old.metadata()?) {
            return Err(io::Error::other(format!(
                "timing log was replaced during scrub: {}",
                path.display()
            )));
        }
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return result;
    }
    recover_tail(&mut old, input.len() as u64, path, salt)
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

/// Lines a hook appended to the old file after it was read, and before or just
/// after the swap, sit past `offset` in the replaced file. Copy them, scrubbed,
/// to the end of the new one. A writer that keeps an old descriptor open past
/// the second look is still lost, and recovered lines may land after newer ones.
fn recover_tail(
    old: &mut fs::File,
    mut offset: u64,
    path: &Path,
    salt: Option<&[u8]>,
) -> io::Result<()> {
    for pass in 0..2 {
        if pass > 0 {
            std::thread::sleep(TAIL_SETTLE);
        }
        old.seek(SeekFrom::Start(offset))?;
        let mut tail = Vec::new();
        old.read_to_end(&mut tail)?;
        if tail.is_empty() {
            continue;
        }
        offset += tail.len() as u64;
        let scrubbed = scrub_bytes(&tail, salt);
        crate::private_files::append_private(path)?.write_all(&scrubbed)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_writer_timestamps_inside_queries_do_not_split_records() {
        for pasted in [
            "2026-11-09T23:02:00.123Z",
            "2026-11-09T",
            "2026-11-09Txx:02:00Z",
            "2026-11-09T23:xx:00Z",
            "2026-11-09T23:02:xxZ",
            "2026-11-09T23-02:00Z",
            "2026-11-09T23:02-00Z",
            "2026-11-09T23:02:00z",
            "2026-11-09T23:02:00Z\t",
        ] {
            let query = format!("secret\n{pasted} pasted secret\nprompt tail");
            let input = format!(
                "2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"{query}\" items=1\n2026-11-09T23:02:01Z CSR hook total=1ms\n"
            );
            let expected = format!(
                "2026-11-09T23:02:00Z CSR prompt-submit inject: {} items=1\n2026-11-09T23:02:01Z CSR hook total=1ms\n",
                query_fields(Some(b"salt"), &query)
            );
            assert_eq!(
                scrub_bytes(input.as_bytes(), Some(b"salt")),
                expected.as_bytes(),
                "pasted timestamp: {pasted:?}"
            );
        }
    }

    #[test]
    fn exact_timestamp_and_space_inside_a_query_does_not_split_the_record() {
        // A pasted line shaped like the writer's prefix, without `CSR `, must
        // stay inside the record so its text is redacted with the rest.
        for pasted in [
            "2026-11-09T23:02:00Z secret",
            "2026-11-09T23:02:00Z CSRx secret",
            "2026-11-09T23:02:00Z ",
        ] {
            let query = format!("first\n{pasted}\nprompt tail");
            let input = format!(
                "2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"{query}\" items=1\n2026-11-09T23:02:01Z CSR hook total=1ms\n"
            );
            let expected = format!(
                "2026-11-09T23:02:00Z CSR prompt-submit inject: {} items=1\n2026-11-09T23:02:01Z CSR hook total=1ms\n",
                query_fields(Some(b"salt"), &query)
            );
            let out = scrub_bytes(input.as_bytes(), Some(b"salt"));
            assert_eq!(out, expected.as_bytes(), "pasted line: {pasted:?}");
            assert!(!String::from_utf8_lossy(&out).contains("secret"));
        }
    }

    #[test]
    fn already_flagged_installs_rescrub_under_the_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        storage.set_meta("timing_query_scrub_v2", "1").unwrap();
        let log = dir.path().join("hook-timing.log");
        fs::write(
            &log,
            b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"a\n2026-11-09T23:02:00Z secret\" items=1\n",
        )
        .unwrap();
        scrub_paths_once(&storage, &log, None).unwrap();
        let text = fs::read_to_string(&log).unwrap();
        assert!(
            !text.contains("query=") && !text.contains("secret"),
            "{text}"
        );
        assert_eq!(storage.get_meta(META_KEY).unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn session_start_marker_inside_a_query_does_not_bypass_redaction() {
        let query = "secret session-start inject [x]: stories=1 preview=\"private tail";
        for prefix in ["2026-11-09T23:02:00Z ", ""] {
            let input =
                format!("{prefix}CSR prompt-submit inject: query=\"{query}\" items=1 anti=0\n");
            let expected = format!(
                "{prefix}CSR prompt-submit inject: {} items=1 anti=0\n",
                query_fields(Some(b"salt"), query)
            );
            assert_eq!(
                scrub_bytes(input.as_bytes(), Some(b"salt")),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn live_log_failure_still_scrubs_rotated_log_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        let live_dir = dir.path().join("live");
        let log = live_dir.join("hook-timing.log");
        let rotated = dir.path().join("hook-timing.log.1");
        let input = b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"secret\" items=1\n";
        // A regular file as the parent makes reading the live path fail even as root.
        fs::write(&live_dir, b"not a directory").unwrap();
        fs::write(&rotated, input).unwrap();
        assert!(scrub_paths_once(&storage, &log, Some(&rotated)).is_err());
        let scrubbed_rotated = fs::read(&rotated).unwrap();
        assert_eq!(
            scrubbed_rotated,
            b"2026-11-09T23:02:00Z CSR prompt-submit inject: q_len=6 items=1\n"
        );
        assert_eq!(storage.get_meta(META_KEY).unwrap(), None);

        fs::remove_file(&live_dir).unwrap();
        fs::create_dir(&live_dir).unwrap();
        fs::write(&log, input).unwrap();
        scrub_paths_once(&storage, &log, Some(&rotated)).unwrap();
        let salt = load_salt(&log).unwrap();
        assert_eq!(
            fs::read_to_string(&log).unwrap(),
            format!(
                "2026-11-09T23:02:00Z CSR prompt-submit inject: {} items=1\n",
                query_fields(Some(&salt), "secret")
            )
        );
        assert_eq!(fs::read(&rotated).unwrap(), scrubbed_rotated);
        assert_eq!(storage.get_meta(META_KEY).unwrap().as_deref(), Some("1"));
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    #[test]
    fn failed_pass_is_retried_and_each_failure_is_logged() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        let live_dir = dir.path().join("live");
        let log = live_dir.join("hook-timing.log");
        let input = b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"secret\" items=1\n";
        // A regular file as the parent makes the first pass fail.
        fs::write(&live_dir, b"not a directory").unwrap();
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish();
        let mut calls = 0;
        let ok = tracing::subscriber::with_default(subscriber, || {
            scrub_with_retry(3, Duration::ZERO, || {
                calls += 1;
                if calls == 2 {
                    // The obstacle clears between attempts, as a busy log does.
                    fs::remove_file(&live_dir).unwrap();
                    fs::create_dir(&live_dir).unwrap();
                    fs::write(&log, input).unwrap();
                }
                scrub_paths_once(&storage, &log, None)
            })
        });
        assert!(ok);
        assert_eq!(calls, 2);
        assert_eq!(storage.get_meta(META_KEY).unwrap().as_deref(), Some("1"));
        let text = fs::read_to_string(&log).unwrap();
        assert!(!text.contains("query=") && text.contains("q_len=6"));
        let warnings = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert_eq!(warnings.matches("WARN").count(), 1, "{warnings}");
        assert!(warnings.contains("timing log scrub failed, retrying"));
        assert!(!warnings.contains("secret"));
    }

    #[test]
    fn exhausted_retries_warn_and_leave_the_flag_unset() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        let live_dir = dir.path().join("live");
        let log = live_dir.join("hook-timing.log");
        fs::write(&live_dir, b"not a directory").unwrap();
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish();
        let mut calls = 0;
        let ok = tracing::subscriber::with_default(subscriber, || {
            scrub_with_retry(3, Duration::ZERO, || {
                calls += 1;
                scrub_paths_once(&storage, &log, None)
            })
        });
        assert!(!ok);
        assert_eq!(calls, 3);
        assert_eq!(storage.get_meta(META_KEY).unwrap(), None);
        let warnings = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert_eq!(warnings.matches("WARN").count(), 3, "{warnings}");
        assert!(warnings.contains("until the next daemon start"));
    }

    #[test]
    fn scrub_preserves_all_other_bytes_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("hook-timing.log");
        let salt = load_salt(&log).unwrap();
        assert_eq!(salt, load_salt(&log).unwrap());
        let before = b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"secret\nsecond \"quote\"\" items=2 anti=0 ancestry=1 stdout=300B top=[0.5/chunk]\n2026-11-09T23:02:01Z CSR hook total=123ms\n";
        let expected = format!(
            "2026-11-09T23:02:00Z CSR prompt-submit inject: {} items=2 anti=0 ancestry=1 stdout=300B top=[0.5/chunk]\n2026-11-09T23:02:01Z CSR hook total=123ms\n",
            query_fields(Some(&salt), "secret\nsecond \"quote\"")
        );
        for path in [&log, &log.with_extension("log.1")] {
            fs::write(path, before).unwrap();
            scrub_file(path, Some(&salt)).unwrap();
            let after = fs::read(path).unwrap();
            assert_eq!(after, expected.as_bytes());
            assert!(!after.windows(6).any(|w| w == b"query="));
            let modified = fs::metadata(path).unwrap().modified().unwrap();
            scrub_file(path, Some(&salt)).unwrap();
            assert_eq!(fs::read(path).unwrap(), after);
            assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::metadata(log.with_file_name("hook-timing.salt"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn lines_appended_before_the_swap_survive_once_and_are_scrubbed() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("hook-timing.log");
        let salt = load_salt(&log).unwrap();
        fs::write(
            &log,
            b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"secret\" items=1\n",
        )
        .unwrap();
        // One hook already holds an append descriptor, another opens the path
        // after the scrubbed copy is written and before it replaces the log.
        let mut held = crate::private_files::append_private(&log).unwrap();
        scrub_file_with(&log, Some(&salt), || {
            held.write_all(b"2026-11-09T23:02:01Z CSR hook total=5ms\n")
                .unwrap();
            crate::private_files::append_private(&log)
                .unwrap()
                .write_all(
                    b"2026-11-09T23:02:02Z CSR prompt-submit inject: query=\"late\" items=1\n",
                )
                .unwrap();
        })
        .unwrap();
        let expected = format!(
            "2026-11-09T23:02:00Z CSR prompt-submit inject: {} items=1\n2026-11-09T23:02:01Z CSR hook total=5ms\n2026-11-09T23:02:02Z CSR prompt-submit inject: {} items=1\n",
            query_fields(Some(&salt), "secret"),
            query_fields(Some(&salt), "late")
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), expected);
    }

    #[test]
    fn rotation_during_the_scrub_is_reported_and_the_new_file_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("hook-timing.log");
        let rotated = dir.path().join("hook-timing.log.1");
        fs::write(
            &log,
            b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"secret\" items=1\n",
        )
        .unwrap();
        let fresh = b"2026-11-09T23:02:01Z CSR hook total=9ms\n";
        let error = scrub_file_with(&log, Some(b"salt"), || {
            fs::rename(&log, &rotated).unwrap();
            fs::write(&log, fresh).unwrap();
        })
        .unwrap_err();
        assert!(error.to_string().contains("replaced during scrub"));
        assert_eq!(fs::read(&log).unwrap(), fresh);
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains(".scrub-")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn meta_guard_is_set_after_both_logs_including_unterminated_records() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        let log = dir.path().join("hook-timing.log");
        let rotated = log.with_extension("log.1");
        let row =
            b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"secret\" items=1 anti=0 stdout=5B top=[]\n";
        fs::write(&log, row).unwrap();
        fs::write(&rotated, b"2026-11-09T23:02:00Z inject: query=\"unfinished").unwrap();
        scrub_paths_once(&storage, &log, Some(&rotated)).unwrap();
        assert_eq!(storage.get_meta(META_KEY).unwrap().as_deref(), Some("1"));
        for path in [&log, &rotated] {
            let text = fs::read_to_string(path).unwrap();
            assert!(!text.contains("query="));
            assert!(text.contains("q_len="));
        }
        // A set flag prevents any further file access, even with a bad salt.
        fs::write(log.with_file_name("hook-timing.salt"), b"bad").unwrap();
        scrub_paths_once(&storage, &log, Some(&rotated)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_left_alone_and_malformed_queries_are_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let log = dir.path().join("hook-timing.log");
        let input = b"2026-11-09T23:02:00Z CSR prompt-submit inject: query=\"unfinished\n";
        fs::write(&target, input).unwrap();
        std::os::unix::fs::symlink(&target, &log).unwrap();
        // The writer follows the link, so the scrub reports it instead of
        // claiming success: the completion flag must stay unset.
        assert!(scrub_file(&log, Some(b"salt")).is_err());
        assert_eq!(fs::read(&target).unwrap(), input);
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        assert!(scrub_paths_once(&storage, &log, None).is_err());
        assert_eq!(storage.get_meta(META_KEY).unwrap(), None);
        let rotated = log.with_extension("log.1");
        std::os::unix::fs::symlink(&target, &rotated).unwrap();
        fs::remove_file(&log).unwrap();
        assert!(scrub_paths_once(&storage, &log, Some(&rotated)).is_err());
        assert_eq!(storage.get_meta(META_KEY).unwrap(), None);
        assert_eq!(fs::read(&target).unwrap(), input);
        fs::remove_file(&rotated).unwrap();
        fs::remove_file(log.with_file_name("hook-timing.salt")).unwrap();
        scrub_file(&target, Some(b"salt")).unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            format!(
                "2026-11-09T23:02:00Z CSR prompt-submit inject: {}\n",
                query_fields(Some(b"salt"), "unfinished")
            )
        );
        std::os::unix::fs::symlink(&target, log.with_file_name("hook-timing.salt")).unwrap();
        assert!(load_salt(&log).is_err());
    }

    #[test]
    fn embedded_items_marker_is_part_of_the_preview() {
        let input = b"2026-11-09T23:02:00Z inject: query=\"secret\" items=trap\nstill secret\" items=2 top=[]\n";
        let expected = format!(
            "2026-11-09T23:02:00Z inject: {} items=2 top=[]\n",
            query_fields(Some(b"salt"), "secret\" items=trap\nstill secret")
        );
        assert_eq!(scrub_bytes(input, Some(b"salt")), expected.as_bytes());
    }

    #[test]
    fn unterminated_record_cannot_consume_a_later_records_items_marker() {
        let later = b"2026-11-09T23:02:01Z CSR diagnostic=\"safe\" items=7\n";
        let mut input = b"2026-11-09T23:02:00Z inject: query=\"secret\n".to_vec();
        input.extend_from_slice(later);
        let mut expected = b"2026-11-09T23:02:00Z inject: q_len=6\n".to_vec();
        expected.extend_from_slice(later);
        assert_eq!(scrub_bytes(&input, None), expected);
    }

    #[test]
    fn invalid_utf8_and_preamble_are_scrubbed_lossily() {
        let input = b"inject: query=\"\xff\n2026-11-09T23:02:00Z CSR unchanged=\xff\n";
        let expected = b"inject: q_len=1\n2026-11-09T23:02:00Z CSR unchanged=\xff\n";
        assert_eq!(scrub_bytes(input, None), expected);
    }

    #[test]
    fn truncated_salt_is_repaired_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("hook-timing.log");
        let path = log.with_file_name("hook-timing.salt");
        fs::write(&path, b"partial").unwrap();
        let salt = load_salt(&log).unwrap();
        assert_eq!(salt.len(), 16);
        assert_eq!(fs::read(&path).unwrap(), salt);
        assert_eq!(load_salt(&log).unwrap(), salt);
    }

    #[test]
    fn unusable_salt_still_scrubs_both_logs_and_sets_flag() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("test.db")).unwrap();
        let log = dir.path().join("hook-timing.log");
        let rotated = log.with_extension("log.1");
        fs::create_dir(log.with_file_name("hook-timing.salt")).unwrap();
        let input = b"2026-11-09T23:02:00Z CSR inject: query=\"secret\" items=1\n2026-11-09T23:02:01Z CSR inject: query=\"unfinished\n";
        for path in [&log, &rotated] {
            fs::write(path, input).unwrap();
        }
        scrub_paths_once(&storage, &log, Some(&rotated)).unwrap();
        for path in [&log, &rotated] {
            assert_eq!(fs::read(path).unwrap(), b"2026-11-09T23:02:00Z CSR inject: q_len=6 items=1\n2026-11-09T23:02:01Z CSR inject: q_len=10\n");
        }
        assert_eq!(storage.get_meta(META_KEY).unwrap().as_deref(), Some("1"));
    }

    #[test]
    fn session_start_preview_is_removed_without_changing_later_records() {
        let input = b"2026-11-09T23:02:00Z CSR session-start inject [project]: stories=2 sessions=1 stdout=42B preview=\"secret | inject: query=\"nested\"\n2026-11-09T23:02:01Z CSR hook total=1ms\n";
        let expected = b"2026-11-09T23:02:00Z CSR session-start inject [project]: stories=2 sessions=1 stdout=42B\n2026-11-09T23:02:01Z CSR hook total=1ms\n";
        assert_eq!(scrub_bytes(input, Some(b"salt")), expected);
    }
}
