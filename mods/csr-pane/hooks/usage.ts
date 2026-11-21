// The context figure: read once per refresh, laid out by pure functions.
import type { EngineInterface, SessionUsage } from 'claude-code'

import type { Segment, Usage } from '../types'

/** Reads `$.session.usage` with the local summary breakdown (no API call). */
export async function readUsage($: EngineInterface): Promise<Usage | null> {
  const got = await $.session.usage({ breakdown: 'summary' }).catch(() => undefined)
  return got ? toUsage(got) : null
}

/** The figure's data. Sentence and bar both come from the breakdown when it exists. */
export function toUsage(u: SessionUsage): Usage | null {
  const limits = (u.rateLimits ?? []).map(l => ({ kind: l.kind, percentUsed: l.percentUsed, resetsAt: l.resetsAt }))
  const b = u.context?.breakdown
  if (b && b.rawMaxTokens > 0) {
    const segments: Segment[] = b.categories
      .filter(c => c.kind !== 'deferred' && c.tokens > 0)
      .map(c => ({ name: c.name, tokens: c.tokens, color: c.color, kind: c.kind as Segment['kind'] }))
    return { used: b.totalTokens, max: b.rawMaxTokens, segments, limits }
  }
  const window = u.context?.window ?? 0
  const used = u.context?.tokens
  if (window > 0 && typeof used === 'number') {
    const segments: Segment[] = [
      { name: 'Used', tokens: used, color: 'promptBorder', kind: 'used' as const },
      { name: 'Free', tokens: Math.max(0, window - used), color: 'inactive', kind: 'free' as const },
    ].filter(s => s.tokens > 0)
    return { used, max: window, segments, limits }
  }
  return limits.length > 0 ? { used: 0, max: 0, segments: [], limits } : null
}

/** Tokens in words: 950, 12k, 133k, 1M, 1.5M. */
export function tokens(n: number): string {
  if (n >= 1_000_000) {
    const m = n / 1_000_000
    return (m >= 10 ? Math.round(m) : Math.round(m * 10) / 10) + 'M'
  }
  if (n >= 1000) return Math.round(n / 1000) + 'k'
  return String(Math.round(n))
}

/** Line 1 of the figure. */
export function sentence(u: Usage): string {
  // The free figure is the free segment's, so line 1 and its label agree.
  const seg = u.segments.find(s => s.kind === 'free')
  const free = seg ? seg.tokens : Math.max(0, u.max - u.used)
  return `Context ${tokens(u.used)} of ${tokens(u.max)} used. ${tokens(free)} free.`
}

/**
 * Cells per segment, summing to exactly `cols`; every segment gets at least one
 * cell while there is room, the rest go by largest remainder.
 */
export function barCells(segments: Segment[], cols: number): number[] {
  const total = segments.reduce((s, x) => s + x.tokens, 0)
  if (cols <= 0 || total <= 0) return segments.map(() => 0)
  const floor = segments.length <= cols ? 1 : 0
  const room = cols - floor * segments.length
  const exact = segments.map(s => (s.tokens / total) * room)
  const cells = exact.map(x => Math.floor(x) + floor)
  let left = cols - cells.reduce((s, x) => s + x, 0)
  const order = exact.map((x, i) => [x - Math.floor(x), i] as const).sort((a, b) => b[0] - a[0])
  for (let k = 0; left > 0; k = (k + 1) % order.length, left--) cells[order[k][1]]++
  return cells
}

/** The labels under the bar, one per segment, packed into rows of at most `cols`. */
export function labelRows(segments: Segment[], cols: number): { text: string; color: string }[][] {
  const rows: { text: string; color: string }[][] = []
  let row: { text: string; color: string }[] = []
  let width = 0
  for (const s of segments) {
    const text = `${s.name} ${tokens(s.tokens)}`
    const need = (row.length > 0 ? 3 : 0) + text.length
    if (row.length > 0 && width + need > cols) {
      rows.push(row)
      row = []
      width = 0
    }
    row.push({ text, color: s.color })
    width += (row.length > 1 ? 3 : 0) + text.length
  }
  if (row.length > 0) rows.push(row)
  return rows
}

const LIMIT_NAMES: Record<string, string> = {
  five_hour: 'Five-hour limit',
  seven_day: 'Weekly limit',
  seven_day_opus: 'Weekly Opus limit',
  spend_limit: 'Spend limit',
}

/** How long until `iso`, in words. */
export function untilWords(iso: string | undefined, now: number): string {
  if (!iso) return ''
  const ms = Date.parse(iso) - now
  if (!Number.isFinite(ms)) return ''
  if (ms <= 0) return ' Resets now.'
  const min = Math.round(ms / 60000)
  if (min < 60) return ` Resets in ${min} min.`
  const h = Math.floor(min / 60)
  if (h < 48) return ` Resets in ${h} h ${min % 60} min.`
  return ` Resets in ${Math.round(h / 24)} days.`
}

/** One rate limit as a sentence. */
export function limitLine(l: Usage['limits'][number], now: number): string {
  const name = LIMIT_NAMES[l.kind] ?? l.kind.replace(/_/g, ' ')
  return `${name}: ${l.percentUsed}% used.${untilWords(l.resetsAt, now)}`
}

/** The bar's glyph per kind, so two segments in one theme colour still read apart. */
export const GLYPH: Record<Segment['kind'], string> = { used: '█', buffer: '▒', free: '░' }
