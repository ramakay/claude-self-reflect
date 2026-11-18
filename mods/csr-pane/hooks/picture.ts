// The cards on a surface, and the frames that keep them moving. A terminal that shows pictures gets
// one painted Image per card (paint.ts, sent as PNG); the desktop app and the editor get one SVG per
// card; a terminal with no pictures gets none here and the pane draws its text cards instead.
// Frames go out only while something moves, and only for the card that moves.
import type { EngineInterface } from 'claude-code'

import { MAX_IMAGE_BYTES, paint } from './paint'
import type { Frame, Shape } from './paint'
import type { Pieces } from './pieces'
import { imageSource } from './png'
import { COLUMN, STILL, bandCard, gridMetrics, plainCard, updateCard, withMetrics, agentsCard, dreamerCard, limitsCard, samePageCard, shouldBeCard, storyCard, timelineCard } from './scene'
import type { Motion, Painted } from './scene'
import { toSvg } from './surface'
import { packCells, rowRuns, segmentsOf, sliceRows, toCells } from './cells'
import type { Cells } from './cells'
import { DARK, LIGHT } from './theme'
import type { Palette } from './theme'
import type { Facts } from './transcript'

/** Pixels painted for each terminal column: a Retina cell of a 7-point-wide font is 14 pixels. */
export const PIXELS_PER_COLUMN = 14
/** The widest the cards go beside the chat, and the narrowest they can be read. */
const MAX_COLUMNS = 60
export const MIN_COLUMNS = 30
/** One frame every this many ms while a bar eases or a drop draws: calm, not a game. */
export const FRAME_MS = 80
/** The dreamer's ring is a small turn: five steps a second are enough. */
export const SPIN_FRAME_MS = 200
/** The ring stops turning after this long with no new reading, even if the look still runs. */
export const SPIN_LIMIT_MS = 90_000
/** Rows a band takes: a card is sent as bands, each drawn whenever all of it is in view. */
export const BAND_ROWS = 3
const EASE_MS = 700
const DROP_MS = 1100
const RECALL_MS = 1600
const SPIN_MS = 3200
const PROBE_MS = 150
const PROBES = 4

/** The ANSI background indexes that are light. */
const LIGHT_GROUNDS = new Set([7, 9, 10, 11, 12, 13, 14, 15])

/** Light or dark from `COLORFGBG` (`fg;bg`), or undefined when it says nothing. */
export function schemeFrom(value: string | undefined): 'light' | 'dark' | undefined {
  const last = (value ?? '').trim().split(';').pop() ?? ''
  if (!/^\d{1,3}$/.test(last)) return undefined
  return LIGHT_GROUNDS.has(Number(last)) ? 'light' : 'dark'
}

/** How tall a cell is for its width, where the person left it to the mod: iTerm2's is about 2.43. */
export function shapeFor(termProgram: string | undefined): number {
  return termProgram === 'iTerm.app' ? 2.43 : 2.2
}

export type CardId = 'dreamer' | 'same' | 'should' | 'story' | 'limits' | 'agents' | 'timeline' | 'update' | `extra${number}`

/** What the cards draw: the pieces, the transcript facts, and the keys that open more. */
export type Model = { pieces: Pieces; facts: Facts | null; hints: { same: string; should: string }; agents?: string[] }

/** Lays out every card the model has, in the herdr pane's order, with the timeline last. */
export function buildCards(model: Model, columns: number, shape: number, p: Palette, m: Motion, now: number): { id: CardId; painted: Painted }[] {
  const width = columns * COLUMN
  const cell = COLUMN * shape
  const { pieces, facts, hints } = model
  const out: { id: CardId; painted: Painted }[] = []
  if (pieces.dreamer) out.push({ id: 'dreamer', painted: dreamerCard(pieces.dreamer, width, cell, p, m) })
  if (pieces.samePage) out.push({ id: 'same', painted: samePageCard(pieces.samePage, width, cell, p, hints.same) })
  if (pieces.shouldBe.length > 0) out.push({ id: 'should', painted: shouldBeCard(pieces.shouldBe, width, cell, p, hints.should) })
  if (pieces.story) out.push({ id: 'story', painted: storyCard(pieces.story, width, cell, p) })
  if (pieces.limits || pieces.context || pieces.csr) {
    out.push({ id: 'limits', painted: limitsCard(pieces.limits, pieces.context, pieces.csr, width, cell, p, m) })
  }
  if (model.agents && model.agents.length > 0) out.push({ id: 'agents', painted: agentsCard(model.agents, width, cell, p) })
  pieces.extra.forEach((c, i) => out.push({ id: `extra${i}`, painted: plainCard(c.title, c.lines, width, cell, p) }))
  if (facts && (facts.replies.length > 0 || facts.prompts.length > 0)) out.push({ id: 'timeline', painted: timelineCard(facts, width, cell, p, m, now) })
  if (pieces.isOutdated) out.push({ id: 'update', painted: updateCard(width, cell, p) })
  return out
}

/**
 * The aurora behind a page of cards, as the herdr pane paints it (instrument_gfx.rs `background`):
 * three radial layers over the whole page. Each card gets the part under it, so the light runs on
 * unbroken from one card's picture to the next.
 */
export function withBackdrop<T extends { painted: Painted }>(cards: T[], p: Palette): T[] {
  const w = cards[0]?.painted.scene.width ?? 0
  const h = cards.reduce((n, c) => n + c.painted.scene.height, 0)
  const diagonal = Math.hypot(w, h)
  const centres = [
    [-0.15 * w, -0.1 * h, 0.9 * diagonal],
    [1.15 * w, 0.45 * h, diagonal],
    [0.45 * w, 1.1 * h, 1.1 * diagonal],
  ]
  const layers = centres.map(([x, y, r], i) => ({ x: x ?? 0, y: y ?? 0, r: r ?? 1, stops: p.aurora[i] ?? [] }))
  let top = 0
  return cards.map(c => {
    const ground: Shape = { kind: 'backdrop', top, ground: p.mesh, layers }
    top += c.painted.scene.height
    return { ...c, painted: { ...c.painted, scene: { ...c.painted.scene, shapes: [ground, ...c.painted.scene.shapes] } } }
  })
}

/** The page on the terminal's grid, the aurora under it: the cell view's cards (cells.ts paints them). */
export function gridCards(model: Model, columns: number, shape: number, p: Palette, m: Motion, now: number): { id: CardId; painted: Painted }[] {
  const cell = COLUMN * shape
  return withMetrics(gridMetrics(cell), () => withBackdrop(buildCards(model, columns, shape, p, m, now), p))
}

/** One band of a painted card: rows `from` to `to` of its frame, cut on row lines. */
export type Band = { key: string; rows: number; frame: Frame }

/**
 * Whether cards go out in bands. In iTerm2 each picture asks "Allow Terminal-Initiated Display?",
 * so there a card is one picture (a cut-off card waits until it is wholly in view); elsewhere bands.
 */
let isBanded = true

export function setBanding(on: boolean): void {
  isBanded = on
}

/** One picture a card where its pixels fit an Image; bands where they do not. */
export function bandRowsFor(frame: Frame, rows: number, banded: boolean): number {
  return banded || frame.rgba.length > MAX_IMAGE_BYTES ? BAND_ROWS : rows
}

/** The most PNG bytes all the Images of one tree may carry (mods reference, limits). */
export const MAX_TREE_BYTES = 2 * 1024 * 1024

/**
 * Keeps a page's pictures under the tree limit: whole cards from the end of the page are left
 * out (the timeline first) until the total fits. Returns what stays and its total in bytes.
 */
export function withinTree<T extends { id: string; source: { png: string } }>(bands: T[], limit = MAX_TREE_BYTES): { kept: T[]; bytes: number } {
  const size = (b: T) => Math.floor((b.source.png.length * 3) / 4)
  let kept = bands
  let bytes = kept.reduce((n, b) => n + size(b), 0)
  while (bytes > limit && kept.length > 0) {
    const last = kept[kept.length - 1]?.id
    kept = kept.filter(b => b.id !== last)
    bytes = kept.reduce((n, b) => n + size(b), 0)
  }
  return { kept, bytes }
}

/** The total PNG bytes of the page last drawn, for the report and the tests. */
export let lastTreeBytes = 0

/** Cuts a card's frame into bands of `bandRows` rows; the seams fall on row lines. */
export function bandsOf(id: string, frame: Frame, rows: number, bandRows = BAND_ROWS): Band[] {
  const out: Band[] = []
  const rowPx = frame.height / rows
  for (let from = 0, i = 0; from < rows; from += bandRows, i++) {
    const to = Math.min(rows, from + bandRows)
    const y0 = Math.round(from * rowPx)
    const y1 = to === rows ? frame.height : Math.round(to * rowPx)
    const bytes = frame.rgba.subarray(y0 * frame.width * 4, y1 * frame.width * 4)
    if (bytes.length > MAX_IMAGE_BYTES) throw new Error('band over the Image limit')
    out.push({ key: `${id}-${i}`, rows: to - from, frame: { width: frame.width, height: y1 - y0, rgba: bytes } })
  }
  return out
}

// ---- motion -------------------------------------------------------------------------------------------

type Ease = { from: number; to: number; start: number }

/** What moves, and since when. Module state: one pane, one session. */
const motion = {
  eases: new Map<string, Ease>(),
  dreaming: false,
  dropAt: undefined as number | undefined,
  recallAt: undefined as number | undefined,
  // When the dreamer was last seen to start or go on looking: the ring stops SPIN_LIMIT_MS after.
  spinSince: 0,
  // Off: every change is drawn where it ends, in one frame.
  isOn: true,
  // Frames go out only while the pane has a seat.
  isPlaced: true,
}

/** Whether the pane has a seat; without one no frame is sent. */
export function setPlaced(placed: boolean): void {
  motion.isPlaced = placed
}

const isSpinning = (now: number) => motion.dreaming && now - motion.spinSince < SPIN_LIMIT_MS

/**
 * Whether the cards move. `auto` keeps them still in iTerm2: there each new picture raises its
 * "Allow Terminal-Initiated Display?" sheet unless the person ticked "Remember my choice", so
 * twelve frames a second would be twelve sheets (seen on the Studio, iTerm2 3.7.6, 2026-11-15).
 */
export function motionFor(setting: string, termProgram: string | undefined): boolean {
  if (setting === 'on') return true
  if (setting === 'off') return false
  return termProgram !== 'iTerm.app'
}

export function setMotion(on: boolean): void {
  motion.isOn = on
  if (!on) {
    motion.dropAt = undefined
    motion.recallAt = undefined
  }
}

const easeOut = (t: number) => 1 - Math.pow(1 - t, 3)

function easedValue(e: Ease, now: number): number {
  const t = Math.max(0, Math.min(1, (now - e.start) / EASE_MS))
  return e.from + (e.to - e.from) * easeOut(t)
}

/** The motion at `now`. */
export function motionAt(now: number): Motion {
  const eased: Record<string, number> = {}
  for (const [key, e] of motion.eases) eased[key] = easedValue(e, now)
  const drop = motion.dropAt === undefined ? 1 : Math.max(0, Math.min(1, (now - motion.dropAt) / DROP_MS))
  const recall = motion.recallAt === undefined ? 0 : Math.max(0, 1 - (now - motion.recallAt) / RECALL_MS)
  // The ring turns in steps, one a frame of SPIN_FRAME_MS.
  const spin = isSpinning(now) ? Math.floor(now / SPIN_FRAME_MS) * (SPIN_FRAME_MS / SPIN_MS) : 0
  return { spin, eased, drop, recall }
}

/** Which cards move at `now`. */
export function movingAt(now: number): Set<CardId> {
  const moving = new Set<CardId>()
  if (!motion.isPlaced) return moving
  if (isSpinning(now)) moving.add('dreamer')
  for (const e of motion.eases.values()) if (now - e.start < EASE_MS) moving.add('limits')
  if (motion.dropAt !== undefined && now - motion.dropAt < DROP_MS) moving.add('timeline')
  if (motion.recallAt !== undefined && now - motion.recallAt < RECALL_MS) moving.add('timeline')
  return moving
}

/** New readings: each bar and the context ease from what is shown now to the new value. */
export function retarget(pieces: Pieces, now: number): void {
  const targets: Record<string, number> = {}
  for (const b of pieces.limits?.bars ?? []) targets[`bar:${b.name}`] = b.percent
  if (pieces.context) targets.ctx = pieces.context.used
  for (const [key, to] of Object.entries(targets)) {
    const had = motion.eases.get(key)
    if (!had || !motion.isOn) motion.eases.set(key, { from: to, to, start: now - EASE_MS })
    else if (had.to !== to) motion.eases.set(key, { from: easedValue(had, now), to, start: now })
  }
  const dreaming = motion.isOn && pieces.dreamer?.dreaming === true
  // A new reading that says the look runs restarts the ring's allowance.
  if (dreaming) motion.spinSince = now
  motion.dreaming = dreaming
}

/** The wait before the next frame: the ring alone steps slower than an ease or a drop. */
export function frameDelay(now: number): number | undefined {
  const moving = movingAt(now)
  if (moving.size === 0) return undefined
  return moving.size === 1 && moving.has('dreamer') ? SPIN_FRAME_MS : FRAME_MS
}

/** A compaction just happened: its drop draws in on the timeline. */
export function compacted(now: number): void {
  if (motion.isOn) motion.dropAt = now
}

/** Memory came into the session: the newest recall mark glows a moment. */
export function recalled(now: number): void {
  if (motion.isOn) motion.recallAt = now
}

/** For tests: back to still. */
export function resetMotion(): void {
  motion.eases.clear()
  motion.dreaming = false
  motion.dropAt = undefined
  motion.recallAt = undefined
  motion.spinSince = 0
  motion.isOn = true
  motion.isPlaced = true
}

// ---- the surface ----------------------------------------------------------------------------------------

/** What is on show in a terminal: the pane, the size, and what each card was drawn from. */
type Shown = { pane: string; model: Model; columns: number; shape: number; palette: Palette; page: 'now' | 'figure'; bodyRows: number; mode: 'picture' | 'cells'; boldText?: boolean }

let shown: Shown | undefined
let timer: { cancel: () => void } | undefined
// Whether this terminal shows pictures: unknown until the first frame is taken or refused.
let pictures: boolean | undefined
let probes = 0
let isVector = false

/** For tests: forget what this terminal could do. */
export function resetSurface(): void {
  shown = undefined
  timer?.cancel()
  timer = undefined
  pictures = undefined
  probes = 0
  sentBands.clear()
  isBanded = true
  restKey = ''
  restBands = []
}

/** Whether the terminal refused pictures: the pane then draws its text cards. */
export const noPictures = (): boolean => pictures === false

function schedule($: EngineInterface, delay: number | undefined): void {
  timer?.cancel()
  timer = undefined
  if (delay === undefined) return
  try {
    // A fault in a frame must never reach the work.
    timer = $.clock.after(delay, () => void tick($).catch(() => undefined))
  } catch {
    // No timer: the cards stay still.
  }
}

// What each band last showed, by its key: a band whose pixels did not change is not sent again
// (in iTerm2 each sent picture can raise a sheet).
const sentBands = new Map<string, string>()
// The last page painted at rest, by what it shows: a redraw with nothing new paints nothing.
let restKey = ''
let restBands: { band: Band; source: { png: string } }[] = []

/** The cards of a page as bands of pixels, the aurora under them. `cache` reuses a still page. */
function paintPage(
  cards: { id: CardId; painted: Painted }[],
  palette: Palette,
  cache: boolean,
  only?: Set<CardId>,
): { id: CardId; band: Band; source: { png: string } }[] {
  // The aurora is worked out over the whole page; only the cards asked for are painted.
  const grounded = withBackdrop(cards, palette).filter(c => only === undefined || only.has(c.id))
  const key = cache ? JSON.stringify(grounded.map(c => c.painted.scene)) : ''
  if (cache && key === restKey) return restBands.map(b => ({ ...b, id: b.band.key.replace(/-\d+$/, '') as CardId }))
  const out: { id: CardId; band: Band; source: { png: string } }[] = []
  for (const c of grounded) {
    const frame = paint(c.painted.scene, PIXELS_PER_COLUMN / COLUMN)
    for (const band of bandsOf(c.id, frame, c.painted.rows, bandRowsFor(frame, c.painted.rows, isBanded))) out.push({ id: c.id, band, source: imageSource(band.frame) })
  }
  if (cache) {
    restKey = key
    restBands = out
  }
  return out
}

/** The next frame of each card that moves; then the one after, while something still moves. */
async function tick($: EngineInterface): Promise<void> {
  timer = undefined
  const view = shown
  if (view === undefined || !motion.isPlaced) return
  const now = await $.clock.now()
  const moving = movingAt(now)
  if (view.mode === 'cells') {
    // Cells: each moving card painted again on the grid and sent as one Raster frame.
    const cards = pageOf(gridCards(view.model, view.columns, view.shape, view.palette, motionAt(now), now), view.page, view.bodyRows)
    for (const c of cards.filter(c => moving.has(c.id))) {
      const grid = toCells(c.painted.scene, COLUMN * view.shape, view.palette.cells)
      // Only the Raster runs move (bars, the ring, the chart); the bold title rows are Text.
      for (const [n, seg] of segmentsOf(grid, view.boldText ?? false).entries()) {
        if (seg.kind !== 'cells') continue
        const part = sliceRows(grid, seg.from, seg.rows)
        const sent = await $.ui.blit({ requestId: view.pane, key: `${c.id}-c${n}`, cells: packCells(part), columns: part.columns, rows: part.rows })
        if (sent.deny !== undefined) {
          if (shown === view) shown = undefined
          return
        }
      }
    }
    if (shown === view) schedule($, frameDelay(now))
    return
  }
  const probing = pictures === undefined
  const cards = pageOf(buildCards(view.model, view.columns, view.shape, view.palette, motionAt(now), now), view.page, view.bodyRows)
  const only = new Set<CardId>(moving)
  if (probing && cards[0]) only.add(cards[0].id)
  const bands = paintPage(cards, view.palette, false, only)
  // Each band that changed; while probing, the first band, to learn whether pictures show here.
  // Only bands on show (a page cut to the tree limit leaves some out).
  const send = bands.filter((b, i) => sentBands.has(b.band.key) && ((probing && i === 0) || sentBands.get(b.band.key) !== b.source.png))
  for (const b of send) {
    const sent = await $.ui.blit({ requestId: view.pane, key: b.band.key, source: b.source })
    if (sent.deny === undefined) {
      pictures = true
      sentBands.set(b.band.key, b.source.png)
      continue
    }
    const saysWords = sent.fix !== undefined || /\balt\b/.test(sent.deny)
    if (pictures === undefined && saysWords) {
      // This terminal draws the words in place of the picture: the text cards take over.
      pictures = false
      shown = undefined
      $.ui.invalidate('ui.render')
      return
    }
    if (pictures === undefined && probes < PROBES) {
      // Not mounted yet: ask again, a few times.
      probes++
      schedule($, PROBE_MS * 2)
      return
    }
    // Closed, behind another pane, or another size by now. The next drawing starts the frames again.
    if (shown === view) shown = undefined
    return
  }
  if (shown !== view) return
  schedule($, frameDelay(now))
}

/** Starts the frames if cards are on show and none is due; where they are SVG, has them drawn again. */
export function wake($: EngineInterface): void {
  if (shown !== undefined && timer === undefined) schedule($, 0)
  else if (isVector) {
    try {
      $.ui.invalidate('ui.render')
    } catch {
      // No surface to redraw: the next drawing has the change.
    }
  }
}

/** What the pane hook gives: the elements of its surface and the body's size. */
export type Site = {
  surface: string
  pane: string
  bodyColumns: number
  /** The rows the body shows under the tabs. */
  bodyRows: number
  /** `now`: the herdr pane's cards, the timeline too where it fits; `figure`: the timeline alone. */
  page: 'now' | 'figure'
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  elements: { Image?: any; Svg?: any; Raster?: any; Box?: any; Text?: any }
  /** What the terminal is (`TERM_PROGRAM`), and whether the person forced pictures on. */
  term?: string
  forced?: boolean
}

/**
 * The cards of a page. On the Now page the timeline joins the herdr cards only where the body has
 * the rows for all of them; it always has its own page (the herdr pane's `i fig`).
 */
export function pageOf<T extends { id: CardId; painted: Painted }>(cards: T[], page: 'now' | 'figure', bodyRows: number): T[] {
  if (page === 'figure') return cards.filter(c => c.id === 'timeline')
  const rows = cards.reduce((n, c) => n + c.painted.rows, 0)
  return rows <= bodyRows ? cards : cards.filter(c => c.id !== 'timeline')
}

const hexOf = (c: number): string | undefined => (c < 0 || c & 0x01000000 ? undefined : '#' + c.toString(16).padStart(6, '0'))

/**
 * A card's cells as elements: Raster runs for the cells, and each title row as bold Text on a Box
 * whose ground is the card's fill (a Raster cell has no bold).
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function cellNodes(id: string, grid: Cells, el: { Raster?: any; Box?: any; Text?: any }, boldText: boolean): { id: string; node: unknown; rows: number }[] {
  return segmentsOf(grid, boldText).map((seg, n) => {
    const key = `${id}-c${n}`
    if (seg.kind === 'cells') {
      const part = sliceRows(grid, seg.from, seg.rows)
      return { id: key, node: el.Raster({ key, columns: part.columns, rows: part.rows, cells: packCells(part) }), rows: part.rows }
    }
    const runs = rowRuns(grid, seg.row).map(r =>
      el.Text({ color: hexOf(r.fg), backgroundColor: hexOf(r.bg), weight: r.bold ? 'bold' : undefined, children: r.text }),
    )
    return { id: key, node: el.Box({ key, flexDirection: 'row', width: grid.columns, height: 1, children: [el.Text({ wrap: 'truncate-end', children: runs })] }), rows: 1 }
  })
}

/** Title rows as bold Text in the cell view. Off: Warp draws a Raster cell and a Box of the same colour differently. */
export let boldTitles = false

/** For tests: draw title rows as bold Text. */
export function setBoldTitles(on: boolean): void {
  boldTitles = on
}

/**
 * Whether the cards go out as cells. `cells`: always. `auto`: in Warp (no pictures there unless
 * forced), where the terminal refused a picture frame, or where there is no Image element.
 * `picture` and `text` never.
 */
export function cellsWanted(view: string, pictures: boolean | undefined, term: string | undefined, forced: boolean | undefined, hasImage: boolean): boolean {
  if (view === 'cells') return true
  if (view !== 'auto') return false
  return pictures === false || !hasImage || (term === 'WarpTerminal' && !forced)
}

/** How the person set the pictures up: `cellShape` undefined is the mod's own pick. */
export type Setup = { theme: string; cellShape: number | undefined; view: string; motion: string }

/** The palette: the person's choice, else the terminal's ground, else dark. */
export function paletteFor(theme: string, scheme: 'light' | 'dark' | undefined): Palette {
  if (theme === 'light') return LIGHT
  if (theme === 'dark') return DARK
  return scheme === 'light' ? LIGHT : DARK
}

/**
 * The page as elements, one Image a band of a card (a terminal draws a picture only while all of
 * it is in view, so a card cut by the pane's edge still shows its bands that fit); one SVG a card
 * on the desktop and in the editor; none where the person chose text, the pane is too narrow, or
 * the terminal shows no pictures. Frames then run by themselves.
 */
export async function drawCards(
  $: EngineInterface,
  site: Site,
  model: Model,
  now: number,
  setup: Setup,
  palette: Palette,
  shape: number,
): Promise<{ id: string; node: unknown; rows: number }[] | undefined> {
  if (setup.view === 'text') return undefined
  retarget(model.pieces, now)
  isVector = site.surface !== 'terminal'
  const columns = Math.min(MAX_COLUMNS, Math.floor(site.bodyColumns))
  if (isVector) {
    const { Svg } = site.elements
    if (!Svg) return undefined
    // SVG takes no frames: what moves is drawn where it ends.
    const cards = withBackdrop(pageOf(buildCards(model, Math.max(MIN_COLUMNS, columns), 2.2, palette, { ...STILL, eased: {} }, now), site.page, Infinity), palette)
    return cards.map(c => ({ id: c.id, node: Svg({ source: toSvg(c.painted.scene), alt: c.painted.alt }), rows: 0 }))
  }
  const { Image, Raster } = site.elements
  const bodyRows = Math.floor(site.bodyRows)
  if (columns < MIN_COLUMNS) {
    shown = undefined
    return undefined
  }
  if (cellsWanted(setup.view, pictures, site.term, site.forced, !!Image)) {
    if (!Raster) return undefined
    const cards = pageOf(gridCards(model, columns, shape, palette, motionAt(now), now), site.page, bodyRows)
    // Bold title rows as Text on a Box of the card's fill: tried in Warp, and Warp drew the Raster's
    // fill a different colour from the same colour on a Box (a seam along every title row), so
    // titles stay cells. The path stays for terminals that draw both alike (`boldTitles`).
    const boldText = boldTitles && !!site.elements.Text && !!site.elements.Box
    shown = { pane: site.pane, model, columns, shape, palette, page: site.page, bodyRows, mode: 'cells', boldText }
    schedule($, frameDelay(now))
    return cards.flatMap(c => cellNodes(c.id, toCells(c.painted.scene, COLUMN * shape, palette.cells), site.elements, boldText))
  }
  if (!Image || pictures === false) {
    shown = undefined
    return undefined
  }
  const cards = pageOf(buildCards(model, columns, shape, palette, motionAt(now), now), site.page, bodyRows)
  shown = { pane: site.pane, model, columns, shape, palette, page: site.page, bodyRows, mode: 'picture' }
  const { kept: bands, bytes } = withinTree(paintPage(cards, palette, movingAt(now).size === 0))
  lastTreeBytes = bytes
  for (const b of bands) sentBands.set(b.band.key, b.source.png)
  // The first frame after the drawing tells whether pictures show here.
  schedule($, frameDelay(now) ?? (pictures === undefined ? PROBE_MS : undefined))
  const alts = new Map(cards.map(c => [c.id, c.painted.alt]))
  return bands.map(b => ({
    id: b.band.key,
    node: Image({
      key: b.band.key,
      source: b.source,
      columns,
      rows: b.band.rows,
      // The card's words on its first band; the rest say nothing more.
      alt: b.band.key.endsWith('-0') ? (alts.get(b.id) ?? ' ') : ' ',
    }),
    rows: b.band.rows,
  }))
}

/** The compact cell band for the spot above the prompt, or none where cells are not wanted. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function drawBand(site: Site, model: Model, setup: Setup, palette: Palette, shape: number): unknown | undefined {
  const { Raster, Image } = site.elements
  if (!Raster || !cellsWanted(setup.view, pictures, site.term, site.forced, !!Image)) return undefined
  const columns = Math.min(MAX_COLUMNS * 2, Math.floor(site.bodyColumns))
  const cell = COLUMN * shape
  const { limits, context, csr } = model.pieces
  const painted = withMetrics(gridMetrics(cell), () => bandCard(limits, context, csr, columns * COLUMN, cell, palette))
  const grid = toCells(painted.scene, cell, palette.cells, false)
  return Raster({ key: 'band-c', columns: grid.columns, rows: grid.rows, cells: packCells(grid) })
}
