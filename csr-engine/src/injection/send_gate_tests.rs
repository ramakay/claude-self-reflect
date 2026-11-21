use super::super::formatter::RenderedInjectionItem;
use super::*;

#[derive(serde::Deserialize)]
struct Pair {
    n: u32,
    stratum: String,
    top1: Option<f32>,
    n_items: usize,
    used: bool,
    on_topic: bool,
}

#[derive(serde::Deserialize)]
struct Pairs {
    pairs: Vec<Pair>,
}

fn pairs() -> Vec<Pair> {
    serde_json::from_str::<Pairs>(include_str!("../../tests/fixtures/prompt_gate_pairs.json"))
        .unwrap()
        .pairs
}

/// The sample oversamples rule-flagged turns: 18 of 18 against 42 of 150.
fn weight(pair: &Pair) -> f64 {
    if pair.stratum == "rule_positive" {
        1.0
    } else {
        150.0 / 42.0
    }
}

fn share(pairs: &[&Pair], of: impl Fn(&Pair) -> bool) -> f64 {
    let total: f64 = pairs.iter().map(|p| weight(p)).sum();
    pairs
        .iter()
        .filter(|p| of(p))
        .map(|p| weight(p))
        .sum::<f64>()
        / total
}

/// Replays every labelled pair through the gate with its recorded score and
/// rendered-item count. A pair labelled not on topic is a look-alike: its
/// items shared only the project, the kind of task or a word with the prompt.
/// Without the gate the hook sent all 60.
#[test]
fn recorded_pairs_replay_through_the_gate() {
    let pairs = pairs();
    assert_eq!(pairs.len(), 60);
    let sent: Vec<&Pair> = pairs
        .iter()
        .filter(|p| decide(p.top1, p.n_items) == Gate::Send)
        .collect();
    let all: Vec<&Pair> = pairs.iter().collect();
    let lookalikes: Vec<&Pair> = pairs.iter().filter(|p| !p.on_topic).collect();
    let withheld = |p: &Pair| decide(p.top1, p.n_items) != Gate::Send;
    let counts = (
        sent.len(),
        sent.iter().filter(|p| p.used).count(),
        pairs.iter().filter(|p| p.used).count(),
        lookalikes.iter().filter(|p| withheld(p)).count(),
        lookalikes.len(),
        sent.iter().filter(|p| p.on_topic).count(),
    );
    eprintln!(
        "gate replay: sent {}/60, uses kept {}/{}, look-alikes withheld {}/{}, on topic {}/{} sent; \
         weighted: on topic {:.3}, look-alikes withheld {:.3}, send rate {:.3}; sent pairs {:?}",
        counts.0,
        counts.1,
        counts.2,
        counts.3,
        counts.4,
        counts.5,
        counts.0,
        share(&sent, |p| p.on_topic),
        share(&lookalikes, withheld),
        share(&all, |p| !withheld(p)),
        sent.iter().map(|p| p.n).collect::<Vec<_>>(),
    );
    // Unchanged by the prompt-time admission rules (machine prompts, the
    // automatic-block kind rule, boilerplate, own-session and repeat items,
    // keyword-only pools): each pair keeps only its best score and its
    // rendered-item count, no prompt, item text or kind, and the floor and the
    // minimum did not move. A replay of those rules needs the whole pipeline.
    assert_eq!(counts, (25, 4, 4, 27, 38, 14));

    // The item's three lines, weighted. Only the send-rate line holds. No
    // floor (0.3 to 0.65 in steps of 0.05) with a minimum of 1 to 5 items
    // meets all three at once on these two features: the cells that clear 70%
    // on topic and 80% withheld send under 30% of the prompts. This pins which
    // lines hold, so improving the rule is a visible change here, and checks
    // the gate beats sending everything on both quality lines.
    let on_topic = share(&sent, |p| p.on_topic);
    let withheld_share = share(&lookalikes, withheld);
    let send_rate = share(&all, |p| !withheld(p));
    assert_eq!(
        (on_topic >= 0.70, withheld_share >= 0.80, send_rate >= 0.30),
        (false, false, true),
        "on topic {on_topic:.3}, withheld {withheld_share:.3}, send {send_rate:.3}"
    );
    assert!(on_topic > share(&all, |p| p.on_topic), "{on_topic:.3}");
    assert!(withheld_share > 0.5, "{withheld_share:.3}");
}

fn winning(scores: &[f32]) -> Vec<InjectionItem> {
    scores
        .iter()
        .map(|&score| InjectionItem {
            content: String::new(),
            score,
            source: String::new(),
        })
        .collect()
}

fn receipt(items: &[(InjectionCategory, usize)]) -> FormattedInjection {
    FormattedInjection {
        text: String::new(),
        rendered_items: items
            .iter()
            .map(|&(category, index)| RenderedInjectionItem { category, index })
            .collect(),
    }
}

/// The score comes from the items that rendered: a stronger item the filters
/// or the budget dropped does not count, and neither does another category.
#[test]
fn the_floor_reads_only_the_rendered_items() {
    let items = winning(&[0.8, 0.30, 0.41, 0.2]);
    let rendered = receipt(&[
        (InjectionCategory::WinningStrategy, 1),
        (InjectionCategory::WinningStrategy, 2),
        (InjectionCategory::RelevantContext, 0),
    ]);
    assert_eq!(top_rendered_score(&items, &rendered), Some(0.41));
    assert_eq!(rendered_items(&rendered), 2);
    assert_eq!(top_rendered_score(&items, &receipt(&[])), None);
    let all = receipt(&[(InjectionCategory::WinningStrategy, 0)]);
    assert_eq!(top_rendered_score(&items, &all), Some(0.8));
}

#[test]
fn each_abstain_names_the_condition_that_failed() {
    assert_eq!(decide(None, 5), Gate::BelowFloor);
    assert_eq!(decide(Some(0.449), 5), Gate::BelowFloor);
    assert_eq!(decide(Some(0.30), 0), Gate::BelowFloor);
    assert_eq!(decide(Some(0.45), 2), Gate::ThinPool);
    assert_eq!(decide(Some(0.757), 0), Gate::ThinPool);
    assert_eq!(decide(Some(0.45), 3), Gate::Send);
}

#[test]
fn the_off_switch_reads_only_one() {
    let _guard = crate::daemon::dream_cadence::env_test_guard();
    {
        let _on = Switch::on();
        assert!(enabled());
    }
    let _off = Switch::off();
    assert!(!enabled());
    std::env::set_var(OFF_SWITCH, "0");
    assert!(enabled());
}
