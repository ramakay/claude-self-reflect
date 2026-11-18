// The herdr pane's pieces, read out of what the mod already runs: the engine's text frame
// (`companion pane --frame`), its summary (`--json`), its compaction tally (`--compact-plan`),
// Claude Code's own usage, and the transcript facts (transcript.ts). Pure functions only: the
// painter (scene.ts) and the text view both draw from this one model.
import type { ContractCard, Plan, Span, Summary, Usage } from '../types'
import { toCards } from './cards'
import type { Card } from './cards'
import type { Facts } from './transcript'

export type Dreamer = {
  /** "Self-Reflect's dreamer", "The dreamer asks" or "Dreams are off". */
  title: string
  /** The state on the title row: "Looks as you work" or "Dreaming"; empty with a question. */
  tag: string
  /** The question, when the dreamer has one ready. */
  question: string
  /** True while a look runs: the ring turns. */
  dreaming: boolean
  /** "935k of 6.3M" and the share of the budget it is. */
  budget: string
  fraction: number
  /** "The last look found nothing. 5 of 48 looks end this way." */
  lastLook: string
}

export type SamePage = { read: string; more: string }
export type ShouldBe = { kind: string; text: string }
export type CodeRow = { file: string; symbol: string; state: 'verified' | 'unverified' | 'commit'; stamp: string }
export type Story = { age: string; text: string; notes: string; rows: CodeRow[] }
export type Bar = { name: string; percent: number }
export type Part = { name: string; tokens: number; ink: 'setup' | 'messages' | 'tools' | 'csr' }
export type Context = {
  used: number
  max: number
  free: number
  buffer: number
  parts: Part[]
  /** Tools, context and other shares, and the one word for them; null with no parts. */
  mix: { shares: [number, number, number]; verdict: string } | null
}
export type Limits = { headline: string; tone: 'quiet' | 'ok' | 'amber' | 'red'; bars: Bar[] }

export type Pieces = {
  dreamer: Dreamer | null
  samePage: SamePage | null
  shouldBe: ShouldBe[]
  /** Lost and found: the open threads and standing notes, in words. */
  lost: string[]
  story: Story | null
  limits: Limits | null
  context: Context | null
  /** "3 compactions · ~2.7M tokens saved (est.)": the product's claim, or null with nothing measured. */
  csr: string | null
  /** Cards the mod has no painter for: the engine's own, and frame sections the mod does not paint. */
  extra: { title: string; lines: string[] }[]
  /** The engine speaks a newer major `--json` shape than this mod reads: say "update csr-pane". */
  isOutdated: boolean
}

/** The newest major `--json` shape this mod reads. */
export const SCHEMA_MAJOR = 1

/** Frame sections the herdr pane shows that the mod has no painter for: drawn as plain cards. */
const PLAIN_SECTIONS = [/^Last time here( · .*)?$/, /^Earlier here$/]

const plain = (spans: Span[]) => spans.map(s => s.text).join('')
const paras = (card: Card | undefined) => (card?.paras ?? []).map(p => plain(p).trim()).filter(t => t !== '')

/** Tokens as the engine writes them (pyfmt.rs `short`): 950, 1.5k, 12k, 133k, 1M, 2.8M, 12M. */
export function tokens(n: number): string {
  for (const [size, unit] of [
    [1e9, 'B'],
    [1e6, 'M'],
    [1e3, 'k'],
  ] as const) {
    if (n >= size) {
      const v = n / size
      return (v < 10 ? v.toFixed(1).replace(/\.0$/, '') : v.toFixed(0)) + unit
    }
  }
  return String(Math.trunc(n))
}

/** "935k" → 935000, "6.3M" → 6300000; NaN for anything else. */
export function readTokens(text: string): number {
  const m = /^([\d.]+)\s*([kKmM]?)$/.exec(text.trim())
  if (!m) return NaN
  const n = Number(m[1])
  const unit = (m[2] ?? '').toLowerCase()
  return n * (unit === 'm' ? 1_000_000 : unit === 'k' ? 1000 : 1)
}

const STATE_LINES = new Set(['The dreamer looks as you work.', 'Dreaming.', 'Turn dreams on.'])

function dreamerOf(opening: string[], lost: string[]): Dreamer | null {
  const first = (opening[0] ?? '').replace(/^☾\s*/, '').trim()
  if (first === '' && opening.length === 0) return null
  const meta = opening.slice(1).join(' · ')
  const budget = /\bdreamer ([\d.]+[kM]?) of ([\d.]+[kM]?)/.exec(meta)
  const spent = budget ? readTokens(budget[1] ?? '') : NaN
  const cap = budget ? readTokens(budget[2] ?? '') : NaN
  const isState = STATE_LINES.has(first)
  return {
    title: first === 'Turn dreams on.' ? 'Dreams are off' : isState ? "Self-Reflect's dreamer" : 'The dreamer asks',
    tag: first === 'Dreaming.' ? 'Dreaming' : first === 'The dreamer looks as you work.' ? 'Looks as you work' : '',
    question: isState ? '' : first,
    dreaming: first === 'Dreaming.',
    budget: budget ? `${budget[1]} of ${budget[2]}` : '',
    fraction: Number.isFinite(spent) && cap > 0 ? Math.max(0, Math.min(1, spent / cap)) : 0,
    lastLook: lost.find(l => /^The last look\b|looks? end this way/.test(l)) ?? '',
  }
}

/** The numbers sentence(s) of a Same page read, then the model's words after them. */
export function samePageOf(read: string): SamePage | null {
  const text = read.trim()
  if (text === '') return null
  const sentences = text.match(/[^.!?]+[.!?]+(\s+|$)|[^.!?]+$/g) ?? [text]
  let cut = 1
  // The numbers end at "You were unclear on N." or "You were clear."; the model's words follow.
  sentences.forEach((s, i) => {
    if (i < 3 && /^\s*You were (unclear on \d+|clear)\b/.test(s)) cut = i + 1
  })
  return { read: sentences.slice(0, cut).join('').trim(), more: sentences.slice(cut).join('').trim() }
}

function shouldBeOf(lines: string[]): ShouldBe[] {
  return lines.map(line => {
    const m = /^([a-z][a-z ]{1,14}):\s+(.*)$/.exec(line)
    const text = (m ? (m[2] ?? '') : line).replace(/\s*·\s*evidence\s*$/, '').trim()
    return { kind: m ? (m[1] ?? '') : 'note', text }
  })
}

const CODE_ROW = /^[+~\-*!]\s+(\S+)\s+·\s+(\S+)\s+(.+)$/

function storyOf(card: Card | undefined, notes: string): Story | null {
  if (!card) return null
  const age = /refreshed (.+)$/.exec(card.title)?.[1] ?? ''
  const text: string[] = []
  const rows: CodeRow[] = []
  for (const line of paras(card)) {
    const m = CODE_ROW.exec(line)
    if (m) {
      const stamp = (m[3] ?? '').trim()
      const state = /^unverified$/.test(stamp) ? 'unverified' : /^verified$/.test(stamp) ? 'verified' : 'commit'
      rows.push({ file: m[1] ?? '', symbol: m[2] ?? '', state, stamp: stamp.replace(/^⌗/, '') })
    } else text.push(line)
  }
  return { age, text: text.join(' '), notes, rows }
}

/** One quota row of the engine's foot: "7D 8% · Early · earlier weeks peaked around 78% · ...". */
type Row = { code: string; name: string; used: number; early: boolean; reach?: number; runsOut?: string }

const QUOTA_NAMES: Record<string, string> = { '5H': '5 Hour', '7D': 'Weekly' }

export function quotaRows(foot: string[]): Row[] {
  const rows: Row[] = []
  for (const line of foot) {
    for (const code of ['5H', '7D']) {
      // The engine may mark the figure ("13%»": ahead of pace); the mark is not part of the state.
      const m = new RegExp(`(?:^|·\\s)(?<!Codex )${code} (\\d+(?:\\.\\d+)?)%[^\\s·]*((?:\\s·\\s[^·]+)*)`).exec(line)
      if (!m || rows.some(r => r.code === code)) continue
      const rest = (m[2] ?? '').split('·').map(s => s.trim())
      const state = rest.find(s => s !== '') ?? ''
      const pace = /On pace for about (\d+)%/.exec(state)
      const out = /On pace to run out around (.+)$/.exec(state)
      rows.push({
        code,
        name: QUOTA_NAMES[code] ?? code,
        used: Number(m[1]),
        early: /^Early\b|^Too early/.test(state),
        reach: pace ? Number(pace[1]) : undefined,
        runsOut: out ? (out[1] ?? '').trim() : undefined,
      })
    }
  }
  return rows
}

const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday']

function resetName(iso: string | undefined, now: number): string {
  const at = iso ? Date.parse(iso) : NaN
  if (!Number.isFinite(at)) return 'the reset'
  const when = new Date(at)
  if (when.toDateString() === new Date(now).toDateString()) {
    const h = when.getHours() % 12 || 12
    return `the ${h}:${String(when.getMinutes()).padStart(2, '0')} ${when.getHours() < 12 ? 'am' : 'pm'} reset`
  }
  return `${DAYS[when.getDay()]}'s reset`
}

/** A span left as the engine writes it (forecast.rs `until`): "2d 20h", "4h 05m", "23m". */
export function untilWords(iso: string | undefined, now: number): string {
  const ms = iso ? Date.parse(iso) - now : NaN
  const s = Math.max(0, Number.isFinite(ms) ? ms / 1000 : 0)
  const d = Math.floor(s / 86400)
  const h = Math.floor((s % 86400) / 3600)
  const m = Math.floor((s % 3600) / 60)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${String(m).padStart(2, '0')}m`
  return `${m}m`
}

/** When a run-out clock ("3:10 pm") falls, for ordering; unknown words sort last. */
function clockAt(text: string, now: number): number {
  const m = /(\d{1,2}):(\d{2})\s*(am|pm)/i.exec(text)
  if (!m) return Infinity
  const d = new Date(now)
  d.setHours((Number(m[1]) % 12) + (/pm/i.test(m[3] ?? '') ? 12 : 0), Number(m[2]), 0, 0)
  return d.getTime() < now ? d.getTime() + 86_400_000 : d.getTime()
}

/**
 * The sentence that heads the limits card, as the engine words it (forecast.rs `headline`), from
 * the quota rows of its text frame. `resets` is Claude Code's reset time per code.
 */
export function headlineOf(rows: Row[], resets: Record<string, string | undefined>, now: number): { text: string; tone: Limits['tone'] } | null {
  const first = rows[0]
  if (!first) return null
  // The soonest to run out; on a tie the first, as the engine's min_by.
  const out = rows
    .filter(r => r.runsOut !== undefined)
    .reduce<Row | undefined>((a, r) => (a === undefined || clockAt(r.runsOut ?? '', now) < clockAt(a.runsOut ?? '', now) ? r : a), undefined)
  if (out) return { text: `On course to run out: ${out.name} limit hit around ${out.runsOut}.`, tone: 'red' }
  const full = rows.find(r => r.used >= 100)
  if (full) return { text: `${full.name} limit reached. It resets in ${untilWords(resets[full.code], now)}.`, tone: 'red' }
  const reach = (r: Row) => Math.max(r.reach ?? r.used, r.used)
  const late = rows.filter(r => !r.early)
  // The engine's max_by: on a tie the last one wins.
  const worst = late.reduce<Row | undefined>((a, r) => (a === undefined || reach(r) >= reach(a) ? r : a), undefined)
  const at = worst ? reach(worst) : 0
  const usedUp = rows.filter(r => r.early && r.used >= 75 && r.used > at).reduce<Row | undefined>((a, r) => (a === undefined || r.used >= a.used ? r : a), undefined)
  if (usedUp) return { text: `${usedUp.name} is already at ${Math.round(usedUp.used)}% of its limit.`, tone: usedUp.used >= 90 ? 'red' : 'amber' }
  if (worst && at >= 90) return { text: `Close to the limit. Expect about ${Math.round(at)}% of the ${worst.name} limit by reset.`, tone: 'red' }
  if (worst && at >= 75) return { text: `Heading toward the limit: about ${Math.round(at)}% of the ${worst.name} limit by reset.`, tone: 'amber' }
  if (rows.every(r => r.reach === undefined && r.runsOut === undefined)) {
    if (rows.every(r => r.early)) {
      const lead = rows.find(r => r.code === '7D') ?? first
      return { text: `Early in the ${lead.code === '7D' ? 'week' : 'window'}. ${lead.name} is at ${Math.round(lead.used)}% so far.`, tone: 'quiet' }
    }
    return { text: `Watching the limits: ${first.name} is at ${Math.round(first.used)}% so far.`, tone: 'quiet' }
  }
  // As the engine: the week's row with a pace, else the worst; neither, no headline.
  const lead = rows.find(r => r.code === '7D' && r.reach !== undefined) ?? worst
  if (!lead) return null
  return { text: `On track. You should reach ${resetName(resets[lead.code], now)} with room to spare.`, tone: 'ok' }
}

function limitsOf(foot: string[], usage: Usage | null, now: number): Limits | null {
  const rows = quotaRows(foot)
  const live = new Map((usage?.limits ?? []).map(l => [l.kind, l]))
  // Claude Code's own reading is the newer one; the engine's frame says the state.
  for (const r of rows) {
    const kind = r.code === '7D' ? 'seven_day' : 'five_hour'
    const reading = live.get(kind)?.percentUsed
    if (typeof reading === 'number') r.used = reading
  }
  const resets: Record<string, string | undefined> = { '7D': live.get('seven_day')?.resetsAt, '5H': live.get('five_hour')?.resetsAt }
  const bars: Bar[] = rows.map(r => ({ name: r.name, percent: r.used }))
  if (rows.length === 0) {
    const week = live.get('seven_day')?.percentUsed
    if (typeof week === 'number') bars.push({ name: 'Weekly', percent: week })
  }
  const codex = foot.map(l => /Codex 7D (\d+(?:\.\d+)?)%/.exec(l)).find(m => m)
  if (codex) bars.push({ name: 'Codex 7D', percent: Number(codex[1]) })
  if (bars.length === 0) return null
  const head = headlineOf(rows, resets, now)
  return { headline: head?.text ?? '', tone: head?.tone ?? 'quiet', bars }
}

/** Where the transcript says the window's messages went: tool output, talk, or Self-Reflect's text. */
export type Shares = { tools: number; talk: number; csr: number; compacted: boolean }

/** The Context card: Claude Code's window, its parts as the herdr pane names them, and the mix. */
export function contextOf(usage: Usage | null, shares: Shares | null): Context | null {
  if (!usage || usage.max <= 0) return null
  const used = usage.segments.filter(s => s.kind === 'used')
  const free = usage.segments.find(s => s.kind === 'free')?.tokens ?? Math.max(0, usage.max - usage.used)
  const buffer = usage.segments.find(s => s.kind === 'buffer')?.tokens ?? 0
  const messages = used.filter(s => s.name === 'Messages').reduce((n, s) => n + s.tokens, 0)
  const setup = used.length > 0 ? used.reduce((n, s) => n + s.tokens, 0) - messages : 0
  const parts: Part[] = []
  if (used.length === 0) {
    if (usage.used > 0) parts.push({ name: 'In use', tokens: usage.used, ink: 'messages' })
  } else {
    if (setup > 0) parts.push({ name: shares?.compacted ? 'Summary + setup' : 'Setup', tokens: setup, ink: 'setup' })
    const total = shares ? shares.tools + shares.talk + shares.csr : 0
    if (shares && total > 0 && messages > 0) {
      parts.push({ name: 'Messages', tokens: (messages * shares.talk) / total, ink: 'messages' })
      parts.push({ name: 'Tool results', tokens: (messages * shares.tools) / total, ink: 'tools' })
      parts.push({ name: 'CSR injected', tokens: (messages * shares.csr) / total, ink: 'csr' })
    } else if (messages > 0) parts.push({ name: 'Messages', tokens: messages, ink: 'messages' })
  }
  // A sliver under half a percent of what is in use is not drawn or named, as in the herdr pane.
  const kept = parts.filter(p => p.tokens >= Math.max(1, usage.used * 0.005))
  return { used: usage.used, max: usage.max, free, buffer, parts: kept, mix: mixOf(kept) }
}

/** Tools, context and other as shares, and "tool-heavy", "context-heavy" or "mixed" (half or more). */
export function mixOf(parts: Part[]): Context['mix'] {
  if (parts.some(p => p.name === 'In use')) return null
  const mix: [number, number, number] = [0, 0, 0]
  for (const p of parts) mix[p.ink === 'tools' ? 0 : p.ink === 'csr' ? 2 : 1] += Math.max(0, p.tokens)
  const total = mix[0] + mix[1] + mix[2]
  if (total <= 0) return null
  const shares = mix.map(n => n / total) as [number, number, number]
  return { shares, verdict: shares[0] >= 0.5 ? 'tool-heavy' : shares[1] >= 0.5 ? 'context-heavy' : 'mixed' }
}

/** Reads `companion pane --compact-plan`: the compactions Self-Reflect made and its net saving (may be negative). */
export function parsePlan(stdout: string): Plan | null {
  try {
    const raw = JSON.parse(stdout) as { story?: { by_csr?: unknown; payback?: unknown } }
    const by = typeof raw.story?.by_csr === 'number' ? raw.story.by_csr : 0
    const saved = typeof raw.story?.payback === 'number' ? raw.story.payback : 0
    return { compactions: Math.max(0, Math.floor(by)), saved }
  } catch {
    return null
  }
}

/**
 * The line under the limits, as the engine builds `csr_row` (render/instrument.rs, the CSR
 * rings): the compactions Self-Reflect made, its net saving only when above zero, and its share
 * of the weekly limit only when above zero (the frame's "CSR 0.1 pts"). The engine's "N recalls
 * used" needs a figure (`asks`) the mod is not given.
 */
export function csrLine(plan: Plan | null, foot: string[] = []): string | null {
  const bits: string[] = []
  if (plan && plan.compactions > 0) bits.push(`${plan.compactions} compaction${plan.compactions === 1 ? '' : 's'}`)
  if (plan && plan.saved > 0) bits.push(`~${tokens(plan.saved)} tokens saved (est.)`)
  const pts = foot.map(l => /\bCSR (\d+(?:\.\d+)?) pts\b/.exec(l)).find(m => m)
  if (pts && Number(pts[1]) > 0) bits.push(`${pts[1]} pts of the weekly limit`)
  return bits.length > 0 ? bits.join(' · ') : null
}

export function sharesOf(facts: Facts | null): Shares | null {
  if (!facts) return null
  const { tools, talk, csr } = facts.window
  return tools + talk + csr > 0 ? { tools, talk, csr, compacted: facts.compacts.length > 0 } : null
}

/** The ids this mod has a painter for; any other contract card draws as a plain card. */
const PAINTED_IDS = new Set(['dreamer', 'same_page', 'should_be', 'story', 'limits', 'agents'])

const lineOf = (c: ContractCard | undefined, style: string) => c?.lines.find(l => l.style === style)?.text ?? ''

/**
 * The card pieces from the engine's finished cards (`schema` 1): its words as they come, no
 * wording and no heading split here. The layout reads the same pieces the frame path makes.
 */
export function fromContract(cards: ContractCard[]): Pick<Pieces, 'dreamer' | 'samePage' | 'shouldBe' | 'story' | 'limits' | 'extra'> {
  const byId = (id: string) => cards.find(c => c.id === id)
  const d = byId('dreamer')
  const sp = byId('same_page')
  const sb = byId('should_be')
  const st = byId('story')
  const lm = byId('limits')
  return {
    dreamer: d
      ? {
          title: d.title,
          tag: d.tag ?? '',
          question: d.lines.filter(l => l.style === 'body').map(l => l.text).join(' '),
          dreaming: d.ring?.turning === true,
          budget: d.ring ? `${tokens(d.ring.spent)} of ${tokens(d.ring.cap)}` : '',
          fraction: d.ring && d.ring.cap > 0 ? Math.max(0, Math.min(1, d.ring.spent / d.ring.cap)) : 0,
          lastLook: lineOf(d, 'dim'),
        }
      : null,
    samePage: sp ? { read: sp.lines[0]?.text ?? '', more: sp.lines.slice(1).map(l => l.text).join(' ') } : null,
    shouldBe: sb ? shouldBeOf(sb.lines.map(l => l.text)) : [],
    story: st
      ? {
          age: (st.tag ?? '').replace(/^refreshed\s+/, ''),
          text: lineOf(st, 'body'),
          notes: lineOf(st, 'mint'),
          rows: st.lines
            .filter(l => l.style === 'code')
            .flatMap(l => {
              const m = CODE_ROW.exec(l.text.replace(/\s+/g, ' '))
              if (!m) return []
              const stamp = (m[3] ?? '').trim()
              const state = stamp === 'unverified' ? 'unverified' : stamp === 'verified' ? 'verified' : 'commit'
              return [{ file: m[1] ?? '', symbol: m[2] ?? '', state: state as CodeRow['state'], stamp: stamp.replace(/^⌗/, '') }]
            }),
        }
      : null,
    limits: lm
      ? {
          headline: lm.title,
          tone: (['quiet', 'ok', 'amber', 'red'].includes(lm.tone ?? '') ? lm.tone : 'quiet') as Limits['tone'],
          bars: lm.rows.map(r => ({ name: r.label, percent: Math.round(r.value * 1000) / 10 })),
        }
      : null,
    extra: cards.filter(c => !PAINTED_IDS.has(c.id)).map(c => ({ title: c.title, lines: c.lines.map(l => l.text) })),
  }
}

/** The engine's context window (schema 1), with its names, its mix and its verdict. */
export function fromContractContext(c: NonNullable<Summary['context']>): Context {
  const inks: Record<string, Part['ink']> = { setup: 'setup', messages: 'messages', tools: 'tools', csr: 'csr', inuse: 'messages' }
  return {
    used: c.used,
    max: c.max,
    free: c.free,
    buffer: c.buffer ?? 0,
    parts: c.parts.map(p => ({ name: p.name, tokens: p.tokens, ink: inks[p.ink] ?? 'messages' })),
    mix: c.mix ? { shares: c.mix.shares, verdict: c.mix.verdict } : null,
  }
}

/** All the pieces, from the frame's lines and the other readings. */
export function piecesOf(
  lines: Span[][] | null,
  cols: number,
  usage: Usage | null,
  plan: Plan | null,
  summary: Summary | null,
  facts: Facts | null,
  now: number,
): Pieces {
  // The engine words the cards itself (schema 1 and on): draw them, no wording here.
  const contract = (summary?.schema ?? 0) >= 1 && (summary?.cards?.length ?? 0) > 0 ? fromContract(summary?.cards ?? []) : null
  const cards = lines && !contract ? toCards(lines, cols) : []
  const find = (re: RegExp) => cards.find(c => re.test(c.title))
  const opening = cards.find(c => c.title === '')
  const lostCard = paras(find(/^Lost and found$/))
  const notes = lostCard.find(l => /^\d+ notes? kept$/.test(l)) ?? ''
  const lost = lostCard.filter(l => l !== notes)
  const same = paras(find(/^Same page\?$/)).join(' ')
  const foot = paras(find(/^Cost and limits$/))
  return {
    dreamer: contract ? contract.dreamer : dreamerOf(paras(opening), lost),
    samePage: contract ? contract.samePage : samePageOf(same),
    shouldBe: contract ? contract.shouldBe : shouldBeOf(paras(find(/^Should be a skill \/ memory \/ tool$/))),
    lost: lost.filter(l => !/^The last look\b|looks? end this way/.test(l)),
    story: contract ? contract.story : storyOf(find(/^Story( · .*)?$/), notes),
    limits: contract ? contract.limits : limitsOf(foot, usage, now),
    context: summary?.context ? fromContractContext(summary.context) : contextOf(usage, sharesOf(facts)),
    csr: summary?.csrRow ? summary.csrRow.parts.join(' · ') : csrLine(plan, foot),
    extra: contract ? contract.extra : cards.filter(c => PLAIN_SECTIONS.some(re => re.test(c.title))).map(c => ({ title: c.title, lines: paras(c) })),
    isOutdated: (summary?.schema ?? 0) > SCHEMA_MAJOR,
  }
}
