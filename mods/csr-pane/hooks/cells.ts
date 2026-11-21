// The third painter of the cards: terminal cells, for a terminal that shows no pictures (Warp,
// tmux without passthrough, any other). The card is laid out on the grid (scene.ts `gridMetrics`)
// and painted here as flat colour: a calm ground, a one-row gap above each card, the card's fill
// with a lighter top row, and a one-column margin each side. Every edge falls on a cell line.
// Words, bars, the chart and the marks are the terminal's own glyphs. Rows that hold a serif title
// are handed back as bold rows: the pane draws those as bold Text on the same fill.
import type { Rgb, Scene } from './paint'
import { COLUMN } from './scene'

/** The terminal's own colour, for a Raster cell. */
export const DEFAULT_COLOR = 0x01000000
/** A blank cell: a space on the cell's ground. */
export const BLANK = 0x20

/** A grid of cells: a code point, a foreground and a background for each, row by row. */
export type Cells = {
  columns: number
  rows: number
  glyphs: Uint32Array
  fg: Uint32Array
  bg: Uint32Array
  /** Cells under a title's characters: drawn bold where the pane can. */
  bold: Uint8Array
}

/** The flat colours a card is painted in. */
export type Fills = { ground: Rgb; card: Rgb; top: Rgb }

/**
 * A card laid out on the grid, as cells. `cell` is the grid's row height in scene units (one
 * column is COLUMN units). Row 0 is the gap above the card; row 1 its lighter top; the first and
 * last columns are the ground.
 */
export function toCells(scene: Scene, cell: number, fills: Fills, framed = true): Cells {
  const columns = Math.max(1, Math.round(scene.width / COLUMN))
  const rows = Math.max(1, Math.round(scene.height / cell))
  const glyphs = new Uint32Array(columns * rows).fill(BLANK)
  const fg = new Uint32Array(columns * rows).fill(DEFAULT_COLOR)
  const bg = new Uint32Array(columns * rows)
  const bold = new Uint8Array(columns * rows)
  for (let row = 0; row < rows; row++) {
    for (let col = 0; col < columns; col++) {
      const isGround = framed && (row === 0 || col === 0 || col === columns - 1)
      bg[row * columns + col] = isGround ? fills.ground : framed && row === 1 ? fills.top : fills.card
    }
  }
  for (const s of scene.shapes) {
    if (s.kind !== 'text') continue
    const chars = Array.from(s.text)
    const row = Math.floor(s.y / cell)
    if (row < (framed ? 1 : 0) || row >= rows) continue
    const start = Math.round((s.anchor === 'end' ? s.x - chars.length * COLUMN : s.anchor === 'middle' ? s.x - (chars.length * COLUMN) / 2 : s.x) / COLUMN)
    chars.forEach((ch, n) => {
      const col = start + n
      const code = ch.codePointAt(0) ?? BLANK
      if (col < (framed ? 1 : 0) || col >= columns - (framed ? 1 : 0) || code > 0xffff) return
      const i = row * columns + col
      if (s.back !== undefined) bg[i] = s.back
      if (s.face === 'serif') bold[i] = 1
      if (code === BLANK) return
      glyphs[i] = code
      fg[i] = s.color
    })
  }
  return { columns, rows, glyphs, fg, bg, bold }
}

/** The rows that hold bold characters: the pane draws them as Text, the rest as Raster. */
export function boldRows(cells: Cells): number[] {
  const out: number[] = []
  for (let row = 0; row < cells.rows; row++) {
    for (let col = 0; col < cells.columns; col++) {
      if (cells.bold[row * cells.columns + col]) {
        out.push(row)
        break
      }
    }
  }
  return out
}

/** A run of rows of one card: cells to blit, or a bold row drawn as Text. */
export type Segment = { kind: 'cells'; from: number; rows: number } | { kind: 'text'; row: number }

/** The card cut into Raster runs and bold Text rows, top to bottom. */
export function segmentsOf(cells: Cells, withText: boolean): Segment[] {
  const bolds = new Set(withText ? boldRows(cells) : [])
  const out: Segment[] = []
  let from = 0
  for (let row = 0; row <= cells.rows; row++) {
    if (row === cells.rows || bolds.has(row)) {
      if (row > from) out.push({ kind: 'cells', from, rows: row - from })
      if (row < cells.rows) out.push({ kind: 'text', row })
      from = row + 1
    }
  }
  return out
}

/** Rows `from` to `from + rows` of a grid. */
export function sliceRows(cells: Cells, from: number, rows: number): Cells {
  const a = from * cells.columns
  const b = (from + rows) * cells.columns
  return { columns: cells.columns, rows, glyphs: cells.glyphs.slice(a, b), fg: cells.fg.slice(a, b), bg: cells.bg.slice(a, b), bold: cells.bold.slice(a, b) }
}

/** One row as runs of like cells: the characters, their colours, and whether they are bold. */
export function rowRuns(cells: Cells, row: number): { text: string; fg: number; bg: number; bold: boolean }[] {
  const out: { text: string; fg: number; bg: number; bold: boolean }[] = []
  for (let col = 0; col < cells.columns; col++) {
    const i = row * cells.columns + col
    const code = cells.glyphs[i] ?? BLANK
    const f = code === BLANK ? -1 : (cells.fg[i] ?? DEFAULT_COLOR)
    const b = cells.bg[i] ?? DEFAULT_COLOR
    const isBold = cells.bold[i] === 1
    const last = out[out.length - 1]
    if (last && last.bg === b && (last.fg === f || f === -1) && last.bold === isBold) last.text += String.fromCharCode(code)
    else if (last && last.bg === b && last.fg === -1 && code !== BLANK && !last.bold && !isBold && /^ +$/.test(last.text)) {
      last.text += String.fromCharCode(code)
      last.fg = f
    } else out.push({ text: String.fromCharCode(code), fg: f, bg: b, bold: isBold })
  }
  return out
}

/** Cells as a Raster's `cells`: base64 of little-endian u32 triplets. */
export function packCells(cells: Cells): string {
  const words = new Uint32Array(cells.glyphs.length * 3)
  for (let i = 0; i < cells.glyphs.length; i++) {
    words[i * 3] = cells.glyphs[i] ?? BLANK
    words[i * 3 + 1] = cells.fg[i] ?? DEFAULT_COLOR
    words[i * 3 + 2] = cells.bg[i] ?? DEFAULT_COLOR
  }
  const bytes = new Uint8Array(words.buffer)
  const encode = (bytes as { toBase64?: () => string }).toBase64
  if (typeof encode === 'function') return encode.call(bytes)
  let text = ''
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(text)
}

/** The cells as lines of text, for a test or a log. */
export function cellText(cells: Cells): string[] {
  const lines: string[] = []
  for (let row = 0; row < cells.rows; row++) {
    let line = ''
    for (let col = 0; col < cells.columns; col++) line += String.fromCodePoint(cells.glyphs[row * cells.columns + col] ?? BLANK)
    lines.push(line)
  }
  return lines
}
