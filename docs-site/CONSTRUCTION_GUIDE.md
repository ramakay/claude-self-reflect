# Claude Self-Reflect Bento Landing Page Construction Guide

This guide is the implementation contract for rebuilding the documentation landing page as a React + Vite + Tailwind site deployed to GitHub Pages.

Primary references:

- `docs-site/DESIGN_PROPOSAL.md`
- `docs-site/public/images/design-comp.png`
- `docs-site/public/images/card-detail.png`
- Current prototype: `docs-site/src/pages/Landing.jsx`
- Generated inner page comp: `docs-site/public/images/inner-page-comp.png`

The target is an editorial glassmorphic bento page: pale cloud sky, translucent paper cards, newspaper rules, serif headlines, mono datelines, compact data graphics, and a large faded hero statement behind the grid. Do not turn this into a conventional SaaS landing page.

## Stack And Deployment

Use the existing stack:

- React 19
- Vite 6
- Tailwind CSS 4 via `@tailwindcss/vite`
- `react-router-dom` for internal links
- `lucide-react` is already installed and can be used for Lock, Copy, Search, BookOpen, Boxes, Terminal, and ArrowRight icons.

The existing `vite.config.js` already has the GitHub Pages base path:

```js
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: '/claude-self-reflect/',
  build: {
    outDir: 'dist',
  },
})
```

Build with:

```bash
cd docs-site
npm run build
```

For GitHub Pages, publish `docs-site/dist` from a GitHub Actions workflow or the Pages deployment settings. Keep the Vite `base` value above or image and route URLs will be wrong on `https://<owner>.github.io/claude-self-reflect/`.

## Global CSS Tokens

Use Tailwind v4 tokens in `src/index.css`. The current file already defines most of these, but the landing needs slightly stronger glass borders, richer sky layering, and exact animation names.

```css
@import "tailwindcss";

@theme {
  --font-serif: 'Newsreader', Georgia, 'Times New Roman', serif;
  --font-sans: 'Inter', system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
  --font-mono: 'JetBrains Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  --font-hand: 'Caveat', cursive;

  --color-sky-0: #e8e4f0;
  --color-sky-1: #d4d0e8;
  --color-sky-2: #c8cae0;
  --color-cloud: #f7f8fb;
  --color-glass: rgba(255, 255, 255, 0.80);
  --color-glass-strong: rgba(255, 255, 255, 0.90);
  --color-glass-border: rgba(255, 255, 255, 0.72);
  --color-glass-shadow: rgba(42, 38, 66, 0.16);
  --color-ink: #1a1a2e;
  --color-body: #4a4a6a;
  --color-muted: #767692;
  --color-rule: rgba(26, 26, 46, 0.13);
  --color-purple: #6b5b95;
  --color-rose: #b5838d;
  --color-sage: #7c9473;
  --color-amber-paper: #f6e6a9;
  --color-rose-paper: #f3c9c3;
  --color-sage-paper: #dce8cf;
  --color-paper-ink: #343145;
  --color-code-bg: rgba(26, 26, 46, 0.06);
}
```

Load fonts in `index.html`:

```html
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Caveat:wght@400;600&family=Inter:wght@400;450;500;600;700&family=JetBrains+Mono:wght@500;700&family=Newsreader:opsz,wght@6..72,650;6..72,700&display=swap" rel="stylesheet">
```

## Cloud Sky Background

Use CSS gradients plus a noise overlay. Do not use `design-comp.png` as a page background; it is a reference, not a production asset.

Exact base CSS:

```css
html {
  scroll-behavior: smooth;
}

body {
  min-height: 100vh;
  margin: 0;
  font-family: var(--font-sans);
  color: var(--color-body);
  background:
    radial-gradient(circle at 78% 16%, rgba(255, 255, 255, 0.78) 0 8%, rgba(255, 255, 255, 0.34) 12%, transparent 28%),
    radial-gradient(ellipse at 22% 38%, rgba(255, 248, 236, 0.58) 0 13%, rgba(255, 255, 255, 0.28) 26%, transparent 48%),
    radial-gradient(ellipse at 82% 68%, rgba(255, 255, 255, 0.48) 0 10%, transparent 34%),
    linear-gradient(145deg, #edf0f7 0%, #e8e4f0 26%, #d8d4e8 52%, #c8cae0 100%);
  background-attachment: fixed;
}

.sky-shell {
  position: relative;
  min-height: 100vh;
  overflow: hidden;
  isolation: isolate;
}

.sky-shell::before {
  content: "";
  position: fixed;
  inset: 0;
  z-index: -2;
  pointer-events: none;
  opacity: 0.13;
  mix-blend-mode: overlay;
  background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='160' height='160' viewBox='0 0 160 160'%3E%3Cfilter id='n'%3E%3CfeTurbulence type='fractalNoise' baseFrequency='.86' numOctaves='3' stitchTiles='stitch'/%3E%3C/filter%3E%3Crect width='160' height='160' filter='url(%23n)' opacity='.42'/%3E%3C/svg%3E");
}

.sky-shell::after {
  content: "";
  position: fixed;
  inset: -12vh -8vw;
  z-index: -1;
  pointer-events: none;
  background:
    radial-gradient(ellipse at 50% 26%, rgba(255, 255, 255, 0.40) 0 16%, transparent 46%),
    radial-gradient(ellipse at 14% 82%, rgba(255, 255, 255, 0.28) 0 10%, transparent 36%);
  filter: blur(28px);
  opacity: 0.86;
  mix-blend-mode: screen;
}
```

Atmospheric haze technique:

- The base background uses three cloud-like radial gradients over one sky linear gradient.
- The `::before` pseudo-element adds low-opacity SVG turbulence noise. Keep opacity between `0.10` and `0.16`.
- The `::after` pseudo-element adds large blurred white radial gradients with `mix-blend-mode: screen`. This gives the cards the same washed morning haze as the comps.
- Avoid high-contrast cloud photos behind text. The sky should add depth while staying quiet behind the glass.

## Typography Utilities

Use these exact utility classes or equivalent Tailwind classes.

```css
.type-dateline {
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 500;
  line-height: 1.2;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--color-muted);
}

.type-card-headline-lg {
  font-family: var(--font-serif);
  font-size: 34px;
  font-weight: 700;
  line-height: 0.95;
  letter-spacing: 0;
  color: var(--color-ink);
}

.type-card-headline-md {
  font-family: var(--font-serif);
  font-size: 24px;
  font-weight: 700;
  line-height: 1;
  letter-spacing: 0;
  color: var(--color-ink);
}

.type-card-headline-sm {
  font-family: var(--font-serif);
  font-size: 19px;
  font-weight: 650;
  line-height: 1.08;
  letter-spacing: 0;
  color: var(--color-ink);
}

.type-body {
  font-family: var(--font-sans);
  font-size: 15px;
  font-weight: 450;
  line-height: 1.55;
  letter-spacing: 0;
  color: var(--color-body);
}

.type-body-sm {
  font-family: var(--font-sans);
  font-size: 13px;
  font-weight: 450;
  line-height: 1.45;
  letter-spacing: 0;
  color: var(--color-body);
}

.type-caption {
  font-family: var(--font-sans);
  font-size: 11px;
  font-weight: 500;
  line-height: 1.35;
  letter-spacing: 0.01em;
  color: var(--color-muted);
}

.type-metric-lg {
  font-family: var(--font-mono);
  font-size: 32px;
  font-weight: 700;
  line-height: 1;
  letter-spacing: 0;
  color: var(--color-ink);
}

.type-metric-md {
  font-family: var(--font-mono);
  font-size: 22px;
  font-weight: 700;
  line-height: 1;
  letter-spacing: 0;
  color: var(--color-ink);
}

.type-code {
  font-family: var(--font-mono);
  font-size: 14px;
  font-weight: 500;
  line-height: 1.45;
  letter-spacing: 0;
}
```

## Glass And Animation Primitives

Every bento card starts from the same frosted-paper base.

```css
.bento-card {
  position: relative;
  overflow: hidden;
  padding: 22px;
  border-radius: 18px;
  background: rgba(255, 255, 255, 0.80);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow:
    0 20px 60px rgba(42, 38, 66, 0.16),
    0 2px 8px rgba(42, 38, 66, 0.05),
    inset 0 1px 0 rgba(255, 255, 255, 0.82);
  transition:
    background 260ms ease,
    border-color 260ms ease,
    transform 360ms cubic-bezier(0.16, 1, 0.3, 1),
    box-shadow 360ms ease;
}

.bento-card:hover {
  transform: translateY(-4px);
  background: rgba(255, 255, 255, 0.90);
  border-color: rgba(255, 255, 255, 0.90);
  box-shadow:
    0 26px 72px rgba(42, 38, 66, 0.20),
    0 3px 12px rgba(42, 38, 66, 0.06),
    inset 0 1px 0 rgba(255, 255, 255, 0.90);
}

.rule {
  border-top: 1px solid rgba(26, 26, 46, 0.13);
}

.rule-double {
  border-top: 3px double rgba(26, 26, 46, 0.28);
}

.section-number {
  font-family: var(--font-mono);
  font-size: 16px;
  font-weight: 500;
  line-height: 1;
  letter-spacing: 0.08em;
  color: var(--color-purple);
}

.post-it {
  position: absolute;
  z-index: 4;
  border-radius: 4px;
  padding: 12px 14px;
  color: var(--color-paper-ink);
  font-family: var(--font-hand);
  font-size: 18px;
  line-height: 1.05;
  box-shadow:
    0 14px 28px rgba(42, 38, 66, 0.12),
    0 2px 6px rgba(42, 38, 66, 0.08);
}

.post-it::before {
  content: "";
  position: absolute;
  top: -9px;
  left: 50%;
  width: 54px;
  height: 18px;
  transform: translateX(-50%) rotate(-2deg);
  background: rgba(240, 228, 190, 0.76);
  box-shadow: 0 2px 4px rgba(42, 38, 66, 0.08);
}

.chart-path {
  stroke-dasharray: 1;
  stroke-dashoffset: 1;
}

.is-visible .chart-path {
  animation-name: drawPath;
  animation-duration: 700ms;
  animation-timing-function: ease-out;
  animation-fill-mode: forwards;
}

.is-visible .bento-shell {
  animation-name: cardRise;
  animation-duration: 620ms;
  animation-timing-function: cubic-bezier(0.16, 1, 0.3, 1);
  animation-fill-mode: both;
}

@keyframes cardRise {
  from {
    opacity: 0;
    transform: translateY(18px) scale(0.985);
    filter: blur(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0) scale(1);
    filter: blur(0);
  }
}

@keyframes heroFade {
  from {
    opacity: 0;
    transform: translate(-50%, 10px);
  }
  to {
    opacity: 1;
    transform: translate(-50%, 0);
  }
}

@keyframes drawPath {
  to {
    stroke-dashoffset: 0;
  }
}

@keyframes growX {
  from {
    transform: scaleX(0);
  }
  to {
    transform: scaleX(1);
  }
}

@keyframes growY {
  from {
    transform: scaleY(0);
  }
  to {
    transform: scaleY(1);
  }
}

@keyframes hookPulse {
  0%, 100% {
    opacity: 0.58;
    transform: scale(1);
  }
  50% {
    opacity: 1;
    transform: scale(1.34);
  }
}

@keyframes postItIn {
  from {
    opacity: 0;
    transform: rotate(var(--from-rot, -2deg)) translateY(10px);
  }
  to {
    opacity: 1;
    transform: rotate(var(--rot, -1deg)) translateY(0);
  }
}

@keyframes connectorDraw {
  from {
    stroke-dashoffset: 1;
  }
  to {
    stroke-dashoffset: 0;
  }
}

@keyframes scorePop {
  from {
    opacity: 0;
    transform: translateY(6px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes lockDraw {
  to {
    stroke-dashoffset: 0;
  }
}

@keyframes tabLift {
  from {
    opacity: 0;
    transform: translateY(8px);
  }
  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@media (prefers-reduced-motion: reduce) {
  *,
  *::before,
  *::after {
    animation-duration: 1ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 1ms !important;
    scroll-behavior: auto !important;
  }

  .chart-path {
    stroke-dashoffset: 0;
  }
}
```

## Navigation Bar

Desktop nav in the comp is quiet and archival, not app-like. Exact treatment:

```css
.site-nav {
  position: sticky;
  top: 0;
  z-index: 50;
  height: 72px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.68);
  background: rgba(247, 248, 251, 0.58);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
}

.site-nav__inner {
  max-width: 1180px;
  height: 72px;
  margin: 0 auto;
  padding: 0 24px;
  display: flex;
  align-items: center;
  gap: 34px;
}

.site-nav__brand {
  display: inline-flex;
  align-items: center;
  gap: 12px;
  color: var(--color-ink);
  text-decoration: none;
}

.site-nav__mark {
  width: 34px;
  height: 34px;
  color: var(--color-purple);
}

.site-nav__name {
  font-family: var(--font-serif);
  font-size: 22px;
  font-weight: 700;
  line-height: 1;
  color: var(--color-ink);
}

.site-nav__tag {
  display: block;
  margin-top: 3px;
  font-family: var(--font-sans);
  font-size: 12px;
  font-weight: 450;
  line-height: 1.2;
  color: var(--color-muted);
}

.site-nav__links {
  display: flex;
  align-items: center;
  gap: 34px;
  margin-left: auto;
}

.site-nav__link {
  font-family: var(--font-serif);
  font-size: 15px;
  font-weight: 650;
  line-height: 1;
  color: var(--color-ink);
  text-decoration: none;
  opacity: 0.86;
  transition: opacity 180ms ease, color 180ms ease;
}

.site-nav__link:hover {
  color: var(--color-purple);
  opacity: 1;
}

.site-nav__github {
  min-height: 38px;
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 0 16px;
  border: 1px solid rgba(26, 26, 46, 0.20);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.34);
  color: var(--color-ink);
  font-family: var(--font-serif);
  font-size: 14px;
  font-weight: 650;
  text-decoration: none;
}
```

JSX:

```jsx
function NavBar() {
  return (
    <header className="site-nav">
      <div className="site-nav__inner">
        <Link className="site-nav__brand" to="/">
          <Brain className="site-nav__mark" aria-hidden="true" />
          <span>
            <span className="site-nav__name">Claude Self-Reflect</span>
            <span className="site-nav__tag">Memory for Claude Code.</span>
          </span>
        </Link>
        <nav className="site-nav__links" aria-label="Primary">
          <Link className="site-nav__link" to="/docs/why-csr">Guide</Link>
          <Link className="site-nav__link" to="/docs/mcp-tools">Reference</Link>
          <Link className="site-nav__link" to="/docs/architecture">Architecture</Link>
          <Link className="site-nav__link" to="/docs/troubleshooting">FAQ</Link>
          <Link className="site-nav__link" to="/docs/upgrading">Changelog</Link>
          <a className="site-nav__github" href="https://github.com/ramakay/claude-self-reflect">
            <Github size={18} aria-hidden="true" />
            View on GitHub
          </a>
        </nav>
      </div>
    </header>
  )
}
```

At `max-width: 768px`, set nav height to `60px`, hide the middle links, keep brand and GitHub. At `max-width: 480px`, hide GitHub text and show only the icon.

## Hero Text Treatment

The hero statement is a background object, not foreground marketing copy. It must sit behind the bento cards.

Exact CSS:

```css
.hero-watermark {
  position: absolute;
  z-index: 0;
  top: clamp(86px, 10vh, 128px);
  left: 50%;
  width: min(94vw, 1320px);
  transform: translateX(-50%);
  pointer-events: none;
  user-select: none;
  animation: heroFade 900ms ease-out 0ms both;
}

.hero-watermark__title {
  margin: 0;
  max-width: 1120px;
  font-family: var(--font-serif);
  font-size: clamp(72px, 10vw, 164px);
  font-weight: 700;
  line-height: 0.88;
  letter-spacing: 0;
  color: rgba(42, 42, 62, 0.19);
  text-wrap: balance;
  filter: blur(0.15px);
  mix-blend-mode: multiply;
}

.hero-watermark__subtext {
  margin: 12px 0 0 min(52vw, 720px);
  font-family: var(--font-serif);
  font-size: clamp(22px, 2.6vw, 42px);
  font-style: normal;
  font-weight: 650;
  line-height: 1;
  letter-spacing: 0;
  color: rgba(42, 42, 62, 0.40);
  filter: blur(0.1px);
}
```

JSX:

```jsx
function HeroWatermark() {
  return (
    <div className="hero-watermark" aria-hidden="true">
      <h1 className="hero-watermark__title">
        You or your agent don't have to remember any of this
      </h1>
      <p className="hero-watermark__subtext">because Claude Code does.</p>
    </div>
  )
}
```

## Overall Grid

Use a fixed 12-column CSS grid at desktop, with stable row heights matching the proposal. The current `Landing.jsx` prototype uses a different row-span structure and includes a ninth MCP tools card; the construction target below is the canonical eight-card hero bento.

```css
.landing-main {
  position: relative;
  z-index: 1;
  max-width: 1180px;
  margin: 0 auto;
  padding: clamp(88px, 12vh, 148px) 24px 76px;
}

.landing-dateline {
  margin-bottom: 18px;
}

.bento-grid {
  display: grid;
  grid-template-columns: repeat(12, minmax(0, 1fr));
  grid-template-rows: 300px 250px 210px;
  gap: 16px;
  align-items: stretch;
}

.card--forgetting { grid-column: 1 / span 5; grid-row: 1; }
.card--active { grid-column: 6 / span 4; grid-row: 1; }
.card--search { grid-column: 10 / span 3; grid-row: 1; }
.card--import { grid-column: 1 / span 4; grid-row: 2; }
.card--binary { grid-column: 5 / span 3; grid-row: 2; }
.card--pipeline { grid-column: 8 / span 5; grid-row: 2; }
.card--privacy { grid-column: 1 / span 3; grid-row: 3; }
.card--install { grid-column: 4 / span 4; grid-row: 3; }

@media (max-width: 768px) {
  .landing-main {
    padding: 96px 18px 64px;
  }

  .bento-grid {
    grid-template-columns: repeat(8, minmax(0, 1fr));
    grid-template-rows: none;
    grid-auto-rows: minmax(220px, auto);
    gap: 14px;
  }

  .card--forgetting,
  .card--pipeline {
    grid-column: 1 / -1;
  }

  .card--active,
  .card--search,
  .card--import,
  .card--binary,
  .card--privacy,
  .card--install {
    grid-column: span 4;
  }
}

@media (max-width: 480px) {
  .landing-main {
    padding: 86px 14px 52px;
  }

  .bento-grid {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .bento-card {
    min-height: auto;
    padding: 16px;
    border-radius: 14px;
    backdrop-filter: blur(12px) saturate(1.04);
    -webkit-backdrop-filter: blur(12px) saturate(1.04);
  }

  .hero-watermark {
    top: 72px;
    width: 96vw;
  }

  .hero-watermark__title {
    font-size: clamp(48px, 17vw, 72px);
  }

  .hero-watermark__subtext {
    margin-left: 0;
    font-size: 22px;
  }
}
```

The third desktop row deliberately leaves columns `8 / 13` open for a secondary documentation entry card below the fold. Do not add that as one of the eight story cards. If a ninth entry card is kept, place it below the first viewport with `.docs-entry-card { grid-column: 8 / span 5; grid-row: 3; }`.

Landing JSX shell:

```jsx
export default function Landing() {
  return (
    <div className="sky-shell">
      <NavBar />
      <HeroWatermark />
      <main className="landing-main">
        <p className="type-dateline landing-dateline">
          Claude Self-Reflect / v8.0 / Rust Engine / Local Memory Archive
        </p>
        <section className="bento-grid" aria-label="Claude Self-Reflect overview">
          <ForgettingCard />
          <ActiveMemoryCard />
          <SearchCard />
          <ImportCard />
          <OneBinaryCard />
          <PipelineCard />
          <PrivacyCard />
          <InstallCard />
        </section>
        <LandingFooter />
      </main>
    </div>
  )
}
```

## Shared React Primitives

Use `IntersectionObserver` once per card to add `.is-visible`. Do not replay entry animations repeatedly.

```jsx
import { useEffect, useRef, useState } from 'react'

function useVisible(threshold = 0.18) {
  const ref = useRef(null)
  const [visible, setVisible] = useState(false)

  useEffect(() => {
    const node = ref.current
    if (!node) return

    const observer = new IntersectionObserver(
      ([entry]) => {
        if (entry.isIntersecting) {
          setVisible(true)
          observer.unobserve(node)
        }
      },
      { threshold, rootMargin: '0px 0px -8% 0px' }
    )

    observer.observe(node)
    return () => observer.disconnect()
  }, [threshold])

  return [ref, visible]
}

function BentoCard({ as: Tag = 'article', className = '', delay = 0, children }) {
  const [ref, visible] = useVisible()

  return (
    <Tag
      ref={ref}
      className={`bento-card bento-shell ${className} ${visible ? 'is-visible' : ''}`}
      style={{ animationDelay: `${delay}ms` }}
    >
      {children}
    </Tag>
  )
}

function PostIt({ className = '', color = 'var(--color-amber-paper)', rot = '-2deg', children }) {
  return (
    <div
      className={`post-it ${className}`}
      style={{ background: color, '--rot': rot, '--from-rot': '-4deg' }}
    >
      {children}
    </div>
  )
}
```

## Card 1: The Forgetting Problem

Grid: `.card--forgetting { grid-column: 1 / span 5; grid-row: 1; min-height: 300px; }`

CSS treatment:

```css
.card--forgetting {
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.86), rgba(247, 248, 251, 0.70)),
    rgba(255, 255, 255, 0.80);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow:
    0 20px 60px rgba(42, 38, 66, 0.16),
    inset 0 1px 0 rgba(255, 255, 255, 0.86);
}

.card--forgetting::before {
  content: "";
  position: absolute;
  inset: 0 0 auto 0;
  height: 18px;
  background: linear-gradient(90deg, transparent 0 4%, rgba(255, 255, 255, 0.70) 8% 18%, transparent 22% 28%, rgba(255, 255, 255, 0.58) 34% 60%, transparent 68%);
  opacity: 0.44;
  clip-path: polygon(0 0, 100% 0, 100% 58%, 92% 82%, 82% 52%, 68% 76%, 48% 55%, 34% 86%, 16% 54%, 0 78%);
}

.forgetting-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 190px;
  gap: 18px;
  height: 100%;
}

.forgetting-chart {
  align-self: end;
  padding: 14px;
  border: 1px solid rgba(26, 26, 46, 0.16);
  border-radius: 6px;
  background: rgba(255, 255, 255, 0.34);
}
```

Typography specs:

- Section number `01`: JetBrains Mono, 16px, 500, line-height 1, color `#b5838d`, letter-spacing `0.08em`.
- Dateline `SESSION 04 / CONTEXT WINDOW`: JetBrains Mono, 11px, 500, uppercase, color `#767692`, line-height 1.2.
- Headline `The Forgetting Problem`: Newsreader, 34px, 700, color `#1a1a2e`, line-height 0.95.
- Subhead: Inter, 13px, 450, color `#4a4a6a`, line-height 1.45.
- Chart label: JetBrains Mono, 10px, 500, uppercase, color `#767692`.
- Callout `memory drift +42%`: JetBrains Mono, 11px, 700, color `#b5838d`.

Data visualization specs:

- SVG viewBox: `0 0 220 150`
- Grid lines: `y=22, 52, 82, 112`, stroke `rgba(26,26,46,0.10)`, strokeWidth `1`
- Area path: `M10 16 C26 42 38 58 52 70 C70 88 88 97 108 108 C132 121 154 130 180 138 C190 142 200 145 210 147 L210 146 L10 146 Z`, fill `rgba(181, 131, 141, 0.10)`
- Line path: `M10 16 C26 42 38 58 52 70 C70 88 88 97 108 108 C132 121 154 130 180 138 C190 142 200 145 210 147`, stroke `#b5838d`, strokeWidth `2`, strokeLinecap `round`, pathLength `1`
- X labels: `S1`, `S2`, `S3`, `S4`, `S5`
- Y callout: `memory drift +42%`

Animation specs:

- Card entry: `cardRise`, 620ms, `cubic-bezier(0.16, 1, 0.3, 1)`, delay `0ms`
- Chart draw: `drawPath`, 700ms, `ease-out`, delay `180ms`
- Post-it entry: `postItIn`, 680ms, `cubic-bezier(0.16, 1, 0.3, 1)`, delay `420ms`
- Hover: card translates `-4px`; chart line does not replay.

Decorative elements:

- `01` at top-left.
- Horizontal rule starts after the number: `left: 58px; right: 18px; top: 28px`.
- Faux torn-paper top edge via `::before`.
- Post-it placement: `right: -12px; bottom: 16px; width: 150px; transform rotate(3deg); background #f3c9c3`.

Exact JSX:

```jsx
function ForgettingCard() {
  return (
    <BentoCard className="card--forgetting" delay={0}>
      <div className="card-kicker">
        <span className="section-number text-rose">01</span>
        <span className="card-kicker__rule" />
        <span className="type-dateline">SESSION 04 / CONTEXT WINDOW</span>
      </div>
      <div className="forgetting-layout">
        <div>
          <h2 className="type-card-headline-lg">The Forgetting Problem</h2>
          <p className="type-body-sm">
            Claude amnesia compounds across disconnected sessions.
          </p>
          <div className="rule my-4" />
          <div className="newspaper-lines" aria-hidden="true">
            <span style={{ width: '94%' }} />
            <span style={{ width: '72%' }} />
            <span style={{ width: '88%' }} />
            <span style={{ width: '64%' }} />
          </div>
          <p className="type-caption mt-4">
            SOURCE: Claude Code empirical study
          </p>
        </div>
        <figure className="forgetting-chart">
          <figcaption className="type-dateline">Context Lost Per Session</figcaption>
          <svg viewBox="0 0 220 150" role="img" aria-label="Context loss rises across five sessions">
            {[22, 52, 82, 112].map((y) => (
              <line key={y} x1="10" x2="210" y1={y} y2={y} stroke="rgba(26,26,46,0.10)" />
            ))}
            <path
              d="M10 16 C26 42 38 58 52 70 C70 88 88 97 108 108 C132 121 154 130 180 138 C190 142 200 145 210 147 L210 146 L10 146 Z"
              fill="rgba(181, 131, 141, 0.10)"
            />
            <path
              className="chart-path"
              pathLength="1"
              d="M10 16 C26 42 38 58 52 70 C70 88 88 97 108 108 C132 121 154 130 180 138 C190 142 200 145 210 147"
              fill="none"
              stroke="var(--color-rose)"
              strokeWidth="2"
              strokeLinecap="round"
              style={{ animationDelay: '180ms' }}
            />
            {['S1', 'S2', 'S3', 'S4', 'S5'].map((label, index) => (
              <text key={label} x={12 + index * 49} y="145" className="chart-label">{label}</text>
            ))}
          </svg>
          <p className="type-caption"><strong className="text-rose">memory drift +42%</strong></p>
        </figure>
      </div>
      <PostIt className="forgetting-note" color="var(--color-rose-paper)" rot="3deg">
        Re-explained the same architecture.
      </PostIt>
    </BentoCard>
  )
}
```

## Card 2: Active Memory

Grid: `.card--active { grid-column: 6 / span 4; grid-row: 1; min-height: 300px; }`

CSS treatment:

```css
.card--active {
  background:
    radial-gradient(circle at 84% 22%, rgba(107, 91, 149, 0.10), transparent 36%),
    rgba(255, 255, 255, 0.78);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.hook-timeline {
  margin: 18px 0 16px;
}

.hook-node__dot {
  transform-origin: center;
}

.is-visible .hook-node__dot {
  animation: hookPulse 3.6s ease-in-out infinite;
}
```

Typography specs:

- Section number `02`: JetBrains Mono, 16px, 500, color `#6b5b95`, line-height 1.
- Dateline `DAILY EDITION / ACTIVE MEMORY`: JetBrains Mono, 11px, 500, uppercase, color `#767692`, line-height 1.2.
- Headline `Active Memory`: Newsreader, 24px, 700, color `#1a1a2e`, line-height 1.
- Metric `12,000/day`: JetBrains Mono, 22px, 700, color `#1a1a2e`, line-height 1.
- Caption: Inter, 11px, 500, color `#767692`, line-height 1.35.
- Hook labels: JetBrains Mono, 10px, 500, color `#4a4a6a`, line-height 1.2.

Data visualization specs:

- SVG viewBox: `0 0 360 96`
- Timeline rail path: `M24 54 H336`, stroke `rgba(107,91,149,0.32)`, strokeWidth `2`
- Node positions: `24, 86, 148, 210, 272, 336`
- Node labels: `PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Stop`, `SubagentStop`, `Notification`
- Dot colors alternate: `#6b5b95`, `#7c9473`, `#6b5b95`, `#7c9473`, `#6b5b95`, `#7c9473`
- Density ticks: three groups at morning, midday, evening using short vertical lines at x `48-76`, `152-196`, `266-318`

Animation specs:

- Card entry: `cardRise`, 620ms, `cubic-bezier(0.16, 1, 0.3, 1)`, delay `80ms`
- Node first pulse: `hookPulse`, 700ms, `ease-out`, stagger `90ms` per node starting at `260ms`
- Idle heartbeat: `hookPulse`, 3.6s, `ease-in-out`, infinite, opacity never below `0.58`
- Density ticks fade in with `scorePop`, 420ms, delay `640ms`

Decorative elements:

- `02` and rule in the top kicker.
- Dateline line: `SAN FRANCISCO, CA / APR 24, 2025`.
- A horizontal rule separates metric from caption.
- No post-it on this card; keep it clean to contrast the paper notes elsewhere.

Exact JSX:

```jsx
function ActiveMemoryCard() {
  const nodes = [
    ['PreToolUse', '#6b5b95'],
    ['PostToolUse', '#7c9473'],
    ['UserPromptSubmit', '#6b5b95'],
    ['Stop', '#7c9473'],
    ['SubagentStop', '#6b5b95'],
    ['Notification', '#7c9473'],
  ]

  return (
    <BentoCard className="card--active" delay={80}>
      <div className="card-kicker">
        <span className="section-number">02</span>
        <span className="card-kicker__rule" />
        <span className="type-dateline">DAILY EDITION / ACTIVE MEMORY</span>
      </div>
      <h2 className="type-card-headline-md">Active Memory</h2>
      <p className="type-dateline mt-3">SAN FRANCISCO, CA / APR 24, 2025</p>
      <p className="type-body-sm mt-3">
        CSR installs 6 hooks into Claude Code. They run automatically on every turn.
      </p>
      <svg className="hook-timeline" viewBox="0 0 360 96" role="img" aria-label="Six active memory hooks fire throughout the day">
        <path d="M24 54 H336" stroke="rgba(107,91,149,0.32)" strokeWidth="2" />
        {nodes.map(([label, color], index) => {
          const x = [24, 86, 148, 210, 272, 336][index]
          return (
            <g key={label} className="hook-node">
              <text x={x} y="22" textAnchor="middle" className="chart-label">{label}</text>
              <circle cx={x} cy="54" r="13" fill={color} opacity="0.10" />
              <circle
                className="hook-node__dot"
                cx={x}
                cy="54"
                r="5"
                fill={color}
                style={{ animationDelay: `${260 + index * 90}ms` }}
              />
            </g>
          )
        })}
        {[48, 58, 68, 76, 152, 164, 176, 188, 196, 266, 278, 290, 302, 318].map((x) => (
          <line key={x} x1={x} x2={x} y1="74" y2="84" stroke="rgba(26,26,46,0.16)" />
        ))}
      </svg>
      <div className="rule pt-4">
        <p className="type-metric-md">12,000/day</p>
        <p className="type-caption mt-2">6 hooks fire before, during, and after work.</p>
      </div>
    </BentoCard>
  )
}
```

## Card 3: The Search

Grid: `.card--search { grid-column: 10 / span 3; grid-row: 1; min-height: 300px; }`

CSS treatment:

```css
.card--search {
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.84), rgba(255, 255, 255, 0.70)),
    rgba(255, 255, 255, 0.80);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.search-query {
  margin-top: 14px;
  padding: 10px 12px;
  border-radius: 8px;
  border: 1px solid rgba(26, 26, 46, 0.16);
  background: rgba(255, 255, 255, 0.42);
  transition: border-color 180ms ease, box-shadow 180ms ease;
}

.card--search:hover .search-query {
  border-color: rgba(107, 91, 149, 0.42);
  box-shadow: 0 0 0 3px rgba(107, 91, 149, 0.14);
}

.result-strip {
  padding: 9px 10px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.56);
  background: rgba(255, 255, 255, 0.48);
}
```

Typography specs:

- Section number `03`: JetBrains Mono, 16px, 500, color `#6b5b95`.
- Headline `The Search`: Newsreader, 19px, 650, color `#1a1a2e`, line-height 1.08.
- Query `"auth bug from last week"`: JetBrains Mono, 14px, 500, color `#1a1a2e`, line-height 1.45.
- Metric `<1ms`: JetBrains Mono, 32px, 700, color `#1a1a2e`, line-height 1.
- Caption `local vector lookup`: Inter, 11px, 500, color `#767692`, line-height 1.35.
- p95 callout: JetBrains Mono, 11px, 500, color `#6b5b95`.

Data visualization specs:

- SVG viewBox: `0 0 180 64`
- Latency path for values `0.9, 0.7, 0.8, 0.6, 0.7`: `M6 24 L48 34 L90 28 L132 40 L174 33`
- Stroke: `#6b5b95`, strokeWidth `2`, strokeLinecap `round`, pathLength `1`
- Fill path: `M6 24 L48 34 L90 28 L132 40 L174 33 L174 58 L6 58 Z`, fill `rgba(107,91,149,0.08)`
- Axis labels: `0ms`, `1ms`, `2ms`
- Callout: `p95 0.82ms`

Animation specs:

- Card entry: `cardRise`, 620ms, delay `160ms`
- Query arrow draw: `drawPath`, 420ms, `ease-out`, delay `290ms`
- Sparkline draw: `drawPath`, 700ms, `ease-out`, delay `340ms`
- Result strips fade with `scorePop`, 360ms, stagger `70ms` starting at `430ms`

Decorative elements:

- `03` top-left; short rule to the headline.
- Query arrow is a fine rule ending in a dot, not a chunky icon.
- Result rows look like translucent index cards.

Exact JSX:

```jsx
function SearchCard() {
  return (
    <BentoCard className="card--search" delay={160}>
      <div className="card-kicker">
        <span className="section-number">03</span>
        <span className="card-kicker__rule" />
        <span className="type-dateline">DATELINE 10:24:31</span>
      </div>
      <h2 className="type-card-headline-sm">The Search</h2>
      <div className="search-query type-code">"auth bug from last week"</div>
      <svg viewBox="0 0 160 24" className="my-2" aria-hidden="true">
        <path
          className="chart-path"
          pathLength="1"
          d="M18 12 H132"
          stroke="rgba(26,26,46,0.34)"
          strokeWidth="1"
          style={{ animationDelay: '290ms' }}
        />
        <circle cx="136" cy="12" r="3" fill="var(--color-purple)" />
      </svg>
      <div className="space-y-2">
        <div className="result-strip type-caption">Top result / score 0.87</div>
        <div className="result-strip type-caption">Related file / hooks.yaml</div>
      </div>
      <div className="rule mt-4 pt-4">
        <p className="type-metric-lg">&lt;1ms</p>
        <p className="type-caption mt-1">local vector lookup</p>
        <svg viewBox="0 0 180 64" role="img" aria-label="Search latency stays below one millisecond">
          <path d="M6 24 L48 34 L90 28 L132 40 L174 33 L174 58 L6 58 Z" fill="rgba(107,91,149,0.08)" />
          <path
            className="chart-path"
            pathLength="1"
            d="M6 24 L48 34 L90 28 L132 40 L174 33"
            fill="none"
            stroke="var(--color-purple)"
            strokeWidth="2"
            strokeLinecap="round"
            style={{ animationDelay: '340ms' }}
          />
          <text x="6" y="62" className="chart-label">p95 0.82ms</text>
        </svg>
      </div>
    </BentoCard>
  )
}
```

## Card 4: The Import

Grid: `.card--import { grid-column: 1 / span 4; grid-row: 2; min-height: 250px; }`

CSS treatment:

```css
.card--import {
  background:
    linear-gradient(135deg, rgba(255, 255, 255, 0.84), rgba(255, 255, 255, 0.68)),
    rgba(255, 255, 255, 0.80);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.import-ledger {
  display: grid;
  grid-template-columns: 1fr 1fr;
  border: 1px dashed rgba(181, 131, 141, 0.42);
  border-radius: 8px;
  overflow: hidden;
}

.import-ledger > div {
  padding: 14px;
}

.import-ledger > div + div {
  border-left: 1px dashed rgba(181, 131, 141, 0.42);
}

.progress-rail {
  height: 11px;
  border-radius: 999px;
  background: rgba(26, 26, 46, 0.06);
  overflow: hidden;
}

.progress-rail__fill {
  height: 100%;
  width: 100%;
  transform-origin: left;
  background: linear-gradient(90deg, rgba(124, 148, 115, 0.82), rgba(124, 148, 115, 0.96));
}

.is-visible .progress-rail__fill {
  animation: growX 700ms ease-out 420ms both;
}
```

Typography specs:

- Section number `04`: JetBrains Mono, 16px, 500, color `#b5838d`.
- Dateline `ARCHIVE IMPORT / LOCAL ONLY`: JetBrains Mono, 11px, 500, uppercase, color `#767692`.
- Headline `The Import`: Newsreader, 24px, 700, color `#1a1a2e`.
- Metrics `1,107 conversations`, `15,745 chunks`: JetBrains Mono, 22px, 700 for numbers, Inter 13px, 450 for labels.
- Bar labels `parse`, `chunk`, `embed`, `index`: JetBrains Mono, 10px, 500, color `#4a4a6a`.
- Caption: Inter, 11px, 500, color `#767692`.

Data visualization specs:

- Progress rail: 100%, fill `#7c9473`
- SVG viewBox for bars: `0 0 260 86`
- Bars:
  - parse: x `18`, height `58`, value `100%`, fill `#7c9473`
  - chunk: x `76`, height `58`, value `100%`, fill `#7c9473`
  - embed: x `134`, height `53`, value `92%`, fill `#b5838d`
  - index: x `192`, height `58`, value `100%`, fill `#7c9473`
- Axis baseline y `72`, stroke `rgba(26,26,46,0.18)`

Animation specs:

- Card entry: `cardRise`, 620ms, delay `240ms`
- Progress rail: `growX`, 700ms, `ease-out`, delay `420ms`
- Bars: `growY`, 520ms, `ease-out`, delays `480ms`, `540ms`, `600ms`, `660ms`

Decorative elements:

- Ledger border is dashed rose.
- Dateline sits in top kicker.
- Progress label right-aligned as `100%`.
- Bottom caption: `From exports, transcripts, and logs.`

Exact JSX:

```jsx
function ImportCard() {
  const bars = [
    ['parse', 58, '#7c9473'],
    ['chunk', 58, '#7c9473'],
    ['embed', 53, '#b5838d'],
    ['index', 58, '#7c9473'],
  ]

  return (
    <BentoCard className="card--import" delay={240}>
      <div className="card-kicker">
        <span className="section-number text-rose">04</span>
        <span className="card-kicker__rule" />
        <span className="type-dateline">ARCHIVE IMPORT / LOCAL ONLY</span>
      </div>
      <h2 className="type-card-headline-md">The Import</h2>
      <p className="type-body-sm mt-2">Your past. Brought forward.</p>
      <div className="import-ledger mt-4">
        <div>
          <p className="type-metric-md">1,107</p>
          <p className="type-body-sm">conversations</p>
        </div>
        <div>
          <p className="type-metric-md">15,745</p>
          <p className="type-body-sm">chunks</p>
        </div>
      </div>
      <div className="mt-4 flex items-center justify-between">
        <span className="type-dateline">Import Progress</span>
        <span className="type-dateline">100%</span>
      </div>
      <div className="progress-rail mt-2">
        <div className="progress-rail__fill" />
      </div>
      <svg viewBox="0 0 260 86" className="mt-3" role="img" aria-label="Import stages parse chunk embed and index">
        <line x1="12" x2="244" y1="72" y2="72" stroke="rgba(26,26,46,0.18)" />
        {bars.map(([label, height, color], index) => {
          const x = 18 + index * 58
          return (
            <g key={label}>
              <rect
                x={x}
                y={72 - height}
                width="32"
                height={height}
                fill={color}
                opacity="0.72"
                style={{
                  transformOrigin: `${x + 16}px 72px`,
                  animation: `growY 520ms ease-out ${480 + index * 60}ms both`,
                }}
              />
              <text x={x + 16} y="84" textAnchor="middle" className="chart-label">{label}</text>
            </g>
          )
        })}
      </svg>
    </BentoCard>
  )
}
```

## Card 5: One Binary

Grid: `.card--binary { grid-column: 5 / span 3; grid-row: 2; min-height: 250px; }`

CSS treatment:

```css
.card--binary {
  background:
    radial-gradient(circle at 48% 40%, rgba(246, 230, 169, 0.18), transparent 42%),
    rgba(255, 255, 255, 0.78);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.binary-stamp {
  width: 118px;
  height: 118px;
  margin: 16px auto 14px;
  display: grid;
  place-items: center;
  border: 2px solid rgba(26, 26, 46, 0.62);
  border-radius: 999px;
  background:
    radial-gradient(circle at 48% 42%, rgba(255, 255, 255, 0.20), transparent 64%),
    rgba(246, 230, 169, 0.22);
}

.binary-stack {
  position: relative;
  min-height: 52px;
}

.binary-layer {
  position: absolute;
  width: 72px;
  height: 28px;
  display: grid;
  place-items: center;
  border-radius: 3px;
  border: 1px solid rgba(26, 26, 46, 0.18);
  background: rgba(246, 230, 169, 0.78);
  box-shadow: 0 6px 14px rgba(42, 38, 66, 0.08);
  transition: transform 260ms cubic-bezier(0.16, 1, 0.3, 1);
}

.card--binary:hover .binary-layer:nth-child(1) { transform: translate(-3px, -3px) rotate(-2deg); }
.card--binary:hover .binary-layer:nth-child(2) { transform: translate(0, 0) rotate(1deg); }
.card--binary:hover .binary-layer:nth-child(3) { transform: translate(3px, 3px) rotate(2deg); }
```

Typography specs:

- Section number `05`: JetBrains Mono, 16px, 500, color `#b5838d`.
- Headline `One Binary`: Newsreader, 19px, 650, color `#1a1a2e`, line-height 1.08.
- Main metric `44MB`: JetBrains Mono, 32px, 700, color `#1a1a2e`, line-height 1.
- Stamp label `CSR single binary`: JetBrains Mono, 11px, 500, color `#343145`.
- Caption: Inter, 11px, 500, color `#767692`.
- Competitor labels: JetBrains Mono, 10px, 500, color `#343145`.

Data visualization specs:

- Stamp SVG viewBox: `0 0 150 150`
- Circle path: `M75 14 A61 61 0 1 1 74.9 14`, stroke `rgba(26,26,46,0.62)`, strokeWidth `2`, fill `rgba(246,230,169,0.20)`
- Competitor stack values: Docker, Python, DB, plus caption `~1.2GB and 40+ dependencies`
- CSR value: `44MB`

Animation specs:

- Card entry: `cardRise`, 620ms, delay `320ms`
- Stamp fade/pop: `scorePop`, 460ms, `ease-out`, delay `520ms`
- Competitor stack layers: `tabLift`, 360ms, `ease-out`, stagger `70ms`, start `600ms`
- Hover separates competitor stack by `3px` per layer.

Decorative elements:

- Background paper grain inside stamp via radial gradient.
- Competitor stack is three offset post-it strips on right/bottom.
- Small note: `less to remember before memory can work`.

Exact JSX:

```jsx
function OneBinaryCard() {
  return (
    <BentoCard className="card--binary" delay={320}>
      <div className="card-kicker">
        <span className="section-number text-rose">05</span>
        <span className="card-kicker__rule" />
      </div>
      <h2 className="type-card-headline-sm">One Binary</h2>
      <div className="binary-stamp" aria-label="Claude Self-Reflect is a 44 megabyte single binary">
        <div>
          <p className="type-metric-lg text-center">44MB</p>
          <p className="type-caption text-center">CSR single binary</p>
        </div>
      </div>
      <p className="type-caption text-center">ship it as a single artifact</p>
      <div className="rule my-3" />
      <div className="binary-stack" aria-label="Competitor stack requires Docker Python and database">
        <span className="binary-layer type-dateline" style={{ left: '8px', top: '0px', animationDelay: '600ms' }}>Docker</span>
        <span className="binary-layer type-dateline" style={{ left: '78px', top: '10px', animationDelay: '670ms' }}>Python</span>
        <span className="binary-layer type-dateline" style={{ left: '148px', top: '20px', animationDelay: '740ms' }}>DB</span>
      </div>
      <p className="type-caption mt-2">less to remember before memory can work</p>
    </BentoCard>
  )
}
```

## Card 6: The Pipeline

Grid: `.card--pipeline { grid-column: 8 / span 5; grid-row: 2; min-height: 250px; }`

CSS treatment:

```css
.card--pipeline {
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.84), rgba(255, 255, 255, 0.68)),
    rgba(255, 255, 255, 0.80);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.pipeline-layers {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 10px;
  margin-top: 14px;
}

.pipeline-layer {
  min-height: 136px;
  padding: 13px;
  border: 1px solid rgba(26, 26, 46, 0.12);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.40);
}

.pipeline-score-bar {
  height: 5px;
  border-radius: 999px;
  background: rgba(26, 26, 46, 0.06);
  overflow: hidden;
}

.pipeline-score-bar span {
  display: block;
  height: 100%;
  transform-origin: left;
}

.is-visible .pipeline-score-bar span {
  animation: growX 520ms ease-out both;
}
```

Typography specs:

- Section number `06`: JetBrains Mono, 16px, 500, color `#6b5b95`.
- Dateline `ENRICHMENT PIPELINE / QUALITY REPORT`: JetBrains Mono, 11px, 500, uppercase, color `#767692`.
- Headline `The Pipeline`: Newsreader, 24px, 700, color `#1a1a2e`.
- Layer labels `L1 raw`, `L2 contextualized`, `L3 reflective`: JetBrains Mono, 11px, 500, uppercase, color `#767692`.
- Layer titles: Newsreader, 19px, 650, color `#1a1a2e`, line-height 1.08.
- Scores `0.074`, `0.345`, `0.691`: JetBrains Mono, 22px, 700, color `#1a1a2e`.
- Caption: Inter, 11px, 500, color `#767692`.

Data visualization specs:

- Quality scores: `0.074 -> 0.345 -> 0.691`
- Mini bar proportional widths:
  - `0.074`: 11%, fill `#b5838d`
  - `0.345`: 50%, fill `#6b5b95`
  - `0.691`: 100%, fill `#7c9473`
- Connector SVG viewBox: `0 0 520 40`
- Connector paths: `M158 20 H236`, `M334 20 H412`, stroke `#6b5b95`, strokeWidth `1.5`, marker arrow with small open arrowhead.
- Optional dot clouds inside layers use 11 circles each, opacity `0.22-0.70`, same accent as layer.

Animation specs:

- Card entry: `cardRise`, 620ms, delay `400ms`
- Connector draw: `connectorDraw`, 420ms, `ease-out`, delay `580ms`, second connector delay `650ms`
- Score pop: `scorePop`, 420ms, `ease-out`, delays `620ms`, `700ms`, `780ms`
- Mini bars: `growX`, 520ms, `ease-out`, same delays as scores.

Decorative elements:

- Numbered circles `1`, `2`, `3` inside each mini-panel.
- Horizontal rule under headline.
- Caption strip at bottom: `reflection turns fragments into retrievable memory.`

Exact JSX:

```jsx
function PipelineCard() {
  const layers = [
    { no: '1', label: 'L1 raw', title: 'Retrieve', score: '0.074', pct: '11%', color: '#b5838d' },
    { no: '2', label: 'L2 contextualized', title: 'Re-rank', score: '0.345', pct: '50%', color: '#6b5b95' },
    { no: '3', label: 'L3 reflective', title: 'Re-write', score: '0.691', pct: '100%', color: '#7c9473' },
  ]

  return (
    <BentoCard className="card--pipeline" delay={400}>
      <div className="card-kicker">
        <span className="section-number">06</span>
        <span className="card-kicker__rule" />
        <span className="type-dateline">ENRICHMENT PIPELINE / QUALITY REPORT</span>
      </div>
      <h2 className="type-card-headline-md">The Pipeline</h2>
      <div className="pipeline-layers">
        {layers.map((layer, index) => (
          <section className="pipeline-layer" key={layer.label}>
            <div className="flex items-center justify-between">
              <span className="type-dateline">{layer.label}</span>
              <span className="pipeline-no">{layer.no}</span>
            </div>
            <h3 className="type-card-headline-sm mt-2">{layer.title}</h3>
            <p className="type-caption mt-1">Quality Score</p>
            <p className="type-metric-md mt-1">{layer.score}</p>
            <div className="pipeline-score-bar mt-3">
              <span
                style={{
                  width: layer.pct,
                  background: layer.color,
                  animationDelay: `${620 + index * 80}ms`,
                }}
              />
            </div>
          </section>
        ))}
      </div>
      <svg viewBox="0 0 520 40" className="pipeline-connectors" aria-hidden="true">
        <path
          className="chart-path"
          pathLength="1"
          d="M158 20 H236"
          stroke="var(--color-purple)"
          strokeWidth="1.5"
          style={{ animationDelay: '580ms' }}
        />
        <path
          className="chart-path"
          pathLength="1"
          d="M334 20 H412"
          stroke="var(--color-purple)"
          strokeWidth="1.5"
          style={{ animationDelay: '650ms' }}
        />
      </svg>
      <p className="type-caption rule mt-3 pt-3">
        reflection turns fragments into retrievable memory.
      </p>
    </BentoCard>
  )
}
```

## Card 7: Privacy

Grid: `.card--privacy { grid-column: 1 / span 3; grid-row: 3; min-height: 210px; }`

CSS treatment:

```css
.card--privacy {
  background:
    radial-gradient(circle at 72% 38%, rgba(220, 232, 207, 0.30), transparent 46%),
    rgba(255, 255, 255, 0.76);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.16), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.privacy-note {
  right: 18px;
  bottom: 20px;
  width: 150px;
  min-height: 116px;
  background: var(--color-sage-paper);
  transform: rotate(-4deg);
  transition: transform 260ms cubic-bezier(0.16, 1, 0.3, 1);
}

.card--privacy:hover .privacy-note {
  transform: rotate(-3deg);
}

.lock-path {
  stroke-dasharray: 1;
  stroke-dashoffset: 1;
}

.is-visible .lock-path {
  animation: lockDraw 620ms ease-out 620ms forwards;
}
```

Typography specs:

- Section number `07`: JetBrains Mono, 16px, 500, color `#7c9473`.
- Headline `Privacy`: Newsreader, 19px, 650, color `#1a1a2e`, line-height 1.08.
- Main line `Zero network connections.`: Inter, 24px, 700, color `#343145`, line-height 1.05.
- Caption `memory stays on the machine`: Inter, 11px, 500, color `#767692`, line-height 1.35.
- Status `127.0.0.1 only`: JetBrains Mono, 11px, 500, color `#7c9473`.

Data visualization specs:

- Boolean proof point: zero network connections.
- Lock SVG viewBox: `0 0 48 48`
- Lock path: `M15 22V17C15 10.9 19.5 7 24 7C28.5 7 33 10.9 33 17V22 M13 22H35V40H13V22Z M24 29V34`, stroke `#7c9473`, strokeWidth `2.4`, strokeLinecap `round`, strokeLinejoin `round`, pathLength `1`
- Status strip: `127.0.0.1 only`

Animation specs:

- Card entry: `cardRise`, 620ms, delay `480ms`
- Lock draw: `lockDraw`, 620ms, `ease-out`, delay `620ms`
- Post-it align hover: transition 260ms, rotate from `-4deg` to `-3deg`

Decorative elements:

- Sage post-it occupies the right half, slightly rotated.
- Small tape piece from `.post-it::before`.
- Left side intentionally sparse.

Exact JSX:

```jsx
function PrivacyCard() {
  return (
    <BentoCard className="card--privacy" delay={480}>
      <div className="card-kicker">
        <span className="section-number text-sage">07</span>
        <span className="card-kicker__rule" />
      </div>
      <h2 className="type-card-headline-sm">Privacy</h2>
      <p className="type-caption mt-3">CSR never calls out.</p>
      <p className="type-dateline mt-8">127.0.0.1 only</p>
      <PostIt className="privacy-note" color="var(--color-sage-paper)" rot="-4deg">
        <svg viewBox="0 0 48 48" width="48" height="48" aria-hidden="true">
          <path
            className="lock-path"
            pathLength="1"
            d="M15 22V17C15 10.9 19.5 7 24 7C28.5 7 33 10.9 33 17V22 M13 22H35V40H13V22Z M24 29V34"
            fill="none"
            stroke="var(--color-sage)"
            strokeWidth="2.4"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        <strong>Zero network connections.</strong>
        <span>All memory stays on your machine.</span>
      </PostIt>
    </BentoCard>
  )
}
```

## Card 8: Install

Grid: `.card--install { grid-column: 4 / span 4; grid-row: 3; min-height: 210px; }`

CSS treatment:

```css
.card--install {
  background:
    linear-gradient(180deg, rgba(255, 255, 255, 0.80), rgba(255, 255, 255, 0.62)),
    rgba(255, 255, 255, 0.76);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(26, 26, 46, 0.22);
  border-radius: 10px;
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.14), inset 0 1px 0 rgba(255, 255, 255, 0.78);
}

.install-command {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 14px;
  padding: 10px 12px;
  border-radius: 6px;
  border: 1px solid rgba(26, 26, 46, 0.16);
  background: rgba(26, 26, 46, 0.06);
  color: var(--color-ink);
}

.install-copy {
  margin-left: auto;
  width: 28px;
  height: 28px;
  display: grid;
  place-items: center;
  border: 1px solid rgba(26, 26, 46, 0.12);
  border-radius: 5px;
  background: rgba(255, 255, 255, 0.44);
  color: var(--color-purple);
}

.tear-tabs {
  position: absolute;
  left: 18px;
  right: 18px;
  bottom: 12px;
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 6px;
}

.tear-tab {
  padding: 7px 4px;
  border: 1px dashed rgba(26, 26, 46, 0.18);
  border-radius: 3px;
  background: rgba(255, 255, 255, 0.38);
  text-align: center;
}

.is-visible .tear-tab {
  animation: tabLift 360ms ease-out both;
}
```

Typography specs:

- Section number `08`: JetBrains Mono, 16px, 500, color `#6b5b95`.
- Dateline `CLASSIFIEDS / TOOLS`: JetBrains Mono, 11px, 500, uppercase, color `#767692`.
- Headline `Install`: Newsreader, 24px, 700, color `#1a1a2e`, line-height 1.
- Classified line `curl | sh. Done.`: Newsreader, 28px, 700, color `#1a1a2e`, line-height 0.95.
- Command `curl -fsSL ... | sh`: JetBrains Mono, 14px, 500, color `#1a1a2e`, line-height 1.45.
- Tear-off tabs: JetBrains Mono, 10px, 500, uppercase, color `#767692`.

Data visualization specs:

- Install promise: one command.
- Setup marker: `<60 sec setup`, JetBrains Mono, 11px, color `#7c9473`.
- Tear tabs are the data tokens: `local`, `fast`, `search`, `hooks`.
- No chart line; this card is a classified ad module.

Animation specs:

- Card entry: `cardRise`, 620ms, delay `560ms`
- Tear-off tabs: `tabLift`, 360ms, `ease-out`, delays `720ms`, `790ms`, `860ms`, `930ms`
- Copy button hover: background to `rgba(107,91,149,0.10)`, border to `rgba(107,91,149,0.32)`
- After click: set local state `copied`, show `Check` icon for 1600ms with `aria-live="polite"`

Decorative elements:

- Classified ad border uses `rgba(26,26,46,0.22)`, not white.
- Double rule above the command and single rule below.
- Tear-off tabs along bottom.

Exact JSX:

```jsx
function InstallCard() {
  const [copied, setCopied] = useState(false)
  const command = 'curl -fsSL https://raw.githubusercontent.com/ramakay/claude-self-reflect/main/install.sh | sh'

  async function copyCommand() {
    await navigator.clipboard.writeText(command)
    setCopied(true)
    window.setTimeout(() => setCopied(false), 1600)
  }

  return (
    <BentoCard className="card--install" delay={560}>
      <div className="card-kicker">
        <span className="section-number">08</span>
        <span className="card-kicker__rule" />
        <span className="type-dateline">CLASSIFIEDS / TOOLS</span>
      </div>
      <h2 className="type-card-headline-md">Install</h2>
      <p className="install-classified">curl | sh. Done.</p>
      <div className="rule-double mt-3" />
      <div className="install-command">
        <code className="type-code">curl -fsSL ... | sh</code>
        <button className="install-copy" type="button" onClick={copyCommand} aria-label="Copy install command">
          {copied ? <Check size={15} /> : <Copy size={15} />}
        </button>
      </div>
      <p className="type-dateline mt-2 text-sage">&lt;60 sec setup</p>
      <div className="tear-tabs" aria-hidden="true">
        {['local', 'fast', 'search', 'hooks'].map((label, index) => (
          <span
            key={label}
            className="tear-tab type-dateline"
            style={{ animationDelay: `${720 + index * 70}ms` }}
          >
            {label}
          </span>
        ))}
      </div>
      <span className="sr-only" aria-live="polite">{copied ? 'Install command copied' : ''}</span>
    </BentoCard>
  )
}
```

## Card Kicker And Helper CSS

These helpers are shared across the exact JSX above.

```css
.card-kicker {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-bottom: 12px;
}

.card-kicker__rule {
  flex: 1;
  border-top: 1px solid rgba(26, 26, 46, 0.22);
}

.text-rose {
  color: var(--color-rose);
}

.text-sage {
  color: var(--color-sage);
}

.chart-label {
  font-family: var(--font-mono);
  font-size: 9px;
  font-weight: 500;
  fill: var(--color-muted);
  letter-spacing: 0.02em;
}

.newspaper-lines {
  display: grid;
  gap: 8px;
}

.newspaper-lines span {
  height: 7px;
  border-radius: 999px;
  background: rgba(26, 26, 46, 0.08);
}

.install-classified {
  margin-top: 7px;
  font-family: var(--font-serif);
  font-size: 28px;
  font-weight: 700;
  line-height: 0.95;
  color: var(--color-ink);
}

.pipeline-no {
  width: 24px;
  height: 24px;
  display: grid;
  place-items: center;
  border: 1px solid rgba(26, 26, 46, 0.18);
  border-radius: 999px;
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 700;
  color: var(--color-purple);
}
```

## Footer Design

The footer should continue the editorial tone. It is not a dark band.

Exact treatment:

```css
.landing-footer {
  margin-top: 72px;
  padding: 28px 0 40px;
  border-top: 1px solid rgba(26, 26, 46, 0.13);
  background: transparent;
}

.landing-footer__inner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 24px;
}

.landing-footer__copy {
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 500;
  line-height: 1.35;
  letter-spacing: 0.02em;
  color: var(--color-muted);
}

.landing-footer__links {
  display: flex;
  align-items: center;
  gap: 22px;
}

.landing-footer__link {
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 500;
  line-height: 1.35;
  letter-spacing: 0.02em;
  color: var(--color-muted);
  text-decoration: none;
}

.landing-footer__link:hover {
  color: var(--color-ink);
  text-decoration: underline;
  text-underline-offset: 3px;
}

@media (max-width: 768px) {
  .landing-footer__inner {
    align-items: flex-start;
    flex-direction: column;
  }
}
```

JSX:

```jsx
function LandingFooter() {
  return (
    <footer className="landing-footer">
      <div className="landing-footer__inner">
        <p className="landing-footer__copy">
          (c) 2026 Claude Self-Reflect. Built for long-term context. MIT License.
        </p>
        <nav className="landing-footer__links" aria-label="Footer">
          <a className="landing-footer__link" href="https://github.com/ramakay/claude-self-reflect">GitHub</a>
          <a className="landing-footer__link" href="https://www.npmjs.com/package/claude-self-reflect">npm</a>
          <Link className="landing-footer__link" to="/docs/why-csr">Documentation</Link>
        </nav>
      </div>
    </footer>
  )
}
```

## Implementation Notes Against Current Landing.jsx

The current implementation already has useful pieces: `Sparkline`, `Bar`, `HookDot`, `PostIt`, `Card`, Tailwind v4 color tokens, and an IntersectionObserver pattern. Keep that direction, but adjust the target:

- Replace rounded 20px cards with 18px glass sheets and the stronger white border from this guide.
- Replace the current 9-card grid with the exact 8-card canonical bento grid above.
- Move the MCP tools entry below the first viewport as a secondary documentation entry, not part of the eight story cards.
- Use the full hero text from the proposal: `You or your agent don't have to remember any of this`.
- Replace emoji lock and checkmarks with `lucide-react` icons or inline SVG.
- Convert ad hoc sparklines to fixed SVG paths so the comp can be reproduced exactly.

## Inner Doc Pages

Recommendation: maintain the sky/glass aesthetic, but switch the main content area into a cleaner reading mode. The generated comp at `public/images/inner-page-comp.png` shows the right balance: the cloud sky still bleeds through the sidebar and content panel, but body text sits on a larger, calmer frosted reading surface with stronger line length control. This preserves brand continuity from the landing page without making long documentation feel like a decorative dashboard.

Use this layout:

- Same `sky-shell`, nav, background gradients, noise overlay, and haze effect.
- Sidebar is a glass panel with nav items and a small local-only post-it.
- Main content is one large glass reading panel with lower transparency than cards: `rgba(255,255,255,0.84)`.
- Maximum reading line length: `74ch`.
- Keep bento thumbnails as section headers, not as dense cards inside prose.

### Inner Page Structure

```jsx
function DocLayout({ children, currentSlug }) {
  return (
    <div className="sky-shell">
      <NavBar />
      <main className="doc-shell">
        <DocSidebar currentSlug={currentSlug} />
        <article className="doc-content">
          {children}
        </article>
      </main>
    </div>
  )
}

function DocSidebar({ currentSlug }) {
  const items = [
    ['Guide', '/docs/why-csr', BookOpen],
    ['Installation', '/docs/installation', Download],
    ['Search', '/docs/search', Search],
    ['Active Memory Hooks', '/docs/hooks', GitBranch],
    ['MCP Tools', '/docs/mcp-tools', Wrench],
    ['Privacy', '/docs/privacy', Lock],
  ]

  return (
    <aside className="doc-sidebar" aria-label="Documentation navigation">
      <p className="type-dateline doc-sidebar__label">Documentation</p>
      <div className="rule" />
      <nav className="doc-sidebar__nav">
        {items.map(([label, href, Icon]) => (
          <Link
            key={href}
            to={href}
            className={`doc-sidebar__link ${currentSlug === href ? 'is-active' : ''}`}
          >
            <Icon size={18} aria-hidden="true" />
            <span>{label}</span>
          </Link>
        ))}
      </nav>
      <div className="doc-sidebar__note">
        <PostIt color="var(--color-amber-paper)" rot="-5deg">Everything stays on your machine.</PostIt>
        <p className="type-dateline">127.0.0.1 only.</p>
      </div>
    </aside>
  )
}
```

### Sidebar CSS

```css
.doc-shell {
  max-width: 1320px;
  margin: 0 auto;
  padding: 32px 24px 72px;
  display: grid;
  grid-template-columns: 270px minmax(0, 1fr);
  gap: 28px;
}

.doc-sidebar {
  position: sticky;
  top: 96px;
  align-self: start;
  min-height: calc(100vh - 128px);
  padding: 28px 24px;
  border-radius: 18px;
  background: rgba(255, 255, 255, 0.58);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.14), inset 0 1px 0 rgba(255, 255, 255, 0.82);
}

.doc-sidebar__label {
  margin-bottom: 18px;
}

.doc-sidebar__nav {
  display: grid;
  gap: 8px;
  margin-top: 22px;
}

.doc-sidebar__link {
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 44px;
  padding: 0 12px;
  border-radius: 8px;
  color: var(--color-ink);
  font-family: var(--font-serif);
  font-size: 17px;
  font-weight: 650;
  line-height: 1;
  text-decoration: none;
  transition: background 180ms ease, color 180ms ease, box-shadow 180ms ease;
}

.doc-sidebar__link:hover,
.doc-sidebar__link.is-active {
  background: rgba(107, 91, 149, 0.12);
  color: var(--color-purple);
  box-shadow: inset 3px 0 0 var(--color-purple);
}

.doc-sidebar__note {
  position: absolute;
  left: 24px;
  right: 24px;
  bottom: 28px;
  min-height: 112px;
  padding: 18px;
  border: 1px solid rgba(26, 26, 46, 0.12);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.32);
}

.doc-sidebar__note .post-it {
  position: relative;
  display: inline-block;
  margin: 0 0 16px 8px;
}
```

### Content Typography Scale

```css
.doc-content {
  min-height: calc(100vh - 128px);
  padding: clamp(34px, 5vw, 56px);
  border-radius: 18px;
  background: rgba(255, 255, 255, 0.84);
  backdrop-filter: blur(18px) saturate(1.08);
  -webkit-backdrop-filter: blur(18px) saturate(1.08);
  border: 1px solid rgba(255, 255, 255, 0.72);
  box-shadow: 0 20px 60px rgba(42, 38, 66, 0.14), inset 0 1px 0 rgba(255, 255, 255, 0.84);
}

.doc-content__inner {
  max-width: 74ch;
}

.doc-breadcrumbs {
  margin-bottom: 22px;
  font-family: var(--font-serif);
  font-size: 14px;
  font-weight: 650;
  line-height: 1.3;
  color: var(--color-purple);
}

.doc-content h1 {
  margin: 0 0 22px;
  padding-bottom: 18px;
  border-bottom: 3px double rgba(26, 26, 46, 0.24);
  font-family: var(--font-serif);
  font-size: clamp(48px, 6vw, 86px);
  font-weight: 700;
  line-height: 0.92;
  letter-spacing: 0;
  color: var(--color-ink);
}

.doc-content h2 {
  margin: 42px 0 14px;
  padding-bottom: 9px;
  border-bottom: 1px solid rgba(26, 26, 46, 0.18);
  font-family: var(--font-serif);
  font-size: 30px;
  font-weight: 700;
  line-height: 1;
  color: var(--color-ink);
}

.doc-content h3 {
  margin: 30px 0 10px;
  font-family: var(--font-serif);
  font-size: 22px;
  font-weight: 700;
  line-height: 1.08;
  color: var(--color-ink);
}

.doc-content p,
.doc-content li {
  font-family: var(--font-sans);
  font-size: 16px;
  font-weight: 450;
  line-height: 1.7;
  color: var(--color-body);
}

.doc-content figcaption,
.doc-caption {
  font-family: var(--font-sans);
  font-size: 12px;
  font-weight: 500;
  line-height: 1.35;
  letter-spacing: 0.01em;
  color: var(--color-muted);
}
```

### Code Block Styling

Use a dark glass panel, not flat black.

```css
.doc-content pre {
  position: relative;
  margin: 24px 0;
  padding: 18px 20px;
  overflow-x: auto;
  border-radius: 10px;
  border: 1px solid rgba(255, 255, 255, 0.16);
  background:
    linear-gradient(180deg, rgba(26, 26, 46, 0.94), rgba(26, 26, 46, 0.88)),
    rgba(26, 26, 46, 0.92);
  box-shadow: 0 18px 42px rgba(26, 26, 46, 0.20), inset 0 1px 0 rgba(255, 255, 255, 0.08);
}

.doc-content code {
  font-family: var(--font-mono);
  font-size: 14px;
  font-weight: 500;
  line-height: 1.65;
}

.doc-content :not(pre) > code {
  padding: 0.12em 0.36em;
  border-radius: 4px;
  background: rgba(26, 26, 46, 0.06);
  color: var(--color-purple);
}
```

Syntax highlight palette:

- Plain text: `#d4d0e8`
- Comments: `#8fa58b`
- Keywords: `#c5a3d8`
- Strings: `#a8d0a0`
- Numbers: `#f0c38e`
- Function names: `#d9c1ff`
- Punctuation: `#a7a4bc`
- Error or removal lines: `#e2a3a7`
- Insertions or success lines: `#a8d0a0`

### Table Styling

```css
.doc-content table {
  width: 100%;
  margin: 22px 0 28px;
  border-collapse: separate;
  border-spacing: 0;
  overflow: hidden;
  border: 1px solid rgba(107, 91, 149, 0.28);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.38);
  font-size: 14px;
}

.doc-content th {
  padding: 10px 12px;
  text-align: left;
  background: rgba(107, 91, 149, 0.10);
  border-bottom: 1px solid rgba(107, 91, 149, 0.28);
  color: var(--color-ink);
  font-family: var(--font-serif);
  font-size: 17px;
  font-weight: 700;
  line-height: 1.2;
}

.doc-content td {
  padding: 10px 12px;
  border-bottom: 1px solid rgba(26, 26, 46, 0.10);
  color: var(--color-body);
  font-family: var(--font-sans);
  font-size: 14px;
  font-weight: 450;
  line-height: 1.45;
}

.doc-content tr:nth-child(even) td {
  background: rgba(255, 255, 255, 0.24);
}

.doc-content tr:last-child td {
  border-bottom: 0;
}
```

The `Active Memory Hooks` page table should use these columns:

- Hook Name
- Trigger
- Input
- Output

### Breadcrumbs And Inter-Page Navigation

Use both breadcrumbs and prev/next links:

- Breadcrumbs at the top orient the reader inside the docs tree.
- Prev/next links at the bottom support linear reading.

Breadcrumb JSX:

```jsx
function Breadcrumbs() {
  return (
    <nav className="doc-breadcrumbs" aria-label="Breadcrumb">
      <Link to="/">Claude Self-Reflect</Link>
      <span aria-hidden="true"> &gt; </span>
      <Link to="/docs/hooks">Hooks</Link>
      <span aria-hidden="true"> &gt; </span>
      <span>Active Memory Hooks</span>
    </nav>
  )
}
```

Prev/next CSS:

```css
.doc-pager {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 16px;
  margin-top: 48px;
  padding-top: 22px;
  border-top: 1px solid rgba(26, 26, 46, 0.13);
}

.doc-pager__link {
  min-height: 82px;
  padding: 16px;
  border-radius: 10px;
  border: 1px solid rgba(255, 255, 255, 0.68);
  background: rgba(255, 255, 255, 0.44);
  color: var(--color-ink);
  text-decoration: none;
}

.doc-pager__label {
  display: block;
  margin-bottom: 8px;
  font-family: var(--font-mono);
  font-size: 11px;
  font-weight: 500;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--color-muted);
}

.doc-pager__title {
  font-family: var(--font-serif);
  font-size: 20px;
  font-weight: 700;
  line-height: 1;
  color: var(--color-ink);
}
```

### Bento Card Thumbnails In Doc Pages

Use mini bento thumbnails as section headers when a doc section corresponds to a landing-page concept. Keep them compact and semantic.

```jsx
function DocSectionHeader({ number, title, caption, accent = 'purple', children }) {
  return (
    <header className={`doc-section-card doc-section-card--${accent}`}>
      <div>
        <p className="section-number">{number}</p>
        <h2>{title}</h2>
        <p className="doc-caption">{caption}</p>
      </div>
      <div className="doc-section-card__viz" aria-hidden="true">
        {children}
      </div>
    </header>
  )
}
```

```css
.doc-section-card {
  margin: 38px 0 18px;
  padding: 18px;
  display: grid;
  grid-template-columns: minmax(0, 1fr) 180px;
  gap: 18px;
  border-radius: 12px;
  border: 1px solid rgba(255, 255, 255, 0.72);
  background: rgba(255, 255, 255, 0.58);
  backdrop-filter: blur(14px) saturate(1.06);
  -webkit-backdrop-filter: blur(14px) saturate(1.06);
  box-shadow: 0 14px 34px rgba(42, 38, 66, 0.10), inset 0 1px 0 rgba(255, 255, 255, 0.76);
}

.doc-section-card h2 {
  margin: 6px 0 8px;
  padding: 0;
  border: 0;
}

.doc-section-card--purple .section-number {
  color: var(--color-purple);
}

.doc-section-card--rose .section-number {
  color: var(--color-rose);
}

.doc-section-card--sage .section-number {
  color: var(--color-sage);
}
```

Example for hook docs:

```jsx
<DocSectionHeader
  number="02"
  title="PreToolUse"
  caption="Capture intent and context before a tool runs."
  accent="purple"
>
  <svg viewBox="0 0 160 72">
    <path d="M22 38 H138" stroke="rgba(107,91,149,0.32)" />
    <circle cx="38" cy="38" r="5" fill="var(--color-purple)" />
    <circle cx="82" cy="38" r="5" fill="var(--color-sage)" />
    <circle cx="126" cy="38" r="5" fill="var(--color-purple)" />
  </svg>
</DocSectionHeader>
```

### Inner Page Responsive Rules

```css
@media (max-width: 768px) {
  .doc-shell {
    grid-template-columns: 1fr;
    padding: 20px 18px 60px;
  }

  .doc-sidebar {
    position: relative;
    top: auto;
    min-height: auto;
  }

  .doc-sidebar__note {
    position: relative;
    left: auto;
    right: auto;
    bottom: auto;
    margin-top: 24px;
  }

  .doc-content {
    padding: 32px 22px;
  }

  .doc-pager {
    grid-template-columns: 1fr;
  }
}

@media (max-width: 480px) {
  .doc-shell {
    padding: 14px 14px 48px;
    gap: 14px;
  }

  .doc-content {
    padding: 24px 16px;
    border-radius: 14px;
    backdrop-filter: blur(12px) saturate(1.04);
    -webkit-backdrop-filter: blur(12px) saturate(1.04);
  }

  .doc-content h1 {
    font-size: clamp(38px, 14vw, 54px);
  }

  .doc-section-card {
    grid-template-columns: 1fr;
  }
}
```
