# Docs-site brand refresh (10.1, branch docs/brand-refresh)

## Inventory (found at start)

Docs content (all `src/content/`): architecture, btw-these-too, cli, configuration, dev-setup, enrichment, features, hooks, installation, mcp-tools, privacy, quickstart, report-card, search-strategies, search, structure, troubleshooting, upgrading (+ why-csr via index.js), `index.js` (nav + page map). Pages: `src/pages/Landing.jsx`, `DocPage.jsx`; components `Layout.jsx`, `BrandMark.jsx`.

Brand marks already on the new mark when I started (10.1.0 release commit): `BrandMark.jsx` (inline SVG), `public/favicon.svg`, `public/apple-touch-icon.png`, `images/logo-icon.png`, `images/og-card.png`.

Still old or off-brand at the start:
- `og-card.png`: new mark, but purple/navy palette and the line "Cross-session memory for Claude Code" (memory framing).
- `index.html`: Google Fonts (Inter, Newsreader, JetBrains Mono, Caveat); og:image was a relative path; no twitter card, no 32px or 512px favicon.
- `Landing.jsx`: unused `LogoPreviews` component (old logo concepts incl. the mirror-book).
- `index.css`: sky-photo + glassmorphism theme, purple/rose accent.

Embedded raster graphics (used by docs/README): `images/card-04-paper-{light,dark}.png` (README), `hooks-lifecycle-2.png`, `hooks-diagram.png`, `arch-diagram.png`, `project-structure-2.png`, `search-scoring-pipeline.png` (content pages), `sky-bg*.webp` (old landing background, no longer referenced after this refresh), `csr-demo.gif` (README).
Unreferenced old art (kept, not deleted): `readme-hero-light.png` ("Claude Self-Reflect / Perfect memory", 8 MB), `hero-brain.png`, `card-01..03-*`, `card-*-dark*.png`, `design-comp*.png`, `*-comp.png`, `sky-bg.png`, `sky-clean-*.png`, `enrichment-diagram-*`, `hooks-lifecycle-1`, `hooks-timeline`, `how-it-works`, `search-demo`, `project-structure-1`, `architecture-*`, `*.json` card specs.
`generate-cards.mjs` outputs card PNGs through the OpenAI images API (needs a key); not run.

## Changes

Files: `src/index.css`, `src/pages/Landing.jsx`, `src/components/` (colour literals), `index.html`, `DESIGN_SYSTEM.md`, `public/favicon.svg`, `public/favicon-32.png` (new), `public/icon-512.png` (new), `public/apple-touch-icon.png`, `public/brand/*` (new), `public/images/logo-icon.png`, `public/images/og-card.png`.

Assets from the chosen masters (`.plans/brand/self-reflect/`): SVG master, light and dark transparent 1024 marks, favicon svg + 32/180/512 png, 1024 tile, og card 1200x630 (paper, ink, vermilion rule, "Claude reflecting on its own work."). File paths kept stable.

Theme tokens before -> after: page bg sky photo + lavender gradient -> #f5f2eb (dark #17140f); card glass rgba(255,255,255,.8) -> #fdfbf6 (dark #211d18); ink #1a1a2e -> #24211e (dark #f3eee4); accent purple #6b5b95 -> vermilion #c8442a (dark #ec7557); rose #b5838d -> #a0432f; rule rgba(26,26,46,.13) -> #d8d2c8 (dark #3a352d); fonts Newsreader/Inter/JetBrains Mono/Caveat -> Georgia / system sans / system mono (no web fonts loaded). Bento cards kept, restyled flat with hairline, 2px radius, no blur. Hero moved from an absolute watermark behind the cards to in-flow above them.

Display name: no "Claude Self-Reflect" in display copy was found in src/ (remaining hits are `claude-self-reflect` CLI/package/project names, left as is).

## Not done / for the lead
- Content-page diagrams (`hooks-lifecycle-2`, `hooks-diagram`, `arch-diagram`, `project-structure-2`, `search-scoring-pipeline`) and `card-04-paper-*` (README) are AI-rendered with the old navy/purple palette. They carry no logo. Regenerate with the paper/vermilion style (needs image-gen); I only gave them a hairline frame.
- Dead CSS for `.logo-previews/.logo-option` remains in `index.css` (component removed). Dozens of per-card rules in the old glass layer are overridden by the brand layer rather than deleted.
- Old unreferenced art listed above, `readme-hero-light.png` in particular, should be deleted or regenerated.

## Stale or doubtful numbers (not changed)
- Landing "Import" card: "1,107 conversations / 15,745 chunks" with chart data is labelled "Sample data" in the UI; confirm that is intended for the release.
- Memory footprint is "~138 MB" on landing/architecture/installation but the old `readme-hero-light.png` says "44MB" (unreferenced).
- Landing chart arrays with versions like 1.2.8/2.7.6 are illustrative data.

## Verification
Dev server on 4194 (headless agent-browser). Shots in `.brand-shots/` (git-excluded): before-* (landing + features, hooks, installation, light/dark, 1440x900 and 390x844 full page) and after1..after5 (landing top/mid, content pages). Two look-fix rounds: round 1 found the hero title colliding with the cards (fixed by moving it in flow) and a transform-override from the old hero animation (fixed); round 2 checked landing and three content pages in both themes on desktop and mobile.
`npm run build`: passes.
