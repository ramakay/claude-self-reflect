//! Claude's status line, moved into the CSR pane while one is open beside the session.
//!
//! `csr-engine companion statusline --sh '<command>'` (one shell string) or
//! `csr-engine companion statusline <command> [args...]` (argv, no shell) runs the real status
//! command with the status JSON Claude Code sent on stdin. While a pane's pid file names this
//! session and that pid is alive, the line is kept for the pane to draw and nothing is printed;
//! otherwise the line passes through untouched. Beside the line it keeps the few raw fields the
//! pane's masthead draws. This is the port of `plugins/csr-companion/bin/statusline.cjs`.
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions, Permissions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The pane touches its pid file every couple of seconds; one untouched this long names a pane
/// that is gone, whatever process now holds its old pid.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// How long the wrapped command may run before it is killed.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The status JSON's fields the pane's masthead draws, in the order they are kept.
const FIELDS: [&str; 3] = ["prompt_cache", "rate_limits", "context_window"];

#[derive(clap::Args, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
pub struct StatuslineArgs {
    /// Run the words after it, joined by one space, as one shell string
    #[arg(long)]
    pub sh: bool,
    /// The real status command and its arguments, passed through untouched
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0..)]
    pub command: Vec<OsString>,
}

/// What the wrapped command did. `stdout` and `stderr` are what it wrote before it ended.
struct Ran {
    stdout: String,
    stderr: String,
    code: Option<i32>,
    /// Why it did not start or ran out of time, worded as the Node wrapper worded it.
    error: Option<String>,
}

/// The data folders a pane may keep its pid files under: the dev mod, then the plugin.
fn keeps(home: &Path) -> [PathBuf; 2] {
    let data = home.join(".claude-self-reflect");
    [data.join("csr-shifts"), data.join("csr-companion")]
}

/// `String(data.session_id || '')` for the values a session id can plausibly be.
fn session_of(data: &Value) -> String {
    match data.get("session_id") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(true)) => "true".into(),
        Some(Value::Number(n)) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(0), ..) => String::new(),
            (Some(i), ..) => i.to_string(),
            (_, Some(u), _) => u.to_string(),
            (_, _, Some(f)) if f.fract() == 0. && f.abs() < 1e21 && f != 0. => format!("{f:.0}"),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// `/^[\w-]+$/` with `\w` ASCII only, as in JavaScript.
fn valid_session(session: &str) -> bool {
    !session.is_empty()
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The status JSON's fields the pane's masthead draws, kept as they came; absent ones stay
/// absent. Like the Node wrapper, anything truthy of type "object" counts, a list included.
fn raw_fields(data: &Value) -> Vec<(&'static str, Value)> {
    FIELDS
        .iter()
        .filter_map(|name| match data.get(*name) {
            Some(value @ (Value::Object(_) | Value::Array(_))) => Some((*name, value.clone())),
            _ => None,
        })
        .collect()
}

/// A pid file's `pid`, as `Number(pid)` read it: a whole number, or text that holds one.
fn pid_of(value: &Value) -> Option<i32> {
    let pid = match value {
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_f64().filter(|f| f.fract() == 0.).map(|f| f as i64))?,
        Value::String(text) => text.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    i32::try_from(pid).ok().filter(|pid| *pid > 0)
}

/// The first keep whose pid file for `session` was touched within the heartbeat and names a
/// live process.
fn pane_keep(keeps: &[PathBuf], session: &str, now: SystemTime) -> Option<PathBuf> {
    keeps.iter().find_map(|keep| {
        let file = keep.join("panes").join(format!("{session}.json"));
        let mtime = fs::metadata(&file).ok()?.modified().ok()?;
        // A time ahead of now is a fresh touch, as in the Node wrapper.
        if now.duration_since(mtime).is_ok_and(|age| age > HEARTBEAT) {
            return None;
        }
        let text = String::from_utf8_lossy(&fs::read(&file).ok()?).into_owned();
        let record: Value = serde_json::from_str(&text).ok()?;
        let pid = pid_of(record.get("pid")?)?;
        // SAFETY: signal 0 only checks that the process exists and may be signalled.
        (unsafe { libc::kill(pid, 0) } == 0).then(|| keep.clone())
    })
}

fn errno_name(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "ENOENT".into(),
        io::ErrorKind::PermissionDenied => "EACCES".into(),
        _ => error.to_string(),
    }
}

/// Reads a pipe to its end on its own thread; the bytes arrive on the returned channel.
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    rx
}

/// Runs the wrapped command with `input` on its stdin and a time limit of `timeout`.
fn run_command(
    program: &OsStr,
    args: &[&OsStr],
    input: Vec<u8>,
    compact: bool,
    timeout: Duration,
) -> Ran {
    let name = program.to_string_lossy().into_owned();
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if compact {
        // For the pane's narrow column the command is asked for its compact line.
        command.env("STATUSLINE_COMPACT", "1");
    }
    let start = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Ran {
                stdout: String::new(),
                stderr: String::new(),
                code: None,
                error: Some(format!("spawnSync {name} {}", errno_name(&error))),
            }
        }
    };
    // The input goes on its own thread so a command that fills its output before it reads
    // cannot deadlock us. A command that exits without reading it (EPIPE) is no error.
    if let Some(mut stdin) = child.stdin.take() {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        });
    }
    let out = drain(child.stdout.take().expect("piped"));
    let err = drain(child.stderr.take().expect("piped"));
    let mut pause = Duration::from_micros(200);
    let (status, error) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), None),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, Some(format!("spawnSync {name} ETIMEDOUT")));
            }
            Ok(None) => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(5));
            }
            Err(error) => {
                break (
                    None,
                    Some(format!("spawnSync {name} {}", errno_name(&error))),
                )
            }
        }
    };
    // What it wrote before it ended. A grandchild that keeps the pipe open past the limit
    // costs a short grace, not the rest of its life.
    let until = (start + timeout).max(Instant::now() + Duration::from_millis(250));
    let collect = |rx: mpsc::Receiver<Vec<u8>>| {
        let wait = until.saturating_duration_since(Instant::now());
        String::from_utf8_lossy(&rx.recv_timeout(wait).unwrap_or_default()).into_owned()
    };
    let stdout = collect(out);
    let stderr = collect(err);
    Ran {
        stdout,
        stderr,
        code: status,
        error,
    }
}

/// The folder and file of the companion's own, never a link: chmod and open would act on the
/// link's target.
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn refuse_link(path: &Path) -> io::Result<()> {
    if is_link(path) {
        return Err(io::Error::other("refusing to follow a symlink"));
    }
    Ok(())
}

/// Writes `<keep>/statusline/<session>.json`, private to the person: 0700 folders, 0600 file.
/// A mode given to mkdir or open counts only for what they create and the umask can narrow
/// it, so each is set after.
fn write_kept(keep: &Path, session: &str, text: &str) -> io::Result<()> {
    let dir = keep.join("statusline");
    refuse_link(&dir)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    fs::set_permissions(keep, Permissions::from_mode(0o700))?;
    fs::set_permissions(&dir, Permissions::from_mode(0o700))?;
    let file = dir.join(format!("{session}.json"));
    refuse_link(&file)?;
    let mut temp = file.clone().into_os_string();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    // One a crash left, or a link planted there: removed itself, never written through.
    let _ = fs::remove_file(&temp);
    let mut handle = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&temp)?;
    let written = (|| {
        handle.set_permissions(Permissions::from_mode(0o600))?;
        handle.write_all(text.as_bytes())
    })();
    drop(handle);
    written.and_then(|()| fs::rename(&temp, &file))
}

/// The record the pane reads: `{"at": ..., "line": ..., "fields": {...}}`, credentials out.
fn kept_record(line: &str, fields: Vec<(&'static str, Value)>, at: f64) -> String {
    let mut parts = Vec::with_capacity(fields.len());
    for (name, mut value) in fields {
        super::private::redact_value(&mut value);
        parts.push(format!("\"{name}\":{value}"));
    }
    format!(
        "{{\"at\":{},\"line\":{},\"fields\":{{{}}}}}",
        Value::from(at),
        Value::from(crate::redact::redact_private(line).into_owned()),
        parts.join(",")
    )
}

pub fn run(args: &StatuslineArgs) -> i32 {
    let mut input = Vec::new();
    let _ = io::stdin().read_to_end(&mut input);
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let now = SystemTime::now();
    let (session, fields) = match serde_json::from_slice::<Value>(&input) {
        Ok(data) if data.is_object() => (session_of(&data), raw_fields(&data)),
        _ => (String::new(), Vec::new()),
    };
    let keep = if valid_session(&session) {
        pane_keep(&keeps(&home), &session, now)
    } else {
        None
    };
    let ran = if args.sh {
        // One shell string, run through a shell as Claude Code runs a statusLine command.
        let shell = args
            .command
            .iter()
            .map(|word| word.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        (!shell.is_empty()).then(|| {
            let shell: &OsStr = shell.as_ref();
            let flag: &OsStr = "-c".as_ref();
            run_command(
                "/bin/sh".as_ref(),
                &[flag, shell],
                input,
                keep.is_some(),
                TIMEOUT,
            )
        })
    } else if let Some((program, rest)) = args.command.split_first().filter(|(p, _)| !p.is_empty())
    {
        let rest: Vec<&OsStr> = rest.iter().map(OsString::as_os_str).collect();
        Some(run_command(program, &rest, input, keep.is_some(), TIMEOUT))
    } else {
        None
    };
    let line = ran.as_ref().map(|ran| ran.stdout.as_str()).unwrap_or("");
    // What the wrapped command says on stderr, and why it did not start or ran out of time,
    // reach stderr as they would without this wrapper. A line exits 0: Claude Code shows the
    // status line only after exit 0, so a command that printed its line and then failed must
    // not blank it. With no line, its own status, or 1.
    let mut stderr = io::stderr();
    if let Some(ran) = &ran {
        let _ = stderr.write_all(ran.stderr.as_bytes());
        if let Some(error) = &ran.error {
            let _ = writeln!(stderr, "statusline: {error}");
        }
    }
    let code = match &ran {
        Some(ran) if line.is_empty() => ran.code.unwrap_or(1),
        _ => 0,
    };
    if let Some(keep) = keep {
        let at = now
            .duration_since(UNIX_EPOCH)
            .map_or(0., |d| d.as_secs_f64());
        if write_kept(&keep, &session, &kept_record(line, fields, at)).is_ok() {
            return code; // drawn in the pane
        }
    }
    let mut stdout = io::stdout();
    let _ = stdout.write_all(line.as_bytes());
    let _ = stdout.flush();
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::symlink;

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    fn sh(script: &str, input: &str, compact: bool) -> Ran {
        let flag: &OsStr = "-c".as_ref();
        run_command(
            "/bin/sh".as_ref(),
            &[flag, script.as_ref()],
            input.as_bytes().to_vec(),
            compact,
            Duration::from_secs(5),
        )
    }
    /// A keep with a pane file for `session` naming `pid`.
    fn pane(root: &Path, name: &str, session: &str, pid: i64) -> PathBuf {
        let keep = root.join(".claude-self-reflect").join(name);
        fs::create_dir_all(keep.join("panes")).unwrap();
        let file = keep.join("panes").join(format!("{session}.json"));
        fs::write(&file, json!({ "pid": pid }).to_string()).unwrap();
        keep
    }
    fn mine() -> i64 {
        i64::from(std::process::id())
    }

    #[test]
    fn test_a_session_id_is_a_word_of_letters_digits_underscore_and_dash() {
        for ok in ["s1", "sess-w", "a_b-C9", "0"] {
            assert!(valid_session(ok), "{ok}");
        }
        for bad in [
            "",
            "a b",
            "a/b",
            "../x",
            "é",
            "a.b",
            "a\n",
            "[object Object]",
        ] {
            assert!(!valid_session(bad), "{bad:?}");
        }
    }

    #[test]
    fn test_the_session_is_read_as_javascript_would_make_a_string_of_it() {
        let of = |text: &str| session_of(&serde_json::from_str(text).unwrap());
        assert_eq!(of(r#"{"session_id":"s1"}"#), "s1");
        assert_eq!(of(r#"{"session_id":42}"#), "42");
        assert_eq!(of(r#"{"session_id":7.0}"#), "7");
        assert_eq!(of(r#"{"session_id":true}"#), "true");
        for none in [
            r#"{"session_id":0}"#,
            r#"{"session_id":null}"#,
            r#"{"session_id":false}"#,
        ] {
            assert_eq!(of(none), "");
        }
        assert_eq!(of(r#"{"session_id":{"a":1}}"#), "");
        assert_eq!(of("{}"), "");
    }

    #[test]
    fn test_only_the_three_fields_are_kept_and_only_when_they_are_objects() {
        let data = json!({
            "session_id": "s", "model": {"display_name": "Opus"},
            "prompt_cache": {"warm": true}, "rate_limits": null, "context_window": "odd",
        });
        assert_eq!(
            raw_fields(&data),
            vec![("prompt_cache", json!({"warm": true}))]
        );
        assert!(raw_fields(&json!({"rate_limits": 0, "prompt_cache": ""})).is_empty());
        let all = json!({"context_window": {"a": 1}, "rate_limits": {}, "prompt_cache": {"b": 2}});
        let names: Vec<_> = raw_fields(&all).iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["prompt_cache", "rate_limits", "context_window"]);
    }

    #[test]
    fn test_a_pane_with_a_fresh_pid_file_and_a_live_pid_is_found() {
        let home = tempfile::tempdir().unwrap();
        let keep = pane(home.path(), "csr-companion", "s1", mine());
        let all = keeps(home.path());
        assert_eq!(pane_keep(&all, "s1", SystemTime::now()), Some(keep));
        assert_eq!(pane_keep(&all, "s2", SystemTime::now()), None);
    }

    #[test]
    fn test_a_pid_file_nobody_touched_for_30_seconds_is_a_pane_that_is_gone() {
        let home = tempfile::tempdir().unwrap();
        let keep = pane(home.path(), "csr-companion", "s1", mine());
        let all = keeps(home.path());
        let file = keep.join("panes/s1.json");
        let touched = fs::metadata(&file).unwrap().modified().unwrap();
        assert!(pane_keep(&all, "s1", touched + Duration::from_secs(29)).is_some());
        assert!(pane_keep(&all, "s1", touched + Duration::from_secs(31)).is_none());
        // A touch in the future counts as fresh.
        assert!(pane_keep(&all, "s1", touched - Duration::from_secs(600)).is_some());
    }

    #[test]
    fn test_a_dead_pid_or_a_bad_pid_file_is_no_pane() {
        let home = tempfile::tempdir().unwrap();
        let all = keeps(home.path());
        let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
        let dead = i64::from(gone.id());
        gone.wait().unwrap();
        let keep = pane(home.path(), "csr-companion", "s1", dead);
        assert!(pane_keep(&all, "s1", SystemTime::now()).is_none());
        for body in [
            "",
            "not json",
            "null",
            "{}",
            r#"{"pid":0}"#,
            r#"{"pid":-1}"#,
            r#"{"pid":"x"}"#,
        ] {
            fs::write(keep.join("panes/s1.json"), body).unwrap();
            assert!(pane_keep(&all, "s1", SystemTime::now()).is_none(), "{body}");
        }
        fs::write(
            keep.join("panes/s1.json"),
            format!(r#"{{"pid":"{}"}}"#, mine()),
        )
        .unwrap();
        assert!(pane_keep(&all, "s1", SystemTime::now()).is_some());
    }

    #[test]
    fn test_the_dev_mods_keep_wins_over_the_plugins_when_both_pass() {
        let home = tempfile::tempdir().unwrap();
        pane(home.path(), "csr-companion", "s1", mine());
        let shifts = pane(home.path(), "csr-shifts", "s1", mine());
        assert_eq!(
            pane_keep(&keeps(home.path()), "s1", SystemTime::now()),
            Some(shifts)
        );
    }

    #[test]
    fn test_a_first_keep_that_fails_falls_to_the_second() {
        let home = tempfile::tempdir().unwrap();
        let shifts = pane(home.path(), "csr-shifts", "s1", 2_000_000_000);
        let companion = pane(home.path(), "csr-companion", "s1", mine());
        let found = pane_keep(&keeps(home.path()), "s1", SystemTime::now());
        assert_ne!(found, Some(shifts));
        assert_eq!(found, Some(companion));
    }

    #[test]
    fn test_the_command_gets_the_input_whole_and_the_compact_flag_only_in_a_pane() {
        let ran = sh(
            "cat; echo; echo \"c=$STATUSLINE_COMPACT\"",
            "{\"a\":1}",
            false,
        );
        assert_eq!(
            (ran.stdout.as_str(), ran.code),
            ("{\"a\":1}\nc=\n", Some(0))
        );
        let ran = sh("echo \"c=$STATUSLINE_COMPACT\"", "", true);
        assert_eq!(ran.stdout, "c=1\n");
    }

    #[test]
    fn test_stderr_and_the_exit_code_come_back() {
        let ran = sh("echo line; echo oops >&2; exit 3", "", false);
        assert_eq!(
            (ran.stdout.as_str(), ran.stderr.as_str(), ran.code),
            ("line\n", "oops\n", Some(3))
        );
        assert!(ran.error.is_none());
    }

    #[test]
    fn test_a_command_that_never_reads_a_large_input_adds_no_error() {
        let big = "x".repeat(4_000_000);
        let ran = sh("echo line", &big, false);
        assert_eq!(
            (ran.stdout.as_str(), ran.stderr.as_str(), ran.code),
            ("line\n", "", Some(0))
        );
        assert!(ran.error.is_none());
    }

    #[test]
    fn test_a_command_that_cannot_start_says_why() {
        let program: &OsStr = "/nonexistent/csr-statusline-command".as_ref();
        let ran = run_command(program, &[], Vec::new(), false, TIMEOUT);
        assert_eq!(ran.code, None);
        assert_eq!(
            ran.error.as_deref(),
            Some("spawnSync /nonexistent/csr-statusline-command ENOENT")
        );
    }

    #[test]
    fn test_a_command_that_runs_out_of_time_is_killed_and_keeps_what_it_said() {
        let start = Instant::now();
        let flag: &OsStr = "-c".as_ref();
        let script: &OsStr = "echo early; exec sleep 30".as_ref();
        let ran = run_command(
            "/bin/sh".as_ref(),
            &[flag, script],
            Vec::new(),
            false,
            Duration::from_millis(300),
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(ran.error.as_deref(), Some("spawnSync /bin/sh ETIMEDOUT"));
        assert_eq!((ran.stdout.as_str(), ran.code), ("early\n", None));
    }

    #[test]
    fn test_the_kept_record_has_its_keys_in_order_and_no_credential() {
        let fields = vec![(
            "prompt_cache",
            json!({"warm": true, "token": "x", "note": "token=abc123"}),
        )];
        let text = kept_record("ctx 40% token=abc123 Bearer abc123def456", fields, 12.5);
        assert!(text.starts_with(
            "{\"at\":12.5,\"line\":\"ctx 40% token=[redacted] Bearer [redacted]\",\"fields\":{"
        ));
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["fields"],
            json!({"prompt_cache": {
                "warm": true, "token": "[redacted]", "note": "token=[redacted]"
            }})
        );
        assert_eq!(
            kept_record("", Vec::new(), 1.0),
            r#"{"at":1.0,"line":"","fields":{}}"#
        );
    }

    #[test]
    fn test_the_kept_file_is_0600_in_0700_folders_and_an_old_one_is_tightened() {
        let home = tempfile::tempdir().unwrap();
        let keep = home.path().join("keep");
        fs::create_dir_all(keep.join("statusline")).unwrap();
        let old = keep.join("statusline/s1.json");
        fs::write(&old, "{}").unwrap();
        for path in [&keep, &keep.join("statusline")] {
            fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
        }
        fs::set_permissions(&old, Permissions::from_mode(0o644)).unwrap();
        write_kept(&keep, "s1", "{\"x\":1}").unwrap();
        assert_eq!(
            [mode(&keep), mode(&keep.join("statusline")), mode(&old)],
            [0o700, 0o700, 0o600]
        );
        assert_eq!(fs::read_to_string(&old).unwrap(), "{\"x\":1}");
        assert_eq!(fs::read_dir(keep.join("statusline")).unwrap().count(), 1);
    }

    #[test]
    fn test_a_link_for_the_folder_the_file_or_the_temp_is_never_written_through() {
        let home = tempfile::tempdir().unwrap();
        let outside = home.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, Permissions::from_mode(0o755)).unwrap();
        let keep = home.path().join("keep");
        fs::create_dir(&keep).unwrap();
        symlink(&outside, keep.join("statusline")).unwrap();
        assert!(write_kept(&keep, "s3", "{}").is_err());
        assert_eq!(
            (fs::read_dir(&outside).unwrap().count(), mode(&outside)),
            (0, 0o755)
        );
        fs::remove_file(keep.join("statusline")).unwrap();
        fs::create_dir(keep.join("statusline")).unwrap();
        let target = outside.join("target");
        fs::write(&target, "keep").unwrap();
        symlink(&target, keep.join("statusline/s3.json")).unwrap();
        assert!(write_kept(&keep, "s3", "{}").is_err());
        fs::remove_file(keep.join("statusline/s3.json")).unwrap();
        symlink(&target, keep.join("statusline/s3.json.tmp")).unwrap();
        write_kept(&keep, "s3", "new").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "keep");
        assert_eq!(
            fs::read_to_string(keep.join("statusline/s3.json")).unwrap(),
            "new"
        );
    }
}
