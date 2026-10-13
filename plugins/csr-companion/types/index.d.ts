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

declare module 'claude-code' {
  interface PluginState {
    'csr-companion': { data: ShiftsData | null; view: NoteView | null; compact: CompactView | null; infinite: InfiniteRow | null; setup: SetupView | null; taskOwners: TaskOwners }
  }
}
