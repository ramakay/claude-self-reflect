//! Shared logic for AI-narrative `claude -p` invocations (session briefing +
//! session story): opt-out gate, model fallback chain, JSON result parsing,
//! and a persistence-safe content hash.
//!
//! The two call sites keep their own process plumbing (sync vs tokio); only
//! the pure decision/parsing logic lives here.

use serde_json::Value;

/// Kill switch: user disabled all AI-narrative generation.
pub fn narratives_disabled() -> bool {
    std::env::var("CSR_NO_AI_NARRATIVES")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Kill switch: user disabled ratification enrichment.
pub fn ratification_disabled() -> bool {
    std::env::var("CSR_NO_RATIFICATION")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Model candidates in preference order. `None` means omit `--model` and let
/// the claude CLI pick its default — the last-resort path if every Haiku-family
/// alias is decommissioned.
pub fn model_candidates() -> Vec<Option<String>> {
    let mut chain = Vec::with_capacity(3);
    if let Ok(m) = std::env::var("CSR_NARRATIVE_MODEL") {
        let m = m.trim().to_string();
        if !m.is_empty() {
            chain.push(Some(m));
        }
    }
    chain.push(Some("haiku".to_string()));
    chain.push(None);
    chain
}

/// Opt-out for [`isolation_args`]: keep loading the user's settings files in
/// headless children. For installs whose credentials live there (`apiKeyHelper`,
/// an `env` block selecting Bedrock or Vertex); hooks are still switched off.
fn headless_keeps_user_settings() -> bool {
    std::env::var("CSR_HEADLESS_USER_SETTINGS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Argv that cuts a headless `claude -p` child off from the machine's Claude
/// Code setup. Every CSR spawn site passes it.
///
/// * `--setting-sources ""` loads no user, project or local settings, so the
///   child runs none of the user's plugins and none of their SessionStart
///   hooks. Measured 2026-09-19 on one machine, same Haiku call: 7.1s and
///   8,741 prompt tokens with settings loaded, 1.5s and 6,336 without. A
///   month of ratification children had fired one plugin's SessionStart hook
///   21,116 times.
/// * `--no-session-persistence` writes no transcript. Those transcripts were
///   14,186 files in one project directory, re-imported by the watcher as if
///   they were conversations, and 19,852 of 20,159 re-ranker harvest sources.
///
/// Both are checked against `claude --help` once per process (~55ms): a CLI
/// that predates an option rejects it outright, which would fail every call.
pub fn isolation_args() -> Vec<String> {
    static ARGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    if let Some(args) = ARGS.get() {
        return args.clone();
    }
    let mut probe = std::process::Command::new("claude");
    probe.arg("--help");
    match run_with_deadline(probe, HELP_PROBE_DEADLINE) {
        Some(stdout) => ARGS
            .get_or_init(|| {
                isolation_args_for(
                    &String::from_utf8_lossy(&stdout),
                    headless_keeps_user_settings(),
                )
            })
            .clone(),
        // A hung, missing or failing CLI: nothing is learnt, so nothing is
        // cached; the next call probes again.
        None => isolation_args_for("", headless_keeps_user_settings()),
    }
}

/// How long `claude --help` may run before it is killed.
const HELP_PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Run `cmd` (stdin closed) and return its stdout if it exits successfully
/// within `deadline`. On timeout the child is killed AND reaped before this
/// returns, so no process outlives the call. Stdout is drained on its own
/// thread so a full pipe cannot stall the child and with it the deadline poll.
pub(crate) fn run_with_deadline(
    mut cmd: std::process::Command,
    deadline: std::time::Duration,
) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        pipe.read_to_end(&mut buf).ok().map(|_| buf)
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // After a kill the reader is not joined: a grandchild holding the pipe
    // would stall the caller past its deadline. The thread ends with the pipe.
    let status = status?;
    let output = reader.join().ok().flatten();
    status.success().then_some(output).flatten()
}

/// How an async headless run ended.
#[derive(Debug)]
pub enum HeadlessRun {
    Output(std::process::Output),
    /// The deadline passed: the child was killed and reaped.
    TimedOut,
    /// The program could not be started.
    SpawnFailed,
}

/// Run `cmd` with `stdin_bytes` on its stdin, bounded by `deadline` for the
/// whole exchange (writing, reading, exiting). On timeout the child is killed
/// and reaped BEFORE this returns: a caller holding a lock around the call
/// never releases it while the child lives, and a dropped future also kills it.
pub async fn run_headless(
    mut cmd: tokio::process::Command,
    stdin_bytes: &[u8],
    deadline: std::time::Duration,
) -> HeadlessRun {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut child = match cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Piped intentionally: model-not-found detection reads stderr.
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return HeadlessRun::SpawnFailed,
    };
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let exchange = async {
        let write = async {
            if let Some(mut pipe) = stdin.take() {
                let _ = pipe.write_all(stdin_bytes).await;
            }
        };
        let read_out = async {
            let mut buf = Vec::new();
            if let Some(pipe) = stdout.as_mut() {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            buf
        };
        let read_err = async {
            let mut buf = Vec::new();
            if let Some(pipe) = stderr.as_mut() {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            buf
        };
        let (_, out, err) = tokio::join!(write, read_out, read_err);
        let status = child.wait().await.ok()?;
        Some(std::process::Output {
            status,
            stdout: out,
            stderr: err,
        })
    };
    match tokio::time::timeout(deadline, exchange).await {
        Ok(Some(output)) => HeadlessRun::Output(output),
        Ok(None) => {
            let _ = child.kill().await;
            HeadlessRun::TimedOut
        }
        Err(_) => {
            // `kill` signals and then waits: the child is reaped on return.
            let _ = child.kill().await;
            HeadlessRun::TimedOut
        }
    }
}

/// [`isolation_args`] for a given `claude --help` text. No option is
/// variadic, so the result can sit anywhere before `--mcp-config`.
fn isolation_args_for(help: &str, keep_user_settings: bool) -> Vec<String> {
    let mut args = Vec::new();
    if keep_user_settings {
        if help.contains("--settings ") {
            args.push("--settings".to_string());
            args.push(r#"{"disableAllHooks":true}"#.to_string());
        }
    } else if help.contains("--setting-sources") {
        args.push("--setting-sources".to_string());
        args.push(String::new());
    }
    if help.contains("--no-session-persistence") {
        args.push("--no-session-persistence".to_string());
    }
    args
}

/// Path to an EMPTY MCP config, for use with `--strict-mcp-config` so a
/// `claude -p` subprocess loads ZERO MCP servers.
///
/// Every headless `claude -p` invocation inherits the user's full MCP
/// configuration and serialises every tool schema into the request. That is not
/// a small overhead: on a machine with a typical plugin/connector set it is
/// ~180k tokens of tool definitions before the prompt is even considered, which
/// overruns the model's context window and fails the request with HTTP 400
/// `prompt_too_long` — deterministically, before any inference happens, and
/// therefore on every retry.
///
/// Every CSR subprocess call site embeds what the model needs directly in the
/// prompt, so all of them need ZERO tools. Passing this alongside
/// `--strict-mcp-config` also avoids spawning a recursive csr-engine MCP server
/// and gives the fastest possible `claude -p` startup.
pub fn minimal_mcp_config() -> anyhow::Result<std::path::PathBuf> {
    let dir = dirs::home_dir()
        .map(|h| h.join(".claude-self-reflect"))
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    minimal_mcp_config_in(&dir)
}

fn minimal_mcp_config_in(dir: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let config = serde_json::json!({ "mcpServers": {} });
    std::fs::create_dir_all(dir).ok();
    let path = dir.join("briefing-mcp.json");

    // Atomic write. All three call sites can run concurrently, so a plain
    // truncating write would let one caller observe a 0-byte config while
    // another rewrites it — failing that subprocess before inference, which is
    // the exact class of silent failure this config exists to prevent.
    //
    // The temp name carries a counter as well as the pid: the daemon drives
    // ratification and story generation from a single process, so a shared pid
    // is precisely the case that needs distinguishing. (Sibling call sites in
    // this repo use a fixed `.tmp` name; they have one writer each.)
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let tmp_path = dir.join(format!("briefing-mcp.json.{pid}.{seq}.tmp"));
    std::fs::write(&tmp_path, serde_json::to_string(&config)?)?;
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(path)
}

/// Distinguishes temp files written concurrently by `minimal_mcp_config`.
static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug)]
pub struct ParsedNarrative {
    pub text: String,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
}

/// Parse `claude -p --output-format json` stdout. Returns None on error
/// results, missing text, or unparseable output — callers treat None as
/// "no narrative this time", never as a hook failure.
pub fn parse_claude_json(stdout: &str) -> Option<ParsedNarrative> {
    let v: Value = serde_json::from_str(stdout.trim()).ok()?;
    if v.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let text = v.get("result")?.as_str()?.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let usage = v.get("usage");
    let get = |key: &str| -> i64 {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    let model = v
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|m| m.keys().next().cloned())
        .unwrap_or_else(|| "unknown".to_string());
    Some(ParsedNarrative {
        text,
        model,
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cache_read_tokens: get("cache_read_input_tokens"),
        cache_creation_tokens: get("cache_creation_input_tokens"),
    })
}

/// Heuristic over stderr (or an error-JSON body): did this invocation fail
/// because the requested model does not exist? Only then do we walk to the
/// next candidate — rate limits and network errors must NOT burn retries
/// across the whole chain.
///
/// Real claude CLI 404 wording (verified 2026-07-11 research probe): exit=1,
/// JSON is_error:true, api_error_status:404, result text "There's an issue
/// with the selected model (X). It may not exist or you may not have access
/// to it. Run --model to pick a different model." — contains NONE of
/// "not found"/"invalid"/"unknown", so a naive substring check on those
/// words alone misses the real-world case. Match the real wording plus
/// defensive legacy variants.
pub fn is_model_not_found(stderr_or_json: &str) -> bool {
    let s = stderr_or_json.to_lowercase();
    (s.contains("issue with the selected model") || s.contains("may not exist"))
        || (s.contains("\"api_error_status\":404") && s.contains("model"))
        || (s.contains("model") && (s.contains("not found") || s.contains("invalid model")))
}

/// Outcome of one `claude -p` attempt. Decides whether the model chain
/// walks to the next candidate (ModelNotFound) or stops (Failed).
#[derive(Debug)]
pub enum AttemptOutcome {
    /// Exit success and stdout parsed as a narrative result.
    Parsed(ParsedNarrative),
    /// The requested model does not exist / is inaccessible — walk the chain.
    ModelNotFound,
    /// Any other failure (rate limit, network, garbage output) — stop; never
    /// burn remaining candidates on non-model errors.
    Failed(String),
}

pub fn classify_attempt(status_success: bool, stdout: &str, stderr: &str) -> AttemptOutcome {
    if status_success {
        match parse_claude_json(stdout) {
            Some(p) => AttemptOutcome::Parsed(p),
            None if is_model_not_found(stdout) => AttemptOutcome::ModelNotFound,
            None => AttemptOutcome::Failed("claude -p returned unparseable/error JSON".to_string()),
        }
    } else if is_model_not_found(stderr) || is_model_not_found(stdout) {
        AttemptOutcome::ModelNotFound
    } else {
        AttemptOutcome::Failed(format!(
            "claude -p failed: {}",
            if stderr.is_empty() { stdout } else { stderr }
        ))
    }
}

/// FNV-1a 64-bit. Deterministic across processes and versions (unlike
/// std's DefaultHasher), so the digest can be persisted in the meta table.
pub fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP_CURRENT: &str = "  --no-session-persistence   Disable session persistence\n  --setting-sources <sources>   Comma-separated list\n  --settings <file-or-json>   Path to a settings JSON file\n";

    #[test]
    fn isolation_drops_settings_and_the_transcript() {
        assert_eq!(
            isolation_args_for(HELP_CURRENT, false),
            ["--setting-sources", "", "--no-session-persistence"]
        );
    }

    #[test]
    fn isolation_never_passes_an_option_the_cli_does_not_list() {
        // An unknown option fails the whole call, so an old CLI (or no CLI:
        // empty help) gets nothing rather than a guess.
        assert!(isolation_args_for("", false).is_empty());
        assert!(isolation_args_for("", true).is_empty());
        assert_eq!(
            isolation_args_for("  --setting-sources <sources>\n", false),
            ["--setting-sources", ""]
        );
        // `--setting-sources` must not be mistaken for `--settings`.
        assert!(isolation_args_for("  --setting-sources <sources>\n", true).is_empty());
    }

    #[test]
    fn keeping_user_settings_still_switches_hooks_off() {
        assert_eq!(
            isolation_args_for(HELP_CURRENT, true),
            [
                "--settings",
                r#"{"disableAllHooks":true}"#,
                "--no-session-persistence"
            ]
        );
    }

    const FIXTURE: &str = include_str!("../tests/fixtures/claude_p_result.json");

    #[test]
    fn test_parse_real_fixture() {
        let p = parse_claude_json(FIXTURE).expect("fixture must parse");
        assert!(!p.text.is_empty());
        assert!(p.input_tokens > 0);
        assert!(p.output_tokens > 0);
        assert_ne!(p.model, ""); // "unknown" acceptable, empty is not
    }

    #[test]
    fn test_parse_rejects_error_result() {
        let json = r#"{"is_error": true, "result": "boom", "usage": {"input_tokens": 1, "output_tokens": 1}}"#;
        assert!(parse_claude_json(json).is_none());
    }

    #[test]
    fn test_parse_rejects_garbage() {
        assert!(parse_claude_json("not json").is_none());
        assert!(parse_claude_json("{}").is_none());
    }

    #[test]
    fn test_model_candidates() {
        // Merged (no serial_test dep available): sequential assertions avoid
        // races on the process-global CSR_NARRATIVE_MODEL env var.
        std::env::remove_var("CSR_NARRATIVE_MODEL");
        let c = model_candidates();
        assert_eq!(c, vec![Some("haiku".to_string()), None]);

        std::env::set_var("CSR_NARRATIVE_MODEL", "sonnet");
        let c = model_candidates();
        assert_eq!(c[0], Some("sonnet".to_string()));
        assert_eq!(c[1], Some("haiku".to_string()));
        assert_eq!(c[2], None);
        std::env::remove_var("CSR_NARRATIVE_MODEL");
    }

    #[test]
    fn test_is_model_not_found() {
        // Verbatim real CLI 404 wording (Task 1 research probe)
        assert!(is_model_not_found(
            "There's an issue with the selected model (no-such-model-zz9). It may not exist or you may not have access to it."
        ));
        assert!(is_model_not_found(
            r#"{"is_error":true,"api_error_status":404,"result":"There's an issue with the selected model"}"#
        ));
        // Defensive legacy variants
        assert!(is_model_not_found("Error: model 'zz9' not found"));
        assert!(is_model_not_found("invalid model specified"));
        // Must NOT trip on unrelated failures
        assert!(!is_model_not_found("rate limit exceeded"));
        assert!(!is_model_not_found("network error"));
        assert!(!is_model_not_found("Invalid API key"));
    }

    #[test]
    fn test_fnv1a_stable_across_runs() {
        // FNV-1a is deterministic — unlike DefaultHasher (SipHash, random per-process
        // seed), which is why we can persist it in the meta table.
        assert_eq!(fnv1a_64(b"hello"), 0xa430d84680aabd0b);
        assert_ne!(fnv1a_64(b"hello"), fnv1a_64(b"hello "));
    }

    #[test]
    fn test_narratives_disabled_env() {
        // `CSR_NO_AI_NARRATIVES` is also read by `dream::strategy::category_disabled`,
        // whose own tests mutate it under this same shared crate-level guard —
        // without it, `cargo test`'s cross-module thread parallelism can stomp
        // one test's `set_var`/`remove_var` with another's mid-assertion.
        let _g = crate::daemon::dream_cadence::env_test_guard();
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
        assert!(!narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "1");
        assert!(narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "true");
        assert!(narratives_disabled());
        std::env::set_var("CSR_NO_AI_NARRATIVES", "0");
        assert!(!narratives_disabled());
        std::env::remove_var("CSR_NO_AI_NARRATIVES");
    }

    #[test]
    fn test_ratification_disabled_env() {
        std::env::remove_var("CSR_NO_RATIFICATION");
        assert!(!ratification_disabled());
        std::env::set_var("CSR_NO_RATIFICATION", "1");
        assert!(ratification_disabled());
        std::env::set_var("CSR_NO_RATIFICATION", "true");
        assert!(ratification_disabled());
        std::env::set_var("CSR_NO_RATIFICATION", "0");
        assert!(!ratification_disabled());
        std::env::remove_var("CSR_NO_RATIFICATION");
    }

    #[test]
    fn test_classify_attempt_success_fixture() {
        match classify_attempt(true, FIXTURE, "") {
            AttemptOutcome::Parsed(p) => assert!(!p.text.is_empty()),
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_attempt_model_404_json_on_failure() {
        let json = r#"{"is_error":true,"api_error_status":404,"result":"There's an issue with the selected model (zz9). It may not exist or you may not have access to it."}"#;
        assert!(matches!(
            classify_attempt(false, json, ""),
            AttemptOutcome::ModelNotFound
        ));
        assert!(matches!(
            classify_attempt(false, "", json),
            AttemptOutcome::ModelNotFound
        ));
    }

    #[test]
    fn test_classify_attempt_model_404_wording_on_success_status() {
        let json = r#"{"is_error":true,"result":"There's an issue with the selected model (zz9). It may not exist."}"#;
        assert!(matches!(
            classify_attempt(true, json, ""),
            AttemptOutcome::ModelNotFound
        ));
    }

    #[test]
    fn test_classify_attempt_rate_limit_is_failed() {
        assert!(matches!(
            classify_attempt(false, "", "rate limit exceeded"),
            AttemptOutcome::Failed(_)
        ));
    }

    #[test]
    fn test_classify_attempt_network_error_is_failed() {
        assert!(matches!(
            classify_attempt(false, "network error: connection refused", ""),
            AttemptOutcome::Failed(_)
        ));
    }

    #[test]
    fn test_classify_attempt_garbage_on_success_is_failed() {
        assert!(matches!(
            classify_attempt(true, "not json at all", ""),
            AttemptOutcome::Failed(_)
        ));
    }

    #[test]
    fn test_classify_attempt_failed_prefers_stderr() {
        match classify_attempt(false, "stdout noise", "real error") {
            AttemptOutcome::Failed(msg) => assert!(msg.contains("real error")),
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[test]
    fn test_minimal_mcp_config_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = minimal_mcp_config_in(dir.path()).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(
            servers.len(),
            0,
            "claude -p call sites need zero MCP servers"
        );
    }

    #[test]
    fn a_hung_sync_probe_is_killed_and_reaped_at_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("echo $$ > {}; exec sleep 30", pidfile.display()));
        let started = std::time::Instant::now();
        assert!(run_with_deadline(cmd, std::time::Duration::from_millis(300)).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_process_gone(&pidfile);
    }

    #[test]
    fn a_sync_probe_that_finishes_returns_its_stdout() {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("printf '%s' '--setting-sources'");
        assert_eq!(
            run_with_deadline(cmd, std::time::Duration::from_secs(10)).unwrap(),
            b"--setting-sources"
        );
        let mut failing = std::process::Command::new("sh");
        failing.arg("-c").arg("echo x; exit 3");
        assert!(run_with_deadline(failing, std::time::Duration::from_secs(10)).is_none());
    }

    /// `kill -0` succeeds on a zombie, so a failure proves the child was reaped.
    fn assert_process_gone(pidfile: &std::path::Path) {
        let pid = std::fs::read_to_string(pidfile).expect("child wrote its pid");
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "pid {} still exists after the deadline", pid.trim());
    }

    #[tokio::test]
    async fn an_async_narrator_past_its_deadline_is_killed_and_reaped_before_return() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(format!("echo $$ > {}; exec sleep 30", pidfile.display()));
        let started = std::time::Instant::now();
        let run = run_headless(cmd, b"prompt", std::time::Duration::from_millis(400)).await;
        assert!(matches!(run, HeadlessRun::TimedOut), "{run:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_process_gone(&pidfile);
    }

    #[tokio::test]
    async fn an_async_narrator_that_never_reads_its_stdin_still_hits_the_deadline() {
        // A prompt bigger than the pipe buffer, to a child that ignores stdin.
        let big = vec![b'x'; 4 * 1024 * 1024];
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg("exec sleep 30");
        let run = run_headless(cmd, &big, std::time::Duration::from_millis(400)).await;
        assert!(matches!(run, HeadlessRun::TimedOut), "{run:?}");
    }

    #[tokio::test]
    async fn an_async_narrator_that_answers_in_time_returns_stdout_and_stderr() {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg("cat; echo oops >&2");
        match run_headless(cmd, b"hello", std::time::Duration::from_secs(10)).await {
            HeadlessRun::Output(out) => {
                assert!(out.status.success());
                assert_eq!(out.stdout, b"hello");
                assert_eq!(out.stderr, b"oops\n");
            }
            other => panic!("{other:?}"),
        }
        let missing = tokio::process::Command::new("/nonexistent/claude");
        assert!(matches!(
            run_headless(missing, b"", std::time::Duration::from_secs(1)).await,
            HeadlessRun::SpawnFailed
        ));
    }
}
