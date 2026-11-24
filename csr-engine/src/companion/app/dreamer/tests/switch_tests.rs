//! `CSR_NO_DREAMING` stops the companion's looks as the `/config` Dreams row off does, and wins
//! over a row that is on. The companion reads it through `env.get`, so a test scripts that answer
//! (it never sets process env).

use super::*;
use std::collections::VecDeque;

/// `env.get` answers `value` (`None`: `CSR_NO_DREAMING` is not set) for the next 3000 reads.
fn set_switch(r: &Rig, value: Option<&str>) {
    let answers: VecDeque<Result<Value, String>> = (0..3000)
        .map(|_| Ok(value.map_or(Value::Null, |value| json!(value))))
        .collect();
    let mut world = r.fx.host.world.lock().unwrap();
    world.script.insert("env.get".into(), answers);
}

#[tokio::test]
async fn csr_no_dreaming_stops_the_main_schedule_though_the_dreams_row_is_on() {
    let r = rig(replies(&[dream_reply()]));
    set_switch(&r, Some("1"));
    r.go(30).await;
    quiesce().await;
    assert_eq!(r.forks(), Vec::<String>::new());
    assert_eq!(r.looks(), Vec::<Value>::new());
    // The same rig looks once the switch is gone.
    set_switch(&r, None);
    r.go(30).await;
    assert!(until(|| !r.forks().is_empty()).await);
}

#[tokio::test]
async fn csr_no_dreaming_set_to_zero_false_or_nothing_is_not_set_and_the_looks_start() {
    for value in ["0", "false", ""] {
        let r = rig(replies(&[dream_reply()]));
        set_switch(&r, Some(value));
        r.go(30).await;
        assert!(until(|| !r.forks().is_empty()).await);
    }
}

#[tokio::test]
async fn csr_no_dreaming_leaves_the_dream_that_is_up_uncarried_at_a_compaction() {
    let r = rig(replies(&[dream_reply()]));
    let cx = r.cx();
    r.go(12).await;
    assert!(until(|| r.up().is_some()).await);
    let view = r.row(SID, "view");
    let chapter = r.row(SID, "chapter");
    set_switch(&r, Some("TRUE"));
    r.compact(&cx, "manual").await;
    // As with the row off: not carried, not expired, no new chapter.
    assert_eq!(r.row(SID, "view"), view);
    assert_eq!(r.row(SID, "chapter"), chapter);
    assert!(r.with("answer", "answer", "carried").is_empty());
    assert!(r.with("answer", "answer", "expired").is_empty());
}

#[tokio::test]
async fn csr_no_dreaming_keeps_no_prompts_for_the_dreamer() {
    let r = rig(world(Some(true)));
    let cx = r.cx();
    set_switch(&r, Some("1"));
    r.prompt(&cx, "first ask", "composer").await;
    assert_eq!(r.row(SID, "asked"), Value::Null);
    set_switch(&r, None);
    r.prompt(&cx, "second ask", "composer").await;
    assert_eq!(r.row(SID, "asked"), json!(["second ask"]));
}
