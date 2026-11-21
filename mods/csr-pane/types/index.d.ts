// The $.state contract of csr-pane. Writers: refresh (engine.ts) and the
// band's and the tabs' buttons. Readers: the Pane and AbovePrompt render hooks.

/** One run of text from the engine's frame, with the style its SGR codes set. */
export type Span = { text: string; color?: string; bold?: boolean; dim?: boolean }

/** The engine's text pane, parsed. `cols` and `rows` are what it was asked for. */
export type Frame = { lines: Span[][]; cols: number; rows: number; at: number }

/** A dream from `companion pane --json`. */
export type Dream = { id: string; at: number; line: string; title?: string; answer?: string }

/** One piece of code that changed after the session that talked about it. */
export type Shift = { file: string; symbol: string; verb: string; subject: string }

/** A past session with newer commits under its code, from `companion pane --json`. */
export type Change = { title: string; talked: string; commits: number; shifts: Shift[] }

/**
 * What the band, the drawing and the Dreams and Changed tabs need from `companion pane --json`.
 * `recalled` counts the times memory came into this session; `days` counts code changes by
 * the day they were made, as 2026-11-15.
 */
export type Summary = {
  status: string
  dreams: Dream[]
  changes: Change[]
  changesTotal: number
  recalled: number
  days: Record<string, number>
  /** The engine's finished cards (`schema` 1 and on): drawn as they come; an unknown id plainly. */
  cards?: ContractCard[]
  /** The major version of the engine's `--json` shape, when it says one. */
  schema?: number
  /** The context window as the herdr pane's Context card reads it (schema 1). */
  context?: {
    used: number
    max: number
    free: number
    buffer?: number
    parts: { name: string; tokens: number; ink: string }[]
    mix?: { shares: [number, number, number]; verdict: string }
  }
  /** "Self-Reflect this session:" and its measured parts (schema 1). */
  csrRow?: { lead: string; parts: string[] }
  /** Whether the herdr pane runs for this session (schema 1). */
  herdrPaneLive?: boolean
}

/** Self-Reflect's compactions in this session and the tokens they saved, from `companion pane --compact-plan`. */
export type Plan = { compactions: number; saved: number }

/** One finished card from the engine's `--json --cards` (the pane contract, csr-engine contract.rs). */
export type ContractCard = {
  id: string
  title: string
  tag?: string
  tone?: string
  lines: { text: string; style: string }[]
  rows: { label: string; value: number; text: string; tone: string }[]
  ring?: { spent: number; cap: number; turning: boolean }
}

/** The tab the pane shows. */
export type Tab = 'now' | 'dreams' | 'changes' | 'notes' | 'figure'

/** The last engine failure in words, or null after a good run. */
export type Failure = { text: string } | null

/** One segment of the context bar. */
export type Segment = { name: string; tokens: number; color: string; kind: 'used' | 'free' | 'buffer' }

/** What the figure at the top of the pane draws, read in refresh. */
export type Usage = {
  used: number
  max: number
  segments: Segment[]
  limits: { kind: string; percentUsed: number; resetsAt?: string }[]
}

declare module 'claude-code' {
  interface PluginState {
    'csr-pane': {
      frame: Shaped<Frame | null>
      summary: Shaped<Summary | null>
      usage: Shaped<Usage | null>
      plan: Shaped<Plan | null>
      failure: Failure
      isPlaced: boolean
      isHidden: boolean
      tab: Tab
    }
  }
}
