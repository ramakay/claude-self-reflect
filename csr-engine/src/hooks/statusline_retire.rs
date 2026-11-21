//! Retire the CSR status line wrapper during `hook install --apply`.
//!
//! `/csr statusline on` (now gone) rewrote the user's `statusLine.command` to
//! `node "<HOME>/.claude-self-reflect/csr-companion/statusline.cjs" --sh '<original>'`.
//! The pane now reads context and rate-limit figures from the Claude Code mod, so
//! the wrapper has no job left. This module puts the original command back in the
//! settings value `hook install` is already rewriting, then deletes the leftovers.
//!
//! The parsing is a port of `unwrapCommand` / `unquote` in the removed TypeScript companion, the
//! code that wrote the wrapped form and read it back.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// What the settings file held, and what was done about it.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// No wrapped status line: nothing to do, nothing to say.
    NotWrapped,
    /// `statusLine.command` now holds this original command.
    Restored(String),
    /// Wrapped, but the original could not be found: settings are left alone.
    Unresolved,
}

/// The kept copy of the settings as they were before `/csr statusline on`.
fn backup_path(home: &Path) -> PathBuf {
    companion_dir(home).join("settings.before-statusline.json")
}

fn companion_dir(home: &Path) -> PathBuf {
    home.join(".claude-self-reflect").join("csr-companion")
}

/// True for a command that runs the CSR wrapper (`IS_WRAPPED` in the TypeScript companion:
/// `csr-companion/statusline.cjs`, `csr-companion/statusline.js` or
/// `csr-pane-statusline.js`).
pub fn is_wrapped(command: &str) -> bool {
    command.contains("csr-companion/statusline.cjs")
        || command.contains("csr-companion/statusline.js")
        || command.contains("csr-pane-statusline.js")
}

/// The command a wrapped one runs: the `--sh` word unquoted, or, in the argv form
/// set up by hand (`node csr-pane-statusline.js <command...>`), the words after
/// the wrapper. None when neither reads back.
pub fn unwrap_command(command: &str) -> Option<String> {
    if let Some(word) = sh_word(command) {
        return unquote(word.trim());
    }
    let (token, rest) = argv_form(command)?;
    if !is_wrapped(token) {
        return None;
    }
    let rest = rest.trim();
    (!rest.is_empty() && rest != "--sh").then(|| rest.to_string())
}

/// Everything after the first whitespace-delimited `--sh`: `/\s--sh\s+([\s\S]*)$/`.
fn sh_word(command: &str) -> Option<&str> {
    for (i, c) in command.char_indices() {
        if !c.is_whitespace() {
            continue;
        }
        let Some(after) = command[i + c.len_utf8()..].strip_prefix("--sh") else {
            continue;
        };
        let trimmed = after.trim_start();
        if trimmed.len() < after.len() {
            return Some(trimmed);
        }
    }
    None
}

/// `/^\s*node\s+("[^"]*"|'[^']*'|\S+)\s+([\s\S]+)$/`: the wrapper token and the
/// text after it, alternatives tried in the regex's order.
fn argv_form(command: &str) -> Option<(&str, &str)> {
    let after_node = command.trim_start().strip_prefix("node")?;
    let body = after_node.trim_start();
    if body.len() == after_node.len() {
        return None;
    }
    let first = body.chars().next()?;
    if first == '"' || first == '\'' {
        if let Some(close) = body[1..].find(first) {
            let (token, rest) = body.split_at(close + 2);
            if let Some(tail) = split_after_space(rest) {
                return Some((token, tail));
            }
        }
    }
    let end = body.find(char::is_whitespace)?;
    let (token, rest) = body.split_at(end);
    split_after_space(rest).map(|tail| (token, tail))
}

/// `\s+([\s\S]+)$` on `rest`: at least one whitespace char, then at least one more
/// char. Returns the text after the leading whitespace run, or the last
/// whitespace char when the run reaches the end (it then trims to nothing).
fn split_after_space(rest: &str) -> Option<&str> {
    let first = rest.chars().next().filter(|c| c.is_whitespace())?;
    let tail = &rest[first.len_utf8()..];
    if tail.is_empty() {
        return None;
    }
    let stripped = tail.trim_start();
    Some(if stripped.is_empty() { tail } else { stripped })
}

/// One shell word back to its text: `'...'` runs, `\`-escapes and bare characters.
/// None for anything else: an open quote, a second word, or an empty result.
pub fn unquote(word: &str) -> Option<String> {
    let chars: Vec<char> = word.chars().collect();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            let end = chars[i + 1..].iter().position(|&q| q == '\'')? + i + 1;
            text.extend(&chars[i + 1..end]);
            i = end;
        } else if c == '\\' && i + 1 < chars.len() {
            i += 1;
            text.push(chars[i]);
        } else if c.is_whitespace() {
            return None;
        } else {
            text.push(c);
        }
        i += 1;
    }
    (!text.is_empty()).then_some(text)
}

/// The settings' `statusLine.command`, when it is a non-blank string.
fn command_of(settings: &Value) -> Option<&str> {
    settings
        .get("statusLine")?
        .get("command")?
        .as_str()
        .filter(|command| !command.trim().is_empty())
}

/// The command "on" saved, when the kept settings parse and are not wrapped too.
fn backup_command(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(backup_path(home)).ok()?;
    let kept: Value = serde_json::from_str(&text).ok()?;
    command_of(&kept)
        .filter(|command| !is_wrapped(command))
        .map(str::to_string)
}

/// Put the original command back in `settings`. Only `statusLine.command`
/// changes; its other keys stay. The original is read off the wrapped command,
/// else taken from the settings "on" kept.
pub fn restore_in_place(settings: &mut Value, home: &Path) -> Outcome {
    let Some(command) = command_of(settings).filter(|command| is_wrapped(command)) else {
        return Outcome::NotWrapped;
    };
    let Some(original) = unwrap_command(command).or_else(|| backup_command(home)) else {
        return Outcome::Unresolved;
    };
    settings["statusLine"]["command"] = Value::String(original.clone());
    Outcome::Restored(original)
}

/// The line `hook install` prints about the status line, if any. `would` words the
/// dry run, which writes nothing.
pub fn report(outcome: &Outcome, settings_path: &Path, would: bool) -> Option<String> {
    match outcome {
        Outcome::NotWrapped => None,
        Outcome::Restored(original) if would => Some(format!(
            "CSR: --apply would restore your status line to your own command \
             (the CSR wrapper is retired): {original}"
        )),
        Outcome::Restored(original) => Some(format!(
            "CSR: status line restored to your own command \
             (the CSR wrapper is retired): {original}"
        )),
        Outcome::Unresolved => Some(format!(
            "CSR: the status line in {} still runs the retired CSR wrapper and its original \
             command could not be found. Set statusLine.command there to your own command by hand.",
            settings_path.display()
        )),
    }
}

/// True for a regular file; a symlink is never one, so it is never followed.
fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

fn remove_file(path: &Path, failures: &mut Vec<String>) {
    if !is_regular_file(path) {
        return;
    }
    if let Err(e) = std::fs::remove_file(path) {
        failures.push(format!("could not delete {}: {e}", path.display()));
    }
}

/// Delete what "on" left behind: the wrapper copy, the kept settings, and the kept
/// status-line files with their empty folder. Call after the settings write
/// succeeded. Returns one line per failed delete; a failure is never fatal.
pub fn delete_leftovers(home: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    let companion = companion_dir(home);
    remove_file(&companion.join("statusline.cjs"), &mut failures);
    remove_file(&backup_path(home), &mut failures);
    let base = home.join(".claude-self-reflect");
    for keep in [base.join("csr-shifts"), companion] {
        let dir = keep.join("statusline");
        // A symlinked folder is someone's own arrangement: leave it and what it holds.
        if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.file_type().is_dir()) {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    remove_file(&path, &mut failures);
                }
            }
        }
        // Only an empty folder goes; anything else in it is not ours to remove.
        if let Err(e) = std::fs::remove_dir(&dir) {
            if !matches!(
                e.kind(),
                std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
            ) {
                failures.push(format!("could not delete {}: {e}", dir.display()));
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// What `/csr statusline on` wrote: `wrapCommand` in the TypeScript companion.
    fn wrap(wrapper: &str, original: &str) -> String {
        format!(
            "node \"{wrapper}\" --sh '{}'",
            original.replace('\'', "'\\''")
        )
    }

    const WRAPPER: &str = "/Users/me/.claude-self-reflect/csr-companion/statusline.cjs";

    #[test]
    fn unquote_reads_single_quotes_escapes_and_bare_words() {
        assert_eq!(unquote("'a b'").as_deref(), Some("a b"));
        assert_eq!(unquote("bare").as_deref(), Some("bare"));
        assert_eq!(unquote(r"a\ b").as_deref(), Some("a b"));
        assert_eq!(unquote(r"'it'\''s'").as_deref(), Some("it's"));
        assert_eq!(unquote("'a'b'c'").as_deref(), Some("abc"));
    }

    #[test]
    fn unquote_refuses_what_on_never_wrote() {
        assert_eq!(unquote("two words"), None, "unquoted whitespace");
        assert_eq!(unquote("'open"), None, "unterminated quote");
        assert_eq!(unquote(""), None, "empty");
        assert_eq!(unquote("''"), None, "quotes around nothing");
        assert_eq!(unquote(r"trailing\").as_deref(), Some(r"trailing\"));
    }

    #[test]
    fn unwrap_round_trips_what_on_wrote() {
        for original in [
            "echo hi",
            "bash -c 'echo \"a b\" | tr a A'",
            "it's | cat && x",
            "~/bin/line --flag  two  spaces",
        ] {
            assert_eq!(
                unwrap_command(&wrap(WRAPPER, original)).as_deref(),
                Some(original),
                "{original}"
            );
        }
    }

    #[test]
    fn unwrap_reads_the_argv_form_set_up_by_hand() {
        assert_eq!(
            unwrap_command("node /x/csr-pane-statusline.js ccstatusline --compact").as_deref(),
            Some("ccstatusline --compact")
        );
        assert_eq!(
            unwrap_command(&format!("node \"{WRAPPER}\" my-line | jq .")).as_deref(),
            Some("my-line | jq .")
        );
        assert_eq!(
            unwrap_command(&format!("node '{WRAPPER}' my-line")).as_deref(),
            Some("my-line")
        );
        assert_eq!(
            unwrap_command("  node /a/csr-companion/statusline.js   run  ").as_deref(),
            Some("run")
        );
    }

    #[test]
    fn unwrap_gives_none_for_garbage_and_foreign_commands() {
        assert_eq!(unwrap_command("echo hi"), None, "a bare unwrapped command");
        assert_eq!(unwrap_command(""), None);
        assert_eq!(
            unwrap_command("node /other/script.js run"),
            None,
            "not our wrapper"
        );
        assert_eq!(
            unwrap_command("node /x/csr-pane-statusline.js"),
            None,
            "nothing after"
        );
        assert_eq!(unwrap_command("node /x/csr-pane-statusline.js   "), None);
        assert_eq!(unwrap_command("nodejs /x/csr-pane-statusline.js run"), None);
        assert_eq!(
            unwrap_command(&format!("node \"{WRAPPER}\" --sh 'open")),
            None
        );
        assert_eq!(
            unwrap_command(&format!("node \"{WRAPPER}\" --sh a b")),
            None
        );
        // No `--sh` word: the oracle's argv form would hand back "--sh" itself, which is
        // no status line command; the kept settings get their turn instead.
        assert_eq!(unwrap_command(&format!("node \"{WRAPPER}\" --sh")), None);
    }

    #[test]
    fn is_wrapped_matches_the_three_wrapper_names() {
        assert!(is_wrapped(&wrap(WRAPPER, "x")));
        assert!(is_wrapped("node /a/csr-companion/statusline.js x"));
        assert!(is_wrapped("node /a/csr-pane-statusline.js x"));
        assert!(!is_wrapped("echo csr-companion"));
        assert!(!is_wrapped("bash ~/.claude/statusline.sh"));
    }

    #[test]
    fn restore_changes_only_the_command_and_keeps_other_keys() {
        let home = tempfile::tempdir().unwrap();
        let mut settings = json!({
            "model": "opus",
            "statusLine": {
                "type": "command",
                "padding": 2,
                "command": wrap(WRAPPER, "bash -c 'echo \"a b\" | tr a A'"),
            },
        });
        let outcome = restore_in_place(&mut settings, home.path());
        assert_eq!(
            outcome,
            Outcome::Restored("bash -c 'echo \"a b\" | tr a A'".into())
        );
        assert_eq!(
            settings,
            json!({
                "model": "opus",
                "statusLine": {
                    "type": "command",
                    "padding": 2,
                    "command": "bash -c 'echo \"a b\" | tr a A'",
                },
            })
        );
        // Idempotent: the second pass sees an unwrapped line.
        assert_eq!(
            restore_in_place(&mut settings, home.path()),
            Outcome::NotWrapped
        );
    }

    #[test]
    fn restore_leaves_unwrapped_and_missing_status_lines_alone() {
        let home = tempfile::tempdir().unwrap();
        for mut settings in [
            json!({}),
            json!({"statusLine": "text"}),
            json!({"statusLine": {"command": 7}}),
            json!({"statusLine": {"command": "   "}}),
            json!({"statusLine": {"command": "echo hi"}}),
        ] {
            let before = settings.clone();
            assert_eq!(
                restore_in_place(&mut settings, home.path()),
                Outcome::NotWrapped
            );
            assert_eq!(settings, before);
        }
    }

    #[test]
    fn restore_falls_back_to_the_kept_settings() {
        let home = tempfile::tempdir().unwrap();
        let kept = companion_dir(home.path());
        std::fs::create_dir_all(&kept).unwrap();
        // A wrapped command that does not read back (second word after --sh).
        let wrapped = format!("node \"{WRAPPER}\" --sh a b");
        let mut settings = json!({"statusLine": {"type": "command", "command": wrapped}});

        // No backup: unresolved, settings untouched.
        let before = settings.clone();
        assert_eq!(
            restore_in_place(&mut settings, home.path()),
            Outcome::Unresolved
        );
        assert_eq!(settings, before);

        // A backup that is itself wrapped does not count.
        std::fs::write(
            backup_path(home.path()),
            json!({"statusLine": {"command": wrapped}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            restore_in_place(&mut settings, home.path()),
            Outcome::Unresolved
        );

        // A backup that does not parse does not count.
        std::fs::write(backup_path(home.path()), "{not json").unwrap();
        assert_eq!(
            restore_in_place(&mut settings, home.path()),
            Outcome::Unresolved
        );

        std::fs::write(
            backup_path(home.path()),
            json!({"statusLine": {"command": "my-line"}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            restore_in_place(&mut settings, home.path()),
            Outcome::Restored("my-line".into())
        );
        assert_eq!(settings["statusLine"]["command"], "my-line");
        assert_eq!(settings["statusLine"]["type"], "command");
    }

    #[test]
    fn report_words_each_outcome() {
        let path = Path::new("/h/.claude/settings.json");
        assert_eq!(report(&Outcome::NotWrapped, path, false), None);
        let done = report(&Outcome::Restored("x".into()), path, false).unwrap();
        assert!(
            done.starts_with("CSR: status line restored to your own command"),
            "{done}"
        );
        assert!(done.ends_with(": x"), "{done}");
        let dry = report(&Outcome::Restored("x".into()), path, true).unwrap();
        assert!(dry.contains("would restore"), "{dry}");
        let stuck = report(&Outcome::Unresolved, path, false).unwrap();
        assert!(stuck.contains("/h/.claude/settings.json"), "{stuck}");
        assert!(stuck.contains("by hand"), "{stuck}");
        assert!(!stuck.contains('\n'));
    }

    #[test]
    fn delete_leftovers_removes_only_what_on_left_and_spares_the_rest() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join(".claude-self-reflect");
        let companion = base.join("csr-companion");
        let shifts = base.join("csr-shifts");
        std::fs::create_dir_all(companion.join("statusline")).unwrap();
        std::fs::create_dir_all(shifts.join("statusline")).unwrap();
        std::fs::write(companion.join("statusline.cjs"), "x").unwrap();
        std::fs::write(companion.join("settings.before-statusline.json"), "{}").unwrap();
        std::fs::write(companion.join("keep-me.txt"), "x").unwrap();
        std::fs::write(companion.join("statusline").join("a.json"), "{}").unwrap();
        std::fs::write(shifts.join("statusline").join("b.json"), "{}").unwrap();
        std::fs::write(shifts.join("statusline").join("note.txt"), "x").unwrap();

        assert!(delete_leftovers(home.path()).is_empty());

        assert!(!companion.join("statusline.cjs").exists());
        assert!(!companion.join("settings.before-statusline.json").exists());
        assert!(
            !companion.join("statusline").exists(),
            "emptied folder goes"
        );
        assert!(companion.join("keep-me.txt").exists());
        assert!(!shifts.join("statusline").join("b.json").exists());
        assert!(
            shifts.join("statusline").join("note.txt").exists(),
            "foreign file stays"
        );
        assert!(shifts.join("statusline").exists(), "non-empty folder stays");
        // A second pass finds nothing and fails nothing.
        assert!(delete_leftovers(home.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn delete_leftovers_never_follows_a_symlink() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let companion = home
            .path()
            .join(".claude-self-reflect")
            .join("csr-companion");
        std::fs::create_dir_all(&companion).unwrap();
        let target = outside.path().join("precious.cjs");
        std::fs::write(&target, "x").unwrap();
        std::os::unix::fs::symlink(&target, companion.join("statusline.cjs")).unwrap();
        let folder = outside.path().join("folder");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("c.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&folder, companion.join("statusline")).unwrap();

        assert!(delete_leftovers(home.path()).is_empty());

        assert!(target.exists(), "the symlink's target survives");
        assert!(companion.join("statusline.cjs").symlink_metadata().is_ok());
        assert!(
            folder.join("c.json").exists(),
            "a symlinked folder is not walked"
        );
        assert!(companion.join("statusline").symlink_metadata().is_ok());
    }
}
