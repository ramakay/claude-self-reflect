// How often the dreamer looks: earned, not a fixed number a day and not unlimited.
//
// Three rules, each decided without a model call:
//  1. Something new. A look happens only when CSR's brief near this work holds
//     a line (a receipt, a file) the last look did not have.
//  2. Earned pace. It starts on You should know's clock, a look every 6 steps of
//     Claude's work; a look that finds nothing worth asking doubles the wait, up
//     to 48 steps; a dream you used (helpful, asked Claude) sets it back to 6.
//     A dream you type past backs off further, as You should know does.
//  3. A share of the work. The dreamer never spends more than 1 token for every
//     10 the main session spends.

export const FIRST_EVERY = 6
export const LONGEST_EVERY = 48
export const SHARE = 0.1

export type Pace = {
  session: string
  claude: number // tokens the main session spent, counted step by step
  dreamer: number // tokens the dreamer's looks spent
  every: number // steps between looks now
  looks: number
  dreams: number
  lastLook: number // tokens the last look cost: the estimate for the next
}

export const freshPace = (session: string): Pace => ({
  session,
  claude: 0,
  dreamer: 0,
  every: FIRST_EVERY,
  looks: 0,
  dreams: 0,
  lastLook: 0,
})

// The brief's lines that carry a fact: a receipt in brackets, or a listed file.
// Dreams already offered are left out, so a dream does not count as news.
export function facts(brief: string): string[] {
  const cut = brief.indexOf('Dreams already offered')
  const body = cut === -1 ? brief : brief.slice(0, cut)
  return [...new Set(body.split('\n').map(line => line.trim()).filter(line => /\[[^\]]+\]/.test(line) || line.startsWith('- ')))]
}

export const hasNews = (now: readonly string[], before: readonly string[]) => now.some(line => !before.includes(line))

// What the next look would cost, guessed from the last look, else from the
// main session's last step (a fork re-reads the same context).
export const canAfford = (pace: Pace, lastStep: number) =>
  pace.dreamer + (pace.lastLook || lastStep) <= SHARE * pace.claude

const slower = (every: number) => Math.min(LONGEST_EVERY, every * 2)

// The pace after a look came back empty, or after you answered a dream.
export function paceAfter(pace: Pace, outcome: string): Pace {
  if (outcome === 'none' || outcome === 'not_relevant') return { ...pace, every: slower(pace.every) }
  if (outcome === 'helpful' || outcome === 'ask_claude') return { ...pace, every: FIRST_EVERY }
  return pace
}

export function paceText(pace: Pace | null, on: boolean): string {
  if (!on) return 'Dreams are off. Turn them on in /config: csr-companion, Dreams.'
  if (!pace || pace.looks === 0) {
    return 'Dreams are on. The dreamer looks only when CSR has something new near this work, waits longer each time it finds nothing, and never spends more than 1 token for every 10 Claude spends. No looks yet this session.'
  }
  const k = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : `${Math.round(n / 1e3)}k`)
  const share = pace.claude ? Math.round((pace.dreamer / pace.claude) * 100) : 0
  return `Dreams are on. This session: ${pace.looks} ${pace.looks === 1 ? 'look' : 'looks'}, ${pace.dreams} ${pace.dreams === 1 ? 'dream' : 'dreams'}, ${k(pace.dreamer)} tokens (${share}% of Claude's ${k(pace.claude)}). Next look after ${pace.every} steps, if CSR has something new.`
}
