//! The session before this one, read once and kept (pane.py's `carry_into`): its threads, which a
//! restart would lose, and the facts the default view draws as "Last session". It is read off the
//! draw path in a thread, and kept to `keep/carried/<sid>.json` and `keep/previous/<sid>.json`
//! first, shown second, so the two always agree. A one-shot run (`--frame`, `--png`) has no draw
//! loop to hold up and nothing to keep, so it reads inline.
use super::{
    db::Db,
    lost::{self, Thread},
    paths::{self, Paths},
    session::Session,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// What the carry needs to find the session before this one.
#[derive(Clone)]
pub struct Job {
    pub paths: Paths,
    pub sid: String,
    /// This session's transcript, or where it will be.
    pub path: PathBuf,
    pub started: f64,
    /// Keep the result under `keep/` (the watch loop does; a one-shot frame does not).
    pub persist: bool,
}

/// The carry's result.
#[derive(Default, Clone)]
pub struct Done {
    pub carried: Vec<Thread>,
    /// `previous_facts` of the session before this one; `{}` when there is none.
    pub facts: Value,
}

fn nothing() -> Done {
    Done {
        carried: vec![],
        facts: json!({}),
    }
}

#[derive(Default)]
pub struct Carry {
    started: bool,
    waiting: Option<Receiver<Done>>,
    done: Option<Done>,
}

impl Carry {
    pub fn started(&self) -> bool {
        self.started
    }
    pub fn done(&self) -> Option<&Done> {
        self.done.as_ref()
    }
    /// The threads carried in; none until the carry lands.
    pub fn threads(&self) -> &[Thread] {
        self.done.as_ref().map_or(&[], |d| &d.carried)
    }
    /// Start the carry once. `wait` reads inline (a one-shot run); otherwise a thread does it and
    /// `poll` picks it up.
    pub fn start(&mut self, job: Job, wait: bool) {
        if self.started {
            return;
        }
        if wait {
            self.started = true;
            self.done = Some(work(&job));
        } else {
            self.spawn(move || work(&job));
        }
    }
    /// Run `work` in a thread, once; the draw path never waits for it.
    pub fn spawn<F: FnOnce() -> Done + Send + 'static>(&mut self, work: F) {
        if self.started {
            return;
        }
        self.started = true;
        let (send, receive) = channel();
        self.waiting = Some(receive);
        std::thread::spawn(move || {
            // a carry that panics leaves this session its own threads, as one that fails does
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            let _ = send.send(done.unwrap_or_else(|_| nothing()));
        });
    }
    /// Take the result if the thread has finished; true once, when it lands.
    pub fn poll(&mut self) -> bool {
        let Some(receive) = &self.waiting else {
            return false;
        };
        match receive.try_recv() {
            Ok(done) => {
                self.done = Some(done);
                self.waiting = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.done = Some(nothing());
                self.waiting = None;
                true
            }
        }
    }
}

/// The carry kept for a session, or None when none was kept.
pub fn read_carried(paths: &Paths, sid: &str) -> Option<Vec<Thread>> {
    let value = paths::read_json(&paths.kept_file("carried", sid)?);
    if !value.is_array() {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// The facts kept for a session, or None when none were kept.
pub fn read_previous(paths: &Paths, sid: &str) -> Option<Value> {
    let value = paths::read_json(&paths.kept_file("previous", sid)?);
    value.is_object().then_some(value)
}

fn keep(paths: &Paths, folder: &str, sid: &str, record: &Value) {
    if let Some(path) = paths.kept_file(folder, sid) {
        let _ = super::private::write_atomic(&path, &super::private::dumps(record), &paths.keep);
    }
}

/// The session before this one, its transcript read.
fn previous_read(paths: &Paths, mine: &std::path::Path, started: f64) -> Option<Session> {
    let path = super::sources::previous_session(paths, mine, started)?;
    let id = path.file_stem()?.to_str()?.to_owned();
    let mut prev = Session::new(&id);
    prev.path = Some(path);
    prev.read_transcript(paths);
    Some(prev)
}

/// What is already kept is read back, never worked out again: by now that earlier session may
/// have been resumed and no longer count as the one before.
pub fn work(job: &Job) -> Done {
    let kept = read_carried(&job.paths, &job.sid);
    let facts = read_previous(&job.paths, &job.sid);
    if let (Some(carried), Some(facts)) = (&kept, &facts) {
        return Done {
            carried: carried.clone(),
            facts: facts.clone(),
        };
    }
    let prev = previous_read(&job.paths, &job.path, job.started);
    let carried = kept.unwrap_or_else(|| {
        let carried = prev
            .as_ref()
            .map(|prev| lost::carried_threads(&job.paths, prev))
            .unwrap_or_default();
        if job.persist {
            keep(
                &job.paths,
                "carried",
                &job.sid,
                &serde_json::to_value(&carried).unwrap_or_else(|_| json!([])),
            );
        }
        carried
    });
    let facts = facts.unwrap_or_else(|| {
        let db = Db::open(&job.paths).ok();
        let facts = prev
            .as_ref()
            .map(|prev| super::session::previous_facts(&job.paths, prev, db.as_ref()))
            .unwrap_or_else(|| json!({}));
        if job.persist {
            keep(&job.paths, "previous", &job.sid, &facts);
        }
        facts
    });
    Done { carried, facts }
}

/// The session's threads: its own summaries' and the carried ones, as `s["lost"]` holds them.
pub fn lost_of(paths: &Paths, s: &Value, project: Option<&str>, carried: &[Thread]) -> Value {
    let found = lost::lost_found(
        paths,
        &lost::pairs(&s["summaries"]),
        &lost::pairs(&s["typed"]),
        carried,
        project,
        &super::agents::handoffs_of(paths, super::string(&s["id"])),
    );
    serde_json::to_value(found).unwrap_or_else(|_| json!([]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn test_a_spawned_carry_never_holds_up_the_caller() {
        let (release, wait) = channel::<()>();
        let mut carry = Carry::default();
        carry.spawn(move || {
            let _ = wait.recv();
            Done {
                carried: vec![Thread {
                    text: "a thread".into(),
                    ..Thread::default()
                }],
                facts: json!({"id": "before"}),
            }
        });
        // the work is blocked, and the caller still returns at once, any number of times
        let began = Instant::now();
        for _ in 0..50 {
            assert!(!carry.poll());
            assert!(carry.threads().is_empty());
        }
        assert!(began.elapsed() < Duration::from_secs(1));
        assert!(carry.started());
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while !carry.poll() {
            assert!(Instant::now() < until, "the carry never landed");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(carry.threads()[0].text, "a thread");
        assert_eq!(carry.done().unwrap().facts["id"], "before");
        // once: a second start does not run again
        carry.spawn(|| panic!("ran twice"));
        assert!(!carry.poll());
    }

    #[test]
    fn test_a_carry_that_panics_leaves_the_session_its_own_threads() {
        let mut carry = Carry::default();
        carry.spawn(|| panic!("boom"));
        let until = Instant::now() + Duration::from_secs(10);
        while !carry.poll() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(carry.threads().is_empty());
        assert!(carry.done().unwrap().facts.is_object());
    }
}
