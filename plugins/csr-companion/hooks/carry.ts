// Prep-compact: before a compaction, the threads you set aside for later ("later" in the pane's Lost and
// found) are handed to the summarizer as reminders, in a section of their own. Never into Pending Tasks:
// that list is how Lost and found sees a thread go quiet, and Claude reads it as its to-do list.

export type Thread = { text: string; state: string; waiting?: boolean; hidden?: string | null; later?: boolean; rank?: number }

// How many threads Lost and found shows waiting on you: not set aside, lost or open (rank under 3), and waiting.
// bin/pane.py's lost_waiting, by which the compaction card counts them.
export const waitingOn = (threads: Thread[]) => threads.filter(t => !t.hidden && (t.rank ?? 9) < 3 && t.waiting).length

const HEAD =
  "After the summary, add a section headed 'Reminders from CSR (not tasks)' and list these lines verbatim. " +
  'The person set them aside for later; do not act on them unless the person asks:'
export const CARRY_MAX = 8
export const CARRY_CHARS = 1200

// The block added to the compaction's instructions: up to eight threads, the whole of it within 1200
// characters; null when there is nothing to carry.
export function carryBlock(threads: Thread[]): string | null {
  const lines: string[] = []
  let room = CARRY_CHARS - HEAD.length
  for (const thread of threads) {
    if (lines.length >= CARRY_MAX || thread.hidden === 'drop' || !thread.later) continue
    const flag = ''
    const text = thread.text.replace(/\s+/g, ' ').trim()
    const line = `- ${text}${flag}`
    if (room < 40) break // no room for a line worth reading
    const fitted = line.length + 1 > room ? `${line.slice(0, room - 2)}…` : line
    lines.push(fitted)
    room -= fitted.length + 1
  }
  return lines.length ? [HEAD, ...lines].join('\n') : null
}
