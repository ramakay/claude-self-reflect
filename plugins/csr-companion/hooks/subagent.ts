// A compacting subagent keeps its brief and its progress. The stock summarizer can drop the task a subagent was
// given and where it had got to; the companion receives every subagent compaction as session.compact with the
// subagent's own transcript (e.messages), and adds a block to the summarizer's instructions, built from that
// transcript alone: no process, no engine call, no model call. Pure, so a test reads it without an engine.

import { KEEP_HEADER } from './compact'

// The part of SessionMessage this reads (the engine's types: SessionMessage, ToolUseSummary, ToolResultSummary).
export type Said = {
  role: 'user' | 'assistant'
  text: string
  toolUses?: ReadonlyArray<{ tool_use_id: string; tool: string; input: Record<string, unknown>; text?: string }>
  toolResults?: ReadonlyArray<{ tool_use_id: string; text: string }>
}

export const BRIEF_MAX = 6144 // 6 KB of characters; longer keeps the first 4 KB and the last 2 KB
export const BRIEF_HEAD = 4096
export const BRIEF_TAIL = 2048
export const BLOCK_MAX = 8192 // the whole block, in characters
export const FILES_MAX = 30
export const COMMITS_MAX = 20
export const TEST_LINES_MAX = 10
export const TEST_LINE_CHARS = 200
export const PROGRESS_CHARS = 600
const EDITS = new Set(['Edit', 'Write', 'MultiEdit'])
const HEAD = `${KEEP_HEADER} subagent. Keep these verbatim in the summary; they are the subagent's task and state:`
const MIN_ROOM = 40 // a section with less room than this is left out whole

const cut = (text: string, max: number) => (text.length > max ? `${text.slice(0, Math.max(0, max - 1))}…` : text)

// The brief, whole to 6 KB; past it the head and the tail around a cut mark that says how much is missing.
export function briefOf(text: string): string {
  if (text.length <= BRIEF_MAX) return text
  const missing = text.length - BRIEF_HEAD - BRIEF_TAIL
  return `${text.slice(0, BRIEF_HEAD)}\n[… ${missing} characters cut …]\n${text.slice(text.length - BRIEF_TAIL)}`
}

type Result = { tool: string | undefined; text: string; order: number }

// Every tool result the transcript holds, once each (an assistant message carries its call's text and the next user
// message carries the same result), oldest first; the tool that made it is known where its call is in the transcript.
function resultsOf(messages: readonly Said[]): Result[] {
  const tools = new Map<string, string>()
  const found = new Map<string, Result>()
  messages.forEach((message, order) => {
    for (const use of message.toolUses ?? []) {
      tools.set(use.tool_use_id, use.tool)
      if (typeof use.text === 'string') found.set(use.tool_use_id, { tool: use.tool, text: use.text, order })
    }
    for (const result of message.toolResults ?? []) {
      if (typeof result.text === 'string') found.set(result.tool_use_id, { tool: tools.get(result.tool_use_id), text: result.text, order })
    }
  })
  return [...found.values()].sort((a, b) => a.order - b.order)
}

// `git commit` prints "[branch sha] subject" (and "[branch (root-commit) sha] subject"). Only Bash makes one.
const COMMIT = /^\[([^\]\n]+?) ([0-9a-f]{7,40})\] (\S.*)$/gm
const TESTED = /test result:|\bpassed\b|\bfailed\b/i

// The block for the summarizer's instructions, or null when the transcript holds nothing worth keeping.
export function subagentBlock(messages: readonly Said[]): string | null {
  const first = messages.find(message => message.role === 'user' && message.text.trim())
  const brief = first ? briefOf(first.text.trim()) : ''

  const results = resultsOf(messages)
  const commits = new Map<string, string>() // by sha, so a commit shown twice counts once
  for (const result of results) {
    if (result.tool !== undefined && result.tool !== 'Bash') continue
    for (const match of result.text.matchAll(COMMIT)) commits.set(match[2]!, `[${match[1]} ${match[2]}] ${cut(match[3]!.trim(), 200)}`)
  }
  const made = [...commits.values()].slice(-COMMITS_MAX)

  const files = new Map<string, true>() // by path, in the order each was last edited
  for (const message of messages) {
    for (const use of message.toolUses ?? []) {
      const path = use.input?.file_path
      if (!EDITS.has(use.tool) || typeof path !== 'string' || !path) continue
      files.delete(path)
      files.set(path, true)
    }
  }
  const edited = [...files.keys()].slice(-FILES_MAX)

  let tests: string[] = []
  for (let i = results.length - 1; i >= 0; i--) {
    const result = results[i]!
    if (result.tool !== undefined && result.tool !== 'Bash') continue // a Read of a file that says "passed" is no test run
    const lines = result.text.split('\n').map(line => line.trim()).filter(line => TESTED.test(line))
    if (lines.length) {
      tests = lines.slice(-TEST_LINES_MAX).map(line => cut(line, TEST_LINE_CHARS))
      break
    }
  }

  const newest = [...messages].reverse().find(message => message.role === 'assistant' && message.text.trim())
  const progress = newest ? cut(newest.text.trim(), PROGRESS_CHARS) : ''

  const lines = (items: string[], mark = '- ') => items.map(item => `${mark}${item}`).join('\n')
  // [priority, title, body]: shown in this order; room is given in the order of the priority column (lower first).
  const parts: Array<[number, string, string]> = [
    [0, 'Task brief (its first message, verbatim):', brief],
    [1, 'Commits it made:', lines(made)],
    [4, `Files it edited (newest ${FILES_MAX}):`, lines(edited)],
    [2, 'Its last test outcome:', lines(tests)],
    [3, 'In progress (its newest words):', progress],
  ]
  const present = parts.filter(([, , body]) => body)
  if (!present.length) return null

  let room = BLOCK_MAX - HEAD.length
  const given = new Map<number, string>()
  for (const [rank, title, body] of [...present].sort((a, b) => a[0] - b[0])) {
    const space = room - title.length - 2 // the newlines around it
    if (space < MIN_ROOM) continue
    const fitted = body.length <= space ? body : cut(body, space)
    given.set(rank, `${title}\n${fitted}`)
    room -= title.length + fitted.length + 2
  }
  const shown = present.filter(([rank]) => given.has(rank)).map(([rank]) => given.get(rank)!)
  return shown.length ? [HEAD, ...shown].join('\n') : null
}
