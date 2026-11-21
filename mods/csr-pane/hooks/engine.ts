// Runs the installed csr-engine and stores what it answered in $.state.
// The render hooks only read the atoms; every process starts here.
import { atom, update } from 'claude-code'
import type { EngineInterface, PluginOptions } from 'claude-code'

import type { Change, ContractCard, Dream, Summary } from '../types'
import { parsePlan } from './pieces'
import { PANE } from './state'
import { parseFrame } from './sgr'
import { newReader, readMore, transcriptPath } from './transcript'
import type { Facts, Reader } from './transcript'
import { readUsage } from './usage'

// The atoms of the $.state contract (types/index.d.ts). Keep these in step with register.tsx/engine.ts.
const frameAtom = atom({ plugin: 'csr-pane', key: 'frame' } as const, null, { shape: 'frame-1' })
const summaryAtom = atom({ plugin: 'csr-pane', key: 'summary' } as const, null, { shape: 'summary-3' })
const usageAtom = atom({ plugin: 'csr-pane', key: 'usage' } as const, null, { shape: 'usage-2' })
const failureAtom = atom({ plugin: 'csr-pane', key: 'failure' } as const, null)
const placedAtom = atom({ plugin: 'csr-pane', key: 'isPlaced' } as const, false)
const planAtom = atom({ plugin: 'csr-pane', key: 'plan' } as const, null, { shape: 'plan-1' })

const TIMEOUT_MS = 5000
const INSTALLED = '/usr/local/bin/csr-engine'

/** The body size the Pane hook last drew at; the refresh asks the engine for this size. */
export type Seat = { cols: number; rows: number }

/** Resolves the engine: the userConfig path, then the installed binary, then PATH. */
export async function enginePath($: EngineInterface, options: PluginOptions): Promise<string> {
  const set = typeof options.enginePath === 'string' ? options.enginePath.trim() : ''
  if (set !== '') return set
  const stat = await $.fs.stat(INSTALLED).catch(() => undefined)
  return stat?.kind === 'file' ? INSTALLED : 'csr-engine'
}

type Ran = { ok: true; stdout: string } | { ok: false; why: string; refused?: boolean }

async function run($: EngineInterface, argv: string[]): Promise<Ran> {
  const start = await $.clock.now()
  try {
    const out = await $.process.run(argv, { timeoutMs: TIMEOUT_MS })
    // An engine older than the pane contract does not know `--cards`: its argument parser says so
    // and exits 2. That is no failure to show: the mod words the cards from a text frame instead.
    const refused = out.exitCode === 2 && argv.includes('--cards') && /--cards/.test(String(out.stderr ?? ''))
    if (out.exitCode !== 0) return { ok: false, why: `exit ${out.exitCode}`, refused }
    return { ok: true, stdout: out.stdout }
  } catch (err) {
    // The rejection's wording is the host's; the elapsed time says a timeout too.
    const text = String((err as Error)?.message ?? err)
    const late = (await $.clock.now()) - start >= TIMEOUT_MS - 500
    return { ok: false, why: late || /time/i.test(text) ? 'timeout' : 'could not start' }
  }
}

const text = (v: unknown) => (typeof v === 'string' ? v : '')

// What the Changed tab keeps: the newest sessions and a few changes of each.
const MAX_CHANGES = 12
const MAX_SHIFTS = 3

function toChanges(raw: unknown): Change[] {
  if (!Array.isArray(raw)) return []
  return raw
    .filter((c): c is Record<string, unknown> => !!c && typeof c === 'object' && typeof c.title === 'string')
    .slice(0, MAX_CHANGES)
    .map(c => ({
      title: text(c.title),
      talked: text(c.talked),
      commits: typeof c.commits === 'number' ? c.commits : 0,
      shifts: (Array.isArray(c.shifts) ? c.shifts : [])
        .filter((s): s is Record<string, unknown> => !!s && typeof s === 'object')
        .slice(0, MAX_SHIFTS)
        .map(s => ({ file: text(s.file), symbol: text(s.symbol), verb: text(s.verb), subject: text(s.subject) })),
    }))
}

/** Code changes counted by the day they were made. */
function toDays(raw: unknown): Record<string, number> {
  const days: Record<string, number> = {}
  if (!Array.isArray(raw)) return days
  for (const s of raw) {
    const day = s && typeof s === 'object' ? text((s as Record<string, unknown>).date).slice(0, 10) : ''
    if (/^\d{4}-\d{2}-\d{2}$/.test(day)) days[day] = (days[day] ?? 0) + 1
  }
  return days
}

/** The engine's cards (`[{ id, title, tag, tone, lines, rows, ring }]`); a line may be a string or `{ text, style }`. */
export function toCardList(raw: unknown): ContractCard[] | undefined {
  if (!Array.isArray(raw)) return undefined
  const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0)
  return raw
    .filter((c): c is Record<string, unknown> => !!c && typeof c === 'object' && typeof c.title === 'string')
    .slice(0, 12)
    .map(c => {
      const lines = (Array.isArray(c.lines) ? c.lines : typeof c.text === 'string' ? [c.text] : [])
        .map(l => (typeof l === 'string' ? { text: l, style: 'body' } : l && typeof l === 'object' ? { text: text((l as Record<string, unknown>).text), style: text((l as Record<string, unknown>).style) || 'body' } : null))
        .filter((l): l is { text: string; style: string } => !!l && l.text !== '')
        .slice(0, 12)
      const rows = (Array.isArray(c.rows) ? c.rows : [])
        .filter((r): r is Record<string, unknown> => !!r && typeof r === 'object')
        .map(r => ({ label: text(r.label), value: num(r.value), text: text(r.text), tone: text(r.tone) }))
      const ring = c.ring && typeof c.ring === 'object' ? (c.ring as Record<string, unknown>) : undefined
      return {
        id: text(c.id),
        title: text(c.title),
        tag: typeof c.tag === 'string' ? c.tag : undefined,
        tone: typeof c.tone === 'string' ? c.tone : undefined,
        lines,
        rows,
        ring: ring ? { spent: num(ring.spent), cap: num(ring.cap), turning: ring.turning === true } : undefined,
      }
    })
}

const n0 = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0)

/** The engine's context window (`context` of `--json --cards`, schema 1). */
export function toContext(raw: unknown): Summary['context'] {
  if (!raw || typeof raw !== 'object') return undefined
  const c = raw as Record<string, unknown>
  if (!(n0(c.max) > 0)) return undefined
  const mix = c.mix && typeof c.mix === 'object' ? (c.mix as Record<string, unknown>) : undefined
  const shares = Array.isArray(mix?.shares) ? (mix?.shares as unknown[]).map(n0) : []
  return {
    used: n0(c.used),
    max: n0(c.max),
    free: n0(c.free),
    buffer: typeof c.buffer === 'number' ? c.buffer : undefined,
    parts: (Array.isArray(c.parts) ? c.parts : [])
      .filter((p): p is Record<string, unknown> => !!p && typeof p === 'object')
      .map(p => ({ name: text(p.name), tokens: n0(p.tokens), ink: text(p.ink) })),
    mix: mix && shares.length === 3 ? { shares: [shares[0] ?? 0, shares[1] ?? 0, shares[2] ?? 0], verdict: text(mix.verdict) } : undefined,
  }
}

/** The engine's Self-Reflect row (`csr_row` of `--json --cards`, schema 1). */
export function toCsrRow(raw: unknown): Summary['csrRow'] {
  if (!raw || typeof raw !== 'object') return undefined
  const r = raw as Record<string, unknown>
  const parts = Array.isArray(r.parts) ? r.parts.filter((x): x is string => typeof x === 'string') : []
  return parts.length > 0 ? { lead: text(r.lead), parts } : undefined
}

/** The major version in a `schema` or `version` field: 2, "2", "2.1" → 2. */
export function majorOf(raw: unknown): number | undefined {
  const m = /^(\d+)/.exec(typeof raw === 'number' ? String(raw) : typeof raw === 'string' ? raw : '')
  return m ? Number(m[1]) : undefined
}

/** Reads `companion pane --json` into the band's sentence, the dreams, the changed sessions and the counts. */
export function parseSummary(stdout: string): Summary | null {
  try {
    const raw = JSON.parse(stdout) as {
      status?: unknown
      dreams?: unknown
      changes?: unknown
      changes_total?: unknown
      recalled?: unknown
      shifts?: unknown
    }
    const status = typeof raw.status === 'string' ? raw.status.replace(/\bCSR\b/g, 'Self-Reflect') : ''
    const dreams: Dream[] = Array.isArray(raw.dreams)
      ? raw.dreams
          .filter((d): d is Dream => !!d && typeof d.line === 'string' && typeof d.at === 'number')
          .map(d => ({ id: String(d.id), at: d.at, line: d.line, title: d.title, answer: d.answer }))
          .sort((a, b) => b.at - a.at)
      : []
    const changes = toChanges(raw.changes)
    const changesTotal = typeof raw.changes_total === 'number' ? raw.changes_total : changes.length
    const recalled = typeof raw.recalled === 'number' && raw.recalled > 0 ? Math.floor(raw.recalled) : 0
    const r = raw as { cards?: unknown; schema?: unknown; version?: unknown; context?: unknown; csr_row?: unknown; herdr_pane_live?: unknown }
    return {
      status,
      dreams,
      changes,
      changesTotal,
      recalled,
      days: toDays(raw.shifts),
      cards: toCardList(r.cards),
      schema: majorOf(r.schema ?? r.version),
      context: toContext(r.context),
      csrRow: toCsrRow(r.csr_row),
      herdrPaneLive: typeof r.herdr_pane_live === 'boolean' ? r.herdr_pane_live : undefined,
    }
  } catch {
    return null
  }
}

// One refresh at a time. A call while one runs asks for one more run after it,
// so the newest seat and events are read, and never two engines at once.
let running: Promise<void> | null = null
let again = false

// The engine (path and modification time) that refused `--cards`: it is asked plain `--json` and
// a text frame until it changes. Plain `--json` costs what it always did; the cards cost a pass
// over the session, so they are asked only when the pane draws.
let refusedBy: string | null = null

async function engineKey($: EngineInterface, bin: string): Promise<string> {
  const stat = await $.fs.stat(bin).catch(() => undefined)
  return `${bin}@${stat?.mtimeMs ?? ''}`
}

// The session's transcript, read from where the last refresh left off (transcript.ts).
let reader: Reader | undefined

/** What the transcript said so far, or null before the first read. */
export function currentFacts(): Facts | null {
  return reader && reader.offset > 0 ? reader.facts : null
}

/** For tests: forget the transcript read so far. */
export function resetFacts(): void {
  reader = undefined
  refusedBy = null
}

async function readTranscript($: EngineInterface, sid: string): Promise<void> {
  if (sid === '') return
  if (!reader || !reader.path.endsWith(`/${sid}.jsonl`)) {
    const home = (await $.env.get('HOME').catch(() => undefined)) ?? ''
    const root = await $.session.root().catch(() => '')
    if (home === '' || root === '') return
    reader = newReader(transcriptPath(home, root, sid))
  }
  await readMore($, reader).catch(() => false)
}

async function once($: EngineInterface, options: PluginOptions, seat: Seat | null): Promise<void> {
  const bin = await enginePath($, options)
  const sid = await $.session.id().catch(() => '')
  const cwd = await $.session.cwd().catch(() => '')
  const base = ['companion', 'pane', ...(cwd ? ['--repo', cwd] : []), ...(sid ? ['--sid', sid] : [])]
  // The tally and the transcript only feed the pictures: they run with the frame, never for the band.
  // To draw, an engine that words its own cards (`--json --cards`) needs no text frame; the band
  // reads plain `--json`, which composes nothing.
  const key = await engineKey($, bin)
  const toFrame = (s: Seat) => run($, [bin, ...base, '--frame', '--cols', String(s.cols), '--rows', String(s.rows)])
  const asks = !!seat && sid !== '' && refusedBy !== key
  const [first, early, plan, usage, panes] = await Promise.all([
    run($, [bin, ...base, '--json', ...(asks ? ['--cards'] : [])]),
    seat && !asks ? toFrame(seat) : null,
    seat && sid ? run($, [bin, ...base, '--compact-plan']) : null,
    readUsage($),
    $.ui.panes().catch(() => []),
  ])
  let json = first
  let frame = early
  if (asks && !json.ok && json.refused) {
    // Asked once per engine: from now on this one gets the plain summary and a text frame.
    refusedBy = key
    ;[json, frame] = await Promise.all([run($, [bin, ...base, '--json']), seat ? toFrame(seat) : null])
  }
  if (plan?.ok) {
    const parsed = parsePlan(plan.stdout)
    if (parsed) await update($, planAtom, () => parsed)
  }
  const failed: string[] = []
  if (json.ok) {
    const summary = parseSummary(json.stdout)
    const carded = !!summary && (summary.schema ?? 0) >= 1 && (summary.cards?.length ?? 0) > 0
    // A cards answer with no cards (nothing composed): the pictures still have the text frame.
    if (seat && !frame && !carded) frame = await toFrame(seat)
    if (summary) await update($, summaryAtom, () => summary)
    else failed.push('bad answer')
  } else failed.push(json.why)
  if (frame && seat) {
    if (frame.ok) {
      const lines = parseFrame(frame.stdout)
      const at = await $.clock.now()
      await update($, frameAtom, () => ({ lines, cols: seat.cols, rows: seat.rows, at }))
    } else failed.push(frame.why)
  }
  await update($, usageAtom, () => usage)
  const placed = panes.some(p => p.id === PANE && p.isPlaced)
  await update($, placedAtom, () => placed)
  const text = failed.length === 0 ? null : { text: `Self-Reflect engine did not answer (${failed[0]}).` }
  await update($, failureAtom, () => text)
  // The transcript last: the cards are up before a long first read (4 MiB a step) is done.
  if (seat && (await readTranscript($, sid).then(() => true).catch(() => false))) {
    try {
      $.ui.invalidate('ui.render')
    } catch {
      // No surface: the next drawing reads the facts.
    }
  }
}

/** Runs the engine for `seat()` (read at each run) and stores the answers. */
export function refresh($: EngineInterface, options: PluginOptions, seat: () => Seat | null): Promise<void> {
  if (running) {
    again = true
    return running
  }
  running = (async () => {
    try {
      do {
        again = false
        await once($, options, seat()).catch(() => undefined)
      } while (again)
    } finally {
      running = null
    }
  })()
  return running
}
