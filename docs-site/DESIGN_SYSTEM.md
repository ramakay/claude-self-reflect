# CSR Documentation Site — Design System

> Reference comp: `public/images/csr-bento-comp.png`

## Design Direction: Editorial Glassmorphic Bento

**Anti-patterns (BANNED):**
- Gradient text with hero sections
- Neon/glow effects
- Dark mode SaaS archetype
- React component library aesthetics
- "Get Started" buttons with arrow icons on gradient backgrounds

**Design language:**
- Newspaper/editorial — each card tells a story
- Glassmorphism on serene sky
- Microcharts and sparklines with real data
- Post-it notes as gentle ambient elements
- Staggered reveal animations

## Typography

| Role | Font | Weight | Size | Color |
|------|------|--------|------|-------|
| **Hero background** | Playfair Display | 900 | 6-8vw | #2a2a3e / 20% opacity |
| **Card headlines** | Playfair Display | 700 | 1.25rem | #1a1a2e |
| **Card body** | Inter | 400 | 0.875rem | #4a4a6a |
| **Metrics** | JetBrains Mono | 500 | 1.75rem | #1a1a2e |
| **Labels** | Inter | 500 | 0.7rem | #8a8aaa |
| **Post-it text** | Caveat (handwritten) | 400 | 0.8rem | #4a4a6a |

## Color Palette

| Name | Hex | Usage |
|------|-----|-------|
| Sky top | #e8e4f0 | Background gradient start |
| Sky bottom | #c8cae0 | Background gradient end |
| Glass white | rgba(255,255,255,0.65) | Card background |
| Glass border | rgba(255,255,255,0.3) | Card border |
| Charcoal | #1a1a2e | Headlines |
| Slate | #4a4a6a | Body text |
| Muted purple | #6b5b95 | Primary accent |
| Dusty rose | #b5838d | Secondary accent |
| Sage green | #7c9473 | Positive/success |
| Amber cream | #d4a574 | Warning/optional |
| Post-it lavender | #d8d0e8 | Post-it bg |
| Post-it rose | #e8c8d0 | Post-it bg |
| Post-it sage | #c8dcc0 | Post-it bg |
| Post-it amber | #e8dcc0 | Post-it bg |

## Card Styles

### Glassmorphic Card
```css
.glass-card {
  background: rgba(255, 255, 255, 0.55);
  backdrop-filter: blur(16px);
  -webkit-backdrop-filter: blur(16px);
  border: 1px solid rgba(255, 255, 255, 0.3);
  border-radius: 1.25rem;
  box-shadow: 0 4px 30px rgba(0, 0, 0, 0.05);
}
```

### Post-it Note
```css
.post-it {
  background: linear-gradient(135deg, #d8d0e8 0%, #c8c0d8 100%);
  border-radius: 4px;
  padding: 0.5rem 0.75rem;
  font-family: 'Caveat', cursive;
  transform: rotate(-3deg);
  box-shadow: 2px 2px 8px rgba(0,0,0,0.08);
}
```

## Bento Grid Layout (8 cards)

```
┌───────────┬──────────┬─────────────┐
│           │          │             │
│  FORGET   │  0.7ms   │  6 HOOKS    │
│  PROBLEM  │ latency  │  timeline   │
│  (tall)   │          │             │
├───────────┼──────────┼─────────────┤
│           │ WANTED:  │ ENRICHMENT  │
│  IMPORT   │ INSTALL  │  PIPELINE   │
│  STATS    │ (ad)     │  3 layers   │
└───────────┴──────────┴─────────────┘
```

## Animation Choreography

| Element | Animation | Delay | Duration | Easing |
|---------|-----------|-------|----------|--------|
| Hero text | fade-in + slight scale | 0ms | 800ms | ease-out |
| Card row 1 | slide-up + fade | 200ms stagger | 600ms | cubic-bezier(0.16,1,0.3,1) |
| Card row 2 | slide-up + fade | 400ms stagger | 600ms | cubic-bezier(0.16,1,0.3,1) |
| Sparklines | draw-in (path animation) | 800ms | 1200ms | ease-in-out |
| Progress bars | width grow | 600ms | 800ms | ease-out |
| Hook dots | pulse-in (scale 0→1) | 100ms stagger each | 400ms | spring |
| Post-its | float-in + rotate | 1000ms | 500ms | ease-out |

## Microcharts

All sparklines use SVG `<polyline>` with real data points:
- Search latency: [2.1, 1.8, 1.2, 0.9, 0.8, 0.7, 0.7] — declining trend
- Memory retention: [100, 85, 60, 30, 10, 0] — decay curve
- Import progress: filled bar at 100%

## Interaction

- **Hover on card**: subtle lift (translateY -2px) + border brightens
- **Hover on post-it**: slight rotation change + scale 1.05
- **Scroll**: cards revealed via IntersectionObserver with stagger
- **Cursor**: default (no custom cursor for v1 — keep it clean)

## File Structure

```
docs-site/
├── src/
│   ├── pages/Landing.jsx      # Bento dashboard
│   ├── components/
│   │   ├── GlassCard.jsx      # Reusable glass card
│   │   ├── PostIt.jsx         # Post-it note component
│   │   ├── Sparkline.jsx      # SVG sparkline
│   │   ├── ProgressBar.jsx    # Animated progress bar
│   │   └── HookTimeline.jsx   # 6-dot hook visualization
│   └── hooks/useInView.js     # IntersectionObserver hook
├── public/images/
│   ├── design-comp-claude-1.png
│   ├── csr-bento-comp.png     # Primary reference comp
│   └── ...
└── DESIGN_SYSTEM.md           # This file
```
