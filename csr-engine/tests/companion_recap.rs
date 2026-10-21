use csr_engine::companion::{
    render::{
        instrument::{self, Composition, Open},
        theme::Theme,
    },
    repo_recap::RepoRecap,
};
use serde_json::json;

#[test]
fn empty_cards_stack_at_s3_and_the_final_card_owns_surplus_height() {
    let c = Composition::build(&json!({}), &json!({"id":"new-session"}), 1000.);
    assert!(c.empty_session);
    for theme in [Theme::Dark, Theme::Light] {
        for (cols, rows) in [(38, 30), (44, 30), (60, 40), (76, 67)] {
            let (_, m) = instrument::draw(
                &c,
                cols * 16,
                (rows - 1) * 34,
                theme,
                Open::default(),
                false,
            )
            .unwrap();
            let (help_y, help_h) = m.help_region.unwrap();
            assert!(
                help_h >= 190.,
                "final card must own surplus height: {help_h}"
            );
            let first_gap = m.figure_region.0 - (16. + m.dream);
            let second_gap = help_y - (m.figure_region.0 + m.figure_region.1);
            assert!(
                (first_gap - 12.).abs() < 0.1 && (second_gap - 12.).abs() < 0.1,
                "{first_gap} vs {second_gap}"
            );
            assert!(m.figure_region.1 >= 154.);
            assert!((m.last_card_gap_px - 12.).abs() <= 2.);
        }
    }
}

#[test]
fn prior_recap_replaces_pitch_without_dropping_its_receipts() {
    let data = json!({"repo_recap":RepoRecap {session:"prior-session".into(),at:100.,age:"15m ago".into(),
        history:vec![],text:"Repair the audio player: fixed pause handling (abcdef1).\nSettled: use one audio owner (fedcba9).\nNext: run the device tests.".into()}});
    let c = Composition::build(&data, &json!({"id":"brand-new"}), 1000.);
    for cols in [38, 44] {
        let rows = instrument::text_rows(&c, cols, 60, Open::default()).join("\n");
        assert!(rows.contains("Last time here") && rows.contains("15m ago"));
        assert!(rows.contains("abcdef1") && rows.contains("fedcba9"));
        assert!(!rows.contains("Ask about past work"));
        let (_, m) = instrument::draw(
            &c,
            cols as u32 * 16,
            29 * 34,
            Theme::Light,
            Open::default(),
            false,
        )
        .unwrap();
        assert!(m.help_region.unwrap().1 > 300.);
        assert!((m.last_card_gap_px - 12.).abs() <= 2.);
    }
    let active = Composition::build(&data, &json!({"id":"active","prompts":[900.]}), 1000.);
    assert!(active.repo_recap.is_none());
}
