use super::*;
use crate::codemap::query;
use crate::codemap::testutil::{py, write, Fx};
use std::sync::Mutex;

struct Rig {
    fx: Fx,
    _db_dir: tempfile::TempDir,
    db: PathBuf,
    shutdown: Arc<AtomicBool>,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            fx: Fx::new(),
            db: dir.path().join("csr-engine.db"),
            _db_dir: dir,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
    fn db_path(&self) -> PathBuf {
        self.db.clone()
    }
    fn ctx(&self, opts: LoopOpts) -> Ctx<'_> {
        Ctx::new(
            &self.db,
            &self.fx.home,
            self.shutdown.clone(),
            opts,
            None,
            &quiet,
        )
    }
    fn register(&self, repo: &Path) {
        let storage = Storage::open(&self.db_path()).unwrap();
        let common = repo.join(".git");
        storage
            .with_connection(|c| {
                store::upsert_repo(
                    c,
                    &common.to_string_lossy(),
                    Some(&repo.to_string_lossy()),
                    "cli",
                    true,
                )
            })
            .unwrap();
    }
    /// A repo discovery registered from a session signal `age` seconds ago, as
    /// the daemon stores it (row `signal`, signal row resolved to the repo).
    fn signalled(&self, repo: &Path, age: i64) {
        let common = repo.join(".git").to_string_lossy().to_string();
        let at = crate::codemap::now_secs() - age;
        self.conn()
            .with_connection(|c| {
                store::upsert_repo(c, &common, Some(&repo.to_string_lossy()), "signal", true)?;
                store::signal_upsert(c, &repo.to_string_lossy(), "session_start", at)?;
                store::signal_resolve(c, &repo.to_string_lossy(), Some(&common), "repo")
            })
            .unwrap();
    }
    /// Move a repo's signal to `age` seconds ago (an upsert never moves it back).
    fn set_signal_age(&self, repo: &Path, age: i64) {
        let at = crate::codemap::now_secs() - age;
        self.conn()
            .with_connection(|c| {
                c.execute(
                    "UPDATE codemap_signals SET last_at = ?2 WHERE cwd = ?1",
                    rusqlite::params![repo.to_string_lossy(), at],
                )?;
                Ok(())
            })
            .unwrap();
    }
    fn conn(&self) -> Storage {
        Storage::open(&self.db_path()).unwrap()
    }
    fn hits(&self, name: &str) -> usize {
        self.conn()
            .with_connection(|c| query::query(c, name, 20))
            .unwrap()
            .len()
    }
    fn opts(&self) -> LoopOpts {
        let mut o = LoopOpts::default();
        o.scan.home = Some(self.fx.home.clone());
        o.debounce = Duration::from_millis(150);
        o.max_wait = Duration::from_secs(2);
        o.queue_tick = Duration::from_millis(200);
        o.tick = Duration::from_millis(100);
        o.lock_wait = Duration::from_secs(10);
        o
    }
}

fn quiet(_: &str) {}

fn wait_until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    f()
}

#[test]
fn poll_scans_a_repo_after_a_commit_and_skips_an_unchanged_one() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);

    let ctx = rig.ctx(rig.opts());
    let mut p = Poller::new(&ctx).unwrap();
    let t0 = Instant::now();

    // First sight of a registered repo: its cold scan.
    let o = p.step(t0).expect("first step polls");
    assert_eq!(o.repos, 1);
    assert_eq!(o.scanned.len(), 1, "cold scan on first pass");
    assert_eq!(rig.hits("alpha"), 1);

    // Nothing changed: the poll runs, scan_repo does not.
    let o = p.step(t0 + Duration::from_secs(61)).expect("polls again");
    assert_eq!(o.repos, 1);
    assert!(o.scanned.is_empty(), "unchanged repo is not rescanned");
    let polled = rig.conn().with_connection(store::list_repos).unwrap()[0].last_poll_at;
    assert!(polled.is_some(), "last_poll_at is recorded on every poll");

    // Not due yet: nothing runs.
    assert!(p.step(t0 + Duration::from_secs(70)).is_none());

    // A commit moves HEAD and the index: the next poll rescans.
    write(&repo, "b.py", &py(&["beta_new"]));
    rig.fx.commit_all(&repo, "two");
    let o = p.step(t0 + Duration::from_secs(125)).unwrap();
    assert_eq!(o.scanned.len(), 1);
    assert_eq!(rig.hits("beta_new"), 1);
}

#[test]
fn a_restarted_poller_rescans_each_repo_once_to_catch_edits_made_while_nothing_watched() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let t0 = Instant::now();
    {
        let ctx = rig.ctx(rig.opts());
        let mut p = Poller::new(&ctx).unwrap();
        assert_eq!(p.step(t0).unwrap().scanned.len(), 1);
    }
    // A dirty edit with the daemon down: HEAD and the index do not move.
    write(&repo, "a.py", &py(&["alpha", "while_down"]));
    let ctx = rig.ctx(rig.opts());
    let mut p = Poller::new(&ctx).unwrap();
    assert_eq!(
        p.step(t0).unwrap().scanned.len(),
        1,
        "first poll after start"
    );
    assert_eq!(rig.hits("while_down"), 1);
    assert!(
        p.step(t0 + Duration::from_secs(61))
            .unwrap()
            .scanned
            .is_empty(),
        "later polls trust the probe"
    );
}

#[test]
fn poll_sees_a_worktree_added_and_removed() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let ctx = rig.ctx(rig.opts());
    let mut p = Poller::new(&ctx).unwrap();
    let t0 = Instant::now();
    p.step(t0).unwrap();
    let views = |rig: &Rig| {
        rig.conn()
            .with_connection(|c| store::stored_views(c, None))
            .unwrap()
            .len()
    };
    assert_eq!(views(&rig), 1);

    let wt = rig.fx.add_worktree(&repo, &rig.fx.root.join("wt1"), "feat");
    let o = p.step(t0 + Duration::from_secs(61)).unwrap();
    assert_eq!(o.views_added, vec![wt.to_string_lossy().to_string()]);
    assert_eq!(views(&rig), 2);

    rig.fx.git(
        &repo,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    let o = p.step(t0 + Duration::from_secs(122)).unwrap();
    assert_eq!(o.views_removed, vec![wt.to_string_lossy().to_string()]);
    assert_eq!(views(&rig), 1);
}

#[test]
fn discovery_runs_on_its_cadence_not_every_poll() {
    let rig = Rig::new();
    let mut opts = rig.opts();
    opts.poll = Duration::from_secs(60);
    opts.discovery = Duration::from_secs(600);
    let ctx = rig.ctx(opts);
    let mut p = Poller::new(&ctx).unwrap();
    let t0 = Instant::now();
    for i in 0..10u64 {
        p.step(t0 + Duration::from_secs(i * 60));
    }
    assert_eq!(p.polls, 10, "ten polls in ten minutes");
    assert_eq!(p.discoveries, 1, "discovery is not run on every poll");
    p.step(t0 + Duration::from_secs(10 * 60 + 1));
    assert_eq!(p.discoveries, 2, "and runs again after its interval");
}

#[test]
fn repos_beyond_the_cap_are_not_polled() {
    // The selection truncates to CSR_CODEMAP_MAX_REPOS (default 16).
    let conn = crate::codemap::testutil::db();
    for i in 0..20 {
        store::upsert_repo(&conn, &format!("/x/r{i}/.git"), None, "cli", true).unwrap();
    }
    let sel = select_repos(&conn, Path::new("/home/x"), 1_800_000_000, 24, 16).unwrap();
    assert_eq!(sel.active.len(), 16);
    assert_eq!(sel.over_cap.len(), 4);
}

fn counts(rig: &Rig, sql: &str) -> i64 {
    rig.conn()
        .with_connection(|c| Ok(c.query_row(sql, [], |r| r.get::<_, i64>(0))?))
        .unwrap()
}

#[test]
fn an_idle_repo_is_not_polled_or_watched_keeps_its_rows_and_comes_back_warm() {
    let rig = Rig::new();
    let a = rig.fx.init_repo("a");
    write(&a, "a.py", &py(&["alpha_a"]));
    rig.fx.commit_all(&a, "one");
    let b = rig.fx.init_repo("b");
    write(&b, "b.py", &py(&["beta_b"]));
    rig.fx.commit_all(&b, "one");
    rig.signalled(&a, 3600);
    rig.signalled(&b, 3600);
    let ctx = rig.ctx(rig.opts());
    let mut poller = Poller::new(&ctx).unwrap();
    let mut pump = Pump::new(&ctx).unwrap();

    let o = poller.poll_once(Instant::now());
    assert_eq!((o.repos, o.idle, o.scanned.len()), (2, 0, 2), "{o:?}");
    pump.tick(Instant::now());
    assert_eq!(pump.watched.len(), 2);
    assert_eq!((rig.hits("alpha_a"), rig.hits("beta_b")), (1, 1));
    let blobs = counts(&rig, "SELECT COUNT(*) FROM codemap_blobs");
    let defs = counts(&rig, "SELECT COUNT(*) FROM codemap_defs");
    let views = counts(&rig, "SELECT COUNT(*) FROM codemap_views");

    // b's last signal is now two days old: idle. An edit made meanwhile is not seen.
    rig.set_signal_age(&b, 48 * 3600);
    write(&b, "b.py", &py(&["beta_b", "gamma_b"]));
    let o = poller.poll_once(Instant::now());
    assert_eq!((o.repos, o.idle, o.scanned.len()), (1, 1, 0), "{o:?}");
    pump.tick(Instant::now());
    assert_eq!(pump.watched.len(), 1, "the idle repo's root is unwatched");
    assert!(pump.watched.iter().all(|p| !p.starts_with(&b)));
    assert_eq!(rig.hits("gamma_b"), 0, "an idle repo is not rescanned");
    assert_eq!(rig.hits("beta_b"), 1, "its defs stay answerable");
    assert_eq!(counts(&rig, "SELECT COUNT(*) FROM codemap_blobs"), blobs);
    assert_eq!(counts(&rig, "SELECT COUNT(*) FROM codemap_defs"), defs);
    assert_eq!(counts(&rig, "SELECT COUNT(*) FROM codemap_views"), views);
    let o = poller.poll_once(Instant::now());
    assert_eq!((o.repos, o.idle, o.scanned.len()), (1, 1, 0), "stays idle");

    // A new signal brings it back: one warm scan sees the edit, the watcher follows.
    rig.set_signal_age(&b, 5);
    let o = poller.poll_once(Instant::now());
    assert_eq!((o.repos, o.idle), (2, 0), "{o:?}");
    assert_eq!(
        o.scanned,
        vec![b.join(".git").to_string_lossy().to_string()]
    );
    assert_eq!(rig.hits("gamma_b"), 1);
    pump.tick(Instant::now());
    assert_eq!(pump.watched.len(), 2);
}

#[test]
fn a_repo_named_by_cli_is_never_idle_and_legacy_rows_are_idle_without_a_signal() {
    let rig = Rig::new();
    let pinned = rig.fx.init_repo("pinned");
    write(&pinned, "p.py", &py(&["pin_fn"]));
    rig.fx.commit_all(&pinned, "one");
    rig.register(&pinned);
    let legacy = rig.fx.init_repo("legacy");
    write(&legacy, "l.py", &py(&["legacy_fn"]));
    rig.fx.commit_all(&legacy, "one");
    rig.conn()
        .with_connection(|c| {
            store::upsert_repo(
                c,
                &legacy.join(".git").to_string_lossy(),
                Some(&legacy.to_string_lossy()),
                "history",
                true,
            )
        })
        .unwrap();
    let ctx = rig.ctx(rig.opts());
    let mut poller = Poller::new(&ctx).unwrap();
    let o = poller.poll_once(Instant::now());
    assert_eq!((o.repos, o.idle, o.scanned.len()), (1, 1, 1), "{o:?}");
    assert_eq!((rig.hits("pin_fn"), rig.hits("legacy_fn")), (1, 0));
}

#[test]
fn a_cwd_seen_for_the_first_time_runs_discovery_ahead_of_its_interval() {
    let rig = Rig::new();
    let dir = rig.fx.root.join("plain");
    std::fs::create_dir_all(&dir).unwrap();
    rig.conn()
        .with_connection(|c| {
            store::signal_upsert(
                c,
                &dir.to_string_lossy(),
                "session_start",
                crate::codemap::now_secs() - 10,
            )
        })
        .unwrap();
    let mut opts = rig.opts();
    opts.discovery = Duration::from_secs(3600);
    let ctx = rig.ctx(opts);
    let mut poller = Poller::new(&ctx).unwrap();
    let t0 = Instant::now();
    poller.poll_once(t0);
    assert_eq!(
        poller.discoveries, 1,
        "an unresolved signal triggers discovery"
    );
    let status = rig
        .conn()
        .with_connection(|c| store::list_signals(c, 0))
        .unwrap();
    assert!(status[0].status.is_some(), "and it is resolved: {status:?}");
    poller.poll_once(t0 + Duration::from_secs(60));
    assert_eq!(poller.discoveries, 1, "a resolved signal does not");
}

#[test]
fn lexical_filter_drops_build_output_and_git_and_protected_paths() {
    let roots = vec![
        PathBuf::from("/w/repo"),
        PathBuf::from("/w/repo/.claude/worktrees/x"),
    ];
    let ok = |p: &str| event_path_relevant(Path::new(p), &roots, None);
    assert!(ok("/w/repo/src/a.rs"));
    assert!(ok("/w/repo/.claude/worktrees/x/src/b.py"));
    assert!(!ok("/w/repo/target/debug/build/x/out.rs"));
    assert!(!ok("/w/repo/web/node_modules/pkg/index.js"));
    assert!(!ok("/w/repo/.git/hooks/pre-commit.py"));
    assert!(!ok("/w/repo/.claude/worktrees/x/target/gen.rs"));
    assert!(!ok("/w/repo/README.md"));
    assert!(!ok("/elsewhere/a.rs"));
    let tcc = vec![PathBuf::from("/Users/nobody/Desktop/p")];
    assert!(!event_path_relevant(
        Path::new("/Users/nobody/Desktop/p/a.rs"),
        &tcc,
        None
    ));
}

#[test]
fn lexical_filter_forwards_extensionless_paths_that_may_be_directories() {
    let roots = vec![PathBuf::from("/w/repo")];
    let ok = |p: &str| event_path_relevant(Path::new(p), &roots, None);
    // Directories (and files like Makefile): forwarded, the pump decides.
    assert!(ok("/w/repo/src"));
    assert!(ok("/w/repo/src/deep/lib"));
    assert!(ok("/w/repo/Makefile"));
    assert!(ok("/w/repo/.plans"));
    // Every segment, the last included, clears the skip lists.
    assert!(!ok("/w/repo/target"));
    assert!(!ok("/w/repo/web/node_modules"));
    assert!(!ok("/w/repo/target/debug/build"));
    assert!(!ok("/w/repo/.git"));
    assert!(!ok("/w/repo/.git/objects/ab"));
    assert!(!ok("/w/repo/.venv"));
    assert!(!ok("/w/repo/.cache/x"));
    // The root itself, paths outside any root, and files with some other
    // extension are not directory candidates.
    assert!(!ok("/w/repo"));
    assert!(!ok("/elsewhere/src"));
    assert!(!ok("/w/repo/README.md"));
    assert!(!ok("/w/repo/src/notes.txt"));
    // Protected folders stay out, whatever the shape.
    let tcc = vec![PathBuf::from("/Users/nobody/Desktop/p")];
    assert!(!event_path_relevant(
        Path::new("/Users/nobody/Desktop/p/src"),
        &tcc,
        None
    ));
}

fn stored_paths(rig: &Rig) -> Vec<String> {
    rig.conn()
        .with_connection(|c| {
            let view = store::stored_views(c, None)?.remove(0);
            Ok(store::stored_files(c, view.id)?
                .into_iter()
                .map(|f| f.path)
                .collect())
        })
        .unwrap()
}

#[test]
fn renaming_a_directory_replaces_its_rows() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "src/a.py", &py(&["alpha_fn"]));
    write(&repo, "src/b.py", &py(&["beta_fn"]));
    write(&repo, "keep.py", &py(&["keep_fn"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let mut opts = rig.opts();
    opts.watch = false;
    let ctx = rig.ctx(opts);
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    let before = vec!["keep.py", "src/a.py", "src/b.py"];
    assert_eq!(stored_paths(&rig), before);

    // No git involved: `mv src lib`.
    std::fs::rename(repo.join("src"), repo.join("lib")).unwrap();
    let mut pump = Pump::new(&ctx).unwrap();

    // A vim-style `4913` probe file and a Makefile: one stat and one range
    // query each, nothing else, and nothing changes.
    write(&repo, "4913", "x");
    write(&repo, "Makefile", "all:\n");
    pump.add_pending(repo.join("4913"), Instant::now());
    pump.add_pending(repo.join("Makefile"), Instant::now());
    pump.flush();
    assert_eq!(stored_paths(&rig), before, "plain files change nothing");

    // The two directory events of the rename: the old and the new path.
    pump.add_pending(repo.join("src"), Instant::now());
    pump.add_pending(repo.join("lib"), Instant::now());
    pump.flush();
    assert_eq!(stored_paths(&rig), vec!["keep.py", "lib/a.py", "lib/b.py"]);
    let hit_paths = |name: &str| -> Vec<String> {
        rig.conn()
            .with_connection(|c| query::query(c, name, 20))
            .unwrap()
            .into_iter()
            .map(|h| h.path)
            .collect()
    };
    assert_eq!(hit_paths("alpha_fn"), vec!["lib/a.py"]);
    assert_eq!(hit_paths("beta_fn"), vec!["lib/b.py"]);
    let fts: i64 = rig
        .conn()
        .with_connection(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM codemap_fts WHERE path LIKE 'src/%'",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(fts, 0, "no search row names the old location");

    // A directory removed outright drops its rows too.
    std::fs::remove_dir_all(repo.join("lib")).unwrap();
    pump.add_pending(repo.join("lib"), Instant::now());
    pump.flush();
    assert_eq!(stored_paths(&rig), vec!["keep.py"]);
}

#[test]
fn a_directory_event_only_touches_paths_below_it() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    for p in [
        "src/a.py",
        "src.py",
        "src2/x.py",
        "src-x/y.py",
        "target/gen.py",
    ] {
        write(&repo, p, &py(&["f"]));
    }
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let ctx = rig.ctx(rig.opts());
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    let conn = rig.conn();
    let (view_id, root) = conn
        .with_connection(|c| {
            let v = store::stored_views(c, None)?.remove(0);
            Ok((v.id, PathBuf::from(v.worktree_path)))
        })
        .unwrap();
    let under = conn
        .with_connection(|c| store::stored_paths_under(c, view_id, "src"))
        .unwrap();
    assert_eq!(
        under,
        vec!["src/a.py"],
        "src.py, src2/ and src-x/ are not below src"
    );

    // `src` expands to its stored file (the same file is on disk, so the
    // walk adds nothing new). A hard-skip `target`, a path outside every
    // view and a missing name inside the view expand to nothing; the missing
    // name is looked at (one stat) but is neither stored nor a directory.
    let exp = conn
        .with_connection(|c| {
            expand_dir_events(
                c,
                &[
                    root.join("src"),
                    root.join("target"),
                    root.join("no-such-name"),
                    PathBuf::from("/nowhere/at/all"),
                ],
            )
        })
        .unwrap();
    assert_eq!(exp.paths, vec![root.join("src/a.py")]);
    assert_eq!(
        exp.dir_events, 2,
        "src and no-such-name; target and outside are not"
    );
}

#[test]
fn cover_keeps_only_outermost_roots() {
    let roots = vec![
        PathBuf::from("/w/repo"),
        PathBuf::from("/w/repo/.claude/worktrees/x"),
        PathBuf::from("/w/other"),
    ];
    let c = cover(&roots);
    assert_eq!(c.len(), 2);
    assert!(c.contains(Path::new("/w/repo")) && c.contains(Path::new("/w/other")));
}

#[test]
fn watcher_batch_rescans_an_edited_file_and_ignores_build_output() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let ctx = rig.ctx(rig.opts());
    let mut p = Poller::new(&ctx).unwrap();
    p.step(Instant::now()).unwrap();
    let mut pump = Pump::new(&ctx).unwrap();
    pump.tick(Instant::now());
    assert_eq!(pump.watched.len(), 1, "the one view root is watched");

    // Build output and dependency trees: never reach a batch.
    for i in 0..50 {
        write(&repo, &format!("target/debug/gen{i}.rs"), "fn gen() {}\n");
        write(
            &repo,
            &format!("node_modules/p/f{i}.js"),
            "function g(){}\n",
        );
    }
    // A real edit.
    write(&repo, "a.py", &py(&["alpha", "gamma_dirty"]));

    let ok = wait_until(15, || {
        pump.tick(Instant::now());
        rig.hits("gamma_dirty") == 1
    });
    assert!(ok, "the edited file is rescanned by the watcher batch");
    assert!(pump.flushes >= 1);
    assert_eq!(
        pump.flushed_paths, 1,
        "only a.py reached rescan_paths; target/ and node_modules/ did not"
    );
}

#[test]
fn target_churn_alone_never_flushes() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let ctx = rig.ctx(rig.opts());
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    let mut pump = Pump::new(&ctx).unwrap();
    pump.tick(Instant::now());
    for i in 0..300 {
        write(
            &repo,
            &format!("target/release/deps/x{i}.rs"),
            "fn x() {}\n",
        );
    }
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        pump.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(pump.flushes, 0);
    assert!(pump.pending.is_empty());
}

#[test]
fn queue_drain_keeps_a_row_enqueued_during_the_drain() {
    let conn = crate::codemap::testutil::db();
    let a = Path::new("/w/r/a.py");
    let b = Path::new("/w/r/b.py");
    crate::codemap::enqueue(&conn, a).unwrap();
    crate::codemap::enqueue(&conn, b).unwrap();
    let (n, _) = drain_queue(&conn, None, |paths| {
        assert_eq!(paths.len(), 2);
        // The hook fires again for a.py while the rescan runs.
        crate::codemap::enqueue(&conn, a).unwrap();
        Ok(PathScanReport::default())
    })
    .unwrap()
    .unwrap();
    assert_eq!(n, 2);
    let left = store::queue_read(&conn).unwrap();
    assert_eq!(left.len(), 1, "only the re-enqueued row survives");
    assert_eq!(left[0].0, "/w/r/a.py");
    // The next drain takes it.
    drain_queue(&conn, None, |_| Ok(PathScanReport::default()))
        .unwrap()
        .unwrap();
    assert!(store::queue_read(&conn).unwrap().is_empty());
}

#[test]
fn the_daemon_queue_drain_leaves_protected_rows_in_place_and_drops_old_ones() {
    let conn = crate::codemap::testutil::db();
    let home = Path::new("/Users/nobody-here");
    let protected = "/Users/nobody-here/Documents/probe/a.rs";
    let ordinary = "/w/r/a.rs";
    crate::codemap::enqueue(&conn, Path::new(protected)).unwrap();
    crate::codemap::enqueue(&conn, Path::new(ordinary)).unwrap();
    let mut seen = Vec::new();
    let (n, _) = drain_queue(&conn, Some(home), |paths| {
        seen = paths.to_vec();
        Ok(PathScanReport::default())
    })
    .unwrap()
    .unwrap();
    assert_eq!((n, seen), (1, vec![PathBuf::from(ordinary)]));
    let left = store::queue_read(&conn).unwrap();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(
        left[0].0, protected,
        "the protected row waits for its session"
    );
    // Only protected rows left: nothing to rescan, the rescan is not called.
    let none = drain_queue(&conn, Some(home), |_| panic!("rescan must not run")).unwrap();
    assert!(none.is_none());
    assert_eq!(store::queue_read(&conn).unwrap().len(), 1);
    // A protected row older than an hour is dropped, like any stale row.
    conn.execute_batch("DELETE FROM codemap_queue").unwrap();
    store::queue_push(
        &conn,
        protected,
        crate::codemap::now_nanos() - 2 * QUEUE_STALE_NANOS,
    )
    .unwrap();
    drain_queue(&conn, Some(home), |_| panic!("rescan must not run")).unwrap();
    assert!(store::queue_read(&conn).unwrap().is_empty());
}

#[test]
fn a_failed_rescan_keeps_the_queue_rows() {
    let conn = crate::codemap::testutil::db();
    crate::codemap::enqueue(&conn, Path::new("/w/r/a.py")).unwrap();
    let r = drain_queue(&conn, None, |_| Err(anyhow::anyhow!("boom")));
    assert!(r.is_err());
    assert_eq!(store::queue_read(&conn).unwrap().len(), 1);
}

#[test]
fn enqueue_twice_strictly_raises_the_stamp() {
    let conn = crate::codemap::testutil::db();
    let p = Path::new("/w/r/a.py");
    store::queue_push(&conn, &p.to_string_lossy(), 100).unwrap();
    store::queue_push(&conn, &p.to_string_lossy(), 100).unwrap();
    let rows = store::queue_read(&conn).unwrap();
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].1 > 100,
        "an equal clock reading still raises the stamp"
    );
}

#[test]
fn queue_rows_are_rescanned_by_the_pump() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let mut opts = rig.opts();
    opts.watch = false;
    let ctx = rig.ctx(opts);
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    let mut pump = Pump::new(&ctx).unwrap();
    write(&repo, "a.py", &py(&["alpha", "queued_fn"]));
    rig.conn()
        .with_connection(|c| crate::codemap::enqueue(c, &repo.join("a.py")))
        .unwrap();
    pump.tick(Instant::now());
    assert_eq!(rig.hits("queued_fn"), 1);
    assert!(rig
        .conn()
        .with_connection(store::queue_read)
        .unwrap()
        .is_empty());
}

#[test]
fn protected_roots_are_neither_watched_nor_polled() {
    let rig = Rig::new();
    let real = rig.fx.init_repo("r");
    write(&real, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&real, "one");
    rig.register(&real);
    // A repo and a view under ~/Desktop, planted straight into the tables.
    let tcc_root = "/Users/nobody-here/Desktop/proj";
    rig.conn()
        .with_connection(|c| {
            store::upsert_repo(c, &format!("{tcc_root}/.git"), Some(tcc_root), "cli", true)?;
            c.execute(
                "INSERT INTO codemap_views (common_dir, worktree_path, status, files, truncated)
                     VALUES (?1, ?2, 'ok', 0, 0)",
                rusqlite::params![format!("{tcc_root}/.git"), tcc_root],
            )?;
            Ok(())
        })
        .unwrap();
    let ctx = rig.ctx(rig.opts());
    let mut poller = Poller::new(&ctx).unwrap();
    let o = poller.step(Instant::now()).unwrap();
    assert_eq!(o.repos, 1, "the protected repo is not even selected");
    assert_eq!(o.scanned.len(), 1, "the protected repo is not scanned");
    let mut pump = Pump::new(&ctx).unwrap();
    pump.tick(Instant::now());
    assert_eq!(pump.watched.len(), 1);
    assert!(pump.watched.iter().all(|p| !p.starts_with(tcc_root)));
    assert!(pump
        .view_roots
        .read()
        .unwrap()
        .iter()
        .all(|p| !p.starts_with(tcc_root)));
}

fn repo_with_alpha(rig: &Rig) -> PathBuf {
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    repo
}

#[test]
fn a_pump_flush_waits_for_the_repo_lock_and_then_writes() {
    let rig = Rig::new();
    let repo = repo_with_alpha(&rig);
    let mut opts = rig.opts();
    opts.watch = false;
    let ctx = rig.ctx(opts);
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    write(&repo, "a.py", &py(&["alpha", "newest_fn"]));

    let guard = take(&ctx, &repo.join(".git").to_string_lossy());
    std::thread::scope(|s| {
        let flusher = s.spawn(|| {
            let mut pump = Pump::new(&ctx).unwrap();
            pump.add_pending(repo.join("a.py"), Instant::now());
            pump.flush();
        });
        std::thread::sleep(Duration::from_millis(600));
        assert!(!flusher.is_finished(), "the flush waits for the lock");
        assert_eq!(rig.hits("newest_fn"), 0, "and writes nothing meanwhile");
        drop(guard);
        flusher.join().unwrap();
    });
    assert_eq!(rig.hits("newest_fn"), 1, "it writes once the lock is free");
}

#[test]
fn a_full_scan_cannot_overwrite_a_newer_partial_rescan() {
    let rig = Rig::new();
    let repo = repo_with_alpha(&rig);
    let mut opts = rig.opts();
    opts.watch = false;
    let ctx = rig.ctx(opts);
    let t0 = Instant::now();
    Poller::new(&ctx).unwrap().step(t0).unwrap();
    // Blob A: a dirty edit that neither HEAD nor the index will ever show.
    write(&repo, "a.py", &py(&["alpha", "zeta_older"]));

    let (collected_tx, collected_rx) = mpsc::channel::<()>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    std::thread::scope(|s| {
        // A fresh poller runs a full scan. The hook parks it after it has
        // read blob A and before it writes.
        let scanner = s.spawn(|| {
            let mut p = Poller::new(&ctx).unwrap();
            crate::codemap::scan::test_hooks::set_after_collect(move || {
                collected_tx.send(()).unwrap();
                // Bounded: a failing run must end, not hang the scope.
                let _ = go_rx.recv_timeout(Duration::from_secs(20));
            });
            let o = p.step(t0 + Duration::from_secs(61)).unwrap();
            assert_eq!(o.scanned.len(), 1, "the full scan ran");
        });
        collected_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the scan reached the hook");
        // Blob B lands, and the pump is asked to rescan the path.
        write(&repo, "a.py", &py(&["alpha", "omega_newer"]));
        let flusher = s.spawn(|| {
            let mut pump = Pump::new(&ctx).unwrap();
            pump.add_pending(repo.join("a.py"), Instant::now());
            pump.flush();
        });
        std::thread::sleep(Duration::from_millis(500));
        let waited = !flusher.is_finished();
        go_tx.send(()).unwrap();
        scanner.join().unwrap();
        flusher.join().unwrap();
        assert!(
            waited,
            "the pump rescan waits while the scan holds the lock"
        );
    });
    assert_eq!(
        rig.hits("omega_newer"),
        1,
        "the newest blob is what the map holds"
    );
    assert_eq!(
        rig.hits("zeta_older"),
        0,
        "the older scan did not overwrite it"
    );
}

#[test]
fn the_pump_loop_does_not_spin_when_the_watcher_is_off() {
    let rig = Rig::new();
    let mut opts = rig.opts();
    opts.watch = false;
    opts.tick = Duration::from_millis(100);
    let ctx = rig.ctx(opts);
    let mut pump = Pump::new(&ctx).unwrap();
    assert!(pump.watcher.is_none(), "no watcher: the Sender is dropped");
    let stop = rig.shutdown.clone();
    std::thread::scope(|s| {
        s.spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            stop.store(true, Ordering::SeqCst);
        });
        pump_loop(&ctx, &mut pump);
    });
    // 600 ms at one tick per 100 ms is about 6; a spinning loop makes
    // thousands. The floor is 1, not 3: a loaded machine can starve the
    // loop, and a low count says nothing against a spin.
    assert!(
        (1..=10).contains(&pump.ticks),
        "pump ticked {} times in 600 ms",
        pump.ticks
    );
}

fn take(ctx: &Ctx<'_>, common: &str) -> crate::codemap::lock::RepoLock {
    match ctx.lock_repo(common).unwrap() {
        crate::codemap::lock::Acquired::Held(l) => l,
        other => panic!("repo lock not free: {other:?}"),
    }
}

#[test]
fn a_shutdown_ends_the_wait_for_a_repo_lock_and_a_busy_one_times_out() {
    let rig = Rig::new();
    let mut opts = rig.opts();
    opts.lock_wait = Duration::from_millis(300);
    let ctx = rig.ctx(opts);
    let held = take(&ctx, "/w/r/.git");
    let t0 = Instant::now();
    assert!(matches!(
        ctx.lock_repo("/w/r/.git").unwrap(),
        crate::codemap::lock::Acquired::Busy
    ));
    assert!(t0.elapsed() >= Duration::from_millis(250));

    let mut opts = rig.opts();
    opts.lock_wait = Duration::from_secs(30);
    let ctx = rig.ctx(opts);
    rig.shutdown.store(true, Ordering::SeqCst);
    let t0 = Instant::now();
    assert!(matches!(
        ctx.lock_repo("/w/r/.git").unwrap(),
        crate::codemap::lock::Acquired::Cancelled
    ));
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "shutdown ends the wait"
    );
    drop(held);
}

#[test]
fn a_poll_skips_a_repo_another_scanner_holds_and_scans_it_on_the_next_poll() {
    let rig = Rig::new();
    let repo = repo_with_alpha(&rig);
    let mut opts = rig.opts();
    opts.watch = false;
    opts.lock_wait = Duration::from_millis(200);
    let lines: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let log = |m: &str| lines.lock().unwrap().push(m.to_string());
    let ctx = Ctx::new(
        &rig.db,
        &rig.fx.home,
        rig.shutdown.clone(),
        opts,
        None,
        &log,
    );
    let common = repo.join(".git").to_string_lossy().to_string();
    let t0 = Instant::now();
    let mut p = Poller::new(&ctx).unwrap();

    let held = take(&ctx, &common);
    let o = p.step(t0).unwrap();
    assert_eq!(o.skipped_locked, 1);
    assert!(o.scanned.is_empty());
    assert_eq!(o.errors, 0, "a busy repo is not an error");
    assert_eq!(rig.hits("alpha"), 0, "nothing was written");
    let want = format!("scan skipped: {common} locked by another scanner");
    assert!(
        lines.lock().unwrap().contains(&want),
        "{:?}",
        lines.lock().unwrap()
    );

    drop(held);
    let o = p.step(t0 + Duration::from_secs(61)).unwrap();
    assert_eq!(o.scanned.len(), 1, "no backoff: scanned on the next poll");
    assert_eq!(rig.hits("alpha"), 1);
}

#[test]
fn a_path_batch_a_busy_repo_refused_is_kept_for_the_next_flush() {
    let rig = Rig::new();
    let repo = repo_with_alpha(&rig);
    let mut opts = rig.opts();
    opts.watch = false;
    opts.lock_wait = Duration::from_millis(200);
    let ctx = rig.ctx(opts);
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    let common = repo.join(".git").to_string_lossy().to_string();
    write(&repo, "a.py", &py(&["alpha", "edit_while_locked"]));
    let mut pump = Pump::new(&ctx).unwrap();
    pump.add_pending(repo.join("a.py"), Instant::now());

    let held = take(&ctx, &common);
    pump.flush();
    assert_eq!(
        rig.hits("edit_while_locked"),
        0,
        "nothing written meanwhile"
    );
    assert!(
        pump.pending.contains(&repo.join("a.py")),
        "the edit is kept"
    );

    drop(held);
    pump.flush();
    assert_eq!(rig.hits("edit_while_locked"), 1, "and lands on the retry");
    assert!(pump.pending.is_empty());
}

#[test]
fn queue_rows_stay_when_the_repo_is_locked_and_are_taken_after() {
    let rig = Rig::new();
    let repo = repo_with_alpha(&rig);
    let mut opts = rig.opts();
    opts.watch = false;
    opts.lock_wait = Duration::from_millis(200);
    let ctx = rig.ctx(opts);
    Poller::new(&ctx).unwrap().step(Instant::now()).unwrap();
    let common = repo.join(".git").to_string_lossy().to_string();
    write(&repo, "a.py", &py(&["alpha", "queued_while_locked"]));
    rig.conn()
        .with_connection(|c| crate::codemap::enqueue(c, &repo.join("a.py")))
        .unwrap();
    let mut pump = Pump::new(&ctx).unwrap();
    let held = take(&ctx, &common);
    pump.drain_queue_now();
    assert_eq!(rig.hits("queued_while_locked"), 0);
    assert_eq!(
        rig.conn().with_connection(store::queue_read).unwrap().len(),
        1,
        "the row stays for the next drain"
    );
    drop(held);
    pump.drain_queue_now();
    assert_eq!(rig.hits("queued_while_locked"), 1);
    assert!(rig
        .conn()
        .with_connection(store::queue_read)
        .unwrap()
        .is_empty());
}

#[test]
fn the_kill_switch_ends_the_loop_at_once() {
    let rig = Rig::new();
    let lines: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let t0 = Instant::now();
    run_loop_inner(
        &rig.db_path(),
        &rig.fx.home,
        rig.shutdown.clone(), // never set: only the switch can end this
        rig.opts(),
        None,
        &|m| lines.lock().unwrap().push(m.to_string()),
        true,
    );
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert_eq!(lines.lock().unwrap().len(), 1);
    assert!(lines.lock().unwrap()[0].contains("CSR_NO_CODEMAP"));
    assert!(!rig.db_path().exists(), "disabled loop opens no database");
}

#[test]
fn the_loop_follows_a_dirty_edit_and_a_commit_and_stops_on_shutdown() {
    let rig = Rig::new();
    let repo = rig.fx.init_repo("r");
    write(&repo, "a.py", &py(&["alpha"]));
    rig.fx.commit_all(&repo, "one");
    rig.register(&repo);
    let mut opts = rig.opts();
    opts.poll = Duration::from_secs(1);
    let db = rig.db_path();
    let home = rig.fx.home.clone();
    let shutdown = rig.shutdown.clone();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = lines.clone();
    let handle = std::thread::spawn(move || {
        run_loop(&db, &home, shutdown, opts, None, move |m| {
            sink.lock().unwrap().push(m.to_string())
        });
    });
    assert!(
        wait_until(20, || rig.hits("alpha") == 1),
        "cold scan on the first pass"
    );
    // The watcher needs a tick to watch a view the cold scan just stored.
    assert!(
        wait_until(20, || lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("watch +"))),
        "the new view's root is watched"
    );
    std::thread::sleep(Duration::from_millis(500));
    // Dirty edit, no commit: the watcher.
    write(&repo, "a.py", &py(&["alpha", "dirty_fn"]));
    assert!(wait_until(20, || rig.hits("dirty_fn") == 1), "watcher");
    // Commit a new file: the poll (and the watcher).
    write(&repo, "c.py", &py(&["committed_fn"]));
    rig.fx.commit_all(&repo, "two");
    assert!(wait_until(20, || rig.hits("committed_fn") == 1), "poll");
    let t0 = Instant::now();
    rig.shutdown.store(true, Ordering::SeqCst);
    handle.join().unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "stops within a tick or two"
    );
}
