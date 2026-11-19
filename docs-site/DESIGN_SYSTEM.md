# Self-Reflect Documentation Site: Design System

Matches the marketing site: warm paper, near-black ink, one vermilion accent, hairline rules, Georgia display type. Source of truth for tokens is `src/index.css` (`@theme`, `[data-theme="dark"]`, and the "Brand layer" block at the end of the file).

## Direction

- Editorial paper, not glass. Cards are flat, 1px hairline, 2px radius, no blur, no shadow.
- One accent (vermilion). Sage stays as the positive state colour only.
- Anatomy / hairline illustration style for figures: thin ink strokes on paper, vermilion for the one thing that matters.
- Banned: gradient text, neon glow, sky photo backgrounds, glassmorphism, handwritten fonts.
- Display name is "Self-Reflect". Package, repo and CLI names (`claude-self-reflect`, `csr-engine`) stay as they are.

## Type

| Role | Font | Notes |
|------|------|-------|
| Display / headings | Georgia, Times New Roman | weight 400, letter-spacing -0.035em |
| Body / UI | system UI sans (`ui-sans-serif, system-ui, ...`) | no web fonts are loaded |
| Code / metrics | `ui-monospace, SFMono-Regular, Menlo` | |

## Tokens

| Token | Light | Dark |
|-------|-------|------|
| `--paper` (page) | #f5f2eb | #17140f |
| `--paper-card` | #fdfbf6 | #211d18 |
| `--paper-rule` (hairline) | #d8d2c8 | #3a352d |
| `--color-ink` | #24211e | #f3eee4 |
| `--color-body` | #3d3934 | #d6d0c4 |
| `--color-muted` | #645f57 | #a39d92 |
| `--vermilion` / `--color-purple` (accent alias) | #c8442a | #ec7557 |
| `--color-sage` | #5f7a56 | #9dbb90 |

`--color-purple` and `--color-rose` keep their names so existing selectors work; they now hold vermilion tones.

## Logo and icons

Chosen mark: two facing profiles forming the S in a ring, R on the right.

| File | Use |
|------|-----|
| `src/components/BrandMark.jsx` | inline SVG, takes `currentColor`, used in nav |
| `public/favicon.svg` | ink tile, paper mark |
| `public/favicon-32.png`, `public/apple-touch-icon.png` (180), `public/icon-512.png` | raster favicons |
| `public/images/logo-icon.png` | 1024 tile |
| `public/brand/self-reflect-mark.svg` | master vector |
| `public/brand/self-reflect-mark-light-1024.png` | ink mark, transparent (for light backgrounds) |
| `public/brand/self-reflect-mark-dark-1024.png` | paper mark, transparent (for dark backgrounds) |
| `public/images/og-card.png` | 1200x630 social card |

## Layout

Landing: in-flow hero (Georgia, vermilion sub-line), then the 12-column bento grid. Doc pages: left nav, 62ch prose column. Mobile: single column, touch targets 44px.
