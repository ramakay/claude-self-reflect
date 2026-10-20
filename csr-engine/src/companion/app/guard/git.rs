//! Git as the push guard reads it: port of the git half of `hooks/coach.ts` (git's own option
//! tables, `gitOptions`, `gitCall`, `gitEnv`). Like the shell half it is a literal port: an option
//! git would refuse is skipped, not guessed at.

use super::shell::{head_at, is_word, last_segment, split_ws, Word};
use serde_json::Value;
use std::{collections::HashMap, sync::LazyLock};

/// A `-c name=value` as the string, a `--config-env=name=VARIABLE` as `Env("name=VARIABLE")`, its
/// value read when the hook asks git: git takes the last of two, so the order is kept.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigArg {
    Set(String),
    Env(String),
}

impl ConfigArg {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Set(text) => Value::String(text.clone()),
            Self::Env(text) => serde_json::json!({ "env": text }),
        }
    }
}

/// An insertion-ordered map, as a JavaScript object with names that are not numbers is: a name
/// set again keeps its place and takes the new value.
#[derive(Clone, Debug, PartialEq)]
pub struct Ordered<V>(pub Vec<(String, V)>);

impl<V> Default for Ordered<V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<V> Ordered<V> {
    pub fn set(&mut self, name: &str, value: V) {
        match self.0.iter_mut().find(|(n, _)| n == name) {
            Some(slot) => slot.1 = value,
            None => self.0.push((name.to_string(), value)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&V> {
        self.0.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A shell assignment's value, with whether the shell expands something in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Assigned {
    pub value: String,
    pub expands: bool,
}

/// `/^([A-Za-z_]\w*)=([\s\S]*)$/`: the name and the value.
pub fn assignment(text: &str) -> Option<(&str, &str)> {
    let mut chars = text.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let name = text.chars().take_while(|&c| is_word(c)).count();
    let rest = text[name..].strip_prefix('=')?;
    Some((&text[..name], rest))
}

/// The assignments among `words`, by name.
pub fn assigned(words: &[Word]) -> Ordered<Assigned> {
    let mut out = Ordered::default();
    for word in words {
        if let Some((name, value)) = assignment(&word.text) {
            out.set(
                name,
                Assigned {
                    value: value.to_string(),
                    expands: word.expands,
                },
            );
        }
    }
    out
}

// ---------------------------------------------------------------- hooksPath

/// `/^core\.hookspath(=|$)/i`: core.hooksPath set for one call points git at other hooks, or none.
pub fn hooks_path(set: &str) -> bool {
    let lower = set.to_ascii_lowercase();
    lower
        .strip_prefix("core.hookspath")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('='))
}

/// `/^GIT_CONFIG_(?:KEY_\d+=core\.hookspath$|PARAMETERS=.*core\.hookspath)/i`.
pub fn hooks_path_env(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("git_config_") else {
        return false;
    };
    if let Some(rest) = rest.strip_prefix("key_") {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        return digits > 0 && &rest[digits..] == "=core.hookspath";
    }
    let Some(rest) = rest.strip_prefix("parameters=") else {
        return false;
    };
    // `.*` stops at a line terminator.
    rest.char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(rest.len()))
        .take_while(|&at| !rest[..at].contains(['\n', '\r', '\u{2028}', '\u{2029}']))
        .any(|at| rest[at..].starts_with("core.hookspath"))
}

// ---------------------------------------------------------------- git's own options

/// Each subcommand's long options as `git <sub> -h` lists them (git 2.50, hidden ones added).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Flag,
    Value,
    /// Only after `=`; `--gpg-sign --no-verify` skips the hooks.
    Optional,
}

/// `[no-]` for an option git also takes as no-<name>, `=` for one whose value is required (a
/// separate next word is it), `=?` for one whose value is optional. Git takes a long option by any
/// prefix no other name shares, and refuses one that two share.
pub struct Longs(HashMap<String, Kind>);

impl Longs {
    fn of(spec: &str) -> Self {
        let mut table = HashMap::new();
        for entry in spec.split_whitespace() {
            let bare = entry.replacen("[no-]", "", 1);
            let name = bare
                .strip_suffix("=?")
                .or_else(|| bare.strip_suffix('='))
                .unwrap_or(&bare)
                .to_string();
            let kind = if bare.ends_with("=?") {
                Kind::Optional
            } else if bare.ends_with('=') {
                Kind::Value
            } else {
                Kind::Flag
            };
            table.insert(name.clone(), kind);
            if entry.starts_with("[no-]") {
                table.insert(format!("no-{name}"), Kind::Flag);
            }
        }
        Self(table)
    }

    fn has(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }
}

pub static PUSH_OPTIONS: LazyLock<Longs> = LazyLock::new(|| {
    Longs::of(
        "[no-]verbose [no-]quiet [no-]repo= [no-]all [no-]branches [no-]mirror [no-]delete [no-]tags
  [no-]dry-run [no-]porcelain [no-]force [no-]force-with-lease=? [no-]force-if-includes [no-]recurse-submodules= [no-]thin
  [no-]receive-pack= [no-]exec= [no-]set-upstream [no-]progress [no-]prune no-verify verify [no-]follow-tags [no-]signed=?
  [no-]atomic [no-]push-option= ipv4 ipv6",
    )
});
pub static COMMIT_OPTIONS: LazyLock<Longs> = LazyLock::new(|| {
    Longs::of(
        "[no-]quiet [no-]verbose [no-]file= [no-]author= [no-]date= [no-]message= [no-]reedit-message=
  [no-]reuse-message= [no-]fixup= [no-]squash= [no-]reset-author trailer= [no-]signoff [no-]template= [no-]edit [no-]cleanup=
  [no-]status [no-]gpg-sign=? [no-]all [no-]include [no-]interactive [no-]patch [no-]only no-verify verify [no-]dry-run
  [no-]short [no-]branch [no-]ahead-behind [no-]porcelain [no-]long [no-]null [no-]amend no-post-rewrite post-rewrite
  [no-]untracked-files=? [no-]pathspec-from-file= [no-]pathspec-file-nul allow-empty allow-empty-message",
    )
});
pub static MERGE_OPTIONS: LazyLock<Longs> = LazyLock::new(|| {
    Longs::of(
        "[no-]stat [no-]summary [no-]log=? [no-]squash [no-]commit [no-]edit [no-]cleanup= [no-]ff ff-only
  [no-]rerere-autoupdate [no-]verify-signatures [no-]strategy= [no-]strategy-option= [no-]message= file= [no-]into-name=
  [no-]verbose [no-]quiet [no-]abort [no-]quit [no-]continue [no-]allow-unrelated-histories [no-]progress [no-]gpg-sign=?
  [no-]autostash [no-]overwrite-ignore [no-]signoff no-verify verify",
    )
});

/// Short options: those whose value is required (the rest of the cluster, else the next word),
/// those whose value is optional (the rest of the cluster only), and the long name each flag
/// stands for.
pub struct Shorts {
    value: &'static str,
    optional: &'static str,
    names: &'static [(char, &'static str)],
}

pub const PUSH_SHORTS: Shorts = Shorts {
    value: "o",
    optional: "",
    names: &[
        ('n', "dry-run"),
        ('d', "delete"),
        ('f', "force"),
        ('u', "set-upstream"),
        ('v', "verbose"),
        ('q', "quiet"),
    ],
};
/// `-uno` is -u's mode "no".
pub const COMMIT_SHORTS: Shorts = Shorts {
    value: "mFcCt",
    optional: "Su",
    names: &[('n', "no-verify")],
};
/// Merge's -n is --no-stat.
pub const MERGE_SHORTS: Shorts = Shorts {
    value: "msXF",
    optional: "S",
    names: &[],
};

/// What a subcommand's words came to.
#[derive(Debug, Default)]
pub struct Options {
    /// Each flag's last state.
    pub on: HashMap<String, bool>,
    /// Whether --no-verify won.
    pub skip: bool,
    pub places: Vec<String>,
}

/// `/^\d*[<>]/`: `2>&1` and the like.
fn redirection(word: &str) -> bool {
    let digits = word.chars().take_while(char::is_ascii_digit).count();
    word[digits..].starts_with(['<', '>'])
}

/// One git subcommand's words, read as git reads them: up to `--`, never inside an option's value,
/// each flag set in order so the last of a pair wins.
pub fn git_options(rest: &[String], longs: &Longs, shorts: &Shorts) -> Options {
    let mut out = Options::default();
    let set = |out: &mut Options, name: &str| {
        if name == "no-verify" || name == "verify" {
            out.skip = name == "no-verify";
        } else if let Some(bare) = name.strip_prefix("no-").filter(|bare| longs.has(bare)) {
            out.on.insert(bare.to_string(), false);
        } else {
            out.on.insert(name.to_string(), true);
        }
    };
    let mut k = 0;
    while k < rest.len() {
        let w = rest[k].as_str();
        if w == "--" {
            out.places.extend(rest[k + 1..].iter().cloned());
            break;
        }
        if let Some(body) = w.strip_prefix("--") {
            let eq = body.find('=');
            let typed = &body[..eq.unwrap_or(body.len())];
            let sharing: Vec<&String> = longs.0.keys().filter(|n| n.starts_with(typed)).collect();
            let name = if longs.has(typed) {
                Some(typed.to_string())
            } else if sharing.len() == 1 {
                Some(sharing[0].clone())
            } else {
                None
            };
            // unknown, or a prefix two names share: git refuses the command
            if let Some(name) = name {
                set(&mut out, &name);
                if longs.0.get(&name) == Some(&Kind::Value) && eq.is_none() {
                    k += 1;
                }
            }
        } else if w.starts_with('-') && w.chars().nth(1).is_some_and(|c| c != '-') {
            let letters: Vec<char> = w.chars().collect();
            for (i, &letter) in letters.iter().enumerate().skip(1) {
                if let Some((_, long)) = shorts.names.iter().find(|(l, _)| *l == letter) {
                    set(&mut out, long);
                }
                if shorts.value.contains(letter) {
                    if i == letters.len() - 1 {
                        k += 1; // `-m msg`: msg is the value
                    }
                    break;
                }
                if shorts.optional.contains(letter) {
                    break;
                }
            }
        } else if !redirection(w) {
            out.places.push(w.to_string());
        }
        k += 1;
    }
    out
}

/// A push's words: whether it is a dry run, skips hooks, sends every branch (--all, --branches,
/// --mirror), and the remote and refspecs it names.
pub struct PushWords {
    pub dry: bool,
    pub skip: bool,
    pub every: bool,
    pub remote: Option<String>,
    pub specs: Vec<String>,
}

pub fn push_words(rest: &[String]) -> PushWords {
    let Options { on, skip, places } = git_options(rest, &PUSH_OPTIONS, &PUSH_SHORTS);
    let is_on = |name: &str| on.get(name) == Some(&true);
    PushWords {
        dry: is_on("dry-run"),
        skip,
        every: ["all", "branches", "mirror"].into_iter().any(is_on),
        remote: places.first().cloned(),
        specs: places.iter().skip(1).cloned().collect(),
    }
}

// ---------------------------------------------------------------- one git command

/// What one command's words say about git.
#[derive(Debug)]
pub struct GitCall {
    pub sub: String,
    pub rest: Vec<String>,
    pub dir: Option<String>,
    /// `-C $DIR`: where it runs is unknown.
    pub dir_expands: bool,
    pub hooks_off: bool,
    /// `-c` and `--config-env`, in the order given.
    pub config: Vec<ConfigArg>,
    /// A `-c` or `--config-env` argument the shell expands: what it sets is unknown.
    pub config_expands: bool,
    /// The variables set before it (`DEST=... git`, `env DEST=... git`).
    pub env: Ordered<Assigned>,
}

fn takes_one(option: &str) -> bool {
    matches!(
        option,
        "-C" | "-c" | "--config-env" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
    )
}

/// What one command's words say about git: the subcommand, what follows it, the -C directory, the
/// -c and --config-env settings in their order, and the variables set before it; none when it is
/// no git.
pub fn git_call(tokens: &[Word]) -> Option<GitCall> {
    let words: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
    let head = head_at(&words);
    let mut i = head;
    if last_segment(words.get(i).copied().unwrap_or("")) != "git" {
        return None;
    }
    let mut dir: Option<String> = None;
    let mut dir_expands = false;
    let mut hooks_off = words[..head.min(words.len())]
        .iter()
        .any(|w| hooks_path_env(w));
    let mut config = Vec::new();
    let mut config_expands = false;
    i += 1;
    while i < words.len() && words[i].starts_with('-') {
        let w = words[i];
        let next = tokens.get(i + 1);
        if w == "-C" {
            dir = next.map(|n| n.text.clone());
            dir_expands = dir_expands || next.is_some_and(|n| n.expands);
        }
        let joined = w.starts_with("--config-env=");
        let set = if w == "-c" || w == "--config-env" {
            next.map_or("", |n| n.text.as_str())
        } else if joined {
            &w["--config-env=".len()..]
        } else {
            ""
        };
        hooks_off = hooks_off || hooks_path(set);
        if w == "-c" || w == "--config-env" || joined {
            let expands = if joined {
                tokens[i].expands
            } else {
                next.is_some_and(|n| n.expands)
            };
            config_expands = config_expands || expands;
        }
        if w == "-c" && next.is_some() {
            config.push(ConfigArg::Set(set.to_string()));
        } else if w == "--config-env" || joined {
            config.push(ConfigArg::Env(set.to_string()));
        }
        i += if takes_one(w) { 2 } else { 1 };
    }
    if i >= words.len() {
        return None;
    }
    Some(GitCall {
        sub: words[i].to_string(),
        rest: words[i + 1..].iter().map(|w| w.to_string()).collect(),
        dir,
        dir_expands,
        hooks_off,
        config,
        config_expands,
        env: assigned(&tokens[..head.min(tokens.len())]),
    })
}

/// The variables git reads that a command line sets: those its --config-env settings name, and
/// GIT_* (GIT_CONFIG_COUNT/KEY_n/VALUE_n set config). `unknown`: one of them the shell expands
/// (`GIT_CONFIG_COUNT=$N`). Only the named ones and GIT_CONFIG_* go to the hook's git: never PATH
/// and the like.
pub fn git_env(config: &[ConfigArg], set: &Ordered<Assigned>) -> (Ordered<String>, bool) {
    let named: Vec<&str> = config
        .iter()
        .filter_map(|arg| match arg {
            ConfigArg::Env(text) => Some(text.split_once('=').map_or(text.as_str(), |p| p.1)),
            ConfigArg::Set(_) => None,
        })
        .collect();
    let read: Vec<&(String, Assigned)> = set
        .0
        .iter()
        .filter(|(name, _)| named.contains(&name.as_str()) || name.starts_with("GIT_"))
        .collect();
    let mut env = Ordered::default();
    for (name, value) in &read {
        if named.contains(&name.as_str()) || name.starts_with("GIT_CONFIG") {
            env.set(name, value.value.clone());
        }
    }
    (env, read.iter().any(|(_, v)| v.expands))
}

/// A refspec's destination: `+HEAD:refs/heads/release/v9` -> release/v9; `release/v9` ->
/// release/v9.
pub fn destination(spec: &str) -> String {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    let target = spec.rsplit(':').next().unwrap_or("");
    target
        .strip_prefix("refs/heads/")
        .unwrap_or(target)
        .to_string()
}

/// `upstream.trim().replace(/^[^/]+\//, '')`: the remote's name off `origin/release/v9`.
pub fn off_remote(upstream: &str) -> &str {
    match upstream.find('/') {
        Some(at) if at > 0 => &upstream[at + 1..],
        _ => upstream,
    }
}

/// The last word of a line: `line.trim().split(/\s+/).pop()`.
pub fn last_word(line: &str) -> &str {
    split_ws(super::shell::trim(line)).pop().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::app::guard::shell::split_words;

    fn call(text: &str) -> Option<GitCall> {
        git_call(&split_words(text).remove(0))
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn hooks_path_is_matched_as_the_case_insensitive_regexes_match_it() {
        assert!(hooks_path("core.hooksPath=/dev/null"));
        assert!(hooks_path("core.hookspath="));
        assert!(hooks_path("CORE.HOOKSPATH"));
        assert!(!hooks_path("core.hooksPathX=1"));
        assert!(!hooks_path("xcore.hooksPath=1"));
        assert!(hooks_path_env("GIT_CONFIG_KEY_0=core.hooksPath"));
        assert!(hooks_path_env("git_config_key_12=CORE.HOOKSPATH"));
        assert!(!hooks_path_env("GIT_CONFIG_KEY_=core.hooksPath"));
        assert!(!hooks_path_env("GIT_CONFIG_KEY_0=core.hooksPath/"));
        assert!(!hooks_path_env("GIT_CONFIG_VALUE_0=core.hooksPath"));
        assert!(hooks_path_env("GIT_CONFIG_PARAMETERS='core.hookspath=/x'"));
        assert!(hooks_path_env("GIT_CONFIG_PARAMETERS=core.hookspath"));
        assert!(!hooks_path_env("GIT_CONFIG_PARAMETERS='a=b'"));
        // `.*` does not cross a line end
        assert!(!hooks_path_env("GIT_CONFIG_PARAMETERS=a\nb=core.hookspath"));
        assert!(!hooks_path_env(
            "GIT_CONFIG_PARAMETERS=a\u{2028}core.hookspath"
        ));
    }

    #[test]
    fn an_assignment_is_a_name_an_equals_and_anything() {
        assert_eq!(assignment("A=1"), Some(("A", "1")));
        assert_eq!(assignment("_a1=b=c"), Some(("_a1", "b=c")));
        assert_eq!(assignment("A="), Some(("A", "")));
        assert_eq!(assignment("1A=1"), None);
        assert_eq!(assignment("=1"), None);
        assert_eq!(assignment("A-B=1"), None);
        assert_eq!(assignment("A"), None);
        assert_eq!(assignment("A=line\nbreak"), Some(("A", "line\nbreak")));
    }

    #[test]
    fn a_name_set_again_keeps_its_place_and_takes_the_new_value() {
        let mut map = Ordered::default();
        map.set("a", 1);
        map.set("b", 2);
        map.set("a", 3);
        assert_eq!(map.0, [("a".to_string(), 3), ("b".to_string(), 2)]);
    }

    #[test]
    fn long_options_are_taken_by_any_prefix_no_other_name_shares() {
        let o = |words: &[&str]| git_options(&strings(words), &COMMIT_OPTIONS, &COMMIT_SHORTS);
        assert!(o(&["--no-veri"]).skip);
        assert!(!o(&["--no-ver"]).skip); // no-verify or no-verbose: git refuses it
        assert!(!o(&["--no-verify", "--veri"]).skip);
        assert!(!o(&["--verify", "--no-verify", "--verify"]).skip);
        assert!(o(&["--verify", "--no-verify"]).skip);
        assert!(o(&["--no-verify=x"]).skip);
        assert!(!o(&["--=x", "--"]).skip);
        let p = |words: &[&str]| push_words(&strings(words));
        assert!(p(&["--mirr", "origin"]).every);
        assert!(p(&["--al"]).every);
        assert!(!p(&["--all", "--no-all"]).every);
        assert!(p(&["--dry"]).dry);
        assert!(!p(&["-n", "--no-dry-run"]).dry);
    }

    #[test]
    fn short_clusters_stop_at_an_option_that_takes_a_value() {
        let o = |words: &[&str]| git_options(&strings(words), &COMMIT_OPTIONS, &COMMIT_SHORTS);
        assert!(o(&["-an", "-m", "x"]).skip);
        assert!(o(&["-nm", "x"]).skip);
        assert!(!o(&["-mn"]).skip);
        assert!(!o(&["-m", "n"]).skip);
        assert!(!o(&["-uno"]).skip);
        assert!(!o(&["-m", "x", "--", "-n"]).skip);
        assert_eq!(o(&["-m", "x", "--", "-n"]).places, ["-n"]);
        assert!(!o(&["-Sabc"]).skip);
        assert_eq!(o(&["a", "2>&1", "&>x", "-"]).places, ["a", "&>x", "-"]);
    }

    #[test]
    fn a_git_call_has_its_directory_settings_and_the_variables_set_before_it() {
        let c =
            call("A=1 env B=2 git -C /r -c user.name=me --config-env=remote.o.push=DEST push o")
                .unwrap();
        assert_eq!(c.sub, "push");
        assert_eq!(c.rest, ["o"]);
        assert_eq!(c.dir.as_deref(), Some("/r"));
        assert_eq!(
            c.config,
            [
                ConfigArg::Set("user.name=me".into()),
                ConfigArg::Env("remote.o.push=DEST".into())
            ]
        );
        assert_eq!(
            c.env.0.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        assert!(call("git").is_none());
        assert!(call("git -C").is_none());
        assert!(call("echo git push").is_none());
        assert!(call("/usr/bin/git status").is_some());
        assert!(
            call("git --config-env core.hooksPath=HP merge")
                .unwrap()
                .hooks_off
        );
        assert!(
            call("GIT_CONFIG_KEY_0=core.hooksPath git commit")
                .unwrap()
                .hooks_off
        );
        assert!(call("git -c \"x=$V\" commit").unwrap().config_expands);
        assert!(call("git -C \"$R\" push").unwrap().dir_expands);
        assert!(!call("git -c 'x=$V' commit").unwrap().config_expands);
        // `-c` at the end names no setting
        let last = call("git commit -c").unwrap();
        assert_eq!(last.sub, "commit");
    }

    #[test]
    fn the_variables_git_reads_are_the_named_ones_and_git_config() {
        let words = split_words("A=1 GIT_CONFIG_COUNT=1 GIT_DIR=/x DEST=$HOME git x").remove(0);
        let set = assigned(&words[..4]);
        let (env, unknown) = git_env(&[ConfigArg::Env("remote.o.push=DEST".into())], &set);
        let names: Vec<_> = env.0.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["GIT_CONFIG_COUNT", "DEST"]);
        assert!(unknown); // DEST is expanded
        let (env, unknown) = git_env(&[], &set);
        assert_eq!(env.0.len(), 1);
        assert!(!unknown);
    }

    #[test]
    fn a_destination_is_what_follows_the_last_colon_without_refs_heads() {
        assert_eq!(destination("+HEAD:refs/heads/release/v9"), "release/v9");
        assert_eq!(destination("release/v9"), "release/v9");
        assert_eq!(destination("a:b:c"), "c");
        assert_eq!(destination(":feat/old"), "feat/old");
        assert_eq!(destination("+:"), "");
        assert_eq!(destination("refs/heads/x"), "x");
        assert_eq!(off_remote("origin/release/v9"), "release/v9");
        assert_eq!(off_remote("/x"), "/x");
        assert_eq!(off_remote("nothing"), "nothing");
        assert_eq!(last_word("  a b\tc  "), "c");
        assert_eq!(last_word(""), "");
    }
}
