//! The guard's tests: the 23 of the removed TypeScript `companion.test.ts`, on a pretend machine.
//! The two coaching tests that press a band button draw the real band and press it by label; the
//! third drives `add_to_claude_md`, the function its button calls.

use super::{
    shell::commands,
    verdict::{protected_text, GitVerdict},
    *,
};
use crate::companion::{
    app::band::{
        self,
        tests::kit::{self as bandkit, Drawn},
        Seams,
    },
    relay::{
        dispatch::testing::{fixture, run, until, Fixture},
        host::fake::World,
    },
};
use std::{collections::HashMap, sync::Mutex};

const HOME: &str = "/home/u";
const REPO_GUARD: &str = "/repo/.claude/csr-guard.json";
const USER_GUARD: &str = "/home/u/.claude-self-reflect/csr-companion/guard.json";
const ERRORS: &str = "/home/u/.claude-self-reflect/csr-companion/pane-errors.log";
const CLAUDE_MD: &str = "/home/u/.claude/CLAUDE.md";

/// The guard this repository's tests run under.
fn guard_json() -> Value {
    json!({ "denyNoVerify": true, "protectedPush": ["release/v*"],
        "reason": { "release/v*": RELEASE } })
}

fn the_guard() -> Guard {
    parse_guard(&guard_json().to_string()).unwrap()
}

fn verdict_in(command: &str, cwd: Option<&str>) -> GitVerdict {
    git_verdict(command, Some(&the_guard()), cwd)
}

fn verdict_of(command: &str) -> GitVerdict {
    verdict_in(command, None)
}

fn parse_of(command: &str) -> GitVerdict {
    git_parse(command, &the_guard(), None)
}

/// What a verdict comes to as one word: the reason of a denial, else its kind; none for nothing.
fn said(verdict: GitVerdict) -> Option<String> {
    match verdict {
        Some(Verdict::Deny(reason)) => Some(reason),
        Some(Verdict::Ask) => Some("ask".into()),
        Some(Verdict::Check(_)) => Some("check".into()),
        None => None,
    }
}

fn reason(command: &str) -> Option<String> {
    said(parse_of(command))
}

fn all(commands: &[&str], want: Option<&str>) {
    for command in commands {
        assert_eq!(reason(command).as_deref(), want, "{command}");
    }
}

fn push_to(dir: Option<&str>, remote: Option<&str>, implicit: bool) -> GitPush {
    GitPush {
        dir: dir.map(String::from),
        remote: remote.map(String::from),
        config: vec![],
        env: None,
        implicit,
    }
}

// ---------------------------------------------------------------- a pretend machine

/// What is on the disk and what git answers (the removed `companion.test.ts`'s `host`: `files`,
/// `unreadable`,
/// `git`, `gitIn`, `gitExit`, `noRepo`, `hostEnv`, `coach`).
#[derive(Default)]
struct Disk {
    files: HashMap<String, String>,
    unreadable: HashSet<String>,
    writes: Vec<String>,
    git: HashMap<String, String>,
    git_in: HashMap<String, HashMap<String, String>>,
    git_exit: HashMap<String, i32>,
    no_repo: bool,
    host_env: HashMap<String, String>,
    coach: Vec<Value>,
    coach_calls: usize,
    /// The key of every git run, as the TypeScript's fake keys it.
    asked: Vec<String>,
}

struct Pretend(Mutex<Disk>);

impl Pretend {
    /// A repository at /repo with the guard the tests run under.
    fn repo() -> Arc<Self> {
        let mut disk = Disk::default();
        disk.files.insert("/repo/.git".into(), "gitdir: x".into());
        disk.files
            .insert(REPO_GUARD.into(), guard_json().to_string());
        Arc::new(Self(Mutex::new(disk)))
    }

    fn with<R>(&self, change: impl FnOnce(&mut Disk) -> R) -> R {
        change(&mut self.0.lock().unwrap())
    }

    fn put(&self, path: &str, text: &str) {
        self.with(|d| d.files.insert(path.into(), text.into()));
    }

    fn git_says(&self, key: &str, out: &str) {
        self.with(|d| d.git.insert(key.into(), out.into()));
    }
}

impl Machine for Pretend {
    fn home(&self) -> Option<String> {
        Some(HOME.into())
    }

    fn exists(&self, path: &str) -> bool {
        self.with(|d| d.files.contains_key(path))
    }

    fn read(&self, path: &str) -> Option<String> {
        self.with(|d| {
            if d.unreadable.contains(path) {
                return None;
            }
            d.files.get(path).cloned()
        })
    }

    fn write(&self, path: &str, text: &str) -> bool {
        self.with(|d| {
            d.files.insert(path.into(), text.into());
            d.writes.push(path.into());
        });
        true
    }

    fn keep(&self, path: &str, text: &str) -> bool {
        self.write(path, text)
    }

    fn variable(&self, name: &str) -> Option<String> {
        self.with(|d| d.host_env.get(name).cloned())
    }

    fn git(
        &self,
        dir: &str,
        settings: &[String],
        env: &[(String, String)],
        args: &[&str],
    ) -> Option<Ran> {
        let prefix: String = env.iter().map(|(n, v)| format!("{n}={v} ")).collect();
        let key = format!(
            "{prefix}{}",
            [settings, &to_strings(args)].concat().join(" ")
        );
        self.with(|d| {
            d.asked.push(key.clone());
            let ran = |exit, out: &str| {
                Some(Ran {
                    exit: Some(exit),
                    out: out.into(),
                })
            };
            if let Some(&exit) = d.git_exit.get(&key) {
                return ran(exit, "");
            }
            if args.ends_with(&["rev-parse", "--git-dir"]) {
                return if d.no_repo {
                    ran(128, "")
                } else {
                    ran(0, ".git\n")
                };
            }
            let said = d.git_in.get(dir).unwrap_or(&d.git).get(&key);
            match said {
                Some(out) => ran(0, out),
                None => ran(1, ""),
            }
        })
    }

    fn coach(&self, _now: f64) -> Vec<Value> {
        self.with(|d| {
            d.coach_calls += 1;
            d.coach.clone()
        })
    }
}

fn to_strings(args: &[&str]) -> Vec<String> {
    args.iter().map(ToString::to_string).collect()
}

struct Rig {
    fx: Fixture,
    table: Table,
}

fn rig(disk: Arc<Pretend>) -> Rig {
    let mut table = Table::new();
    register_with(&mut table, disk.clone());
    // The band draws the coaching note, and its 1 writes through the same pretend machine.
    let seams = Seams {
        machine: disk.clone(),
        ..bandkit::gear().seams
    };
    band::register_with(&mut table, seams);
    Rig {
        fx: fixture(World::default()),
        table,
    }
}

impl Rig {
    async fn band(&self) -> Drawn {
        bandkit::band(&self.table, &self.fx.cx).await
    }

    /// What the guard answers a Bash call: the reason it denies with, or none.
    async fn bash(&self, command: &str) -> Option<String> {
        let e = json!({ "tool": "Bash", "command": command });
        let reply = run(&self.table, "guard", &self.fx.cx, e).await;
        reply
            .answer
            .map(|answer| answer["deny"].as_str().unwrap().to_string())
    }

    async fn denies(&self, commands: &[&str], want: &str) {
        for command in commands {
            assert_eq!(self.bash(command).await.as_deref(), Some(want), "{command}");
        }
    }

    async fn lets_through(&self, commands: &[&str]) {
        for command in commands {
            assert_eq!(self.bash(command).await, None, "{command}");
        }
    }

    fn view(&self) -> Option<Value> {
        self.fx.cx.store.get("s", "view").unwrap()
    }
}

fn term_note() -> Value {
    json!({
        "id": "term:quokka", "kind": "term", "file": "",
        "line": "You said “quokka” in 2 sessions and Claude looked it up each time. Define it once?",
        "title": "Define “quokka” once",
        "text": "- quokka: said in 2 sessions, each time Claude looked it up (e.g. “call quokka \
            now”). Edit this line to say what you mean.",
        "sketch": [], "symbol": null, "oid": null, "conversation": "", "at": 1,
    })
}

fn machine(disk: &Arc<Pretend>) -> Arc<dyn Machine> {
    disk.clone()
}

// ---------------------------------------------------------------- coaching

#[tokio::test]
async fn a_coaching_note_names_a_word_you_said_in_2_sessions_writes_nothing_and_adds_the() {
    let disk = Pretend::repo();
    disk.with(|d| d.coach = vec![term_note()]);
    disk.put(CLAUDE_MD, "# Prefs\n\n## Core\n- be brief\n");
    let r = rig(disk.clone());
    run(&r.table, "start", &r.fx.cx, json!({ "cwd": "/repo" })).await;
    assert!(until(|| r.view().is_some()).await, "the note is shown");
    let view = r.view().unwrap();
    assert_eq!(view["tag"], "Coaching");
    assert_eq!(view["note"]["id"], "term:quokka");
    assert!(
        !disk.with(|d| d.writes.contains(&CLAUDE_MD.to_string())),
        "shown is not written"
    );
    assert!(r.fx.host.calls().iter().any(|(m, _)| m == "ui.invalidate"));

    let shown = r.band().await;
    assert!(shown.has("You said “quokka”"));
    assert_eq!(shown.labels(), ["Add to CLAUDE.md", "Dismiss"]);

    // the key 1: the band hands the note's text to add_to_claude_md
    assert!(bandkit::press(&r.table, &r.fx.cx, "Add to CLAUDE.md").await);
    let want = format!(
        "# Prefs\n\n## Core\n- be brief\n\n## Terms\n{}\n",
        term_note()["text"].as_str().unwrap()
    );
    assert_eq!(disk.read(CLAUDE_MD), Some(want));
    assert_eq!(strings(&r.fx.cx, MACHINE, "coached"), ["term:quokka"]);
    assert_eq!(r.view(), None, "the band closed it");

    // once: the same word is not offered again
    run(&r.table, "start", &r.fx.cx, json!({ "cwd": "/repo" })).await;
    assert!(until(|| disk.with(|d| d.coach_calls) == 2).await);
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(r.view(), None);
}

#[tokio::test]
async fn dismissing_a_coaching_note_leaves_claude_md_alone() {
    let disk = Pretend::repo();
    disk.with(|d| d.coach = vec![term_note()]);
    disk.put(CLAUDE_MD, "# Prefs\n");
    let r = rig(disk.clone());
    run(&r.table, "start", &r.fx.cx, json!({ "cwd": "/repo" })).await;
    assert!(until(|| r.view().is_some()).await);
    assert!(bandkit::press(&r.table, &r.fx.cx, "Dismiss").await);
    assert_eq!(r.view(), None);
    assert_eq!(disk.read(CLAUDE_MD).as_deref(), Some("# Prefs\n"));
    assert!(disk.with(|d| d.writes.is_empty()));
}

#[test]
fn addterm_puts_a_line_under_an_existing_terms_heading_makes_one_if_missing_and() {
    assert_eq!(add_term("# A\n", "- x: y"), "# A\n\n## Terms\n- x: y\n");
    assert_eq!(add_term("", "- x: y"), "## Terms\n- x: y\n");
    assert_eq!(
        add_term("# A\n## Terms\n- a: b\n\n## Next\ntext\n", "- x: y"),
        "# A\n## Terms\n- a: b\n- x: y\n\n## Next\ntext\n"
    );
    assert_eq!(
        add_term("# A\n## Terms\n- a: b\n", "- x: y"),
        "# A\n## Terms\n- a: b\n- x: y\n"
    );
    assert_eq!(
        add_term("## Terms\n- x: y\n", "- x: y"),
        "## Terms\n- x: y\n"
    );
    // the headings as the regexes read them
    assert!(terms_heading("##  Terms \t"));
    assert!(!terms_heading("##Terms"));
    assert!(!terms_heading("### Terms"));
    assert!(!terms_heading("## Terms and more"));
    assert!(any_heading("###### x"));
    assert!(!any_heading("####### x"));
    assert!(!any_heading("#x"));
}

#[tokio::test]
async fn a_claude_md_that_is_there_but_cannot_be_read_is_never_written_over() {
    let disk = Pretend::repo();
    disk.with(|d| d.coach = vec![term_note()]);
    disk.put(CLAUDE_MD, "# Prefs\n");
    disk.with(|d| d.unreadable.insert(CLAUDE_MD.into()));
    let r = rig(disk.clone());
    run(&r.table, "start", &r.fx.cx, json!({ "cwd": "/repo" })).await;
    assert!(until(|| r.view().is_some()).await);
    let line = term_note()["text"].as_str().unwrap().to_string();
    assert!(
        !add_to_claude_md(&machine(&disk), &line).await,
        "the band says it could not write"
    );
    assert_eq!(disk.with(|d| d.files[CLAUDE_MD].clone()), "# Prefs\n");
    assert!(!disk.with(|d| d.writes.contains(&CLAUDE_MD.to_string())));
}

// ---------------------------------------------------------------- the push guard

#[tokio::test]
async fn the_push_guard_no_verify_and_release_v_are_denied_every_other_push_goes_through() {
    let r = rig(Pretend::repo());
    assert_eq!(
        r.bash("git commit --no-verify -m wip").await.as_deref(),
        Some(NO_VERIFY)
    );
    assert_eq!(
        r.bash("git push origin HEAD:release/v9.5.9")
            .await
            .as_deref(),
        Some(RELEASE)
    );
    r.lets_through(&["git status", r#"echo "git push origin main""#])
        .await;
    // no word needed: Claude's work is never held
    r.lets_through(&["git push origin main", "git push -u origin feat/x"])
        .await;
    // release/v* is never let through
    r.denies(&["git push origin HEAD:release/v9"], RELEASE)
        .await;
}

#[tokio::test]
async fn a_push_that_names_no_branch_is_denied_when_git_says_it_lands_on_release_v() {
    let disk = Pretend::repo();
    disk.git_says("symbolic-ref -q --short HEAD", "release/v10.1.0\n");
    let r = rig(disk);
    r.denies(
        &["git push", "git push -u origin HEAD", "git push origin"],
        RELEASE,
    )
    .await;
    // the words name the branch
    r.lets_through(&["git push origin HEAD:feat/x"]).await;
}

#[tokio::test]
async fn upstream_and_configured_push_refspecs_count_a_git_that_cannot_answer_lets_the() {
    let disk = Pretend::repo();
    disk.git_says("symbolic-ref -q --short HEAD", "feat/x\n");
    let r = rig(disk.clone());
    r.lets_through(&["git push"]).await;
    let upstream = "rev-parse --abbrev-ref --symbolic-full-name @{upstream}";
    disk.git_says(upstream, "origin/release/v9\n");
    r.denies(&["git push"], RELEASE).await;
    disk.with(|d| d.git.remove(upstream));
    disk.git_says(
        "config --get-all remote.origin.push",
        "refs/heads/*:refs/heads/*\n",
    );
    r.denies(&["git push"], RELEASE).await;
    disk.with(|d| d.git.clear());
    r.lets_through(&["git push"]).await;
}

#[tokio::test]
async fn where_a_push_lands_is_asked_with_the_command_s_own_config_of_its_own_remote_for() {
    let spec = "remote.origin.push='refs/heads/*:refs/heads/*'";
    let disk = Pretend::repo();
    let wild = "-c remote.origin.push=refs/heads/*:refs/heads/*";
    disk.git_says("symbolic-ref -q --short HEAD", "main\n");
    disk.git_says(&format!("{wild} symbolic-ref -q --short HEAD"), "main\n");
    disk.git_says(
        &format!("{wild} config --get-all remote.origin.push"),
        "refs/heads/*:refs/heads/*\n",
    );
    // another remote's: not this push's
    disk.git_says(
        "config --get-all remote.mirror.push",
        "refs/heads/*:refs/heads/release/v9\n",
    );
    let r = rig(disk.clone());
    r.denies(&[&format!("git -c {spec} push origin")], RELEASE)
        .await;
    r.lets_through(&["git push origin"]).await;
    r.denies(&["git push mirror"], RELEASE).await;
    // no remote named: the default push remote's
    disk.git_says("config --get remote.pushDefault", "mirror\n");
    r.denies(&["git push"], RELEASE).await;
    disk.with(|d| d.git.remove("config --get remote.pushDefault"));
    // every branch both sides have
    disk.git_says("config --get push.default", "matching\n");
    r.denies(&["git push origin"], RELEASE).await;
    disk.with(|d| d.git.remove("config --get push.default"));
    assert_eq!(
        verdict_of("git -C /a push origin HEAD; git -C /b push origin HEAD"),
        Some(Verdict::Check(vec![
            push_to(Some("/a"), Some("origin"), false),
            push_to(Some("/b"), Some("origin"), false),
        ]))
    );
}

#[tokio::test]
async fn push_default_matching_counts_only_for_a_push_that_names_no_refspec() {
    let disk = Pretend::repo();
    disk.git_says(
        "-c push.default=matching symbolic-ref -q --short HEAD",
        "main\n",
    );
    disk.git_says(
        "-c push.default=matching config --get push.default",
        "matching\n",
    );
    let r = rig(disk);
    r.lets_through(&[
        "git -c push.default=matching push origin HEAD",
        "git -c push.default=matching push origin @",
        "git -c push.default=matching push -u origin HEAD",
    ])
    .await;
    r.denies(
        &[
            "git -c push.default=matching push origin",
            "git -c push.default=matching push",
        ],
        RELEASE,
    )
    .await;
    assert_eq!(
        verdict_of("git push origin HEAD"),
        Some(Verdict::Check(vec![push_to(None, Some("origin"), false)]))
    );
    assert_eq!(
        verdict_of("git push origin"),
        Some(Verdict::Check(vec![push_to(None, Some("origin"), true)]))
    );
}

#[tokio::test]
async fn config_env_counts_as_c_with_its_variable_s_value_and_git_config_in_the() {
    let spec = "refs/heads/study/dreamer-2026-10-12:refs/heads/release/v9";
    let wild = "refs/heads/*:refs/heads/*";
    let safe = "refs/heads/main:refs/heads/main";
    let disk = Pretend::repo();
    disk.with(|d| d.host_env.insert("FROM_HOST".into(), spec.into()));
    disk.git_says("symbolic-ref -q --short HEAD", "study/dreamer-2026-10-12\n");
    for (given, out) in [(spec, spec), (wild, wild), (safe, safe)] {
        let key = format!("-c remote.origin.push={given} config --get-all remote.origin.push");
        disk.git_says(&key, &format!("{out}\n"));
    }
    let env = "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.push GIT_CONFIG_VALUE_0";
    disk.git_says(
        &format!("{env}={wild} config --get-all remote.origin.push"),
        &format!("{wild}\n"),
    );
    let r = rig(disk);
    let config_env = "git --config-env=remote.origin.push=DEST push origin";
    let denied = [
        format!("DEST='{wild}' {config_env}"),
        format!("DEST='{wild}' git --config-env remote.origin.push=DEST push origin"),
        format!("env DEST='{wild}' git --config-env=remote.origin.push=DEST push"),
        format!("export DEST='{wild}'; {config_env}"),
        "git --config-env=remote.origin.push=FROM_HOST push origin".to_string(),
        format!("GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.push GIT_CONFIG_VALUE_0='{wild}' git push origin"),
    ];
    for command in &denied {
        assert_eq!(r.bash(command).await.as_deref(), Some(RELEASE), "{command}");
    }
    r.lets_through(&[&format!("DEST={safe} {config_env}")])
        .await;
    // unknown: denied
    r.denies(
        &[
            "git --config-env=remote.origin.push=NOT_SET push origin",
            "git --config-env=remote.origin.push=-x push origin",
        ],
        UNRESOLVED,
    )
    .await;
    r.lets_through(&["git push origin"]).await;
}

#[tokio::test]
async fn settings_keep_their_order_and_what_the_check_cannot_resolve_is_denied() {
    let disk = Pretend::repo();
    disk.git_says("symbolic-ref -q --short HEAD", "main\n");
    disk.git_says(
        "-c push.default=current -c push.default=matching config --get push.default",
        "matching\n",
    );
    disk.git_says(
        "-c push.default=matching -c push.default=current config --get push.default",
        "current\n",
    );
    let r = rig(disk.clone());
    r.denies(
        &["MODE=current git --config-env=push.default=MODE -c push.default=matching push origin"],
        RELEASE,
    )
    .await;
    r.lets_through(&[
        "MODE=current git -c push.default=matching --config-env=push.default=MODE push origin",
    ])
    .await;
    r.denies(
        &[
            "N=1\nGIT_CONFIG_COUNT=$N GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=Me \\\n  git -c push.default=matching push origin",
            r#"git -c "push.default=$MODE" push origin"#,
            "DEST=$(cat spec) git --config-env=remote.origin.push=DEST push origin",
            "GIT_CONFIG_VALUE_0=`cat v` git push origin",
            r#"git -C "$REPO" push origin"#,
            "cd ~/repo && git push",
            r#"cd "$R" && git push origin HEAD"#,
            r#"git -c "core.$KEY=x" commit -m x"#,
        ],
        UNRESOLVED,
    )
    .await;
    // single quotes: literal
    r.lets_through(&[
        "git -c 'push.default=$MODE' push origin",
        "cd sub && git push origin HEAD",
    ])
    .await;
    disk.with(|d| d.no_repo = true); // git refuses the settings, or no repository
    r.denies(&["git push origin"], UNRESOLVED).await;
    disk.with(|d| d.no_repo = false);
    // a question that fails other than by "not set"
    disk.with(|d| d.git_exit.insert("config --get push.default".into(), 128));
    r.denies(&["git push origin"], UNRESOLVED).await;
    // a named branch: nothing to ask git
    r.lets_through(&["git push origin feat/x"]).await;
}

#[tokio::test]
async fn the_guard_is_opt_in_no_file_nothing_a_repository_file_a_user_file_both_a_bad() {
    let disk = Pretend::repo();
    disk.with(|d| d.files.remove(REPO_GUARD));
    disk.git_says("symbolic-ref -q --short HEAD", "release/abc\n");
    let r = rig(disk.clone());
    let (push, skip) = ("git push origin release/v9", "git commit --no-verify -m x");
    let both = async |r: &Rig| (r.bash(push).await, r.bash(skip).await);
    // no file: no guard, and no git asked anything
    assert_eq!(both(&r).await, (None, None));
    assert_eq!(disk.with(|d| d.asked.len()), 0);
    disk.put(REPO_GUARD, &guard_json().to_string()); // the repository's
    assert_eq!(
        both(&r).await,
        (Some(RELEASE.into()), Some(NO_VERIFY.into()))
    );
    disk.with(|d| d.files.remove(REPO_GUARD));
    disk.put(USER_GUARD, r#"{"denyNoVerify": true}"#); // the person's alone
    assert_eq!(both(&r).await, (None, Some(NO_VERIFY.into())));
    r.lets_through(&["git push"]).await; // nothing protected: no question for git
    disk.put(REPO_GUARD, r#"{"protectedPush": ["release/*"]}"#); // both, and a glob of its own
    r.denies(&["git push origin release/x"], &protected_text("release/*"))
        .await;
    r.lets_through(&["git push origin main"]).await;
    // on release/abc: git says where it lands
    r.denies(&["git push"], &protected_text("release/*")).await;
    r.denies(&[skip], NO_VERIFY).await;
    // not JSON: counts for nothing, said once
    disk.put(REPO_GUARD, r#"{"protectedPush": ["release/v*""#);
    disk.with(|d| d.files.remove(USER_GUARD));
    assert_eq!(both(&r).await, (None, None));
    let log = disk.read(ERRORS).unwrap_or_default();
    assert_eq!(
        log.split('\n')
            .filter(|line| line.contains(REPO_GUARD))
            .count(),
        1
    );
    assert!(log.contains("is no guard"));
    assert!(parse_guard(r#"{"denyNoVerify": "yes"}"#).is_none()); // a key of the wrong kind
    let nothing = Guard {
        deny_no_verify: false,
        protected_push: vec![],
        reason: HashMap::new(),
    };
    assert!(merge_guards(&[nothing]).is_none());
}

#[test]
fn the_tripwire_reads_only_where_git_is_the_command() {
    for command in [
        r#"codex exec "review the git range; check --no-verify and release/v9 handling""#,
        "grep -n 'release/v' file && git status",
        "echo git push origin release/v9",
    ] {
        assert_eq!(verdict_of(command), None, "{command}");
    }
    for command in [
        "((1<<2))\ngit push origin release/v9",
        "cat <<1 \\\n; git push origin release/v9\nx\n1",
        "bash <<\\EOF\ngit push origin release/v9\nEOF",
        "sudo -u me git push origin release/v9",
        "FOO=1 env BAR=2 timeout 5 git commit --no-verify -m x",
        "x=$(git push origin release/v9)",
        "ls | xargs -I{} git push origin release/v9",
        "/usr/bin/git push --mirror backup",
    ] {
        assert!(
            matches!(verdict_of(command), Some(Verdict::Deny(_))),
            "{command}"
        );
    }
}

#[test]
fn the_tripwire_reads_the_raw_text_first_the_parse_only_makes_a_denial_more_precise() {
    let deny = |reason: &str| Some(Verdict::Deny(reason.to_string()));
    for command in [
        "((1<<2))\ngit push origin release/v9",
        "cat <<1 \\\n; git push origin release/v9\nx\n1",
        "cat <<'EOF'; bash -c ':'\ngit push origin release/v9\nEOF",
        "for ((i=0; i<<1; i++)); do :; done\ngit push origin release/v9",
    ] {
        assert!(
            matches!(verdict_of(command), Some(Verdict::Deny(_))),
            "{command}"
        );
    }
    // only where git stands as the command: a message line that merely names the flag goes
    // through; a heredoc line that starts with git still trips it (accepted)
    assert_eq!(
        verdict_of("git commit -F - <<'EOF'\nfix: never pass --no-verify\nEOF"),
        None
    );
    let as_text = format!("{RELEASE} {AS_TEXT}");
    assert_eq!(
        verdict_of("cat <<'EOF' > notes.md\ngit push origin release/v9\nEOF"),
        deny(&as_text)
    );
    assert_eq!(
        verdict_of(r#"git commit -m "mention release/v9""#),
        deny(&as_text)
    );
    // a real one: the parse's reason
    assert_eq!(verdict_of("git commit --no-verify -m x"), deny(NO_VERIFY));
    assert_eq!(
        verdict_of("git -c core.hooksPath=/dev/null commit -m x"),
        deny(HOOKS_OFF)
    );
    for command in [
        "git push origin :feat/old",
        "git push --mirror backup",
        "GIT_CONFIG_KEY_0=core.hooksPath git commit -m x",
    ] {
        assert!(
            matches!(verdict_of(command), Some(Verdict::Deny(_))),
            "{command}"
        );
    }
    for command in [
        "echo release/v9",
        "ls --no-verify",
        "git status",
        "gitk release/v9",
        "cat .git/release/v9",
    ] {
        assert_eq!(tripwire(command, &the_guard()), None, "{command}");
    }
    // the parse alone: arithmetic and continued lines are no heredocs, and a heredoc is a script
    // only in a shell's pipeline
    for command in [
        "((1<<2))\ngit commit -n -m x",
        "x=$[1<<2]\ngit commit -n -m x",
        "cat <<1 \\\n; git commit -n -m x\nx\n1",
        "cat <<'EOF' | bash -s\ngit commit -n -m x\nEOF",
        "git commit \\\n  -n -m x",
    ] {
        assert_eq!(parse_of(command), deny(NO_VERIFY), "{command}");
    }
    assert_eq!(
        parse_of("cat <<'EOF'; bash -c ':'\ngit commit -n -m x\nEOF"),
        None
    );
}

#[tokio::test]
async fn of_two_pushes_the_words_leave_to_the_repository_either_one_on_release_v_is() {
    let disk = Pretend::repo();
    let head = "symbolic-ref -q --short HEAD";
    disk.with(|d| {
        d.git_in
            .insert("/a".into(), HashMap::from([(head.into(), "main\n".into())]));
        d.git_in.insert(
            "/b".into(),
            HashMap::from([(head.into(), "release/v9\n".into())]),
        );
    });
    let r = rig(disk);
    r.denies(
        &["git -C /a push origin HEAD; git -C /b push origin HEAD"],
        RELEASE,
    )
    .await;
    r.lets_through(&["git -C /a push origin HEAD; git -C /a push origin HEAD"])
        .await;
}

#[test]
fn gitverdict_reads_git_as_a_command_word_not_git_in_words() {
    all(
        &[
            "git commit --no-verify -m x",
            "git push --no-verify",
            "git -C /r merge --no-verify topic",
            "cd r && git commit -n -m x",
        ],
        Some(NO_VERIFY),
    );
    all(
        &[
            "git push origin release/v9.5.9",
            "git push origin HEAD:release/v9",
            "git push origin refs/heads/release/v9",
            "git push origin +HEAD:refs/heads/release/v9",
            "git push --all",
            "git push --mirror origin",
            "git push -u origin release/v10",
            "git -C /repo push origin HEAD:release/v1",
            "FOO=1 git push origin release/v2",
            r#"bash -c "git push origin release/v3""#,
            "git push origin 'refs/heads/*:refs/heads/*'",
            "git push --dry-run --no-dry-run origin HEAD:release/v1",
            "sh -ce 'git push origin release/v1'",
            "env -i git push origin release/v1",
            "sudo -u me git push origin release/v1",
            r#"echo "$(git push origin release/v1)""#,
            "bash <<'EOF'\ngit push origin release/v1\nEOF",
            "bash -o pipefail -c 'git push origin release/v1'",
            "x=`git push origin release/v1`",
        ],
        Some(RELEASE),
    );
    all(
        &[
            "git commit --verify --no-verify -m x",
            "git push -o ci.skip --no-verify origin main",
        ],
        Some(NO_VERIFY),
    );
    // where it lands is the repository's to say: the hook asks git (see the next test)
    all(
        &[
            "git push",
            "git push origin",
            "git push origin HEAD",
            "git push -u origin HEAD",
            "cd /r && git push",
        ],
        Some("check"),
    );
    assert_eq!(
        verdict_in("cd /r && git push", Some("/elsewhere")),
        Some(Verdict::Check(vec![push_to(Some("/r"), None, true)]))
    );
    assert_eq!(
        verdict_in("git -C sub push origin HEAD", Some("/r")),
        Some(Verdict::Check(vec![push_to(
            Some("/r/sub"),
            Some("origin"),
            false
        )]))
    );
    all(
        &[
            "git push origin main",
            "git push -u origin feature/x",
            "git push -o --dry-run origin main",
            "echo hi && git push origin backport/x",
            "sudo git push origin main",
            "(git push origin main)",
            "sh -c 'git push origin main'",
            "git push origin main 2>&1 | tail -3",
        ],
        Some("ask"),
    );
    all(
        &[
            "ls",
            "git status",
            "git log --oneline",
            r#"git commit -m "git push --no-verify""#,
            r#"git commit -m "mention release/v9 push""#,
            "echo git push origin main",
            r#"echo "git push origin main""#,
            r#"grep "git push" notes.md"#,
            "git push --dry-run origin main",
            "cat <<'EOF'\ngit push origin release/v9\ngit commit --no-verify\nEOF",
            "echo hi # git push origin main",
            "git pull origin release/v9",
            "git checkout release/v9",
            r#"git commit -m "x" --amend"#,
            "git stash push",
            "echo sh -c 'git push origin release/v1'",
            "git commit -m --no-verify",
            "git commit --no-verify --verify -m x",
            "cat <<'EOF'\nbash -c 'git push origin release/v1'\nEOF",
        ],
        None,
    );
    let want: Vec<Vec<String>> = [vec!["a", "b c"], vec!["d"], vec!["e"], vec!["f"]]
        .into_iter()
        .map(|words| words.into_iter().map(String::from).collect())
        .collect();
    assert_eq!(commands(r#"a "b c" && d; e | f"#), want);
}

#[test]
fn commit_n_is_read_as_git_reads_it_in_a_cluster_never_inside_an_option_value_or() {
    all(
        &[
            "git commit -an -m x",
            "git commit -nm x",
            "git commit -m x -n",
        ],
        Some(NO_VERIFY),
    );
    all(
        &[
            "git commit -uno -m x",
            "git commit -m n",
            "git commit -mn",
            "git commit -m x -- -n",
            "git commit -F notes -S",
        ],
        None,
    );
}

#[test]
fn an_option_whose_value_is_optional_never_eats_the_next_word_and_an_abbreviated_no() {
    all(
        &[
            "git commit --gpg-sign --no-verify -m x",
            "git commit -S --no-verify -m x",
            "git commit --gpg-sign=ABC --no-verify -m x",
            "git commit -Sabc -n -m x",
            "git commit --untracked-files --no-verify -m x",
            "git commit -m x --no-veri",
            "git commit --no-verif -m x",
            "git push --no-veri origin main",
            "git push --signed --no-verify origin main",
        ],
        Some(NO_VERIFY),
    );
    // --no-ver is ambiguous (--no-verbose): git refuses it; --veri turns verify back on
    all(
        &[
            "git commit --no-ver -m x",
            "git commit -m x --gpg-sign",
            r#"git commit -S -m "--no-verify""#,
            "git commit --no-verify --veri -m x",
        ],
        None,
    );
    // -o's value sits after it in a push cluster: `-on` is the push option "n", not a dry run
    let deny = Some(Verdict::Deny(RELEASE.into()));
    assert_eq!(verdict_of("git push -on origin release/v1"), deny);
    assert_eq!(verdict_of("git push -uo nightly origin release/v1"), deny);
    // -n first: a dry run
    assert_eq!(said(parse_of("git push -nu origin release/v1")), None);
}

#[test]
fn the_guard_reads_git_under_timeout_nice_stdbuf_and_eval_and_a_hookspath_set_for() {
    all(
        &[
            "timeout 120 git push origin release/v1",
            "timeout -s KILL 60 git push origin release/v1",
            "timeout --signal=KILL -k 5 60 git push origin release/v1",
            "nice -n 10 git push origin release/v1",
            "nice -5 git push origin release/v1",
            "stdbuf -o L git push origin release/v1",
            r#"eval "git push origin release/v1""#,
            "eval git push origin release/v1",
            "sudo -S git push origin release/v1",
            r#"zsh -c "git push origin release/v1""#,
            "env FOO=1 git push origin release/v1",
            "command git push origin release/v1",
            "true && git push origin release/v1",
            "false || git push origin release/v1",
            "echo y | git push origin release/v1",
        ],
        Some(RELEASE),
    );
    all(
        &[
            "nice git commit --no-verify -m x",
            "sudo -n git commit --no-verify -m x",
            "timeout 60 git commit -n -m x",
            r#"eval "git commit --no-verify -m x""#,
            "git -C /r commit --no-verify -m x; echo done",
        ],
        Some(NO_VERIFY),
    );
    all(
        &[
            "git -c core.hooksPath=/dev/null commit -m x",
            "git -c core.hookspath= commit -m x",
            "git -c user.name=me -c core.hooksPath=/tmp/none push origin main",
            "git --config-env=core.hooksPath=HP merge topic",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null git commit -m x",
            r#"bash -c "git -c core.hooksPath=/dev/null commit -m x""#,
        ],
        Some(HOOKS_OFF),
    );
    all(
        &[
            r#"git commit -m "--no-verify and core.hooksPath=/dev/null""#,
            "git -c user.name=me commit -m x",
            "echo timeout 60 git push origin release/v1",
            "nice -n 10 git status",
            r#"eval "echo git push origin release/v1""#,
            "git -c core.hooksPath=/dev/null status",
            "git config core.hooksPath .githooks",
        ],
        None,
    );
    all(&["timeout 60 git push origin main"], Some("ask"));
}

#[test]
fn the_guard_reads_options_as_git_and_the_wrappers_do_long_values_any_unique_prefix() {
    all(
        &[
            "timeout --signal TERM 60 git commit --no-verify -m x",
            "timeout --kill-after 5 60 git commit -n -m x",
            "timeout --sig TERM 60 git commit -n -m x",
            "nice --adjustment 5 git commit -n -m x",
            "env --unset HOME git commit -n -m x",
            "stdbuf --output L git commit -n -m x",
            "sudo --user me git commit -n -m x",
        ],
        Some(NO_VERIFY),
    );
    all(
        &[
            "git push --dry-run --no-dry-ru origin HEAD:refs/heads/release/v9",
            "git push --mirr origin",
            "git push --al origin",
            "git push --branc origin",
            "git push -n --no-dry-run origin release/v9",
        ],
        Some(RELEASE),
    );
    all(
        &[
            "git push --dry origin release/v9",
            "git push --dry-r origin release/v9",
            "git push --all --no-all --dry-run origin",
        ],
        None,
    );
    // the last is ambiguous: git refuses it
    all(
        &[
            "git merge --no-verify --verify topic",
            "git merge -m --no-verify topic",
            "git merge --message --no-verify topic",
            "git merge -sours --no-verify-signatures topic",
            "git merge --no-veri topic",
        ],
        None,
    );
    all(
        &[
            "git merge --no-verify topic",
            "git merge --verify --no-verify topic",
            "git merge -m x --no-verify topic",
            "git merge -Xours --no-verify topic",
        ],
        Some(NO_VERIFY),
    );
}

#[test]
fn a_heredoc_hides_only_its_body_and_or_backticks_run_only_where_the_shell_runs() {
    all(
        &[
            "cat <<'EOF'\nhello\nEOF\ngit commit --no-verify -m x",
            "cat <<A\nx\nA\ncat <<B\ny\nB\ngit commit -n -m x",
            "cat <<<\"here string\"\ngit commit --no-verify -m x",
            "cat <<-'EOF'\n\tbody\n\tEOF\ngit commit -n -m x",
            "cat <<\\EOF\nbody\nEOF\ngit commit -n -m x",
        ],
        Some(NO_VERIFY),
    );
    all(
        &[
            "cat <<EOF\n$(git push origin release/v9)\nEOF",
            "cat <<EOF\nit's `git push origin release/v9`\nEOF",
            r#"echo "$(git push origin release/v9)""#,
            "x=$(echo $(git push origin release/v9))",
            r#"echo "a `git push origin release/v9` b""#,
        ],
        Some(RELEASE),
    );
    all(
        &[
            "printf '%s' '$(git push origin release/v9)'",
            "echo '`git push origin release/v9`'",
            "cat <<'EOF'\n$(git push origin release/v9)\nEOF",
            "cat <<\"EOF\"\n`git commit --no-verify -m x`\nEOF",
            r#"echo "it's" '$(git push origin release/v9)'"#,
            "echo $((1 + 2))",
        ],
        None,
    );
}

#[test]
fn every_heredoc_on_a_line_is_cut_in_order_and_one_a_shell_reads_is_its_script() {
    all(
        &[
            "bash <<\\EOF\ngit push origin release/v9\nEOF",
            "sh <<'EOF'\ngit push origin release/v9\nEOF",
            "bash <<\"EOF\"\ngit push origin release/v9\nEOF",
            "cat <<'EOF' | bash\ngit push origin release/v9\nEOF",
            "cat <<'A'; bash <<'B'\nx\nA\ngit push origin release/v9\nB",
            "cat <<'A' <<'B'\nhello\nA\nworld\nB\ngit push origin release/v9",
            "echo \"a <<b\"\ngit push origin release/v9\nb",
            "echo $((1<<x))\ngit push origin release/v9\nx",
        ],
        Some(RELEASE),
    );
    all(
        &["bash <<\\EOF\ngit commit --no-verify -m x\nEOF"],
        Some(NO_VERIFY),
    );
    all(
        &[
            "cat <<'A' <<'B'\nhello\nA\ngit push origin release/v9\nB",
            "cat <<\\A <<-\"B\"\nx\nA\n\tgit commit -n -m x\n\tB",
            "echo bash <<'EOF'\ngit push origin release/v9\nEOF",
        ],
        None,
    );
}

#[test]
fn the_matching_refspec_pushes_every_branch_both_sides_have_so_it_is_a_release_push() {
    all(
        &[
            "git push origin :",
            "git push origin +:",
            "git push -u origin feat/x :",
        ],
        Some(RELEASE),
    );
    // a deletion names its branch
    all(&["git push origin :feat/old"], Some("ask"));
    all(&["git push --dry-run origin :"], None);
}

// ---------------------------------------------------------------- the rest of the port

#[test]
fn a_glob_runs_across_slashes_and_not_across_line_ends() {
    let guard = parse_guard(r#"{"protectedPush": ["release/*", "x?y"]}"#).unwrap();
    let by = |branch| verdict::protected_by(&guard, branch);
    assert_eq!(by("release/v9/rc"), Some("release/*"));
    assert_eq!(by("release/"), Some("release/*"));
    assert_eq!(by("release"), None);
    assert_eq!(by("release/a\nb"), None);
    assert_eq!(by("xzy"), Some("x?y"));
    assert_eq!(by("xy"), None);
}

#[test]
fn git_is_a_word_when_no_word_character_or_dash_touches_it() {
    for text in ["git", "a git b", ";git push", "x|git", "$(git"] {
        assert!(verdict::git_word(text), "{text}");
    }
    for text in ["gitk", "digit", "a.git", "my-git", "git-lfs", "git_x", ""] {
        assert!(!verdict::git_word(text), "{text}");
    }
}

#[tokio::test]
async fn the_relay_is_armed_at_start_and_told_again_only_on_a_change_or_a_respawn() {
    let r = rig(Pretend::repo());
    let arms = || -> Vec<Value> {
        r.fx.host
            .calls()
            .into_iter()
            .filter(|(m, _)| m == "guard.arm")
            .map(|(_, a)| a[0].clone())
            .collect()
    };
    run(&r.table, "start", &r.fx.cx, json!({ "cwd": "/repo" })).await;
    assert_eq!(arms().len(), 1);
    assert_eq!(arms()[0]["reason"], ENGINE_DOWN);
    assert_eq!(arms()[0]["pattern"], armed_pattern(&the_guard()));
    // the same guard on the next Bash call: the relay is not told again
    r.bash("ls").await;
    assert_eq!(arms().len(), 1);
    // a respawned engine tells it again
    let again = Ctx {
        respawn: true,
        ..r.fx.cx.clone()
    };
    run(&r.table, "start", &again, json!({ "cwd": "/repo" })).await;
    assert_eq!(arms().len(), 2);
}

#[tokio::test]
async fn the_relay_is_disarmed_when_the_guard_files_go() {
    let disk = Pretend::repo();
    let r = rig(disk.clone());
    let arms = || -> Vec<Value> {
        r.fx.host
            .calls()
            .into_iter()
            .filter(|(m, _)| m == "guard.arm")
            .map(|(_, a)| a[0].clone())
            .collect()
    };
    r.bash("ls").await;
    assert_eq!(arms().len(), 1);
    assert!(arms()[0].is_object());
    disk.with(|d| d.files.remove(REPO_GUARD));
    r.bash("ls").await;
    assert_eq!(arms().len(), 2);
    assert!(arms()[1].is_null(), "a null state disarms the relay");
    r.bash("ls").await;
    assert_eq!(arms().len(), 2, "told only on a change");
}

fn push_only() -> Guard {
    Guard {
        deny_no_verify: false,
        protected_push: vec!["release/v*".into()],
        reason: HashMap::new(),
    }
}

/// The relay fixture's cases run `PATTERN` in JavaScript; it must be the source the engine
/// builds, or they prove nothing about it.
#[test]
fn the_relay_fixture_carries_the_pattern_the_engine_builds() {
    let fixture = include_str!("../../../../tests/fixtures/relay/relay.test.ts");
    let source = format!("String.raw`{}`", armed_pattern(&push_only()));
    assert!(fixture.contains(&source), "{source}");
}

/// The armed pattern as the relay runs it, in `regex` here; the relay fixture runs the same
/// source in JavaScript.
#[test]
fn the_armed_pattern_matches_any_command_that_could_push_and_no_other() {
    let re = regex::Regex::new(&armed_pattern(&push_only())).unwrap();
    for command in [
        "git push",
        "git push origin main",
        "git -C /r push origin HEAD",
        r#"bash -c "git push""#,
        "cd x && git push -f",
        "cat <<EOF | bash\ngit push origin release/v9\nEOF",
        "x=$(git push)",
        "echo push; git status",
        "/usr/bin/git push",
        "git\tpush",
        "git\npush",
        "push git",
    ] {
        assert!(re.is_match(command), "{command}");
    }
    for command in [
        "git status",
        "git log --oneline",
        "git commit -m x",
        "ls",
        "",
        "gitk --all",
        "git pushd",
        "git unpush",
        "git push-all",
        "digit push",
        "./a.git push",
        "echo pushing",
    ] {
        assert!(!re.is_match(command), "{command}");
    }
    // with denyNoVerify, a commit or merge may skip the gate too
    let re = regex::Regex::new(&armed_pattern(&the_guard())).unwrap();
    for command in [
        "git commit -m x",
        "git merge topic",
        "git push",
        "cd r && git commit -n",
    ] {
        assert!(re.is_match(command), "{command}");
    }
    assert!(!re.is_match("git status"));
}

#[test]
fn the_armed_pattern_has_no_lookaround_and_no_unicode_class() {
    for guard in [
        the_guard(),
        parse_guard(r#"{"protectedPush": ["a"]}"#).unwrap(),
    ] {
        let source = armed_pattern(&guard);
        for banned in ["(?=", "(?!", "(?<", "\\w", "\\b", "\\d"] {
            assert!(!source.contains(banned), "{banned} in {source}");
        }
    }
}

#[test]
fn the_armed_state_names_the_pattern_and_how_to_recover() {
    let state = armed_state(Some(&the_guard()));
    assert_eq!(state["pattern"], armed_pattern(&the_guard()));
    let said = state["reason"].as_str().unwrap();
    assert!(said.contains("not answering") && said.contains("/reload-plugins"));
    assert_eq!(armed_state(None), Value::Null);
}

#[test]
fn the_real_machine_runs_git_with_the_command_s_variables_and_gives_up_on_a_hang() {
    let dir = std::env::temp_dir();
    let dir = dir.to_str().unwrap();
    let ran = Real.git(
        dir,
        &[],
        &[("GIT_CONFIG_COUNT".into(), "0".into())],
        &["--version"],
    );
    let ran = ran.expect("git runs");
    assert_eq!(ran.exit, Some(0));
    assert!(ran.out.starts_with("git version"), "{}", ran.out);
    let bad = Real.git(dir, &[], &[], &["no-such-subcommand"]).unwrap();
    assert_ne!(bad.exit, Some(0));
}

#[tokio::test]
async fn the_guard_stays_a_noop_for_a_call_without_a_command() {
    let r = rig(Pretend::repo());
    let reply = run(&r.table, "guard", &r.fx.cx, json!({ "tool": "Bash" })).await;
    assert!(reply.answer.is_none());
}
