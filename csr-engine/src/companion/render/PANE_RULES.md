# Pane layout rules

The picture pane (`instrument::draw`) shares one fixed height among its cards. Every rule below
is checked by `tests/pane_contract.rs` for every fixture, pane size and theme. A failure ends
`REDO: the layout violates PANE_RULES #N`.

1. **Height budget.** The pane's last row is the key row. The picture fills the rows above it
   and no more: `(rows - 1) * cell_h` px, `Metrics::limit` pt of layout. No card, no card's
   content and no text run ends below `limit`.
2. **Fonts are never resized to fit.** The canvas is never taller than `limit`. Text size comes
   from the pane alone: the width fit, or the 800 pt floor of a short pane. Content that does
   not fit gives way (rule 4); it never shrinks the type. The dreamer card keeps to 30% of that
   height: a long question or status loses its last lines ("…"), never grows the canvas.
3. **Tiers.** T0 is never folded: the dreamer card (hero), the Context card, an open section or
   page, the stack row and the open accordion card. T1 never folds and stays small: notice,
   agents bar, empty-session help. T2 folds into the stack: the two lists (What confused the agent,
   Should be a skill / memory / tool), the Lost and found card (one row: this session's newest open
   thread, drawn only while one is open, after Should be and before the story; it folds with the
   lists and a tap on it opens that thread's handoff popup), the story (Where the work stands) and
   CSR performance.
4. **Collapse order.** Content gives way in `FIT_STEPS` order, first to go first: performance
   check rows, its clock and bar, list rows to 3, list rows to 1, the credit row, rings to one
   line (a new session's start note goes with them, rule 14), the Context card's "this turn" note
   and cache line (and with them the "CSR handed over" row of a session past its start, rule 14),
   both lists and the Lost and found card fold, the story folds,
   performance folds; with a section open, the Context card to its floor, then the section to its
   least rows. A T2 card is never drawn while a card
   that folds before it is folded (the open accordion card aside).
   The notice (T1) wraps to three lines so its words are never cut. It keeps its first line only
   when a section or the accordion card is open, or when every card has folded and the picture
   would still be cut; it is never the first thing to give way.
5. **Minimum and full heights.** Each card's height is computed by the function that sizes it
   for the fit (`observation_card_h`, `story_h_with`, `soak_plan`, `dash_heights_with`, the hero
   block). The drawing code draws exactly that: a card's content ends inside its card
   (`Metrics::overdrawn` stays empty), and every text run lies inside some card's band.
6. **No title-only card.** A card is drawn with at least one body row or not at all.
7. **The stack.** Folded cards become one stack row under the hero: `N more · title · title`.
   Every folded card can be opened from it: `e` opens the first, `n` the next, a tap on a title
   opens that one, `e` or Esc closes. The key hint `e more` shows only when a stack is drawn.
8. **The accordion never grows the picture.** The open card takes the room of the stackable
   cards (they fold) and of the Context card down to its floor. Rows that still do not fit are
   cut, the last one ending in "…" or a "+N more" row (the performance card's failing checks).
9. **No overlapping text.** No two text runs share any area, measured by the advance each run
   is drawn with. A column of labels is as wide as its widest label. Every label a producer can
   emit is drawn by the suite: the should-be kinds come from `instrument::SHOULD_BE_KINDS`, the
   table the row reads, so a new kind is checked the day it is added.
10. **A new card registers.** Add it to `instrument::CARD_REGISTRY` with its tier and stack
    index, size it in the fit's `need`, and add a fixture that draws it to `tests/pane_contract.rs`.
    A card drawn but not registered, or registered but drawn by no fixture, fails the suite, as
    does any `panel` drawn that is not a registered card's region.
11. **Hover.** A hover state may change colour, shadow and a 1-2 pt rise inside the card's own
    box. It never changes any height, and it redraws only on enter and leave.
12. **The session mix rides the bar.** The Context card's donut (Tools / Context / Other, from
    the breakdown's parts only) and its one-word verdict sit on the parts bar's line and add no
    row. Tools or Context at half the session or more names it ("tool-heavy", "context-heavy");
    anything else, an Other-majority session included, is "mixed". No breakdown, no donut. A new
    session with no reading yet (rule 14) has no breakdown either: its donut reads the measured
    start parts alone (the text CSR injected is Other, the rest Context), and no measured part, no
    donut.
    The Lost and found handoff popup draws the same donut on its "Shape:" line for a thread's origin
    session (`session_mix_of`, same words and threshold), from the byte counts of `session_facts`'s one
    pass over the transcript; not measured (loading, no transcript, nothing in it) is "Shape: not measured"
    and no donut.
13. **The Context bar spans the auto-compact window.** The bar's full width is the window Claude
    Code compacts to (`CLAUDE_CODE_AUTO_COMPACT_WINDOW`, else `autoCompactWindow` in
    settings.json, read once per frame as `compactor::rules::native_window_of` reads it), its
    last 33k (`NATIVE_UNDER`; all of a smaller window) a hatched buffer with a tick at its start.
    The heading reads "85k of 300k (28%)" and "182k free · 33k buffer". No setting: the model's
    window, no buffer. No segment runs past the bar's end, even with the context past the window.
14. **A new session's Context card tells its start.** The card is never the quotas alone for a
    session with a name: with no context reading yet it is the dashboard (the limits as bars) and
    its Context section reads "no reading yet" over what the session started with, from real data
    only. The parts are CSR's SessionStart injection (the hook's own note, so it is known before
    the first prompt, when no transcript exists), then, once the transcript has the rows, the
    memory and CLAUDE.md files, the skill list and the system prompt, each sized from its
    characters (3 to a token) and marked est. A part nobody measured is named "unmeasured" in
    the note under the legend, never sized, and with nothing measured no bar segment and no donut
    is drawn. With the first reply, the live Context section takes over and one line under it
    reads "Started at <first reply's total>: <parts> · rest <what the parts do not explain>
    (est.)" until the session has more than three replies or compacts. Under it, one row each
    for what CSR handed the session: the recap's first clause, and "Lost and found: N open" with
    the newest thread's words; that row is the thread's handoff popup (`ObsCard::Lost`), as a
    monologue row is, until the Lost and found card is on the main view: that card says the threads
    and the start block does not say them twice. The "CSR handed over" row and the "Self-Reflect this
    session" row (one line or rings) open "What CSR told Claude" (`ObsCard::Handed`, `observe:4:0`).
    After a compact, or past the first replies, the start block is gone but CSR's text is not
    (`handed::item_of`: the transcript's SessionStart text, else the hook's note, else its recap
    clause only): "CSR handed over: <clause>" is still drawn above the Self-Reflect row, the first
    thing the Context card gives up after the rings (it goes with the "this turn" note and cache
    line). The `i` key keeps the figure's facts card under a card with no reading.
    The card is as tall as its content (the richest fit step): with no card under it to take the
    surplus (no "Last time here" help), the picture ends under the card and the rest of the pane
    is background, never a hollow tail.
15. **Type scale of the rows and the popups.** The dreamer card, the two lists and the Context card
    draw a third larger than the other cards: heading 22.5 pt (was 17), row 16.5 (was 12.5),
    caption 13.75 (was 10.5), on lines scaled the same (`theme::ROW_*`). The popups (`observation::POP_*`)
    use four levels: title 26, action keys 18.5, body 16.25, caption 13.65, so title > keys > body >
    caption. The action keys are the largest step (at least 1.4 times their old size) and are drawn as
    pills along the panel's foot, labels never cut: shorten their words (`cta_keys`), never the type.
    A row or a key label is one line: text that does not fit ends in "…" (a path keeps its file
    name), a long sentence becomes one-line items cut at its sentence and clause boundaries, and a
    popup that is too tall drops its lowest-priority lines first (where CSR keeps it, then later
    summary sentences, then the session's numbers). Three exceptions keep wrapping: the "In full"
    popup, whose only job is to show every word of a cut row, "What CSR told Claude" (below), and
    the Context card's headline (two lines at most, as before).
    "What CSR told Claude" says every word CSR put in front of Claude, so its rows wrap and the popup
    fills up to the pane's height and scrolls (`j`/`k`, up and down, three rows a press; the foot
    says "j/k for N more" only while rows are below). It keeps the `POP_*` levels: Lost and found
    bullets are body size with a gold dot, the briefing, earlier threads and notes on prompts are
    caption size with a quiet dot. Its foot pills are `r Resume` (the primary pill, filled in the
    gold accent), `c copy` and `esc close`; `r`, `s` and a tap on the pill type the Resume prompt
    into Claude's prompt, never Enter. The prompt uses the handoff grammar
    (`storage::handoff_marker`) so the importer knows it is CSR's, and carries every open Lost and
    found thread as a bullet. `tests/companion_type.rs` pins the sizes, the one-line rows and that
    every key is on the panel at 35x30, 35x24, 38x30 and 50x40.

16. **The first part of the Context legend says what it holds.** It is the floor: what the
    transcript's characters cannot explain (`breakdown::floor_name`). In a session that has never
    compacted it is "Setup" (system prompt, tools, CLAUDE.md, memory, skill list). Once a
    compaction is in the transcript it is "Summary + setup": the summary that replaced the old
    context is in no row, so it lands in the floor with the re-injected setup. Never "System": the
    floor is several times the system prompt and tools alone. A label is never cut mid-word; the
    legend goes to a second line when its parts do not fit the card's width (`legend_lines`).

## Same page?

Once the dreamer has read a session (`same_page` on a look's record, `same_page.rs`), the "What
confused the agent" card becomes one "Same page?" card: the 8-bit Claude, the numbers on a row of
their own, labelled as a model's read with no percentage because repeat readings differ, the
editorial on the rows left, at least two rows in all. It opens as an observation
(`observe:0:0`); `s` types the recap message into Claude's prompt, never pressing Enter. The pane
writes the numbers, the model writes the words, and a reading that does not add up is dropped.
With no reading the card stays the plain list. A Lost and found popup is the thread only: its
sentences, when it went quiet and what `s` types. The session's numbers and where CSR keeps it stay
in the draft. A thread the dreamer shows finished, by a quote the code finds in a newer summary,
leaves the card for "Guessed done" with that quote; a summary that lists it again brings it back.
