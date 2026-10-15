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

// Every size here is in UTF-8 bytes, what a "KB" of the summarizer's instructions is, and every cut falls between
// code points, never inside a surrogate pair.
export const BRIEF_MAX = 6144 // 6 KB; longer keeps the first 4 KB and the last 2 KB
export const BRIEF_HEAD = 4096
export const BRIEF_TAIL = 2048
export const BLOCK_MAX = 8192 // the whole block
export const FILES_MAX = 30
export const COMMITS_MAX = 20
export const TEST_LINES_MAX = 10
export const TEST_LINE_BYTES = 200
export const PROGRESS_BYTES = 600
const EDITS = new Set(['Edit', 'Write', 'MultiEdit'])
const HEAD = `${KEEP_HEADER} subagent. Keep these verbatim in the summary; they are the subagent's task and state:`
const MIN_ROOM = 40 // bytes; a section with less room than this is left out whole

const ELLIPSIS = '…'
const isHigh = (unit: number) => unit >= 0xd800 && unit <= 0xdbff
const isLow = (unit: number) => unit >= 0xdc00 && unit <= 0xdfff

// UTF-8 bytes of a string; a lone surrogate counts as the 3 bytes of U+FFFD, as an encoder writes it.
export function byteLength(text: string): number {
  let bytes = 0
  for (let i = 0; i < text.length; i++) {
    const unit = text.charCodeAt(i)
    if (unit < 0x80) bytes += 1
    else if (unit < 0x800) bytes += 2
    else if (isHigh(unit) && i + 1 < text.length && isLow(text.charCodeAt(i + 1))) {
      bytes += 4
      i++
    } else bytes += 3
  }
  return bytes
}

// The longest start of `text` that fits `max` bytes, ending between code points.
function firstBytes(text: string, max: number): string {
  let bytes = 0
  let i = 0
  while (i < text.length) {
    const unit = text.charCodeAt(i)
    const pair = isHigh(unit) && i + 1 < text.length && isLow(text.charCodeAt(i + 1))
    const size = unit < 0x80 ? 1 : unit < 0x800 ? 2 : pair ? 4 : 3
    if (bytes + size > max) break
    bytes += size
    i += pair ? 2 : 1
  }
  return text.slice(0, i)
}

// The longest end of `text` that fits `max` bytes, starting between code points.
function lastBytes(text: string, max: number): string {
  let bytes = 0
  let i = text.length
  while (i > 0) {
    const unit = text.charCodeAt(i - 1)
    const pair = isLow(unit) && i - 2 >= 0 && isHigh(text.charCodeAt(i - 2))
    const size = unit < 0x80 ? 1 : unit < 0x800 ? 2 : pair ? 4 : 3
    if (bytes + size > max) break
    bytes += size
    i -= pair ? 2 : 1
  }
  return text.slice(i)
}

// `text` in at most `max` bytes, the cut marked by an ellipsis that is counted in them.
const cut = (text: string, max: number) =>
  byteLength(text) <= max ? text : max < byteLength(ELLIPSIS) ? '' : `${firstBytes(text, max - byteLength(ELLIPSIS))}${ELLIPSIS}`

// The brief, whole to 6 KB; past it the head and the tail around a cut mark that says how much is missing.
export function briefOf(text: string): string {
  if (byteLength(text) <= BRIEF_MAX) return text
  const head = firstBytes(text, BRIEF_HEAD)
  const tail = lastBytes(text.slice(head.length), BRIEF_TAIL)
  const missing = [...text.slice(head.length, text.length - tail.length)].length
  return `${head}\n[… ${missing} characters cut …]\n${tail}`
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
      tests = lines.slice(-TEST_LINES_MAX).map(line => cut(line, TEST_LINE_BYTES))
      break
    }
  }

  const newest = [...messages].reverse().find(message => message.role === 'assistant' && message.text.trim())
  const progress = newest ? cut(newest.text.trim(), PROGRESS_BYTES) : ''

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

  let room = BLOCK_MAX - byteLength(HEAD)
  const given = new Map<number, string>()
  for (const [rank, title, body] of [...present].sort((a, b) => a[0] - b[0])) {
    const space = room - byteLength(title) - 2 // the newlines around it
    if (space < MIN_ROOM) continue
    const fitted = cut(body, space)
    given.set(rank, `${title}\n${fitted}`)
    room -= byteLength(title) + byteLength(fitted) + 2
  }
  const shown = present.filter(([rank]) => given.has(rank)).map(([rank]) => given.get(rank)!)
  return shown.length ? [HEAD, ...shown].join('\n') : null
}
