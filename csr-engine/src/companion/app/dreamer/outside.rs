//! What the dreamer asks of the machine around the engine, as one seam.
//!
//! The TypeScript ran the removed `lease.py`, the removed `pane.py` (`--brief`, `--keep`) and `git`
//! as processes and read the dream log with `$.fs.read`. The engine never spawns itself, so `Here`
//! calls what those modes ran: `lease::step_under_lock`, `brief::brief`, the pane's `--keep`
//! (`pace::real_keep`), and `git cat-file`. A test puts `fake::Fake` in its place and so touches
//! no HOME, no database and no repository.

use crate::{
    companion::{
        app::pace::{real_keep, Keep},
        brief,
        db::Db,
        lease::{self, LeaseArgs, LeaseOutput, Step},
        paths::{self, Paths},
    },
    process::CommandDeadline,
};
use anyhow::Result;
use async_trait::async_trait;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

/// A lease step, a brief and a `git cat-file` each get two seconds, as `process.run` did.
const STEP_LIMIT: Duration = Duration::from_secs(2);
/// Where the lease files live, under the pane's folder (`LEASES` in the TypeScript companion).
const LEASES: &str = "dreamer-lease";

#[async_trait]
// async_trait marks each boxed-future method `#[must_use]`; clippy 1.99 flags that as doubled.
#[allow(clippy::double_must_use)]
pub trait Outside: Send + Sync {
    /// One lease step for `session`: whether this copy holds the lease afterwards (`lease`).
    async fn lease(&self, session: &str, step: Step, busy: bool, now: f64) -> bool;

    /// What the removed `pane.py --brief` printed for the session, or none when it failed (`run`).
    async fn brief(&self, cwd: &str, session: &str, now: f64) -> Option<String>;

    /// Where `--keep` records go.
    fn keep(&self) -> Keep;

    /// The dream log's text: none with no HOME, empty where it cannot be read.
    async fn dream_log(&self) -> Option<String>;

    /// The cited commits this repository does not have (`missingCommits`).
    async fn missing_commits(&self, cwd: &str, commits: &[String]) -> HashSet<String>;

    /// `CSR_NO_YSK_BRIEF=1`: You should know's prompt is left alone.
    fn no_ysk_brief(&self) -> bool;
}

/// A copy of the plugin that lives in the plugin cache ranks under a dev copy (`rankOf`).
pub fn rank_of(root: &str) -> i64 {
    if root.contains("/.claude/plugins/") {
        0
    } else {
        1
    }
}

/// The arguments of `companion lease STEP` for this copy (`lease` in the TypeScript companion).
pub fn lease_args(
    paths: &Paths,
    root: &str,
    session: &str,
    step: Step,
    busy: bool,
    now: f64,
) -> LeaseArgs {
    LeaseArgs {
        step,
        folder: paths.keep.join(LEASES),
        session: session.to_string(),
        root: root.to_string(),
        rank: rank_of(root),
        now,
        busy,
    }
}

/// Whether the lease step says this copy holds the lease (`leaseHeld`). Only a step that ran and
/// said so grants it: one that crashed, could not get its lock in time or was refused its
/// arguments does not. (The TypeScript left one case open, a `python3` that could not start; the
/// engine is always there, so that case is gone. The one left open is no HOME: `no_home`.)
pub fn lease_held(answer: &Result<LeaseOutput>) -> bool {
    matches!(answer, Ok(out) if out.held)
}

/// With no HOME there is no lease to renew, so an active look cannot keep ownership; any other
/// step is granted (nothing can be leased on this machine, and the look goes on).
pub fn no_home(step: Step) -> bool {
    !matches!(step, Step::Renew)
}

/// The engine's own copy: what the real hooks run.
pub struct Here;

/// Where this copy runs from, which is all that tells it from another copy on the same session.
fn root() -> String {
    (std::env::current_exe().ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "csr-engine".to_string())
}

#[async_trait]
impl Outside for Here {
    async fn lease(&self, session: &str, step: Step, busy: bool, now: f64) -> bool {
        let paths = Paths::from_env();
        if paths.home.as_os_str().is_empty() {
            return no_home(step);
        }
        let args = lease_args(&paths, &root(), session, step, busy, now);
        let step = tokio::task::spawn_blocking(move || lease::step_under_lock(&args));
        match tokio::time::timeout(STEP_LIMIT, step).await {
            Ok(Ok(answer)) => lease_held(&answer),
            _ => false,
        }
    }

    async fn brief(&self, cwd: &str, session: &str, now: f64) -> Option<String> {
        let (cwd, session) = (cwd.to_string(), session.to_string());
        let work = tokio::task::spawn_blocking(move || brief_of(&cwd, &session, now));
        work.await.ok().flatten()
    }

    fn keep(&self) -> Keep {
        real_keep()
    }

    async fn dream_log(&self) -> Option<String> {
        let paths = Paths::from_env();
        if paths.home.as_os_str().is_empty() {
            return None;
        }
        let path = paths.keep.join("dreams.jsonl");
        let read = tokio::task::spawn_blocking(move || std::fs::read_to_string(path));
        Some(read.await.ok().and_then(Result::ok).unwrap_or_default())
    }

    async fn missing_commits(&self, cwd: &str, commits: &[String]) -> HashSet<String> {
        let (cwd, commits) = (cwd.to_string(), commits.to_vec());
        let work = tokio::task::spawn_blocking(move || missing_in(&cwd, &commits));
        work.await.unwrap_or_default()
    }

    fn no_ysk_brief(&self) -> bool {
        std::env::var("CSR_NO_YSK_BRIEF").is_ok_and(|value| value == "1")
    }
}

/// The removed `pane.py --brief --sid SID` run for `cwd`: its text and a newline, or nothing when
/// it failed, and an empty text where there is no repository to read.
fn brief_of(cwd: &str, session: &str, now: f64) -> Option<String> {
    let paths = Paths::from_env();
    if session.is_empty() {
        return Some(String::new());
    }
    let repo = Path::new(cwd);
    let db = Db::open(&paths).ok();
    let root: Option<PathBuf> = paths::git(repo, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .or_else(|| db.as_ref().and_then(|db| db.pane_root(repo)));
    let Some(root) = root else {
        return Some(String::new());
    };
    let db = db?;
    Some(format!(
        "{}\n",
        brief::brief(&db, &paths, &root, session, now / 1000.)
    ))
}

/// The first five of `commits` that `git cat-file -e OID^{commit}` says are not there; a git that
/// cannot answer marks nothing.
fn missing_in(cwd: &str, commits: &[String]) -> HashSet<String> {
    let mut missing = HashSet::new();
    for oid in commits.iter().take(5) {
        let ran = std::process::Command::new("git")
            .args(["-C", cwd, "cat-file", "-e", &format!("{oid}^{{commit}}")])
            .output_with_deadline(STEP_LIMIT);
        if ran.is_ok_and(|out| !out.status.success() && out.status.code().is_some()) {
            missing.insert(oid.clone());
        }
    }
    missing
}

/// A stand-in for the machine, for tests: a lease on files of its own, a brief a test sets, a
/// dream log kept as text, and the commits the repository has.
#[cfg(test)]
pub mod fake {
    use super::*;
    use crate::companion::relay::host::{fake::FakeHost, Call, Detached, Host};
    use serde_json::Value;
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    /// Where this copy of the plugin claims to run from: a dev copy, rank 1.
    pub const ROOT: &str = "/home/u/dev-mods/csr-companion";
    pub const HOME: &str = "/home/u";

    /// One lease step as the dreamer made it.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Asked {
        pub step: String,
        pub folder: PathBuf,
        pub session: String,
        pub root: String,
        pub rank: i64,
        pub busy: bool,
    }

    pub struct Fake {
        keep: Keep,
        pub root: Mutex<String>,
        /// The lease files, by session: what the removed `lease.py` read and wrote.
        pub files: Mutex<HashMap<String, Value>>,
        pub asked: Mutex<Vec<Asked>>,
        /// The dream log's text; none: there is no HOME.
        pub log: Mutex<Option<String>>,
        pub brief: Mutex<String>,
        pub briefs: AtomicUsize,
        /// The commits `git cat-file` finds; every other one is missing.
        pub commits: Mutex<HashSet<String>>,
        pub no_ysk_brief: AtomicBool,
        /// Whether a lease step fails to run at all (a crash, a lock not got in time).
        pub lease_fails: AtomicBool,
    }

    impl Fake {
        pub fn new(keep: Keep, brief: &str) -> Self {
            Self {
                keep,
                root: Mutex::new(ROOT.to_string()),
                files: Mutex::default(),
                asked: Mutex::default(),
                log: Mutex::new(Some(String::new())),
                brief: Mutex::new(brief.to_string()),
                briefs: AtomicUsize::new(0),
                commits: Mutex::default(),
                no_ysk_brief: AtomicBool::new(false),
                lease_fails: AtomicBool::new(false),
            }
        }

        pub fn set_brief(&self, text: &str) {
            *self.brief.lock().unwrap() = text.to_string();
        }

        pub fn set_log(&self, text: &str) {
            *self.log.lock().unwrap() = Some(text.to_string());
        }

        pub fn lease_file(&self, session: &str) -> Value {
            (self.files.lock().unwrap().get(session))
                .cloned()
                .unwrap_or(Value::Null)
        }

        pub fn put_lease(&self, session: &str, file: Value) {
            self.files.lock().unwrap().insert(session.to_string(), file);
        }

        pub fn steps(&self, step: &str) -> Vec<Asked> {
            let asked = self.asked.lock().unwrap();
            (asked.iter())
                .filter(|ask| ask.step == step)
                .cloned()
                .collect()
        }
    }

    #[async_trait]
    impl Outside for Fake {
        async fn lease(&self, session: &str, step: Step, busy: bool, now: f64) -> bool {
            let paths = Paths::new(HOME.into());
            let root = self.root.lock().unwrap().clone();
            let args = lease_args(&paths, &root, session, step, busy, now);
            self.asked.lock().unwrap().push(Asked {
                step: format!("{step:?}").to_lowercase(),
                folder: args.folder.clone(),
                session: session.to_string(),
                root: root.clone(),
                rank: args.rank,
                busy,
            });
            if self.lease_fails.load(Ordering::SeqCst) {
                return lease_held(&Err(anyhow::anyhow!("the lease step did not run")));
            }
            // What the removed `lease.py` did on its file, with the decision the engine's own code
            // makes.
            let mut files = self.files.lock().unwrap();
            let held = files.get(session).cloned().unwrap_or(Value::Null);
            let (mine, write) = lease::decide(&args, &held);
            let lease = write.clone().unwrap_or(held);
            if let Some(write) = write {
                files.insert(session.to_string(), write);
            }
            lease_held(&Ok(LeaseOutput { held: mine, lease }))
        }

        async fn brief(&self, _cwd: &str, _session: &str, _now: f64) -> Option<String> {
            self.briefs.fetch_add(1, Ordering::SeqCst);
            Some(self.brief.lock().unwrap().clone())
        }

        fn keep(&self) -> Keep {
            self.keep.clone()
        }

        async fn dream_log(&self) -> Option<String> {
            self.log.lock().unwrap().clone()
        }

        async fn missing_commits(&self, _cwd: &str, commits: &[String]) -> HashSet<String> {
            let present = self.commits.lock().unwrap();
            (commits.iter().take(5))
                .filter(|oid| !present.contains(*oid))
                .cloned()
                .collect()
        }

        fn no_ysk_brief(&self) -> bool {
            self.no_ysk_brief.load(Ordering::SeqCst)
        }
    }

    /// A host whose forks are made and then wait, answer unseen, until a test lets them go (the
    /// TypeScript tests' `forkGate`). Every other call goes straight through.
    #[derive(Clone)]
    pub struct Gated {
        inner: FakeHost,
        open: Arc<tokio::sync::watch::Sender<bool>>,
        arrived: Arc<AtomicUsize>,
    }

    impl Gated {
        pub fn new(inner: FakeHost) -> Self {
            let (open, _) = tokio::sync::watch::channel(false);
            Self {
                inner,
                open: Arc::new(open),
                arrived: Arc::new(AtomicUsize::new(0)),
            }
        }

        /// A gate that lets forks through until `hold`.
        pub fn passing(inner: FakeHost) -> Self {
            let gate = Self::new(inner);
            gate.open();
            gate
        }

        /// Let the forks go, those waiting and those to come.
        pub fn open(&self) {
            self.open.send_replace(true);
        }

        /// Hold the forks to come until `open`.
        pub fn hold(&self) {
            self.open.send_replace(false);
        }

        /// How many forks reached the gate.
        pub fn arrived(&self) -> usize {
            self.arrived.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Host for Gated {
        async fn batch(&self, calls: Vec<Call>) -> Result<Vec<Result<Value, String>>> {
            let forks = calls.iter().any(|call| call.m == "model.fork");
            // The fork is made (and seen) first, as the TypeScript host's was; its answer waits.
            let answer = self.inner.batch(calls).await;
            if forks {
                self.arrived.fetch_add(1, Ordering::SeqCst);
                let mut watch = self.open.subscribe();
                let _ = watch.wait_for(|open| *open).await;
            }
            answer
        }

        fn detach(&self, work: Detached) {
            tokio::spawn(work(Arc::new(self.clone())));
        }
    }
}
