//! What the engine knows about each Claude Code session it has heard from.
//!
//! A session is opened by the first request that names its `sid`: `session.start` is not the only
//! way in, because `/clear` changes the sid with no `session.start`. One that has been quiet for
//! `IDLE_MS` is forgotten; the store keeps whatever must outlive it.

use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

/// A session unheard from for this long is dropped (6 hours, in milliseconds).
pub const IDLE_MS: f64 = 6. * 60. * 60. * 1000.;

/// How many gate results a session keeps for their `/after`; the oldest go first.
const LEFT_KEPT: usize = 64;

pub struct Session {
    pub sid: String,
    pub cwd: String,
    /// From `session.start`; unknown until one arrives.
    pub interactive: Option<bool>,
    pub first_seen: f64,
    pub last_seen: f64,
    /// What a gate's `/event` left for its `/after`, by conversation id.
    left: VecDeque<(String, Value)>,
}

impl Session {
    /// Keep `value` for the `/after` that names `id`.
    pub fn leave(&mut self, id: &str, value: Value) {
        if self.left.len() >= LEFT_KEPT {
            self.left.pop_front();
        }
        self.left.push_back((id.to_string(), value));
    }

    /// What `/event` left under `id`, taken: an `/after` comes once.
    pub fn take_left(&mut self, id: &str) -> Option<Value> {
        let at = self.left.iter().position(|(key, _)| key == id)?;
        self.left.remove(at).map(|(_, value)| value)
    }
}

pub type SessionRef = Arc<Mutex<Session>>;

#[derive(Default)]
pub struct Sessions {
    by_sid: HashMap<String, SessionRef>,
}

impl Sessions {
    /// The session for `sid`, opened if it is new; `cwd` is kept when it is not empty. Returns
    /// whether this call opened it. Sessions idle past `IDLE_MS` are dropped on the way.
    pub fn touch(&mut self, sid: &str, cwd: &str, now: f64) -> (SessionRef, bool) {
        self.by_sid.retain(|key, session| {
            key == sid || session.lock().is_ok_and(|s| now - s.last_seen <= IDLE_MS)
        });
        let opened = !self.by_sid.contains_key(sid);
        let session = self.by_sid.entry(sid.to_string()).or_insert_with(|| {
            Arc::new(Mutex::new(Session {
                sid: sid.to_string(),
                cwd: String::new(),
                interactive: None,
                first_seen: now,
                last_seen: now,
                left: VecDeque::new(),
            }))
        });
        {
            let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
            session.last_seen = now;
            if !cwd.is_empty() {
                session.cwd = cwd.to_string();
            }
        }
        (session.clone(), opened)
    }

    pub fn get(&self, sid: &str) -> Option<SessionRef> {
        self.by_sid.get(sid).cloned()
    }

    pub fn len(&self) -> usize {
        self.by_sid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_sid.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_unknown_sid_opens_a_session_and_a_known_one_is_the_same_record() {
        let mut sessions = Sessions::default();
        let (first, opened) = sessions.touch("a", "/repo", 100.);
        assert!(opened);
        first.lock().unwrap().interactive = Some(true);
        let (again, opened) = sessions.touch("a", "", 250.);
        assert!(!opened);
        let seen = again.lock().unwrap();
        assert_eq!((seen.first_seen, seen.last_seen), (100., 250.));
        assert_eq!(seen.cwd, "/repo");
        assert_eq!(seen.interactive, Some(true));
    }

    #[test]
    fn a_clear_opens_a_second_session_beside_the_first() {
        let mut sessions = Sessions::default();
        sessions.touch("before-clear", "/repo", 0.);
        let (_, opened) = sessions.touch("after-clear", "/repo", 10.);
        assert!(opened);
        assert_eq!(sessions.len(), 2);
        assert!(sessions.get("before-clear").is_some());
    }

    #[test]
    fn a_session_idle_past_six_hours_is_dropped_and_a_live_one_stays() {
        let mut sessions = Sessions::default();
        sessions.touch("old", "/a", 0.);
        sessions.touch("live", "/b", IDLE_MS - 1.);
        sessions.touch("new", "/c", IDLE_MS);
        assert!(
            sessions.get("old").is_some(),
            "exactly six hours is not past it"
        );
        sessions.touch("new", "/c", IDLE_MS + 1.);
        assert!(sessions.get("old").is_none());
        assert!(sessions.get("live").is_some());
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn an_idle_session_that_calls_again_is_not_dropped_first() {
        let mut sessions = Sessions::default();
        let (first, _) = sessions.touch("a", "/a", 0.);
        first.lock().unwrap().interactive = Some(false);
        let (back, opened) = sessions.touch("a", "/a", IDLE_MS * 2.);
        assert!(!opened);
        assert_eq!(back.lock().unwrap().interactive, Some(false));
    }

    #[test]
    fn what_a_gate_left_comes_back_once_and_the_oldest_go_first() {
        let mut sessions = Sessions::default();
        let (session, _) = sessions.touch("a", "/a", 0.);
        let mut session = session.lock().unwrap();
        session.leave("e1", json!({ "n": 1 }));
        assert_eq!(session.take_left("e1"), Some(json!({ "n": 1 })));
        assert_eq!(session.take_left("e1"), None);
        for i in 0..(LEFT_KEPT + 3) {
            session.leave(&format!("x{i}"), json!(i));
        }
        assert_eq!(session.take_left("x0"), None);
        assert_eq!(session.take_left("x3"), Some(json!(3)));
    }
}
