// What a file tool call touches, for a note about a file: the part of the call the pane reads to tell which symbols
// of the file the call is about (bin/pane.py --note FILE --touch -). Pure: no host, no clock.
//
// A note about a symbol fires only when the symbol's current span meets a line the call touches, so the pane needs
// the call's own words (an Edit's old_string, a Write's content, a Read's offset and limit), not just the file. They
// go to the pane on stdin as one JSON object, never in argv: a Write's content can be megabytes.

import { byteLength } from './subagent'

export const TOUCH_MAX = 8 * 1024 * 1024 // bytes: a call whose input is bigger names no symbol (the pane reads `too_big`)
export const ASKED_MAX = 200 // calls remembered per file, so a long session does not grow without end

// The tools whose call changes the file: the pane needs the file as it was before the tool ran.
const CHANGES = new Set(['Edit', 'MultiEdit', 'Write'])

export type Touch = {
  tool: string
  offset?: number
  limit?: number
  old_string?: string
  replace_all?: boolean
  edits?: Array<{ old_string: string; replace_all?: boolean }>
  content?: string
  too_big?: boolean
}

const text = (value: unknown): string | undefined => (typeof value === 'string' ? value : undefined)
const count = (value: unknown): number | undefined => (typeof value === 'number' && Number.isFinite(value) ? value : undefined)

// The words of the call the pane reads, from the tool's own argument names. Nothing the call writes beyond what the
// pane needs (an Edit's new_string stays here).
export function touchOf(call: Record<string, unknown>): Touch {
  const tool = String(call.tool)
  const touch: Touch = { tool }
  if (tool === 'Read') {
    const [offset, limit] = [count(call.offset), count(call.limit)]
    if (offset !== undefined) touch.offset = offset
    if (limit !== undefined) touch.limit = limit
  } else if (tool === 'Edit') {
    const old = text(call.old_string)
    if (old !== undefined) touch.old_string = old
    if (call.replace_all === true) touch.replace_all = true
  } else if (tool === 'MultiEdit' && Array.isArray(call.edits)) {
    touch.edits = call.edits.flatMap((edit: unknown) => {
      const old = edit && typeof edit === 'object' ? text((edit as Record<string, unknown>).old_string) : undefined
      return old === undefined ? [] : [{ old_string: old, ...((edit as Record<string, unknown>).replace_all === true ? { replace_all: true } : {}) }]
    })
  } else if (tool === 'Write') {
    const content = text(call.content)
    if (content !== undefined) touch.content = content
  }
  return touch
}

export const changesFile = (touch: Touch): boolean => CHANGES.has(touch.tool)

// A short name for the call, to tell an identical second call from a different one.
export function touchDigest(touch: Touch): string {
  let hash = 5381
  const body = JSON.stringify(touch)
  for (let i = 0; i < body.length; i++) hash = ((hash * 33) ^ body.charCodeAt(i)) >>> 0
  return `${touch.tool}:${body.length}:${hash.toString(36)}`
}

// What goes to the pane's stdin. `before`: the file as it was before the tool ran (null: it was not there or could
// not be read), for the tools that change it; the pane places the call's lines in that text, and asks the engine
// about it, so a tool that has already run by the time the pane starts cannot move them. A payload past TOUCH_MAX
// is replaced by one that says so.
export function touchPayload(touch: Touch, before?: string | null): string {
  const whole = JSON.stringify(before === undefined ? touch : { ...touch, before })
  return byteLength(whole) <= TOUCH_MAX ? whole : tooBigPayload(touch)
}

// The payload for a call the pane is not given: its input, or the file it would change, is past TOUCH_MAX.
export const tooBigPayload = (touch: Touch): string => JSON.stringify({ tool: touch.tool, too_big: true })

// What the pane answered: the notes, and how many notes (shift and rule) there would have been for any touch.
// An answer from before `--touch` is a plain list: every note is a candidate.
export function parseAnswer<T>(answer: unknown): { notes: T[]; candidates: number } | null {
  if (Array.isArray(answer)) return { notes: answer as T[], candidates: answer.length }
  if (answer && typeof answer === 'object') {
    const { notes, candidates } = answer as { notes?: unknown; candidates?: unknown }
    if (Array.isArray(notes)) {
      return { notes: notes as T[], candidates: typeof candidates === 'number' && Number.isFinite(candidates) ? candidates : notes.length }
    }
  }
  return null
}

// The calls already put to the pane for each file, so an identical call does not spawn it again.
export class Asked {
  private readonly calls = new Map<string, Set<string>>()
  private readonly quiet = new Set<string>()

  // True when the pane is due for this call: the file is not quiet and this exact call was not asked. Marks it asked.
  claim(file: string, digest: string): boolean {
    if (this.quiet.has(file)) return false
    const calls = this.calls.get(file) ?? new Set<string>()
    if (calls.has(digest)) return false
    if (calls.size >= ASKED_MAX) calls.clear()
    calls.add(digest)
    this.calls.set(file, calls)
    return true
  }

  // The pane found nothing to say about the file for any call: ask no more while this module lives.
  silence(file: string): void {
    this.quiet.add(file)
    this.calls.delete(file)
  }
}
