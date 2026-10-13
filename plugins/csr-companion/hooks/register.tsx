import { atom, read, update } from 'claude-code'
import type { EngineInterface, ModelForkResult, ModelUsage, Register, RenderElement, RenderInput, Settings, SettingsSource, Timer } from 'claude-code'

import type { AgentState, CompactView, CompletionKey, CompletionDisposition, InfiniteRow, Note, NoteView, SetupView, Shift, ShiftsData, StepId, TaskOwners } from '../types'
import { admitCompletion, anyActive, claimCompletions, completionKey, completionNews, handoffRequests, parseAgentEnvelope, stripEnvelopes, emptyAgentState, isEditTool, LIST_EVERY_MS, markDisposition, normalizePath, pendingCompletions, planWrite, projectAgentsFile, reconcileAgentList, reduceAgentEvent, settleCompletions } from './agents'
import type { AgentEvent, Envelope, Ledger, WritePlan } from './agents'
import { carryBlock, waitingOn } from './carry'
import type { Thread } from './carry'
import { askClaude, citedCommits, citedFrom, cleanForDraft, dreamId, dreamPrompt, dreamSent, isYskOrigin, isYskPropose, leansOnOld, leaseHeld, markReceipts, nearDream, offersAnotherWay, ownPrompt, parseDream, rebuts, seenEntry, withCsrFacts, wordsBeside, yskFacts, yskLine } from './dreamer'
import { allowance, CHAPTER_MS, FIRST_EVERY, USUAL_STEPS, canAfford, closeTurn, countStep, dreamsLeft, facts, freshChapter, freshPace, guess, hasNews, mayLook, median, owed, paceText, spacing } from './pace'
import type { Chapter, Pace } from './pace'
import { addTerm, AGAIN_MS, BAND_WINDOW_MS, gitVerdict, isBareDigit, mergeGuards, parseGuard, protectedFromGit, reasonFor, UNRESOLVED } from './coach'
import type { LeaseStep } from './dreamer'
import type { GitPush, Guard } from './coach'
import { actWhy, BAND, COUNTDOWN_MS, commitIn, countdownText, decide, gateIn, k, keepList, keepRows, lastQuestion, modeOf, noticeLine, offerText, planText, REARM_TOKENS, resumeOf, ruleIn, seamOf, settingOf } from './compact'
import type { Human, Keep, Mode, Payback, Running, Seam, Setting } from './compact'
import { CHOICES, dueSteps, heading, LINES, refusedText, STEPS } from './setup'

// CSR beside Claude, patterned on Claude Code's own "You should know".
//
// The band speaks in two voices, one note at a time, never the same twice,
// gone after two prompts unanswered:
//  - Heads up / You should know: when Claude reads or edits a file, what CSR
//    remembers about it (a past session's work there that a commit changed
//    after that session ended, or a rule you set for it). Counters and misses,
//    from CSR's memory, no model calls.
//  - Dream: every few steps of Claude's work, the dreamer asks one "what if...?",
//    the question you keep asking the agent, grounded in what CSR remembers.
//    It forks this session the way You should know does, so it spends tokens:
//    off by default, a /config row as You should know is a /plugin switch, and
//    paced by what it earns (./pace.ts), not a daily number.
//
// The pane: this session live, the dreams by date, and code that changed since
// your past sessions, in a herdr split beside Claude.
const script = ($: EngineInterface) => `${$.plugin.root}/bin/pane.py`

// Everything written into the companion's folder (~/.claude-self-reflect/csr-companion) goes through bin/private.py:
// $.fs.write has no mode to ask for, and a file it makes is 0644 in a 0755 folder, readable by every local user, where
// the folder holds text from your transcripts. private.py makes the folders 0700 and the file 0600, and with `redact`
// removes keys, bearer tokens, password=/token= values and PEM blocks from the text first. Rejects when it could not.
// Files that are the person's own (settings.json, CLAUDE.md) are not the companion's and keep $.fs.write.
// `redact`: 'text' removes credentials from plain text; 'json' parses the JSON, redacts the strings in it and
// serializes it again (redacting the serialized form would break its escaping and could leave the secret in).
async function keepWrite($: EngineInterface, path: string, text: string, redact?: 'text' | 'json'): Promise<void> {
  const flag = redact === 'json' ? ['--redact-json'] : redact === 'text' ? ['--redact'] : []
  const ran = await $.process.run(['python3', '-I', `${$.plugin.root}/bin/private.py`, 'write', path, ...flag], { stdin: text, timeoutMs: 10_000 })
  if (ran.exitCode !== 0) throw new Error(`private write failed: ${ran.stderr.trim()}`)
}
const guard = ($: EngineInterface) => `${$.plugin.root}/bin/pane_guard.py` // runs pane.py, and outlives an edit that breaks it
const PANE = 'csr'
const LABEL = 'CSR'
const STAR = '☾' // CSR's mark where You should know draws its ✦
const REFRESH_MS = 10 * 60 * 1000
const CLEAR_AFTER_PROMPTS = 2
const KEPT = 200
const FILE_TOOLS = new Set(['Read', 'Edit', 'Write', 'MultiEdit', 'NotebookEdit'])
// The dreamer: the last 50 dreams never asked again, and after dreams you typed
// past, 1, 2, 4... up to 16 looks skipped, as You should know does. How often it
// looks otherwise is ./pace.ts.
const SEEN_KEPT = 50
const SKIP_MAX = 16
const FACTS_KEPT = 400
const CHAPTERS_KEPT = 20
// One store serves every Claude process on the machine: each session's chapter, pace, news and back-off
// live under its own id, or a second window (or a `claude -p` child) restarts this one's chapter and zeroes
// its pace. Seen and known dreams, answers and chapter lengths stay shared: they are about you, not a session.
const PER_SESSION = ['pace', 'chapter', 'briefFacts', 'looksToSkip', 'typedPastInARow', 'asked', 'sentDream', 'agentLedger'] as const
type Mine = (typeof PER_SESSION)[number]
const SESSIONS_KEPT = 20 // sessions whose keys are kept; older ones are swept at a session's start
const keyOf = (key: Mine, session: string) => `${key}:${session}`
async function ownKey($: EngineInterface, key: Mine): Promise<string> {
  return keyOf(key, await $.session.id())
}
const ADOPTED = new Set(['helpful', 'ask_claude']) // a dream you took up
const OFF_ROW = 'csr-companion.dreams'
// The herdr pane's paper and inks (bin/pane.py: PIGMENT, INK, and each section's tint at HEAD and ROW).
const PAPER = {
  bar: '#2f3a48', // payne: the herdr pane's masthead
  barLow: '#0e0f11', // night
  barText: '#eef5fa', // chalk
  barLabel: '#939ba5',
  text: '#17191c', // lamp
  quiet: '#5e5e59', // graphite
  faint: '#888781', // pencil
  alarm: '#c8442a', // vermilion
}
const INKS = { recall: '#248363', ctx: '#2c5aa0', quiet: '#5e5e59', sepia: '#6f5642' }
type Look = { ink: string; head: string; row: string; glyph: string }
type Part = { text: string; color?: string; bold?: boolean }
// One item of a card: text wrapped under its indent (or cut at the edge), with a mark in its own column.
type Item = { text: string; color?: string; bold?: boolean; cut?: boolean; mark?: [string, string] }

// Words wrapped to a width, as the pane's rows are drawn; a word longer than the width is cut.
function wrapped(text: string, width: number): string[] {
  if (!text) return ['']
  const lines: string[] = []
  let line = ''
  for (const word of text.split(/\s+/).filter(Boolean)) {
    const piece = word.length > width ? word.slice(0, width) : word
    if (line && line.length + 1 + piece.length > width) {
      lines.push(line)
      line = piece
    } else line = line ? `${line} ${piece}` : piece
  }
  return [...lines, line]
}
const CARD: Record<'dreams' | 'can' | 'changed' | 'infinite', Look> = {
  dreams: { ink: '#986c00', head: '#ece6d4', row: '#f1ecdf', glyph: '☾' },
  infinite: { ink: '#2c5aa0', head: '#e6ebf0', row: '#edf0f3', glyph: '♻' },
  can: { ink: '#2c5aa0', head: '#e6ebf0', row: '#edf0f3', glyph: '§' },
  changed: { ink: '#5e5e59', head: '#ebebe9', row: '#f0f0ee', glyph: '~' },
}

const data = atom({ plugin: 'csr-companion', key: 'data' } as const, null as ShiftsData | null)
const view = atom({ plugin: 'csr-companion', key: 'view' } as const, null as NoteView | null)

type HerdrPane = { pane_id: string; tab_id?: string; workspace_id?: string; label?: string | null }

async function run($: EngineInterface, argv: string[]): Promise<string | null> {
  const ran = await $.process.run(['python3', '-I', script($), ...argv], { timeoutMs: 20_000 })
  return ran.exitCode === 0 ? ran.stdout : null
}

// Where a push that names no branch lands, asked of git with the command's own -c settings (Codex review #5: a
// `-c remote.origin.push=...` was left out): the current branch, its upstream, and the push refspecs of the remote it
// goes to, never another remote's. That remote is the one named, else the branch's push remote, the default push
// remote, the branch's remote, origin. push.default=matching sends every branch both sides have, when the push names
// no refspec: git consults push.default only then (Codex r2 #5: `push origin HEAD` on main was denied for it).
// A --config-env setting is a -c setting with its variable's value, in its own place among the others (git takes the
// last of two: astra r3 #2), read from what the command line sets, else this process's environment, which the Bash
// tool's commands inherit. GIT_CONFIG_* the command line sets go with every question.
// Fail closed (astra r3 #3): a variable neither holds, settings git refuses (`git rev-parse --git-dir` fails with
// them), or a question that fails other than by "not set" (config 1, symbolic-ref 1: detached) leave the landing
// unknown, and the push is denied. The reason to deny, or null.
async function landsOnRelease($: EngineInterface, push: GitPush, cwd: string, guard: Guard): Promise<string | null> {
  const settings: string[] = []
  for (const arg of push.config) {
    if (typeof arg === 'string') {
      settings.push('-c', arg)
      continue
    }
    const eq = arg.env.indexOf('=')
    const name = arg.env.slice(eq + 1)
    const value = eq > 0 ? push.env?.[name] ?? (await hostVariable($, name)) : undefined
    if (value === undefined) return UNRESOLVED
    settings.push('-c', `${arg.env.slice(0, eq)}=${value}`)
  }
  const env = Object.fromEntries(Object.entries(push.env ?? {}).filter(([name]) => /^GIT_CONFIG/.test(name)))
  let failed = false
  // What git says, '' for "not set" (an exit in `absent`); anything else marks the landing unknown.
  const git = async (absent: number[], ...args: string[]) => {
    const ran = await $.process.run(['git', '-C', push.dir ?? cwd, ...settings, ...args], { timeoutMs: 2_000, ...(Object.keys(env).length ? { env } : {}) })
      .catch(() => null)
    if (ran?.exitCode === 0) return ran.stdout.trim()
    if (!ran || !absent.includes(ran.exitCode ?? -1)) failed = true
    return ''
  }
  if ((await git([], 'rev-parse', '--git-dir')) === '' || failed) return UNRESOLVED
  const branch = await git([1], 'symbolic-ref', '-q', '--short', 'HEAD')
  const remote = push.remote ||
    (branch && (await git([1], 'config', '--get', `branch.${branch}.pushRemote`))) ||
    (await git([1], 'config', '--get', 'remote.pushDefault')) ||
    (branch && (await git([1], 'config', '--get', `branch.${branch}.remote`))) ||
    'origin'
  const [upstream, specs, mode] = await Promise.all([
    git([1, 128], 'rev-parse', '--abbrev-ref', '--symbolic-full-name', '@{upstream}'), // no upstream: 128
    git([1], 'config', '--get-all', `remote.${remote}.push`),
    git([1], 'config', '--get', 'push.default'),
  ])
  if (failed) return UNRESOLVED
  const found = protectedFromGit(guard, branch, upstream, specs)
  if (found) return found
  return push.implicit && mode === 'matching' && guard.protectedPush.length ? reasonFor(guard, guard.protectedPush[0] as string) : null
}

// The guard this session runs under: the repository's <git toplevel>/.claude/csr-guard.json (the toplevel of the
// session's folder, a worktree's own) and the person's guard.json beside CSR's other files, every protection of
// either. A file that is not a guard (not JSON, a key of the wrong kind) counts for nothing and is said once in the
// pane's error log. Neither file, or neither good: null, and the guard does nothing.
const GUARD_FILE = '.claude-self-reflect/csr-companion/guard.json' // under HOME
const PANE_ERRORS = '.claude-self-reflect/csr-companion/pane-errors.log' // under HOME: pane.py's ERROR_LOG
const told = new Set<string>() // the bad guard files said in the error log since this module loaded
async function guardOf($: EngineInterface, cwd: string): Promise<Guard | null> {
  const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
  let top: string | null = null
  for (let dir = cwd.replace(/\/+$/, ''); dir; dir = dir.slice(0, dir.lastIndexOf('/'))) {
    if (await $.fs.exists(`${dir}/.git`).catch(() => false)) {
      top = dir
      break
    }
  }
  const guards: Guard[] = []
  for (const path of [top && `${top}/.claude/csr-guard.json`, home && `${home}/${GUARD_FILE}`]) {
    if (!path || !(await $.fs.exists(path).catch(() => false))) continue
    const text = await $.fs.read(path).catch(() => null)
    const guard = text === null ? null : parseGuard(text)
    if (guard) guards.push(guard)
    else if (home && !told.has(path)) {
      told.add(path)
      const log = `${home}/${PANE_ERRORS}`
      const was = await $.fs.read(log).catch(() => '')
      const at = new Date(await $.clock.now()).toISOString()
      await keepWrite($, log, `${was}${was && !was.endsWith('\n') ? '\n' : ''}${at} csr guard: ${path} is no guard (not JSON, or a key of the wrong kind); it counts for nothing\n`, 'text').catch(() => undefined)
    }
  }
  return mergeGuards(guards)
}

// A variable of this process's environment, by any name (`$.env.get` takes only names spelled in the source):
// undefined when it is unset, or its name is no shell variable's.
async function hostVariable($: EngineInterface, name: string): Promise<string | undefined> {
  if (!/^[A-Za-z_]\w*$/.test(name)) return undefined
  const ran = await $.process.run(['printenv', name], { timeoutMs: 2_000 }).catch(() => null)
  return ran?.exitCode === 0 ? ran.stdout.replace(/\n$/, '') : undefined
}

async function json<T>($: EngineInterface, argv: string[]): Promise<T | null> {
  const out = await run($, argv)
  if (out === null) return null
  try {
    return JSON.parse(out) as T
  } catch {
    return null
  }
}

async function strings($: EngineInterface, key: string): Promise<string[]> {
  const value = await $.store.get(key).catch(() => undefined)
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []
}

async function count($: EngineInterface, key: string): Promise<number> {
  const value = await $.store.get(key).catch(() => undefined)
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

async function remember($: EngineInterface, key: string, value: string, kept = KEPT, owns?: () => Promise<boolean>): Promise<void> {
  const list = await strings($, key)
  if (!list.includes(value) && (!owns || await owns())) await $.store.set(key, [...list, value].slice(-kept))
}

// What each note came to, kept across sessions: the "at least 1 in 3 acted on" gate
// reads this. A dream's answer also goes to the dream log, where the pane draws it.
async function answered($: EngineInterface, note: Note, answer: string, logged = false, owns?: () => Promise<boolean>): Promise<void> {
  const at = await $.clock.now()
  const value = await $.store.get('answers').catch(() => undefined)
  const list = Array.isArray(value) ? value : []
  if (owns && !(await owns())) return
  await $.store.set('answers', [...list, { id: note.id, answer, at }].slice(-KEPT))
  if (note.kind !== 'dream' || answer === 'shown') return
  if (answer !== 'ignored') {
    // You answered: the dreamer stops backing off.
    await $.store.set(await ownKey($, 'typedPastInARow'), 0)
    await $.store.set(await ownKey($, 'looksToSkip'), 0)
  }
  if (ADOPTED.has(answer)) {
    // You adopted it: it gives its place in the chapter back, if it came in this chapter.
    await changeChapter($, chapter => (note.at >= chapter.started ? { ...chapter, adopted: chapter.adopted + 1 } : chapter))
  }
  if (logged) return // the pane wrote it to the dream log already
  await run($, ['--repo', await $.session.cwd(), '--keep', JSON.stringify({ kind: 'answer', id: note.id, answer, at: at / 1000 })])
}

const DREAM_LOG = '.claude-self-reflect/csr-companion/dreams.jsonl' // under HOME: the pane's answers land here
const SETTLED_IN_PANE_MS = 15_000

// A dream answered in the pane (its answer buttons write the dream log) leaves the band here too, and
// paces the dreamer as an answer in the band would. The log is read only while a dream is up.
async function settledInThePane($: EngineInterface): Promise<void> {
  const up: { note?: Note } = {}
  await update($, view, current => {
    if (current && current.kind !== 'asked' && current.note.kind === 'dream') up.note = current.note
    return current
  })
  const note = up.note
  const home = note && (await $.env.get('HOME'))
  if (!note || !home) return
  const log = await $.fs.read(`${home}/${DREAM_LOG}`).catch(() => '')
  let answer: string | undefined
  for (const line of log.split('\n')) {
    try {
      const record = JSON.parse(line) as { kind?: string; id?: string; answer?: unknown; by?: string }
      if (record.kind === 'answer' && record.id === note.id && record.by === 'pane' && typeof record.answer === 'string') {
        answer = record.answer
      }
    } catch {}
  }
  if (!answer) return
  await update($, view, current => (current?.note.id === note.id ? null : current))
  await answered($, note, answer, true)
}

// A dream that reaches Claude as a prompt is logged "sent": the adoption that counts. "ask_claude" only says the
// box was filled (3 of 7 never went out, 10-09/12), and a dream sent from the pane was logged "ignored" by the
// band. Logged once per dream; the first sending of one not yet taken up gives its chapter place back and stops
// the back-off, as an answer in the band would.
// A dream you sent on and said no to is "rejected", never sent: your words beside it (dreamer.ts, wordsBeside), or
// your next prompt within REPLY_MS. Your words are kept with it, and the next brief quotes them to the dreamer.
const REPLY_MS = 30 * 60 * 1000
type SentDream = { id: string; at: number }
async function sentToClaude($: EngineInterface, text: string, yours: boolean): Promise<void> {
  const home = await $.env.get('HOME')
  if (!home || !text) return
  const log = await $.fs.read(`${home}/${DREAM_LOG}`).catch(() => '')
  const dreams: Array<{ id: string; line: string; at: number }> = []
  const answers = new Map<string, string[]>()
  for (const line of log.split('\n')) {
    try {
      const record = JSON.parse(line) as { kind?: string; id?: string; line?: string; at?: number; answer?: string }
      if (record.kind === 'dream' && record.id && record.line) dreams.push({ id: record.id, line: record.line, at: record.at ?? 0 })
      if (record.kind === 'answer' && record.id && record.answer) answers.set(record.id, [...(answers.get(record.id) ?? []), record.answer])
    } catch {}
  }
  const lastSent = await ownKey($, 'sentDream')
  const dream = dreamSent(text, dreams)
  if (!dream) {
    // Your next prompt after a dream you sent on: a reply to it, which rejects it if it says no. Only the next.
    const own = yours ? ownPrompt(text) : null
    const last = own ? ((await $.store.get(lastSent).catch(() => undefined)) as SentDream | undefined) : undefined
    if (!own || !last?.id) return
    await $.store.delete(lastSent)
    const sent = dreams.find(candidate => candidate.id === last.id)
    if (sent && (await $.clock.now()) - last.at <= REPLY_MS && rebuts(own)) await reject($, sent, own, 'reply', answers.get(sent.id) ?? [])
    return
  }
  const was = answers.get(dream.id) ?? []
  if (was.includes('rejected')) return
  const said = yours ? wordsBeside(text, dream.line) : ''
  if (said && rebuts(said)) {
    await $.store.delete(lastSent).catch(() => undefined)
    return reject($, dream, said, 'prompt', was)
  }
  if (was.includes('sent')) return
  const at = await $.clock.now()
  await run($, ['--repo', await $.session.cwd(), '--keep', JSON.stringify({ kind: 'answer', id: dream.id, answer: 'sent', by: 'prompt', at: at / 1000 })])
  await $.store.set(await ownKey($, 'typedPastInARow'), 0)
  await $.store.set(await ownKey($, 'looksToSkip'), 0)
  if (!was.some(answer => ADOPTED.has(answer))) {
    await changeChapter($, chapter => (dream.at * 1000 >= chapter.started ? { ...chapter, adopted: chapter.adopted + 1 } : chapter))
  }
  if (yours) await $.store.set(lastSent, { id: dream.id, at } satisfies SentDream)
}

// A dream you said no to: "rejected" with your words. An answer, so the back-off stops; a place in the chapter it
// gave back when it was sent or taken up is the chapter's again.
async function reject($: EngineInterface, dream: { id: string; at: number }, said: string, by: string, was: string[]): Promise<void> {
  const at = await $.clock.now()
  await run($, ['--repo', await $.session.cwd(), '--keep', JSON.stringify({ kind: 'answer', id: dream.id, answer: 'rejected', by, said, at: at / 1000 })])
  await $.store.set(await ownKey($, 'typedPastInARow'), 0)
  await $.store.set(await ownKey($, 'looksToSkip'), 0)
  if (was.some(answer => answer === 'sent' || ADOPTED.has(answer))) {
    await changeChapter($, chapter => (dream.at * 1000 >= chapter.started ? { ...chapter, adopted: Math.max(0, chapter.adopted - 1) } : chapter))
  }
}

async function refresh($: EngineInterface, cwd: string): Promise<ShiftsData | null> {
  const fresh = await json<ShiftsData>($, ['--repo', cwd, '--json', '--limit', '60', '--sid', await $.session.id()])
  await update($, data, () => fresh)
  return fresh
}

// Shows a note unless another is up; true when it was shown.
async function show($: EngineInterface, note: Note, tag: NoteView['tag'],
  owns: () => Promise<boolean> = async () => true, held: () => boolean = () => true): Promise<boolean> {
  const shownAt = await $.clock.now()
  if (!(await owns())) return false
  let isShown = false
  await update($, view, (current): NoteView | null => {
    if (!held() || (current && current.kind !== 'asked')) return current // one note at a time
    isShown = true
    return { kind: 'offer', note, tag, shownAt, promptsSurvived: 0 }
  })
  if (isShown && held()) await answered($, note, 'shown', false, owns)
  return isShown
}

async function isBusy($: EngineInterface): Promise<boolean> {
  let busy = false
  await update($, view, current => {
    busy = !!current && current.kind !== 'asked'
    return current
  })
  return busy
}

// ---------------------------------------------------------------- notes from CSR's memory

const looked = new Set<string>() // files already looked up since this module loaded

async function offerFor($: EngineInterface, file: string, tag: NoteView['tag']): Promise<void> {
  if (looked.has(file)) return
  looked.add(file)
  const notes = await json<Note[]>($, ['--repo', file, '--note', file, '--sid', await $.session.id()])
  if (!notes?.length) return
  const skip = new Set([...(await strings($, 'seen')), ...(await strings($, 'known'))])
  const note = notes.find(candidate => !skip.has(candidate.id))
  if (note && (await show($, note, tag))) await remember($, 'seen', note.id)
}

function handoff(shown: NoteView): string {
  const { note } = shown
  if (note.kind === 'dream') return askClaude(note)
  const receipts = [note.oid ? `commit ${note.oid.slice(0, 7)}` : '', `session ${note.conversation.slice(0, 8)}`]
    .filter(Boolean)
    .join(', ')
  return cleanForDraft([
    'Here is a note offered by CSR (claude-self-reflect):',
    '',
    `> ${shown.tag} · ${note.line}`,
    '>',
    `> ${note.text}`,
    '>',
    `> Receipts: ${receipts}.`,
  ].join('\n'))
}

// ---------------------------------------------------------------- the dreamer

async function paceOf($: EngineInterface): Promise<Pace> {
  const session = await $.session.id()
  const value = (await $.store.get(keyOf('pace', session)).catch(() => undefined)) as Pace | undefined
  return value?.session === session ? { ...freshPace(session), ...value } : freshPace(session)
}

// Every change to pace goes through one queue, as the chapter's do: Claude's steps count what they spent while a
// look writes what it spent, and a step that read pace before the look wrote it dropped the look's tokens (or the
// look's late write dropped the steps'): the 1-in-10 cap then counted wrong (PR #317 review).
let paceQueue: Promise<unknown> = Promise.resolve()
function changePace($: EngineInterface, change: (pace: Pace) => Pace, held?: () => boolean): Promise<Pace> {
  const done = paceQueue.then(async () => {
    const current = await paceOf($)
    if (held && !held()) return current
    const pace = change(current)
    await $.store.set(keyOf('pace', pace.session), pace)
    return pace
  })
  paceQueue = done.catch(() => undefined)
  return done
}

const tokensOf = (usage: ModelUsage) =>
  usage.input_tokens + usage.output_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens

// What a look spent, as the dream log keeps it beside its tokens (which old readers still read): the fork's four
// counts, and the model that answered. A fork re-sends the main thread's last request, model and all, so that is
// the model of the step that started the look. The API gives a fork no 5m/1h split of its cache writes.
type Spent = { usage: ModelUsage; model?: string }
const spentFields = (spent?: Spent) => spent ? {
  usage: {
    input_tokens: spent.usage.input_tokens,
    output_tokens: spent.usage.output_tokens,
    cache_read_input_tokens: spent.usage.cache_read_input_tokens,
    cache_creation_input_tokens: spent.usage.cache_creation_input_tokens,
  },
  ...(spent.model ? { model: spent.model } : {}),
} : {}

// This chapter of the session: a fresh one for a new session, and after two hours without a compaction.
async function chapterOf($: EngineInterface): Promise<Chapter> {
  const session = await $.session.id()
  const value = (await $.store.get(keyOf('chapter', session)).catch(() => undefined)) as Chapter | undefined
  const now = await $.clock.now()
  if (value?.session === session && now - value.started < CHAPTER_MS) return { ...freshChapter(session, now), ...value }
  return newChapter($, value?.session === session ? 'two hours' : 'start')
}

// A chapter starts over: five dreams again, the first look soon. A chapter a compaction ended teaches
// how long your chapters run; the dream log gets a line, so the pane counts from here.
async function newChapter($: EngineInterface, why: 'start' | 'compact' | 'two hours'): Promise<Chapter> {
  const session = await $.session.id()
  const old = (await $.store.get(keyOf('chapter', session)).catch(() => undefined)) as Chapter | undefined
  if (why === 'compact' && old?.session === session && old.steps > 0) {
    const lengths = (await $.store.get('chapterSteps').catch(() => undefined)) as number[] | undefined
    await $.store.set('chapterSteps', [...(Array.isArray(lengths) ? lengths : []), old.steps].slice(-CHAPTERS_KEPT))
  }
  const at = await $.clock.now()
  const chapter = { ...freshChapter(session, at), gen: (old?.session === session ? old.gen ?? 0 : 0) + 1 }
  await $.store.set(keyOf('chapter', session), chapter)
  await run($, ['--repo', await $.session.cwd(), '--keep', JSON.stringify({
    kind: 'chapter', at: at / 1000, session, why, left: dreamsLeft(chapter), of: allowance(chapter),
    interactive: person ?? null, // who started it: null where the module never learnt it
  })])
  return chapter
}

// Every change to the chapter goes through one queue: Claude's steps count while a look runs and an
// answer lands, and two read-then-write changes at once would lose one.
let chapterQueue: Promise<unknown> = Promise.resolve()
function serially<T>(work: () => Promise<T>): Promise<T> {
  const done = chapterQueue.then(work)
  chapterQueue = done.catch(() => undefined)
  return done
}

// The chapter as it is now, changed: read fresh inside the queue.
function changeChapter($: EngineInterface, change: (chapter: Chapter) => Chapter, held?: () => boolean): Promise<Chapter> {
  return serially(async () => {
    const current = await chapterOf($)
    const key = await ownKey($, 'chapter')
    if (held && !held()) return current
    const chapter = change(current)
    await $.store.set(key, chapter)
    return chapter
  })
}

async function usualSteps($: EngineInterface): Promise<number> {
  const lengths = (await $.store.get('chapterSteps').catch(() => undefined)) as number[] | undefined
  return median(Array.isArray(lengths) ? lengths : [])
}

// What came of a look, kept in the dream log: once you have answered the last dream, the pane shows it
// in the dream's place, so a quiet dreamer reads as quiet, not as stuck. "every" is the steps from the
// look to the next.
async function lookCameTo($: EngineInterface, owns: () => Promise<boolean>, cwd: string, session: string, outcome: string, chapter: Chapter, base: number,
  tokens = 0, spent?: Spent, extra: Record<string, unknown> = {}) {
  const at = await $.clock.now()
  if (!(await owns())) return
  await run($, ['--repo', cwd, '--keep', JSON.stringify({
    kind: 'look', at: at / 1000, session, outcome, every: Math.max(0, chapter.due - base), tokens, ...spentFields(spent),
    left: dreamsLeft(chapter), of: allowance(chapter), ...extra,
  })])
}

// The two sides of the spend check, kept on every look: "budget" can be checked against what Claude really spent.
// 10-11/12, before pace was kept per session, all 39 budget looks were false; nothing in the log could show it.
const sidesOf = (pace: Pace, lastStep: number) => ({ claude: pace.claude, dreamer: pace.dreamer, next: guess(pace, lastStep), owed: owed(pace) })

// The next look, counted from the step that started this one (Claude's steps go on while a look runs):
// soon after a look that cost nothing, else the chapter's dreams left spread over the rest of it.
const soon = (base: number) => (chapter: Chapter): Chapter => ({ ...chapter, due: base + FIRST_EVERY })
const spread = (base: number, usual: number) => (chapter: Chapter): Chapter => ({
  ...chapter,
  due: mayLook(chapter) ? base + spacing({ ...chapter, steps: base }, usual) : Number.MAX_SAFE_INTEGER,
})

// A chapter left as it is, except that its dreams spent end the look schedule (a completion's look used one).
const kept = (chapter: Chapter): Chapter => (mayLook(chapter) ? chapter : { ...chapter, due: Number.MAX_SAFE_INTEGER })

// What started a look: the main session's own schedule, or the completion of agents (their identities).
type DreamTrigger = { kind: 'main' } | { kind: 'agent'; completions: CompletionKey[] }
const MAIN: DreamTrigger = { kind: 'main' }
type Trace = { forked: boolean; threw: boolean }

// The cited commits this repo does not have: `git cat-file -e`, two seconds each; a git that cannot answer
// marks nothing.
async function missingCommits($: EngineInterface, cwd: string, commits: string[]): Promise<Set<string>> {
  const missing = new Set<string>()
  for (const oid of commits.slice(0, 5)) {
    const ran = await $.process.run(['git', '-C', cwd, 'cat-file', '-e', `${oid}^{commit}`], { timeoutMs: 2_000 }).catch(() => null)
    if (ran && ran.exitCode !== 0 && ran.exitCode !== undefined) missing.add(oid)
  }
  return missing
}

// One dreamer per session: bin/lease.py takes the lease, under an OS lock (dreamer.ts, leaseHeld). With no HOME
// there is no lease to renew, so an active look cannot keep ownership.
const LEASES = '.claude-self-reflect/csr-companion/dreamer-lease' // under HOME
const rankOf = (root: string) => (/\/\.claude\/plugins\//.test(root) ? 0 : 1)

async function lease($: EngineInterface, step: LeaseStep, busy = false): Promise<boolean> {
  const home = await $.env.get('HOME')
  if (!home) return step !== 'renew'
  const argv = ['python3', '-I', `${$.plugin.root}/bin/lease.py`, step, '--folder', `${home}/${LEASES}`, '--session', await $.session.id(),
    '--root', $.plugin.root, '--rank', String(rankOf($.plugin.root)), '--now', String(await $.clock.now()), ...(busy ? ['--busy'] : [])]
  let failure = ''
  const ran = await $.process.run(argv, { timeoutMs: 2_000 }).catch((error: unknown) => {
    failure = error instanceof Error ? error.message : String(error)
    return null
  })
  return leaseHeld(ran, step === 'renew' ? '' : failure)
}

const LEASE_RENEW_MS = 60_000

const holdsTheLease = ($: EngineInterface, busy: boolean) => lease($, 'take', busy)

// The look has ended, however it ended: the lease stays this copy's, no longer busy.
async function leaseIdle($: EngineInterface): Promise<void> {
  await lease($, 'release')
}

// One look, when the chapter allows it: a fork of this session asking for one "what if...?". `base` is
// the chapter step that started it, `model` the model that answered that step.
//
// `trigger` says what started it: a step of the main session's own schedule (the chapter's), or the completion of agents
// (their identities, claimed by the caller before this ran). A completion's look is one chance for each of those
// completions: however it ends, they are settled here (looked, skipped or failed), and none is offered again.
async function dream($: EngineInterface, lastStep: number, base: number, model?: string, trigger: DreamTrigger = MAIN): Promise<void> {
  const trace: Trace = { forked: false, threw: false }
  try {
    await dreamOnce($, lastStep, base, model, trigger, trace)
  } finally {
    if (trigger.kind === 'agent') await settled($, trigger.completions, trace.threw ? 'failed' : trace.forked ? 'looked' : 'skipped').catch(() => undefined)
  }
}

async function dreamOnce($: EngineInterface, lastStep: number, base: number, model: string | undefined, trigger: DreamTrigger, trace: Trace): Promise<void> {
  // Another copy of this plugin dreams in this session: this one stays quiet, and asks again in a few steps.
  if (!(await holdsTheLease($, true))) return trigger.kind === 'main' ? void (await changeChapter($, soon(base))) : undefined
  let held = true
  let renewing = Promise.resolve(true)
  const owns = () => {
    renewing = renewing.then(async () => {
      if (held) {
        held = await lease($, 'renew').catch(() => false)
        if (!held) timer.cancel()
      }
      return held
    })
    return renewing
  }
  const timer = $.clock.every(LEASE_RENEW_MS, () => { void owns() })
  try {
    await look($, lastStep, base, model, owns, () => held, trigger, trace)
  } finally {
    timer.cancel()
    await renewing
    await leaseIdle($).catch(() => undefined)
  }
}

async function look($: EngineInterface, lastStep: number, base: number, model?: string,
  owns: () => Promise<boolean> = async () => true, held: () => boolean = () => true, trigger: DreamTrigger = MAIN,
  trace: Trace = { forked: false, threw: false }): Promise<void> {
  const agent = trigger.kind === 'agent'
  const ownChapter = (change: (chapter: Chapter) => Chapter) => changeChapter($, change, held)
  const ownPace = (change: (pace: Pace) => Pace) => changePace($, change, held)
  const cwd = await $.session.cwd()
  const session = await $.session.id()
  const began = await $.clock.now()
  const usual = await usualSteps($)
  let pace = await paceOf($)
  // What every look record carries besides its outcome: the spend check's two sides, how long the look took
  // (brief and fork included), and, when the fork came back without text, why.
  let reason: string | undefined
  const more = async () => ({
    ...sidesOf(pace, lastStep), took_ms: (await $.clock.now()) - began, ...(reason ? { reason } : {}),
    ...(agent ? { trigger: 'agent', completions: trigger.completions.length } : {}),
  })
  // The main schedule is the main session's: a look a completion started leaves the chapter's next-look step alone,
  // whatever it came to (it moves only when the look itself used one of the chapter's dreams).
  const soonish = (at: number) => (agent ? (chapter: Chapter) => chapter : soon(at))
  const spreadout = (at: number, steps: number) => (agent ? kept : spread(at, steps))
  const opening = await chapterOf($)
  if (!mayLook(opening)) {
    // The chapter's dreams are spent: no look until the next compaction starts a new one.
    return lookCameTo($, owns, cwd, session, 'spent', await ownChapter(spreadout(base, usual)), base, 0, undefined, await more())
  }
  const skip = await count($, await ownKey($, 'looksToSkip'))
  if (skip > 0) {
    const key = await ownKey($, 'looksToSkip')
    if (!(await owns())) return
    await $.store.set(key, skip - 1)
    return lookCameTo($, owns, cwd, session, 'skip', await ownChapter(spreadout(base, usual)), base, 0, undefined, await more())
  }
  if (await isBusy($)) return void (await ownChapter(soonish(base))) // a note is up: no question would be seen
  if (!canAfford(pace, lastStep)) return lookCameTo($, owns, cwd, session, 'budget', await ownChapter(soonish(base)), base, 0, undefined, await more())
  const full = (await run($, ['--repo', cwd, '--brief', '--sid', session])) ?? ''
  // The brief carries the agents as a machine envelope (bin/agents.py). It is read for who is running and which of the
  // claimed runs reported, and kept out of what the dreamer reads as CSR's memory: the prompt gets it as its own section.
  const envelope = parseAgentEnvelope(full)
  const brief = stripEnvelopes(full)
  const reported = agent ? completionNews(envelope, await ledgerOf($, session).catch(() => ({})), trigger.completions) : []
  const now = facts(brief)
  const before = await strings($, await ownKey($, 'briefFacts'))
  // A chapter's first look is always taken: after a compaction the work has moved on even when CSR's notes
  // have not, and the feed must never wait on them forever. Later looks need a new note.
  // A completion's look gets no such exception: it needs news like any other. The completions it was started for are
  // news in themselves (an agent's report that is not yet in a dream), and nothing else is.
  const fresh = !agent && opening.looks === 0
  const news = hasNews(now, before) || reported.length > 0
  if (!fresh && !news) return lookCameTo($, owns, cwd, session, 'quiet', await ownChapter(soonish(base)), base, 0, undefined, await more())
  const seen = await strings($, 'seenDreams')
  const known = await strings($, 'knownDreams')
  const asked = await strings($, await ownKey($, 'asked'))
  if (!(await owns())) return
  // A fork takes no token ceiling (ModelForkRequest is a prompt and nothing else), so the check above is a guess and
  // one look can cost more. What it did cost is written down the moment the fork returns, before anything asks whether
  // this look is still wanted (a lost lease, a compaction): money spent is spent. Owed past the share, it holds back
  // every look after it (pace.ts, owed). A fork that threw may have been paid for and says nothing of what: it is
  // charged as the guess.
  let reply: ModelForkResult
  try {
    trace.forked = true
    reply = await $.model.fork({ prompt: dreamPrompt(brief, seen, known, asked, agentsText(envelope, reported)) })
  } catch (error) {
    trace.threw = true
    await changePace($, current => ({ ...current, looks: current.looks + 1, dreamer: current.dreamer + guess(current, lastStep) }))
    throw error
  }
  const used = 'usage' in reply ? tokensOf(reply.usage) : 0
  pace = await changePace($, current => ({ ...current, looks: current.looks + 1, dreamer: current.dreamer + used, lastLook: used || current.lastLook }))
  if (!(await owns())) return
  if (!reply.isAnswered) reason = reply.reason
  const spent: Spent | undefined = 'usage' in reply
    ? { usage: reply.usage, model: model ?? (await $.session.model().catch(() => undefined)) }
    : undefined
  const briefKey = await ownKey($, 'briefFacts')
  if (!(await owns())) return
  await $.store.set(briefKey, [...before.filter(line => !now.includes(line)), ...now].slice(-FACTS_KEPT))
  // A compaction that started a new chapter while the fork ran: this answer was about the old one, whose
  // context is gone. Its tokens count; the new chapter is left as the compaction made it.
  const same = (chapter: Chapter) =>
    chapter.started === opening.started && chapter.session === opening.session && chapter.gen === opening.gen
  const ours = (change: (chapter: Chapter) => Chapter) => (current: Chapter) => (same(current) ? change(current) : current)
  if (!same(await chapterOf($))) {
    return lookCameTo($, owns, cwd, session, 'stale', await chapterOf($), base, used, spent, await more())
  }
  // No grounding gate: one kept out the only dream acted on. The prompt asks for a receipt; you judge.
  const found = reply.isAnswered ? parseDream(reply.text) : null
  const repeat = !!found && [...seen, ...known].some(entry => nearDream(found, entry))
  // Another way to what your last three prompts decided, or a reason that is only a line older than 30 days:
  // dropped, whatever else it says (dreamer.ts).
  const decided = found && !repeat ? offersAnotherWay(found, asked) : null
  const stale = !!found && !repeat && !decided && leansOnOld(found, brief, session)
  if (!found || repeat || decided || stale) {
    const chapter = await ownChapter(ours(current => spreadout(base, usual)({ ...current, looks: current.looks + 1 })))
    const outcome = repeat ? 'repeat' : decided ? 'decided' : stale ? 'stale' : 'none'
    return lookCameTo($, owns, cwd, session, outcome, chapter, base, used, spent, { ...(await more()), ...(decided ? { against: decided } : {}) })
  }
  const text = markReceipts(found.text, brief, session, await missingCommits($, cwd, citedCommits(found.text)))
  const at = await $.clock.now()
  const note: Note = {
    id: dreamId(found.line, at),
    kind: 'dream',
    file: '',
    line: found.line,
    title: found.title,
    text,
    sketch: [],
    symbol: null,
    oid: null,
    conversation: session,
    at,
    tokens: used,
  }
  if (!(await owns())) return
  const isShown = await show($, note, 'Dream', owns, held)
  if (!held()) return
  pace = await ownPace(current => ({ ...current, dreams: current.dreams + 1 }))
  const chapter = await ownChapter(ours(current =>
    spreadout(base, usual)({ ...current, looks: current.looks + 1, shown: current.shown + (isShown ? 1 : 0) })))
  if (!isShown) return
  if (!(await owns())) return
  await remember($, 'seenDreams', seenEntry(found), SEEN_KEPT, owns)
  if (!held()) return
  const record = JSON.stringify({
    kind: 'dream', id: note.id, at: at / 1000, session, line: note.line, title: note.title, text: note.text, tokens: used,
    ...spentFields(spent),
    outcome: 'dream', every: Math.max(0, chapter.due - base), left: dreamsLeft(chapter), of: allowance(chapter), ...(await more()),
    brief, // what the dreamer read: with the answer that settles this dream, one labelled example
  })
  if (!(await owns())) return
  await run($, ['--repo', cwd, '--keep', record])
  void refresh($, cwd).catch(() => undefined)
}

// Coaching: a word you have said in 2 sessions that Claude looked up each time, offered once as a band note
// (bin/pane.py --coach works it out off the hot path). Nothing is written until you press 1.
async function coachNote($: EngineInterface, cwd: string): Promise<void> {
  const notes = await json<Note[]>($, ['--repo', cwd, '--coach'])
  const skip = new Set(await strings($, 'coached'))
  const note = notes?.find(candidate => candidate.kind === 'term' && !skip.has(candidate.id))
  if (note && (await show($, note, 'Coaching'))) await remember($, 'coached', note.id)
}

// One line under "## Terms" of the global CLAUDE.md, written only when the person pressed the key.
async function addToClaudeMd($: EngineInterface, line: string): Promise<boolean> {
  const home = await $.env.get('HOME')
  if (!home) return false
  const file = `${home}/.claude/CLAUDE.md`
  // Read it first: a file that is there but could not be read is left alone, never written over.
  const text = (await $.fs.exists(file).catch(() => null)) === false ? '' : await $.fs.read(file).catch(() => null)
  if (text === null) return false
  return $.fs.write(file, addTerm(text, line)).then(() => true, () => false) // user file: the person's own CLAUDE.md
}

async function dreamsText($: EngineInterface, on: boolean): Promise<string> {
  return paceText(on ? await paceOf($) : null, on ? await chapterOf($) : null, on)
}

// A /config row of this plugin's, found by its plugin's prefix: a folder-loaded plugin may carry "@inline".
async function configRow($: EngineInterface, name: string): Promise<string> {
  const rows = await $.config.list().catch(() => [])
  const pattern = new RegExp(`^csr-companion(@[^.]+)?\\.${name}$`)
  return rows.find(row => pattern.test(row.key))?.key ?? `csr-companion.${name}`
}

async function dreamsRow($: EngineInterface): Promise<string> {
  return (await configRow($, 'dreams')) || OFF_ROW
}

// Whether a band holds a Button with a hotkey: it is asking the person something.
function asks(node: unknown): boolean {
  if (!node || typeof node !== 'object') return false
  const element = node as { type?: string; props?: { hotkey?: unknown }; children?: unknown[] }
  if (element.type === 'Button' && element.props?.hotkey) return true
  return (element.children ?? []).some(asks)
}

// A person at the prompt. A `claude -p` child that loads your user settings loads this plugin too, and must
// never count steps, start chapters or look. Set at session.start; a module loaded later asks the session
// where it draws (nowhere, in a -p run). One process, one module: this is never shared.
let person: boolean | undefined
async function isPerson($: EngineInterface): Promise<boolean> {
  if (person === undefined) person = (await $.session.surfaces().catch(() => [])).length > 0
  return person
}

// The per-session keys of all but the newest sessions go, and the unkeyed ones every process once shared.
async function sweepSessions($: EngineInterface): Promise<void> {
  const keys = await $.store.keys()
  for (const key of PER_SESSION) if (keys.includes(key)) await $.store.delete(key)
  const started = new Map<string, number>()
  for (const key of keys) {
    const found = /^chapter:(.+)$/.exec(key)
    if (!found?.[1]) continue
    const chapter = (await $.store.get(key).catch(() => undefined)) as Chapter | undefined
    started.set(found[1], chapter?.started ?? 0)
  }
  // Only a session whose chapter is known and past the newest SESSIONS_KEPT goes: one with no chapter yet may be live
  // in another window, its first look not taken (Codex review #9 lost its asked and pace keys).
  const gone = new Set([...started].sort((a, b) => b[1] - a[1]).slice(SESSIONS_KEPT).map(([session]) => session))
  gone.delete(await $.session.id())
  for (const key of keys) {
    const found = /^(pace|chapter|briefFacts|looksToSkip|typedPastInARow|asked|sentDream|agentLedger):(.+)$/.exec(key)
    if (found?.[2] && gone.has(found[2])) await $.store.delete(key)
  }
}

// You should know's looks pass this plugin's model.fork hook (dreamer.ts, "You should know reads CSR"). Its
// propose fork gets CSR's code, rule, correction and past-session lines; every fork it makes is kept in the
// dream log as a "ysk" record, the only local count of its looks, tokens and lines (it keeps no counters of its own).
// CSR_NO_YSK_BRIEF=1 keeps the record and leaves its prompt alone. One brief per session per 5 minutes: it
// forks at turn steps 6, 12, ... of a long turn (binary @222888688), and each brief runs pane.py.
const YSK_BRIEF_MS = 5 * 60 * 1000
const yskBriefs = new Map<string, { at: number; facts: string }>()
async function yskFactsFor($: EngineInterface, cwd: string, session: string): Promise<string> {
  const now = await $.clock.now()
  const held = yskBriefs.get(session)
  if (held && now - held.at < YSK_BRIEF_MS) return held.facts
  const facts = yskFacts((await run($, ['--repo', cwd, '--brief', '--sid', session]).catch(() => null)) ?? '')
  yskBriefs.set(session, { at: now, facts })
  return facts
}

// ---------------------------------------------------------------- compact at the right moment

// Infinite sessions, auto-managed for cache payback: CSR compacts at the moment the owner would, so past
// exchanges stop being re-sent. The rules are ./compact.ts; the payback math is bin/pane.py's.
//
// At each main-loop turn end in the owner's band, a plan row in the pane says what CSR will do and what it
// waits for. At a seam (nothing running in the background, no question waiting) it acts:
//  - attended (a prompt typed at the terminal in the last 10 minutes): a 20 s countdown above the prompt,
//    1 compacts now, 0 cancels, and typing cancels;
//  - unattended (the last prompt came from the phone, or nobody typed for 10 minutes): it compacts at once,
//    and leaves one line the phone shows;
//  - suggest: the band offers it, and only a press compacts.
// At or past the band's high edge it acts at the next turn end, seam or not. Never while a turn runs, never in
// a subagent, a fork or a `claude -p` child.

const compactView = atom({ plugin: 'csr-companion', key: 'compact' } as const, null as CompactView | null)
const infinite = atom({ plugin: 'csr-companion', key: 'infinite' } as const, null as InfiniteRow | null)

// ---------------------------------------------------------------- the first run's walkthrough (./setup.ts)

// Each setting a person should choose, a step at a time above the prompt, at the session start after the
// plugin is added. An answer is kept across sessions, per step, so a step added later shows alone. Typing
// sets it aside for the session; unanswered, it shows at the next start, at 2 starts in all, then never.
// /csr setup brings every step back.
const setupView = atom({ plugin: 'csr-companion', key: 'setup' } as const, null as SetupView | null)

const nextStep = (shown: SetupView | null): StepId | null =>
  !shown || shown.aside ? null : (shown.steps.find(step => !shown.done.includes(step)) ?? null)

// At a session start: the steps due here, each counted as shown at this start. A reload of the module (a
// setting just written reloads it) starts the session again: the walkthrough keeps its place.
async function startSetup($: EngineInterface): Promise<void> {
  if (await read($, setupView)) return
  const session = await $.session.id()
  const answered = new Set<StepId>()
  const seen: Partial<Record<StepId, string[]>> = {}
  for (const step of STEPS) {
    if ((await $.store.get(`setup:answer:${step}`).catch(() => undefined)) !== undefined) answered.add(step)
    seen[step] = await strings($, `setup:seen:${step}`)
  }
  const steps = dueSteps(answered, seen, session)
  for (const step of steps) {
    const starts = seen[step] ?? []
    if (!starts.includes(session)) await $.store.set(`setup:seen:${step}`, [...starts, session])
  }
  await update($, setupView, () => ({ steps, done: [], aside: false }))
}

// Typed past: set aside for this session.
async function setAside($: EngineInterface): Promise<void> {
  if (nextStep(await read($, setupView))) await update($, setupView, shown => (shown ? { ...shown, aside: true, message: undefined } : shown))
}

// An answer is kept first, then written to /config: a write that changes the row reloads this module. current is
// the step's value as the module was loaded with it.
async function answerStep($: EngineInterface, step: StepId, value: boolean | string, current: boolean | string): Promise<void> {
  await $.store.set(`setup:answer:${step}`, { value, at: await $.clock.now() })
  await update($, setupView, shown => (shown ? { ...shown, done: [...shown.done, step], message: undefined } : shown))
  if (value === current) return
  const done = await $.config.set({ key: await configRow($, step), value }).catch(() => ({ deny: 'failed' }))
  if (!('deny' in done) || !done.deny) return
  const message = refusedText('csr-companion', step)
  await update($, setupView, shown => (shown ? { ...shown, message } : shown))
  if (!nextStep(await read($, setupView))) $.ui.toast(message) // the last step: no band left to say it on
}

// One step above the prompt: what it does, the choices on keys, the one in force marked.
async function setupBand($: EngineInterface, e: RenderInput, step: StepId, current: boolean | string): Promise<RenderElement> {
  const { Box, Button, Text } = $.ui.resolve(e)
  const [first, ...rest] = LINES[step]
  const message = (await read($, setupView))?.message
  return (
    <Box flexDirection="column" marginTop={1}>
      <Box flexDirection="row" alignItems="flex-start">
        <Box flexShrink={0} width={2}>
          <Text color="suggestion">{STAR}</Text>
        </Box>
        <Text wrap="wrap">
          <Text dimColor>{heading(step)} · </Text>
          {first}
        </Text>
      </Box>
      {rest.map(line => (
        <Box marginLeft={2}>
          <Text dimColor wrap="wrap">{line}</Text>
        </Box>
      ))}
      {message ? <Box marginLeft={2}><Text color="warning" wrap="wrap">{message}</Text></Box> : null}
      <Box marginLeft={2} flexWrap="wrap">
        {CHOICES[step].map(choice => (
          <Box marginRight={3}>
            <Button
              key={`setup-${step}-${choice.hotkey}`}
              hotkey={choice.hotkey}
              plain
              label={choice.value === current ? `${choice.label} (now)` : choice.label}
              onPress={() => void answerStep($, step, choice.value, current)}
            />
          </Box>
        ))}
        <Text dimColor>Typing skips it; /csr setup brings it back.</Text>
      </Box>
    </Box>
  )
}

type PlanJson = { ctx?: number; floor?: number; measured?: boolean; payback?: number | null; per_turn?: number | null; worth?: boolean; story?: { text?: string } | null }
type Armed = { ctx: number; payback: Payback | null; seam: Seam; mode: Mode; forced: boolean; floor: number | null; answer: string }

const PLANS = '.claude-self-reflect/csr-companion/compact-plan' // under HOME: <session>.json, the herdr pane draws it
const BACKGROUND = new Set(['Agent', 'Workflow'])
const EDITS = new Set(['Edit', 'Write', 'NotebookEdit'])
const TN_TOOL = /<tool-use-id>([^<]+)<\/tool-use-id>/
const TN_TASK = /<task-id>([^<]+)<\/task-id>/
const TN_DONE = /<status>(completed|failed|killed|stopped|cancelled|error)<\/status>/
const COMMIT_OUT = /^\[[^\]\n]*?\s([0-9a-f]{7,40})\]/m // `git commit`'s own "[branch 6369e6b] subject"
const RULES_KEPT = 3

// One process, one module, one session: what the hooks learn between turn ends.
const CS = {
  setting: 'auto' as Setting,
  planVersion: 0, // one more when an operation starts
  settingStamp: 0, // one more at each /config change of the setting: an automatic compaction acts on the one it started under
  loadedAt: null as number | null,
  last: null as Human | null, // the last prompt a person sent: where from, and when
  stopped: null as { at: number; running: Running[] } | null, // background work as the last main Stop listed it
  launched: new Map<string, Running>(), // background work seen launched and not yet reported done
  turnAt: 0, // when the running turn started
  turnRunning: false,
  asking: 0, // AskUserQuestion calls still open
  edits: 0,
  commit: undefined as string | undefined,
  gate: false,
  cancelledAt: null as number | null, // the context at the owner's last cancel, until the context leaves the band
  armed: null as Armed | null, // what a countdown or an offer would compact with
  timers: [] as Timer[],
  compacting: false,
  rules: [] as string[], // the owner's binding sentences this session
  wrote: new Set<string>(), // the files this session's Edit and Write calls wrote, its subagents' too
  committedIn: new Set<string>(), // the folders the main thread committed in
  theirs: new Map<string, string>(), // background tasks a subagent started: task id -> the subagent
}

const taskOwners = atom({ plugin: 'csr-companion', key: 'taskOwners' } as const, {} as TaskOwners)

// ---------------------------------------------------------------- the agents of this session (./agents.ts)

// What the typed events say of the session's agents: their lifecycle, files, completions. Kept in the host's state,
// so a reload of the module keeps it, and keyed by the session id on every entry point (a /clear starts it over).
// These observers change nothing: every one lets `next` answer first or alone, and a failure in one is swallowed
// where it happens, so it can never fail a call or run a tool twice.
const agentSession = atom({ plugin: 'csr-companion', key: 'agentSession' } as const, emptyAgentState(''))
const ownAgents = (state: AgentState, session: string): AgentState => (state.session === session ? state : emptyAgentState(session))

async function agentsChange($: EngineInterface, change: (state: AgentState) => AgentState): Promise<void> {
  const session = await $.session.id()
  await update($, agentSession, current => change(ownAgents(current, session)))
}

async function track($: EngineInterface, event: AgentEvent): Promise<void> {
  try {
    let moved = false
    await agentsChange($, state => {
      const next = reduceAgentEvent(state, event)
      moved = next.handoffs !== state.handoffs
      return next
    })
    if (moved) void writeHandoffs($).catch(() => undefined)
    void scheduleAgents($)
  } catch {}
}

// $.agent.list() into the state: once at the start, after a main turn, and every minute while any agent is running.
async function pollAgents($: EngineInterface): Promise<void> {
  try {
    const list = await $.agent.list()
    const at = await $.clock.now()
    let moved = false
    await agentsChange($, state => {
      const next = reconcileAgentList(state, list, at)
      moved = next.handoffs !== state.handoffs
      return next
    })
    if (moved) void writeHandoffs($).catch(() => undefined)
    void scheduleAgents($)
  } catch {}
}

// The agents file, <HOME>/.claude-self-reflect/csr-companion/agents/<session>.json, what the pane draws In flight from:
// a bounded projection of the state (agents.ts, projectAgentsFile), written through bin/private.py as JSON (0600, its
// strings redacted again). The session id is checked before it is made a file name.
const AGENTS_DIR = '.claude-self-reflect/csr-companion/agents' // under HOME
const SESSION_NAME = /^[A-Za-z0-9_-]{1,128}$/

async function writeAgents($: EngineInterface): Promise<void> {
  const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
  const session = await $.session.id()
  if (!home || !SESSION_NAME.test(session)) return
  const file = projectAgentsFile(ownAgents(await read($, agentSession), session), await $.clock.now())
  if (file.agents.length) await keepWrite($, `${home}/${AGENTS_DIR}/${session}.json`, JSON.stringify(file), 'json')
}

// What became of the requests to hand a Lost and found thread to a background agent (agents.ts, associateHandoff), where the
// pane reads it: handoffs/<session>.json beside the pane's own drafts of them. Ids and words, no prompt and no report.
const HANDOFFS_DIR = '.claude-self-reflect/csr-companion/handoffs' // under HOME

async function writeHandoffs($: EngineInterface): Promise<void> {
  const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
  const session = await $.session.id()
  if (!home || !SESSION_NAME.test(session)) return
  const { handoffs } = ownAgents(await read($, agentSession), session)
  if (handoffs.length) await keepWrite($, `${home}/${HANDOFFS_DIR}/${session}.json`, JSON.stringify({ version: 1, session, handoffs }), 'json')
}

// A prompt that entered, and carried the envelope of a request the pane drafted: that request was sent. Only the text that
// entered counts (a prompt a hook dropped did not go), and only a request the pane drafted in this session.
async function handoffSent($: EngineInterface, entered: { text?: string; drop?: unknown }): Promise<void> {
  try {
    if (entered.drop !== undefined || !entered.text?.includes('[[CSR:AGENTS:v1]]')) return
    const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
    const session = await $.session.id()
    const asked = handoffRequests(entered.text, session)
    if (!home || !SESSION_NAME.test(session) || !asked.length) return
    const drafts = await $.fs.read(`${home}/${HANDOFFS_DIR}/${session}.drafts.jsonl`).catch(() => '')
    for (const { request, thread } of asked) {
      for (const line of drafts.split('\n')) {
        let draft: { request?: unknown; thread?: unknown; drafted_at?: unknown } = {}
        try {
          draft = JSON.parse(line)
        } catch {
          continue
        }
        if (draft.request === request && draft.thread === thread && typeof draft.drafted_at === 'number') {
          await track($, { kind: 'handoff', at: await $.clock.now(), request, thread, draftedAt: draft.drafted_at })
          break
        }
      }
    }
  } catch {}
}

// At most one write in five seconds. The first goes at once; a change inside the interval sets one timer for its end
// and the writes it covers are one (the state says when it is due, so a reload that lost the timer sets it again).
async function scheduleAgents($: EngineInterface): Promise<void> {
  try {
    const now = await $.clock.now()
    let plan = { act: 'none', wait: 0 } as Pick<WritePlan, 'act' | 'wait'>
    await agentsChange($, state => {
      const decided = planWrite(state, now)
      plan = decided
      return decided.state
    })
    if (plan.act === 'now') await writeAgents($)
    else if (plan.act === 'later') $.clock.after(plan.wait, () => void scheduleAgents($))
  } catch {}
}

// The minute poll, from the session start (the API fires it on every fresh module load, a reload included, and the host
// cancels the old module's timers): it asks $.agent.list() only while an agent is pending, running or waiting.
function watchAgents($: EngineInterface): void {
  $.clock.every(LIST_EVERY_MS, () => {
    void (async () => {
      if (anyActive(ownAgents(await read($, agentSession), await $.session.id()))) await pollAgents($)
    })().catch(() => undefined)
  })
}

// ---------------------------------------------------------------- completions: the ledger, and the claim

// Each agent's finished run is offered one look. The store's ledger (agents.ts, Ledger) is what says which were taken:
// identifiers and words only, by session, never the report. A completion enters it pending; a look claims it before
// it checks anything; whatever the look comes to, the claim is settled and the completion is not offered again.
// If the ledger cannot be written, or is full, no completion look is started (nothing is evicted to make room).
const ledgerKey = (session: string) => keyOf('agentLedger', session)
const DISPOSITIONS = new Set<string>(['pending', 'claimed', 'looked', 'skipped', 'failed'])

async function ledgerOf($: EngineInterface, session: string): Promise<Ledger> {
  const value = await $.store.get(ledgerKey(session))
  if (value === undefined) return {}
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('the completion ledger is not a ledger')
  return Object.fromEntries(Object.entries(value as Record<string, unknown>).filter(([, word]) => typeof word === 'string' && DISPOSITIONS.has(word))) as Ledger
}

// Changes to the ledger go one at a time: a claim and an admission read, change and write the same record.
let ledgerQueue: Promise<unknown> = Promise.resolve()
function ledgerSerially<T>(work: () => Promise<T>): Promise<T> {
  const done = ledgerQueue.then(work)
  ledgerQueue = done.catch(() => undefined)
  return done
}

// A completion enters the ledger pending. True when it is new and the ledger now holds it; false for one seen before,
// a full ledger, or a store that would not take it.
async function admitted($: EngineInterface, key: CompletionKey): Promise<boolean> {
  try {
    return await ledgerSerially(async () => {
      const session = await $.session.id()
      const admission = admitCompletion(await ledgerOf($, session), key)
      if (!admission.fresh) return false
      await $.store.set(ledgerKey(session), admission.ledger)
      return true
    })
  } catch {
    return false
  }
}

// Every completion that is pending, claimed in one write and recorded before any gate of the look is checked. [] when
// there is none, or when the claim could not be kept (then nothing was claimed and no look is started).
async function takeCompletions($: EngineInterface): Promise<CompletionKey[]> {
  try {
    return await ledgerSerially(async () => {
      const session = await $.session.id()
      const ledger = await ledgerOf($, session)
      const due = pendingCompletions(ownAgents(await read($, agentSession), session), ledger)
      if (!due.length) return []
      const claim = claimCompletions(ledger, due.map(entry => entry.key))
      await $.store.set(ledgerKey(session), claim.ledger)
      await agentsChange($, current => markDisposition(current, claim.claimed, 'claimed'))
      return claim.claimed
    })
  } catch {
    return []
  }
}

// The look ended: the completions it claimed are looked, skipped or failed, in the ledger and in the state.
async function settled($: EngineInterface, keys: readonly CompletionKey[], word: 'looked' | 'skipped' | 'failed'): Promise<void> {
  await ledgerSerially(async () => {
    const session = await $.session.id()
    await $.store.set(ledgerKey(session), settleCompletions(await ledgerOf($, session), keys, word))
  })
  await agentsChange($, current => markDisposition(current, keys, word as CompletionDisposition))
}

const hasPending = async ($: EngineInterface): Promise<boolean> => {
  const session = await $.session.id()
  return pendingCompletions(ownAgents(await read($, agentSession), session), await ledgerOf($, session)).length > 0
}

// What the prompt says of the agents (dreamer.ts, AGENT_RULES) from the brief's envelope: the agents still running, as
// context only, and the reports of the runs this look was started for, each in the agent's own words under its name.
// Nothing else of the agents is read: a finished run no look claimed, a failed one, an empty one say nothing here.
function agentsText(envelope: Envelope | null, reported: readonly CompletionKey[]): string {
  if (!envelope || envelope.payload.kind !== 'context') return ''
  const { session, agents } = envelope.payload
  const running = agents.filter(row => row.status === 'running' || row.status === 'waiting' || row.status === 'pending')
  const lines = running.length ? [`Still at work, for context only (nothing to ask about yet): ${running.map(row => `${row.type ?? 'an agent'} (${row.status})`).join(', ')}.`] : []
  for (const row of agents) {
    if (!row.completion || !reported.includes(completionKey(session, row.id, row.completion.turn_id))) continue
    const quoted = row.completion.answer.split('\n').map(line => `  > ${line}`).join('\n')
    lines.push(`- ${row.type ?? 'An agent'} finished, and reported:\n${quoted}`)
  }
  return lines.join('\n')
}

async function restoreOwners($: EngineInterface): Promise<void> {
  const owners = await read($, taskOwners)
  for (const [id, agent] of Object.entries(owners)) CS.theirs.set(id, agent)
}

async function ownTask($: EngineInterface, id: string, agent: string): Promise<void> {
  CS.theirs.set(id, agent)
  await update($, taskOwners, current => ({ ...current, [id]: agent }))
}

// Background work in flight: the Stop's own list when it came during this turn, else what the hooks saw
// launched and not reported done.
const running = (): Running[] => (CS.stopped && CS.stopped.at >= CS.turnAt ? CS.stopped.running : [...CS.launched.values()])

// The payback as the log and the plan file keep it, flat: requests, and the session's requests a turn.
const paybackFields = (payback: Payback | null) => ({ payback: payback?.requests ?? null, per_turn: payback?.perTurn ?? null })

const seamFields = (seam: Seam) => ({
  seam: {
    is: seam.isSeam, why: seam.why, question: seam.question, wait: seam.wait ?? null,
    running: seam.running ?? 0, commit: seam.commit ?? null, gate: seam.gate ?? false, edits: seam.edits ?? 0,
  },
})

// bin/pane.py, within a time limit: a run that overruns it, or fails, reads as no answer.
async function paneWithin($: EngineInterface, argv: string[], timeoutMs = 20_000): Promise<string | null> {
  const ran = await $.process.run(['python3', '-I', `${$.plugin.root}/bin/pane.py`, ...argv], { timeoutMs }).catch(() => null)
  return ran && ran.exitCode === 0 ? ran.stdout : null
}

// Every plan, countdown, cancel, compaction and skip, in the dream log as kind "compact": the pane reads it back.
async function logCompact($: EngineInterface, event: string, fields: Record<string, unknown>): Promise<void> {
  const at = (await $.clock.now()) / 1000
  await paneWithin($, ['--repo', await $.session.cwd(), '--keep', JSON.stringify({
    kind: 'compact', event, at, session: await $.session.id(), setting: CS.setting, ...fields,
  })])
}

type PlanOp = { stamp: number; sequence: number }
const planOp = (stamp = CS.settingStamp): PlanOp => ({ stamp, sequence: ++CS.planVersion })
const currentPlan = (op: PlanOp) => op.stamp === CS.settingStamp && op.sequence === CS.planVersion
let planQueue: Promise<void> = Promise.resolve()

// Only the latest operation publishes. Writes finish in order, including cancellation.
async function showPlan($: EngineInterface, state: string, text: string | null, extra: Record<string, unknown> = {},
  op: PlanOp = planOp()): Promise<void> {
  const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
  const path = home ? `${home}/${PLANS}/${await $.session.id()}.json` : null
  const at = (await $.clock.now()) / 1000
  const done = planQueue.then(async () => {
    if (!currentPlan(op)) return
    await update($, infinite, current => (currentPlan(op) ? { plan: text, story: current?.story ?? null } : current))
    if (path && currentPlan(op)) await keepWrite($, path, JSON.stringify({ at, state, text, ...extra }), 'json').catch(() => undefined)
  })
  planQueue = done.catch(() => undefined)
  await done
}

// A countdown or an offer ends, however it ends: the band clears, the plan stays.
async function disarm($: EngineInterface, op?: PlanOp): Promise<void> {
  if (op && !currentPlan(op)) return
  for (const timer of CS.timers) timer.cancel()
  CS.timers = []
  CS.armed = null
  await update($, compactView, current => (!op || currentPlan(op) ? null : current))
  $.ui.invalidate('ui.render')
}

// The owner said not now (0, typing, a prompt): the next seam with 20k more context.
async function cancelCompact($: EngineInterface, by: string): Promise<void> {
  const was = CS.armed
  if (!was || CS.compacting) return
  const op = planOp()
  await disarm($, op)
  if (!currentPlan(op)) return
  CS.cancelledAt = was.ctx
  await showPlan($, 'planned', planText(was.ctx, was.payback, `cancelled · waits for ${k(was.ctx + REARM_TOKENS)}`, was.forced), {}, op)
  await logCompact($, 'cancel', { by, mode: was.mode, ctx_before: was.ctx, ...paybackFields(was.payback), ...seamFields(was.seam) })
}

// 1 on the band: compact now, between turns.
function pressCompact($: EngineInterface): void {
  const at = CS.armed
  if (at && !CS.turnRunning) void compactNow($, at, 'press').catch(() => undefined)
}

async function gitOut($: EngineInterface, cwd: string, args: string[]): Promise<string | null> {
  const ran = await $.process.run(['git', '-C', cwd, ...args], { timeoutMs: 2000 }).catch(() => null)
  return ran && ran.exitCode === 0 ? ran.stdout.replace(/\s+$/, '') : null // a porcelain line starts with a space
}

// What the summary must keep: background work with ids, open decisions, unfinished work with SHAs, rules.
// Running: the main thread's own tasks; a shell a subagent started is that subagent's, which is kept itself (10-13
// 04:50: the keep list named three subagent shells as running, where Claude Code's own task list showed one).
// The open decision is the question this turn ended on. Lost and found's threads stay out, as in prep-compact:
// a later summary dropped them, and 10-12 20:53 the keep list brought back three from 10-11 and 10-12 morning,
// one waiting on an agent long gone. The threads you set aside for later reach the summary by the
// session.compact hook, which a compaction of CSR's runs too.
async function keepOf($: EngineInterface, answer: string): Promise<Keep> {
  const cwd = await $.session.cwd()
  const asked = lastQuestion(answer)
  const decisions = asked ? [asked] : []
  const work = await workOf($, cwd)
  return { running: running().filter(task => !CS.theirs.has(task.id)), decisions, work, rules: CS.rules.slice(-RULES_KEPT) }
}

// Unfinished work that is this session's: uncommitted changes to files it wrote (its Edit and Write calls), or in a
// repository its main thread committed in, and that repository's commits not pushed. Another session's changes in
// the same checkout are not this one's (10-13 04:50: the keep list carried "7 uncommitted files (src/engine.rs,
// mcp/mod.rs, .agents/, ...)" this session never touched). Nothing of its own: no section.
async function workOf($: EngineInterface, cwd: string): Promise<string[]> {
  const top = await gitOut($, cwd, ['rev-parse', '--show-toplevel'])
  if (!top) return []
  const tops = await Promise.all([...CS.committedIn].map(dir => gitOut($, dir, ['rev-parse', '--show-toplevel'])))
  const committed = tops.includes(top)
  const wrote = (line: string) => {
    const path = `${top}/${porcelainPath(line)}`
    return path.endsWith('/') ? [...CS.wrote].some(file => file.startsWith(path)) : CS.wrote.has(path)
  }
  const work: string[] = []
  const dirty = (await gitOut($, cwd, ['status', '--porcelain']))?.split('\n').filter(Boolean).filter(line => committed || wrote(line)) ?? []
  if (dirty.length) {
    const names = dirty.slice(0, 3).map(line => porcelainPath(line).split('/').filter(Boolean).slice(-2).join('/') + (line.endsWith('/') ? '/' : ''))
    work.push(`${dirty.length} uncommitted ${dirty.length === 1 ? 'file' : 'files'} (${names.join(', ')}${dirty.length > 3 ? ', …' : ''})`)
  }
  const ahead = committed ? (await gitOut($, cwd, ['log', '--format=%h', '@{u}..HEAD']))?.split('\n').filter(Boolean) ?? [] : []
  if (ahead.length) work.push(`not pushed: ${ahead.slice(0, 4).join(', ')}${ahead.length > 4 ? ` and ${ahead.length - 4} more` : ''}`)
  if (work.length) {
    const head = await gitOut($, cwd, ['rev-parse', '--short', 'HEAD'])
    const branch = await gitOut($, cwd, ['symbolic-ref', '-q', '--short', 'HEAD'])
    if (head) work.unshift(`${branch ? `branch ${branch} at` : 'HEAD'} ${head}`)
  }
  return work
}

// A `git status --porcelain` line's path, from the repository's top: the new name of a rename, quotes off.
const porcelainPath = (line: string) => (line.slice(3).split(' -> ').pop() ?? '').replace(/^"|"$/g, '')

// A background task's id in the result of the call that started it: a shell's "with ID: b...", an agent's
// "agentId: a...". The Stop hook's task list carries the same ids.
const TASK_ID = /\b(?:agentId|ID|task_id|taskId):\s*([A-Za-z0-9_-]{6,})/

// Resolve only a simple chain whose commit and directory are unambiguous.
function commitDir(command: string, cwd: string): string | null {
  const segments: string[][] = [[]]
  const tokens = /[^\S\n]+|\n|&&|\|\||;|'[^']*'|"[^"\\$`]*"|[^\s'";&|()<>\\$`]+/gy
  let end = 0
  for (const match of command.matchAll(tokens)) {
    end = match.index + match[0].length
    const word = match[0]
    if (/^[^\S\n]+$/.test(word)) continue
    if (word === '\n' || word === ';' || word === '&&' || word === '||') segments.push([])
    else segments[segments.length - 1]!.push(word.replace(/^['"]|['"]$/g, ''))
  }
  if (end !== command.length) return null
  const resolve = (base: string, dir: string, logical = true) => {
    if (dir.startsWith('~') || !dir) return null
    const path = dir.startsWith('/') ? dir : `${base}/${dir}`
    if (!logical) return path // Git follows links before resolving ..
    const parts: string[] = []
    for (const part of path.split('/')) {
      if (part === '..') parts.pop()
      else if (part && part !== '.') parts.push(part)
    }
    return `/${parts.join('/')}`
  }
  let directory = cwd
  let committed: string | null = null
  for (const words of segments) {
    if (!words.length) continue
    if (words[0] === 'cd') {
      if (words.length !== 2) return null
      const dir = resolve(directory, words[1]!)
      if (!dir) return null
      directory = dir
      continue
    }
    if (words[0] !== 'git') return null
    let dir = directory
    let i = 1
    while (words[i]?.startsWith('-')) {
      const option = words[i++]
      if (option === '-C') {
        const next = words[i++]
        const resolved = next && resolve(dir, next, false)
        if (!resolved) return null
        dir = resolved
      } else if (option === '-c') {
        if (!words[i++]) return null
      } else return null
    }
    if (words[i] === 'commit') {
      if (committed) return null
      committed = dir
    }
  }
  return committed
}

// How many threads wait on you in Lost and found (pane.py --lost, as prep-compact reads it), for the notice: 0
// when pane.py does not answer within two seconds.
async function lostWaiting($: EngineInterface): Promise<number> {
  const said = await paneWithin($, ['--repo', await $.session.cwd(), '--lost', '--sid', await $.session.id()], 2000)
  try {
    const threads: unknown = said ? JSON.parse(said) : []
    return Array.isArray(threads) ? waitingOn(threads as Thread[]) : 0
  } catch {
    return 0
  }
}

// One compaction by CSR: the plan row says so first, the summary gets the keep list, the phone gets one line.
// `stamp`: an automatic one's (unattended, a countdown run out), the setting's stamp when it started. The person may
// turn Auto off while it is prepared (the plan logged, the keep list read: Codex r2 #3, it compacted with the setting
// off): Auto, unchanged since, is asked for again right before the compaction, and anything else ends it unrun.
async function compactNow($: EngineInterface, at: Armed, by: string, stamp?: number, operation?: PlanOp): Promise<void> {
  if (CS.compacting) return
  const op = operation ?? planOp(stamp)
  const stillAuto = () => currentPlan(op) && (stamp === undefined || (CS.setting === 'auto' && CS.settingStamp === stamp))
  if (!stillAuto()) return void (await settingMoved($, at, by, op))
  CS.compacting = true
  try {
    await disarm($, op)
    const began = await $.clock.now()
    const why = actWhy(at.seam)
    await showPlan($, 'compacting', `Compacting · ${k(at.ctx)} · ${why}`, {}, op)
    const keep = await keepOf($, at.answer)
    const instructions = keepList(keep)
    // The pane's card streams from these: what it keeps while the summary runs, then the result.
    await logCompact($, 'start', { by, mode: at.mode, forced: at.forced, ctx_before: at.ctx, ...paybackFields(at.payback), why, keep: keepRows(keep) })
    if (!stillAuto()) return void (await settingMoved($, at, by, op))
    let outcome = 'compacted'
    let reason: string | null = null
    let after: number | null = null
    try {
      const result = await $.session.compact({ instructions })
      if (result.skip !== undefined) {
        outcome = 'skipped'
        reason = result.skip
      } else after = result.tokensAfter ?? null
    } catch (error) {
      outcome = 'failed' // a turn ran, or the summary failed
      reason = error instanceof Error ? error.message : String(error)
    }
    const kept = { running: keep.running.map(task => task.id), decisions: keep.decisions.length, work: keep.work.length, rules: keep.rules.length }
    const took = (await $.clock.now()) - began
    const waiting = outcome === 'compacted' ? await lostWaiting($) : 0 // read after the summary that decides lost or open
    await logCompact($, outcome === 'compacted' ? 'compact' : 'skip', {
      by, mode: at.mode, forced: at.forced, outcome, reason, ctx_before: at.ctx, ctx_after: after, floor: at.floor,
      ...paybackFields(at.payback), took_ms: took, instructions: instructions.length, kept, why, keep: keepRows(keep), ...seamFields(at.seam),
      ...(outcome === 'compacted' ? { lost_waiting: waiting } : {}),
    })
    if (outcome !== 'compacted') {
      await showPlan($, 'planned', planText(at.ctx, at.payback, `the compaction did not run (${reason})`, at.forced), {}, op)
      return
    }
    CS.cancelledAt = null
    await showPlan($, 'none', null, {}, op)
    // One line the phone shows: a notice, which the model never reads; its header keeps it out of CSR's memory.
    const line = noticeLine(at.ctx, at.floor ?? after, at.payback, keep, waiting)
    const posted = await $.session.append({ message: { type: 'system', content: [{ type: 'text', text: line }] } }).catch(() => null)
    if (!posted || posted.deny !== undefined) $.ui.log(line)
  } finally {
    CS.compacting = false
  }
}

// A setting change ends the old operation. Its skip is logged without changing a newer plan.
async function settingMoved($: EngineInterface, at: Armed, by: string, op: PlanOp): Promise<void> {
  await showPlan($, 'none', null, {}, op)
  await logCompact($, 'skip', { by, reason: `the setting changed to ${CS.setting}`, mode: at.mode, forced: at.forced, ctx_before: at.ctx,
    ...paybackFields(at.payback), ...seamFields(at.seam) })
}

// The countdown's timers: the compaction when it runs out, and the band's seconds redrawn.
function armTimers($: EngineInterface, at: Armed, ms: number): void {
  CS.timers = [
    $.clock.after(ms, () => {
      if (CS.armed === at && CS.setting === 'auto') void compactNow($, at, 'countdown', CS.settingStamp).catch(() => undefined) // auto alone
    }),
    $.clock.every(1000, () => $.ui.invalidate('ui.render')),
  ]
}

// After a reload: the countdown or the offer the module before this one left in the band (resumeOf).
async function resumeArmed($: EngineInterface): Promise<void> {
  if (CS.armed) return
  const op = planOp()
  const shown = await read($, compactView)
  const how = resumeOf(shown, await $.clock.now(), CS.setting)
  if (!currentPlan(op) || how.act === 'none' || !shown) return
  if (how.act === 'drop' || !shown.armed) {
    await disarm($, op)
    await showPlan($, 'planned', planText(shown.ctx, shown.payback, 'the next turn end, after a reload', false), {}, op)
    await logCompact($, 'skip', { reason: 'the plugin reloaded', ctx_before: shown.ctx, ...paybackFields(shown.payback) })
    return
  }
  const at = shown.armed as unknown as Armed
  CS.armed = at
  if (how.act === 'countdown') armTimers($, at, how.leftMs)
  else if (shown.kind === 'countdown') await offerInstead($, at, shown.why, op) // suggest now: a press compacts
  $.ui.invalidate('ui.render')
}

// A countdown the setting no longer allows becomes an offer: its timers stop, and a press compacts.
async function offerInstead($: EngineInterface, at: Armed, why: string, op = planOp()): Promise<void> {
  if (!currentPlan(op)) return
  for (const timer of CS.timers) timer.cancel()
  CS.timers = []
  await update($, compactView, current => (currentPlan(op) ? { kind: 'offer', ctx: at.ctx, why, payback: at.payback, armed: at } as CompactView : current))
  await showPlan($, 'planned', planText(at.ctx, at.payback, 'your press', at.forced), { ctx: at.ctx, ...paybackFields(at.payback) }, op)
}

// The person changed "Compact at the right moment" in /config: the countdown or offer standing now follows it at once
// (the change also reloads the module, whose resume follows it too). Off cancels it; suggest makes a countdown an offer.
async function settingChanged($: EngineInterface, setting: Setting): Promise<void> {
  CS.setting = setting
  CS.settingStamp++
  const op = planOp()
  const shown = await read($, compactView)
  if (!currentPlan(op)) return
  if (setting === 'off') {
    const was = CS.armed
    await disarm($, op)
    await showPlan($, 'none', null, {}, op)
    if (was) await logCompact($, 'cancel', { by: 'setting', mode: was.mode, ctx_before: was.ctx, ...paybackFields(was.payback), ...seamFields(was.seam) })
  } else if (CS.armed && !CS.compacting && setting === 'suggest' && shown?.kind === 'countdown') {
    await offerInstead($, CS.armed, shown.why, op)
  }
  $.ui.invalidate('ui.render')
}

// A countdown keeps the identity it started with across every wait.
async function startCountdown($: EngineInterface, at: Armed, stamp: number, op = planOp(stamp)): Promise<void> {
  const endsAt = (await $.clock.now()) + COUNTDOWN_MS
  if (!currentPlan(op)) return void (await settingMoved($, at, 'countdown', op))
  for (const timer of CS.timers) timer.cancel()
  CS.armed = at
  const why = actWhy(at.seam)
  await update($, compactView, current => (currentPlan(op)
    ? { kind: 'countdown', ctx: at.ctx, why, endsAt, payback: at.payback, armed: at } as CompactView : current))
  if (!currentPlan(op)) return
  armTimers($, at, COUNTDOWN_MS)
  await showPlan($, 'countdown', countdownText(COUNTDOWN_MS, at.ctx, why), { ends_at: endsAt / 1000, ctx: at.ctx, why }, op)
  if (!currentPlan(op) || CS.armed !== at) return
  await logCompact($, 'countdown', { mode: at.mode, forced: at.forced, ctx_before: at.ctx, ...paybackFields(at.payback), ends_at: endsAt / 1000, why, ...seamFields(at.seam) })
  $.ui.invalidate('ui.render')
}

// A main-loop turn end: plan, offer, count down, compact, or nothing.
async function atTurnEnd($: EngineInterface, answer: string): Promise<void> {
  const op = planOp()
  const ctx = (await $.session.usage().catch(() => null))?.context.tokens
  if (!ctx || !currentPlan(op)) return
  if (ctx < BAND.low) {
    // Under the band (a compaction came, the owner's or CSR's): nothing to plan, and a cancel is forgotten.
    CS.cancelledAt = null
    if ((await read($, infinite))?.plan) await showPlan($, 'none', null, {}, op)
    return
  }
  let plan: PlanJson | null = null
  try {
    plan = JSON.parse((await paneWithin($, ['--repo', await $.session.cwd(), '--compact-plan', '--sid', await $.session.id(), '--ctx', String(ctx)])) ?? 'null') as PlanJson | null
  } catch {}
  if (!currentPlan(op)) return
  const story = plan?.story?.text
  if (story) await update($, infinite, current => ({ plan: current?.plan ?? null, story }))
  const payback: Payback | null | undefined = typeof plan?.payback === 'number'
    ? { requests: plan.payback, perTurn: typeof plan.per_turn === 'number' && plan.per_turn > 0 ? plan.per_turn : null }
    : plan ? null : undefined // undefined: no answer
  const seam = seamOf({ ctx, answer, asking: CS.asking > 0, running: running(), commit: CS.commit, gate: CS.gate, edits: CS.edits })
  CS.loadedAt ??= await $.clock.now() // the first this module saw of the session: the 10 minutes count from here
  const mode = modeOf(CS.last, await $.clock.now(), CS.loadedAt)
  if (!currentPlan(op)) return
  const stamp = op.stamp
  const verdict = decide({ setting: CS.setting, mode, ctx, payback, seam, cancelledAt: CS.cancelledAt })
  const at: Armed = { ctx, payback: payback ?? null, seam, mode, forced: verdict.forced, floor: typeof plan?.floor === 'number' ? plan.floor : null, answer }
  if (verdict.act === 'none') {
    if ((await read($, infinite))?.plan) await showPlan($, 'none', null, {}, op)
    return
  }
  if (verdict.act === 'plan' || verdict.act === 'offer') {
    await showPlan($, 'planned', planText(ctx, at.payback, verdict.act === 'plan' ? verdict.wait : 'your press', verdict.forced), { ctx, ...paybackFields(at.payback) }, op)
  }
  await logCompact($, 'plan', { act: verdict.act, mode, forced: verdict.forced, ctx_before: ctx, ...paybackFields(at.payback), floor: at.floor, wait: verdict.wait ?? null, ...seamFields(seam) })
  if (verdict.act === 'offer') {
    if (!currentPlan(op)) return void (await settingMoved($, at, 'offer', op))
    CS.armed = at
    await update($, compactView, current => (currentPlan(op) ? { kind: 'offer', ctx, why: actWhy(seam), payback: at.payback, armed: at } as CompactView : current))
    $.ui.invalidate('ui.render')
  } else if (verdict.act === 'countdown') await startCountdown($, at, stamp, op)
  else if (verdict.act === 'compact') await compactNow($, at, 'unattended', stamp, op)
}

// A turn that starts during a countdown or an offer (a notification came in) ends it: the next turn end decides.
async function turnStarted($: EngineInterface): Promise<void> {
  const op = planOp()
  CS.turnRunning = true
  CS.turnAt = await $.clock.now()
  if (!currentPlan(op)) return
  CS.loadedAt ??= CS.turnAt
  const was = CS.armed
  if (CS.compacting) {
    await showPlan($, 'none', null, {}, op)
    return
  }
  if (!was) return
  await disarm($, op)
  await logCompact($, 'skip', { reason: 'a turn started', mode: was.mode, ctx_before: was.ctx, ...seamFields(was.seam) })
  await showPlan($, 'planned', planText(was.ctx, was.payback, 'a turn is running', was.forced), {}, op)
}

async function stopSeen($: EngineInterface, tasks: ReadonlyArray<{ id: string; type: string; description?: string }>): Promise<void> {
  await restoreOwners($)
  CS.stopped = { at: await $.clock.now(), running: tasks.map(task => ({ id: task.id, type: task.type, description: task.description ?? '' })) }
}

async function promptSeen($: EngineInterface, kind: string, text: string): Promise<void> {
  CS.loadedAt ??= await $.clock.now()
  if (kind === 'composer' || kind === 'bridge') {
    CS.last = { kind, at: await $.clock.now() }
    const rule = ruleIn(text)
    if (rule && !CS.rules.includes(rule)) CS.rules.push(rule)
    if (CS.armed) await cancelCompact($, 'prompt') // a prompt cancels a countdown, and puts an offer away
    return
  }
  // A background task reported: done when it says so, and what it reports counts for this turn's end.
  if (TN_DONE.test(text)) {
    const tool = TN_TOOL.exec(text)?.[1]
    const task = TN_TASK.exec(text)?.[1]
    for (const [id, item] of CS.launched) if (id === tool || item.id === task) CS.launched.delete(id)
  }
  CS.commit = commitIn(text) ?? CS.commit
  CS.gate = CS.gate || gateIn(text)
}

// The band while CSR counts down or offers; null when it does neither. `quiet`: no hotkeys of its own.
async function compactBand($: EngineInterface, e: RenderInput, quiet: boolean): Promise<RenderElement | null> {
  const shown = await read($, compactView)
  if (!shown || e.component !== 'AbovePrompt') return null
  const { Box, Button, Text } = $.ui.resolve(e)
  const text = shown.kind === 'countdown'
    ? countdownText((shown.endsAt ?? 0) - (await $.clock.now()), shown.ctx, shown.why)
    : offerText(shown.ctx, shown.payback, shown.why)
  return (
    <Box flexDirection="column" marginTop={1}>
      <Box flexDirection="row" alignItems="flex-start">
        <Box flexShrink={0} width={2}>
          <Text color="suggestion">☾</Text>
        </Box>
        <Text wrap="wrap">{text}</Text>
      </Box>
      {quiet ? (
        <Box marginLeft={2}>
          <Text dimColor>Type anything to cancel.</Text>
        </Box>
      ) : (
        <Box marginLeft={2} flexWrap="wrap">
          <Box marginRight={3}>
            <Button key="compact-now" hotkey="1" plain label="Compact now" onPress={() => pressCompact($)} />
          </Box>
          <Box marginRight={3}>
            <Button key="compact-cancel" hotkey="0" plain label={shown.kind === 'offer' ? 'Not now' : 'Cancel'} role="dismiss" onPress={() => void cancelCompact($, 'press')} />
          </Box>
        </Box>
      )}
    </Box>
  )
}

// ---------------------------------------------------------------- hooks

// What the hooks of one load share (see register).
type Fleet = { dreamsOn: boolean; dreaming: boolean; active: boolean; stepAt: number; spent: number; timer: boolean }

// An agent's report waits for the main session's next step, or 20 seconds if the main session is idle. One look takes
// all that waits (agents that finish together share it), after claiming them: no other step or timer can take them.
const COMPLETION_WAIT_MS = 20_000
async function completions($: EngineInterface, fleet: Fleet): Promise<void> {
  if (!fleet.dreamsOn || fleet.dreaming || !(await isPerson($).catch(() => false)) || !(await hasPending($).catch(() => false)) || fleet.dreaming) return
  fleet.dreaming = true
  try {
    const taken = await takeCompletions($)
    if (!taken.length) return
    await writeAgents($).catch(() => undefined) // the brief reads the agents file: these reports must be in it
    const chapter = await chapterOf($)
    const pace = await paceOf($)
    // What a look would cost, guessed from the main session's last step (a fork re-reads the same context); with none known,
    // from an average step of a usual chapter.
    const step = Math.max(1, fleet.spent || pace.step || Math.round(pace.claude / USUAL_STEPS))
    await dream($, step, chapter.steps, undefined, { kind: 'agent', completions: taken })
  } catch {
  } finally {
    fleet.dreaming = false
  }
}

async function armCompletions($: EngineInterface, fleet: Fleet): Promise<void> {
  if (!fleet.dreamsOn || fleet.timer || !(await hasPending($).catch(() => false))) return
  fleet.timer = true
  $.clock.after(COMPLETION_WAIT_MS, () => {
    fleet.timer = false
    void (async () => {
      if (fleet.active || (await $.clock.now()) - fleet.stepAt < COMPLETION_WAIT_MS) return // mid-work: its next step takes them
      await completions($, fleet)
    })().catch(() => undefined)
  })
}

export const register: Register = (on, options) => {
  const dreamsOn = options.dreams === true
  // What the hooks of this load share: whether a look runs (one at a time, whoever started it), whether the main loop is
  // mid-turn, when it last stepped and what that step cost (a completion's look waits for a quiet main loop and borrows
  // the cost for its budget check), and whether a timer waits to offer the reports that came in.
  const fleet: Fleet = { dreamsOn, dreaming: false, active: false, stepAt: 0, spent: 0, timer: false }
  // True while another band (You should know's, or CSR's compaction countdown: the section above) asks, and the
  // note waits behind it.
  let waiting = false


  // Compact at the right moment: off registers none of these hooks.
  CS.setting = settingOf(options.autoCompact)
  if (CS.setting !== 'off') {
    on('classic.Stop', async ($, e, next) => {
      await stopSeen($, Array.isArray(e.background_tasks) ? e.background_tasks : [])
      return next(e)
    }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the stop go on

    // A matcher of its own: the file-tool hook below is the one that takes every call.
    on('tool.call', { tool: ['AskUserQuestion', 'Edit', 'Write', 'NotebookEdit', 'Bash', 'Agent', 'Workflow', 'Monitor'] }, async ($, e, next) => {
      const file = 'file_path' in e && typeof e.file_path === 'string' ? e.file_path
        : 'notebook_path' in e && typeof e.notebook_path === 'string' ? e.notebook_path : undefined
      const wrote = async (result: { isError?: boolean; deny?: unknown }) => {
        if (EDITS.has(e.tool) && file && !result.isError && result.deny === undefined) {
          CS.wrote.add(file.startsWith('/') ? file : `${await $.session.cwd()}/${file}`)
        }
      }
      if (e.agentId !== undefined) { // a subagent's: what it starts in the background is its own
        const result = await next(e)
        await wrote(result)
        const id = TASK_ID.exec(typeof result.text === 'string' ? result.text : '')?.[1]
        if (id && (BACKGROUND.has(e.tool) || e.tool === 'Bash' || e.tool === 'Monitor') && !result.isError && result.deny === undefined) {
          await ownTask($, id, e.agentId)
        }
        return result
      }
      if (e.tool === 'AskUserQuestion') {
        CS.asking++
        try {
          return await next(e)
        } finally {
          CS.asking--
        }
      }
      if (EDITS.has(e.tool)) CS.edits++
      const result = await next(e)
      await wrote(result)
      const text = typeof result.text === 'string' ? result.text : ''
      const args = e as unknown as { run_in_background?: unknown; description?: unknown; command?: unknown }
      if (e.tool === 'Bash' && !result.isError && result.deny === undefined && typeof args.command === 'string' && /\bgit\b[^\n|;&]*\bcommit\b/.test(args.command)) {
        const found = COMMIT_OUT.exec(text)
        if (found?.[1]) CS.commit = found[1].slice(0, 7)
        if (found?.[1]) {
          const dir = commitDir(args.command, await $.session.cwd())
          if (dir) CS.committedIn.add(dir)
        }
      }
      const background = BACKGROUND.has(e.tool) ? /launched|in the background/i.test(text) : e.tool === 'Bash' && args.run_in_background === true && !result.isError
      if (background) {
        const type = e.tool === 'Bash' ? 'shell' : e.tool === 'Workflow' ? 'workflow' : 'subagent'
        const id = TASK_ID.exec(text)?.[1] ?? e.tool_use_id // the id Claude Code's own task list shows
        CS.launched.set(e.tool_use_id, { id, type, description: typeof args.description === 'string' ? args.description : '' })
      }
      return result
    }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the call go on

    // A matcher of its own: the hotkey and dreamer hook below is the one that takes every prompt.
    on('prompt.submit', { origin: { kind: /^(composer|bridge|task-notification)$/ } }, async ($, e, next) => {
      await promptSeen($, e.origin.kind, e.text)
      return next(e)
    }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the prompt go on

  }

  // The main loop's turn boundaries, whatever the settings: compaction (when it is on) and the agents both read them.
  on('turn.start', async ($, e, next) => {
    fleet.active = true
    if (CS.setting !== 'off') await turnStarted($)
    return next(e)
  }).catch((_$, e, next) => next(e))

  on('turn.complete', async ($, e, next) => {
    const result = await next(e)
    if (e.agentId !== undefined) {
      // An agent's turn ends: a run of that agent. e.answer is its report (the hook result's text is not).
      if (await isPerson($).catch(() => false)) {
        await track($, { kind: 'complete', at: await $.clock.now(), agentId: e.agentId, turnId: e.turnId, reason: e.reason, answer: e.answer })
        if (dreamsOn && e.reason === 'answer' && e.answer.trim() && (await admitted($, completionKey(await $.session.id(), e.agentId, e.turnId)))) {
          void armCompletions($, fleet).catch(() => undefined)
        }
      }
      return result
    }
    fleet.active = false
    if (dreamsOn && (await isPerson($).catch(() => false))) await changePace($, pace => closeTurn(pace, e.turnId)).catch(() => undefined)
    if (await isPerson($).catch(() => false)) {
      void pollAgents($)
      void armCompletions($, fleet).catch(() => undefined) // reports that came in mid-turn are looked at once the person has the floor
    }
    if (CS.setting === 'off') return result
    CS.turnRunning = false
    try {
      if (e.reason === 'answer' && !CS.compacting && (await isPerson($))) {
        await atTurnEnd($, e.answer)
      }
    } catch {
    } finally {
      CS.edits = 0
      CS.commit = undefined
      CS.gate = false
    }
    return result
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the turn end

  // The agent tool's spawn, whoever's loop it happens in. What was asked is recorded before `next` answers (and no
  // lock is held across it); what came back, after. A spawn that started nothing makes no row.
  on('agent.spawn', async ($, e, next) => {
    const person = await isPerson($).catch(() => false)
    if (person) {
      await track($, {
        kind: 'spawn', at: await $.clock.now(), toolUseId: e.tool_use_id, description: e.description, type: e.subagentType || null,
        parent: e.parentAgentId ?? null, model: e.model ?? null,
        requests: handoffRequests(e.prompt, await $.session.id()).map(entry => entry.request), // handoffs the call's prompt carries
      })
    }
    const result = await next(e)
    if (person) {
      await track($, {
        kind: 'spawned', at: await $.clock.now(), toolUseId: e.tool_use_id,
        ...(result.deny === undefined && result.agentId ? { agentId: result.agentId, model: result.model } : {}),
      })
    }
    return result
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the spawn go on

  // Typing cancels a countdown and sets the walkthrough aside; a lone digit into an empty box is a band hotkey on
  // its way, not typing.
  on('prompt.edit', async ($, e, next) => {
    if (!(e.text === '' && /^\d$/.test(e.key?.key ?? ''))) {
      if (CS.armed?.mode === 'attended') await cancelCompact($, 'typing')
      await setAside($)
    }
    return next(e)
  }).catch((_$, e, next) => next(e))

  on('turn.step', async function* ($, e, next) {
    const result = yield* next(e)
    const person = await isPerson($).catch(() => false)
    const spent = result?.usage ? tokensOf(result.usage) : 0
    if (e.agentId !== undefined) {
      // A subagent's step: a row's activity (its tokens show on its row), and that is all. The dreamer reads the main
      // conversation, so its share is measured against that: a subagent's tokens are never Claude's spend here, and its
      // step never counts as a step of the chapter nor starts a look of the main schedule.
      if (person) {
        const turn = result?.turnId ?? e.turnId
        const index = result?.index ?? e.index
        await track($, { kind: 'step', at: await $.clock.now(), agentId: e.agentId, turnId: turn, index, answer: result?.answer ?? '', tokens: spent })
      }
      return result
    }
    fleet.stepAt = await $.clock.now()
    if (!dreamsOn || !person) return result
    fleet.spent = spent || fleet.spent
    // Count what Claude spends: the dreamer's share is measured against it. A step is one index of one turn, counted once
    // in the pace's receipts: a replay of it adds no usage and is no step of the chapter.
    const turn = result?.turnId ?? e.turnId
    const index = result?.index ?? e.index
    let fresh = true
    await changePace($, pace => {
      const counted = countStep(pace, turn, index, spent)
      fresh = counted !== pace
      return counted
    })
    if (!fresh) return result
    // Steps counted in the store, so a reload of this module does not start the count over.
    const chapter = await changeChapter($, current => ({ ...current, steps: current.steps + 1 }))
    if (chapter.steps >= chapter.due && !fleet.dreaming) {
      fleet.dreaming = true
      void dream($, spent, chapter.steps, result?.usage?.model)
        .catch(() => undefined)
        .finally(() => {
          fleet.dreaming = false
        })
    } else void completions($, fleet).catch(() => undefined) // the next step of the main session: a finished agent's turn
    return result
  })

  on('model.fork', async ($, e, next) => {
    if (!(await isPerson($))) return next(e)
    const origin = `${next.origin.plugin}/${next.origin.tier}`
    const cwd = await $.session.cwd()
    const session = await $.session.id()
    if (!isYskOrigin(next.origin)) {
      // Another plugin's fork: keep its seat name, so a You should know look that names itself some other
      // way shows up in the log instead of passing unseen.
      void run($, ['--repo', cwd, '--keep', JSON.stringify({
        kind: 'ysk', at: (await $.clock.now()) / 1000, session, outcome: 'other-origin', origin,
      })]).catch(() => undefined)
      return next(e)
    }
    const began = await $.clock.now()
    const propose = isYskPropose(e.prompt)
    const off = (await $.env.get('CSR_NO_YSK_BRIEF').catch(() => undefined)) === '1'
    const given = propose && !off ? await yskFactsFor($, cwd, session).catch(() => '') : ''
    const result = await next(given ? { ...e, prompt: withCsrFacts(e.prompt, given) } : e)
    const reply = result.value
    const said = reply?.isAnswered ? yskLine(reply.text) : null
    void run($, ['--repo', cwd, '--keep', JSON.stringify({
      kind: 'ysk', at: (await $.clock.now()) / 1000, session, origin, prompt: propose ? 'propose' : 'explain',
      outcome: !reply ? 'denied' : !reply.isAnswered ? reply.reason : !propose ? 'explained' : said?.line ? 'line' : said?.parsed ? 'none' : 'unparsed',
      line: said?.line ?? null, given: given.length, cited: reply?.isAnswered && given ? citedFrom(reply.text, given) : [],
      ...(reply && 'usage' in reply ? { usage: reply.usage } : {}), took_ms: (await $.clock.now()) - began,
    })]).catch(() => undefined)
    return result
  })

  // A compaction ends the chapter and the next starts with five. A dream still open is carried into it once,
  // marked "still open", with two prompts again to answer it: 3 of 18 dreams on 10-09/12 expired at a
  // compaction 28 to 243 minutes after they were shown, each with the person at the prompt in between. A dream
  // already carried once folds into the pane's passed ones.
  on('session.compact', async ($, e, next) => {
    const result = await next(e)
    if (!dreamsOn || e.agentId !== undefined || e.trigger === 'precompute' || result.skip !== undefined) return result
    if (!(await isPerson($))) return result
    let expired: Note | null = null
    let carried: Note | null = null
    await update($, view, current => {
      if (current && current.kind !== 'asked' && current.note.kind === 'dream') {
        if (!current.carried) {
          carried = current.note
          return { ...current, carried: true, promptsSurvived: 0 }
        }
        expired = current.note
        return null
      }
      return current
    })
    const note = expired as Note | null
    const kept = carried as Note | null
    if (note) await answered($, note, 'expired')
    if (kept) {
      const at = await $.clock.now()
      await run($, ['--repo', await $.session.cwd(), '--keep', JSON.stringify({ kind: 'answer', id: kept.id, answer: 'carried', at: at / 1000 })])
    }
    await serially(() => newChapter($, 'compact'))
    return result
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the compaction go on

  let held: { text: string; at: number } | null = null // a bare digit kept back, for 10 s
  let bandAt = 0 // when another band (You should know's) last drew a question

  // The push guard, opt-in (coach.ts, Guard): what the repository's .claude/csr-guard.json and the person's
  // guard.json protect, and nothing when neither is there. Every other git command goes through untouched. A push
  // whose destination the words leave to the repository (bare `git push`, `git push origin HEAD`) is settled by
  // asking git where it lands; what cannot be settled is denied (astra r3).
  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const cwd = await $.session.cwd()
    const guard = await guardOf($, cwd)
    if (!guard) return next(e) // no guard: no git asked anything
    const verdict = gitVerdict(e.command, guard, cwd)
    if (!verdict) return next(e)
    if (verdict.kind === 'deny') return { deny: verdict.reason }
    if (verdict.kind === 'check') for (const push of verdict.pushes) {
      const why = await landsOnRelease($, push, cwd, guard)
      if (why) return { deny: why }
    }
    return next(e)
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the call go on

  on('tool.call', async ($, e, next) => {
    if (e.agentId === undefined && FILE_TOOLS.has(e.tool)) {
      const file = 'file_path' in e && typeof e.file_path === 'string'
        ? e.file_path
        : 'notebook_path' in e && typeof e.notebook_path === 'string' ? e.notebook_path : undefined
      if (file) void offerFor($, file, e.tool === 'Read' ? 'You should know' : 'Heads up').catch(() => undefined)
    }
    // The agents: an agent's tool is alive when it begins and counted when it ends; the main loop's writes are kept
    // for the files an agent also wrote. `next` answers once, whatever these observations come to.
    const tracked = e.agentId !== undefined || isEditTool(e.tool)
    const person = tracked && (await isPerson($).catch(() => false))
    if (person && e.agentId !== undefined) await track($, { kind: 'touch', at: await $.clock.now(), agentId: e.agentId, tool: e.tool })
    const result = await next(e)
    if (person) {
      try {
        const file = 'file_path' in e && typeof e.file_path === 'string' ? e.file_path
          : 'notebook_path' in e && typeof e.notebook_path === 'string' ? e.notebook_path : null
        await track($, {
          kind: 'tool', at: await $.clock.now(), agentId: e.agentId, toolUseId: e.tool_use_id, tool: e.tool,
          ok: result.deny === undefined && !result.isError, path: file ? normalizePath(file, await $.session.cwd()) : null,
        })
      } catch {}
    }
    return result
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the call go on

  on('prompt.submit', async ($, e, next) => {
    // A prompt that is only a digit, just after a band note, is a hotkey that missed: held with a toast,
    // sent on if the same digit comes again within 10 s. A held digit is not counted as typed past a note.
    const at = await $.clock.now()
    const yours = e.origin.kind === 'composer' || e.origin.kind === 'bridge' // you typed it, not a peer or a plugin
    if (yours && isBareDigit(e.text)) {
      const digit = e.text.trim()
      const up = await read($, view)
      if (held?.text === digit && at - held.at <= AGAIN_MS) held = null
      else if ((!!up && up.kind !== 'asked' && at - up.shownAt <= BAND_WINDOW_MS) || (bandAt > 0 && at - bandAt <= BAND_WINDOW_MS)) {
        held = { text: digit, at }
        const said = `“${digit}” looks like a band hotkey. Press it on the band, or send it again within 10 s to give it to Claude.`
        $.ui.toast(said)
        return { drop: said }
      }
    }
    if (yours) await setAside($).catch(() => undefined) // a prompt sent past the walkthrough
    await settledInThePane($).catch(() => undefined) // answered there: never counted as typed past
    if (dreamsOn) await sentToClaude($, e.text, yours).catch(() => undefined) // a dream on its way to Claude, or your no to it
    // The person's last three prompts go to the dreamer as already on the table: 10-11 a dream offered back what the
    // person had typed 30 s before. Kept per session; a forwarded note or dream is not the person's ask.
    const own = dreamsOn && yours ? ownPrompt(e.text) : null
    if (own) await $.store.set(await ownKey($, 'asked'), [...(await strings($, await ownKey($, 'asked'))), own].slice(-3)).catch(() => undefined)
    let ignored: Note | null = null
    await update($, view, current => {
      if (!current || waiting) return current // a note nobody saw was not typed past
      const promptsSurvived = current.promptsSurvived + 1
      if (promptsSurvived < CLEAR_AFTER_PROMPTS) return { ...current, promptsSurvived, message: undefined }
      if (current.kind === 'offer') ignored = current.note
      return null
    })
    const left = ignored as Note | null
    if (left) {
      await answered($, left, 'ignored')
      if (left.kind === 'dream') {
        // Typed past: ask less often, as You should know does.
        const inARow = (await count($, await ownKey($, 'typedPastInARow'))) + 1
        await $.store.set(await ownKey($, 'typedPastInARow'), inARow)
        await $.store.set(await ownKey($, 'looksToSkip'), Math.min(SKIP_MAX, 2 ** (inARow - 1)))
      }
    }
    const entered = await next(e)
    await handoffSent($, entered)
    return entered
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the prompt go on

  // While a compaction runs, a marker file says so and when it began; the pane draws the timer from it. It
  // is removed when the compaction ends, however it ends. Nothing here can fail the compaction.
  // A matcher of its own: a compaction hook with none is registered once (the chapter's, above).
  on('session.compact', { trigger: ['manual', 'auto', 'plugin'] }, async ($, e, next) => {
    const marker = e.agentId === undefined ? await markCompacting($, e.trigger) : null
    try {
      return await next(e)
    } finally {
      if (marker) await unmarkCompacting($, marker)
    }
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the compaction go on

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const beneath = await next(e)
    if (asks(beneath)) bandAt = await $.clock.now()
    if (e.props.hasSurvey) return beneath
    // A compaction counting down, or offered, comes first: its 20 s are short, and its 1 and 0 are the ones up.
    // Under another band that asks, it draws no hotkeys of its own (two on one hotkey: the later wins).
    const counting = await compactBand($, e, asks(beneath))
    if (counting) {
      waiting = Boolean(await read($, view)) // a note behind it waits, and is not typed past
      const { Box } = $.ui.resolve(e)
      return beneath ? (
        <Box flexDirection="column">
          {counting}
          {beneath}
        </Box>
      ) : counting
    }
    // The walkthrough next, one step at a time; under another band that asks, it waits, as a note does.
    const step = asks(beneath) ? null : nextStep(await read($, setupView))
    if (step && (await isPerson($))) {
      bandAt = await $.clock.now() // a bare digit sent as a prompt is a hotkey that missed: held, as for a note
      waiting = Boolean(await read($, view))
      const walk = await setupBand($, e, step, step === 'dreams' ? dreamsOn : CS.setting)
      const { Box } = $.ui.resolve(e)
      return beneath ? (
        <Box flexDirection="column">
          {walk}
          {beneath}
        </Box>
      ) : walk
    }
    const shown = await read($, view)
    // One question at a time. Two bands both on 1, 2, 4 and 0 would answer
    // the lower one ("two on one hotkey: the later wins"), so 4 under a dream
    // would turn You should know off. The note waits until that band clears.
    waiting = Boolean(shown) && asks(beneath)
    if (!shown || waiting) return beneath
    const { Box, Button, Code, Markdown, Text } = $.ui.resolve(e)
    const { note } = shown
    const isDream = note.kind === 'dream'

    const close = async (answer: string, then: NoteView | null) => {
      await answered($, note, answer)
      await update($, view, () => then)
    }
    const ask = () => close('dismiss', { ...shown, kind: 'asked', message: undefined })
    // You should know's way out: a dim 4 that asks for a second press, then turns dreams off.
    const turnOff = async () => {
      if (!shown.confirmOff) {
        await update($, view, () => ({ ...shown, confirmOff: true }))
        return
      }
      await answered($, note, 'turn_off')
      const done = await $.config.set({ key: await dreamsRow($), value: false }).catch(() => ({ deny: 'failed' }))
      await update($, view, () =>
        'deny' in done && done.deny ? { ...shown, confirmOff: false, message: "Couldn't turn dreams off here. Use /config." } : null,
      )
    }
    const wayOut = isDream
      ? [{ hotkey: '4', label: shown.confirmOff ? 'Press again to turn dreams off' : 'Disable', onPress: turnOff, dim: true }]
      : []
    const buttons = (row: Array<{ hotkey: string; label: string; onPress: () => unknown; dismiss?: boolean; dim?: boolean }>) => (
      <Box marginLeft={2} flexWrap="wrap">
        {row.map(button => (
          <Box marginRight={3}>
            <Button
              key={button.label}
              hotkey={button.hotkey}
              plain
              label={button.label}
              dimColor={button.dim}
              role={button.dismiss ? 'dismiss' : undefined}
              onPress={() => void Promise.resolve(button.onPress()).catch(() => undefined)}
            />
          </Box>
        ))}
      </Box>
    )
    const lead = (body: string) => (
      <Box flexDirection="row" alignItems="flex-start">
        <Box flexShrink={0} width={2}>
          <Text color="suggestion">{STAR}</Text>
        </Box>
        <Text wrap="wrap">
          <Text dimColor>{shown.tag}{shown.carried ? ' · still open from before the compaction' : ''} · </Text>
          {body}
        </Text>
      </Box>
    )

    let ours
    if (note.kind === 'term') {
      // Coaching: one word, defined once in your CLAUDE.md, only if you press 1.
      const add = async () => {
        if (await addToClaudeMd($, note.text)) {
          $.ui.toast('Added to your CLAUDE.md under Terms. Edit the line to say what you mean.')
          await close('added', null)
        } else await update($, view, () => ({ ...shown, message: "Couldn't write your CLAUDE.md. Nothing was changed." }))
      }
      ours = (
        <Box flexDirection="column" marginTop={1}>
          {lead(note.line)}
          {buttons([
            { hotkey: '1', label: 'Add to CLAUDE.md', onPress: add },
            { hotkey: '0', label: 'Dismiss', onPress: () => close('dismiss', null), dismiss: true },
          ])}
          {shown.message ? <Box marginLeft={2}><Text color="warning" wrap="wrap">{shown.message}</Text></Box> : null}
        </Box>
      )
    } else if (shown.kind === 'offer') {
      ours = (
        <Box flexDirection="column" marginTop={1}>
          {lead(note.line)}
          {buttons([
            { hotkey: '1', label: 'Learn more', onPress: () => update($, view, () => ({ ...shown, kind: 'explained' })) },
            {
              hotkey: '2',
              label: isDream ? 'Already thought of it' : 'Knew this already',
              onPress: async () => {
                if (isDream) await remember($, 'knownDreams', seenEntry(note), SEEN_KEPT)
                else await remember($, 'known', note.id)
                await close(isDream ? 'thought_of_it' : 'know_it', null)
              },
            },
            ...wayOut,
            { hotkey: '0', label: 'Dismiss', onPress: ask, dismiss: true },
          ])}
          {shown.message ? <Box marginLeft={2}><Text color="warning" wrap="wrap">{shown.message}</Text></Box> : null}
        </Box>
      )
    } else if (shown.kind === 'explained') {
      const chat = async () => {
        const box = await $.prompt.read().catch(() => undefined)
        if (box?.text.trim()) {
          await update($, view, () => ({ ...shown, message: 'Your prompt box has text in it. Send or clear it, then press 2 again.' }))
          return
        }
        const filled = await $.prompt.fill({ text: handoff(shown), mode: 'replace' }).catch(() => undefined)
        if (filled?.isFilled) await close(isDream ? 'ask_claude' : 'chat', null)
        else await update($, view, () => ({ ...shown, message: "The prompt box didn't take it. Press 2 to try again." }))
      }
      const source = isDream
        ? `From CSR's dreamer · this look ${(note.tokens ?? 0).toLocaleString('en-US')} tokens, mostly re-read from the prompt cache`
        : `From CSR's memory · 0 model tokens${note.oid ? ` · commit ${note.oid.slice(0, 7)}` : ''} · session ${note.conversation.slice(0, 8)}`
      ours = (
        <Box flexDirection="column" marginTop={1}>
          {lead(note.line)}
          <Box flexDirection="column" marginLeft={2} marginTop={1} marginBottom={1}>
            <Text bold wrap="wrap">{note.title}</Text>
            {isDream ? <Markdown text={note.text} /> : <Text wrap="wrap">{note.text}</Text>}
            {note.sketch.length ? (
              <Box marginTop={1}>
                <Code source={note.sketch.join('\n')} language="text" wrap="truncate-end" />
              </Box>
            ) : null}
            <Text dimColor wrap="truncate-end">{source}</Text>
            {shown.message ? <Text color="warning" wrap="wrap">{shown.message}</Text> : null}
          </Box>
          {buttons([
            { hotkey: '1', label: 'Understood', onPress: () => close('got_it', { ...shown, kind: 'asked', message: undefined }) },
            { hotkey: '2', label: isDream ? 'Ask Claude' : 'Chat in main session', onPress: chat },
            { hotkey: '3', label: 'Open the CSR pane', onPress: async () => openPane($, await $.session.cwd()) },
            { hotkey: '0', label: 'Dismiss', onPress: ask, dismiss: true },
          ])}
        </Box>
      )
    } else {
      ours = (
        <Box flexDirection="row" marginTop={1} flexWrap="wrap">
          <Box flexShrink={0} width={2}>
            <Text color="suggestion">{STAR}</Text>
          </Box>
          <Box marginRight={3}>
            <Text dimColor>{isDream ? 'Was that worth asking?' : 'Was that worth showing?'}</Text>
          </Box>
          {buttons([
            { hotkey: '1', label: 'That was helpful', onPress: () => close('helpful', null) },
            { hotkey: '2', label: 'Not relevant', onPress: () => close('not_relevant', null) },
            ...wayOut,
            { hotkey: '0', label: 'Close', onPress: () => update($, view, () => null), dismiss: true },
          ])}
        </Box>
      )
    }
    return beneath ? (
      <Box flexDirection="column">
        {ours}
        {beneath}
      </Box>
    ) : ours
  })

  // ---------------------------------------------------------------- the pane, /csr

  // A /config change to this plugin's "Compact at the right moment" row: the countdown standing follows it now.
  on('config.set', async ($, e, next) => {
    const done = await next(e)
    if (/^csr-companion(@[^.]+)?\.autoCompact$/.test(e.key) && !('deny' in done && done.deny)) await settingChanged($, settingOf(e.value))
    return done
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the change go on

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    person = e.isInteractive
    if (!e.isInteractive) return started
    watchAgents($) // the agents of this session: the minute poll, and one look at the list now
    await agentsChange($, state => ({ ...state, fileDue: null })).catch(() => undefined) // a timer a reload cancelled is due no more
    await pollAgents($)
    void armCompletions($, fleet).catch(() => undefined) // reports a reload left pending are offered once
    await resumeArmed($).catch(() => undefined) // a reload mid-countdown: it runs on, never stuck at 0s
    await startSetup($).catch(() => undefined) // the walkthrough, at the start after the plugin is added
    void sweepSessions($).catch(() => undefined)
    $.ui.status(undefined) // CSR pins no line under the prompt: the pane carries what it knows
    await $.command.register({
      name: 'csr',
      description:
        'Open the CSR pane beside Claude; "/csr setup" walks you through its settings; "/csr dreams" says how the dreamer is pacing itself; "/csr statusline" moves your status line into the pane',
    })
    void refreshWrapper($).catch(() => undefined) // never blocks or fails the start
    // The recurring jobs first, however the first pane.py call ends (astra r4 #11: a first call that timed out left
    // no refresh and no pane-answer sync for the session, and an unhandled rejection).
    $.clock.every(REFRESH_MS, () => void refresh($, e.cwd).catch(() => undefined))
    $.clock.every(SETTLED_IN_PANE_MS, () => void settledInThePane($).catch(() => undefined))
    void (async () => {
      const fresh = await refresh($, e.cwd).catch(() => null)
      void coachNote($, e.cwd).catch(() => undefined) // a word you keep saying, offered once
      if (fresh?.repo) await openPane($, e.cwd)
    })().catch(() => undefined)
    return started
  })

  on('command.run', { command: 'csr' }, async ($, e) => {
    const [sub, choice] = e.args.trim().split(/\s+/)
    if (sub === 'dreams') return { text: await dreamsText($, dreamsOn) }
    if (sub === 'setup') {
      await update($, setupView, () => ({ steps: [...STEPS], done: [], aside: false }))
      return { text: `CSR setup is above the prompt: ${STEPS.length} steps, a key press each.` }
    }
    if (sub === 'statusline') {
      return { text: await statusline($, choice).catch((error: unknown) => `/csr statusline: ${String(error)}`) }
    }
    const cwd = await $.session.cwd()
    const fresh = await refresh($, cwd)
    const where = await openPane($, cwd)
    const counts = fresh
      ? `${fresh.dreams_7d} dreams this week, ${fresh.changes_total} changed since your sessions`
      : 'csr-engine could not be read'
    return { text: `${where === 'herdr' ? 'CSR pane opened beside this one' : 'CSR pane opened'} (${counts}). ${await dreamsText($, dreamsOn)}` }
  })

  // Prep-compact: the threads you set aside for later go to the summarizer as reminders. A failure, or
  // a pane.py that takes over two seconds, lets the compaction run as it was. It names its triggers: the
  // engine refuses two session.compact hooks that both match every one, and the chapter hook above is one.
  on('session.compact', { trigger: /^(manual|auto|plugin|precompute)$/ }, async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    let block: string | null = null
    try {
      const ran = await $.process.run(
        ['python3', '-I', script($), '--repo', await $.session.cwd(), '--lost', '--sid', await $.session.id()],
        { timeoutMs: 2000 }, // past it the call rejects: the compaction goes on as it was
      )
      block = ran.exitCode === 0 ? carryBlock(JSON.parse(ran.stdout) as Thread[]) : null
    } catch {
      block = null
    }
    return next(block ? { ...e, instructions: e.instructions ? `${e.instructions}\n\n${block}` : block } : e)
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the compaction go on

  // The pane says what CSR did here, what it found, and what it can do: with dreams off it offers them.
  // The /config row is in your user settings, so it turns dreams on in every session, the other windows
  // too: the first press asks again, as the band's turn-off does. The second changes the row and the
  // module reloads with dreams on, so the answer is kept here, where a reload forgets it.
  let turnedOn: 'asked' | 'on' | 'refused' | null = null

  // Drawn on the herdr pane's paper, so the two panes read as one: a dark bar for the counts, then a card
  // per section, each in its own ink, its heading a step deeper than its rows. A Box's background paints
  // only under its text, so each row is wrapped here and padded to the pane's width, as pane.py's are.
  // Every Text names its colour: the cards are light whatever the terminal's theme.
  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Button, Text } = $.ui.resolve(e)
    const shown = await read($, data)
    if (!shown) return <Text dimColor>csr-engine could not be read.</Text>
    const turnOn = async () => {
      if (turnedOn !== 'asked') {
        turnedOn = 'asked'
        $.ui.invalidate('ui.render')
        return
      }
      const done = await $.config.set({ key: await dreamsRow($), value: true }).catch(() => ({ deny: 'failed' }))
      turnedOn = 'deny' in done && done.deny ? 'refused' : 'on'
      $.ui.invalidate('ui.render')
    }
    const cols = Math.max(40, e.props.bodyColumns)
    const times = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`

    // One row of the pane: its parts in their inks, on one ground, filled to the pane's width.
    const paint = (back: string, parts: Part[]) => {
      const used = parts.reduce((n, part) => n + part.text.length, 0)
      return (
        <Text backgroundColor={back} wrap="truncate-end">
          {parts.map(part => (
            <Text backgroundColor={back} color={part.color} bold={part.bold}>{part.text}</Text>
          ))}
          {/* the engine trims a row's trailing spaces: a block in the ground's own colour keeps the fill */}
          <Text backgroundColor={back} color={back}>{`${' '.repeat(Math.max(0, cols - used - 1))}█`}</Text>
        </Text>
      )
    }
    // A card: its heading, then each item wrapped under its indent; a mark sits in a column of its own.
    const card = (look: Look, title: string, meta: string, items: Item[]) => {
      const left = ` ${look.glyph}  ${title}`
      const rows = [
        paint(look.head, [
          { text: left, color: look.ink, bold: true },
          { text: meta ? ' '.repeat(Math.max(1, cols - left.length - meta.length - 1)) + meta : '', color: PAPER.quiet },
        ]),
      ]
      for (const item of items) {
        const indent = item.mark ? 7 : 4
        const lines = item.cut ? [item.text] : wrapped(item.text, cols - indent - 1)
        lines.forEach((text, i) => {
          const lead: Part[] = item.mark
            ? [{ text: '    ' }, { text: i ? '   ' : item.mark[0].padEnd(3), color: item.mark[1], bold: true }]
            : [{ text: '    ' }]
          rows.push(paint(look.row, [...lead, { text, color: item.color ?? PAPER.text, bold: item.bold }]))
        })
      }
      return (
        <Box flexDirection="column" marginTop={1}>
          {rows}
        </Box>
      )
    }
    // The counts, on the herdr pane's dark masthead.
    const bar = (back: string, label: string, value: string) =>
      wrapped(value, cols - 15).map((text, i) =>
        paint(back, [
          { text: ` ${(i ? '' : label).padEnd(13)} `, color: PAPER.barLabel },
          { text, color: PAPER.barText },
        ]),
      )

    const recalled = shown.recalled ?? 0
    const week = [
      shown.corrections_carried_7d ? `carried ${times(shown.corrections_carried_7d, 'correction', 'corrections')} of yours` : '',
      shown.shifts_told_7d ? `told Claude about ${times(shown.shifts_told_7d, 'code change', 'code changes')}` : '',
    ].filter(Boolean)
    const counts = (
      <Box flexDirection="column">
        {bar(PAPER.bar, 'THIS SESSION', recalled ? `recalled your past work for Claude ${times(recalled, 'time', 'times')}` : 'nothing recalled yet; CSR looks on each prompt')}
        {week.length ? bar(PAPER.barLow, 'THIS WEEK', week.join(' · ')) : null}
      </Box>
    )

    let dreams
    let offer = null
    if (!dreamsOn && turnedOn !== 'on') {
      // Your own dream, when there is one, says what dreams do better than a description can.
      const sample = shown.sample
      const asked = sample
        ? `On ${new Date(sample.at * 1000).toISOString().slice(5, 10)}${sample.project ? `, in ${sample.project},` : ''} the dreamer asked this, and you ${sample.answer === 'helpful' ? 'marked it helpful' : 'handed it to Claude'}:`
        : ''
      dreams = card(CARD.dreams, 'Dreams are off', 'off', [
        ...(sample ? [{ text: asked, color: PAPER.faint }, { text: `“${sample.line}”`, bold: true }, { text: '' }] : []),
        {
          text: sample
            ? 'Turned on, it asks one "what if...?" like this while Claude works, when your past sessions give it a reason. It looks only when CSR has something new, and checks before each look that its spend stays within 1 token for every 10 Claude spends. A look that runs over is paid back before the next.'
            : 'Turn them on, and while Claude works a side agent asks one "what if...?" when your past sessions give it a reason. It looks only when CSR has something new, and checks before each look that its spend stays within 1 token for every 10 Claude spends. A look that runs over is paid back before the next.',
          color: PAPER.quiet,
        },
        { text: 'The switch is in your user settings: it covers all your Claude sessions.', color: PAPER.quiet },
      ])
      // The press sits under the card, on the terminal's own ground, where the engine draws its hotkey.
      const press = turnedOn === 'asked' ? 'Press again: dreams on in all your sessions' : 'Turn dreams on'
      offer = (
        <Box flexDirection="column" paddingLeft={4}>
          <Button key="dreams-on" hotkey="1" plain label={press} onPress={() => void turnOn()}>
            <Text bold color="suggestion">{press}</Text>
          </Button>
          {turnedOn === 'refused' ? <Text color="warning" wrap="wrap">Couldn't turn dreams on here. Use /config: csr-companion, Dreams.</Text> : null}
        </Box>
      )
    } else if (!shown.dreams.length) {
      dreams = card(CARD.dreams, 'Dreams are on', 'none yet', [
        { text: 'The first look comes after Claude has done some work, and only when CSR has something new about it. "/csr dreams" shows the pace.', color: PAPER.quiet },
      ])
    } else {
      dreams = card(
        CARD.dreams,
        'Dreams',
        `${shown.dreams_7d} this week`,
        shown.dreams.slice(0, 6).flatMap(entry => [
          { text: `${new Date(entry.at * 1000).toISOString().slice(5, 16).replace('T', ' ')} · ${entry.answer ?? 'open'}`, color: PAPER.faint, cut: true },
          { text: entry.line },
        ]),
      )
    }

    const changed = shown.changes.length
      ? card(
          CARD.changed,
          'Changed since your sessions',
          `${shown.changes_total}`,
          shown.changes.flatMap(change => {
            const latest = change.shifts[0]
            const names = latest ? change.shifts.filter(s => s.oid === latest.oid).map(s => `${MARK[s.verb] ?? ''}${s.symbol ?? '(file)'}`) : []
            return [
              { text: `● ${change.talked.slice(5)} ${change.title}`, bold: true, cut: true },
              ...(latest ? [{ text: `  ${latest.date.slice(5)} ${latest.oid.slice(0, 7)} ${names.join(' ')}`, color: PAPER.quiet, cut: true }] : []),
            ]
          }),
        )
      : null
    const log = shown.shifts.length
      ? card(
          CARD.changed,
          'Change log',
          `${shown.total}`,
          byCommit(shown.shifts).flatMap(([title, group]) => [
            { text: title, bold: true, cut: true },
            ...group.map(shift => ({
              text: `  ${MARK[shift.verb] ?? ''}${shift.symbol ?? '(whole file)'}  ${shift.file.split('/').slice(-2).join('/')}`,
              color: PAPER.quiet,
              cut: true,
            })),
          ]),
        )
      : null

    // What CSR can do here: the things this folder has not shown you yet, each marked in its section's ink.
    const can = card(CARD.can, 'What CSR can do here', '', [
      { mark: ['↑', INKS.recall], text: 'Ask about past work: "what did we decide about the pane?" CSR hands Claude the session.' },
      { mark: ['§', INKS.ctx], text: 'Ask "why is this code here?" Claude traces it to the sessions and commits behind it.' },
      ...(shown.changes.length
        ? []
        : [
            shown.git === false
              ? { mark: ['~', INKS.quiet] as [string, string], text: 'Not in git, so code changes cannot show here. Recall still works.' }
              : { mark: ['~', INKS.quiet] as [string, string], text: 'Code you worked on that a later commit changes shows here, with the commit.' },
          ]),
      { mark: ['‹›', INKS.ctx], text: 'In herdr, this pane opens as a split beside Claude, with the whole session.' },
      { mark: ['→', INKS.sepia], text: '"/csr statusline" puts your status line in this pane.' },
    ])
    // The infinite session: what CSR plans for this session's context, and what its compactions did.
    const lasting = await read($, infinite)
    const session = lasting && (lasting.plan || lasting.story)
      ? card(CARD.infinite, 'Infinite session', '', [
          ...(lasting.plan ? [{ text: lasting.plan, bold: true }] : []),
          ...(lasting.story ? [{ text: lasting.story, color: PAPER.quiet }] : []),
        ])
      : null
    return (
      <Box flexDirection="column">
        {counts}
        {session}
        {dreams}
        {offer}
        {changed}
        {log}
        {can}
      </Box>
    )
  })
}

// A herdr split beside this pane: a real side pane in the terminal (Warp), at
// any width, where the session strip and the date axes draw in pixels. One per
// tab: an older pane is closed and opened fresh, so it follows this session.
async function openHerdrPane($: EngineInterface, cwd: string): Promise<boolean> {
  const self = await $.env.get('HERDR_PANE_ID')
  const herdr = (await $.env.get('HERDR_BIN_PATH')) ?? 'herdr'
  if (!self) return false
  // Claude's own space and tab, asked of herdr by its pane id: the old CSR pane is looked for there,
  // whichever space you are looking at when the session starts.
  const got = await $.process.run([herdr, 'pane', 'get', self])
  let mine: HerdrPane | undefined
  try {
    mine = got.exitCode === 0 ? JSON.parse(got.stdout).result?.pane : undefined
  } catch {}
  const tab = mine?.tab_id
  if (!tab) return false
  const space = mine?.workspace_id ?? tab.split(':')[0] ?? ''
  const listed = await $.process.run([herdr, 'pane', 'list', '--workspace', space])
  if (listed.exitCode !== 0) return false
  let panes: HerdrPane[] = []
  try {
    panes = JSON.parse(listed.stdout).result?.panes ?? []
  } catch {
    return false
  }
  for (const pane of panes) {
    if (pane.tab_id === tab && (pane.label === LABEL || pane.label === 'Shifts')) await $.process.run([herdr, 'pane', 'close', pane.pane_id])
  }
  const split = await $.process.run([
    herdr, 'pane', 'split', '--pane', self, '--direction', 'right', '--ratio', '0.62', '--cwd', cwd, '--no-focus',
  ])
  if (split.exitCode !== 0) return false
  let id: string | undefined
  try {
    id = JSON.parse(split.stdout).result?.pane?.pane_id
  } catch {
    return false
  }
  if (!id) return false
  await $.process.run([herdr, 'pane', 'rename', id, LABEL])
  const session = await $.session.id()
  await $.process.run([herdr, 'pane', 'run', id, 'python3', '-I', guard($), '--repo', cwd, '--watch', '--sid', session, '--agent', self])
  return true
}

async function openPane($: EngineInterface, cwd: string): Promise<'herdr' | 'pane'> {
  if (await openHerdrPane($, cwd)) return 'herdr'
  void $.ui.open({ id: PANE, title: LABEL })
  return 'pane'
}

// ---------------------------------------------------------------- /csr statusline

// Claude's own status line moves into the CSR pane only when you ask: the user
// settings' statusLine.command is wrapped in bin/statusline.cjs, which passes the
// line through unless a CSR pane is open beside the session. Settings point at a
// stable copy of the wrapper, never into the plugin's folder: that path changes
// on every update, and the line has to keep working after an uninstall.
const shippedWrapper = ($: EngineInterface) => `${$.plugin.root}/bin/statusline.cjs`
const IS_WRAPPED = /csr-companion\/statusline\.c?js|csr-pane-statusline\.js/
const WRAPPING = 'statusline' // $.store: { original, wrapped }, what "off" puts back
// The settings that win over the user's, lowest first.
const OVER_USER: Array<[SettingsSource, string]> = [
  ['project', 'project settings (.claude/settings.json)'],
  ['local', 'local settings (.claude/settings.local.json)'],
  ['flag', '--settings'],
  ['policy', 'managed policy settings'],
]

type Places = { settings: string; wrapper: string; backup: string }
type UserSettings = { raw: string; json: Record<string, unknown> }
type Wrapping = { original: string; wrapped: string }

async function places($: EngineInterface): Promise<Places | null> {
  const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
  if (!home) return null
  const config = (await $.env.get('CLAUDE_CONFIG_DIR'))?.replace(/\/+$/, '')
  const keep = `${home}/.claude-self-reflect/csr-companion`
  return {
    settings: `${config || `${home}/.claude`}/settings.json`,
    wrapper: `${keep}/statusline.cjs`,
    backup: `${keep}/settings.before-statusline.json`,
  }
}

// The user settings file as written, or why there is none to edit.
async function userSettings($: EngineInterface, path: string): Promise<UserSettings | 'missing' | 'not JSON'> {
  if (!(await $.fs.exists(path))) return 'missing'
  const raw = await $.fs.read(path)
  try {
    const json: unknown = JSON.parse(raw)
    if (json && typeof json === 'object' && !Array.isArray(json)) return { raw, json: json as Record<string, unknown> }
  } catch {}
  return 'not JSON'
}

function commandOf(settings: Settings): string | undefined {
  const line = settings.statusLine as { command?: unknown } | undefined
  return line && typeof line === 'object' && typeof line.command === 'string' && line.command.trim() ? line.command : undefined
}

// Only statusLine.command changes: every other key keeps its place; 2-space
// JSON, ending in a newline when the file did.
async function writeCommand($: EngineInterface, path: string, settings: UserSettings, command: string): Promise<void> {
  const statusLine = { ...(settings.json.statusLine as Record<string, unknown>), command }
  const text = JSON.stringify({ ...settings.json, statusLine }, null, 2)
  await $.fs.write(path, settings.raw.endsWith('\n') ? `${text}\n` : text) // user file: settings.json
}

// Claude Code runs a statusLine command through a shell: the original rides as one single-quoted word.
const shellWord = (text: string) => `'${text.replace(/'/g, `'\\''`)}'`
const wrapCommand = (wrapper: string, original: string) => `node "${wrapper}" --sh ${shellWord(original)}`

// The command a wrapped one runs: --sh's word unquoted, or in the argv form set
// up by hand (node csr-pane-statusline.js <command...>) the words after the wrapper.
function unwrapCommand(command: string): string | null {
  const sh = /\s--sh\s+([\s\S]*)$/.exec(command)
  if (sh) return unquote((sh[1] ?? '').trim())
  const argv = /^\s*node\s+("[^"]*"|'[^']*'|\S+)\s+([\s\S]+)$/.exec(command)
  return argv && IS_WRAPPED.test(argv[1] ?? '') ? (argv[2] ?? '').trim() || null : null
}

// One shell word back to its text: '...' runs, \-escapes and bare characters; null for anything else.
function unquote(word: string): string | null {
  let text = ''
  for (let i = 0; i < word.length; i++) {
    const c = word.charAt(i)
    if (c === "'") {
      const end = word.indexOf("'", i + 1)
      if (end < 0) return null
      text += word.slice(i + 1, end)
      i = end
    } else if (c === '\\' && i + 1 < word.length) text += word.charAt(++i)
    else if (/\s/.test(c)) return null // a second word: not what "on" wrote
    else text += c
  }
  return text || null
}

// Each statusLine set where it wins over the user's, as "where: command".
async function overrides($: EngineInterface): Promise<string[]> {
  const found: string[] = []
  for (const [source, label] of OVER_USER) {
    const settings = await $.settings.read({ source }).catch((): Settings => ({}))
    if (settings.statusLine !== undefined) found.push(`${label}: ${commandOf(settings) ?? '(no command)'}`)
  }
  return found
}

const overrideText = (found: string[]) =>
  `Set where it wins over the user settings, so Claude Code runs it instead of yours:\n${found.map(line => `  ${line}`).join('\n')}`

// "/csr statusline": a preview that writes nothing; "on" wraps the command; "off" puts it back.
async function statusline($: EngineInterface, choice: string | undefined): Promise<string> {
  if (choice && choice !== 'on' && choice !== 'off') {
    return `/csr statusline ${choice}: unknown. "/csr statusline" shows what would change, "on" moves the line into the CSR pane, "off" puts it back.`
  }
  const where = await places($)
  if (!where) return "/csr statusline: HOME isn't set, so there is no user settings file to read. Nothing changed."
  const settings = await userSettings($, where.settings)
  if (settings === 'not JSON') return `${where.settings} is not a JSON object CSR can edit. Nothing changed.`
  const command = typeof settings === 'string' ? undefined : commandOf(settings.json)
  if (!command || typeof settings === 'string') {
    return `Your user settings (${where.settings}) have no statusLine command: there is nothing to ${choice === 'off' ? 'put back' : 'move'}. Nothing changed.`
  }
  if (choice === 'off') return unwrapStatusline($, where, settings, command)
  if (IS_WRAPPED.test(command)) {
    return `Your status line is already wrapped for the CSR pane:\n  ${command}\nNothing changed. "/csr statusline off" puts the original back.`
  }
  const wrapped = wrapCommand(where.wrapper, command)
  const found = await overrides($)
  if (choice !== 'on') {
    return [
      `Your status line, from ${where.settings}:`,
      `  ${command}`,
      '"/csr statusline on" would change it to:',
      `  ${wrapped}`,
      'That runs your command as now. Its line shows in the CSR pane while one is open beside the session, and under the prompt otherwise. Settings point at a copy of the wrapper outside the plugin, so the line keeps working after an update or an uninstall.',
      found.length ? overrideText(found) : 'No project, local, --settings or policy statusLine wins over yours.',
      'Nothing changed. Next: /csr statusline on',
    ].join('\n')
  }
  // The wrapper runs under node: without one on PATH the status line would go blank.
  const node = await $.process.run(['node', '--version'], { timeoutMs: 5_000 }).catch(() => null)
  if (!node || node.exitCode !== 0) {
    return `"/csr statusline on" runs your status line through a small node script, and node didn't run here. Nothing changed.`
  }
  await keepWrite($, where.wrapper, await $.fs.read(shippedWrapper($)))
  await keepWrite($, where.backup, settings.raw) // settings.json can hold tokens: kept 0600, and as it was
  await writeCommand($, where.settings, settings, wrapped)
  await $.store.set(WRAPPING, { original: command, wrapped } satisfies Wrapping)
  return [
    `Status line wrapped for the CSR pane. ${where.settings} now runs:`,
    `  ${wrapped}`,
    `The settings as they were are saved at ${where.backup}. "/csr statusline off" puts your command back.`,
    ...(found.length ? [overrideText(found)] : []),
  ].join('\n')
}

async function unwrapStatusline($: EngineInterface, where: Places, settings: UserSettings, command: string): Promise<string> {
  if (!IS_WRAPPED.test(command)) return `Your status line isn't wrapped for the CSR pane:\n  ${command}\nNothing changed.`
  const kept = (await $.store.get(WRAPPING).catch(() => undefined)) as Partial<Wrapping> | undefined
  // The kept original while settings still hold what "on" wrote; otherwise it is read off the command.
  const original = kept?.wrapped === command && typeof kept.original === 'string' ? kept.original : unwrapCommand(command)
  if (!original) {
    return `Couldn't find the original command in:\n  ${command}\nNothing changed. Edit statusLine.command in ${where.settings} by hand.`
  }
  await writeCommand($, where.settings, settings, original)
  await $.store.delete(WRAPPING).catch(() => undefined)
  return `Status line back under the prompt. ${where.settings} runs your command again:\n  ${original}`
}

// The stable copy follows the wrapper the plugin ships. Only "/csr statusline on" makes one.
async function refreshWrapper($: EngineInterface): Promise<void> {
  const where = await places($)
  if (!where || !(await $.fs.exists(where.wrapper))) return
  const shipped = await $.fs.read(shippedWrapper($))
  if (shipped !== (await $.fs.read(where.wrapper))) await keepWrite($, where.wrapper, shipped)
}

// ---------------------------------------------------------------- compaction marker

// <HOME>/.claude-self-reflect/csr-companion/compacting/<session>.json, {at, trigger}: bin/pane.py reads it.
async function markCompacting($: EngineInterface, trigger: string): Promise<string | null> {
  try {
    const home = (await $.env.get('HOME'))?.replace(/\/+$/, '')
    if (!home) return null
    const path = `${home}/.claude-self-reflect/csr-companion/compacting/${await $.session.id()}.json`
    await keepWrite($, path, JSON.stringify({ at: (await $.clock.now()) / 1000, trigger }))
    return path
  } catch {
    return null
  }
}

// Removed with rm; where that cannot run, the marker is overwritten as ended, which the pane reads as none.
async function unmarkCompacting($: EngineInterface, path: string): Promise<void> {
  try {
    await $.process.run(['rm', '-f', path], { timeoutMs: 5_000 })
    if (await $.fs.exists(path)) await keepWrite($, path, JSON.stringify({ ended: true }))
  } catch {}
}

function byCommit(shifts: Shift[]): Array<[string, Shift[]]> {
  const groups = new Map<string, Shift[]>()
  for (const shift of shifts) {
    const key = `${shift.date.slice(5)} ${shift.oid.slice(0, 7)} ${shift.subject ?? ''}`
    groups.set(key, [...(groups.get(key) ?? []), shift])
  }
  return [...groups.entries()]
}

const MARK: Record<string, string> = { changed: '~', removed: '-', 'came back': '+' }
