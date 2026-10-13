export type Shift = {
  file: string
  symbol: string | null
  verb: string
  oid: string
  date: string
  subject?: string
}

// Changed since: a past session set against the later commit that changed the code it worked on.
export type Change = {
  conversation: string
  title: string
  current: boolean
  talked: string
  commits: number
  shifts: Shift[]
}

// A "what if...?" the dreamer asked, as the dream log keeps it.
export type DreamEntry = {
  id: string
  at: number
  line: string
  title: string | null
  answer: string | null
}

export type ShiftsData = {
  repo: string | null
  total: number
  shifts: Shift[]
  changes: Change[]
  changes_total: number
  dreams: DreamEntry[]
  dreams_7d: number
  dream_tokens_7d: number
  status: string | null
  reminded_7d: number
  dreams_on?: boolean
  recalled?: number // CSR's recalls for this session
  corrections_carried_7d?: number
  shifts_told_7d?: number
  git?: boolean // false: a folder CSR remembers that git does not know
  sample?: DreamSample | null // while dreams are off: the dream you took up last, from any folder
}

// A dream you marked helpful or handed to Claude: the pane's proof of what dreams do.
export type DreamSample = {
  line: string
  at: number
  answer: 'helpful' | 'ask_claude'
  project: string | null
}

// One thing for the band: a note from CSR's memory about a file (bin/pane.py --note),
// or a dream the dreamer asked.
export type Note = {
  id: string
  kind: 'shift' | 'rule' | 'dream' | 'term'
  file: string
  line: string
  title: string
  text: string
  sketch: string[]
  symbol: string | null
  oid: string | null
  conversation: string
  at: number
  tokens?: number
}

// The band, as You should know draws its own: an offer, the note opened, then
// a one-row "was this useful".
export type NoteView = {
  kind: 'offer' | 'explained' | 'asked'
  note: Note
  tag: 'Heads up' | 'You should know' | 'Dream' | 'Coaching'
  shownAt: number
  promptsSurvived: number
  message?: string
  confirmOff?: boolean // the dim 4 was pressed once: the next press turns dreams off
  carried?: boolean // a dream still open when a compaction came: kept up once, marked so
}

// Compact at the right moment (hooks/compactor.tsx): the band while CSR counts down or offers, and the
// infinite-session row (the plan, and what CSR's compactions did) for the CSR pane.
// armed: what it would compact with (register.tsx's Armed), kept here so a reload of the module can resume it.
export type CompactView = { kind: 'countdown' | 'offer'; ctx: number; why: string; endsAt?: number; payback: { requests: number; perTurn: number | null } | null; armed?: Record<string, unknown> }
export type InfiniteRow = { plan: string | null; story: string | null }

// The first run's walkthrough (hooks/setup.ts): the steps this session shows, those answered here, and whether
// the person typed past it. Session state, so a reload of the module (a setting just written) keeps its place.
export type StepId = 'dreams' | 'autoCompact'
export type SetupView = { steps: StepId[]; done: StepId[]; aside: boolean; message?: string }

// Task id to the subagent that started it, kept across module reloads.
export type TaskOwners = Record<string, string>

// ---------------------------------------------------------------- the agents of this session (hooks/agents.ts)

// JSON.stringify([sessionId, agentId, turnId]): one run of one agent, the identity a completion look is claimed by.
export type CompletionKey = string
// JSON.stringify([sessionId, agentId ?? null, turnId, index]): one model request, the identity its tokens are charged by.
export type StepKey = string

export type CompletionDisposition = 'pending' | 'claimed' | 'looked' | 'skipped' | 'failed'

export type AgentCompletion = {
  key: CompletionKey
  turnId: string
  at: number // epoch milliseconds
  reason: 'answer' | 'aborted' | 'refusal' | 'error'
  answer: string // redacted assistant text; at most 700 Unicode code points
  disposition: CompletionDisposition
}

export type AgentStatusWord = 'pending' | 'running' | 'waiting' | 'idle' | 'completed' | 'failed' | 'killed' | 'unknown'

export type AgentRow = {
  id: string
  parent: string | null
  description: string
  type: string | null
  model: string | null
  status: AgentStatusWord
  started: number | null
  first_seen: number
  last_at: number // actual activity, never a list poll
  tools: number
  tokens: number
  edits: string[]
  edits_omitted: number
  overlaps: string[]
  overlaps_omitted: number
  last: string // assistant text only
  last_tool: string | null // a tool's name only
  current_turn: string | null
  completion: AgentCompletion | null
  listed: boolean
}

export type AgentsFile = {
  version: 1
  session: string
  at: number
  agents: AgentRow[]
  omitted_agents: number
}

// One agent as the session keeps it: its row, and what the row's lists are cut from. `paths` maps a fingerprint of each
// file the agent wrote to the file, so an overlap on the ninth path is still found when the row shows eight.
export type AgentRec = Omit<AgentRow, 'edits' | 'edits_omitted' | 'overlaps' | 'overlaps_omitted'> & {
  paths: Record<string, string>
  seenTools: string[]
  seenSteps: StepKey[]
  first_turn: string | null // the first turn this agent ran that was seen: the one a handoff to it is bound to
}

// A request to hand a Lost and found thread to a background agent, and what became of it. Ids and words only: no prompt, no
// report. Written to handoffs/<session>.json; the pane's draft of it is in handoffs/<session>.drafts.jsonl.
export type AgentHandoff = {
  request: string
  session: string
  thread: string // Lost and found's stable key for the thread
  drafted_at: number
  submitted_at: number | null
  spawn_tool_use_id: string | null
  agent_id: string | null
  turn_id: string | null
  outcome: 'draft' | 'submitted' | 'bound' | 'succeeded' | 'failed'
  ended_at: number | null
  reason: string | null
}

// A spawn seen before the result of `next(e)` is: what the Agent call asked for.
export type PendingSpawn = { at: number; description: string; type: string | null; parent: string | null; model: string | null }

// A completion waiting for its look, until it is claimed (the store's ledger says which; the answer is here).
export type PendingCompletion = { agent: string; turnId: string; at: number; answer: string }

// The agents of one session, in the host's state so a reload of the module keeps them. Nothing here is a transcript.
export type AgentState = {
  version: 1
  session: string
  rows: Record<string, AgentRec>
  spawns: Record<string, PendingSpawn> // by the Agent call's tool_use_id
  bound: Record<string, string> // tool_use_id -> the agent it started
  main: Record<string, string> // fingerprint -> file, what the main loop wrote
  mainOmitted: number
  seen: CompletionKey[] // completions already taken in, newest last
  pending: Record<CompletionKey, PendingCompletion>
  listedAt: number | null
  fileAt: number | null // when the agents file was last written
  fileDue: number | null // when a write that was held back is due (a timer sets it off; a reload sets it again)
  handoffs: AgentHandoff[] // requests to hand a thread to a background agent, oldest first
}

declare module 'claude-code' {
  interface PluginState {
    'csr-companion': { data: ShiftsData | null; view: NoteView | null; compact: CompactView | null; infinite: InfiniteRow | null; setup: SetupView | null; taskOwners: TaskOwners; agentSession: AgentState }
  }
}
