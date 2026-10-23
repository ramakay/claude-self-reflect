//! The Shift Map's own refresh after a turn: port of `refreshStory` and
//! of its call at the end of a turn. The removed `story.ts`, which decides when, is already
//! ported as `companion::story::decide`.
//!
//! Only with `CSR_STORY_REWRITES=1` (one Haiku call each time) and not under the map's kill switch
//! `CSR_NO_STORY_MAP=1`. What the removed `pane.py --story-news` read is the pane's `story_news`
//! here, run in this process on a blocking thread: it is SQLite and file reads, no embedder. One
//! refresh at a time, never awaited by the turn: the decision is made at the turn's end, the work
//! is a task of its own.
//!
//! The telling itself, which the TypeScript started as `csr-engine generate-story --rewrite`, is
//! run the same way here: this binary, spawned again. This is the one deliberate exception to
//! "the engine never spawns itself", which is meant for cheap calls. `generate_story_cli` needs a
//! full `Engine` (the embedder and the search index), and `companion serve` lives as long as the
//! Claude Code process it serves: built in this process, that memory would stay for the session.
//! A child frees it when the telling is done, and `RUN_LIMIT` kills one that hangs.
//!
//! The decision, in order: another refresh running (`Busy`), the environment (`Off`), then
//! `story::decide` over the news, the starts recorded in the store row `(sid, "storyStarts")`
//! (the TypeScript's `storyStarts:<sid>`), and the tokens Claude has spent (`pace::pace_of`).

use super::{pace::pace_of, state::is_person};
use crate::companion::{
    db::Db,
    paths::Paths,
    relay::{
        dispatch::{handler, Ctx, Handler, Reply, Table},
        host::BoxFuture,
    },
    story::{self, Facts, News, Why},
};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

/// How long the news may take before it reads as no answer (`paneWithin`, 10 s).
pub const NEWS_LIMIT: Duration = Duration::from_secs(10);
/// How long a telling may run before its process is killed (the TypeScript's process limit).
pub const RUN_LIMIT: Duration = Duration::from_secs(180);

/// What `story_news` found, as `decide` reads it, and the transcript the telling would read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Told {
    pub facts: Facts,
    pub transcript: Option<String>,
}

/// The news of a session: `(cwd, sid, now in seconds)`.
pub type NewsFn = Arc<dyn Fn(String, String, f64) -> BoxFuture<Option<Told>> + Send + Sync>;
/// One telling again: `(transcript, cwd)`. Done when the story is written or skipped.
pub type RunFn = Arc<dyn Fn(String, String) -> BoxFuture<()> + Send + Sync>;

/// What the refresh asks of the rest of the engine; tests give their own.
#[derive(Clone)]
pub struct Seams {
    pub news: NewsFn,
    pub run: RunFn,
}

impl Seams {
    /// The news read here, the telling run by `program` (this binary, in the app) with `limit`.
    pub fn spawning(program: PathBuf, limit: Duration) -> Self {
        Self {
            news: Arc::new(|cwd, sid, now| {
                Box::pin(async move {
                    let read = move || read_news(&Paths::from_env(), &cwd, &sid, now);
                    tokio::task::spawn_blocking(read).await.ok()
                })
            }),
            run: Arc::new(move |transcript, cwd| {
                let program = program.clone();
                Box::pin(async move {
                    let mut argv = story::refresh_argv(&transcript, &cwd);
                    let args = argv.split_off(1); // argv[0] is the name the program goes by
                    run_program(&program, &args, limit).await;
                })
            }),
        }
    }

    pub fn real() -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("csr-engine"));
        Self::spawning(exe, RUN_LIMIT)
    }
}

/// How a spawned telling came to an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ran {
    /// It exited; true where it exited well.
    Exited(bool),
    /// It was still running at the limit and was killed.
    TimedOut,
    /// The program could not be started.
    NotStarted,
}

/// Run `program args` with nothing on stdin, stdout or stderr. A child that outlives `limit` is
/// killed and waited for, and one whose future is dropped is killed too (`kill_on_drop`).
pub async fn run_program(program: &Path, args: &[String], limit: Duration) -> Ran {
    let spawned = tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = spawned else {
        return Ran::NotStarted;
    };
    match tokio::time::timeout(limit, child.wait()).await {
        Ok(status) => Ran::Exited(status.is_ok_and(|status| status.success())),
        Err(_) => {
            let _ = child.kill().await;
            Ran::TimedOut
        }
    }
}

/// The removed `pane.py --story-news`: the same as the pane's `--story-news` mode.
fn read_news(paths: &Paths, cwd: &str, sid: &str, now: f64) -> Told {
    let news = match Db::open(paths).ok() {
        Some(db) => {
            let root = db.pane_root(Path::new(cwd));
            story::story_news_in(&db, paths, root.as_deref(), sid, now)
        }
        None => News::Absent { story: false },
    };
    let transcript = match &news {
        News::Present(news) => news.transcript.as_ref().map(|t| t.display().to_string()),
        News::Absent { .. } => None,
    };
    Told {
        facts: Facts::of(&news),
        transcript,
    }
}

/// What a refresh came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refresh {
    /// A gate said no, or `Go`: a refresh was started.
    Why(Why),
    /// The environment does not ask for rewrites, or the map is switched off.
    Off,
    /// Another refresh is running.
    Busy,
}

/// Whether a refresh is running now (the TypeScript's `storyRefreshing`).
#[derive(Default)]
struct Refreshing(bool);

async fn env_is_one(cx: &Ctx, name: &str) -> bool {
    matches!(
        cx.host.call("env.get", vec![json!(name)]).await,
        Ok(Value::String(value)) if value == "1"
    )
}

/// The decision, and the refresh when it is `Go`. Returns what happened and whether a refresh
/// was started.
async fn decide_and_start(cx: &Ctx, seams: &Seams) -> (Option<Refresh>, bool) {
    let cwd = cx.cwd.clone();
    let now = cx.clock.now();
    let news = tokio::time::timeout(
        NEWS_LIMIT,
        (seams.news)(cwd.clone(), cx.sid.clone(), now / 1000.),
    );
    let told = news.await.ok().flatten().unwrap_or_default();
    let kept = (cx.store.get(&cx.sid, "storyStarts").ok().flatten()).unwrap_or(Value::Null);
    let starts: Vec<f64> = kept
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .filter(|at| at.is_finite() && now - at < story::CHAPTER_MS)
        .collect();
    let why = story::decide(&told.facts, &starts, now, pace_of(cx).claude);
    let Some(transcript) = told.transcript.filter(|_| why == Why::Go) else {
        return (Some(Refresh::Why(why)), false);
    };
    let mut starts = starts;
    starts.push(now);
    if cx
        .store
        .set(&cx.sid, "storyStarts", &json!(starts))
        .is_err()
    {
        return (None, false);
    }
    let (cx, run) = (cx.clone(), seams.run.clone());
    tokio::spawn(async move {
        run(transcript, cwd).await;
        cx.kept(|s: &mut Refreshing| s.0 = false);
    });
    (Some(Refresh::Why(Why::Go)), true)
}

/// Tell the story again if the news earns it (the TypeScript companion's `refreshStory`).
pub async fn refresh_story(cx: &Ctx, seams: &Seams) -> Option<Refresh> {
    if cx.kept(|s: &mut Refreshing| s.0) {
        return Some(Refresh::Busy);
    }
    let rewrites = env_is_one(cx, "CSR_STORY_REWRITES").await;
    let killed = env_is_one(cx, "CSR_NO_STORY_MAP").await;
    if !rewrites || killed {
        return Some(Refresh::Off);
    }
    // Another turn's refresh may have started while the environment was read.
    if cx.kept(|s: &mut Refreshing| std::mem::replace(&mut s.0, true)) {
        return Some(Refresh::Busy);
    }
    let (done, started) = decide_and_start(cx, seams).await;
    if !started {
        cx.kept(|s: &mut Refreshing| s.0 = false);
    }
    done
}

/// `turn_complete`: on an answer of the person's own session.
fn on_turn_complete(seams: Seams) -> Handler {
    handler(move |cx, e| {
        let seams = seams.clone();
        async move {
            // An agent's turn is the fleet's.
            if !e["agentId"].is_null() || e["reason"] != "answer" {
                return Ok(Reply::none());
            }
            let person = is_person(&cx).await;
            let refresh = if person {
                refresh_story(&cx, &seams).await
            } else {
                None
            };
            // Why the story was or was not told again at this answer: a stale story is read here.
            let why = refresh.map_or("none".to_string(), |r| format!("{r:?}"));
            let _ = cx.store.set(
                &cx.sid,
                "storyWhy",
                &json!({ "why": why, "person": person, "at": cx.clock.now() }),
            );
            Ok(Reply::none())
        }
    })
}

pub fn register_with(t: &mut Table, seams: Seams) {
    t.on("turn_complete", "story", on_turn_complete(seams));
}

pub fn register(t: &mut Table) {
    register_with(t, Seams::real());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::relay::{
        dispatch::testing::{fixture_in, run, until, Fixture},
        host::fake::World,
    };
    use std::sync::Mutex;
    use tokio::sync::Notify;

    const SID: &str = "sess-1";

    /// The facts of a story that is due: news, a transcript, no spend that stops it.
    fn due() -> Facts {
        Facts {
            story: true,
            at: Some(0.),
            messages_since: 80.,
            prompts_since: 3.,
            receipts: 6.,
            usd_24h: Some(0.),
            has_transcript: true,
            ..Facts::default()
        }
    }

    fn told(facts: Facts) -> Told {
        Told {
            facts,
            transcript: Some("/t/s.jsonl".into()),
        }
    }

    /// A fake engine: the news it answers, the refreshes it was asked for, and a gate a test may
    /// close to hold a refresh running.
    struct Engine {
        news: Mutex<Option<Told>>,
        asked: Mutex<Vec<(String, f64)>>,
        ran: Mutex<Vec<(String, String)>>,
        hold: Option<Arc<Notify>>,
        hang_news: bool,
    }

    fn engine(news: Option<Told>, hold: Option<Arc<Notify>>, hang_news: bool) -> Arc<Engine> {
        Arc::new(Engine {
            news: Mutex::new(news),
            asked: Mutex::default(),
            ran: Mutex::default(),
            hold,
            hang_news,
        })
    }

    fn seams(engine: &Arc<Engine>) -> Seams {
        let (news, ran) = (engine.clone(), engine.clone());
        Seams {
            news: Arc::new(move |cwd, _sid, now| {
                let engine = news.clone();
                Box::pin(async move {
                    engine.asked.lock().unwrap().push((cwd, now));
                    if engine.hang_news {
                        std::future::pending::<()>().await;
                    }
                    engine.news.lock().unwrap().clone()
                })
            }),
            run: Arc::new(move |transcript, cwd| {
                let engine = ran.clone();
                Box::pin(async move {
                    engine.ran.lock().unwrap().push((transcript, cwd));
                    if let Some(gate) = &engine.hold {
                        gate.notified().await;
                    }
                })
            }),
        }
    }

    /// A world whose `env.get` answers `(CSR_STORY_REWRITES, CSR_NO_STORY_MAP)` for each of the
    /// next `times` refreshes; a refresh that stops early leaves some unread.
    fn world(rewrites: Option<&str>, killed: Option<&str>, times: usize) -> World {
        let said = |v: Option<&str>| Ok(v.map_or(Value::Null, |v| json!(v)));
        let mut world = World::default();
        let env = world.script.entry("env.get".into()).or_default();
        for _ in 0..times {
            env.push_back(said(rewrites));
            env.push_back(said(killed));
        }
        world
    }

    fn fx(rewrites: Option<&str>, killed: Option<&str>) -> Fixture {
        fixture_in(world(rewrites, killed, 8), SID)
    }

    fn starts(fx: &Fixture) -> Value {
        fx.cx
            .store
            .get(SID, "storyStarts")
            .unwrap()
            .unwrap_or(Value::Null)
    }

    fn put_starts(fx: &Fixture, value: Value) {
        fx.cx.store.set(SID, "storyStarts", &value).unwrap();
    }

    fn refreshing(cx: &Ctx) -> bool {
        cx.kept(|s: &mut Refreshing| s.0)
    }

    #[tokio::test]
    async fn the_story_is_not_told_again_unless_the_environment_asks_for_rewrites() {
        for (rewrites, killed) in [(None, None), (Some("0"), None), (Some("1"), Some("1"))] {
            let fx = fx(rewrites, killed);
            let engine = engine(Some(told(due())), None, false);
            let got = refresh_story(&fx.cx, &seams(&engine)).await;
            assert_eq!(got, Some(Refresh::Off), "{rewrites:?} {killed:?}");
            assert!(
                engine.asked.lock().unwrap().is_empty(),
                "the news is not even read"
            );
            assert!(engine.ran.lock().unwrap().is_empty());
            assert_eq!(starts(&fx), Value::Null);
            assert!(!refreshing(&fx.cx));
        }
    }

    #[tokio::test]
    async fn a_story_that_is_due_is_told_again_once_and_the_start_is_kept() {
        let fx = fx(Some("1"), None);
        let engine = engine(Some(told(due())), None, false);
        fx.clock.set(1_000_000.);
        let got = refresh_story(&fx.cx, &seams(&engine)).await;
        assert_eq!(got, Some(Refresh::Why(Why::Go)));
        assert!(until(|| !engine.ran.lock().unwrap().is_empty()).await);
        let ran = [("/t/s.jsonl".to_string(), "/repo".to_string())];
        assert_eq!(*engine.ran.lock().unwrap(), ran);
        assert_eq!(
            *engine.asked.lock().unwrap(),
            [("/repo".to_string(), 1_000.)]
        );
        assert_eq!(starts(&fx), json!([1_000_000.]));
    }

    #[tokio::test]
    async fn the_reason_a_story_is_not_due_is_said_and_nothing_is_started() {
        let thin = Facts {
            receipts: 1.,
            ..due()
        };
        let no_transcript = Facts {
            has_transcript: false,
            ..due()
        };
        let cases = [
            (Facts::default(), Why::NoStory),
            (no_transcript, Why::NoTranscript),
            (thin, Why::NoNews),
        ];
        for (facts, why) in cases {
            let fx = fx(Some("1"), None);
            fx.clock.set(1_000_000.);
            let engine = engine(Some(told(facts)), None, false);
            assert_eq!(
                refresh_story(&fx.cx, &seams(&engine)).await,
                Some(Refresh::Why(why))
            );
            assert!(engine.ran.lock().unwrap().is_empty(), "{why:?}");
            assert_eq!(starts(&fx), Value::Null, "{why:?}");
            // The flag is let go: the next turn's refresh is not `Busy`.
            let again = refresh_story(&fx.cx, &seams(&engine)).await;
            assert_eq!(again, Some(Refresh::Why(why)));
        }
    }

    #[tokio::test]
    async fn a_news_that_does_not_come_reads_as_no_story_and_never_blocks_the_turn() {
        let fx = fx(Some("1"), None);
        let engine = engine(Some(told(due())), None, true);
        tokio::time::pause();
        let got = refresh_story(&fx.cx, &seams(&engine)).await;
        assert_eq!(got, Some(Refresh::Why(Why::NoStory)));
        assert!(engine.ran.lock().unwrap().is_empty());
        assert!(!refreshing(&fx.cx));
    }

    #[tokio::test]
    async fn the_starts_of_a_chapter_ago_are_dropped_and_the_recent_ones_hold_the_next_back() {
        let fx = fx(Some("1"), None);
        let now = 10. * story::CHAPTER_MS;
        fx.clock.set(now);
        let kept = json!([now - story::CHAPTER_MS - 5., "x", now - 30_000.]);
        put_starts(&fx, kept.clone());
        let engine = engine(Some(told(due())), None, false);
        let got = refresh_story(&fx.cx, &seams(&engine)).await;
        assert_eq!(
            got,
            Some(Refresh::Why(Why::Cooldown)),
            "one started 30 s ago"
        );
        assert_eq!(
            starts(&fx),
            kept,
            "a refresh that did not start writes nothing"
        );
        put_starts(
            &fx,
            json!([now - story::CHAPTER_MS - 5., now - 2. * 60_000.]),
        );
        let got = refresh_story(&fx.cx, &seams(&engine)).await;
        assert_eq!(got, Some(Refresh::Why(Why::Go)));
        assert_eq!(
            starts(&fx),
            json!([now - 2. * 60_000., now]),
            "the old one is let go"
        );
    }

    #[tokio::test]
    async fn what_claude_has_spent_in_this_session_bounds_what_the_story_may_cost() {
        let costly = Facts {
            tokens_2h: 500.,
            ..due()
        };
        let counted = fx(Some("1"), None);
        let pace = json!({ "session": SID, "claude": 10_000 });
        counted.cx.store.set(SID, "pace", &pace).unwrap();
        let engine = engine(Some(told(costly.clone())), None, false);
        let got = refresh_story(&counted.cx, &seams(&engine)).await;
        assert_eq!(
            got,
            Some(Refresh::Why(Why::Share)),
            "500 + 1600 > 2% of 10000"
        );
        // Where nothing was counted (a fresh session) the share does not hold it back.
        let fresh = fx(Some("1"), None);
        let engine = self::engine(Some(told(costly)), None, false);
        assert_eq!(
            refresh_story(&fresh.cx, &seams(&engine)).await,
            Some(Refresh::Why(Why::Go))
        );
    }

    #[tokio::test]
    async fn one_refresh_at_a_time_and_the_next_one_may_start_when_it_ends() {
        let fx = fx(Some("1"), None);
        let gate = Arc::new(Notify::new());
        let engine = engine(Some(told(due())), Some(gate.clone()), false);
        let seams = seams(&engine);
        fx.clock.set(10. * story::CHAPTER_MS);
        assert_eq!(
            refresh_story(&fx.cx, &seams).await,
            Some(Refresh::Why(Why::Go))
        );
        assert!(until(|| !engine.ran.lock().unwrap().is_empty()).await);
        assert_eq!(
            refresh_story(&fx.cx, &seams).await,
            Some(Refresh::Busy),
            "still running"
        );
        assert_eq!(
            engine.asked.lock().unwrap().len(),
            1,
            "a busy refresh reads nothing"
        );
        gate.notify_one();
        assert!(
            until(|| !refreshing(&fx.cx)).await,
            "the refresh ended and let go of the flag"
        );
        // The next turn asks again: not busy any more, held back by the cooldown of the last.
        assert_eq!(
            refresh_story(&fx.cx, &seams).await,
            Some(Refresh::Why(Why::Cooldown))
        );
    }

    /// A program under `dir`: a shell script with `body`.
    fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Wait up to ten seconds for `done`, on the real clock.
    async fn within_ten_seconds(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..500 {
            if done() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        done()
    }

    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[tokio::test]
    async fn a_telling_is_this_program_spawned_with_the_generate_story_argv() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("argv");
        let body = format!("printf '%s\\n' \"$@\" > '{}'", seen.display());
        let fake = program(dir.path(), "csr-engine", &body);
        let argv: Vec<String> = story::refresh_argv("/t/s.jsonl", "/repo")[1..].to_vec();
        assert_eq!(
            run_program(&fake, &argv, RUN_LIMIT).await,
            Ran::Exited(true)
        );
        let written = std::fs::read_to_string(&seen).unwrap();
        let args: Vec<&str> = written.lines().collect();
        let want = [
            "generate-story",
            "--transcript",
            "/t/s.jsonl",
            "--cwd",
            "/repo",
            "--rewrite",
        ];
        assert_eq!(
            args, want,
            "argv[0] is the program itself and is not passed"
        );
    }

    #[tokio::test]
    async fn a_telling_that_fails_or_cannot_start_is_told_apart_from_one_that_ends() {
        let dir = tempfile::tempdir().unwrap();
        let fails = program(dir.path(), "fails", "exit 3");
        assert_eq!(
            run_program(&fails, &[], RUN_LIMIT).await,
            Ran::Exited(false)
        );
        let missing = dir.path().join("not-there");
        assert_eq!(run_program(&missing, &[], RUN_LIMIT).await, Ran::NotStarted);
    }

    #[tokio::test]
    async fn a_telling_that_hangs_is_killed_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let body = format!("echo $$ > '{}'\nexec sleep 60", pid_file.display());
        let hangs = program(dir.path(), "hangs", &body);
        let limit = Duration::from_secs(2);
        let began = std::time::Instant::now();
        assert_eq!(run_program(&hangs, &[], limit).await, Ran::TimedOut);
        assert!(
            began.elapsed() < Duration::from_secs(30),
            "not left to finish its sleep"
        );
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_string();
        assert!(!pid.is_empty());
        assert!(!alive(&pid), "the child {pid} is gone, not left running");
    }

    #[tokio::test]
    async fn a_refresh_through_the_program_seam_runs_it_once_and_lets_go_when_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("argv");
        let body = format!("printf '%s\\n' \"$@\" >> '{}'", seen.display());
        let fake = program(dir.path(), "csr-engine", &body);
        let fx = fx(Some("1"), None);
        let engine = engine(Some(told(due())), None, false);
        let through = Seams {
            run: Seams::spawning(fake, RUN_LIMIT).run,
            ..seams(&engine)
        };
        let got = refresh_story(&fx.cx, &through).await;
        assert_eq!(got, Some(Refresh::Why(Why::Go)));
        assert!(within_ten_seconds(|| !refreshing(&fx.cx)).await, "it ended");
        let written = std::fs::read_to_string(&seen).unwrap();
        assert_eq!(written.lines().count(), 6, "{written}");
        assert!(written.starts_with("generate-story\n--transcript\n/t/s.jsonl\n"));
    }

    #[tokio::test]
    async fn a_refresh_that_never_ends_is_killed_and_counts_as_ended_after_its_limit() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let body = format!("echo $$ > '{}'\nexec sleep 60", pid_file.display());
        let hangs = program(dir.path(), "csr-engine", &body);
        let fx = fx(Some("1"), None);
        let engine = engine(Some(told(due())), None, false);
        let through = Seams {
            run: Seams::spawning(hangs, Duration::from_secs(2)).run,
            ..seams(&engine)
        };
        let got = refresh_story(&fx.cx, &through).await;
        assert_eq!(got, Some(Refresh::Why(Why::Go)));
        assert!(refreshing(&fx.cx), "it has not ended yet");
        assert!(
            within_ten_seconds(|| !refreshing(&fx.cx)).await,
            "the limit let go of it"
        );
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_string();
        assert!(!alive(&pid), "the child {pid} was killed");
    }

    #[tokio::test]
    async fn a_turn_s_answer_of_a_person_asks_for_a_refresh_and_nothing_else_does() {
        let mut table = Table::new();
        let engine = engine(Some(told(due())), None, false);
        register_with(&mut table, seams(&engine));
        let answer = json!({ "answer": "ok", "durationMs": 1, "isAborted": false,
            "turnId": "t1", "reason": "answer" });
        let asked = |engine: &Arc<Engine>| engine.asked.lock().unwrap().len();
        // Not an answer: an interruption, a subagent's turn.
        let fx = fx(Some("1"), None);
        let other = json!({ "reason": "interrupted", "turnId": "t1" });
        run(&table, "turn_complete", &fx.cx, other).await;
        let agent = json!({ "reason": "answer", "turnId": "t1", "agentId": "a1" });
        run(&table, "turn_complete", &fx.cx, agent).await;
        assert_eq!(asked(&engine), 0);
        // A session no person is at (a `claude -p` child).
        fx.cx.session.lock().unwrap().interactive = Some(false);
        run(&table, "turn_complete", &fx.cx, answer.clone()).await;
        assert_eq!(asked(&engine), 0);
        let why = |fx: &Fixture| fx.cx.store.get(SID, "storyWhy").unwrap().unwrap();
        assert_eq!(why(&fx)["why"], "none");
        assert_eq!(why(&fx)["person"], false);
        // A person's answer asks, and says what came of it.
        fx.cx.session.lock().unwrap().interactive = Some(true);
        run(&table, "turn_complete", &fx.cx, answer).await;
        assert_eq!(asked(&engine), 1);
        assert!(until(|| !engine.ran.lock().unwrap().is_empty()).await);
        assert_eq!(why(&fx)["why"], "Why(Go)");
        assert_eq!(why(&fx)["person"], true);
    }

    #[test]
    fn the_story_registers_on_the_turn_s_end_only() {
        let mut t = Table::new();
        register(&mut t);
        assert_eq!(t.features("turn_complete"), ["story"]);
        for key in ["step", "chapter", "start"] {
            assert!(t.features(key).is_empty(), "{key}");
        }
    }
}
