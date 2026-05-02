# Claude Self-Reflect Documentation Landing Page Design Proposal

## Direction

Claude Self-Reflect should feel like an intelligent archive rather than a software launch page. The landing page will combine newspaper editorial structure with frosted dashboard surfaces over a quiet sky field. The result should read as "institutional memory made visible": archival, calm, precise, and slightly tactile.

The brief's core aesthetic is strong. The refinement is to keep glassmorphism functional instead of decorative. Cards should look like clipped translucent sheets laid over a pale morning sky, with their content organized like newspaper modules: headlines, datelines, figures, captions, rules, and tiny charts. No gradient headlines, neon blooms, dark hero panels, pricing-table rhythm, or generic SaaS conversion blocks.

The hero message sits behind the dashboard as a large typographic atmosphere, not as a conventional foreground claim. The bento grid is the primary object. It obscures the headline enough that the page feels layered and discoverable while keeping the sentence legible in fragments:

Primary hero text: "You or your agent don't have to remember any of this"

Subtext: "because Claude Code does."

## Page Composition

The first viewport is a full-bleed sky gradient with a centered bento dashboard floating above it. The oversized hero headline is positioned behind the grid, spanning nearly the full viewport width. It uses low opacity, soft blending, and slight blur so it reads like ink behind vellum.

Desktop composition:

- Background: fixed full-page sky gradient with subtle cloud texture.
- Hero headline: absolute, behind cards, 8vw to 11vw, max 164px, heavy serif, opacity 0.16.
- Bento dashboard: 12-column grid, max-width 1180px, centered, top margin clamp(88px, 12vh, 148px).
- Cards: varied spans and heights, alternating dense editorial content with quiet post-it moments.
- Scroll continuation: second band repeats the editorial rhythm with documentation entry points and progressive disclosure, but no marketing hero reset.

## Typography Scale

Use `Newsreader` as the primary editorial serif. `Playfair Display` is acceptable if Newsreader is unavailable. Use `Inter` for body/UI and `JetBrains Mono` for metrics, labels, code, and chart axes.

- Display hero: Newsreader, 700, clamp(72px, 10vw, 164px), line-height 0.88, letter-spacing 0.
- Page eyebrow/dateline: JetBrains Mono, 500, 11px, line-height 1.2, uppercase, letter-spacing 0.08em.
- Card headline large: Newsreader, 700, 34px, line-height 0.95, letter-spacing 0.
- Card headline medium: Newsreader, 700, 24px, line-height 1.0, letter-spacing 0.
- Card headline small: Newsreader, 650, 19px, line-height 1.08, letter-spacing 0.
- Body: Inter, 450, 15px, line-height 1.55, letter-spacing 0.
- Body small: Inter, 450, 13px, line-height 1.45, letter-spacing 0.
- Caption: Inter, 500, 11px, line-height 1.35, letter-spacing 0.01em.
- Mono metric large: JetBrains Mono, 700, 32px, line-height 1.0, letter-spacing 0.
- Mono metric medium: JetBrains Mono, 700, 22px, line-height 1.0, letter-spacing 0.
- Mono label: JetBrains Mono, 500, 11px, line-height 1.3, letter-spacing 0.02em.
- Code/classified text: JetBrains Mono, 500, 14px, line-height 1.45, letter-spacing 0.

## Color System

CSS custom properties should define every color. Avoid one-hue domination by balancing muted purple with rose, sage, ink, and warm paper tones.

- `--sky-0: #e8e4f0` - page gradient start, top left.
- `--sky-1: #d4d0e8` - page gradient middle.
- `--sky-2: #c8cae0` - page gradient end, lower right.
- `--cloud: #f7f8fb` - soft cloud highlights and chart fills.
- `--glass: rgba(255, 255, 255, 0.80)` - primary card fill.
- `--glass-strong: rgba(255, 255, 255, 0.90)` - hover or active card fill.
- `--glass-border: rgba(255, 255, 255, 0.72)` - card border.
- `--glass-shadow: rgba(42, 38, 66, 0.16)` - large soft card shadow.
- `--ink: #1a1a2e` - headlines and high-importance text.
- `--body: #4a4a6a` - body copy.
- `--muted: #767692` - captions, axes, metadata.
- `--rule: rgba(26, 26, 46, 0.13)` - newspaper rules and separators.
- `--purple: #6b5b95` - primary accent, chart strokes, active dots.
- `--rose: #b5838d` - secondary accent, alerts, context-loss chart.
- `--sage: #7c9473` - success and privacy accents.
- `--amber-paper: #f6e6a9` - post-it note fill.
- `--rose-paper: #f3c9c3` - secondary post-it fill.
- `--sage-paper: #dce8cf` - quiet success note fill.
- `--paper-ink: #343145` - text on post-it notes.
- `--code-bg: rgba(26, 26, 46, 0.06)` - inline command field.

Usage rules:

- Headlines use `--ink`; never gradient-filled text.
- Body uses `--body`; captions and chart axes use `--muted`.
- Glass cards use `--glass` with `backdrop-filter: blur(18px) saturate(1.08)`.
- Chart strokes rotate between `--purple`, `--rose`, and `--sage`; no high-saturation cyan, magenta, or neon.
- Post-its use paper colors with low-contrast faded radial or linear washes, not luminous gradients.

## Bento Grid System

Use a 12-column CSS grid with 16px gaps at desktop, 14px at tablet, and 12px at mobile. Cards use stable min-heights so animation and hover states do not shift the layout.

Desktop card spans:

- Card 1, The Forgetting Problem: columns 1 / span 5, height 300px.
- Card 2, Active Memory: columns 6 / span 4, height 300px.
- Card 3, The Search: columns 10 / span 3, height 300px.
- Card 4, The Import: columns 1 / span 4, height 250px.
- Card 5, One Binary: columns 5 / span 3, height 250px.
- Card 6, The Pipeline: columns 8 / span 5, height 250px.
- Card 7, Privacy: columns 1 / span 3, height 210px.
- Card 8, Install: columns 4 / span 4, height 210px.
- Supporting docs entry card: columns 8 / span 5, height 210px if the implementation extends beyond the eight story cards.

The eight requested story cards are the mandatory visual set for the hero bento. Any additional docs entry card should appear below the first viewport or as a secondary card after the required eight.

## Card Specs

### 1. The Forgetting Problem

Layout: large horizontal clipping-style card with a faux newspaper column on the left and a chart on the right.

Typography:

- Dateline: `SESSION 04 / CONTEXT WINDOW`, mono label.
- Headline: "The Forgetting Problem", large serif.
- Subhead: "Claude amnesia compounds across disconnected sessions.", body small.

Visual treatment:

- Torn-paper top edge simulated with a subtle mask or clipped pseudo-element.
- Two-column newspaper text texture using short abstract line blocks, not lorem ipsum.
- Thin black editorial rules with `--rule`.

Data shown:

- Micro-chart: "context lost per session" as a descending retained-context area or rising loss line.
- X labels: S1, S2, S3, S4, S5.
- Y callout: "memory drift +42%" in rose.

Interaction:

- Hover lifts 4px, chart line draws from 0 to 100%, clipping shadow deepens.

### 2. Active Memory

Layout: medium vertical card with a newspaper dateline top, then a six-node hook timeline.

Typography:

- Dateline: `DAILY EDITION / ACTIVE MEMORY`, mono label.
- Headline: "Active Memory", medium serif.
- Metric: "12,000/day", mono metric medium.
- Caption: "6 hooks fire before, during, and after work."

Visual treatment:

- Timeline runs vertically with six pulsing dots.
- Each node has a compact label: PreToolUse, PostToolUse, UserPromptSubmit, Stop, SubagentStop, Notification.
- Dots alternate purple and sage.

Data shown:

- Main figure: "6 hooks fire 12,000 times/day".
- Tiny tick marks show morning, midday, evening density.

Interaction:

- Dots pulse sequentially on card entry, then settle into a 3.6s low-opacity heartbeat.

### 3. The Search

Layout: narrow high-impact card with query input at top, result strips in middle, latency chart at bottom.

Typography:

- Headline: "The Search", small serif.
- Query: `"auth bug from last week"` in mono.
- Metric: "<1ms", mono metric large.
- Caption: "local vector lookup"

Visual treatment:

- Query arrow is rendered as a thin rule ending in a dot, not a chunky icon.
- Result rows look like translucent index cards.

Data shown:

- Sparkline of search latency over time: 0.9ms, 0.7ms, 0.8ms, 0.6ms, 0.7ms.
- Callout: "p95 0.82ms".

Interaction:

- Query field receives a soft focus ring on hover.
- Sparkline draws on scroll entry with `stroke-dashoffset`.

### 4. The Import

Layout: progress report card with a strong metric pair and mini bar chart.

Typography:

- Dateline: `ARCHIVE IMPORT / LOCAL ONLY`, mono label.
- Headline: "The Import", medium serif.
- Metrics: "1,107 conversations" and "15,745 chunks", mono medium.

Visual treatment:

- Top third reads like an archive ledger.
- Bottom third contains four vertical bars with labels: parse, chunk, embed, index.
- Progress rail uses muted purple fill over pale paper.

Data shown:

- Conversation count: 1,107.
- Chunk count: 15,745.
- Bars: parse 100%, chunk 100%, embed 92%, index 100%.

Interaction:

- Progress rail fills from 0 to final width during scroll entry.
- Bars grow with a 60ms internal stagger.

### 5. One Binary

Layout: compact comparison card with a 44MB circle on the left and competitor stack on the right.

Typography:

- Headline: "One Binary", small serif.
- Main metric: "44MB", mono metric large.
- Caption: "ship it as a single artifact"

Visual treatment:

- The 44MB circle is a quiet ink stamp, not a shiny badge.
- Competitor stack appears as three offset post-it strips labeled Docker, Python, DB.
- A small note reads "less to remember before memory can work".

Data shown:

- CSR: 44MB.
- Competitor stack: Docker + Python + DB, shown as layered blocks.

Interaction:

- Hover separates the competitor stack by 3px per layer.
- Circle gains a subtle paper-grain overlay.

### 6. The Pipeline

Layout: wide infographic card with three enrichment layers left-to-right.

Typography:

- Dateline: `ENRICHMENT PIPELINE / QUALITY REPORT`, mono label.
- Headline: "The Pipeline", medium serif.
- Layer labels: L1 raw, L2 contextualized, L3 reflective.

Visual treatment:

- Newspaper infographic style: numbered circles, connecting rules, captioned quality scores.
- Each layer is a frosted mini-panel inside the card.
- Use sage for improvement, purple for flow, rose only for the low starting score.

Data shown:

- Quality scores: 0.074 -> 0.345 -> 0.691.
- Mini bars under each score, proportional to score.
- Caption: "reflection turns fragments into retrievable memory."

Interaction:

- Scores count up on entry.
- Connecting rules animate left-to-right over 420ms.

### 7. Privacy

Layout: quiet post-it-forward card with a lock icon and one sentence.

Typography:

- Headline: "Privacy", small serif.
- Main line: "Zero network connections.", Inter 700, 24px.
- Caption: "memory stays on the machine"

Visual treatment:

- A sage post-it sits slightly rotated on top of the glass card.
- Lock icon is a simple line icon in sage ink.
- Background content is intentionally sparse for contrast against dense cards.

Data shown:

- Boolean proof point: zero network connections.
- Optional tiny status line: `127.0.0.1 only`.

Interaction:

- Post-it eases into alignment by 1deg on hover.
- Lock stroke draws once on scroll entry.

### 8. Install

Layout: classified advertisement card with command as the central object.

Typography:

- Dateline: `CLASSIFIEDS / TOOLS`, mono label.
- Headline: "Install", medium serif.
- Classified line: "curl | sh. Done.", Newsreader 700, 28px.
- Command: `curl -fsSL ... | sh`, JetBrains Mono 14px.

Visual treatment:

- Thin newspaper box border, small column rules, and condensed classified spacing.
- Command sits in a frosted code strip.
- Small tear-off tabs along the bottom can say "local", "fast", "search", "hooks".

Data shown:

- Install promise: one command.
- Optional time marker: "<60 sec setup" in mono.

Interaction:

- Hover reveals copy affordance and a checkmark state after click.
- Tear-off tabs lift sequentially on card entry.

## Animation Choreography

Use CSS transitions and keyframes for the default implementation. Add Framer Motion only if the docs site already depends on React animation or needs route-aware motion. Pure CSS is enough for the landing page because the animations are deterministic, scroll-triggered, and mostly one-shot.

Entry trigger:

- Cards start at `opacity: 0`, `transform: translateY(18px) scale(0.985)`, `filter: blur(6px)`.
- On `.is-visible`, transition to `opacity: 1`, `transform: translateY(0) scale(1)`, `filter: blur(0)`.
- Transition: `opacity 520ms ease-out, transform 620ms cubic-bezier(0.16, 1, 0.3, 1), filter 620ms ease-out`.

Card stagger:

- Card 1: 0ms.
- Card 2: 80ms.
- Card 3: 160ms.
- Card 4: 240ms.
- Card 5: 320ms.
- Card 6: 400ms.
- Card 7: 480ms.
- Card 8: 560ms.

Hero choreography:

- Sky background fades in over 700ms.
- Oversized hero text starts at `opacity: 0`, `transform: translateY(10px)`, then settles to `opacity: 0.16` over 900ms.
- Bento dashboard begins 180ms after hero text starts so the cards visibly pass in front of the faded statement.

Micro-animation details:

- Sparklines: `stroke-dasharray` and `stroke-dashoffset`, 700ms ease-out, delay equals card delay + 180ms.
- Bar charts: transform scaleY from 0 to 1, transform-origin bottom, 520ms ease-out.
- Hook dots: initial sequence delay of 90ms per node; idle pulse `3.6s ease-in-out infinite`.
- Post-its: entry transform `rotate(-2deg) translateY(10px)` to `rotate(-1deg) translateY(0)`, 680ms cubic-bezier(0.16, 1, 0.3, 1).
- Progressive disclosure: secondary copy and captions fade in only after the card shell and main metric are visible.

Respect `prefers-reduced-motion`:

- Disable continuous pulses and cursor trails.
- Replace transforms with opacity-only fades of 180ms.
- Keep all content visible without scroll-trigger dependency.

## Interaction Patterns

Hover states:

- Cards lift from `translateY(0)` to `translateY(-4px)`.
- Background fill moves from `--glass` to `--glass-strong`.
- Border changes from `--glass-border` to `rgba(255,255,255,0.9)`.
- Shadow changes from `0 20px 60px var(--glass-shadow)` to `0 26px 72px rgba(42, 38, 66, 0.20)`.
- Microcharts reveal precise value captions on hover, but never hide the baseline story.

Scroll triggers:

- Use `IntersectionObserver` with threshold 0.18 and root margin `0px 0px -8% 0px`.
- Add `.is-visible` once; do not remove it when scrolling back up.
- Use a separate `.is-disclosed` class for secondary captions when a card reaches threshold 0.45.

Custom cursor:

- Desktop only, pointer-capable devices only.
- Cursor is a small ink dot with a trailing memory path made of 6 to 10 fading points.
- Over post-it cards, the trail briefly changes to paper flecks using `--amber-paper` at low opacity.
- Over charts, the cursor switches to a fine crosshair dot and shows the nearest micro-value in a small mono tooltip.
- Disable custom cursor below 900px width and for `prefers-reduced-motion`.

Click and focus:

- Install command supports copy with an accessible button and `aria-live` confirmation.
- All interactive cards maintain visible focus rings: `0 0 0 3px rgba(107, 91, 149, 0.22)`.
- Cards that link to docs use the whole-card link pattern with a visible nested "Read" affordance, but chart hover controls must not conflict with the link target.

## Responsive Strategy

Desktop, 1100px and up:

- 12 columns.
- Full bento collage in the first viewport.
- Hero text behind the grid, two lines, max 164px.

Tablet, 760px to 1099px:

- 8 columns.
- Cards collapse to two-column rhythm.
- Cards 1 and 6 span full width.
- Cards 2, 3, 4, 5, 7, and 8 span 4 columns.
- Hero text remains behind the grid but reduces to clamp(58px, 11vw, 104px).

Mobile, below 760px:

- 1 column.
- Cards stack in story order: Forgetting, Active Memory, Search, Import, One Binary, Pipeline, Privacy, Install.
- Hero text sits behind the first two cards, very large but clipped by viewport width: clamp(48px, 17vw, 72px).
- Glass blur reduces to 12px for performance.
- Custom cursor is disabled.
- Dense charts simplify to sparklines and one-line metrics.
- Post-it rotations reduce to 0.5deg or 0deg to avoid cramped edges.

Very small screens, below 380px:

- Card padding decreases from 22px to 16px.
- Mono metrics use medium scale, not large.
- Classified command wraps in two lines with `overflow-wrap: anywhere`.

## Implementation Notes

CSS custom property scaffold:

```css
:root {
  --sky-0: #e8e4f0;
  --sky-1: #d4d0e8;
  --sky-2: #c8cae0;
  --glass: rgba(255, 255, 255, 0.80);
  --glass-strong: rgba(255, 255, 255, 0.90);
  --glass-border: rgba(255, 255, 255, 0.72);
  --ink: #1a1a2e;
  --body: #4a4a6a;
  --muted: #767692;
  --purple: #6b5b95;
  --rose: #b5838d;
  --sage: #7c9473;
}
```

Recommended implementation:

- Use semantic HTML sections with each bento card as an `article`.
- Render microcharts as inline SVG for crisp lines and easy stroke animation.
- Use CSS grid for the bento layout; avoid Masonry libraries.
- Use `IntersectionObserver` for entry and disclosure classes.
- Use pure CSS animations first. Framer Motion is optional only if already present.
- Use `lucide-react` or existing icon library for lock/copy icons if the project already includes it; otherwise inline a small accessible SVG lock to avoid adding a dependency.
- Load fonts with `font-display: swap`; use local fallbacks `Georgia`, `ui-serif`, `system-ui`, and `ui-monospace`.
- Keep the cloud background as CSS gradients plus a subtle noise texture or generated image. Do not use dark gradient panels.

Performance guardrails:

- Cap backdrop blur at 18px desktop, 12px mobile.
- Avoid animating `box-shadow` continuously.
- Animate transform and opacity wherever possible.
- Keep SVG chart paths short and static.
- Cursor trails should use a single fixed overlay element, not many DOM nodes per frame.

Accessibility guardrails:

- Maintain contrast for all text on glass by using the specified ink and body colors.
- Do not encode data only by color; include labels or values beside every chart.
- Respect keyboard focus and reduced motion.
- Keep post-it rotations decorative only, never required for reading order.

