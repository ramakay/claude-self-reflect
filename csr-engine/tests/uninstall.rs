//! `csr-engine uninstall` runs the real binary against a scratch HOME with shims for `claude`,
//! `codex`, `launchctl`, `npm` and `ps` first (and only) on PATH. The real home, the real Claude
//! config, the real Codex config, the real launch agent, the real npm and the real process table
//! are never reachable from here. The shims use shell builtins only, because nothing else is on
//! PATH.
#![cfg(unix)]

#[path = "support/temp_home.rs"]
mod temp_home;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

const MARKER: &str = "<!-- Written by `csr-engine setup`; setup rewrites this file when the install changes. Hand edits belong in a plist without this line. -->";
const LABEL: &str = "com.ramakay.csr-engine.daemon";
const FOREIGN_NOTIFY: &str = "/Users/me/projects/claude-self-reflect/notify.sh";
const OTHER_MCP: &str = r#"{"type":"stdio","command":"/usr/bin/other","args":[]}"#;
const CSR_MCP: &str = r#"{"type":"stdio","command":"/opt/csr/csr-engine","args":[]}"#;

/// `claude`: logs every call. `CLAUDE_MODE` decides what `mcp remove` does to `~/.claude.json`:
/// nothing (the CLI "left it"), `removes` (a good CLI), `corrupt` (leaves unparseable text) or
/// `bump` (Claude Code rewrote the file meanwhile and the entry is still there).
/// `PLUGIN_STICKS` makes the plugin survive `plugin uninstall`; `MARKET_STICKS` keeps the
/// marketplace listed. `mcp remove ... -s local` logs the folder it ran in and, when
/// `LOCAL_AFTER_JSON` is set, writes that text to `~/.claude.json` (a CLI that removed the entry).
const CLAUDE_SHIM: &str = r#"echo "claude $*" >> "$CALLS"
case "$1 $2" in
"mcp remove")
  if [ "$5" = local ]; then
    echo "local-remove cwd=$PWD" >> "$CALLS"
    if [ -n "$LOCAL_AFTER_JSON" ]; then printf '%s\n' "$LOCAL_AFTER_JSON" > "$HOME/.claude.json"; fi
  else
  case "$CLAUDE_MODE" in
    removes) echo '{"numStartups":3,"mcpServers":{"other":{"type":"stdio","command":"/usr/bin/other","args":[]}}}' > "$HOME/.claude.json";;
    corrupt) echo '{ not json' > "$HOME/.claude.json";;
    bump) echo '{"numStartups":99,"newKey":true,"mcpServers":{"claude-self-reflect":{"type":"stdio","command":"/opt/csr/csr-engine","args":[]},"other":{"type":"stdio","command":"/usr/bin/other","args":[]}}}' > "$HOME/.claude.json";;
  esac
  fi;;
"plugin uninstall") if [ -n "$PLUGIN_STICKS" ]; then echo "Error: busy" >&2; exit 1; fi;;
"plugin list") if [ -n "$PLUGIN_STICKS" ]; then echo '[{"id":"csr-companion@claude-self-reflect"}]'; fi;;
"plugin marketplace") if [ "$3" = list ] && [ -n "$MARKET_STICKS" ]; then echo "claude-self-reflect"; fi;;
esac
exit 0"#;

/// `launchctl`: `print` answers from `FAKE_LOADED` (the plist the job was loaded from; empty means
/// no such job) until `bootout` has run; `BOOTOUT_FAIL` and `PRINT_FAIL` make those calls fail.
const LAUNCHCTL_SHIM: &str = r#"echo "launchctl $*" >> "$CALLS"
case "$1" in
print)
  if [ -n "$PRINT_FAIL" ]; then echo "Operation not permitted" >&2; exit 1; fi
  if [ -n "$FAKE_LOADED" ] && [ ! -f "$STATE/booted" ]; then echo "	path = $FAKE_LOADED"; else echo "Could not find service \"x\" in domain" >&2; exit 113; fi;;
bootout)
  if [ -n "$BOOTOUT_FAIL" ]; then echo "Boot-out failed: 5" >&2; exit 5; fi
  : > "$STATE/booted";;
esac
exit 0"#;

/// `codex`: its entry lives in `$HOME/.codex/config.toml` as a `[mcp_servers.claude-self-reflect]`
/// table with a `command = "..."` line; `mcp remove` empties the file unless `CODEX_KEEP` is set.
const CODEX_SHIM: &str = r#"echo "codex $*" >> "$CALLS"
CFG="$HOME/.codex/config.toml"
has=
cmd=
if [ -f "$CFG" ]; then
  while IFS= read -r l; do
    case "$l" in
      "[mcp_servers.claude-self-reflect]") has=1;;
      "command = "*) cmd=${l#command = \"}; cmd=${cmd%\"};;
    esac
  done < "$CFG"
fi
case "$1 $2" in
"mcp get")
  if [ -n "$has" ]; then
    echo "{\"name\":\"claude-self-reflect\",\"enabled\":true,\"transport\":{\"type\":\"stdio\",\"command\":\"$cmd\",\"args\":[]}}"
  else
    echo "Error: No MCP server named 'claude-self-reflect' found." >&2; exit 1
  fi;;
"mcp remove")
  if [ -z "$CODEX_KEEP" ]; then : > "$CFG"; fi;;
esac
exit 0"#;

/// `npm`: `ls -g` lists claude-self-reflect at `$NPM_VERSION` (nothing when unset or after an
/// `uninstall`) and, like npm, exits 1 when it is absent; `NPM_STICKS` makes `uninstall` do nothing.
const NPM_SHIM: &str = r#"echo "npm $*" >> "$CALLS"
case "$1" in
ls)
  if [ -n "$NPM_VERSION" ] && [ ! -f "$STATE/npm_removed" ]; then
    echo "{\"name\":\"lib\",\"dependencies\":{\"claude-self-reflect\":{\"version\":\"$NPM_VERSION\"}}}"
  else
    echo '{}'; exit 1
  fi;;
uninstall)
  if [ -z "$NPM_STICKS" ]; then : > "$STATE/npm_removed"; fi;;
esac
exit 0"#;

/// `ps`: prints `$FAKE_PS` (lines of `pid ppid args`); logs the call.
const PS_SHIM: &str = r#"echo "ps $*" >> "$CALLS"
if [ -n "$FAKE_PS" ]; then printf '%s\n' "$FAKE_PS"; fi
exit 0"#;

struct Scratch {
    _root: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    calls: PathBuf,
    bin: PathBuf,
}

fn plist(label: &str, marked: bool) -> String {
    plist_running(label, marked, "/opt/csr/csr-engine")
}

fn plist_running(label: &str, marked: bool, program: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n{}\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{label}</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{program}</string>\n    <string>daemon</string>\n  </array>\n</dict>\n</plist>\n",
        if marked { MARKER } else { "" }
    )
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn shim(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    write(path, &format!("#!/bin/sh\n{body}\n"));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The settings a user has after `setup`: CSR's hooks, plus hooks that are not CSR's.
fn settings_json() -> Value {
    let mut hooks =
        csr_engine::hooks::install::generate_hook_config_for_test("/opt/csr/csr-engine")["hooks"]
            .clone();
    let stop = hooks["Stop"].as_array_mut().unwrap();
    // A user hook beside CSR's in the same entry, and one whose path names the repo.
    stop[0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "command", "command": "echo mine"}));
    stop.push(json!({"hooks": [{"type": "command", "command": FOREIGN_NOTIFY}]}));
    hooks["PreToolUse"] =
        json!([{"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]}]);
    // A user's own agent hook that only mentions a word CSR once used.
    hooks["Notification"] = json!([{"hooks": [
        {"type": "agent", "prompt": "Summarize each session_episode row in my notes"}
    ]}]);
    json!({"model": "opus", "hooks": hooks, "statusLine": {"type": "command", "command": "ccstatusline"}})
}

fn csr_hooks_in_fixture() -> usize {
    let mut settings = settings_json();
    csr_engine::hooks::install::remove_csr_hooks(&mut settings)
}

fn claude_json(mcp: &[(&str, &str)]) -> String {
    let servers: Vec<String> = mcp.iter().map(|(k, v)| format!("\"{k}\": {v}")).collect();
    format!(
        "{{\"numStartups\": 3, \"mcpServers\": {{{}}}}}",
        servers.join(", ")
    )
}

impl Scratch {
    fn new() -> Self {
        let root = temp_home::tempdir().unwrap();
        let rootp = root.path().to_path_buf();
        let home = rootp.join("home");
        let bin = rootp.join("bin");
        let calls = rootp.join("calls.log");
        std::fs::create_dir_all(&home).unwrap();
        shim(&bin.join("claude"), CLAUDE_SHIM);
        shim(&bin.join("launchctl"), LAUNCHCTL_SHIM);
        shim(&bin.join("codex"), CODEX_SHIM);
        shim(&bin.join("npm"), NPM_SHIM);
        shim(&bin.join("ps"), PS_SHIM);
        write(
            &home.join(".claude/settings.json"),
            &serde_json::to_string_pretty(&settings_json()).unwrap(),
        );
        write(
            &home.join(".claude/projects/p/x.jsonl"),
            "{\"type\":\"user\"}\n",
        );
        write(
            &home.join(".claude.json"),
            &claude_json(&[("claude-self-reflect", CSR_MCP), ("other", OTHER_MCP)]),
        );
        write(
            &home.join(".codex/config.toml"),
            "model = \"x\"\n\n[mcp_servers.claude-self-reflect]\ncommand = \"/opt/csr/csr-engine\"\n",
        );
        write(
            &home.join(format!("Library/LaunchAgents/{LABEL}.plist")),
            &plist(LABEL, true),
        );
        write(
            &home.join("Library/LaunchAgents/com.example.mine.plist"),
            &plist("com.example.mine", false),
        );
        let data = home.join(".claude-self-reflect");
        write(&data.join("csr-engine.db"), "db");
        write(&data.join(".env"), "KEY=1");
        write(&data.join("csr-companion/marketplace/m.json"), "{}");
        write(&data.join("mirror/transcripts/a.zst"), "0123456789");
        Self {
            _root: root,
            root: rootp,
            home,
            calls,
            bin,
        }
    }

    fn plist(&self) -> PathBuf {
        self.home
            .join(format!("Library/LaunchAgents/{LABEL}.plist"))
    }

    fn data(&self) -> PathBuf {
        self.home.join(".claude-self-reflect")
    }

    /// Run `uninstall` with the job loaded from this scratch's own plist.
    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[("FAKE_LOADED", self.plist().to_str().unwrap())])
    }

    fn run_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        self.run_program(Path::new(env!("CARGO_BIN_EXE_csr-engine")), args, envs)
    }

    fn run_program(&self, program: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(program);
        cmd.arg("uninstall")
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.bin)
            .env("CALLS", &self.calls)
            .env("STATE", &self.root);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.calls).unwrap_or_default()
    }

    fn snapshot(&self) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn walk(dir: &Path, into: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    into.insert(path.clone(), None);
                    walk(&path, into);
                } else {
                    into.insert(path.clone(), Some(std::fs::read(&path).unwrap()));
                }
            }
        }
        let mut into = BTreeMap::new();
        walk(&self.home, &mut into);
        into
    }

    fn settings(&self) -> Value {
        let text = std::fs::read_to_string(self.home.join(".claude/settings.json")).unwrap();
        serde_json::from_str(&text).expect("settings.json still parses")
    }

    fn claude_json_text(&self) -> String {
        std::fs::read_to_string(self.home.join(".claude.json")).unwrap()
    }

    /// Files in `dir` whose name starts with `prefix`.
    fn named(dir: &Path, prefix: &str) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
            .map(|e| e.path())
            .collect()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_plan_changes_nothing_and_names_every_artifact() {
    let s = Scratch::new();
    let before = s.snapshot();
    for args in [
        &["--dry-run"][..],
        &["--dry-run", "--yes"][..],
        &[][..],
        &["--dry-run", "--purge"][..],
    ] {
        let out = s.run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", stdout(&out));
        assert_eq!(s.snapshot(), before, "{args:?} changed the scratch home");
        // A plan runs read-only probes (npm ls, ps) and nothing that changes anything.
        let mutating: Vec<String> = s
            .calls()
            .lines()
            .filter(|l| !l.starts_with("npm ls ") && !l.starts_with("ps "))
            .map(str::to_owned)
            .collect();
        assert!(mutating.is_empty(), "{args:?} ran {mutating:?}");
        let data_want = if args.contains(&"--purge") {
            "data: would delete"
        } else {
            "--purge deletes it"
        };
        let text = stdout(&out);
        let n = csr_hooks_in_fixture();
        assert!(n >= 8, "the fixture holds CSR hooks: {n}");
        for want in [
            format!("would remove {n} CSR hook(s)"),
            "would run `claude mcp remove claude-self-reflect -s user`".to_string(),
            "would run `codex mcp remove claude-self-reflect`".to_string(),
            "would run `claude plugin uninstall csr-companion@claude-self-reflect`".to_string(),
            "would run `claude plugin marketplace remove claude-self-reflect`".to_string(),
            format!("would stop {LABEL}"),
            "com.example.mine.plist was not written by setup; left in place".to_string(),
            data_want.to_string(),
            "transcript mirror".to_string(),
        ] {
            assert!(text.contains(&want), "{args:?} lacks {want:?} in:\n{text}");
        }
    }
}

#[test]
fn yes_and_purge_mirror_leave_no_csr_artifact_and_keep_everything_else() {
    let s = Scratch::new();
    let original = std::fs::read(s.home.join(".claude/settings.json")).unwrap();
    let out = s.run(&["--yes", "--purge-mirror"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));

    // Hooks: none of CSR's, every other hook and key as it was.
    let settings = s.settings();
    let text = serde_json::to_string(&settings).unwrap();
    assert!(!text.contains("csr-engine hook"), "{text}");
    assert_eq!(settings["model"], "opus");
    assert_eq!(settings["statusLine"]["command"], "ccstatusline");
    let stop = settings["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2, "{stop:?}");
    assert_eq!(
        stop[0]["hooks"],
        json!([{"type": "command", "command": "echo mine"}])
    );
    assert_eq!(stop[1]["hooks"][0]["command"], FOREIGN_NOTIFY);
    assert_eq!(
        settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "guard.sh"
    );
    assert_eq!(
        settings["hooks"]["Notification"][0]["hooks"][0]["prompt"],
        "Summarize each session_episode row in my notes"
    );
    assert!(settings["hooks"].get("SessionStart").is_none());
    let backups = Scratch::named(&s.home.join(".claude"), "settings.json.csr-uninstall-");
    assert_eq!(backups.len(), 1);
    assert_eq!(std::fs::read(&backups[0]).unwrap(), original);
    for dir in [s.home.clone(), s.home.join(".claude")] {
        assert!(
            Scratch::named(&dir, ".")
                .iter()
                .all(|p| !p.to_string_lossy().ends_with(".tmp")),
            "a temp file was left in {}",
            dir.display()
        );
    }

    // MCP: CSR's entry gone, the other one and the rest of the file stay.
    let claude_json: Value = serde_json::from_str(&s.claude_json_text()).unwrap();
    assert!(claude_json["mcpServers"]
        .get("claude-self-reflect")
        .is_none());
    assert_eq!(
        claude_json["mcpServers"]["other"]["command"],
        "/usr/bin/other"
    );
    assert_eq!(claude_json["numStartups"], 3);

    // Codex: its entry gone through its own CLI, the config backed up first.
    assert_eq!(
        std::fs::read_to_string(s.home.join(".codex/config.toml")).unwrap(),
        ""
    );
    let codex_backups = Scratch::named(&s.home.join(".codex"), "config.toml.csr-uninstall-");
    assert_eq!(codex_backups.len(), 1);
    assert!(std::fs::read_to_string(&codex_backups[0])
        .unwrap()
        .contains("[mcp_servers.claude-self-reflect]"));

    // Launch agent: ours deleted and booted out, the user's own left.
    assert!(!s.plist().exists());
    assert!(s
        .home
        .join("Library/LaunchAgents/com.example.mine.plist")
        .exists());
    let calls = s.calls();
    for want in [
        "claude mcp remove claude-self-reflect -s user",
        "codex mcp remove claude-self-reflect",
        "claude plugin uninstall csr-companion@claude-self-reflect",
        "claude plugin marketplace remove claude-self-reflect",
    ] {
        assert!(calls.contains(want), "{want} not in:\n{calls}");
    }
    assert!(
        calls
            .lines()
            .any(|l| l.starts_with("launchctl bootout gui/") && l.ends_with(LABEL)),
        "{calls}"
    );

    // Data: all of it, and never the transcripts Claude Code keeps.
    assert!(!s.data().exists());
    assert!(s.home.join(".claude/projects/p/x.jsonl").exists());
}

#[test]
fn the_mirror_stays_with_purge_and_its_path_is_printed() {
    let s = Scratch::new();
    let out = s.run(&["--yes", "--purge"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let data = s.data();
    assert!(data.join("mirror/transcripts/a.zst").exists());
    assert!(!data.join("csr-engine.db").exists());
    assert!(!data.join(".env").exists());
    assert!(!data.join("csr-companion").exists());
    assert!(stdout(&out).contains(&format!("kept at {}", data.join("mirror").display())));
}

#[test]
fn by_default_and_with_the_old_keep_data_flag_the_data_folder_is_kept_whole() {
    // `--keep-data` is a hidden no-op now: it must give exactly what no flag gives.
    for args in [&["--yes"][..], &["--yes", "--keep-data"][..]] {
        let s = Scratch::new();
        let data_before: Vec<_> = s
            .snapshot()
            .into_iter()
            .filter(|(p, _)| p.starts_with(s.data()))
            .collect();
        let out = s.run(args);
        let text = stdout(&out);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {text}");
        let data_after: Vec<_> = s
            .snapshot()
            .into_iter()
            .filter(|(p, _)| p.starts_with(s.data()))
            .collect();
        assert_eq!(data_before, data_after, "{args:?}");
        assert!(!s.plist().exists(), "{args:?}: the integrations still go");
        assert!(
            text.contains(&format!("data: {} (", s.data().display()))
                && text.contains("MiB): kept; --purge deletes it"),
            "{args:?}: the data path, its size and the way to delete it are printed: {text}"
        );
        assert!(
            text.contains(&format!("kept at {}", s.data().join("mirror").display())),
            "{args:?}: the mirror path and size are printed: {text}"
        );
    }
}

#[test]
fn purge_deletes_the_data_except_the_mirror_and_purge_mirror_implies_purge() {
    let s = Scratch::new();
    let out = s.run(&["--yes", "--purge"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!s.data().join("csr-engine.db").exists());
    assert!(!s.data().join(".env").exists());
    assert!(s.data().join("mirror/transcripts/a.zst").exists());
    // `--purge-mirror` alone is a purge that also takes the mirror.
    let s = Scratch::new();
    let out = s.run(&["--yes", "--purge-mirror"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!s.data().exists(), "{}", stdout(&out));
}

#[test]
fn the_old_keep_data_flag_still_parses_and_never_beats_a_purge() {
    let s = Scratch::new();
    let before = s.snapshot();
    for conflicting in [&["--purge"][..], &["--purge-mirror"][..]] {
        let mut args = vec!["--yes", "--keep-data"];
        args.extend_from_slice(conflicting);
        let out = s.run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stdout(&out));
        assert_eq!(s.snapshot(), before, "{args:?} changed the scratch home");
    }
    let help = Command::new(env!("CARGO_BIN_EXE_csr-engine"))
        .args(["uninstall", "--help"])
        .output()
        .unwrap();
    let help = stdout(&help);
    assert!(
        help.contains("--purge") && help.contains("--purge-mirror"),
        "{help}"
    );
    assert!(!help.contains("--keep-data"), "hidden from --help: {help}");
}

#[test]
fn a_job_with_no_loaded_instance_is_not_stopped_and_the_files_go() {
    let s = Scratch::new();
    let out = s.run_env(&["--yes", "--purge"], &[("FAKE_LOADED", "")]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!s.calls().contains("bootout"), "{}", s.calls());
    assert!(!s.plist().exists());
    assert!(!s.data().join("csr-engine.db").exists());
}

#[test]
fn a_job_loaded_from_another_file_is_not_stopped_and_the_data_stays() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--purge"],
        &[("FAKE_LOADED", "/elsewhere/other.plist")],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert!(!s.calls().contains("bootout"), "{}", s.calls());
    assert!(!s.plist().exists(), "the file setup wrote is still removed");
    assert!(
        s.data().join("csr-engine.db").exists(),
        "that daemon keeps running on this data: {}",
        stdout(&out)
    );
    assert!(stdout(&out).contains("keeps running"), "{}", stdout(&out));
}

#[test]
fn a_failed_daemon_stop_keeps_the_plist_and_the_data_and_exits_nonzero() {
    let s = Scratch::new();
    let loaded = s.plist();
    let out = s.run_env(
        &["--yes", "--purge-mirror"],
        &[
            ("FAKE_LOADED", loaded.to_str().unwrap()),
            ("BOOTOUT_FAIL", "1"),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(s.plist().exists(), "a retry needs the plist: {text}");
    assert!(
        s.data().join("csr-engine.db").exists()
            && s.data().join("mirror/transcripts/a.zst").exists(),
        "the data folder stays under a daemon that may still run: {text}"
    );
    assert!(text.contains("could not stop"), "{text}");
    // The other integrations were still removed.
    assert!(!s.settings()["hooks"]
        .to_string()
        .contains("csr-engine hook"));
}

#[test]
fn a_launchd_that_cannot_be_asked_is_not_read_as_not_running() {
    let s = Scratch::new();
    let loaded = s.plist();
    let out = s.run_env(
        &["--yes", "--purge"],
        &[
            ("FAKE_LOADED", loaded.to_str().unwrap()),
            ("PRINT_FAIL", "1"),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(s.plist().exists() && s.data().join("csr-engine.db").exists());
    assert!(text.contains("could not tell whether"), "{text}");
}

#[test]
fn unparseable_settings_are_left_byte_for_byte_and_the_exit_is_not_zero() {
    let s = Scratch::new();
    let path = s.home.join(".claude/settings.json");
    std::fs::write(&path, "{ not json").unwrap();
    let out = s.run(&["--yes"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    assert!(stdout(&out).contains("not valid JSON"));
}

#[test]
fn a_link_at_the_old_temp_name_is_never_written_through() {
    use std::os::unix::fs::symlink;
    let s = Scratch::new();
    let victim = s.root.join("victim.txt");
    std::fs::write(&victim, "precious").unwrap();
    // The name the first version of uninstall wrote its temp file to.
    let planted = s.home.join(".claude/settings.csr-uninstall.tmp");
    symlink(&victim, &planted).unwrap();
    let planted_claude = s.home.join(".claude.csr-uninstall.tmp");
    symlink(&victim, &planted_claude).unwrap();
    let out = s.run(&["--yes", "--keep-data"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
    assert!(std::fs::symlink_metadata(&planted)
        .unwrap()
        .file_type()
        .is_symlink());
    let meta = std::fs::symlink_metadata(s.home.join(".claude/settings.json")).unwrap();
    assert!(meta.file_type().is_file(), "settings.json is not a link");
    assert!(!s.settings()["hooks"]
        .to_string()
        .contains("csr-engine hook"));
}

#[test]
fn a_dotfiles_link_for_settings_keeps_pointing_at_its_target() {
    use std::os::unix::fs::symlink;
    let s = Scratch::new();
    let real = s.root.join("dotfiles/settings.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    let path = s.home.join(".claude/settings.json");
    std::fs::rename(&path, &real).unwrap();
    symlink(&real, &path).unwrap();
    let out = s.run(&["--yes", "--keep-data"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(std::fs::symlink_metadata(&path)
        .unwrap()
        .file_type()
        .is_symlink());
    let text = std::fs::read_to_string(&real).unwrap();
    assert!(!text.contains("csr-engine hook"), "{text}");
}

#[test]
fn a_claude_json_that_cannot_be_read_back_is_not_called_clean() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--keep-data"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("CLAUDE_MODE", "corrupt"),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    // (The Codex line says "removed the ... entry" for Codex; only the mcp line is judged here.)
    assert!(
        !text.contains("mcp: removed the 'claude-self-reflect' entry"),
        "{text}"
    );
    assert!(
        text.contains("mcp: could not confirm the 'claude-self-reflect' entry is gone"),
        "{text}"
    );
    // Nothing was written over what the CLI left.
    assert_eq!(s.claude_json_text().trim(), "{ not json");
}

#[test]
fn the_by_hand_edit_starts_from_the_file_as_it_is_after_the_cli_ran() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--keep-data"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("CLAUDE_MODE", "bump"),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let config: Value = serde_json::from_str(&s.claude_json_text()).unwrap();
    assert_eq!(config["numStartups"], 99, "a newer write was overwritten");
    assert_eq!(config["newKey"], true);
    assert!(config["mcpServers"].get("claude-self-reflect").is_none());
    assert_eq!(config["mcpServers"]["other"]["command"], "/usr/bin/other");
    // The backup holds what the file held when it was replaced.
    let backups = Scratch::named(&s.home, ".claude.json.csr-uninstall-");
    assert_eq!(backups.len(), 1);
    assert!(std::fs::read_to_string(&backups[0])
        .unwrap()
        .contains("\"numStartups\":99"));
}

#[test]
fn a_cli_that_removes_the_entry_itself_needs_no_by_hand_edit() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--keep-data"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("CLAUDE_MODE", "removes"),
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(Scratch::named(&s.home, ".claude.json.csr-uninstall-").is_empty());
    assert!(stdout(&out).contains("removed the 'claude-self-reflect' entry (claude mcp remove)"));
}

#[test]
fn an_entry_that_does_not_run_csr_engine_is_left_in_both_configs() {
    let s = Scratch::new();
    let foreign = r#"{"type":"stdio","command":"/usr/bin/mine","args":[]}"#;
    write(
        &s.home.join(".claude.json"),
        &claude_json(&[("claude-self-reflect", foreign)]),
    );
    write(
        &s.home.join(".codex/config.toml"),
        "[mcp_servers.claude-self-reflect]\ncommand = \"/usr/bin/mine\"\n",
    );
    let claude_before = s.claude_json_text();
    let out = s.run(&["--yes", "--keep-data"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert_eq!(s.claude_json_text(), claude_before);
    assert!(std::fs::read_to_string(s.home.join(".codex/config.toml"))
        .unwrap()
        .contains("/usr/bin/mine"));
    let calls = s.calls();
    assert!(!calls.contains("mcp remove"), "{calls}");
}

#[test]
fn a_codex_entry_that_survives_the_remove_is_reported_and_exits_nonzero() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--keep-data"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("CODEX_KEEP", "1"),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("did not remove the entry"), "{text}");
}

#[test]
fn a_plugin_that_is_still_installed_is_reported_and_exits_nonzero() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--keep-data"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("PLUGIN_STICKS", "1"),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("is still installed"), "{text}");
}

#[test]
fn a_second_run_does_not_delete_data_under_a_daemon_the_first_run_could_not_stop() {
    let s = Scratch::new();
    let elsewhere = [("FAKE_LOADED", "/elsewhere/other.plist")];
    let first = s.run_env(&["--yes", "--purge"], &elsewhere);
    assert_eq!(first.status.code(), Some(1), "{}", stdout(&first));
    assert!(!s.plist().exists());
    // The plist is gone now, so a second run has no file to find the daemon by.
    let second = s.run_env(&["--yes", "--purge-mirror"], &elsewhere);
    let text = stdout(&second);
    assert_eq!(second.status.code(), Some(1), "{text}");
    assert!(
        s.data().join("csr-engine.db").exists()
            && s.data().join("mirror/transcripts/a.zst").exists(),
        "data deleted under a running daemon: {text}"
    );
    assert!(text.contains("still loaded in launchd"), "{text}");
}

#[test]
fn a_running_job_with_no_plist_on_disk_keeps_the_data() {
    let s = Scratch::new();
    std::fs::remove_file(s.plist()).unwrap();
    let out = s.run_env(
        &["--yes", "--purge"],
        &[("FAKE_LOADED", "/elsewhere/other.plist")],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(s.data().join("csr-engine.db").exists(), "{text}");
    assert!(text.contains("still loaded in launchd"), "{text}");
}

#[test]
fn a_running_job_behind_a_plist_setup_did_not_write_keeps_the_data() {
    let s = Scratch::new();
    write(&s.plist(), &plist(LABEL, false));
    let loaded = s.plist();
    let out = s.run_env(
        &["--yes", "--purge"],
        &[("FAKE_LOADED", loaded.to_str().unwrap())],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(s.plist().exists(), "a plist that is not ours stays: {text}");
    assert!(s.data().join("csr-engine.db").exists(), "{text}");
    assert!(!s.calls().contains("bootout"), "{}", s.calls());
}

#[test]
fn a_codex_entry_that_cannot_be_removed_because_codex_is_missing_exits_nonzero() {
    let s = Scratch::new();
    std::fs::remove_file(s.bin.join("codex")).unwrap();
    let out = s.run(&["--yes", "--keep-data"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("NOT removed"), "{text}");
    assert!(std::fs::read_to_string(s.home.join(".codex/config.toml"))
        .unwrap()
        .contains("[mcp_servers.claude-self-reflect]"));
}

#[test]
fn a_data_folder_that_cannot_be_listed_is_not_called_deleted() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root reads any folder
    }
    let s = Scratch::new();
    std::fs::set_permissions(s.data(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let out = s.run(&["--yes", "--purge-mirror"]);
    std::fs::set_permissions(s.data(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("could not read"), "{text}");
    assert!(!text.contains("data: deleted"), "{text}");
    assert!(s.data().join("csr-engine.db").exists());
}

#[test]
fn a_custom_db_path_is_refused_and_nothing_changes() {
    let s = Scratch::new();
    let before = s.snapshot();
    let custom = s.root.join("custom/csr-engine.db");
    for extra in [&["--yes"][..], &["--dry-run"][..], &[][..]] {
        let mut args = vec!["--db-path", custom.to_str().unwrap()];
        args.extend_from_slice(extra);
        let out = s.run(&args);
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {}", stdout(&out));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--db-path"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(s.snapshot(), before, "{extra:?} changed the scratch home");
        assert_eq!(s.calls(), "", "{extra:?} ran a command");
    }
    // The default path given explicitly is accepted.
    let default = s.data().join("csr-engine.db");
    let out = s.run(&["--db-path", default.to_str().unwrap(), "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
}

#[test]
fn without_claude_the_plugin_removal_is_not_called_done() {
    let s = Scratch::new();
    std::fs::remove_file(s.bin.join("claude")).unwrap();
    let out = s.run(&["--yes", "--keep-data"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("plugin: could not confirm the plugin is gone"),
        "{text}"
    );
}

#[test]
fn a_marketplace_that_is_still_registered_is_reported_and_exits_nonzero() {
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes", "--keep-data"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("MARKET_STICKS", "1"),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("marketplace claude-self-reflect is still registered"),
        "{text}"
    );
}

#[test]
fn a_data_folder_that_is_a_link_loses_the_link_and_never_the_target() {
    use std::os::unix::fs::symlink;
    let s = Scratch::new();
    let outside = s.root.join("elsewhere");
    write(&outside.join("keep.txt"), "mine");
    write(&outside.join("mirror/transcripts/t.zst"), "t");
    std::fs::remove_dir_all(s.data()).unwrap();
    symlink(&outside, s.data()).unwrap();
    let out = s.run(&["--yes", "--purge-mirror"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(std::fs::symlink_metadata(s.data()).is_err(), "link removed");
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "mine"
    );
    assert!(outside.join("mirror/transcripts/t.zst").exists());
}

#[test]
fn a_link_inside_the_data_folder_loses_the_link_and_never_the_target() {
    use std::os::unix::fs::symlink;
    let s = Scratch::new();
    let outside = s.root.join("elsewhere");
    write(&outside.join("keep.txt"), "mine");
    symlink(&outside, s.data().join("linked-dir")).unwrap();
    symlink(outside.join("keep.txt"), s.data().join("linked-file")).unwrap();
    let out = s.run(&["--yes", "--purge-mirror"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!s.data().exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn the_model_cache_is_planned_with_its_size_and_goes_without_purge() {
    let s = Scratch::new();
    let cache = temp_home::model_cache(&s.home);
    write(
        &cache.join("fastembed/model.onnx"),
        &"x".repeat(2 * 1024 * 1024),
    );
    let plan = stdout(&s.run(&["--dry-run"]));
    assert!(
        plan.contains(&format!(
            "model cache: would delete {} (2.0 MiB)",
            cache.display()
        )),
        "{plan}"
    );
    assert!(cache.exists(), "a plan deletes nothing");
    let out = s.run(&["--yes"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!cache.exists(), "{}", stdout(&out));
    assert!(
        s.data().join("csr-engine.db").exists(),
        "without --purge the data is kept"
    );
    assert!(stdout(&out).contains(&format!("model cache: deleted {}", cache.display())));
}

#[test]
fn a_binary_in_local_bin_is_deleted_last_and_any_other_gets_its_rm_line() {
    let s = Scratch::new();
    // Run from anywhere else (here: the build folder): never deleted, the exact line printed.
    let built = Path::new(env!("CARGO_BIN_EXE_csr-engine"));
    let out = s.run(&["--yes", "--purge-mirror"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(built.exists());
    assert!(
        stdout(&out).contains(&format!("rm \"{}\"", built.display())),
        "{}",
        stdout(&out)
    );
    // Installed into ~/.local/bin: the copy removes itself, after everything else.
    let s = Scratch::new();
    let local = s.home.join(".local/bin/csr-engine");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::copy(built, &local).unwrap();
    let plan = stdout(&s.run_program(&local, &["--dry-run"], &[]));
    assert!(
        plan.contains(&format!("binary: would delete {} last", local.display())),
        "{plan}"
    );
    let loaded = s.plist();
    let out = s.run_program(
        &local,
        &["--yes", "--purge-mirror"],
        &[("FAKE_LOADED", loaded.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!local.exists(), "{}", stdout(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!("binary: deleted {}", local.display())),
        "{text}"
    );
    let last_step = text
        .lines()
        .rfind(|l| l.trim_start().starts_with(|c: char| c.is_ascii_lowercase()) && l.contains(": "))
        .unwrap_or_default();
    assert!(last_step.contains("binary: deleted"), "{text}");
}

/// A binary stand-in: any file will do, nothing here runs it.
fn fake_binary(path: &Path) {
    write(path, "#!/bin/sh\n");
}

#[test]
fn every_binary_the_install_registered_is_found_not_only_the_one_running() {
    let s = Scratch::new();
    let built = Path::new(env!("CARGO_BIN_EXE_csr-engine"));
    let local = s.home.join(".local/bin/csr-engine");
    let via_mcp = s.root.join("npm-lib/csr-engine");
    let via_codex = s.root.join("codex-bin/csr-engine");
    let via_plist = s.root.join("agent-bin/csr-engine");
    let gone = s.root.join("gone/csr-engine");
    for p in [&local, &via_mcp, &via_codex, &via_plist] {
        fake_binary(p);
    }
    // Hooks name the `~/.local/bin` copy (twice over, the dedupe is part of the test) and a
    // copy that no longer exists; the MCP entry, Codex and the launch agent each name another.
    let mut hooks =
        csr_engine::hooks::install::generate_hook_config_for_test(local.to_str().unwrap());
    let extra = json!({"type": "command", "command": format!("{} hook stop", gone.display())});
    hooks["hooks"]["Stop"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(extra);
    write(
        &s.home.join(".claude/settings.json"),
        &serde_json::to_string_pretty(&hooks).unwrap(),
    );
    let mcp = format!(
        r#"{{"type":"stdio","command":"{}","args":[]}}"#,
        via_mcp.display()
    );
    write(
        &s.home.join(".claude.json"),
        &claude_json(&[("claude-self-reflect", &mcp), ("other", OTHER_MCP)]),
    );
    write(
        &s.home.join(".codex/config.toml"),
        &format!(
            "[mcp_servers.claude-self-reflect]\ncommand = \"{}\"\n",
            via_codex.display()
        ),
    );
    write(
        &s.plist(),
        &plist_running(LABEL, true, via_plist.to_str().unwrap()),
    );

    let before = s.snapshot();
    let plan = stdout(&s.run(&["--dry-run"]));
    assert_eq!(s.snapshot(), before, "a plan changed the scratch home");
    assert!(
        plan.contains(&format!("binary: would delete {} last", local.display())),
        "{plan}"
    );
    for other in [built, &via_mcp, &via_codex, &via_plist] {
        let line = format!("rm \"{}\"", other.display());
        assert_eq!(plan.matches(&line).count(), 1, "{line} once in:\n{plan}");
    }
    assert!(!plan.contains(&gone.display().to_string()), "{plan}");

    let loaded = s.plist();
    let out = s.run_env(&["--yes"], &[("FAKE_LOADED", loaded.to_str().unwrap())]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        !local.exists(),
        "the registered copy in ~/.local/bin goes: {text}"
    );
    assert!(
        text.contains(&format!("binary: deleted {}", local.display())),
        "{text}"
    );
    for kept in [built, &via_mcp, &via_codex, &via_plist] {
        assert!(kept.exists(), "{} is not ours to delete", kept.display());
    }

    // A daemon launchd still runs: the registered copy stays.
    let s = Scratch::new();
    fake_binary(&s.home.join(".local/bin/csr-engine"));
    write(
        &s.home.join(".claude/settings.json"),
        &serde_json::to_string_pretty(&csr_engine::hooks::install::generate_hook_config_for_test(
            s.home.join(".local/bin/csr-engine").to_str().unwrap(),
        ))
        .unwrap(),
    );
    let out = s.run_env(&["--yes"], &[("FAKE_LOADED", "/elsewhere/other.plist")]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(s.home.join(".local/bin/csr-engine").exists(), "{text}");
    assert!(text.contains("binary: kept"), "{text}");
}

#[test]
fn the_npm_global_package_is_planned_removed_and_checked() {
    let cmd = "npm uninstall -g claude-self-reflect";
    let s = Scratch::new();
    let before = s.snapshot();
    let plan = stdout(&s.run_env(&["--dry-run"], &[("NPM_VERSION", "9.5.8")]));
    assert_eq!(s.snapshot(), before);
    assert!(
        plan.contains(&format!(
            "npm: would run `{cmd}` (claude-self-reflect@9.5.8"
        )),
        "{plan}"
    );
    assert!(s
        .calls()
        .contains("npm ls -g claude-self-reflect --json --depth=0"));
    assert!(
        !s.calls().contains(cmd),
        "a plan removes nothing: {}",
        s.calls()
    );

    let out = s.run_env(
        &["--yes"],
        &[
            ("NPM_VERSION", "9.5.8"),
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(s.calls().contains(cmd), "{}", s.calls());
    assert!(
        text.contains("npm: removed claude-self-reflect@9.5.8"),
        "{text}"
    );

    // npm that does not remove it: reported, exit 1.
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes"],
        &[
            ("NPM_VERSION", "9.5.8"),
            ("NPM_STICKS", "1"),
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("did not remove claude-self-reflect@9.5.8"),
        "{text}"
    );

    // A daemon that may still run keeps the package.
    let s = Scratch::new();
    let out = s.run_env(
        &["--yes"],
        &[
            ("NPM_VERSION", "9.5.8"),
            ("FAKE_LOADED", "/elsewhere/other.plist"),
        ],
    );
    let text = stdout(&out);
    assert!(!s.calls().contains(cmd), "{}", s.calls());
    assert!(
        text.contains("npm: kept claude-self-reflect@9.5.8"),
        "{text}"
    );

    // Not installed through npm, or no npm at all: no npm line, no failure.
    let s = Scratch::new();
    let text = stdout(&s.run_env(&["--dry-run"], &[]));
    assert!(!text.contains("npm:"), "{text}");
    std::fs::remove_file(s.bin.join("npm")).unwrap();
    let out = s.run_env(&["--yes"], &[("FAKE_LOADED", s.plist().to_str().unwrap())]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    assert!(!stdout(&out).contains("npm:"), "{}", stdout(&out));
}

#[test]
fn the_plugin_cache_goes_after_the_plugin_and_only_that_marketplace_folder() {
    let s = Scratch::new();
    let cache = s.home.join(".claude/plugins/cache/claude-self-reflect");
    let other = s.home.join(".claude/plugins/cache/someone-elses");
    write(&cache.join("csr-companion/10.1.0/plugin.json"), "{}");
    write(&other.join("p/1.0.0/plugin.json"), "{}");
    let before = s.snapshot();
    let plan = stdout(&s.run(&["--dry-run"]));
    assert_eq!(s.snapshot(), before);
    assert!(
        plan.contains(&format!("plugin cache: would delete {}", cache.display())),
        "{plan}"
    );
    let out = s.run(&["--yes"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(!cache.exists(), "{text}");
    assert!(other.join("p/1.0.0/plugin.json").exists());
    assert!(
        text.contains(&format!("plugin cache: removed {}", cache.display())),
        "{text}"
    );

    // The plugin is still installed: its cache stays and the run fails.
    let s = Scratch::new();
    write(
        &s.home
            .join(".claude/plugins/cache/claude-self-reflect/csr-companion/x"),
        "{}",
    );
    let out = s.run_env(
        &["--yes"],
        &[
            ("PLUGIN_STICKS", "1"),
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
        ],
    );
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        s.home
            .join(".claude/plugins/cache/claude-self-reflect/csr-companion/x")
            .exists(),
        "{text}"
    );
    assert!(text.contains("plugin cache: kept"), "{text}");
}

/// A `~/.claude.json` with a project-scoped `claude-self-reflect` entry from an old install, a
/// second server and a setting in the same project, another project that must not change, and
/// (when `with_user`) the user-scope entry too.
fn config_with_local_entry(dir: &Path, with_user: bool, local_entry: bool) -> Value {
    let mut project_servers =
        json!({"keep": {"type": "stdio", "command": "/usr/bin/keep", "args": []}});
    if local_entry {
        project_servers["claude-self-reflect"] = json!({
            "type": "stdio",
            "command": format!("{}/mcp-server/run-mcp.sh", dir.display()),
            "args": []
        });
    }
    let mut config = json!({
        "numStartups": 3,
        "mcpServers": {"other": {"type": "stdio", "command": "/usr/bin/other", "args": []}},
        "projects": {
            dir.to_str().unwrap(): {"allowedTools": ["x"], "mcpServers": project_servers},
            "/untouched": {"mcpServers": {"other": {"command": "/usr/bin/other"}}}
        }
    });
    if with_user {
        config["mcpServers"]["claude-self-reflect"] = serde_json::from_str(CSR_MCP).unwrap();
    }
    config
}

fn assert_only_the_local_key_went(s: &Scratch, dir: &Path) {
    let now: Value = serde_json::from_str(&s.claude_json_text()).unwrap();
    let want = config_with_local_entry(dir, false, false);
    assert_eq!(
        now, want,
        "only projects.<dir>.mcpServers.claude-self-reflect may change"
    );
}

#[test]
fn a_project_scoped_entry_is_removed_through_the_cli_run_from_its_folder() {
    let s = Scratch::new();
    let dir = s.root.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let text = |c: &Value| serde_json::to_string(c).unwrap();
    write(
        &s.home.join(".claude.json"),
        &text(&config_with_local_entry(&dir, true, true)),
    );
    let before = s.snapshot();
    let plan = stdout(&s.run(&["--dry-run"]));
    assert_eq!(s.snapshot(), before);
    assert!(
        plan.contains(&format!(
            "mcp (local): would run `claude mcp remove claude-self-reflect -s local` in {} \
             (the entry runs {}/mcp-server/run-mcp.sh)",
            dir.display(),
            dir.display()
        )),
        "{plan}"
    );
    assert!(!s.calls().contains("local-remove"));

    let after = text(&config_with_local_entry(&dir, false, false));
    let out = s.run_env(
        &["--yes"],
        &[
            ("FAKE_LOADED", s.plist().to_str().unwrap()),
            ("LOCAL_AFTER_JSON", &after),
        ],
    );
    let out_text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{out_text}");
    assert!(
        s.calls()
            .contains(&format!("local-remove cwd={}", dir.display())),
        "run from the project folder: {}",
        s.calls()
    );
    assert!(
        out_text.contains(&format!(
            "mcp (local): removed the 'claude-self-reflect' entry for {} (claude mcp remove -s local)",
            dir.display()
        )),
        "{out_text}"
    );
    assert_only_the_local_key_went(&s, &dir);
}

#[test]
fn a_project_scoped_entry_the_cli_cannot_remove_is_edited_out_by_hand_and_nothing_else() {
    // The CLI runs and leaves it; the user scope has no entry at all (the state a clean install
    // test found), so the scan must not hang off the user-scope step.
    let s = Scratch::new();
    let dir = s.root.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &s.home.join(".claude.json"),
        &serde_json::to_string(&config_with_local_entry(&dir, false, true)).unwrap(),
    );
    let out = s.run(&["--yes"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(s.calls().contains("local-remove"), "{}", s.calls());
    assert!(text.contains("by hand (the CLI left it; backup "), "{text}");
    assert_only_the_local_key_went(&s, &dir);
    assert_eq!(
        Scratch::named(&s.home, ".claude.json.csr-uninstall-").len(),
        1
    );

    // A project folder that no longer exists: the CLI cannot even start there.
    let s = Scratch::new();
    let dir = s.root.join("deleted-project");
    write(
        &s.home.join(".claude.json"),
        &serde_json::to_string(&config_with_local_entry(&dir, false, true)).unwrap(),
    );
    let out = s.run(&["--yes"]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(!s.calls().contains("local-remove"), "{}", s.calls());
    assert!(text.contains("by hand"), "{text}");
    assert_only_the_local_key_went(&s, &dir);
}

#[test]
fn running_csr_engine_processes_are_named_after_the_plan_and_never_signalled() {
    let ps = "424242 1 /opt/csr/csr-engine companion serve --sock /x/abc12345-77.sock\n\
              424243 1 /opt/csr/csr-engine\n\
              424244 1 /usr/bin/something-else --flag";
    let s = Scratch::new();
    let text = stdout(&s.run_env(&["--dry-run"], &[("FAKE_PS", ps)]));
    assert!(
        text.contains("csr-engine is still running in 2 process(es); none was signalled:"),
        "{text}"
    );
    assert!(
        text.contains("pid 424242  relay  session abc12345"),
        "{text}"
    );
    assert!(text.contains("pid 424243  mcp"), "{text}");
    assert!(!text.contains("424244"), "{text}");
    assert!(
        text.contains("restart these sessions; they keep running the removed csr-engine"),
        "{text}"
    );
    // Deleting the data folder: these processes will write it again.
    let out = s.run_env(&["--yes", "--purge"], &[("FAKE_PS", ps)]);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains(&format!(
            "restart these sessions; they will recreate {}",
            s.data().display()
        )),
        "{text}"
    );
    // The names come last, after every step line.
    let steps_end = text.rfind("binary:").expect("the binary line is printed");
    assert!(text.find("pid 424242").unwrap() > steps_end, "{text}");
    // Nothing running: no block.
    let s = Scratch::new();
    let text = stdout(&s.run(&["--dry-run"]));
    assert!(!text.contains("still running"), "{text}");
}
