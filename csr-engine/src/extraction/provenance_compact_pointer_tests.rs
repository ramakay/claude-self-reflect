use super::*;
use crate::transcript::compact_pointer_line;

const SID: &str = "3002ab28-7ba1-4c61-9d2e-0a1b2c3d4e5f";

fn emitted() -> [String; 2] {
    [
        compact_pointer_line(SID, Some((12, 340))),
        compact_pointer_line(SID, None),
    ]
}

/// The exact lines the hook prints, in both forms, are CSR's own output: alone, and in the
/// middle of a longer block past the header window.
#[test]
fn the_compact_pointer_is_suppressed_from_reimport_in_both_forms() {
    for line in emitted() {
        assert!(is_compact_pointer_line(&line), "{line}");
        assert!(is_csr_emission(&line), "{line}");
        assert!(extractable(&line).is_none(), "{line}");
        assert_eq!(strip_csr_emissions(&line), "", "{line}");

        let before = "I asked for the parser fix and you changed three files. ".repeat(10);
        let around = format!("{before}\n{line}\nWhy did it stop?");
        assert!(around.find(&line).unwrap() > HEADER_WINDOW);
        assert!(is_csr_emission(&around));
        assert!(extractable(&around).is_none());
        // The line pass keeps what the person wrote around it.
        assert_eq!(
            strip_csr_emissions(&format!("Look at this.\n{line}\nWhy?")),
            "Look at this.\nWhy?"
        );
    }
}

/// A person's own words about csr_transcript are not the emitter's grammar.
#[test]
fn prose_about_csr_transcript_stays_extractable() {
    let [ranged, _] = emitted();
    let first_half = ranged
        .split(" view=\"yourasks\"")
        .next()
        .unwrap()
        .to_string();
    let edited = ranged.replace("The summary replaced them.", "The summary dropped them.");
    let other_tool = ranged.replace("csr_transcript(", "csr_other(");
    let spoken = format!("My note: {ranged}");
    let bulleted = format!("- {ranged}");
    for text in [
        "Please read turns 3..9 with csr_transcript and tell me",
        "Before this compact: I was reading turns 3..9 with csr_transcript(session=\"x\", view=\"slice\").",
        "Use csr_transcript(session=\"abc\", view=\"slice\", turns=\"3..9\") to read what the summary replaced.",
        first_half.as_str(),
        edited.as_str(),
        other_tool.as_str(),
        spoken.as_str(),
        bulleted.as_str(),
    ] {
        assert!(!is_compact_pointer_line(text), "{text}");
        assert!(!is_csr_emission(text), "{text}");
        assert_eq!(extractable(text).as_deref(), Some(text), "{text}");
        assert_eq!(strip_csr_emissions(text), text, "{text}");
    }
    // Two lines are not one pointer line.
    assert!(!is_compact_pointer_line(&format!("a\n{ranged}")));
}

/// An id or turn number outside the emitter's own shapes is not the line.
#[test]
fn the_pointer_pattern_keeps_its_open_parts_tight() {
    let odd_id = compact_pointer_line("bad id\"; rm", Some((1, 2)));
    assert!(!is_compact_pointer_line(&odd_id));
    let long = compact_pointer_line(&"a".repeat(81), Some((1, 2)));
    assert!(!is_compact_pointer_line(&long));
    let huge = compact_pointer_line(SID, Some((1, 12_345_678_901)));
    assert!(!is_compact_pointer_line(&huge));
    // The edges of what the emitter writes.
    let longest = compact_pointer_line(&"a".repeat(80), Some((100_000_000, 999_999_999)));
    assert!(is_compact_pointer_line(&longest));
}
