import type {
  AgentCompletion, AgentHandoff, AgentRec, AgentRow, AgentsFile, AgentState, AgentStatusWord, CompletionDisposition, CompletionKey, PendingCompletion, PendingSpawn, StepKey,
} from '../types'

// The agents of a session, as the typed events of Claude Code 2.1.296 tell them: a lifecycle reducer, a reconciler
// for $.agent.list(), and the redaction every text of an agent passes before it is kept. Pure: the hooks in
// register.tsx hand it events and keep the state it returns in the host's atom. Nothing here reads a transcript.
//
// What the events carry (claude-code.d.ts):
//  - agent.spawn: the input has tool_use_id, description, subagentType, parentAgentId and the requested model; what
//    `next(e)` resolves to has the resolved model and, when a subagent started, its agentId (none: nothing started,
//    and no row is made up for it).
//  - tool.call: `agentId` is the loop the call runs in (none on main); the tool's arguments are spread on `e`.
//  - turn.step: the result has turnId, index, answer and usage (null when no response came).
//  - turn.complete: agentId, turnId, answer, reason ('answer' | 'aborted' | 'refusal' | 'error').
//  - $.agent.list(): id, parentId, type, description, status. No model and no start time.

export const completionKey = (session: string, agent: string, turn: string): CompletionKey => JSON.stringify([session, agent, turn])
export const stepKey = (session: string, agent: string | null | undefined, turn: string, index: number): StepKey =>
  JSON.stringify([session, agent ?? null, turn, index])

export const STALL_MS = 10 * 60 * 1000 // a running agent with no event for this long is shown as stalled
export const LIST_EVERY_MS = 60 * 1000 // $.agent.list() while any agent is pending, running or waiting
export const ANSWER_MAX = 700 // code points of an agent's report that are kept
export const LAST_MAX = 240
export const DESC_MAX = 160
export const ROWS_KEPT = 60 // agents the session state keeps; the oldest finished ones go first
export const PATHS_KEPT = 400 // files an agent wrote whose names are kept; past it only the fingerprint is
export const PATHS_MEMBERS = 4000 // fingerprints of an agent's files, the membership an overlap is found by
export const MAIN_KEPT = 4000
const SEEN_TOOLS = 400
const SEEN_STEPS = 400
const SEEN_COMPLETIONS = 400
const SPAWNS_KEPT = 100
const EDIT_TOOLS = new Set(['Edit', 'Write', 'MultiEdit', 'NotebookEdit'])
export const isEditTool = (tool: string) => EDIT_TOOLS.has(tool)

export const emptyAgentState = (session: string): AgentState => ({
  version: 1, session, rows: {}, spawns: {}, bound: {}, main: {}, mainOmitted: 0, seen: [], pending: {}, listedAt: null, fileAt: null, fileDue: null, handoffs: [],
})

// ---------------------------------------------------------------- text that is kept

const codePoints = (text: string) => Array.from(text)
export const cap = (text: string, most: number): string => {
  const points = codePoints(text)
  return points.length <= most ? text : `${points.slice(0, Math.max(0, most - 1)).join('')}…`
}

// Terminal escape sequences and control characters out of any text shown or kept: CSI, OSC (BEL or ST ended), other
// two-character escapes, the C1 range, and the C0 controls but newline and tab.
const TERMINAL = /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?|\x1b[@-Z\\-_]|\x1b.|[\x00-\x08\x0b\x0c\x0e-\x1f\x7f-\x9f]/g
export const stripTerminal = (text: string) => text.replace(TERMINAL, '')

// bin/private.py's redact, pattern for pattern (tests/redaction_table.json is the table both are held to), and the
// two kinds of text the design adds: a KEY/TOKEN/SECRET assignment line and a run of 32 or more hexadecimal digits.
const REDACTED = '[redacted]'
const CRED = '[A-Za-z0-9._~+/=\\-]'
const NAMES = '(?:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret)'
const SECRETS: Array<[RegExp, string]> = [
  [/-----BEGIN [A-Z0-9 ]+-----[\s\S]*?(?:-----END [A-Z0-9 ]+-----|$)/g, REDACTED],
  [/\bsk-[A-Za-z0-9_\-]{16,}/g, REDACTED],
  [/\bxai-[A-Za-z0-9]{16,}/g, REDACTED],
  [/\bgh[pousr]_[A-Za-z0-9]{20,}/g, REDACTED],
  [/\bgithub_pat_[A-Za-z0-9_]{20,}/g, REDACTED],
  [/\b(?:AKIA|ASIA)[A-Z0-9]{16}\b/g, REDACTED],
  [/\bxox[abposr]-[A-Za-z0-9\-]{10,}/g, REDACTED],
  [/\bAIza[0-9A-Za-z_\-]{35}/g, REDACTED],
  [/\b(?:hf|npm)_[A-Za-z0-9]{30,}/g, REDACTED],
  [/\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}/g, REDACTED],
  [new RegExp(`\\b(authorization["']?\\s*[:=]\\s*["']?(?:bearer|basic|token)\\s+)${CRED}{8,}`, 'gi'), `$1${REDACTED}`],
  [new RegExp(`\\b(bearer\\s+)(?=${CRED}*[0-9_~+/=])${CRED}{8,}`, 'gi'), `$1${REDACTED}`],
]
const KEYED = new RegExp(`(\\b[A-Za-z0-9_.\\-]*?${NAMES}["']?\\s*[=:]\\s*)(?!\\[redacted\\])("[^"\\n]*"|'[^'\\n]*'|[^\\s,;&"'\`)\\]}]+)`, 'gi')
const looksSecret = (value: string) => value.length >= 6 && /[0-9@#$%^*!~+=]/.test(value)
const ENV_LINE = /^([ \t]*(?:export[ \t]+)?[A-Z][A-Z0-9_]*(?:KEY|TOKEN|SECRET)[A-Z0-9_]*[ \t]*[=:][ \t]*)(?!['"]?\[redacted\])\S.*$/gm
const HEX_RUN = /\b[0-9a-fA-F]{32,}\b/g

// Credentials out of a text. Applying it twice changes nothing more.
export function redactText(text: string): string {
  if (typeof text !== 'string' || !text) return text
  let out = text
  for (const [pattern, replacement] of SECRETS) out = out.replace(pattern, replacement)
  out = out.replace(KEYED, (_whole, head: string, value: string) => {
    if (value[0] === '"' || value[0] === "'") return `${head}${value[0]}${REDACTED}${value[0]}`
    return `${head}${looksSecret(value) ? REDACTED : value}`
  })
  return out.replace(ENV_LINE, `$1${REDACTED}`).replace(HEX_RUN, REDACTED)
}

// Every string inside JSON-shaped data redacted and cleaned of terminal control; keys and numbers stay.
export function redactAgentValue<T>(value: T): T {
  if (typeof value === 'string') return redactText(stripTerminal(value)) as unknown as T
  if (Array.isArray(value)) return value.map(inner => redactAgentValue(inner)) as unknown as T
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>).map(([key, inner]) => [key, redactAgentValue(inner)])) as T
  }
  return value
}

// One line of the agent's own words for a row: redacted first, then cut (a secret cut in half is no longer matched).
const oneLine = (text: string, most: number) => cap(redactAgentValue(text).replace(/\s+/g, ' ').trim(), most)
const report = (text: string) => cap(redactAgentValue(text).replace(/[ \t]+\n/g, '\n').trim(), ANSWER_MAX)

// ---------------------------------------------------------------- files

// A short fingerprint of a path (cyrb53, base 36): membership of a file in an agent's writes without keeping the file.
export function fingerprint(path: string): string {
  let h1 = 0xdeadbeef
  let h2 = 0x41c6ce57
  for (let i = 0; i < path.length; i++) {
    const code = path.charCodeAt(i)
    h1 = Math.imul(h1 ^ code, 2654435761)
    h2 = Math.imul(h2 ^ code, 1597334677)
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909)
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909)
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36)
}

// An absolute path with . and .. resolved; a relative one is taken from `cwd`.
export function normalizePath(path: string, cwd: string): string {
  const joined = path.startsWith('/') ? path : `${cwd.replace(/\/+$/, '')}/${path}`
  const parts: string[] = []
  for (const part of joined.split('/')) {
    if (part === '..') parts.pop()
    else if (part && part !== '.') parts.push(part)
  }
  return `/${parts.join('/')}`
}

// ---------------------------------------------------------------- events

export type AgentEvent =
  // The Agent call, before `next(e)` answers: what it asked for.
  // `requests`: the handoff requests (envelopes) the Agent call's prompt carries.
  | { kind: 'spawn'; at: number; toolUseId: string; description: string; type: string | null; parent: string | null; model: string | null; requests?: readonly string[] }
  // What `next(e)` answered: the resolved model and the agent started. No agentId: it started none (denied).
  | { kind: 'spawned'; at: number; toolUseId: string; agentId?: string; model?: string }
  // A tool began in an agent's loop: it is alive, whatever it comes to.
  | { kind: 'touch'; at: number; agentId: string; tool: string }
  // A tool finished. `agentId` absent: the main loop's (only its writes are kept). `ok`: it ran and did not fail.
  | { kind: 'tool'; at: number; agentId?: string; toolUseId: string; tool: string; ok: boolean; path?: string | null }
  | { kind: 'step'; at: number; agentId: string; turnId: string; index: number; answer: string; tokens: number }
  | { kind: 'complete'; at: number; agentId: string; turnId: string; reason: AgentCompletion['reason']; answer: string }
  // A prompt the person's Enter sent carried the handoff envelope of a request the pane drafted.
  | { kind: 'handoff'; at: number; request: string; thread: string; draftedAt: number }

const ACTIVE = new Set<AgentStatusWord>(['pending', 'running', 'waiting'])
const TERMINAL_STATUS = new Set<AgentStatusWord>(['completed', 'failed', 'killed'])
export const isActive = (status: AgentStatusWord) => ACTIVE.has(status)
export const isTerminal = (status: AgentStatusWord) => TERMINAL_STATUS.has(status)

// Stalled is a display flag, never a status: a running agent no event has touched for STALL_MS. Waiting and idle
// are not stalled, and a list poll never refreshes last_at.
export const isStalled = (row: Pick<AgentRec, 'status' | 'last_at'>, now: number) => row.status === 'running' && now - row.last_at >= STALL_MS

const provisional = (id: string, at: number, status: AgentStatusWord = 'running'): AgentRec => ({
  id, parent: null, description: '', type: null, model: null, status, started: null, first_seen: at, last_at: at, tools: 0, tokens: 0,
  last: '', last_tool: null, current_turn: null, completion: null, listed: false, paths: {}, seenTools: [], seenSteps: [], first_turn: null,
})

const push = <T>(list: readonly T[], item: T, most: number): T[] => [...list, item].slice(-most)

// The rows the state keeps: the newest by last activity; a row with a completion waiting is never the one to go.
function trimRows(state: AgentState): AgentState {
  const ids = Object.keys(state.rows)
  if (ids.length <= ROWS_KEPT) return state
  const waiting = new Set(Object.values(state.pending).map(entry => entry.agent))
  const order = ids.filter(id => !waiting.has(id)).sort((a, b) => (state.rows[a]!.last_at - state.rows[b]!.last_at))
  const drop = new Set(order.slice(0, ids.length - ROWS_KEPT))
  const rows = Object.fromEntries(Object.entries(state.rows).filter(([id]) => !drop.has(id)))
  return { ...state, rows }
}

const withRow = (state: AgentState, row: AgentRec): AgentState => trimRows({ ...state, rows: { ...state.rows, [row.id]: row } })

// One event into the state. The state is never changed in place.
export function reduceAgentEvent(state: AgentState, event: AgentEvent): AgentState {
  switch (event.kind) {
    case 'spawn': {
      const spawn: PendingSpawn = {
        at: event.at, description: oneLine(event.description, DESC_MAX), type: event.type, parent: event.parent, model: event.model,
      }
      const kept = Object.entries({ ...state.spawns, [event.toolUseId]: spawn }).slice(-SPAWNS_KEPT)
      const handoffs = event.requests?.length
        ? associateHandoff(state.handoffs, { kind: 'spawn', at: event.at, requests: event.requests, toolUseId: event.toolUseId, nested: event.parent !== null })
        : state.handoffs
      return { ...state, spawns: Object.fromEntries(kept), handoffs }
    }
    case 'spawned': {
      const { [event.toolUseId]: asked, ...spawns } = state.spawns
      if (!event.agentId) { // denied or none started: nothing is made up
        return { ...state, spawns, handoffs: associateHandoff(state.handoffs, { kind: 'spawned', at: event.at, toolUseId: event.toolUseId, turn: null }) }
      }
      const row = state.rows[event.agentId] ?? provisional(event.agentId, asked?.at ?? event.at)
      const merged: AgentRec = {
        ...row,
        description: asked?.description || row.description,
        type: asked?.type ?? row.type,
        parent: row.parent ?? asked?.parent ?? null,
        model: event.model ?? asked?.model ?? row.model,
        started: row.started ?? asked?.at ?? event.at,
        first_seen: Math.min(row.first_seen, asked?.at ?? row.first_seen),
        status: row.status === 'unknown' || row.status === 'pending' ? 'running' : row.status,
      }
      // A handoff this Agent call carried is bound to the agent it started and its first turn (events that came before this
      // result included); a report that already came is its outcome.
      let handoffs = associateHandoff(state.handoffs, { kind: 'spawned', at: event.at, toolUseId: event.toolUseId, agentId: event.agentId, turn: merged.first_turn })
      if (merged.completion && merged.completion.turnId === merged.first_turn) {
        handoffs = associateHandoff(handoffs, {
          kind: 'ended', at: merged.completion.at, agentId: merged.id, turnId: merged.completion.turnId, reason: merged.completion.reason,
          empty: merged.completion.disposition === 'skipped' && merged.completion.reason === 'answer',
        })
      }
      return withRow({ ...state, spawns, handoffs, bound: { ...state.bound, [event.toolUseId]: event.agentId } }, merged)
    }
    case 'touch': {
      const row = state.rows[event.agentId] ?? provisional(event.agentId, event.at)
      const status = row.status === 'unknown' || row.status === 'pending' ? 'running' : row.status
      return withRow(state, { ...row, status, last_at: Math.max(row.last_at, event.at), last_tool: cap(event.tool, 60) })
    }
    case 'tool': {
      const wrote = event.ok && isEditTool(event.tool) && event.path ? event.path : null
      if (event.agentId === undefined) {
        // The main loop: only what it wrote matters here, for the files an agent also wrote.
        if (!wrote) return state
        const print = fingerprint(wrote)
        if (print in state.main) return state
        if (Object.keys(state.main).length >= MAIN_KEPT) return { ...state, mainOmitted: state.mainOmitted + 1 }
        return { ...state, main: { ...state.main, [print]: wrote } }
      }
      const row = state.rows[event.agentId] ?? provisional(event.agentId, event.at)
      if (row.seenTools.includes(event.toolUseId)) return state
      let paths = row.paths
      if (wrote) {
        const print = fingerprint(wrote)
        if (!(print in paths) && Object.keys(paths).length < PATHS_MEMBERS) {
          paths = { ...paths, [print]: Object.keys(paths).length < PATHS_KEPT ? wrote : '' }
        }
      }
      return withRow(state, {
        ...row, tools: row.tools + 1, last_tool: cap(event.tool, 60), last_at: Math.max(row.last_at, event.at), paths,
        seenTools: push(row.seenTools, event.toolUseId, SEEN_TOOLS),
        status: row.status === 'unknown' || row.status === 'pending' ? 'running' : row.status,
      })
    }
    case 'step': {
      const key = stepKey(state.session, event.agentId, event.turnId, event.index)
      const row = state.rows[event.agentId] ?? provisional(event.agentId, event.at)
      if (row.seenSteps.includes(key)) return state
      const newRun = row.current_turn !== null && row.current_turn !== event.turnId
      const text = oneLine(event.answer, LAST_MAX)
      const first = row.first_turn ?? event.turnId
      const handoffs = row.first_turn === null ? associateHandoff(state.handoffs, { kind: 'turn', at: event.at, agentId: event.agentId, turnId: event.turnId }) : state.handoffs
      return withRow(handoffs === state.handoffs ? state : { ...state, handoffs }, {
        ...row, tokens: row.tokens + Math.max(0, event.tokens), last: text || row.last, last_at: Math.max(row.last_at, event.at),
        current_turn: event.turnId, first_turn: first, seenSteps: push(row.seenSteps, key, SEEN_STEPS),
        // A step is running work: an agent that was idle, waiting or ended before this turn runs again, with no report yet.
        status: row.status === 'running' ? 'running' : isTerminal(row.status) && !newRun && row.current_turn === event.turnId ? row.status : 'running',
        completion: newRun ? null : row.completion,
      })
    }
    case 'complete': {
      const key = completionKey(state.session, event.agentId, event.turnId)
      if (state.seen.includes(key)) return state
      const row = state.rows[event.agentId] ?? provisional(event.agentId, event.at)
      const answer = event.reason === 'answer' ? report(event.answer) : ''
      const eligible = event.reason === 'answer' && answer.trim() !== ''
      const completion: AgentCompletion = {
        key, turnId: event.turnId, at: event.at, reason: event.reason, answer, disposition: eligible ? 'pending' : 'skipped',
      }
      const status: AgentStatusWord = event.reason === 'answer' ? 'completed' : event.reason === 'aborted' ? 'killed' : 'failed'
      const pending: Record<CompletionKey, PendingCompletion> = eligible
        ? { ...state.pending, [key]: { agent: event.agentId, turnId: event.turnId, at: event.at, answer } }
        : state.pending
      // The run's end settles a handoff bound to this agent and turn: a report is success; an error, a refusal, an interrupt or
      // no words are not.
      const handoffs = associateHandoff(state.handoffs, {
        kind: 'ended', at: event.at, agentId: event.agentId, turnId: event.turnId, reason: event.reason, empty: !eligible && event.reason === 'answer',
      })
      return withRow(
        { ...state, pending, handoffs, seen: push(state.seen, key, SEEN_COMPLETIONS) },
        { ...row, status, completion, current_turn: event.turnId, first_turn: row.first_turn ?? event.turnId, last_at: Math.max(row.last_at, event.at) },
      )
    }
    case 'handoff': {
      return { ...state, handoffs: associateHandoff(state.handoffs, { kind: 'submitted', at: event.at, session: state.session, request: event.request, thread: event.thread, draftedAt: event.draftedAt }) }
    }
  }
}

// ---------------------------------------------------------------- $.agent.list()

export type Listed = { id: string; parentId?: string; type?: string; description?: string; status?: string }

const WORDS: ReadonlySet<string> = new Set(['pending', 'running', 'waiting', 'idle', 'completed', 'failed', 'killed'])
const statusOf = (word: string | undefined): AgentStatusWord => (word && WORDS.has(word) ? (word as AgentStatusWord) : 'unknown')

// The list brought into the state. A row the list no longer has is not finished, failed or killed: it keeps its last
// state, marked not listed. A row that ended in an event keeps that unless the list says it runs again (it resumed
// under the same id). The list gives no model, no start and no activity: last_at is never moved by it.
export function reconcileAgentList(state: AgentState, list: readonly Listed[], at: number): AgentState {
  const ids = new Set(list.map(entry => entry.id))
  const rows: Record<string, AgentRec> = {}
  for (const [id, row] of Object.entries(state.rows)) rows[id] = ids.has(id) ? row : row.listed ? { ...row, listed: false } : row
  for (const entry of list) {
    const row = rows[entry.id] ?? provisional(entry.id, at, statusOf(entry.status))
    const listed = statusOf(entry.status)
    const keep = isTerminal(row.status) && !isActive(listed)
    rows[entry.id] = {
      ...row,
      listed: true,
      parent: row.parent ?? entry.parentId ?? null,
      type: row.type ?? (entry.type || null),
      description: row.description || oneLine(entry.description ?? '', DESC_MAX),
      status: keep ? row.status : listed,
      // A resumed agent runs again with no report of its own yet; the earlier one stays in the ledger.
      completion: !keep && isActive(listed) && row.completion && isTerminal(row.status) ? null : row.completion,
    }
  }
  // An agent the list says failed or was killed, bound to a handoff that has no outcome yet: that is its outcome. A row that is
  // merely gone from the list, or quiet, is not.
  let handoffs = state.handoffs
  for (const entry of list) {
    const word = statusOf(entry.status)
    if (word === 'failed' || word === 'killed') handoffs = associateHandoff(handoffs, { kind: 'ended', at, agentId: entry.id, turnId: null, reason: word, empty: false })
  }
  return trimRows({ ...state, rows, listedAt: at, handoffs })
}

// Whether any agent is one the list should be asked about again.
export const anyActive = (state: AgentState) => Object.values(state.rows).some(row => isActive(row.status))

// ---------------------------------------------------------------- completions: pending, claimed, consumed

// The ledger of completion identities, kept in the store under the session (identifiers and dispositions only: no
// report, no path, no prompt). It, not the 12-row file and not the state's own list, says what was taken, and a
// completion a look claimed is never offered again. 'pending' may be scheduled; every other word is final but 'claimed',
// which a look turns into looked, skipped or failed when it ends (a reload in between leaves it claimed: consumed).
export type Ledger = Record<CompletionKey, CompletionDisposition>
export const LEDGER_MAX = 1500

// A completion enters the ledger once. At the limit nothing is evicted to make room (a consumed identity forgotten is a
// completion looked at twice): the entry is refused, and the caller suppresses completion looks.
export function admitCompletion(ledger: Ledger, key: CompletionKey): { ledger: Ledger; fresh: boolean; full: boolean } {
  if (key in ledger) return { ledger, fresh: false, full: false }
  if (Object.keys(ledger).length >= LEDGER_MAX) return { ledger, fresh: false, full: true }
  return { ledger: { ...ledger, [key]: 'pending' }, fresh: true, full: false }
}

export type PendingEntry = PendingCompletion & { key: CompletionKey }

// The completions a look may still take, oldest first: pending in the state, and pending in the ledger too.
export function pendingCompletions(state: AgentState, ledger: Ledger): PendingEntry[] {
  return Object.entries(state.pending)
    .filter(([key]) => ledger[key] === 'pending')
    .map(([key, entry]) => ({ key, ...entry }))
    .sort((a, b) => a.at - b.at)
}

// The pending ones among `keys` become claimed, all at once; the others (taken already, unknown) are not.
export function claimCompletions(ledger: Ledger, keys: readonly CompletionKey[]): { ledger: Ledger; claimed: CompletionKey[] } {
  const claimed = keys.filter(key => ledger[key] === 'pending')
  if (!claimed.length) return { ledger, claimed }
  return { ledger: { ...ledger, ...Object.fromEntries(claimed.map(key => [key, 'claimed' as const])) }, claimed }
}

// A claimed look ended: its completions looked, skipped or failed. Only a claimed one moves.
export function settleCompletions(ledger: Ledger, keys: readonly CompletionKey[], disposition: 'looked' | 'skipped' | 'failed'): Ledger {
  const moved = keys.filter(key => ledger[key] === 'claimed')
  return moved.length ? { ...ledger, ...Object.fromEntries(moved.map(key => [key, disposition])) } : ledger
}

// The state told what became of those completions: they are no longer pending, and their rows say so.
export function markDisposition(state: AgentState, keys: readonly CompletionKey[], disposition: CompletionDisposition): AgentState {
  const gone = new Set(keys)
  const pending = Object.fromEntries(Object.entries(state.pending).filter(([key]) => !gone.has(key) || disposition === 'pending'))
  const rows = Object.fromEntries(Object.entries(state.rows).map(([id, row]) =>
    [id, row.completion && gone.has(row.completion.key) ? { ...row, completion: { ...row.completion, disposition } } : row]))
  return { ...state, pending, rows }
}

// ---------------------------------------------------------------- the agents file

// <HOME>/.claude-self-reflect/csr-companion/agents/<session>.json is what the pane reads: a bounded projection, not the
// lifecycle's truth (the state is). It is valid JSON of at most FILE_BYTES UTF-8 bytes and FILE_ROWS agents, written at
// most once in FILE_EVERY_MS through bin/private.py (0600, credentials redacted again as JSON strings).
export const FILE_BYTES = 16384
export const FILE_ROWS = 12
export const FILE_EVERY_MS = 5000
export const EDITS_SHOWN = 8
const ID = /^[A-Za-z0-9_.:@-]{1,128}$/
export const safeId = (id: string) => ID.test(id)

export function utf8Length(text: string): number {
  let n = 0
  for (const char of text) {
    const code = char.codePointAt(0) as number
    n += code < 0x80 ? 1 : code < 0x800 ? 2 : code < 0x10000 ? 3 : 4
  }
  return n
}

const clean = (text: string | null, most: number): string => cap(redactAgentValue(text ?? '').replace(/\s+/g, ' ').trim(), most)
const cleanOrNull = (text: string | null, most: number): string | null => (text ? clean(text, most) || null : null)

// A record as the file's row: its lists cut from what it wrote (the files the main loop also wrote first), every
// free text redacted first and cut after.
function rowOf(rec: AgentRec, main: Record<string, string>): AgentRow {
  const prints = Object.keys(rec.paths)
  const overlapping = prints.filter(print => print in main)
  const named = (list: string[]) => list.map(print => rec.paths[print] as string).filter(path => path !== '').map(path => clean(path, 300)).sort()
  const overlaps = named(overlapping)
  const rest = named(prints.filter(print => !(print in main)))
  const edits = [...overlaps, ...rest].slice(0, EDITS_SHOWN)
  const shownOverlaps = overlaps.slice(0, EDITS_SHOWN)
  const { paths: _paths, seenTools: _tools, seenSteps: _steps, first_turn: _first, ...row } = rec
  return {
    ...row,
    id: rec.id,
    parent: rec.parent && safeId(rec.parent) ? rec.parent : null,
    description: clean(rec.description, DESC_MAX),
    type: cleanOrNull(rec.type, 60),
    model: cleanOrNull(rec.model, 80),
    last: clean(rec.last, LAST_MAX),
    last_tool: cleanOrNull(rec.last_tool, 60),
    current_turn: rec.current_turn && safeId(rec.current_turn) ? rec.current_turn : null,
    completion: rec.completion ? { ...rec.completion, answer: cap(redactAgentValue(rec.completion.answer), ANSWER_MAX) } : null,
    edits,
    edits_omitted: prints.length - edits.length,
    overlaps: shownOverlaps,
    overlaps_omitted: overlapping.length - shownOverlaps.length,
  }
}

// Which agents the file shows: the running ones first, then the newest finished, parents with their children. A parent
// the file cannot spare a place for is not left out when a child shown needs it (a minimal row stands in for it).
const rank = (rec: AgentRec) => [isActive(rec.status) ? 0 : 1, -rec.last_at] as const
function selection(state: AgentState): { chosen: AgentRec[]; omitted: number; minimal: Set<string> } {
  const all = Object.values(state.rows).filter(rec => safeId(rec.id))
  const byId = new Map(all.map(rec => [rec.id, rec]))
  const ordered = [...all].sort((a, b) => rank(a)[0] - rank(b)[0] || rank(a)[1] - rank(b)[1] || (a.id < b.id ? -1 : 1))
  const taken = new Set<string>()
  for (const rec of ordered) {
    if (taken.has(rec.id)) continue
    const need: string[] = []
    const seen = new Set<string>()
    for (let at: AgentRec | undefined = rec; at && !seen.has(at.id); at = at.parent ? byId.get(at.parent) : undefined) {
      seen.add(at.id)
      if (!taken.has(at.id)) need.push(at.id)
    }
    if (taken.size + need.length > FILE_ROWS) continue
    for (const id of need) taken.add(id)
  }
  // Those the order itself would show are full rows; an ancestor taken only for the child below it is a stand-in.
  const own = new Set(ordered.slice(0, FILE_ROWS).map(rec => rec.id))
  const minimal = new Set([...taken].filter(id => !own.has(id)))
  const chosen = ordered.filter(rec => taken.has(rec.id))
  return { chosen, omitted: all.length - chosen.length, minimal }
}

// Parents before children, each agent once; the order within a level is the selection's.
function parentsFirst(rows: AgentRow[]): AgentRow[] {
  const ids = new Set(rows.map(row => row.id))
  const out: AgentRow[] = []
  const done = new Set<string>()
  const visit = (row: AgentRow, path: Set<string>) => {
    if (done.has(row.id) || path.has(row.id)) return
    const parent = row.parent && ids.has(row.parent) ? rows.find(other => other.id === row.parent) : undefined
    if (parent) visit(parent, new Set([...path, row.id]))
    if (!done.has(row.id)) {
      done.add(row.id)
      out.push(row)
    }
  }
  for (const row of rows) visit(row, new Set())
  return out
}

// What may go from a row to make the file fit, a step at a time, the least useful first. Each step cuts a field and
// says how many paths it took out; none cuts serialized JSON.
type Squeeze = (row: AgentRow) => AgentRow
const cutLists = (most: number): Squeeze => row => ({
  ...row,
  edits: row.edits.slice(0, most), edits_omitted: row.edits_omitted + Math.max(0, row.edits.length - most),
  overlaps: row.overlaps.slice(0, most), overlaps_omitted: row.overlaps_omitted + Math.max(0, row.overlaps.length - most),
})
const SQUEEZES: Squeeze[] = [
  row => ({ ...row, last: cap(row.last, 120) }),
  row => (row.completion ? { ...row, completion: { ...row.completion, answer: cap(row.completion.answer, 400) } } : row),
  row => ({ ...row, description: cap(row.description, 80), last: cap(row.last, 60) }),
  cutLists(4),
  row => (row.completion ? { ...row, completion: { ...row.completion, answer: cap(row.completion.answer, 160) } } : row),
  row => ({ ...row, last: '', description: cap(row.description, 40) }),
  cutLists(1),
  row => (row.completion ? { ...row, completion: { ...row.completion, answer: cap(row.completion.answer, 40) } } : row),
  cutLists(0),
  row => (row.completion ? { ...row, completion: { ...row.completion, answer: '' } } : row),
]

export function projectAgentsFile(state: AgentState, now: number): AgentsFile {
  const { chosen, omitted, minimal } = selection(state)
  let rows = parentsFirst(chosen.map(rec => rowOf(rec, state.main)))
  // A stand-in ancestor carries its place in the tree and nothing more.
  rows = rows.map(row => (minimal.has(row.id) ? cutLists(0)({ ...row, last: '', completion: row.completion && { ...row.completion, answer: '' } }) : row))
  const file = (list: AgentRow[], gone: number): AgentsFile => ({ version: 1, session: state.session, at: now, agents: list, omitted_agents: gone })
  let gone = omitted
  const size = () => utf8Length(JSON.stringify(file(rows, gone)))
  for (const squeeze of SQUEEZES) {
    if (size() <= FILE_BYTES) break
    rows = rows.map(squeeze)
  }
  // Still too big: the rows farthest down the order go, finished ones first, a parent only after its children.
  while (rows.length > 1 && size() > FILE_BYTES) {
    const parents = new Set(rows.map(row => row.parent))
    const candidates = rows.filter(row => !parents.has(row.id))
    const victim = [...candidates].sort((a, b) => Number(isActive(a.status)) - Number(isActive(b.status)) || a.last_at - b.last_at)[0] as AgentRow
    rows = rows.filter(row => row !== victim)
    gone += 1
  }
  return file(rows, gone)
}

// Whether the file is to be written now, later or not at all, and the state with that decided: at most one write in
// FILE_EVERY_MS, the updates in between coalesced into one write when the interval is up. `fileDue` in the past is a timer
// that never fired (a reload cancelled it): it counts for nothing.
export type WritePlan = { act: 'now' | 'later' | 'none'; wait: number; state: AgentState }
export function planWrite(state: AgentState, now: number): WritePlan {
  if (!Object.keys(state.rows).length) return { act: 'none', wait: 0, state }
  if (state.fileDue !== null && state.fileDue > now) return { act: 'none', wait: 0, state }
  const since = state.fileAt === null ? Infinity : now - state.fileAt
  if (since >= FILE_EVERY_MS || since < 0) return { act: 'now', wait: 0, state: { ...state, fileAt: now, fileDue: null } }
  const due = (state.fileAt as number) + FILE_EVERY_MS
  return { act: 'later', wait: due - now, state: { ...state, fileDue: due } }
}

// ---------------------------------------------------------------- the machine envelope (bin/agents.py formats it)

// [[CSR:AGENTS:v1]] / one compact JSON line / [[/CSR:AGENTS:v1]]: what the brief carries of the agents, and what a
// handoff carries into a background agent's prompt. Python writes it; this is the reader, and the one thing that takes
// it out of a prompt before the words that remain are read as the person's. The rules are bin/agents.py's, and the
// fixtures (tests/fixtures/emissions/agents-v1.json) hold the two to each other.
export const ENVELOPE_OPEN = '[[CSR:AGENTS:v1]]'
export const ENVELOPE_CLOSE = '[[/CSR:AGENTS:v1]]'
export const ENVELOPE_MAX = 16384
export const HANDOFF_INSTRUCTION = "Copy this envelope unchanged into the background agent's prompt."
const STATUS_WORDS = new Set(['pending', 'running', 'waiting', 'idle', 'completed', 'failed', 'killed', 'unknown'])
const REASON_WORDS = new Set(['answer', 'aborted', 'refusal', 'error'])

export type EnvelopeAgent = {
  id: string; parent: string | null; type: string | null; model: string | null; status: string
  edits: string[]; overlaps: string[]; completion: { turn_id: string; reason: string; answer: string } | null
}
export type ContextPayload = { kind: 'context'; session: string; at: number; agents: EnvelopeAgent[] }
export type HandoffPayload = { kind: 'handoff'; session: string; request: string; thread: string; instruction: string }
export type Envelope = { start: number; end: number; payload: ContextPayload | HandoffPayload }

const isObject = (value: unknown): value is Record<string, unknown> => !!value && typeof value === 'object' && !Array.isArray(value)
const sameKeys = (value: Record<string, unknown>, keys: readonly string[]) => {
  const have = Object.keys(value)
  return have.length === keys.length && keys.every(key => key in value)
}
const text = (value: unknown, most: number): value is string =>
  typeof value === 'string' && Array.from(value).length <= most && !/[\x00-\x08\x0b-\x1f\x7f-\x9f]/.test(value)
const num = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value) && Math.abs(value) < 1e18
const listOf = (value: unknown, most: number, each: (item: unknown) => boolean) => Array.isArray(value) && value.length <= most && value.every(each)

function validContext(payload: Record<string, unknown>): payload is ContextPayload {
  if (!sameKeys(payload, ['kind', 'session', 'at', 'agents']) || payload.kind !== 'context') return false
  if (typeof payload.session !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(payload.session) || !num(payload.at)) return false
  return listOf(payload.agents, 12, row => {
    if (!isObject(row) || !sameKeys(row, ['id', 'parent', 'type', 'model', 'status', 'edits', 'overlaps', 'completion'])) return false
    if (typeof row.id !== 'string' || !safeId(row.id) || !(row.parent === null || (typeof row.parent === 'string' && safeId(row.parent)))) return false
    if (typeof row.status !== 'string' || !STATUS_WORDS.has(row.status)) return false
    if (!(row.type === null || text(row.type, 300)) || !(row.model === null || text(row.model, 300))) return false
    if (!listOf(row.edits, 8, path => text(path, 300)) || !listOf(row.overlaps, 8, path => text(path, 300))) return false
    const done = row.completion
    if (done === null) return true
    return isObject(done) && sameKeys(done, ['turn_id', 'reason', 'answer']) && typeof done.turn_id === 'string' && safeId(done.turn_id) &&
      typeof done.reason === 'string' && REASON_WORDS.has(done.reason) && text(done.answer, ANSWER_MAX)
  })
}

function validHandoff(payload: Record<string, unknown>): payload is HandoffPayload {
  return sameKeys(payload, ['kind', 'session', 'request', 'thread', 'instruction']) && payload.kind === 'handoff' &&
    typeof payload.session === 'string' && /^[A-Za-z0-9_-]{1,128}$/.test(payload.session) &&
    typeof payload.request === 'string' && /^[A-Za-z0-9_.:-]{1,128}$/.test(payload.request) &&
    typeof payload.thread === 'string' && /^[0-9a-f]{12}$/.test(payload.thread) && payload.instruction === HANDOFF_INSTRUCTION
}

// Every complete valid envelope in a text, offsets in UTF-16 code units; `end` is past the closing line and its newline.
export function parseAgentEnvelopes(source: string): Envelope[] {
  if (!source.includes(ENVELOPE_OPEN)) return []
  const lines = source.split('\n')
  const starts: number[] = []
  let at = 0
  for (const line of lines) {
    starts.push(at)
    at += line.length + 1
  }
  const found: Envelope[] = []
  const unr = (line: string) => (line.endsWith('\r') ? line.slice(0, -1) : line)
  for (let i = 0; i + 2 < lines.length; i++) {
    let head = unr(lines[i] as string)
    if (i === 0 && head.startsWith('﻿')) head = head.slice(1)
    const middle = unr(lines[i + 1] as string)
    const tail = unr(lines[i + 2] as string)
    if (head !== ENVELOPE_OPEN || tail !== ENVELOPE_CLOSE || middle.includes('\r')) continue
    const last = i + 2 === lines.length - 1
    const end = last ? source.length : (starts[i + 2] as number) + (lines[i + 2] as string).length + 1
    // The bytes the text spends on it, line endings included (a CRLF envelope is longer than its LF twin); a closing line
    // that stops the text counts its newline, as the formatter writes it.
    const span = source.slice(starts[i] as number, end)
    if (utf8Length(span) + (span.endsWith('\n') ? 0 : 1) > ENVELOPE_MAX) continue
    let payload: unknown
    try {
      payload = JSON.parse(middle)
    } catch {
      continue
    }
    if (!isObject(payload) || !(validContext(payload) || validHandoff(payload))) continue
    found.push({ start: starts[i] as number, end, payload: payload as ContextPayload | HandoffPayload })
    i += 2
  }
  return found
}

// The first complete valid envelope of a text, or null.
export const parseAgentEnvelope = (source: string): Envelope | null => parseAgentEnvelopes(source)[0] ?? null

// The text without its complete valid envelopes and their final newline; every other character as it was.
export function stripEnvelopes(source: string): string {
  const found = parseAgentEnvelopes(source)
  if (!found.length) return source
  let out = ''
  let from = 0
  for (const envelope of found) {
    out += source.slice(from, envelope.start)
    from = envelope.end
  }
  return out + source.slice(from)
}

// The finished runs a context envelope reports, by identity: each key is [session, agent, turn] as completionKey makes
// it, here only the ones the ledger has claimed. These, and no elapsed-time text, are what a completion's look is news for.
export function completionNews(envelope: Envelope | null, ledger: Ledger, wanted: readonly CompletionKey[]): CompletionKey[] {
  if (!envelope || envelope.payload.kind !== 'context') return []
  const { session, agents } = envelope.payload
  return agents.flatMap(row =>
    row.completion && row.completion.reason === 'answer' && row.completion.answer.trim() !== ''
      ? [completionKey(session, row.id, row.completion.turn_id)] : [])
    .filter(key => wanted.includes(key) && ledger[key] === 'claimed')
}

// ---------------------------------------------------------------- handing a Lost and found thread to a background agent

// The pane drafts a request (bin/pane.py, a click on "→ subagent"): an id, and the thread, in an envelope appended to the
// unsent prompt. What follows is only ever this, by identity and never by likeness of words or "the next agent spawned":
//  1. the person sends a prompt carrying the envelope -> submitted
//  2. Claude's own Agent call carries the envelope in its prompt -> the call is that request's
//  3. the call started an agent -> bound to its id and first turn; denied, or the envelope in two requests' company -> not linked
//  4. that turn ends -> succeeded with a report, else failed (an error, a refusal, an interrupt, no words, killed)
// A nested call (an agent's own, parentAgentId) carrying the same envelope binds nothing. A retry is a new request: an
// earlier failure never reaches it.
export type HandoffEvent =
  | { kind: 'submitted'; at: number; session: string; request: string; thread: string; draftedAt: number }
  | { kind: 'spawn'; at: number; requests: readonly string[]; toolUseId: string; nested: boolean }
  | { kind: 'spawned'; at: number; toolUseId: string; agentId?: string; turn: string | null }
  | { kind: 'turn'; at: number; agentId: string; turnId: string }
  | { kind: 'ended'; at: number; agentId: string; turnId: string | null; reason: string; empty: boolean }

export const HANDOFFS_KEPT = 40
const replaceAt = <T>(list: readonly T[], index: number, item: T): T[] => list.map((entry, at) => (at === index ? item : entry))

// The handoffs with one event applied; the very array when the event changes nothing.
export function associateHandoff(handoffs: AgentHandoff[], event: HandoffEvent): AgentHandoff[] {
  switch (event.kind) {
    case 'submitted': {
      const at = handoffs.findIndex(entry => entry.request === event.request)
      if (at >= 0) {
        const have = handoffs[at] as AgentHandoff
        if (have.submitted_at !== null) return handoffs
        return replaceAt(handoffs, at, { ...have, submitted_at: event.at, outcome: have.outcome === 'draft' ? 'submitted' : have.outcome })
      }
      const made: AgentHandoff = {
        request: event.request, session: event.session, thread: event.thread, drafted_at: event.draftedAt, submitted_at: event.at,
        spawn_tool_use_id: null, agent_id: null, turn_id: null, outcome: 'submitted', ended_at: null, reason: null,
      }
      return [...handoffs, made].slice(-HANDOFFS_KEPT)
    }
    case 'spawn': {
      if (event.nested) return handoffs
      const wanted = [...new Set(event.requests)].filter(request => handoffs.some(entry => entry.request === request && entry.outcome === 'submitted' && entry.spawn_tool_use_id === null))
      if (!wanted.length) return handoffs
      // Two requests in one prompt: which agent is whose cannot be told. Neither is linked.
      if (wanted.length > 1) {
        return handoffs.map(entry => (wanted.includes(entry.request) && entry.outcome === 'submitted' ? { ...entry, reason: 'ambiguous' } : entry))
      }
      const at = handoffs.findIndex(entry => entry.request === wanted[0])
      return replaceAt(handoffs, at, { ...(handoffs[at] as AgentHandoff), spawn_tool_use_id: event.toolUseId, reason: null })
    }
    case 'spawned': {
      const at = handoffs.findIndex(entry => entry.spawn_tool_use_id === event.toolUseId && entry.outcome === 'submitted')
      if (at < 0) return handoffs
      const have = handoffs[at] as AgentHandoff
      if (!event.agentId) return replaceAt(handoffs, at, { ...have, reason: 'spawn denied' })
      return replaceAt(handoffs, at, { ...have, outcome: 'bound', agent_id: event.agentId, turn_id: event.turn, reason: null })
    }
    case 'turn': {
      const at = handoffs.findIndex(entry => entry.outcome === 'bound' && entry.agent_id === event.agentId && entry.turn_id === null)
      return at < 0 ? handoffs : replaceAt(handoffs, at, { ...(handoffs[at] as AgentHandoff), turn_id: event.turnId })
    }
    case 'ended': {
      // Only the run the handoff was bound to: a later run of the same agent is not its outcome, nor an earlier one.
      const at = handoffs.findIndex(entry => entry.outcome === 'bound' && entry.agent_id === event.agentId && (entry.turn_id === null || event.turnId === null || entry.turn_id === event.turnId))
      if (at < 0) return handoffs
      const have = handoffs[at] as AgentHandoff
      const ok = event.reason === 'answer' && !event.empty
      return replaceAt(handoffs, at, {
        ...have, turn_id: have.turn_id ?? event.turnId, ended_at: event.at, outcome: ok ? 'succeeded' : 'failed',
        reason: ok ? null : event.reason === 'answer' ? 'no report' : event.reason,
      })
    }
  }
}

// The requests (handoff envelopes) a text carries for this session, in order.
export function handoffRequests(source: string, session: string): Array<{ request: string; thread: string }> {
  return parseAgentEnvelopes(source).flatMap(envelope =>
    envelope.payload.kind === 'handoff' && envelope.payload.session === session
      ? [{ request: envelope.payload.request, thread: envelope.payload.thread }] : [])
}
