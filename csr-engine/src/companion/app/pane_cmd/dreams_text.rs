//! What `/csr dreams` says about the dreamer's pacing: `dreamsText` (register.tsx 848-852) over
//! pace's `pace_text`, read from the session's `pace` and `chapter` rows.
//!
//! Only the reading is here; the numbers, the words and the writes are pace's. A chapter that is
//! missing, another session's or older than two hours is read as a fresh one and not stored, which
//! says what `newChapter` would.

use crate::companion::{
    app::pace::{pace_of, pace_text, Chapter, CHAPTER_MS},
    relay::dispatch::Ctx,
};
use serde_json::Value;

/// The session's chapter as `chapterOf` would find it, without storing the fresh one it would
/// start.
fn chapter_read(cx: &Ctx) -> Chapter {
    let now = cx.clock.now();
    let fresh = Chapter::fresh(&cx.sid, now);
    let Some(Value::Object(row)) = cx.store.get(&cx.sid, "chapter").ok().flatten() else {
        return fresh;
    };
    let live = row["session"] == cx.sid.as_str()
        && row["started"]
            .as_f64()
            .is_some_and(|at| now - at < CHAPTER_MS);
    if !live {
        return fresh;
    }
    let mut base = serde_json::to_value(&fresh).unwrap_or(Value::Null);
    if let Some(fields) = base.as_object_mut() {
        fields.extend(row);
    }
    serde_json::from_value(base).unwrap_or(fresh)
}

/// `dreamsText`: the pace in words, or that dreams are off.
pub fn dreams_text(cx: &Ctx, on: bool) -> String {
    if !on {
        return pace_text(None, None, false);
    }
    pace_text(Some(&pace_of(cx)), Some(&chapter_read(cx)), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::relay::{dispatch::testing::fixture, host::fake::World};
    use serde_json::json;

    #[test]
    fn a_chapter_that_is_missing_another_s_or_old_is_read_as_a_fresh_one_and_not_stored() {
        let fx = fixture(World::default());
        let cx = &fx.cx;
        let now = cx.clock.now();
        let fresh = Chapter::fresh(&cx.sid, now);
        assert_eq!(chapter_read(cx), fresh, "none");
        let row = |session: &str, started: f64| {
            json!({ "session": session, "started": started, "steps": 3, "due": 9, "looks": 1,
                "shown": 2, "adopted": 1 })
        };
        cx.store
            .set(&cx.sid, "chapter", &row("other", now))
            .unwrap();
        assert_eq!(chapter_read(cx), fresh, "another session's");
        let old = row(&cx.sid, now - CHAPTER_MS);
        cx.store.set(&cx.sid, "chapter", &old).unwrap();
        assert_eq!(chapter_read(cx), fresh, "two hours old");
        assert_eq!(cx.store.get(&cx.sid, "chapter").unwrap(), Some(old));
    }

    #[test]
    fn a_live_chapter_is_read_as_it_is_and_the_words_come_from_pace() {
        let fx = fixture(World::default());
        let cx = &fx.cx;
        let now = cx.clock.now();
        let live = json!({ "session": cx.sid, "started": now, "steps": 2, "due": 9, "looks": 1,
            "shown": 1, "adopted": 1 });
        cx.store.set(&cx.sid, "chapter", &live).unwrap();
        let chapter = chapter_read(cx);
        assert_eq!((chapter.steps, chapter.due, chapter.adopted), (2., 9., 1.));
        assert_eq!(
            dreams_text(cx, true),
            pace_text(Some(&pace_of(cx)), Some(&chapter), true)
        );
        assert!(dreams_text(cx, true).contains("5 of 6 dreams left in this chapter"));
        assert_eq!(dreams_text(cx, false), pace_text(None, None, false));
    }
}
