//! `CSR_NO_DREAMING` stops an agent's completion look as the `/config` Dreams row off does, and
//! wins over a row that is on. The fleet reads it through `env.get`, so a test scripts that
//! answer (it never sets process env).

use super::*;

/// `env.get` answers `value` (`None`: `CSR_NO_DREAMING` is not set) for the next 3000 reads.
fn set_switch(rig: &Rig, value: Option<&str>) {
    let answers: VecDeque<Result<Value, String>> = (0..3000)
        .map(|_| Ok(value.map_or(Value::Null, |value| json!(value))))
        .collect();
    let mut world = rig.fx.host.world.lock().unwrap();
    world.script.insert("env.get".into(), answers);
}

#[tokio::test]
async fn csr_no_dreaming_stops_an_agents_completion_look_though_the_dreams_row_is_on() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    set_switch(&rig, Some("1"));
    rig.roomy(json!({}));
    rig.reported("a1", "turn-1", REPORT).await;
    rig.steps(1, None, None).await;
    rig.advance(20_000.).await;
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    assert_eq!(rig.forks(), Vec::<String>::new());
    assert!(!rig.looked_at());
}

/// The fleet has asked `/config` nothing yet, so its first read of the Dreams row is the one the
/// test scripts next.
fn fleet_not_yet_asked(rig: &Rig) {
    rig.fx.cx.kept(|f: &mut Fleet| f.options = None);
}

#[tokio::test]
async fn a_dreams_row_turned_off_after_the_fleets_first_read_stops_the_look_for_a_finished_agent() {
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
    set_switch(&rig, None);
    fleet_not_yet_asked(&rig);
    rig.roomy(json!({}));
    // the fleet's first read: the row is on, so the report arms the idle timer
    rig.reported("a1", "turn-1", REPORT).await;
    assert!(rig.fx.cx.kept(|f: &mut Fleet| f.timer));
    // the person turns the row off before the timer fires
    script_dreams(&rig.fx.host, false);
    rig.advance(20_000.).await;
    assert!(until(|| !rig.fx.cx.kept(|f: &mut Fleet| f.timer)).await);
    assert_eq!(rig.forks(), Vec::<String>::new());
    assert!(!rig.looked_at());
}

#[tokio::test]
async fn a_dreams_row_turned_on_after_the_fleets_first_read_lets_the_look_for_a_finished_agent_run()
{
    let rig = dreaming_rig(brief_with(&[said("a1", REPORT), said("a2", REPORT)]));
    set_switch(&rig, None);
    script_dreams(&rig.fx.host, false);
    fleet_not_yet_asked(&rig);
    rig.roomy(json!({}));
    // the fleet's first read: the row is off, so the report arms nothing
    rig.reported("a1", "turn-1", REPORT).await;
    assert!(!rig.fx.cx.kept(|f: &mut Fleet| f.timer));
    // the person turns the row on, and the next agent reports
    script_dreams(&rig.fx.host, true);
    rig.reported("a2", "turn-1", REPORT).await;
    rig.advance(20_000.).await;
    assert!(until(|| !rig.forks().is_empty()).await);
    assert!(until(|| rig.looked_at()).await);
}

#[tokio::test]
async fn csr_no_dreaming_set_to_zero_false_or_nothing_is_not_set_and_the_completion_look_runs() {
    for value in ["0", "false", ""] {
        let rig = dreaming_rig(brief_with(&[said("a1", REPORT)]));
        set_switch(&rig, Some(value));
        rig.roomy(json!({}));
        rig.reported("a1", "turn-1", REPORT).await;
        rig.steps(1, None, None).await;
        assert!(until(|| !rig.forks().is_empty()).await);
    }
}
