// Prep-compact: before a compaction, the threads still open or gone quiet are handed to the summarizer, so
// they land in the summary's Pending Tasks instead of dropping out of it. The pane finds them (pane.py --lost).

export type Thread = { text: string; state: string; waiting?: boolean; hidden?: string | null }

const HEAD = 'Carry these open threads into Pending Tasks, one line each, unless the conversation closed them:'
export const CARRY_MAX = 8
export const CARRY_CHARS = 1200

// The block added to the compaction's instructions: up to eight threads, the whole of it within 1200
// characters; null when there is nothing to carry.
export function carryBlock(threads: Thread[]): string | null {
  const lines: string[] = []
  let room = CARRY_CHARS - HEAD.length
  for (const thread of threads) {
    if (lines.length >= CARRY_MAX || thread.hidden === 'drop') continue
    const flag = thread.waiting ? ' (waiting on the user)' : ''
    const text = thread.text.replace(/\s+/g, ' ').trim()
    const line = `- ${text}${flag}`
    if (room < 40) break // no room for a line worth reading
    const fitted = line.length + 1 > room ? `${line.slice(0, room - 2)}…` : line
    lines.push(fitted)
    room -= fitted.length + 1
  }
  return lines.length ? [HEAD, ...lines].join('\n') : null
}
