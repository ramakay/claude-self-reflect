// Compact at the right moment: the rules, apart from the hooks that use them (register.tsx), so a test can
// read them without an engine. No model calls here, and no payback math: that is bin/pane.py's
// (payback_requests, requests_per_turn, floor_of), asked once per turn end in the band.
//
// The numbers are one person's habit, mined from 249 typed /compacts (2026-10-12): they compact from 181k (p10) to 269k (p50); the top half, up to 407k (p90), is the drift they
// call the failure (a session left running from the phone). The turn before a /compact had nothing running in
// the background and did not end on a question 1.83 times as often as other turn ends in that range (54.8% to
// 30.0%); the median gap from that turn's end to the /compact was 252 s, so the countdown is 20 s.

import { ownWords } from './dreamer'

export const BAND = { low: 180_589, high: 269_178, drift: 407_443 } as const
export type Band = { low: number; high: number; drift: number }
export const COUNTDOWN_MS = 20_000 // min(20 s, the median gap 252 s)
export const RESUME_LATE_MS = 60_000 // a countdown this far past its end is not resumed after a reload

// A countdown or an offer the module left behind when it reloaded. A reload drops the module's timers and starts it
// over, while the band's view is the host's and stays: 10-12 15:30 a sync of the dev mod at a turn end did that, and
// the band sat at "Compacting in 0s" with nothing left to compact or to answer its 1. What the view would compact
// with rides in it (armed): a countdown resumes for what is left of it, an offer stands again. A view with nothing
// to compact with, or a countdown more than RESUME_LATE_MS past its end, is dropped; the next turn end decides.
// The setting the module loaded with decides too (Codex review #10: a countdown left by auto resumed, and compacted,
// after the person chose suggest or off; the change reloads the module): off drops it, suggest makes it an offer.
export type Resume = { act: 'none' } | { act: 'drop' } | { act: 'offer' } | { act: 'countdown'; leftMs: number }
export function resumeOf(view: { kind: 'countdown' | 'offer'; endsAt?: number; armed?: unknown } | null | undefined, now: number,
  setting: Setting = 'auto'): Resume {
  if (!view) return { act: 'none' }
  if (!view.armed || setting === 'off') return { act: 'drop' }
  if (view.kind === 'offer' || setting === 'suggest') return { act: 'offer' }
  const left = (view.endsAt ?? 0) - now
  return left < -RESUME_LATE_MS ? { act: 'drop' } : { act: 'countdown', leftMs: Math.max(0, left) }
}
export const REARM_TOKENS = 20_000 // after a cancel: the next seam and this much more context
export const ATTENDED_MS = 10 * 60 * 1000 // a prompt typed at the terminal this recently: someone is there
export const PAYBACK_UNDER = 15 // bin/pane.py's PAYBACK_UNDER: a payback longer than this many turns is not worth planning for
export const KEEP_MAX = 600 // descriptions and other sections fit around the ids
export const KEEP_IDS_MAX = 1200 // whole ids can extend the list this far

// Every word CSR puts into the conversation around a compaction opens with one of these, and csr-engine's
// provenance guard (csr-engine/src/extraction/provenance.rs, EMISSION_HEADERS) holds both: what the summary
// keeps, or the notice the phone shows, is never imported as the owner's words.
export const KEEP_HEADER = 'CSR COMPACT KEEP —'
export const NOTICE_HEADER = 'CSR compacted at'

export type Setting = 'off' | 'suggest' | 'auto'
export type Mode = 'attended' | 'unattended'
export type Human = { kind: 'composer' | 'bridge'; at: number }

// The userConfig value, read as its row allows: anything else is the default, suggest. Compacting on its own is
// opt-in: a person who never chose it is only offered a compaction, never given one.
export const settingOf = (value: unknown): Setting => (value === 'off' || value === 'auto' ? value : 'suggest')

// Attended: the last prompt a person sent was typed at the terminal less than 10 minutes ago. Unattended: it came
// through Remote Control (a phone, the web), or nobody has typed at the terminal for 10 minutes while turns go on
// (task notifications, bridge prompts). A module that has seen no prompt yet counts from when it loaded.
export function modeOf(last: Human | null, now: number, loadedAt: number): Mode {
  if (last?.kind === 'bridge') return 'unattended'
  return now - (last?.at ?? loadedAt) >= ATTENDED_MS ? 'unattended' : 'attended'
}

// ---------------------------------------------------------------- the seam

export type Running = { id: string; type: string; description: string }
export type TurnEnd = {
  ctx: number
  answer: string // the turn's final text
  asking: boolean // an AskUserQuestion still open
  running: Running[] | null // background work in flight at the turn's end; null when no Stop said
  commit?: string // a commit the turn reported landing
  gate?: boolean // a test or gate the turn reported passing
  edits?: number // files Claude edited in the turn
}
// What a turn end was, as the log keeps it: whether it is a seam, its why, and the signs it was read from.
export type Seam = { isSeam: boolean; why: string; question: boolean; wait?: string; commit?: string; gate?: boolean; edits?: number; running?: number }

// The final text ends on a question: its last character, markdown and closing quotes aside, is "?".
export function endsOnQuestion(text: string): boolean {
  return /\?$/.test(text.trimEnd().replace(/[\s*_`)\]>"'”’]+$/, ''))
}

// The question a turn ended on: its last sentence, as the owner would read it.
export function lastQuestion(text: string): string | null {
  if (!endsOnQuestion(text)) return null
  const tail = text.trimEnd().replace(/[\s*_`)\]>"'”’]+$/, '')
  const found = /([^.!?\n]*\?)$/.exec(tail)
  const line = (found?.[1] ?? '').replace(/[*_`#>]/g, '').replace(/\s+/g, ' ').trim()
  return line || null
}

// A commit SHA named next to commit/landed/merged/HEAD (agents commit; the main thread reports it), and the words
// a passing gate is reported in: the patterns the habit mining used.
const SHA_NEAR = /\b(?:commit(?:ted)?|landed|merged|SHA|HEAD)\b[^\n]{0,40}?\b(?=[0-9a-f]*[a-f])(?=[0-9a-f]*\d)([0-9a-f]{7,40})\b/i
const GATE_WORDS = /\b(?:all\s+(?:\d+\s+)?(?:tests?|gates?|checks?)\s+(?:pass|green)|tests?\s+(?:pass|green)|gates?\s+(?:pass|green)|\d+\s+passed|test result: ok)/i
export const commitIn = (text: string): string | undefined => SHA_NEAR.exec(text)?.[1]?.slice(0, 7).toLowerCase()
export const gateIn = (text: string): boolean => GATE_WORDS.test(text)

// Background work that keeps a turn end from being a seam: a running or pending task of any type but a monitor
// (a watcher that never ends on its own).
export const blocking = (running: Running[]) => running.filter(task => task.type !== 'monitor')

// A seam: what preceded the owner's own compactions. Nothing running in the background and no question waiting.
// `why` is what the band and the log say about it, the strongest sign first.
export function seamOf(end: TurnEnd): Seam {
  const question = end.asking || endsOnQuestion(end.answer)
  const busy = end.running === null ? null : blocking(end.running)
  const commit = end.commit ?? commitIn(end.answer)
  const gate = end.gate || gateIn(end.answer)
  const signs = { ...(commit ? { commit } : {}), ...(gate ? { gate } : {}), ...(end.edits ? { edits: end.edits } : {}), running: busy?.length ?? 0 }
  const why = commit
    ? `commit ${commit} landed`
    : gate
      ? 'gates passed'
      : end.edits
        ? `${end.edits} ${end.edits === 1 ? 'edit' : 'edits'} saved`
        : 'nothing running in the background'
  if (question) return { isSeam: false, why, question, wait: end.asking ? 'a question is open' : 'your answer', ...signs }
  if (busy === null) return { isSeam: false, why, question, wait: 'background work unknown', ...signs }
  if (busy.length) {
    const agents = busy.filter(task => task.type !== 'shell').length
    const shells = busy.length - agents
    const parts = [agents ? `${agents} ${agents === 1 ? 'agent' : 'agents'}` : '', shells ? `${shells} ${shells === 1 ? 'shell' : 'shells'}` : '']
    return { isSeam: false, why, question, wait: `${parts.filter(Boolean).join(' and ')} running`, ...signs }
  }
  return { isSeam: true, why, question, ...signs }
}

// ---------------------------------------------------------------- the decision

export type Act = 'none' | 'plan' | 'offer' | 'countdown' | 'compact'
// Compacting now pays back after this many requests (an API call each; a turn makes many), at this session's
// requests a turn (null before it has two whole turns): bin/pane.py's payback_requests and requests_per_turn.
export type Payback = { requests: number; perTurn: number | null }
export type Verdict = { act: Act; wait?: string; forced: boolean }
export type Ask = {
  setting: Setting
  mode: Mode
  ctx: number
  payback: Payback | null | undefined // when compacting now pays back (pane.py); null: it never would; undefined: pane.py did not answer
  seam: Seam
  cancelledAt?: number | null // the context when the owner last cancelled, in this chapter
  band?: Band
}

// What to do at a main-loop turn end.
//  - off, below the band, or a compaction that would not pay back within PAYBACK_UNDER turns: nothing. With no
//    payback at all (pane.py did not answer) only the high edge acts: past it, the owner's own limit is enough.
//  - in the band: a plan; at a seam, act. At or past the high edge: act at this turn end, seam or not.
//  - a question waits on the owner: plan only; unattended past the high edge it no longer holds.
//  - after a cancel: the next seam and 20k more context.
//  - act: suggest offers a press; attended counts down 20 s with a cancel; unattended compacts now.
export function decide(ask: Ask): Verdict {
  const band = ask.band ?? BAND
  const forced = ask.ctx >= band.high
  if (ask.setting === 'off' || ask.ctx < band.low) return { act: 'none', forced: false }
  if (ask.payback === null || (ask.payback ? paybackTurns(ask.payback) : 0) > PAYBACK_UNDER || (ask.payback === undefined && !forced)) return { act: 'none', forced: false }
  if (ask.seam.question && !(ask.mode === 'unattended' && forced)) return { act: 'plan', wait: ask.seam.wait, forced }
  if (typeof ask.cancelledAt === 'number' && ask.ctx < ask.cancelledAt + REARM_TOKENS) {
    return { act: 'plan', wait: `cancelled · waits for ${k(ask.cancelledAt + REARM_TOKENS)}`, forced }
  }
  if (!ask.seam.isSeam && !forced) return { act: 'plan', wait: ask.seam.wait, forced }
  if (ask.setting === 'suggest') return { act: 'offer', forced }
  return { act: ask.mode === 'attended' ? 'countdown' : 'compact', forced }
}

// ---------------------------------------------------------------- what CSR says

export const k = (tokens: number) => (tokens >= 1e6 ? `${(tokens / 1e6).toFixed(1)}M` : `${Math.round(tokens / 1000)}k`)
// The payback in turns; unmeasured, each request taken as a turn (the cautious reading: never sooner than known).
export const paybackTurns = (payback: Payback) => (payback.perTurn ? payback.requests / payback.perTurn : payback.requests)
const plural = (n: number, one: string) => `${n} ${n === 1 ? one : `${one}s`}`
// The payback in your prompts: its API calls over this session's calls a prompt, rounded up (a prompt under way
// counts whole); null while the calls a prompt are unmeasured. bin/pane.py's payback_prompts. 10-12: "2 of 14
// requests to pay back, no clue still": a prompt is the unit the owner counts in.
export const paybackPrompts = (payback: Payback) => (payback.perTurn ? Math.max(1, Math.ceil(payback.requests / payback.perTurn)) : null)
// `pays back in ~14 API calls (~2 prompts)`, as bin/pane.py's payback_text says it.
export function paybackText(payback: Payback | null): string {
  if (!payback) return ''
  const said = `pays back in ~${plural(Math.max(1, Math.round(payback.requests)), 'API call')}`
  const prompts = paybackPrompts(payback)
  return prompts ? `${said} (~${plural(prompts, 'prompt')})` : said
}

// The pane's plan row: `Compact planned · 214k of your 181k–269k band · pays back in ~14 API calls (~2 prompts) · waiting for a seam`.
export function planText(ctx: number, payback: Payback | null, wait: string | undefined, forced: boolean, band: Band = BAND): string {
  const where = forced ? `${k(ctx)}, past your ${k(band.low)}–${k(band.high)} band` : `${k(ctx)} of your ${k(band.low)}–${k(band.high)} band`
  const waiting = wait ? (wait.startsWith('cancelled') ? wait : `waiting: ${wait}`) : 'waiting for a seam'
  return ['Compact planned', where, paybackText(payback), waiting].filter(Boolean).join(' · ')
}

// Why it acts, for the band and the plan row: at a seam, its strongest sign; past the high edge without one, the
// edge and what it compacts over. 10-12 20:53 the row read "nothing running in the background" over a running agent.
export function actWhy(seam: Seam, band: Band = BAND): string {
  if (seam.isSeam) return seam.why
  const over = seam.running ? `${seam.wait?.replace(/ running$/, '')} kept` : seam.question ? 'question kept' : seam.wait
  return [`over ${k(band.high)}`, over].filter(Boolean).join(' · ') // the pane is ~50 columns: the row fits it
}

// The band above the prompt while it counts down: `Compacting in 20s · 214k · commit 6369e6b landed`.
export const countdownText = (left: number, ctx: number, why: string) => `Compacting in ${Math.max(0, Math.ceil(left / 1000))}s · ${k(ctx)} · ${why}`
// In suggest mode, an offer instead: a press compacts.
export const offerText = (ctx: number, payback: Payback | null, why: string) =>
  ['Compact now?', k(ctx), paybackText(payback), why].filter(Boolean).join(' · ')

// ---------------------------------------------------------------- what the summary keeps

export type Keep = {
  running: Running[] // in-flight background work, with ids
  decisions: string[] // open questions waiting on the owner
  work: string[] // uncommitted or unpushed work, with SHAs
  rules: string[] // binding rules the owner stated this session
}

const oneLine = (text: string) => text.replace(/\s+/g, ' ').trim()
const cut = (text: string, room: number) => (text.length <= room ? text : `${text.slice(0, Math.max(0, room - 1)).trimEnd()}…`)

// The keep list as the pane's card shows it, a line each: written when the compaction starts, so the card
// says what it keeps while the summary runs, and again with the result.
export type KeepRows = { running: string[]; decisions: string[]; work: string[]; rules: string[] }
export function keepRows(keep: Keep): KeepRows {
  const line = (text: string) => cut(oneLine(text), 160)
  return {
    running: keep.running.map(task => {
      const id = `${task.type} ${task.id}`
      const description = cut(oneLine(task.description), Math.max(0, 157 - id.length))
      return `${id}${description && id.length < 157 ? ` · ${description}` : ''}`
    }),
    decisions: keep.decisions.map(line),
    work: keep.work.map(line),
    rules: keep.rules.map(line),
  }
}

// Whole running ids come first. Descriptions and other sections share the room left below KEEP_MAX.
// If the ids exceed KEEP_IDS_MAX, Claude Code lists the remaining tasks after compaction.
export function keepList(keep: Keep): string {
  const head = `${KEEP_HEADER} in the summary, keep these, in this order, ids and SHAs word for word:`
  const rules = keep.rules.filter(rule => ownWords(rule).trim()) // never a quoted or forwarded line, whatever kept it
  const groups = new Map<string, Running[]>()
  for (const task of keep.running) {
    if (!groups.has(task.type)) groups.set(task.type, [])
    groups.get(task.type)!.push(task)
  }
  const tasks = [...groups.values()].flat()
  const packed = (items: Running[], descriptions: string[] = []) => {
    const parts = new Map<string, string[]>()
    items.forEach((task, i) => {
      if (!parts.has(task.type)) parts.set(task.type, [])
      parts.get(task.type)!.push(`${task.id}${descriptions[i] ?? ''}`)
    })
    return [...parts].map(([type, ids]) => `${type} ${ids.join(',')}`).join('; ')
  }
  const runningLine = (items: Running[], descriptions: string[] = []) => {
    const omitted = tasks.length - items.length
    return `Running in the background: ${packed(items, descriptions)}${omitted ? ` +${omitted} more running (Claude Code lists them after compaction)` : ''}.`
  }
  const kept = [...tasks]
  if (head.length + 4 + runningLine(kept).length > KEEP_IDS_MAX) {
    kept.length = 0
    for (const task of tasks) {
      kept.push(task)
      if (head.length + 4 + runningLine(kept).length > KEEP_IDS_MAX) {
        kept.pop()
        break
      }
    }
  }
  const running = tasks.length ? runningLine(kept) : ''
  const others = [
    keep.decisions.length ? `Waiting on the owner: ${keep.decisions.map(oneLine).join(' / ')}` : '',
    keep.work.length ? `Unfinished work: ${keep.work.map(oneLine).join('; ')}.` : '',
    rules.length ? `The owner's rules this session: ${rules.map(oneLine).join(' / ')}` : '',
  ]
  const sections = [running, ...others].filter(Boolean)
  if (!sections.length) return cut(`${KEEP_HEADER} nothing runs in the background and nothing waits on the owner: keep the task in hand and its next step.`, KEEP_MAX)
  const number = (text: string, i: number) => `${i + 1}. ${text}`
  // Fair shares of the room the ids leave: each line keeps what it needs up to an even split; what a short one
  // leaves goes on. The running line's share beyond its ids goes to its descriptions.
  let room = KEEP_MAX - head.length - sections.length - (running ? number(running, 0).length : 0)
  const rest = others.filter(Boolean)
  const offset = running ? 1 : 0
  const fitted: string[] = new Array(rest.length).fill('')
  const order = rest.map((text, i) => [number(text, i + offset).length, i] as const).sort((a, b) => a[0] - b[0])
  const described = kept.filter(task => task.description).map(task => ` (${oneLine(task.description)})`)
  const wants = described.reduce((sum, text) => sum + text.length, 0)
  const shareOf = (left: number, n: number) => Math.floor(Math.max(0, left) / n)
  // the descriptions take part as one more line, of the length they want
  const lines = [...order.map(([length, i]) => ({ length, i })), ...(wants ? [{ length: wants, i: -1 }] : [])].sort((a, b) => a.length - b.length)
  let forDescriptions = 0
  lines.forEach(({ length, i }, n) => {
    const share = shareOf(room, lines.length - n)
    if (i === -1) {
      forDescriptions = Math.min(length, share)
      room -= forDescriptions
      return
    }
    fitted[i] = room > 0 ? cut(number(rest[i] as string, i + offset), Math.min(length, share)) : ''
    room -= fitted[i]!.length
  })
  // the running line: every id, each description cut to an even split of the room left for them
  let line = running
  if (running && forDescriptions > 0) {
    let left = forDescriptions
    const pieces = kept.map((task, k) => {
      if (!task.description) return ''
      const others = kept.slice(k).filter(next => next.description).length
      const said = cut(oneLine(task.description), Math.max(0, shareOf(left, others) - 3))
      const piece = said && said !== '…' ? ` (${said})` : ''
      left -= piece.length
      return piece
    })
    line = runningLine(kept, pieces)
  }
  return [head, ...(running ? [number(line, 0)] : []), ...fitted.filter(Boolean)].join('\n')
}

// The line the phone shows after CSR compacts: `CSR compacted at 312k → 66k · pays back in ~14 API calls (~2
// prompts) · kept: 2 running agents, 1 open decision · 2 waiting in Lost and found`. Lost and found's threads stay
// out of the summary (keepOf); waiting counts those that wait on you, so you still know to look.
export function noticeLine(before: number, after: number | null, payback: Payback | null, keep: Keep, waiting = 0): string {
  const shells = keep.running.filter(task => task.type === 'shell').length
  const agents = keep.running.length - shells
  const kept = [
    agents ? `${agents} running ${agents === 1 ? 'agent' : 'agents'}` : '',
    shells ? `${shells} running ${shells === 1 ? 'shell' : 'shells'}` : '',
    keep.decisions.length ? `${keep.decisions.length} open ${keep.decisions.length === 1 ? 'decision' : 'decisions'}` : '',
    keep.work.length ? 'unfinished work' : '',
  ].filter(Boolean)
  return [
    `${NOTICE_HEADER} ${k(before)}${after ? ` → ${k(after)}` : ''}`,
    paybackText(payback),
    kept.length ? `kept: ${kept.join(', ')}` : '',
    waiting > 0 ? `${waiting} waiting in Lost and found` : '',
  ].filter(Boolean).join(' · ')
}

// Fenced code (``` or ~~~, to a closing fence of the same kind at least as long, or to the end when none closes it):
// an example the owner shows, never a rule they set (Codex r2 #10: `# Never run validation in this sample` was one).
export function unfenced(text: string): string {
  const kept: string[] = []
  let fence = ''
  for (const line of text.split('\n')) {
    const mark = /^ {0,3}(`{3,}|~{3,})/.exec(line)?.[1]
    if (fence) {
      if (mark && mark[0] === fence[0] && mark.length >= fence.length && !line.trim().slice(mark.length).trim()) fence = ''
    } else if (mark) fence = mark
    else kept.push(line)
  }
  return kept.join('\n')
}

// A sentence of the owner's that binds: never / always / don't / must, in their own words (dreamer.ts ownWords:
// what they pasted, forwarded and quoted out) and outside code examples. At most 160 characters.
export function ruleIn(text: string): string | null {
  const plain = unfenced(ownWords(text))
  for (const sentence of plain.split(/(?<=[.!?])\s+|\n+/)) {
    const line = sentence.replace(/\s+/g, ' ').trim()
    if (line.length >= 8 && /\b(never|always|don'?t|do not|must|no more)\b/i.test(line)) return cut(line, 160)
  }
  return null
}
