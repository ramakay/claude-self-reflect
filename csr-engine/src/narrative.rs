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

/// Whether headless children keep loading the user's settings files.
///
/// `CSR_HEADLESS_USER_SETTINGS=1` forces it and `=0` forces full isolation.
/// Unset, the settings files decide, and the doubt goes to keeping them:
/// dropping a file that carried the child's way to the API turns every call
/// into an authentication failure, while keeping one costs only some
/// isolation. Hooks are switched off either way.
fn headless_keeps_user_settings() -> bool {
    match std::env::var("CSR_HEADLESS_USER_SETTINGS") {
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => true,
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("false") => false,
        _ => settings_paths().iter().any(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| settings_may_carry_api_access(&text))
        }),
    }
}

/// Every settings file `--setting-sources ""` would stop the child loading:
/// the user's, and the project and local ones of the directory it starts in.
fn settings_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::with_capacity(3);
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => {
            paths.push(std::path::PathBuf::from(dir).join("settings.json"))
        }
        _ => paths.extend(dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))),
    }
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(".claude").join("settings.json"));
        paths.push(cwd.join(".claude").join("settings.local.json"));
    }
    paths
}

/// False only when a settings file provably holds nothing the child could
/// need to reach the API. There is deliberately no list of credential
/// variables here: Claude Code keeps adding them (API keys, OAuth tokens,
/// provider switches, proxies, client certificates), and a name missing from
/// such a list would silently break a working install. So the test runs the
/// other way. A file is safe to drop when every `env` entry is a telemetry
/// switch and no top-level key is named like a credential helper; a file that
/// cannot be parsed, or holds anything else, is kept.
fn settings_may_carry_api_access(settings_json: &str) -> bool {
    let Ok(Value::Object(settings)) = serde_json::from_str::<Value>(settings_json) else {
        return true;
    };
    let helper_like = settings.iter().any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        !value.is_null()
            && ["helper", "auth", "credential", "apikey", "token"]
                .iter()
                .any(|needle| key.contains(needle))
    });
    let env_beyond_telemetry = match settings.get("env") {
        None | Some(Value::Null) => false,
        Some(Value::Object(env)) => env.keys().any(|name| {
            let name = name.to_ascii_uppercase();
            !(name.contains("TELEMETRY") || name == "DO_NOT_TRACK")
        }),
        Some(_) => true,
    };
    helper_like || env_beyond_telemetry
}

/// Argv that cuts a headless `claude -p` child off from the machine's Claude
/// Code setup. Every CSR spawn site passes it.
///
/// * `--setting-sources ""` loads no user, project or local settings, so the
///   child runs none of the user's plugins and none of their SessionStart
///   hooks.
/// * `--no-session-persistence` writes no transcript, so the watcher never
///   re-imports the headless call as if it were a real conversation.
///
/// Both are checked against `claude --help` (~55ms): a CLI that predates an
/// option rejects it outright, which would fail every call. Call this once per
/// narrative operation, outside the model-candidate loop. It is deliberately
/// not cached for the life of the process: the daemon outlives CLI upgrades,
/// and one empty `--help` would otherwise switch isolation off for good.
pub fn isolation_args() -> Vec<String> {
    let help = std::process::Command::new("claude")
        .arg("--help")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    isolation_args_for(&help, headless_keeps_user_settings())
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
    let config = serde_json::json!({ "mcpServers": {} });
    let dir = dirs::home_dir()
        .map(|h| h.join(".claude-self-reflect"))
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    std::fs::create_dir_all(&dir).ok();
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

    #[test]
    fn settings_that_may_carry_api_access_are_kept() {
        for settings in [
            r#"{"apiKeyHelper":"/usr/local/bin/key.sh"}"#,
            r#"{"awsAuthRefresh":"aws sso login"}"#,
            r#"{"awsCredentialExport":"/usr/local/bin/creds"}"#,
            r#"{"env":{"CLAUDE_CODE_USE_BEDROCK":"1","AWS_REGION":"us-east-1"}}"#,
            r#"{"env":{"CLAUDE_CODE_USE_VERTEX":"1"}}"#,
            r#"{"env":{"ANTHROPIC_BASE_URL":"https://gateway.example"}}"#,
            r#"{"env":{"CLAUDE_CODE_OAUTH_TOKEN":"token"}}"#,
            r#"{"env":{"CLAUDE_CODE_CLIENT_CERT":"/etc/ssl/client.pem","CLAUDE_CODE_CLIENT_KEY":"/etc/ssl/client.key"}}"#,
            r#"{"env":{"https_proxy":"http://proxy.example:8080"}}"#,
            r#"{"env":{"NODE_EXTRA_CA_CERTS":"/etc/ssl/corp.pem"}}"#,
            // A variable this code has never heard of is kept, not dropped.
            r#"{"env":{"DO_NOT_TRACK":"1","SOME_FUTURE_CREDENTIAL":"x"}}"#,
            r#"{"env":"not an object"}"#,
            "not json",
            "[]",
        ] {
            assert!(settings_may_carry_api_access(settings), "{settings}");
        }
    }

    #[test]
    fn settings_with_nothing_access_related_are_dropped() {
        for settings in [
            "{}",
            r#"{"apiKeyHelper":null}"#,
            r#"{"env":{}}"#,
            r#"{"env":null}"#,
            r#"{"env":{"DO_NOT_TRACK":"1","SOME_PLUGIN_TELEMETRY":"0","X_NO_TELEMETRY":"1"}}"#,
            r#"{"model":"opus","hooks":{},"enabledPlugins":{"x@y":true},"autoUpdatesChannel":"latest","permissions":{}}"#,
        ] {
            assert!(!settings_may_carry_api_access(settings), "{settings}");
        }
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
        let path = minimal_mcp_config().unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(
            servers.len(),
            0,
            "claude -p call sites need zero MCP servers"
        );
    }
}
