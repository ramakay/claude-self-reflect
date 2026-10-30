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
   agents bar, empty-session help. T2 folds into the stack: the two lists (Confused Claude,
   Should be a skill / memory / tool), the story (Where the work stands) and CSR performance.
4. **Collapse order.** Content gives way in `FIT_STEPS` order, first to go first: performance
   check rows, its clock and bar, list rows to 3, list rows to 1, the credit row, rings to one
   line, both lists fold, the story folds, performance folds; with a section open, the Context
   card to its floor, then the section to its least rows. A T2 card is never drawn while a card
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
    anything else, an Other-majority session included, is "mixed". No breakdown, no donut.
13. **The Context bar spans the auto-compact window.** The bar's full width is the window Claude
    Code compacts to (`CLAUDE_CODE_AUTO_COMPACT_WINDOW`, else `autoCompactWindow` in
    settings.json, read once per frame as `compactor::rules::native_window_of` reads it), its
    last 33k (`NATIVE_UNDER`; all of a smaller window) a hatched buffer with a tick at its start.
    The heading reads "85k of 300k (28%)" and "182k free · 33k buffer". No setting: the model's
    window, no buffer. No segment runs past the bar's end, even with the context past the window.
