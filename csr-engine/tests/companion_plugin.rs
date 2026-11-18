//! `csr-engine companion plugin install`: the marketplace the binary writes and the plugin it
//! installs from it. The first tests drive the real binary against a fake `claude`; the last
//! two run the real Claude Code plugin CLI under a throwaway HOME and skip without it.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const ID: &str = "csr-companion@claude-self-reflect";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn engine() -> &'static str {
    env!("CARGO_BIN_EXE_csr-engine")
}

fn marketplace_dir(home: &Path) -> PathBuf {
    home.join(".claude-self-reflect/csr-companion/marketplace")
}

fn install(home: &Path, path: Option<&Path>) -> Output {
    let mut cmd = Command::new(engine());
    cmd.env("HOME", home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .args(["companion", "plugin", "install"]);
    if let Some(dir) = path {
        let old = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{old}", dir.display()));
    }
    cmd.output().unwrap()
}

/// A `claude` that keeps its state in files: the marketplace listing, and the plugin once an
/// install ran, cached with the register.js the binary wrote.
fn fake_claude(dir: &Path, home: &Path, fail_add: bool) {
    let script = format!(
        r#"#!/bin/sh
D='{d}'
echo "$@" >> "$D/argv.log"
case "$2" in
  marketplace)
    case "$3" in
      list)
        if [ -f "$D/added" ]; then
          echo '[{{"name":"claude-self-reflect","source":"directory","path":"{market}"}}]'
        else echo '[]'; fi ;;
      add)
        if [ {fail_add} = true ]; then echo "no such folder" >&2; exit 1; fi
        touch "$D/added" ;;
    esac ;;
  list)
    if [ -f "$D/installed" ]; then
      echo '[{{"id":"{ID}","version":"{VERSION}","scope":"user","installPath":"{d}/cache"}}]'
    else echo '[]'; fi ;;
  install)
    mkdir -p "$D/cache/hooks"
    cp '{market}/plugins/csr-companion/hooks/register.js' "$D/cache/hooks/register.js"
    touch "$D/installed"
    echo '{{"command":"install","outcome":"ok","plugin":"{ID}","scope":"user","message":"ok"}}' ;;
esac
"#,
        d = dir.display(),
        market = marketplace_dir(home).display(),
    );
    let bin = dir.join("claude");
    fs::write(&bin, script).unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn install_writes_the_marketplace_and_asks_claude_for_the_plugin() {
    let home = tempfile::tempdir().unwrap();
    let fake = tempfile::tempdir().unwrap();
    fake_claude(fake.path(), home.path(), false);
    let out = install(home.path(), Some(fake.path()));
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(stdout.contains("plugin installed: "), "{stdout}");
    assert_eq!(
        stdout.lines().last(),
        Some("run /reload-plugins in open Claude Code sessions")
    );

    let market = marketplace_dir(home.path());
    let listing: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(market.join(".claude-plugin/marketplace.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(listing["name"], "claude-self-reflect");
    assert_eq!(listing["plugins"][0]["name"], "csr-companion");
    assert_eq!(listing["plugins"][0]["version"], VERSION);
    let plugin = market.join("plugins/csr-companion");
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(plugin.join(".claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["version"], VERSION);
    assert!(manifest.get("types").is_none());
    assert!(manifest["userConfig"]["dreams"].is_object());
    let relay = fs::read_to_string(plugin.join("hooks/register.js")).unwrap();
    assert!(
        relay.contains(&format!("const ENGINE = '{}'", engine())),
        "{relay}"
    );
    assert!(relay.contains(&format!("const V = '{VERSION}'")));
    for folder in [&market, &plugin, &plugin.join("hooks")] {
        let mode = fs::metadata(folder).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", folder.display());
    }

    let calls = fs::read_to_string(fake.path().join("argv.log")).unwrap();
    let calls: Vec<&str> = calls.lines().collect();
    assert_eq!(
        calls[1],
        format!("plugin marketplace add {}", market.display())
    );
    assert_eq!(calls[3], format!("plugin install {ID} --json"));

    // a second run finds the plugin current: nothing changes, no reload line
    let again = install(home.path(), Some(fake.path()));
    let stdout = text(&again.stdout);
    assert!(again.status.success(), "{stdout}");
    assert!(stdout.contains("plugin already current: "), "{stdout}");
    assert!(!stdout.contains("/reload-plugins"), "{stdout}");
}

#[test]
fn a_failing_claude_step_is_reported_and_exits_non_zero() {
    let home = tempfile::tempdir().unwrap();
    let fake = tempfile::tempdir().unwrap();
    fake_claude(fake.path(), home.path(), true);
    let out = install(home.path(), Some(fake.path()));
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("not installed: `claude plugin marketplace add` failed"),
        "{stderr}"
    );
    // the marketplace is still written, and the plugin step never ran
    assert!(marketplace_dir(home.path())
        .join(".claude-plugin/marketplace.json")
        .is_file());
    let calls = fs::read_to_string(fake.path().join("argv.log")).unwrap();
    assert!(!calls.contains("install"), "{calls}");
}

fn claude_available() -> bool {
    let home = tempfile::tempdir().unwrap();
    Command::new("claude")
        .env("HOME", home.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
}

fn claude_in(home: &Path, args: &[&str]) -> Output {
    Command::new("claude")
        .env("HOME", home)
        .env_remove("CLAUDE_CONFIG_DIR")
        .args(args)
        .output()
        .unwrap()
}

/// The plugin entry `claude plugin list --json` shows for this plugin.
fn listed(home: &Path) -> serde_json::Value {
    let out = claude_in(home, &["plugin", "list", "--json"]);
    let list: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    list.as_array()
        .and_then(|l| l.iter().find(|e| e["id"] == ID))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

fn cached_relay(entry: &serde_json::Value) -> String {
    let dir = entry["installPath"].as_str().unwrap();
    fs::read_to_string(Path::new(dir).join("hooks/register.js")).unwrap()
}

#[test]
fn claude_installs_the_plugin_from_the_marketplace_the_binary_wrote() {
    if !claude_available() {
        eprintln!("claude is not on PATH: skipped");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let out = install(home.path(), None);
    assert!(
        out.status.success(),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    let entry = listed(home.path());
    assert_eq!(entry["version"], VERSION, "{entry}");
    let written = marketplace_dir(home.path()).join("plugins/csr-companion/hooks/register.js");
    assert_eq!(cached_relay(&entry), fs::read_to_string(written).unwrap());
    let validated = claude_in(
        home.path(),
        &[
            "plugin",
            "validate",
            marketplace_dir(home.path()).to_str().unwrap(),
        ],
    );
    assert!(validated.status.success(), "{}", text(&validated.stdout));
}

#[test]
fn claude_update_moves_the_cache_when_the_marketplace_version_and_relay_change() {
    if !claude_available() {
        eprintln!("claude is not on PATH: skipped");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    assert!(install(home.path(), None).status.success());
    let before = listed(home.path());
    // Drift the marketplace the way a newer engine would, then let Claude update from it.
    let market = marketplace_dir(home.path());
    let manifest = market.join("plugins/csr-companion/.claude-plugin/plugin.json");
    let relay = market.join("plugins/csr-companion/hooks/register.js");
    let listing = market.join(".claude-plugin/marketplace.json");
    for file in [&manifest, &listing] {
        let body = fs::read_to_string(file).unwrap();
        fs::write(file, body.replace(VERSION, "99.0.0")).unwrap();
    }
    let body = fs::read_to_string(&relay).unwrap();
    fs::write(&relay, format!("{body}// changed by the harness\n")).unwrap();
    let update = claude_in(home.path(), &["plugin", "update", ID, "--json"]);
    assert!(update.status.success(), "{}", text(&update.stdout));
    let after = listed(home.path());
    assert_eq!(after["version"], "99.0.0", "{after}");
    assert_ne!(after["installPath"], before["installPath"]);
    assert!(cached_relay(&after).contains("// changed by the harness"));
    // and the next install puts the engine's own plugin back
    let out = install(home.path(), None);
    assert!(
        out.status.success(),
        "{}{}",
        text(&out.stdout),
        text(&out.stderr)
    );
    assert_eq!(listed(home.path())["version"], VERSION);
}

#[test]
fn claude_keeps_a_disabled_plugin_disabled_when_the_binary_moves() {
    if !claude_available() {
        eprintln!("claude is not on PATH: skipped");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    assert!(install(home.path(), None).status.success());
    let off = claude_in(home.path(), &["plugin", "disable", ID]);
    assert!(off.status.success(), "{}", text(&off.stderr));
    assert_eq!(listed(home.path())["enabled"], false);
    // Same version, other path: the relay text differs, so the plugin is reinstalled.
    let moved = tempfile::tempdir().unwrap();
    let copy = moved.path().join("csr-engine");
    fs::copy(engine(), &copy).unwrap();
    let out = Command::new(&copy)
        .env("HOME", home.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .args(["companion", "plugin", "install"])
        .output()
        .unwrap();
    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", text(&out.stderr));
    assert!(stdout.contains("plugin reinstalled: "), "{stdout}");
    let entry = listed(home.path());
    assert_eq!(entry["enabled"], false, "{entry}");
    assert_eq!(entry["version"], VERSION);
    let relay = cached_relay(&entry);
    let engine_line = relay
        .lines()
        .find(|l| l.starts_with("const ENGINE = "))
        .unwrap();
    assert!(engine_line.contains("/csr-engine'"), "{engine_line}");
    assert!(
        !engine_line.contains(engine()),
        "the relay still starts the old binary: {engine_line}"
    );
}
