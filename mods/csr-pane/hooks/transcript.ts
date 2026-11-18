// The session's own record, read from its transcript: the context after each reply, the prompts,
// the compactions, the recalls, the edits, and where the window's messages went. It feeds the
// session timeline (CSR's own figure) and the Context card's parts. The engine has no cheap answer
// for these (its `--png` run takes two seconds), so the mod reads the file itself, from where it
// left off: `tail -c +N | head -c 4M` hands the new bytes, 4 MiB a run. A 34 MB transcript parses
// in about 50 ms, once; after that only what is new is read.
import type { EngineInterface } from 'claude-code'

export type Facts = {
  /** [time in ms, context tokens] after each reply, oldest first. */
  replies: [number, number][]
  prompts: number[]
  compacts: number[]
  /** A memory tool call, or Self-Reflect's hook bringing memory with a prompt. */
  recalls: number[]
  edits: number[]
  /** Characters since the last compaction: tool output, talk, and Self-Reflect's text. */
  window: { tools: number; talk: number; csr: number }
  /** True when the read began inside the file (a long session): the timeline starts later. */
  partial: boolean
}

export const emptyFacts = (): Facts => ({
  replies: [],
  prompts: [],
  compacts: [],
  recalls: [],
  edits: [],
  window: { tools: 0, talk: 0, csr: 0 },
  partial: false,
})

const MEMORY_TOOL = /^mcp__(plugin_[a-z0-9-]+_)?claude-self-reflect__/
const MEMORY_WORDS = /\[\[CSR|\bCSR\b|Self-Reflect|csr_reflect_on_past/
const WRITERS = new Set(['Edit', 'Write', 'MultiEdit', 'NotebookEdit'])
/** The most of a long transcript read back: about the last few hours of a busy session. */
export const MAX_BACK = 64 * 1024 * 1024
const CHUNK = 4 * 1024 * 1024
const MAX_POINTS = 2000
const MAX_IDS = 512

type Raw = {
  type?: string
  subtype?: string
  isSidechain?: boolean
  isMeta?: boolean
  isCompactSummary?: boolean
  timestamp?: string
  message?: { id?: string; content?: unknown; usage?: Record<string, unknown> }
  attachment?: { type?: string; content?: unknown; hookEvent?: string }
}

const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : 0)

/** Characters of a tool result's content: a string, or text blocks. */
function resultChars(content: unknown): number {
  if (typeof content === 'string') return content.length
  if (!Array.isArray(content)) return 0
  let n = 0
  for (const b of content as { type?: string; text?: unknown }[]) if (typeof b?.text === 'string') n += b.text.length
  return n
}

function textOf(content: unknown): { text: number; isResult: boolean; words: string; results: number } {
  if (typeof content === 'string') return { text: content.length, isResult: false, words: content.slice(0, 40), results: 0 }
  if (!Array.isArray(content)) return { text: 0, isResult: false, words: '', results: 0 }
  let text = 0
  let isResult = false
  let words = ''
  let results = 0
  for (const b of content as { type?: string; text?: unknown; content?: unknown }[]) {
    if (b?.type === 'tool_result') {
      isResult = true
      results += resultChars(b.content)
    }
    if (b?.type === 'text' && typeof b.text === 'string') {
      text += b.text.length
      if (words === '') words = b.text.slice(0, 40)
    }
  }
  return { text, isResult, words, results }
}

/** A reader keeps its place in one transcript: what it read, and the facts so far. */
export type Reader = { path: string; offset: number; facts: Facts; ids: Set<string> }

export const newReader = (path: string): Reader => ({ path, offset: 0, facts: emptyFacts(), ids: new Set() })

/** Takes one line of the transcript into the facts. */
export function take(r: Reader, line: string): void {
  if (line.length < 2) return
  let d: Raw
  try {
    d = JSON.parse(line) as Raw
  } catch {
    return
  }
  if (d.isSidechain === true) return
  const at = d.timestamp ? Date.parse(d.timestamp) : NaN
  if (!Number.isFinite(at)) return
  const f = r.facts
  if (d.type === 'assistant') {
    const id = d.message?.id ?? ''
    const u = d.message?.usage
    if (u && id !== '' && !r.ids.has(id)) {
      r.ids.add(id)
      const ctx = num(u.input_tokens) + num(u.cache_read_input_tokens) + num(u.cache_creation_input_tokens)
      if (ctx > 0) f.replies.push([at, ctx])
    }
    const content = d.message?.content
    if (Array.isArray(content)) {
      for (const b of content as { type?: string; name?: string; text?: unknown; input?: unknown }[]) {
        if (b?.type === 'tool_use' && typeof b.name === 'string') {
          if (MEMORY_TOOL.test(b.name)) f.recalls.push(at)
          else if (WRITERS.has(b.name)) f.edits.push(at)
          f.window.talk += JSON.stringify(b.input ?? '').length
        } else if (b?.type === 'text' && typeof b.text === 'string') f.window.talk += b.text.length
      }
    }
  } else if (d.type === 'system' && d.subtype === 'compact_boundary') {
    f.compacts.push(at)
    f.window = { tools: 0, talk: 0, csr: 0 }
  } else if (d.type === 'user') {
    const { text, isResult, words, results } = textOf(d.message?.content)
    if (isResult) f.window.tools += results
    else if (d.isMeta !== true && d.isCompactSummary !== true) {
      f.window.talk += text
      if (text > 0 && !/^\s*<(task-notification|local-command|command-|system-reminder)/.test(words)) f.prompts.push(at)
    }
  } else if (d.type === 'attachment' && d.attachment?.type === 'hook_success') {
    const content = typeof d.attachment.content === 'string' ? d.attachment.content : ''
    if (content !== '' && MEMORY_WORDS.test(content)) {
      f.window.csr += content.length
      if (d.attachment.hookEvent === 'UserPromptSubmit' || d.attachment.hookEvent === 'SessionStart') f.recalls.push(at)
    }
  }
}

/** UTF-8 bytes of `text`, which holds no broken pairs (it ends at a newline). */
export function byteLength(text: string): number {
  let n = 0
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i)
    if (c < 0x80) n += 1
    else if (c < 0x800) n += 2
    else if (c >= 0xd800 && c <= 0xdbff) {
      n += 4
      i++
    } else n += 3
  }
  return n
}

/** Takes a run of text: whole lines only. Returns the bytes taken. */
export function takeText(r: Reader, text: string, isFirstOfPartial: boolean): number {
  const end = text.lastIndexOf('\n')
  if (end < 0) return 0
  const body = text.slice(0, end + 1)
  const lines = body.split('\n')
  // A read that starts inside the file starts inside a line: that line is dropped.
  lines.forEach((line, i) => {
    if (!(isFirstOfPartial && i === 0)) take(r, line)
  })
  return byteLength(body)
}

/** Every other point past MAX_POINTS, the newest kept; ids past MAX_IDS forget the oldest. */
function halve<T>(list: T[]): T[] {
  return list.length > MAX_POINTS ? list.filter((_, i) => i % 2 === 0 || i === list.length - 1) : list
}

export function thin(r: Reader): void {
  const f = r.facts
  f.replies = halve(f.replies)
  f.prompts = halve(f.prompts)
  f.recalls = halve(f.recalls)
  f.edits = halve(f.edits)
  // A message's lines come one after another: only the last few ids are needed to count each once.
  if (r.ids.size > MAX_IDS) r.ids = new Set(Array.from(r.ids).slice(-MAX_IDS / 2))
}

/** The transcript of a session: where Claude Code keeps it for a project directory. */
export function transcriptPath(home: string, root: string, sid: string): string {
  return `${home}/.claude/projects/${root.replace(/[^A-Za-z0-9]/g, '-')}/${sid}.jsonl`
}

/** Reads what is new in the transcript; resolves whether anything was. */
export async function readMore($: EngineInterface, r: Reader): Promise<boolean> {
  const stat = await $.fs.stat(r.path).catch(() => undefined)
  if (!stat || stat.kind !== 'file') return false
  if (stat.size < r.offset) Object.assign(r, newReader(r.path))
  if (stat.size === r.offset) return false
  let partial = false
  if (r.offset === 0 && stat.size > MAX_BACK) {
    r.offset = stat.size - MAX_BACK
    r.facts.partial = true
    partial = true
  }
  let changed = false
  for (let guard = 0; guard < 20 && r.offset < stat.size; guard++) {
    // The offset and the path go in as arguments of the script, never into its text.
    const argv = ['/bin/sh', '-c', `tail -c +"$1" "$2" | head -c ${CHUNK}`, 'csr-pane', String(r.offset + 1), r.path]
    const out = await $.process.run(argv, { timeoutMs: 8000 }).catch(() => undefined)
    if (!out || out.stdout === '') break
    const text = out.stdout
    const isFull = byteLength(text) >= CHUNK - 4
    const taken = takeText(r, text, partial)
    // The dropped first line of a read that began inside the file still counts as read.
    partial = false
    if (taken === 0) {
      // A line longer than a whole run (a huge tool result): skip it, and the next run's first line.
      if (!isFull) break
      r.offset += byteLength(text)
      partial = true
      continue
    }
    r.offset += taken
    changed = true
    thin(r)
    if (!isFull) break
  }
  thin(r)
  return changed
}
