// The herdr pane's cards, laid out as shapes for the painter: the dreamer, Same page?, Should be a
// skill / memory / tool, Where the work stands, the limits with Self-Reflect's own line, and the
// session timeline. Units: one terminal column is 12 units wide, one row is 12 x the cell shape
// tall, so a card is a whole number of rows and the picture keeps its shape.
//
// Titles: Newsreader SemiBold, capitals 1.27 times the body's, as measured in the herdr pane
// (instrument-dark.png: title capitals 17-18 px over body capitals 13-14 px); the limits headline
// has the body's capital height in the serif, as there.
// Type is sized for the terminal it sits in. A text size here is the line box of the font
// (ascent plus descent), not its em. Measured in iTerm2 on the Studio (2026-11-15): the terminal's
// own capitals are 18 px tall at 14 px a column; body text at 27 units paints capitals 18 px tall
// at the same scale, so body text is as tall as the terminal's letters beside it.
import type { Context, Dreamer, Limits, SamePage, ShouldBe, Story } from './pieces'
import { tokens } from './pieces'
import { textWidth } from './paint'
import type { Rgb, Scene, Shape } from './paint'
import type { Palette } from './theme'
import type { Facts } from './transcript'

export const COLUMN = 12
export const TITLE = 32
export const TITLE_LINE = 42
export const HEADLINE = 25
export const BODY = 27
export const BODY_LINE = 34
export const CAPTION = 23
export const CAPTION_LINE = 30
/** The aurora shows this far at each side of a card, and above and below it (half the gap). */
const MARGIN = 7
const PAD = 16
const GAP = 6
const RADIUS = 16

/**
 * The type metrics a card is laid out with. Pixels and SVG use the measured sizes above; the cell
 * view (cells.ts) lays the same cards out on the terminal's grid: every line one row, every
 * character one column, in the terminal's own font.
 */
export type Metrics = {
  mono: boolean
  TITLE: number
  TITLE_LINE: number
  HEADLINE: number
  BODY: number
  BODY_LINE: number
  CAPTION: number
  CAPTION_LINE: number
  PAD_Y: number
}

const PIXEL: Metrics = { mono: false, TITLE, TITLE_LINE, HEADLINE, BODY, BODY_LINE, CAPTION, CAPTION_LINE, PAD_Y: 14 }
let M: Metrics = PIXEL

/** Grid metrics for a cell `cell` units tall: one line a row; row 0 the gap, row 1 the card's top, words from row 2. */
export function gridMetrics(cell: number): Metrics {
  return { mono: true, TITLE: cell, TITLE_LINE: cell, HEADLINE: cell, BODY: cell, BODY_LINE: cell, CAPTION: cell, CAPTION_LINE: cell, PAD_Y: 2 * cell - GAP }
}

/** Runs `build` with `metrics`, then puts the pixel metrics back. */
export function withMetrics<T>(metrics: Metrics, build: () => T): T {
  const had = M
  M = metrics
  try {
    return build()
  } finally {
    M = had
  }
}

/** Whether the cards are being laid out on the terminal's grid. */
export const isGrid = (): boolean => M.mono

/** How wide `text` is: the atlas's advances for pixels, one column a character on the grid. */
function measure(text: string, size: number, face: Face, spacing = 0): number {
  return M.mono ? Array.from(text).length * COLUMN : textWidth(text, size, face, spacing)
}

/** What moves on the cards, at this moment: each eased value as shown now. */
export type Motion = {
  /** Turns of the dreamer's ring while a look runs. */
  spin: number
  /** Eased bar percents and the context fill, by name. */
  eased: Record<string, number>
  /** 0 to 1: how far the newest compaction's drop is drawn; 1 when none is new. */
  drop: number
  /** 0 to 1: the glow on the newest recall; 0 when none is new. */
  recall: number
}

export const STILL: Motion = { spin: 0, eased: {}, drop: 1, recall: 0 }

/** One painted card: its scene and the terminal rows it takes. */
export type Painted = { scene: Scene; rows: number; alt: string }

// ---- words ------------------------------------------------------------------------------------------

type Face = 'sans' | 'bold' | 'serif'

/** `text` cut to `width`, ending in "…" when cut. */
export function fit(text: string, size: number, face: Face, width: number): string {
  if (measure(text, size, face) <= width) return text
  const chars = Array.from(text)
  let lo = 0
  let hi = chars.length
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2)
    if (measure(chars.slice(0, mid).join('').trimEnd() + '…', size, face) <= width) lo = mid
    else hi = mid - 1
  }
  return chars.slice(0, lo).join('').trimEnd() + '…'
}

/** `text` in lines no wider than `width`; past `most` lines the last ends in "…". */
function wrapRaw(text: string, size: number, face: Face, width: number, most = 99): string[] {
  const words = text.split(/\s+/).filter(w => w !== '')
  const lines: string[] = []
  let line = ''
  for (let i = 0; i < words.length; i++) {
    const word = words[i] ?? ''
    const next = line === '' ? word : `${line} ${word}`
    if (measure(next, size, face) <= width || line === '') {
      line = measure(next, size, face) <= width ? next : fit(next, size, face, width)
      continue
    }
    lines.push(line)
    line = word
    if (lines.length === most) {
      const rest = [lines.pop() ?? '', ...words.slice(i)].join(' ')
      lines.push(fit(rest, size, face, width))
      return lines
    }
  }
  if (line !== '') lines.push(line)
  return lines
}

/**
 * `text` in lines no wider than `width`, past `most` lines cut with "…"; where the last line would
 * hold one word alone, the lines are balanced (the narrowest width that keeps their count).
 */
export function wrap(text: string, size: number, face: Face, width: number, most = 99): string[] {
  const lines = wrapRaw(text, size, face, width, most)
  const last = lines[lines.length - 1] ?? ''
  if (lines.length < 2 || last.includes(' ') || last.endsWith('…')) return lines
  let lo = width * 0.55
  let hi = width
  for (let i = 0; i < 12; i++) {
    const mid = (lo + hi) / 2
    const tried = wrapRaw(text, size, face, mid, most)
    if (tried.length === lines.length && !(tried[tried.length - 1] ?? '').endsWith('…')) hi = mid
    else lo = mid
  }
  return wrapRaw(text, size, face, hi, most)
}

/** A run of words in one colour, for a line made of several. */
export type Run = { text: string; color: Rgb; face?: Face }

/** Runs flowed into lines no wider than `width`, each word keeping its run's colour. */
export function wrapRuns(runs: Run[], size: number, width: number): Run[][] {
  const lines: Run[][] = []
  let line: Run[] = []
  let used = 0
  for (const run of runs) {
    for (const word of run.text.split(/\s+/).filter(w => w !== '')) {
      const face = run.face ?? 'sans'
      const gap = line.length > 0 ? measure(' ', size, face) : 0
      const w = measure(word, size, face)
      if (line.length > 0 && used + gap + w > width) {
        lines.push(line)
        line = []
        used = 0
      }
      const last = line[line.length - 1]
      if (last && last.color === run.color && (last.face ?? 'sans') === face) last.text += ' ' + word
      else line.push({ text: (line.length > 0 ? ' ' : '') + word, color: run.color, face })
      used += (line.length > 1 || last ? gap : 0) + w
    }
  }
  if (line.length > 0) lines.push(line)
  return lines
}

// ---- building blocks ---------------------------------------------------------------------------------

class Card {
  shapes: Shape[] = []
  private top = GAP + M.PAD_Y
  /** Where the next line starts. On the grid it snaps to the nearest row, so marks and words share rows. */
  get y(): number {
    return this.top
  }
  set y(value: number) {
    this.top = M.mono ? Math.round(value / M.BODY_LINE) * M.BODY_LINE : value
  }
  constructor(
    readonly width: number,
    readonly p: Palette,
  ) {}

  get left(): number {
    return MARGIN + PAD
  }
  get right(): number {
    return this.width - MARGIN - PAD
  }
  get inner(): number {
    return this.right - this.left
  }

  text(x: number, baseline: number, text: string, size: number, color: Rgb, face: Face = 'sans', anchor: 'start' | 'end' | 'middle' = 'start'): number {
    if (text !== '') this.shapes.push({ kind: 'text', x, y: baseline, text, size, face, color, anchor })
    return measure(text, size, face)
  }

  /** On the grid: one character in the row that holds `cy`. */
  glyph(x: number, cy: number, ch: string, color: Rgb, back?: Rgb): void {
    this.shapes.push({ kind: 'text', x, y: cy, text: ch, size: M.BODY, face: 'sans', color, back })
  }

  /**
   * On the grid: a bar from `x0` to `x1` in the row holding `cy`, the lower half of the row (▄)
   * so two bars on two rows stay apart: each part in its colour, the rest the dim track. Seen in
   * Warp: full-height blocks join with no gap, so two stacked bars read as one block; and a
   * left-eighth end (▏ to ▉) is full height, so the end is whole cells.
   */
  gridBar(x0: number, x1: number, cy: number, parts: { share: number; color: Rgb }[], track: Rgb): void {
    const cols = Math.max(1, Math.floor((x1 - x0) / COLUMN))
    const ends: { at: number; color: Rgb }[] = []
    let at = 0
    for (const part of parts) {
      at += Math.max(0, part.share) * cols
      ends.push({ at: Math.min(at, cols), color: part.color })
    }
    const total = ends[ends.length - 1]?.at ?? 0
    // A share above zero shows at least one cell.
    const filled = total > 0 ? Math.max(1, Math.round(total)) : 0
    for (let col = 0; col < cols; col++) {
      const part = col < filled ? (ends.find(e => e.at > col + 0.5) ?? ends[ends.length - 1]) : undefined
      this.glyph(x0 + col * COLUMN, cy, '▄', part?.color ?? track)
    }
  }

  /** One line of runs starting at `x`. */
  runs(x: number, baseline: number, runs: Run[], size: number): void {
    let pen = x
    for (const r of runs) pen += this.text(pen, baseline, r.text, size, r.color, r.face ?? 'sans')
  }

  /** Lines of `size` type, each `line` units apart; moves down past them. */
  lines(lines: string[], size: number, line: number, color: Rgb, x = this.left): void {
    for (const l of lines) {
      this.text(x, this.y + line * 0.74, l, size, color)
      this.y += line
    }
  }

  /** Closes the card: its ground first, then the rows it takes at `cell` units a row. */
  close(cell: number, alt: string): Painted {
    if (M.mono) {
      // On the grid: one row of the card's fill under the last line; the next card's gap row follows.
      const rows = Math.round(this.y / cell) + 1
      return { scene: { width: this.width, height: rows * cell, shapes: this.shapes }, rows, alt }
    }
    const height = Math.max(cell, Math.ceil((this.y + M.PAD_Y + GAP) / cell) * cell)
    const p = this.p
    const x = MARGIN
    const w = this.width - 2 * MARGIN
    const h = height - 2 * GAP
    // The herdr pane's glass card: its shadow, the tinted glass over the aurora, a hairline, and a
    // line of light along its top edge.
    const ground: Shape[] = [
      { kind: 'shade', x, y: GAP + p.shadow.y / 2, w, h, r: RADIUS, blur: p.shadow.blur / 2, color: p.shadow.color, alpha: p.shadow.alpha },
      { kind: 'rect', x, y: GAP, w, h, r: RADIUS, color: p.glass[0], color2: p.glass[1], alpha: p.glassAlpha },
      { kind: 'rect', x, y: GAP, w, h, r: RADIUS, stroke: 1, color: p.hair, alpha: p.hairAlpha },
      { kind: 'line', x1: x + RADIUS, y1: GAP + 0.75, x2: x + w - RADIUS, y2: GAP + 0.75, width: 1, color: p.highlight, alpha: p.highlightAlpha },
    ]
    return { scene: { width: this.width, height, shapes: [...ground, ...this.shapes] }, rows: Math.round(height / cell), alt }
  }
}

/** A serif title with an icon before it; returns where the words after it may start. */
function title(card: Card, words: string, icon: ((x: number, cy: number) => number) | null, size = M.TITLE): number {
  const baseline = card.y + M.TITLE_LINE * 0.74
  let x = card.left
  if (icon) x += icon(x, baseline - size * 0.33) + 8
  const w = card.text(x, baseline, fit(words, size, 'serif', card.right - x), size, card.p.ink, 'serif')
  return x + w
}

// ---- icons, drawn as shapes (the herdr pane's own marks, not a font's) --------------------------------

function moonMark(card: Card, x: number, cy: number, color: Rgb): void {
  if (M.mono) {
    card.glyph(x, cy, '☾', color)
    return
  }
  card.shapes.push({ kind: 'moon', x, y: cy + 1, r: 11, bx: x + 7, by: cy - 5, br: 9.5, color })
  card.shapes.push({ kind: 'line', x1: x + 9, y1: cy - 11, x2: x + 9, y2: cy - 3, width: 1.8, color })
  card.shapes.push({ kind: 'line', x1: x + 5, y1: cy - 7, x2: x + 13, y2: cy - 7, width: 1.8, color })
}

/** The 8-bit Claude of the Same page? card, 11 by 8 cells. */
const CRITTER = ['..XXXXXXX..', '..X.XXX.X..', 'XXXXXXXXXXX', 'XXXXXXXXXXX', '..XXXXXXX..', '..X.X.X.X..', '..X.X.X.X..', '...........']

function critter(card: Card, x: number, cy: number): number {
  if (M.mono) {
    card.glyph(x, cy, '▞', card.p.claude)
    return 2 * COLUMN
  }
  const cell = 2.6
  const top = cy - (CRITTER.length * cell) / 2 + 2
  CRITTER.forEach((row, j) => {
    Array.from(row).forEach((ch, i) => {
      if (ch === 'X') card.shapes.push({ kind: 'rect', x: x + i * cell, y: top + j * cell, w: cell + 0.3, h: cell + 0.3, r: 0, color: card.p.claude })
    })
  })
  return 11 * cell
}

function bulb(card: Card, x: number, cy: number): number {
  if (M.mono) {
    card.glyph(x, cy, '◌', card.p.quiet)
    return 2 * COLUMN
  }
  const c = card.p.quiet
  card.shapes.push({ kind: 'ring', x: x + 8, y: cy - 3, r: 6.5, width: 1.7, color: c })
  card.shapes.push({ kind: 'line', x1: x + 5, y1: cy + 6, x2: x + 11, y2: cy + 6, width: 1.7, color: c })
  card.shapes.push({ kind: 'line', x1: x + 6, y1: cy + 10, x2: x + 10, y2: cy + 10, width: 1.7, color: c })
  return 16
}

function scroll(card: Card, x: number, cy: number): number {
  if (M.mono) {
    card.glyph(x, cy, '≡', card.p.quiet)
    return 2 * COLUMN
  }
  const c = card.p.quiet
  card.shapes.push({ kind: 'rect', x: x + 2, y: cy - 9, w: 14, h: 17, r: 2.5, stroke: 1.6, color: c })
  card.shapes.push({ kind: 'line', x1: x + 6, y1: cy - 4, x2: x + 12, y2: cy - 4, width: 1.4, color: c })
  card.shapes.push({ kind: 'line', x1: x + 6, y1: cy, x2: x + 12, y2: cy, width: 1.4, color: c })
  card.shapes.push({ kind: 'line', x1: x + 6, y1: cy + 4, x2: x + 10, y2: cy + 4, width: 1.4, color: c })
  return 18
}

function fileMark(card: Card, x: number, cy: number, color: Rgb): number {
  if (M.mono) {
    card.glyph(x, cy, '▫', color)
    return 2 * COLUMN
  }
  card.shapes.push({ kind: 'rect', x, y: cy - 7, w: 11, h: 14, r: 2, stroke: 1.3, color })
  card.shapes.push({ kind: 'line', x1: x + 3, y1: cy - 1, x2: x + 8, y2: cy - 1, width: 1.2, color })
  card.shapes.push({ kind: 'line', x1: x + 3, y1: cy + 3, x2: x + 8, y2: cy + 3, width: 1.2, color })
  return 16
}

function commitMark(card: Card, x: number, cy: number, color: Rgb): number {
  if (M.mono) {
    card.glyph(x, cy, '⊸', color)
    return 2 * COLUMN
  }
  card.shapes.push({ kind: 'line', x1: x, y1: cy, x2: x + 16, y2: cy, width: 1.4, color })
  card.shapes.push({ kind: 'disc', x: x + 8, y: cy, r: 4.2, color: card.p.card })
  card.shapes.push({ kind: 'ring', x: x + 8, y: cy, r: 3.6, width: 1.4, color })
  return 20
}

function refreshMark(card: Card, x: number, cy: number, color: Rgb): number {
  if (M.mono) {
    card.glyph(x, cy, '↻', color)
    return 2 * COLUMN
  }
  card.shapes.push({ kind: 'arc', x: x + 7, y: cy, r: 5.5, from: 0.08, to: 0.85, width: 1.4, color })
  card.shapes.push({ kind: 'line', x1: x + 7, y1: cy - 5.5, x2: x + 10.5, y2: cy - 8, width: 1.4, color })
  card.shapes.push({ kind: 'line', x1: x + 7, y1: cy - 5.5, x2: x + 10, y2: cy - 2.5, width: 1.4, color })
  return 18
}

function checkMark(card: Card, x: number, cy: number, color: Rgb): number {
  if (M.mono) {
    card.glyph(x, cy, '✓', color)
    return 2 * COLUMN
  }
  card.shapes.push({ kind: 'line', x1: x, y1: cy, x2: x + 4, y2: cy + 4, width: 1.8, color })
  card.shapes.push({ kind: 'line', x1: x + 4, y1: cy + 4, x2: x + 11, y2: cy - 4, width: 1.8, color })
  return 15
}

// ---- the cards ----------------------------------------------------------------------------------------

/** 1. The dreamer: its title and state, the moon, the budget ring, and what the last look found. */
export function dreamerCard(d: Dreamer, width: number, cell: number, p: Palette, m: Motion): Painted {
  const card = new Card(width, p)
  const end = title(card, d.title, null)
  const moonX = card.right - 14
  moonMark(card, moonX, card.y + M.TITLE_LINE * 0.5, p.gold)
  const tag = d.tag === '' ? '' : ` · ${d.tag}`
  // The state rides the title line, as in herdr: body size where it fits, a caption where only that fits.
  const tagSize = end + measure(tag, M.BODY, 'sans') <= moonX - 20 ? M.BODY : end + measure(tag, M.CAPTION, 'sans') <= moonX - 20 ? M.CAPTION : 0
  const tagFits = tag !== '' && tagSize > 0
  if (tagFits) card.text(end, card.y + M.TITLE_LINE * 0.74, tag, tagSize, p.gold)
  card.y += M.TITLE_LINE + 4
  if (tag !== '' && !tagFits) {
    card.lines([d.tag], M.BODY, M.BODY_LINE, p.gold)
  }
  if (d.question !== '') card.lines(wrap(d.question, M.BODY, 'sans', card.inner, 4), M.BODY, M.BODY_LINE, p.ink)
  if (d.budget !== '') {
    const cy = card.y + M.BODY_LINE * 0.5
    const r = 8
    if (M.mono) {
      // The budget as a quarter-step disc; while a look runs, a half disc that turns.
      const spinGlyphs = ['◐', '◓', '◑', '◒']
      const shares = ['○', '◔', '◑', '◕', '●']
      const ch = d.dreaming ? (spinGlyphs[Math.floor((m.spin % 1) * 4)] ?? '◐') : (shares[Math.round(d.fraction * 4)] ?? '○')
      card.glyph(card.left, cy, ch, p.gold)
      card.text(card.left + 2 * COLUMN, card.y + M.BODY_LINE * 0.74, d.budget, M.BODY, p.quiet)
      card.y += M.BODY_LINE
    } else {
    card.shapes.push({ kind: 'ring', x: card.left + r, y: cy, r, width: 2.6, color: p.track })
    const share = Math.max(d.fraction, 0.02)
    const turn = d.dreaming ? m.spin % 1 : 0
    const span = d.dreaming ? Math.max(share, 0.28) : share
    card.shapes.push({ kind: 'arc', x: card.left + r, y: cy, r, from: turn, to: turn + span, width: 2.6, color: p.gold })
    card.text(card.left + 2 * r + 9, card.y + M.BODY_LINE * 0.74, d.budget, M.BODY, p.quiet)
    card.y += M.BODY_LINE
    }
  }
  if (d.lastLook !== '') card.lines(wrap(d.lastLook, M.BODY, 'sans', card.inner, 2), M.BODY, M.BODY_LINE, p.quiet)
  return card.close(cell, [d.title, d.tag, d.question, d.budget && `dreamer ${d.budget}`, d.lastLook].filter(Boolean).join('. '))
}

/** 2. Same page?: the mark, the model's read, its dim second line. `hint` names the key that opens it. */
export function samePageCard(s: SamePage, width: number, cell: number, p: Palette, hint: string): Painted {
  const card = new Card(width, p)
  const end = title(card, 'Same page?', (x, cy) => critter(card, x, cy))
  if (hint !== '' && end + measure(hint, M.CAPTION, 'sans') + 12 <= card.right) {
    card.text(card.right, card.y + M.TITLE_LINE * 0.74, hint, M.CAPTION, p.faint, 'sans', 'end')
  }
  card.y += M.TITLE_LINE + 4
  card.lines(wrap(s.read, M.BODY, 'sans', card.inner, 2), M.BODY, M.BODY_LINE, p.ink)
  if (s.more !== '') card.lines(wrap(s.more, M.BODY, 'sans', card.inner, 2), M.BODY, M.BODY_LINE, p.quiet)
  return card.close(cell, `Same page? ${s.read} ${s.more}`)
}

/** 3. Should be a skill / memory / tool: the count, the first one's kind, its words, the evidence mark. */
export function shouldBeCard(items: ShouldBe[], width: number, cell: number, p: Palette, hint: string): Painted {
  const card = new Card(width, p)
  const end = title(card, 'Should be a skill / memory / tool', (x, cy) => bulb(card, x, cy))
  const count = `1 of ${items.length}${hint === '' ? '' : ` · ${hint}`}`
  const countFits = end + measure(count, M.CAPTION, 'sans') + 12 <= card.right
  if (countFits) card.text(card.right, card.y + M.TITLE_LINE * 0.74, count, M.CAPTION, p.faint, 'sans', 'end')
  card.y += M.TITLE_LINE + 4
  const item = items[0]
  if (item) {
    // The kind tag is in the dreamer's gold for every kind, as the herdr pane draws it.
    const ink = p.gold
    const kindW = Math.max(measure('memory', M.BODY, 'sans'), measure(item.kind, M.BODY, 'sans')) + 14
    const evidence = 'evidence'
    const evW = measure(evidence, M.CAPTION, 'sans') + 18
    card.text(card.left, card.y + M.BODY_LINE * 0.74, item.kind, M.BODY, ink)
    const lines = wrap(item.text, M.BODY, 'sans', card.inner - kindW - evW - 8, 3)
    const top = card.y
    card.lines(lines, M.BODY, M.BODY_LINE, p.ink, card.left + kindW)
    fileMark(card, card.right - evW + 2, top + M.BODY_LINE * 0.5, p.faint)
    card.text(card.right, top + M.BODY_LINE * 0.74, evidence, M.CAPTION, p.faint, 'sans', 'end')
  }
  if (!countFits) card.lines([count], M.CAPTION, M.CAPTION_LINE, p.faint)
  return card.close(cell, `Should be a skill, memory or tool: ${items.length}. ${item ? `${item.kind}: ${item.text}` : ''}`)
}

/** 4. Where the work stands: when it was refreshed, the story, the notes kept, the code rows. */
export function storyCard(s: Story, width: number, cell: number, p: Palette): Painted {
  const card = new Card(width, p)
  title(card, 'Where the work stands', (x, cy) => scroll(card, x, cy), 29)
  card.y += M.TITLE_LINE
  if (s.age !== '') {
    const cy = card.y + M.CAPTION_LINE * 0.45
    const w = refreshMark(card, card.left, cy, p.faint)
    card.text(card.left + w + 4, card.y + M.CAPTION_LINE * 0.74, `refreshed ${s.age}`, M.CAPTION, p.faint)
    card.y += M.CAPTION_LINE + 4
  }
  if (s.text !== '') card.lines(wrap(s.text.replace(/`/g, ''), M.BODY, 'sans', card.inner, 6), M.BODY, M.BODY_LINE, p.ink)
  if (s.notes !== '') card.lines([s.notes], M.BODY, M.BODY_LINE, p.mint)
  if (s.rows.length > 0) card.y += 6
  for (const row of s.rows.slice(0, 4)) {
    const base = card.y + M.CAPTION_LINE * 0.74
    const cy = card.y + M.CAPTION_LINE * 0.45
    let x = card.left + fileMark(card, card.left, cy, p.mint)
    const stamp = row.state === 'commit' ? row.stamp.slice(0, 7) : row.state
    const stampW = measure(stamp, M.CAPTION, 'sans') + (row.state === 'unverified' ? 0 : 20)
    const label = codeLabel(row.file, row.symbol, card.right - x - stampW - 12)
    x += card.text(x, base, label, M.CAPTION, p.mint) + 10
    if (row.state === 'commit') {
      card.shapes.push({ kind: 'rect', x: x - 3, y: card.y + 2, w: stampW + 4, h: M.CAPTION_LINE - 4, r: 5, color: p.chip, alpha: p.chipAlpha })
      x += commitMark(card, x, cy, p.mint)
      card.text(x, base, stamp, M.CAPTION, p.mint)
    } else if (row.state === 'verified') {
      x += checkMark(card, x, cy, p.mint)
      card.text(x, base, stamp, M.CAPTION, p.mint)
    } else card.text(x, base, stamp, M.CAPTION, p.faint)
    card.y += M.CAPTION_LINE + 1
  }
  return card.close(cell, `Where the work stands. ${s.text} ${s.notes}`)
}

/**
 * A code row's words in `width`: the path is cut from the left, a folder at a time, so the file
 * name and the symbol stay ("…/hooks/picture.ts · drawPicture", then "picture.ts · drawPicture").
 */
export function codeLabel(file: string, symbol: string, width: number): string {
  const parts = file.split('/')
  for (let keep = parts.length; keep >= 1; keep--) {
    const path = (keep < parts.length ? '…/' : '') + parts.slice(parts.length - keep).join('/')
    const words = `${keep === 1 ? (parts[parts.length - 1] ?? file) : path} · ${symbol}`
    if (measure(words, M.CAPTION, 'sans') <= width) return words
  }
  return fit(`${parts[parts.length - 1] ?? file} · ${symbol}`, M.CAPTION, 'sans', width)
}

/** The herdr pane's agents row: how many run, and the newest one's task. */
export function agentsCard(running: string[], width: number, cell: number, p: Palette): Painted {
  const card = new Card(width, p)
  const cy = card.y + M.BODY_LINE * 0.5
  // The herdr pane's small robot: a head, two eyes, an antenna.
  const x = card.left
  if (M.mono) card.glyph(x, cy, '▣', p.mint)
  else {
  card.shapes.push({ kind: 'rect', x, y: cy - 6, w: 16, h: 12, r: 3, stroke: 1.6, color: p.mint })
  card.shapes.push({ kind: 'disc', x: x + 5, y: cy, r: 1.6, color: p.mint })
  card.shapes.push({ kind: 'disc', x: x + 11, y: cy, r: 1.6, color: p.mint })
  card.shapes.push({ kind: 'line', x1: x + 8, y1: cy - 6, x2: x + 8, y2: cy - 10, width: 1.6, color: p.mint })
  }
  const more = running.length > 1 ? ` (+${running.length - 1})` : ''
  const gap = M.mono ? 2 * COLUMN : 28
  const words = fit(`${running.length} running · ${running[0] ?? ''}${more}`, M.BODY, 'sans', card.inner - gap)
  card.text(x + gap, card.y + M.BODY_LINE * 0.74, words, M.BODY, p.quiet)
  card.y += M.BODY_LINE
  return card.close(cell, words)
}

function barInk(p: Palette, percent: number): Rgb {
  return percent >= 90 ? p.risk : percent >= 75 ? p.amber : p.mint
}

/** The colour of a context part, as the herdr pane draws it. */
export function partInk(p: Palette, ink: Context['parts'][number]['ink']): Rgb {
  return ink === 'setup' ? p.band : ink === 'tools' ? p.asked : ink === 'csr' ? p.pink : p.sky
}

/** 5. The limits: the headline, the bars, the context window and its parts, and Self-Reflect's line. */
export function limitsCard(l: Limits | null, ctx: Context | null, csr: string | null, width: number, cell: number, p: Palette, m: Motion): Painted {
  const card = new Card(width, p)
  if (l) {
    const tone = l.tone === 'red' ? p.risk : l.tone === 'amber' ? p.amber : p.quiet
    // The headline a step under the titles, so a usual sentence keeps to one line, as in herdr.
    const lines = wrap(l.headline, M.HEADLINE, 'serif', card.inner, 2)
    for (const line of lines) {
      card.text(card.left, card.y + M.TITLE_LINE * 0.74, line, M.HEADLINE, tone, 'serif')
      card.y += M.TITLE_LINE - 2
    }
    card.y += 12
    const nameW = Math.max(...l.bars.map(b => measure(b.name, M.BODY, 'sans'))) + 16
    const pctW = measure('100%', M.BODY, 'sans') + 8
    for (const b of l.bars) {
      const shown = m.eased[`bar:${b.name}`] ?? b.percent
      const base = card.y + M.BODY_LINE * 0.74
      const cy = card.y + M.BODY_LINE * 0.5
      card.text(card.left, base, b.name, M.BODY, p.ink)
      const x0 = card.left + nameW
      const x1 = card.right - pctW
      if (M.mono) card.gridBar(x0, x1, cy, [{ share: Math.max(0, Math.min(1, shown / 100)), color: barInk(p, b.percent) }], p.track)
      else {
        card.shapes.push({ kind: 'rect', x: x0, y: cy - 5, w: x1 - x0, h: 10, r: 5, color: p.track })
        const fill = Math.max(0, Math.min(1, shown / 100)) * (x1 - x0)
        if (fill > 0.5) card.shapes.push({ kind: 'rect', x: x0, y: cy - 5, w: Math.max(10, fill), h: 10, r: 5, color: barInk(p, b.percent) })
      }
      card.text(card.right, base, `${Math.round(shown)}%`, M.BODY, barInk(p, b.percent), 'sans', 'end')
      card.y += M.BODY_LINE + 6
    }
  }
  let drawLegend = () => {}
  if (ctx) {
    card.y += l ? 14 : 0
    const used = m.eased['ctx'] ?? ctx.used
    const head = `${tokens(used)} of ${tokens(ctx.max)} (${Math.round((used / ctx.max) * 100)}%)`
    // As herdr: the head at the left, what is free at the right, the bar under them.
    const base = card.y + M.BODY_LINE * 0.74
    const w = card.text(card.left, base, 'Context', M.BODY, p.ink)
    const headEnd = card.left + w + 8 + card.text(card.left + w + 8, base, head, M.BODY, p.amber)
    const withBuffer = `${tokens(ctx.free)} free · ${tokens(ctx.buffer)} buffer`
    const free = ctx.buffer > 0 && headEnd + measure(withBuffer, M.CAPTION, 'sans') + 16 <= card.right ? withBuffer : `${tokens(ctx.free)} free`
    if (headEnd + measure(free, M.CAPTION, 'sans') + 16 <= card.right) card.text(card.right, base, free, M.CAPTION, p.quiet, 'sans', 'end')
    card.y += M.BODY_LINE + 6
    // The bar spans the window; the mix rides at its right end.
    const verdict = ctx.mix?.verdict ?? ''
    const mixW = verdict === '' ? 0 : measure(verdict, M.CAPTION, 'sans') + 30
    const x0 = card.left
    const x1 = card.right - mixW
    const cy = card.y + (M.mono ? M.BODY_LINE * 0.5 : 8)
    const k = (x1 - x0) / ctx.max
    const scale = ctx.used > 0 ? used / ctx.used : 1
    if (M.mono) {
      const parts = ctx.parts.map(part => ({ share: (part.tokens * scale) / ctx.max, color: partInk(p, part.ink) }))
      card.gridBar(x0, x1, cy, parts, p.track)
      if (ctx.mix) {
        card.glyph(x1 + 6, cy, '◕', p.sky)
        card.text(x1 + 6 + 2 * COLUMN, cy, verdict, M.CAPTION, p.quiet)
      }
      card.y += M.BODY_LINE
    } else {
    card.shapes.push({ kind: 'rect', x: x0, y: cy - 5, w: x1 - x0, h: 10, r: 5, color: p.track })
    let x = x0
    for (const part of ctx.parts) {
      const pw = part.tokens * scale * k
      if (pw < 0.5) continue
      card.shapes.push({ kind: 'rect', x, y: cy - 5, w: Math.max(4, pw - 2), h: 10, r: 3, color: partInk(p, part.ink) })
      x += pw
    }
    if (ctx.buffer > 0) {
      const bx = x1 - ctx.buffer * k
      for (let hx = bx + 2; hx < x1 - 2; hx += 5) card.shapes.push({ kind: 'line', x1: hx, y1: cy + 3, x2: hx + 4, y2: cy - 3, width: 1, color: p.band, alpha: 0.7 })
      card.shapes.push({ kind: 'line', x1: bx, y1: cy - 7, x2: bx, y2: cy + 7, width: 1.4, color: p.band })
    }
    if (ctx.mix) {
      const dx = x1 + 16
      const r = 6.5
      card.shapes.push({ kind: 'ring', x: dx, y: cy, r, width: 4, color: p.track })
      let from = 0
      ctx.mix.shares.forEach((share, i) => {
        const to = from + share
        if (to - from > 0.013) card.shapes.push({ kind: 'arc', x: dx, y: cy, r, from, to: to - 0.012, width: 4, color: [p.asked, p.sky, p.pink][i] ?? p.sky })
        from = to
      })
      card.text(dx + r + 8, cy + M.CAPTION * 0.34, verdict, M.CAPTION, p.quiet)
    }
    card.y += 22
    }
    // The legend: each part by name, in its colour. On the grid it waits under Self-Reflect's
    // line, so a short pane shows the product's claim first.
    drawLegend = () => {
      const legend: Run[] = []
      for (const part of ctx.parts) legend.push({ text: `● ${part.name} ${tokens(part.tokens)}`, color: partInk(p, part.ink) })
      const rows = wrapLegend(legend, M.CAPTION, card.inner)
      for (const row of rows.slice(0, 2)) {
        let lx = card.left
        for (const r of row) {
          if (M.mono) card.glyph(lx, card.y + M.CAPTION_LINE * 0.5, '●', r.color)
          else card.shapes.push({ kind: 'disc', x: lx + 4, y: card.y + M.CAPTION_LINE * 0.5, r: 3.6, color: r.color })
          lx += 12 + card.text(lx + 12, card.y + M.CAPTION_LINE * 0.74, r.text.slice(2), M.CAPTION, p.quiet) + 14
        }
        card.y += M.CAPTION_LINE
      }
    }
    if (!M.mono) drawLegend()
  }
  if (csr) {
    card.y += M.mono ? 0 : 12
    const lines = wrapRuns([{ text: 'Self-Reflect this session:', color: p.pink }, { text: csr, color: p.ink }], M.BODY, card.inner - 14)
    if (M.mono) lines.forEach((_, i) => card.glyph(card.left, card.y + (i + 0.5) * M.BODY_LINE, '▌', p.pink))
    else card.shapes.push({ kind: 'rect', x: card.left, y: card.y + 3, w: 3.5, h: lines.length * M.BODY_LINE - 6, r: 1.75, color: p.pink })
    for (const line of lines) {
      card.runs(card.left + 14, card.y + M.BODY_LINE * 0.74, line, M.BODY)
      card.y += M.BODY_LINE
    }
  }
  if (M.mono) drawLegend()
  const alt = [l?.headline, ...(l?.bars ?? []).map(b => `${b.name} ${Math.round(b.percent)}%`), ctx && `Context ${tokens(ctx.used)} of ${tokens(ctx.max)}`, csr && `Self-Reflect this session: ${csr}`]
  return card.close(cell, alt.filter(Boolean).join('. '))
}

function wrapLegend(items: Run[], size: number, width: number): Run[][] {
  const rows: Run[][] = []
  let row: Run[] = []
  let used = 0
  for (const it of items) {
    const w = 12 + measure(it.text.slice(2), size, 'sans') + 14
    if (row.length > 0 && used + w > width) {
      rows.push(row)
      row = []
      used = 0
    }
    row.push(it)
    used += w
  }
  if (row.length > 0) rows.push(row)
  return rows
}

// ---- 6. the session timeline -------------------------------------------------------------------------

/** Idle stretches longer than this fold to a short gap marked by a dotted line. */
const IDLE_MS = 20 * 60_000
const FOLD_MS = 4 * 60_000

/** A clock that folds idle stretches: maps a time to 0..1 across the session. */
export function clockOf(stamps: number[], now: number): { at: (t: number) => number; breaks: number[]; start: number } {
  const sorted = [...stamps].filter(Number.isFinite).sort((a, b) => a - b)
  const start = sorted[0] ?? now
  sorted.push(now)
  const cuts: { at: number; lost: number }[] = []
  for (let i = 1; i < sorted.length; i++) {
    const gap = (sorted[i] ?? 0) - (sorted[i - 1] ?? 0)
    if (gap > IDLE_MS) cuts.push({ at: sorted[i - 1] ?? 0, lost: gap - FOLD_MS })
  }
  const lostBefore = (t: number) => cuts.reduce((n, c) => (t > c.at ? n + Math.min(c.lost, t - c.at) : n), 0)
  const total = Math.max(1, now - start - lostBefore(now))
  const at = (t: number) => Math.max(0, Math.min(1, (t - start - lostBefore(t)) / total))
  return { at, breaks: cuts.map(c => at(c.at + FOLD_MS / 2)), start }
}

function clockWords(ms: number): string {
  const d = new Date(ms)
  return `${d.getHours() % 12 || 12}:${String(d.getMinutes()).padStart(2, '0')}`
}

function spanWords(ms: number): string {
  const min = Math.round(ms / 60000)
  return min < 60 ? `${min}m` : `${Math.floor(min / 60)}h ${min % 60}m`
}

/** The session over time: your prompts, the context after each reply with its drops at the compactions, and the recalls and edits. */
export function timelineCard(f: Facts, width: number, cell: number, p: Palette, m: Motion, now: number): Painted {
  const card = new Card(width, p)
  const stamps = [...f.prompts, ...f.replies.map(r => r[0])]
  const first = Math.min(...stamps, now)
  const end = title(card, 'This session', null)
  const meta = `${f.partial ? 'last ' : ''}${spanWords(now - first)} · ${f.compacts.length} compaction${f.compacts.length === 1 ? '' : 's'} in all`
  if (end + measure(meta, M.CAPTION, 'sans') + 12 <= card.right) card.text(card.right, card.y + M.TITLE_LINE * 0.74, meta, M.CAPTION, p.faint, 'sans', 'end')
  card.y += M.TITLE_LINE + 6

  const clock = clockOf(stamps, now)
  const x0 = card.left
  const x1 = card.right
  const X = (t: number) => x0 + clock.at(t) * (x1 - x0)
  if (M.mono) return timelineGrid(card, f, clock.at, p, m, now, first)
  const laneYou = card.y
  const plotTop = laneYou + 24
  const plotH = 104
  const plotBase = plotTop + plotH
  const laneMem = plotBase + 8

  // Lane guides and the folded idle stretches.
  card.shapes.push({ kind: 'line', x1: x0, y1: plotBase, x2: x1, y2: plotBase, width: 1, color: p.hair, alpha: p.hairAlpha * 2 })
  for (const b of clock.breaks) {
    const bx = x0 + b * (x1 - x0)
    for (let y = laneYou; y < laneMem + 22; y += 6) card.shapes.push({ kind: 'line', x1: bx, y1: y, x2: bx, y2: y + 2.5, width: 1, color: p.faint, alpha: 0.6 })
  }
  // Your prompts: short ticks along the top.
  for (const t of f.prompts) card.shapes.push({ kind: 'line', x1: X(t), y1: laneYou + 3, x2: X(t), y2: laneYou + 15, width: 1.8, color: p.quiet })

  // The context after each reply, scaled to the session's peak; the newest drop draws in.
  const peak = Math.max(1, ...f.replies.map(r => r[1]))
  const lastCompact = f.compacts[f.compacts.length - 1]
  let pts = f.replies.map(([t, ctx]) => [X(t), plotBase - 2 - (ctx / peak) * (plotH - 8)] as [number, number])
  if (m.drop < 1 && lastCompact !== undefined) {
    const cut = f.replies.findIndex(r => r[0] >= lastCompact)
    if (cut > 0) {
      const before = pts.slice(0, cut)
      const a = pts[cut - 1] ?? [0, 0]
      const b = pts[cut] ?? a
      const y = a[1] + (b[1] - a[1]) * m.drop
      pts = [...before, [a[0] + (b[0] - a[0]) * Math.min(1, m.drop * 1.5), y]]
    }
  }
  pts.sort((a, b) => a[0] - b[0])
  if (pts.length > 1) {
    card.shapes.push({ kind: 'area', points: pts, base: plotBase, color: p.sky, alpha: 0.22 })
    for (let i = 1; i < pts.length; i++) {
      const a = pts[i - 1] ?? [0, 0]
      const b = pts[i] ?? a
      card.shapes.push({ kind: 'line', x1: a[0], y1: a[1], x2: b[0], y2: b[1], width: 1.8, color: p.sky })
    }
  } else if (pts.length === 1) {
    const only = pts[0] ?? [x0, plotBase]
    card.shapes.push({ kind: 'disc', x: only[0], y: only[1], r: 3, color: p.sky })
  }
  // Compactions: the drops, marked in Self-Reflect's colour.
  f.compacts.forEach((t, i) => {
    const isNew = i === f.compacts.length - 1 && m.drop < 1
    card.shapes.push({ kind: 'line', x1: X(t), y1: plotTop, x2: X(t), y2: plotBase, width: isNew ? 2.4 : 1.6, color: p.pink, alpha: isNew ? 0.4 + 0.6 * m.drop : 0.85 })
  })

  // Memory: recalls as mint ticks, edits as small sky dots; the newest recall glows a moment.
  for (const t of f.edits) card.shapes.push({ kind: 'disc', x: X(t), y: laneMem + 16, r: 2.2, color: p.asked })
  f.recalls.forEach((t, i) => {
    card.shapes.push({ kind: 'line', x1: X(t), y1: laneMem + 2, x2: X(t), y2: laneMem + 11, width: 1.8, color: p.mint })
    if (i === f.recalls.length - 1 && m.recall > 0) card.shapes.push({ kind: 'ring', x: X(t), y: laneMem + 6, r: 4 + 10 * (1 - m.recall), width: 1.6, color: p.mint, alpha: m.recall })
  })
  card.y = laneMem + 24

  // Times at the ends, then the legend.
  card.text(x0, card.y + M.CAPTION_LINE * 0.74, clockWords(first), M.CAPTION, p.faint)
  card.text(x1, card.y + M.CAPTION_LINE * 0.74, 'now', M.CAPTION, p.faint, 'sans', 'end')
  card.y += M.CAPTION_LINE + 2
  const legend: [string, Rgb, 'tick' | 'dot' | 'bar'][] = [
    ['you', p.quiet, 'tick'],
    [`context, peak ${tokens(peak)}`, p.sky, 'bar'],
    ['compaction', p.pink, 'tick'],
    ['recall', p.mint, 'tick'],
    ['edit', p.asked, 'dot'],
  ]
  let lx = x0
  let ly = card.y
  for (const [word, color, mark] of legend) {
    const w = 14 + measure(word, M.CAPTION, 'sans') + 14
    if (lx + w > x1 + 14) {
      lx = x0
      ly += M.CAPTION_LINE
    }
    const cy = ly + M.CAPTION_LINE * 0.48
    if (mark === 'tick') card.shapes.push({ kind: 'line', x1: lx + 4, y1: cy - 6, x2: lx + 4, y2: cy + 6, width: 1.8, color })
    else if (mark === 'dot') card.shapes.push({ kind: 'disc', x: lx + 4, y: cy, r: 3, color })
    else card.shapes.push({ kind: 'rect', x: lx, y: cy - 4, w: 9, h: 8, r: 2, color, alpha: 0.7 })
    card.text(lx + 14, ly + M.CAPTION_LINE * 0.74, word, M.CAPTION, p.quiet)
    lx += w
  }
  card.y = ly + M.CAPTION_LINE
  if (f.replies.length < 2) card.lines(['The line grows with each reply.'], M.CAPTION, M.CAPTION_LINE, p.faint)
  return card.close(cell, `This session: ${f.prompts.length} prompts, ${f.compacts.length} compactions, ${f.recalls.length} recalls, context peak ${tokens(peak)}.`)
}

/** Left blocks, one eighth to seven eighths of a cell wide: a bar's end. */
export const LEFT_EIGHTHS = ['▏', '▎', '▍', '▌', '▋', '▊', '▉']

/** Eighth blocks, from one eighth to a full cell, for the grid's area chart. */
export const EIGHTHS = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█']
const PLOT_ROWS = 4

/** The glyphs of one column of the area chart, top row first: `share` 0..1 of `rows` rows. */
export function columnGlyphs(share: number, rows = PLOT_ROWS): string[] {
  const eighths = Math.round(Math.max(0, Math.min(1, share)) * rows * 8)
  return Array.from({ length: rows }, (_, i) => {
    const fromBottom = rows - 1 - i
    const left = eighths - fromBottom * 8
    return left >= 8 ? '█' : left > 0 ? (EIGHTHS[left - 1] ?? ' ') : ' '
  })
}

/** The timeline on the terminal's grid: prompts, an eighth-block area chart, compactions, recalls. */
function timelineGrid(card: Card, f: Facts, at: (t: number) => number, p: Palette, m: Motion, now: number, first: number): Painted {
  const cols = Math.max(4, Math.floor(card.inner / COLUMN))
  const colOf = (t: number) => Math.min(cols - 1, Math.max(0, Math.floor(at(t) * cols)))
  const row = M.BODY_LINE
  const x = (c: number) => card.left + c * COLUMN
  // Your prompts along the top.
  for (const t of f.prompts) card.glyph(x(colOf(t)), card.y + row * 0.5, '╷', p.quiet)
  card.y += row
  // The context after each reply, scaled to the peak, read at each column's middle.
  const peak = Math.max(1, ...f.replies.map(r => r[1]))
  let pts = f.replies.map(([t, v]) => [at(t) * cols, v] as [number, number])
  const lastCompact = f.compacts[f.compacts.length - 1]
  if (m.drop < 1 && lastCompact !== undefined) {
    const cut = f.replies.findIndex(r => r[0] >= lastCompact)
    if (cut > 0) {
      const a = pts[cut - 1] ?? [0, 0]
      const b = pts[cut] ?? a
      pts = [...pts.slice(0, cut), [a[0] + (b[0] - a[0]) * Math.min(1, m.drop * 1.5), a[1] + (b[1] - a[1]) * m.drop]]
    }
  }
  pts.sort((a, b) => a[0] - b[0])
  const valueAt = (cx: number): number | undefined => {
    if (pts.length === 0 || cx < (pts[0]?.[0] ?? 0) - 0.5 || cx > (pts[pts.length - 1]?.[0] ?? 0) + 0.5) return undefined
    for (let i = 1; i < pts.length; i++) {
      const a = pts[i - 1] ?? [0, 0]
      const b = pts[i] ?? a
      if (cx <= b[0]) return b[0] > a[0] ? a[1] + ((b[1] - a[1]) * (cx - a[0])) / (b[0] - a[0]) : b[1]
    }
    return pts[pts.length - 1]?.[1]
  }
  const compactCols = new Set(f.compacts.map(colOf))
  for (let c = 0; c < cols; c++) {
    const v = valueAt(c + 0.5)
    const glyphs = v === undefined ? Array.from({ length: PLOT_ROWS }, () => ' ') : columnGlyphs(v / peak)
    glyphs.forEach((g, r) => {
      const cy = card.y + (r + 0.5) * row
      if (compactCols.has(c)) card.glyph(x(c), cy, g === ' ' ? '┃' : g, p.pink)
      else if (g !== ' ') card.glyph(x(c), cy, g, p.sky)
    })
  }
  card.y += PLOT_ROWS * row
  // Memory under the chart: recalls (the newest flashes while it is new), then edits.
  const marks = new Map<number, [string, Rgb]>()
  for (const t of f.edits) marks.set(colOf(t), ['·', p.asked])
  f.recalls.forEach((t, i) => marks.set(colOf(t), [i === f.recalls.length - 1 && m.recall > 0.5 ? '✦' : '╵', p.mint]))
  for (const [c, [g, color]] of marks) card.glyph(x(c), card.y + row * 0.5, g, color)
  card.y += row
  card.text(card.left, card.y + row * 0.74, clockWords(first), M.CAPTION, p.faint)
  card.text(card.right, card.y + row * 0.74, 'now', M.CAPTION, p.faint, 'sans', 'end')
  card.y += row
  const legend: Run[] = [
    { text: '╷ you', color: p.quiet },
    { text: `█ context ≤${tokens(peak)}`, color: p.sky },
    { text: '┃ compaction', color: p.pink },
    { text: '╵ recall', color: p.mint },
  ]
  for (const line of wrapRuns(legend.map((r, i) => ({ ...r, text: (i > 0 ? '  ' : '') + r.text })), M.CAPTION, card.inner)) {
    card.runs(card.left, card.y + row * 0.74, line, M.CAPTION)
    card.y += row
  }
  void now
  return card.close(row, `This session: ${f.prompts.length} prompts, ${f.compacts.length} compactions, ${f.recalls.length} recalls, context peak ${tokens(peak)}.`)
}

/**
 * The compact band above the prompt, on the grid only (a narrow window): the weekly bar, the
 * context bar, and Self-Reflect's line, one row each, no title.
 */
export function bandCard(l: Limits | null, ctx: Context | null, csr: string | null, width: number, cell: number, p: Palette): Painted {
  const card = new Card(width, p)
  card.y = 0
  const row = cell
  const nameW = 9 * COLUMN
  const pctW = 5 * COLUMN
  const week = l?.bars.find(b => b.name === 'Weekly') ?? l?.bars[0]
  if (week) {
    card.text(card.left, card.y + row * 0.74, week.name, M.BODY, p.ink)
    card.gridBar(card.left + nameW, card.right - pctW, card.y + row * 0.5, [{ share: week.percent / 100, color: barInk(p, week.percent) }], p.track)
    card.text(card.right, card.y + row * 0.74, `${Math.round(week.percent)}%`, M.BODY, barInk(p, week.percent), 'sans', 'end')
    card.y += row
  }
  if (ctx) {
    const head = `${tokens(ctx.used)}/${tokens(ctx.max)}`
    card.text(card.left, card.y + row * 0.74, 'Context', M.BODY, p.ink)
    const parts = ctx.parts.map(part => ({ share: part.tokens / ctx.max, color: partInk(p, part.ink) }))
    card.gridBar(card.left + nameW, card.right - (head.length + 1) * COLUMN, card.y + row * 0.5, parts, p.track)
    card.text(card.right, card.y + row * 0.74, head, M.BODY, p.amber, 'sans', 'end')
    card.y += row
  }
  if (csr) {
    const words = fit(`Self-Reflect: ${csr}`, M.BODY, 'sans', card.inner - 2 * COLUMN)
    card.glyph(card.left, card.y + row * 0.5, '▌', p.pink)
    card.text(card.left + 2 * COLUMN, card.y + row * 0.74, words, M.BODY, p.pink)
    card.y += row
  }
  const rows = Math.max(1, Math.round(card.y / cell))
  return { scene: { width, height: rows * cell, shapes: card.shapes }, rows, alt: [week && `Weekly ${Math.round(week.percent)}%`, ctx && `Context ${tokens(ctx.used)} of ${tokens(ctx.max)}`, csr && `Self-Reflect: ${csr}`].filter(Boolean).join('. ') }
}

/** A card the mod has no painter for: its title and its lines, plainly. */
export function plainCard(title: string, lines: string[], width: number, cell: number, p: Palette): Painted {
  const card = new Card(width, p)
  title !== '' && card.text(card.left, card.y + M.TITLE_LINE * 0.74, fit(title, M.TITLE, 'serif', card.inner), M.TITLE, p.ink, 'serif')
  if (title !== '') card.y += M.TITLE_LINE + 4
  for (const line of lines.slice(0, 8)) card.lines(wrap(line, M.BODY, 'sans', card.inner, 3), M.BODY, M.BODY_LINE, p.quiet)
  return card.close(cell, [title, ...lines].join('. '))
}

/** One dim line at the page's foot: the engine is newer than this mod. */
export function updateCard(width: number, cell: number, p: Palette): Painted {
  const card = new Card(width, p)
  card.lines(['update csr-pane: the engine sends more than this version reads'], M.CAPTION, M.CAPTION_LINE, p.faint)
  return card.close(cell, 'update csr-pane')
}
