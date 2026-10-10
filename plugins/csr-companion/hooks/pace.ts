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
//  4. A share of the work. The dreamer never spends more than 1 token for every
//     10 the main session spends.

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
}

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

// What the next look would cost, guessed from the last look, else from the
// main session's last step (a fork re-reads the same context).
export const canAfford = (pace: Pace, lastStep: number) =>
  pace.dreamer + (pace.lastLook || lastStep) <= SHARE * pace.claude

export function paceText(pace: Pace | null, chapter: Chapter | null, on: boolean): string {
  if (!on) return 'Dreams are off. Turn them on in /config: csr-companion, Dreams.'
  const now = chapter
    ? dreamsLeft(chapter)
      ? `${dreamsLeft(chapter)} of ${allowance(chapter)} dreams left in this chapter; the next look in about ${Math.max(0, chapter.due - chapter.steps)} of Claude's steps, if CSR has something new.`
      : `No dreams left in this chapter; it starts over at the next compaction.`
    : `${PER_CHAPTER} dreams to a chapter, which starts over at each compaction.`
  if (!pace || pace.looks === 0) {
    return `Dreams are on. ${now} The dreamer looks only when CSR has something new about the work in hand, and never spends more than 1 token for every 10 Claude spends. No looks yet this session.`
  }
  const k = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : `${Math.round(n / 1e3)}k`)
  const share = pace.claude ? Math.round((pace.dreamer / pace.claude) * 100) : 0
  return `Dreams are on. ${now} This session: ${pace.looks} ${pace.looks === 1 ? 'look' : 'looks'}, ${pace.dreams} ${pace.dreams === 1 ? 'dream' : 'dreams'}, ${k(pace.dreamer)} tokens (${share}% of Claude's ${k(pace.claude)}).`
}
