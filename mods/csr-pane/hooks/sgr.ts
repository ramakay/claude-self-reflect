// Pure: the engine's ANSI text frame into rows of styled spans. No `$` here.
import type { Span } from '../types'

// A CSI sequence of any kind: SGR (`m`) and the rest, which are dropped.
const CSI = /\x1b\[([0-9;:?]*)([@-~])/g
// Any other escape left over (OSC, a lone ESC): dropped, never printed.
const OTHER_ESC = /\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b./g

const BASIC = ['#000000', '#cd3131', '#0dbc79', '#e5e510', '#2472c8', '#bc3fbc', '#11a8cd', '#e5e5e5']
const BRIGHT = ['#666666', '#f14c4c', '#23d18b', '#f5f543', '#3b8eea', '#d670d6', '#29b8db', '#ffffff']

type Style = { color?: string; bold?: boolean; dim?: boolean }

const hex = (r: number, g: number, b: number): string =>
  '#' + [r, g, b].map(n => Math.max(0, Math.min(255, n | 0)).toString(16).padStart(2, '0')).join('')

function cube(n: number): string {
  if (n < 8) return BASIC[n]
  if (n < 16) return BRIGHT[n - 8]
  if (n >= 232) {
    const v = 8 + (n - 232) * 10
    return hex(v, v, v)
  }
  const i = n - 16
  const step = (k: number) => (k === 0 ? 0 : 55 + k * 40)
  return hex(step(Math.floor(i / 36)), step(Math.floor(i / 6) % 6), step(i % 6))
}

/** Applies one SGR parameter list to a style. Unknown codes change nothing. */
export function applySgr(style: Style, params: string): Style {
  const codes = params === '' ? [0] : params.split(/[;:]/).map(p => (p === '' ? 0 : Number(p)))
  let next: Style = { ...style }
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i]
    if (c === 0) next = {}
    else if (c === 1) next.bold = true
    else if (c === 2) next.dim = true
    else if (c === 22) {
      delete next.bold
      delete next.dim
    } else if (c >= 30 && c <= 37) next.color = BASIC[c - 30]
    else if (c >= 90 && c <= 97) next.color = BRIGHT[c - 90]
    else if (c === 39) delete next.color
    else if (c === 38 || c === 48) {
      // Extended colour: 38 sets the foreground; 48 (background) is read past and ignored.
      const mode = codes[i + 1]
      if (mode === 2) {
        if (c === 38) next.color = hex(codes[i + 2], codes[i + 3], codes[i + 4])
        i += 4
      } else if (mode === 5) {
        if (c === 38) next.color = cube(codes[i + 2] ?? 0)
        i += 2
      }
    }
  }
  return next
}

const plain = (line: string): string => line.replace(CSI, '').replace(OTHER_ESC, '')

/** One ANSI line into spans. Trailing spaces are cut; no escape survives. */
export function parseLine(line: string, baseColor?: string): Span[] {
  const spans: Span[] = []
  let style: Style = {}
  let last = 0
  const push = (raw: string) => {
    const text = raw.replace(OTHER_ESC, '').replace(/[\x00-\x08\x0b-\x1f\x7f]/g, '')
    if (text === '') return
    const span: Span = { text }
    if (style.color && style.color !== baseColor) span.color = style.color
    if (style.bold) span.bold = true
    if (style.dim) span.dim = true
    const prev = spans[spans.length - 1]
    if (prev && prev.color === span.color && prev.bold === span.bold && prev.dim === span.dim) {
      prev.text += text
    } else spans.push(span)
  }
  for (const m of line.matchAll(CSI)) {
    push(line.slice(last, m.index))
    if (m[2] === 'm') style = applySgr(style, m[1])
    last = (m.index ?? 0) + m[0].length
  }
  push(line.slice(last))
  // Right-trim: the engine pads each row to the width it was asked for.
  while (spans.length > 0) {
    const tail = spans[spans.length - 1]
    tail.text = tail.text.replace(/\s+$/, '')
    if (tail.text !== '') break
    spans.pop()
  }
  return spans
}

// The engine's key row: single keys with a word each, ending in "q close".
const KEY_ROW = /^\s*(\S\s+\S+\s*)*q\s+close\s*$/

const isBlank = (line: string) => plain(line).trim() === ''

/** Drops leading and trailing blank rows and the key row, which does nothing here. */
export function trimFrame(raw: string): string[] {
  let lines = raw.replace(/\r/g, '').split('\n')
  while (lines.length > 0 && isBlank(lines[lines.length - 1])) lines.pop()
  if (lines.length > 0 && KEY_ROW.test(plain(lines[lines.length - 1]))) lines.pop()
  while (lines.length > 0 && isBlank(lines[lines.length - 1])) lines.pop()
  let first = 0
  while (first < lines.length && isBlank(lines[first])) first++
  lines = lines.slice(first)
  return lines
}

/** The frame's most used foreground: its body text colour, drawn as the theme's own text. */
export function baseColor(lines: string[]): string | undefined {
  const count = new Map<string, number>()
  for (const line of lines) {
    let style: Style = {}
    for (const m of line.matchAll(CSI)) {
      if (m[2] === 'm') style = applySgr(style, m[1])
      if (style.color) count.set(style.color, (count.get(style.color) ?? 0) + 1)
    }
  }
  let best: string | undefined
  let most = 0
  for (const [color, n] of count) if (n > most) [best, most] = [color, n]
  return best
}

/** The whole frame: trimmed, parsed, its body colour left to the theme. */
export function parseFrame(raw: string): Span[][] {
  const lines = trimFrame(raw)
  const base = baseColor(lines)
  return lines.map(line => parseLine(line, base))
}
