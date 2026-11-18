use super::*;
use crate::companion::native_warn::{Kind, Source, Warning};

fn emitted(source: Source) -> String {
    Warning {
        kind: Kind::Window,
        window: 300_000.,
        wins: 2,
        source,
        off_by_env: false,
    }
    .line()
}

fn emitted_off() -> String {
    Warning::native_off(false, Source::Settings).line()
}

/// The exact grammar the emitter writes, in both forms, is CSR's own output.
#[test]
fn the_native_window_warning_is_suppressed_from_reimport_in_both_forms() {
    for source in [Source::Settings, Source::Env] {
        let line = emitted(source);
        assert!(is_native_warn_line(&line), "{line}");
        assert!(is_csr_emission(&line), "{line}");
        assert!(extractable(&line).is_none(), "{line}");
        // pasted into a longer message, alone on its line
        let around = format!("Look at this.\n{line}\nWhy?");
        assert!(is_csr_emission(&around));
    }
    assert_eq!(
        emitted(Source::Settings),
        "Claude's 300k auto-compact window beat CSR 2\u{d7} this week. \
             Remove autoCompactWindow from settings.json to let CSR lead."
    );
}

/// The native-off warning is CSR's own output too, in the same forms.
#[test]
fn the_native_off_warning_is_suppressed_from_reimport_in_both_forms() {
    let line = emitted_off();
    assert_eq!(
        line,
        "Claude's auto-compact is off. A long turn can pass 305k. For a backstop, set \
             autoCompactEnabled true and autoCompactWindow 338000."
    );
    assert!(is_native_warn_line(&line), "{line}");
    assert!(is_csr_emission(&line), "{line}");
    assert!(extractable(&line).is_none(), "{line}");
    // pasted into a longer message, alone on its line
    let around = format!("Look at this.\n{line}\nWhy?");
    assert!(is_csr_emission(&around));
    assert!(!is_native_warn_line(&format!("a\n{line}")));
}

/// The three variant forms are CSR's own output too: the env-off line and the two that name
/// the window variable.
#[test]
fn every_native_off_form_is_suppressed_from_reimport() {
    let forms = [
        Warning::native_off(false, Source::Env).line(),
        Warning::native_off(true, Source::Settings).line(),
        Warning::native_off(true, Source::Env).line(),
    ];
    assert!(forms[1].contains("off by DISABLE_AUTO_COMPACT"));
    assert!(forms[0].contains("CLAUDE_CODE_AUTO_COMPACT_WINDOW=338000"));
    for line in forms {
        assert!(is_native_warn_line(&line), "{line}");
        assert!(is_csr_emission(&line), "{line}");
        assert!(extractable(&line).is_none(), "{line}");
        assert_eq!(strip_csr_emissions(&line), "", "{line}");
        let around = format!("Look at this.\n{line}\nWhy?");
        assert!(is_csr_emission(&around));
        assert_eq!(
            strip_csr_emissions(&around),
            "Look at this.\nWhy?",
            "{line}"
        );
        assert!(!is_native_warn_line(&format!("a\n{line}")));
    }
    // Head of one form with the tail of another is not an emitted line.
    let mixed = "Claude's auto-compact is off by DISABLE_AUTO_COMPACT. A long turn can pass 305k. For a backstop, set autoCompactEnabled true and autoCompactWindow 338000.";
    assert!(!is_native_warn_line(mixed));
}

/// A person's words about the off state are not the emitter's grammar.
#[test]
fn prose_about_the_native_off_state_stays_extractable() {
    for text in [
        "Claude's auto-compact is off and Self-Reflect's is on, so I turned it back on.",
        "My note: Claude's auto-compact is off. A long turn can pass 305k. For a backstop, set autoCompactEnabled true and autoCompactWindow 338000.",
        "Claude's auto-compact is off. A long turn can pass 305k. For a backstop, set autoCompactEnabled true and autoCompactWindow 338000, I think.",
    ] {
        assert!(!is_csr_emission(text), "{text}");
        assert_eq!(extractable(text).as_deref(), Some(text), "{text}");
    }
}

/// A person's own words about the setting are not the emitter's grammar.
#[test]
fn prose_about_the_auto_compact_window_stays_extractable() {
    for text in [
        "Claude's 300k auto-compact window beat CSR twice, so I removed autoCompactWindow.",
        "Claude's 300k auto-compact window beat CSR 2\u{d7} this week, I think.",
        "Remove autoCompactWindow from settings.json to let CSR lead.",
        "Claude's Nk auto-compact window beat CSR n\u{d7} this week. Remove autoCompactWindow from settings.json to let CSR lead.",
        "My note: Claude's 300k auto-compact window beat CSR 2\u{d7} this week. Remove autoCompactWindow from settings.json to let CSR lead.",
    ] {
        assert!(!is_csr_emission(text), "{text}");
        assert_eq!(extractable(text).as_deref(), Some(text), "{text}");
    }
    assert!(!is_native_warn_line("a\nClaude's 300k auto-compact window beat CSR 2\u{d7} this week. Remove autoCompactWindow from settings.json to let CSR lead."));
}
