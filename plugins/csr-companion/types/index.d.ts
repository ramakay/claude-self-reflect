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
}

// One thing for the band: a note from CSR's memory about a file (bin/pane.py --note),
// or a dream the dreamer asked.
export type Note = {
  id: string
  kind: 'shift' | 'rule' | 'dream'
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
  tag: 'Heads up' | 'You should know' | 'Dream'
  shownAt: number
  promptsSurvived: number
  message?: string
  confirmOff?: boolean // the dim 4 was pressed once: the next press turns dreams off
}

declare module 'claude-code' {
  interface PluginState {
    'csr-companion': { data: ShiftsData | null; view: NoteView | null }
  }
}
