/// #6: the person asks about the card; a header matched as a substring took it for the card's draft.
pub(crate) const CARD_REQUEST: &str =
    "Please change CSR's compaction card (the one in the sidebar) to show savings in tokens.";

/// #7: the person's own sentence that opens with the attribution's words, past HEADER_WINDOW.
pub(crate) fn attribution_talk() -> String {
    format!(
        "{}\n\nCSR's dreamer asked this while you worked yesterday, but I wrote today's question myself.",
        "This is an actual observation that I wrote myself. ".repeat(10)
    )
}

/// #8: the band's forward of a 223-character Devanagari dream (553 bytes, past HEADER_WINDOW).
pub(crate) fn hindi_forward() -> String {
    let question = format!(
        "\u{915}\u{94d}\u{92f}\u{93e} \u{939}\u{94b} \u{905}\u{917}\u{930} {}?",
        "\u{939}\u{92e} \u{92a}\u{947}\u{928} \u{915}\u{947} \u{930}\u{902}\u{917} \u{921}\u{947}\u{915} \u{915}\u{940} \u{938}\u{94d}\u{92f}\u{93e}\u{939}\u{940} \u{938}\u{947} \u{932}\u{947}\u{902} "
            .repeat(6)
    );
    format!(
        "{question}\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Share the palette**\n> * Two surfaces read the same inks."
    )
}
