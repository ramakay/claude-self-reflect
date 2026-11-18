// Turns the engine's text pane into cards the mod draws with its own elements.
// The engine sends one wide text frame; a card is a heading and its paragraphs,
// so Claude Code wraps each paragraph to the width the pane has now.
import type { Span } from '../types'

/** One card: a title (empty for the opening block) and its paragraphs. */
export type Card = { title: string; paras: Span[][] }

/** A drawn block and the rows it needs, for the split into columns. */
export type Block<T> = { rows: number; node: T }

// The headings the engine's main page writes. A heading it rewords stays a
// paragraph of the card before it: the words still show.
const HEADS = [
  /^Same page\?$/,
  /^Should be a skill \/ memory \/ tool$/,
  /^Lost and found$/,
  /^Story( · .*)?$/,
  /^Last time here( · .*)?$/,
  /^Earlier here$/,
]
// The first line of the engine's cost and limits block.
const FOOT = /^(API credit|CTX )/
export const FOOT_TITLE = 'Cost and limits'

const plain = (spans: Span[]) => spans.map(s => s.text).join('')

/**
 * Splits the frame's rows into cards. `cols` is the width the frame was asked
 * at: a row is the rest of the paragraph before it when that row's first word
 * did not fit on the line before.
 */
export function toCards(lines: Span[][], cols: number): Card[] {
  const cards: Card[] = []
  let card: Card = { title: '', paras: [] }
  let last = ''
  let inFoot = false
  const close = () => {
    if (card.title !== '' || card.paras.length > 0) cards.push(card)
  }
  for (const spans of lines) {
    const raw = plain(spans).trimEnd()
    const text = raw.trim()
    if (text === '') {
      last = ''
      continue
    }
    if (!inFoot && HEADS.some(h => h.test(text))) {
      close()
      card = { title: text, paras: [] }
      last = ''
      continue
    }
    if (!inFoot && FOOT.test(text)) {
      close()
      card = { title: FOOT_TITLE, paras: [] }
      inFoot = true
      last = ''
    }
    const word = text.split(/\s+/, 1)[0] ?? ''
    const at = card.paras.length - 1
    const before = card.paras[at]
    if (before && last !== '' && last.length + 1 + word.length > cols - 2) {
      card.paras[at] = [...before, { text: ' ' }, ...spans]
    } else card.paras.push(spans)
    last = raw
  }
  close()
  return cards
}

/** Rows a card takes at `inner` columns of text: border, title and wrapped paragraphs. */
export function cardRows(card: Card, inner: number): number {
  const width = Math.max(1, inner)
  const text = card.paras.reduce((n, p) => n + Math.max(1, Math.ceil(plain(p).length / width)), 0)
  return 2 + (card.title === '' ? 0 : 1) + text
}

/** Splits blocks into `n` columns of about equal rows, in reading order. */
export function columnsOf<T>(blocks: Block<T>[], n: number): Block<T>[][] {
  if (n <= 1) return [blocks]
  const total = blocks.reduce((s, b) => s + b.rows, 0)
  const out: Block<T>[][] = Array.from({ length: n }, () => [])
  let col = 0
  let used = 0
  for (const b of blocks) {
    if (col < n - 1 && used > 0 && used + b.rows / 2 > total / n) {
      col++
      used = 0
    }
    out[col]?.push(b)
    used += b.rows
  }
  return out
}

/** How many columns the pane's body takes: one beside the chat, more above the prompt. */
export function columnCount(placement: string, cols: number): number {
  if (placement !== 'inline') return 1
  if (cols >= 150) return 3
  return cols >= 90 ? 2 : 1
}
