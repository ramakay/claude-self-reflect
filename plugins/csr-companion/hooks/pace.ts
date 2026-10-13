// How often the dreamer looks: a feed with a limit, not a fixed number a day and not unlimited.
//
// Four rules, each decided without a model call:
//  1. A chapter holds five dreams. A chapter runs from the session's start, or
//     its last compaction, to the next compaction; one that runs two hours
//     without one starts over too. A dream you adopt (helpful, ask Claude) gives
//     its place back. Looks stop at twice the chapter's dreams.
//  2. The dreams left are spread over the chapter. The first look comes after 6
//     of Claude's steps; each later one after the steps a chapter of yours
//     usually runs (the median of your last chapters), less those gone, over the
//     dreams left. A look that finds nothing does not slow the next.
//  3. Something new. A look happens only when CSR's brief holds a line about the
//     work in hand (a "* " line: a note on a file Claude handled lately, a thing
//     the person built that came up) the last look did not have.
//  4. A share of the work. Before each look the dreamer checks that what it has spent, plus what the last look
//     cost, stays within 1 token for every 10 the main session has spent. The check is a guess: a fork takes no
//     token ceiling (ModelForkRequest is a prompt and nothing else), so one look can cost more than the last and
//     end over the share. That is not forgiven: the excess is owed, every look after it waits, and the wait
//     ends when Claude's own spending brings the total back within the share.

export const FIRST_EVERY = 6
export const PER_CHAPTER = 5
export const CHAPTER_MS = 2 * 60 * 60 * 1000
export const USUAL_STEPS = 120 // a chapter's steps, until there are chapters of yours to learn from
export const SHARE = 0.1

export type Pace = {
  session: string
  claude: number // tokens the main session spent, counted step by step
  dreamer: number // tokens the dreamer's looks spent
  looks: number
  dreams: number
  lastLook: number // tokens the last look cost: the estimate for the next
  step?: number // tokens the main session's last step spent: the estimate for a look no step of its own started
  turns?: Record<string, Receipt> // what a subagent's turns have been charged, so a replay or a reload never charges twice
}

// What one subagent turn has been charged to `claude`: the step indexes charged, their tokens, and whether the turn's
// own total (turn.complete's usage, a sum of its steps, not more usage) has been settled against them.
export type Receipt = { tokens: number; steps: number[]; closed?: boolean }
export const TURNS_KEPT = 60

// The pace with a subagent step's tokens charged to the main session's spend (the share is measured against everything
// Claude spends), once: a step already charged changes nothing. After the turn's total was settled a late step adds no
// tokens (they are in the total) but is remembered.
export function charge(pace: Pace, turn: string, index: number, tokens: number): Pace {
  const seen = pace.turns?.[turn]
  if (seen?.steps.includes(index)) return pace
  const add = seen?.closed ? 0 : Math.max(0, tokens)
  const { [turn]: _old, ...others } = pace.turns ?? {}
  const turns = { ...others, [turn]: { ...seen, tokens: (seen?.tokens ?? 0) + add, steps: [...(seen?.steps ?? []), index] } }
  return { ...pace, claude: pace.claude + add, turns: keepTurns(turns) }
}

// turn.complete's usage is the turn's total, steps included. Only what the steps seen did not already charge is added
// (steps the hooks never saw), never the total again; a total below what was charged adds nothing.
export function settle(pace: Pace, turn: string, total: number): Pace {
  const seen = pace.turns?.[turn]
  const more = Math.max(0, total - (seen?.tokens ?? 0))
  const { [turn]: _old, ...others } = pace.turns ?? {}
  const turns = { ...others, [turn]: { tokens: (seen?.tokens ?? 0) + more, steps: seen?.steps ?? [], closed: true } }
  return { ...pace, claude: pace.claude + more, turns: keepTurns(turns) }
}

const keepTurns = (turns: Record<string, Receipt>) => Object.fromEntries(Object.entries(turns).slice(-TURNS_KEPT))

export const freshPace = (session: string): Pace => ({
  session,
  claude: 0,
  dreamer: 0,
  looks: 0,
  dreams: 0,
  lastLook: 0,
})

export type Chapter = {
  session: string
  started: number // ms
  steps: number // Claude's steps in this chapter
  due: number // the step the next look is due at
  looks: number
  shown: number
  adopted: number
  gen?: number // counts the session's chapters: a look that outlived its chapter knows it, whatever the clock says
}

export const freshChapter = (session: string, started: number): Chapter => ({
  session,
  started,
  steps: 0,
  due: FIRST_EVERY,
  looks: 0,
  shown: 0,
  adopted: 0,
})

export const allowance = (chapter: Chapter) => PER_CHAPTER + chapter.adopted
export const dreamsLeft = (chapter: Chapter) => Math.max(0, allowance(chapter) - chapter.shown)
export const mayLook = (chapter: Chapter) => dreamsLeft(chapter) > 0 && chapter.looks < 2 * allowance(chapter)

// Steps until the next look: the rest of a usual chapter over the dreams left, never under 6.
export function spacing(chapter: Chapter, usual: number): number {
  const left = dreamsLeft(chapter)
  if (!left) return 0
  return Math.max(FIRST_EVERY, Math.round((usual - chapter.steps) / left))
}

export function median(values: readonly number[]): number {
  const sorted = values.filter(value => Number.isFinite(value) && value > 0).sort((a, b) => a - b)
  if (!sorted.length) return USUAL_STEPS
  const middle = Math.floor(sorted.length / 2)
  return sorted.length % 2 ? sorted[middle]! : Math.round((sorted[middle - 1]! + sorted[middle]!) / 2)
}

// The brief's lines about the work in hand, the ones that start with "* ". Past lines, what CSR handed
// Claude and dreams already offered never count: they move without the work moving.
export function facts(brief: string): string[] {
  return [...new Set(brief.split('\n').filter(line => line.startsWith('* ')).map(line => line.trim()))]
}

export const hasNews = (now: readonly string[], before: readonly string[]) => now.some(line => !before.includes(line))

// Tokens the dreamer has spent past its share: 0 while it is within it. A look that cost more than the guess ends
// here with a number above 0, and it stays until `claude` has grown enough to cover it.
export const owed = (pace: Pace) => Math.max(0, pace.dreamer - SHARE * pace.claude)

// The cost of a spend that is not known (a fork that threw may have been paid for): charged as the guess.
export const guess = (pace: Pace, lastStep: number) => pace.lastLook || lastStep

// What the next look would cost, guessed from the last look, else from the
// main session's last step (a fork re-reads the same context). Nothing runs while anything is owed.
export const canAfford = (pace: Pace, lastStep: number) =>
  owed(pace) === 0 && pace.dreamer + guess(pace, lastStep) <= SHARE * pace.claude

export function paceText(pace: Pace | null, chapter: Chapter | null, on: boolean): string {
  if (!on) return 'Dreams are off. Turn them on in /config: csr-companion, Dreams.'
  const now = chapter
    ? dreamsLeft(chapter)
      ? `${dreamsLeft(chapter)} of ${allowance(chapter)} dreams left in this chapter; the next look in about ${Math.max(0, chapter.due - chapter.steps)} of Claude's steps, if CSR has something new.`
      : `No dreams left in this chapter; it starts over at the next compaction.`
    : `${PER_CHAPTER} dreams to a chapter, which starts over at each compaction.`
  if (!pace || pace.looks === 0) {
    return `Dreams are on. ${now} The dreamer looks only when CSR has something new about the work in hand, and checks before each look that its spend stays within 1 token for every 10 Claude spends. No looks yet this session.`
  }
  const k = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : `${Math.round(n / 1e3)}k`)
  const share = pace.claude ? Math.round((pace.dreamer / pace.claude) * 100) : 0
  const over = owed(pace)
  const debt = over > 0 ? ` A look ran over its share by ${k(over)}; the next waits until Claude's spending has caught up.` : ''
  return `Dreams are on. ${now} This session: ${pace.looks} ${pace.looks === 1 ? 'look' : 'looks'}, ${pace.dreams} ${pace.dreams === 1 ? 'dream' : 'dreams'}, ${k(pace.dreamer)} tokens (${share}% of Claude's ${k(pace.claude)}).${debt}`
}
