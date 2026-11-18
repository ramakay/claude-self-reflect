use super::*;
use crate::companion::render::instrument::{text_rows, Open};
use serde_json::json;

const NOW: f64 = 1_800_000_000.;

/// One saved state where every card the contract words has something to say.
fn rich() -> Composition {
    let s = json!({"id": "parity"});
    let reading = json!({
        "asks": 9, "missed": 3, "unclear": 1,
        "line": "You keep sending screenshots to show what Claude could not see.",
        "misses": [
            {"turn": 4, "ask": "show the pane", "miss": "built from docs"},
            {"turn": 9, "ask": "match herdr", "miss": "orbit instead"},
            {"turn": 12, "ask": "keep the pieces", "miss": "cards dropped"}
        ],
        "intents": ["see the pane"], "next": "look on the Studio"
    });
    let data = json!({"dreams": [{"session": "parity", "at": NOW - 600., "same_page": reading}]});
    let mut c = Composition::build(&data, &s, NOW);
    c.offer = false;
    c.dream.state = Batch::Nothing;
    c.dream.looks = 45;
    c.dream.none = 4;
    c.dream.budget = json!({"spent": 607_000., "cap": 5_900_000.});
    c.confusing = crate::companion::same_page::card(&data, &s, vec![]);
    c.should_be = vec![
        json!({"kind": "memory", "line": "Look at a pane change on a real screen before reporting it.", "evidence": "quoted"}),
        json!({"kind": "skill", "line": "A clean probe for a live mod was rebuilt by hand.", "evidence": "quoted"}),
    ];
    c.story = "The work lives in the feat/pane-mod worktree. The rest follows.".into();
    c.story_age = "refreshed 5m ago".into();
    c.learnt = vec!["1 note kept".into()];
    c.shifts =
        vec![json!({"scope": "picture.ts", "sym": "drawPicture", "mark": "+", "verb": "added"})];
    c.figure = json!({
        "outlook": {
            "headline": {"text": "Early in the week. Weekly is at 7% so far.", "sev": "quiet"},
            "windows": [{"code": "7D", "name": "Weekly", "used": 7.0, "sev": "ok",
                         "line": "Early · earlier weeks peaked around 78% · resets in 6d 16h"}]
        },
        "quotas": [{"code": "7D", "used": 7.0, "text": "7D 7%"}],
        "ctx": {"tokens": 130_000.0, "size": 1_000_000.0},
        "breakdown": {"floor": 75_562.0, "floor_name": "Summary + setup", "text": 20_000.0,
                      "you": 3_633.0, "tool": 31_222.0, "csr": 0.0}
    });
    c.codex_week = json!({"percent": 22.0});
    c.tally = json!({"by_csr": 3, "payback": 1_800_000.});
    c
}

fn card<'a>(cards: &'a [Card], id: &str) -> &'a Card {
    cards
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no {id} card in {cards:#?}"))
}

/// Parity: every word and number of each card, as the herdr pane's own text render shows it.
#[test]
fn the_contract_cards_say_what_the_herdr_text_pane_says() {
    let c = rich();
    let cards = cards(&c);
    let rows = text_rows(&c, 400, 80, Open::default());
    let has = |s: &str| rows.iter().any(|r| r == s);
    let all = rows.join("\n");

    // Order: the herdr pane's own.
    let ids: Vec<&str> = cards.iter().map(|c| c.id).collect();
    assert_eq!(
        ids,
        ["dreamer", "same_page", "should_be", "story", "limits"]
    );

    // Dreamer: the status line is a row of its own; the budget is the meta row's "dreamer X of Y".
    let d = card(&cards, "dreamer");
    assert_eq!(d.title, "Self-Reflect's dreamer");
    assert_eq!(d.tag.as_deref(), Some("Looks as you work"));
    let status = d
        .lines
        .iter()
        .find(|l| l.style == "dim")
        .expect("status line");
    assert_eq!(
        status.text,
        "The last look found nothing. 4 of 45 looks end this way."
    );
    assert!(has(&status.text), "{all}");
    let ring = d.ring.as_ref().expect("ring");
    assert_eq!(
        (ring.spent, ring.cap, ring.turning),
        (607_000., 5_900_000., false)
    );
    assert!(
        all.contains(&format!(
            "dreamer {} of {}",
            short(ring.spent),
            short(ring.cap)
        )),
        "{all}"
    );

    // Same page?: its title, then the numbers and the model's words, which the text pane writes
    // as one row.
    let sp = card(&cards, "same_page");
    assert!(has(&sp.title), "{all}");
    assert_eq!(
        sp.lines[0].text,
        "Model's read: 3 of 9 asks missed. You were unclear on 1."
    );
    assert!(
        has(&format!("{} {}", sp.lines[0].text, sp.lines[1].text)),
        "{all}"
    );

    // Should be: its title, and each item a row of the text pane.
    let sb = card(&cards, "should_be");
    assert!(has(&sb.title), "{all}");
    assert_eq!(sb.tag.as_deref(), Some("1 of 2"));
    for l in &sb.lines {
        assert!(has(&l.text), "{} not in\n{all}", l.text);
    }

    // Story: refreshed age (the text pane's "Story · <age>"), the first sentence, notes, code rows.
    let st = card(&cards, "story");
    assert_eq!(st.title, story_title(&c));
    assert!(
        has(&format!("Story · {}", st.tag.as_deref().unwrap_or(""))),
        "{all}"
    );
    for l in &st.lines {
        // The text pane wraps its rows, which folds runs of spaces to one.
        let words = l.text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(all.contains(&words), "{words} not in\n{all}");
    }

    // Limits: each bar's figure is the text pane's; the headline is the forecast's own sentence.
    let lm = card(&cards, "limits");
    assert_eq!(lm.title, "Early in the week. Weekly is at 7% so far.");
    assert_eq!(lm.tone.as_deref(), Some("quiet"));
    let texts: Vec<(&str, &str)> = lm
        .rows
        .iter()
        .map(|r| (r.label.as_str(), r.text.as_str()))
        .collect();
    assert_eq!(texts, [("Weekly", "7%"), ("Codex 7D", "22%")]);
    assert!(all.contains("7D 7%"), "{all}");
    assert!(all.contains("Codex 7D 22%"), "{all}");
}

#[test]
fn the_painter_and_the_contract_share_their_words() {
    let c = rich();
    // The dashboard's headline, windows and Codex bar come from the same functions.
    assert_eq!(
        headline(&c),
        Some((
            "Early in the week. Weekly is at 7% so far.".into(),
            "quiet".into()
        ))
    );
    assert_eq!(
        limits(&c)[0].line,
        "Early · earlier weeks peaked around 78% · resets in 6d 16h"
    );
    assert_eq!(codex(&c).map(|x| x.0), Some(22.));
    // No agent runs: no agents card and no agents row.
    assert_eq!(agents_line(&c), None);
    let mut tally = c.clone();
    tally.tally = json!({"by_csr": 3, "payback": 1_800_000.});
    let parts: Vec<String> = csr_parts(&tally).into_iter().map(|p| p.1).collect();
    assert_eq!(parts, ["3 compactions", "~1.8M tokens saved (est.)"]);
}

/// Stage 2: the context window and Self-Reflect's row as data, as the Context card reads them.
#[test]
fn the_context_and_the_csr_row_are_the_herdr_cards_own() {
    let c = rich();
    let ctx = context(&c).expect("context");
    assert_eq!(
        (ctx.used, ctx.max, ctx.free, ctx.buffer),
        (130_000., 1_000_000., 870_000., None)
    );
    let names: Vec<&str> = ctx.parts.iter().map(|p| p.name).collect();
    assert_eq!(names, ["Summary + setup", "Messages", "Tool results"]);
    let mix = ctx.mix.expect("mix");
    assert_eq!(mix.verdict, "context-heavy");
    // The donut's own reading of the same parts.
    let (named, _) = context_parts(&c);
    let tagged: Vec<(&'static str, f64, [u8; 4])> =
        named.iter().map(|(n, v, _)| (*n, *v, [0; 4])).collect();
    assert_eq!(
        instrument::session_mix(&tagged).map(|m| m.1),
        Some("context-heavy")
    );
    let row = csr_row(&c).expect("csr row");
    assert_eq!(row.lead, "Self-Reflect this session:");
    assert_eq!(row.parts, ["3 compactions", "~1.8M tokens saved (est.)"]);
    // The herdr pane draws "<lead> <parts joined by ' · '>": the same words.
    assert_eq!(
        format!("{} {}", row.lead, row.parts.join(" · ")),
        "Self-Reflect this session: 3 compactions · ~1.8M tokens saved (est.)"
    );
}

/// The JSON the mod reads, kept as a golden file in the mod's fixtures so it cannot go stale:
/// `CSR_BLESS=1` writes it again.
#[test]
fn the_golden_contract_file_matches() {
    // A .ts module: the mod's tests may import only code. Its body is the `--json` contract.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../mods/csr-pane/hooks/fixtures/pane-v1.ts");
    let json = serde_json::to_string_pretty(&contract(&rich())).expect("json");
    let got = format!(
        "// Written by csr-engine (companion/contract_tests.rs, CSR_BLESS=1). Do not edit.\nexport const PANE_V1 = {json}\n"
    );
    if std::env::var_os("CSR_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().expect("dir")).expect("mkdir");
        std::fs::write(&path, &got).expect("write golden");
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        got,
        want,
        "golden {} is stale: run with CSR_BLESS=1",
        path.display()
    );
}
