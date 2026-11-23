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
