//! The session's usage figures, kept for the pane's masthead.
//!
//! Claude Code fires `session.measure` with the context fill, the rate-limit windows and the cost;
//! the relay forwards it as the hook key `measure`. This handler writes the figures to
//! `<keep>/usage/<sid>.json`, and `sources::kept_status` reads them back. The mod's own field names
//! are kept, and only the fields the event carries.

use super::{
    paths::{valid_id, Paths},
    private,
    relay::dispatch::{handler, Handler, Reply},
};
use anyhow::{bail, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The folder under the pane's keep where the usage files live.
pub const FOLDER: &str = "usage";

/// The fields of a `session.measure` event that the masthead reads.
const KEPT: [&str; 3] = ["context", "rateLimits", "cost"];

/// The `measure` handler: it writes under the real keep, found from `HOME` when the event comes.
pub fn measure() -> Handler {
    handler(|cx, e| async move {
        write(&Paths::from_env().keep, &cx.sid, &e, cx.clock.now() / 1000.)?;
        Ok(Reply::none())
    })
}

/// The same handler writing under `keep`, for a test that must not touch the real home.
pub fn measure_in(keep: PathBuf) -> Handler {
    handler(move |cx, e| {
        let keep = keep.clone();
        async move {
            write(&keep, &cx.sid, &e, cx.clock.now() / 1000.)?;
            Ok(Reply::none())
        }
    })
}

/// What one measurement keeps: the time, and each of `KEPT` that the event has.
pub fn kept(event: &Value, at: f64) -> Value {
    let mut out = Map::new();
    out.insert("at".into(), json!(at));
    for name in KEPT {
        if let Some(value) = event.get(name).filter(|v| !v.is_null()) {
            out.insert(name.into(), value.clone());
        }
    }
    Value::Object(out)
}

/// Write `<keep>/usage/<sid>.json`: private, atomic, and refusing a link anywhere in the path.
pub fn write(keep: &Path, sid: &str, event: &Value, at: f64) -> Result<()> {
    if !valid_id(sid) {
        bail!("usage: refusing a conversation id that is not a plain name");
    }
    let path = keep.join(FOLDER).join(format!("{sid}.json"));
    private::write_atomic(&path, &kept(event, at).to_string(), keep)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The body Claude Code 2.1.303 sent for `session.measure` (WP0).
    pub fn wp0_body() -> Value {
        json!({
            "changed": ["context", "rateLimits", "cost"],
            "context": { "percent": 3, "tokens": 34581, "window": 1000000 },
            "cost": { "usd": 0.00381693 },
            "rateLimits": [
                { "kind": "five_hour", "percentUsed": 11, "resetsAt": "2026-10-19T19:00:00.000Z" },
                { "kind": "seven_day", "percentUsed": 25, "resetsAt": "2026-10-25T14:00:00.000Z" }
            ]
        })
    }

    fn read(keep: &Path, sid: &str) -> Value {
        let bytes = std::fs::read(keep.join(FOLDER).join(format!("{sid}.json"))).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn test_a_measure_event_writes_its_figures_under_the_mods_own_names() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        write(&keep, "sess-1", &wp0_body(), 1792000000.5).unwrap();
        let got = read(&keep, "sess-1");
        assert_eq!(got["at"], 1792000000.5);
        assert_eq!(got["context"]["tokens"], 34581);
        assert_eq!(got["context"]["window"], 1000000);
        assert_eq!(got["cost"]["usd"], 0.00381693);
        assert_eq!(got["rateLimits"][1]["kind"], "seven_day");
        assert_eq!(got["rateLimits"][1]["resetsAt"], "2026-10-25T14:00:00.000Z");
        assert!(got.get("changed").is_none());
    }

    #[test]
    fn test_only_the_fields_the_event_has_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let event = json!({ "changed": ["context"], "context": { "window": 200000 } });
        write(&keep, "s", &event, 5.).unwrap();
        let got = read(&keep, "s");
        assert_eq!(got, json!({ "at": 5., "context": { "window": 200000 } }));
        let empty = json!({ "changed": ["rateLimits"], "rateLimits": [], "cost": null });
        write(&keep, "s", &empty, 6.).unwrap();
        assert_eq!(read(&keep, "s"), json!({ "at": 6., "rateLimits": [] }));
    }

    #[test]
    fn test_a_bad_session_id_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        for sid in ["", "../x", "a/b", "a b", "a.json"] {
            assert!(write(&keep, sid, &wp0_body(), 1.).is_err(), "{sid:?}");
        }
        assert!(!keep.exists());
    }

    #[test]
    fn test_a_linked_usage_folder_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let elsewhere = d.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(&keep).unwrap();
        std::os::unix::fs::symlink(&elsewhere, keep.join(FOLDER)).unwrap();
        assert!(write(&keep, "s", &wp0_body(), 1.).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn test_a_linked_usage_file_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let target = d.path().join("target.json");
        std::fs::write(&target, "mine").unwrap();
        std::fs::create_dir_all(keep.join(FOLDER)).unwrap();
        std::os::unix::fs::symlink(&target, keep.join(FOLDER).join("s.json")).unwrap();
        assert!(write(&keep, "s", &wp0_body(), 1.).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "mine");
    }

    #[test]
    fn test_the_file_is_private_and_so_is_its_folder() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        write(&keep, "s", &wp0_body(), 1.).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&keep.join(FOLDER).join("s.json")), 0o600);
        assert_eq!(mode(&keep.join(FOLDER)), 0o700);
        assert_eq!(mode(&keep), 0o700);
    }

    #[tokio::test]
    async fn test_the_handler_writes_for_its_session_and_replies_with_nothing() {
        use crate::companion::relay::{
            dispatch::{Ctx, Log},
            host::fake::{FakeClock, FakeHost, World},
            session::Sessions,
        };
        use std::sync::Arc;
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        let clock = FakeClock::new(1_792_000_000_500.);
        let cx = Ctx {
            host: Arc::new(FakeHost::new(World::default())),
            session: Sessions::default().touch("sess-h", "/repo", 0.).0,
            store: Arc::new(crate::companion::store::Store::at(d.path().join("s.db"))),
            clock: clock.clone(),
            log: Arc::new(Log::default()),
            sid: "sess-h".into(),
            cwd: "/repo".into(),
            id: String::new(),
            left: None,
            respawn: false,
            pid: 1,
            events: 1,
            timers: crate::companion::relay::timers::fake::FakeTimers::new(clock),
            module: Arc::default(),
        };
        let reply = measure_in(keep.clone())(cx, wp0_body()).await.unwrap();
        assert!(reply.answer.is_none() && reply.next.is_none() && reply.tree.is_none());
        let got = read(&keep, "sess-h");
        assert_eq!(got["at"], 1792000000.5);
        assert_eq!(got["context"]["percent"], 3);
    }

    #[test]
    fn test_a_later_measure_replaces_the_earlier_one_and_leaves_no_temp_file() {
        let d = tempfile::tempdir().unwrap();
        let keep = d.path().join("csr-companion");
        write(&keep, "s", &wp0_body(), 1.).unwrap();
        write(&keep, "s", &json!({ "cost": { "usd": 2.0 } }), 2.).unwrap();
        assert_eq!(
            read(&keep, "s"),
            json!({ "at": 2., "cost": { "usd": 2.0 } })
        );
        let names: Vec<_> = std::fs::read_dir(keep.join(FOLDER))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["s.json".to_string()]);
    }
}
