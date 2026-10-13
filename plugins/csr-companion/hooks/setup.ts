import type { StepId } from '../types'
import { BAND, k } from './compact'

// The first run's walkthrough: each setting a person should choose, one step at a time above the prompt.
// Nothing that acts on its own is on until they say so: dreams start off, compaction starts at suggest.

export const STEPS: StepId[] = ['dreams', 'autoCompact']
// A step set aside without an answer shows at this many session starts, then never; /csr setup reopens it.
export const SETUP_STARTS = 2
// The /config rows' titles, as plugin.json's userConfig names them: where a refused write sends the person.
export const TITLES: Record<StepId, string> = { dreams: 'Dreams', autoCompact: 'Compact at the right moment' }

export type Choice = { hotkey: string; label: string; value: boolean | string }
export const CHOICES: Record<StepId, Choice[]> = {
  dreams: [
    { hotkey: '1', label: 'On', value: true },
    { hotkey: '0', label: 'Off', value: false },
  ],
  autoCompact: [
    { hotkey: '1', label: 'Auto', value: 'auto' },
    { hotkey: '2', label: 'Suggest', value: 'suggest' },
    { hotkey: '0', label: 'Off', value: 'off' },
  ],
}

// Short enough for a 100-column terminal: the band wraps, but each line is meant to fit one row.
export const LINES: Record<StepId, string[]> = {
  dreams: [
    'Dreams: while Claude works, a side agent asks one "what if…?" when CSR\'s memory gives a reason.',
    'It looks only when CSR has something new, and checks before each look that its spend stays within',
    '1 token for every 10 Claude spends. A look can run over; the next waits until Claude has caught up.',
  ],
  autoCompact: [
    `Compact for you: CSR compacts at a quiet turn end once context reaches ${k(BAND.low)}–${k(BAND.high)}.`,
    'Auto: on its own. A 20 s countdown at the terminal; at once from the phone or after 10 min away.',
    'Suggest (the default): it offers, a press compacts. Off: never.',
  ],
}

export const heading = (step: StepId) => `CSR setup ${STEPS.indexOf(step) + 1}/${STEPS.length}`
export const refusedText = (plugin: string, step: StepId) => `Couldn't set it here. Use /config: ${plugin}, ${TITLES[step]}.`

// The steps a session start shows: each not answered yet, unless it was already set aside at SETUP_STARTS
// earlier starts. A start that already showed it (a reload of the module) shows it again.
export function dueSteps(answered: ReadonlySet<StepId>, seen: Partial<Record<StepId, string[]>>, session: string): StepId[] {
  return STEPS.filter(step => {
    const starts = seen[step] ?? []
    return !answered.has(step) && (starts.includes(session) || starts.length < SETUP_STARTS)
  })
}
