import { atom, read, update } from 'claude-code'
import type { EngineInterface, ModelUsage, Register, Settings, SettingsSource } from 'claude-code'

import type { Note, NoteView, Shift, ShiftsData } from '../types'
import { askClaude, dreamId, dreamPrompt, parseDream, sameDream, standsOnThePast } from './dreamer'
import { canAfford, facts, freshPace, hasNews, paceAfter, paceText } from './pace'
import type { Pace } from './pace'

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
const OFF_ROW = 'csr-companion.dreams'

const data = atom({ plugin: 'csr-companion', key: 'data' } as const, null as ShiftsData | null)
const view = atom({ plugin: 'csr-companion', key: 'view' } as const, null as NoteView | null)

type HerdrPane = { pane_id: string; tab_id?: string; label?: string | null }

async function run($: EngineInterface, argv: string[]): Promise<string | null> {
  const ran = await $.process.run(['python3', '-I', script($), ...argv], { timeoutMs: 20_000 })
  return ran.exitCode === 0 ? ran.stdout : null
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

async function remember($: EngineInterface, key: string, value: string, kept = KEPT): Promise<void> {
  const list = await strings($, key)
  if (!list.includes(value)) await $.store.set(key, [...list, value].slice(-kept))
}

// What each note came to, kept across sessions: the "at least 1 in 3 acted on" gate
// reads this. A dream's answer also goes to the dream log, where the pane draws it.
async function answered($: EngineInterface, note: Note, answer: string, logged = false): Promise<void> {
  const at = await $.clock.now()
  const value = await $.store.get('answers').catch(() => undefined)
  const list = Array.isArray(value) ? value : []
  await $.store.set('answers', [...list, { id: note.id, answer, at }].slice(-KEPT))
  if (note.kind !== 'dream' || answer === 'shown') return
  if (answer !== 'ignored') {
    // You answered: the dreamer stops backing off.
    await $.store.set('typedPastInARow', 0)
    await $.store.set('looksToSkip', 0)
  }
  const pace = await paceOf($)
  await $.store.set('pace', paceAfter(pace, answer))
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

async function refresh($: EngineInterface, cwd: string): Promise<ShiftsData | null> {
  const fresh = await json<ShiftsData>($, ['--repo', cwd, '--json', '--limit', '60', '--sid', await $.session.id()])
  await update($, data, () => fresh)
  return fresh
}

// Shows a note unless another is up; true when it was shown.
async function show($: EngineInterface, note: Note, tag: NoteView['tag']): Promise<boolean> {
  const shownAt = await $.clock.now()
  let isShown = false
  await update($, view, (current): NoteView | null => {
    if (current && current.kind !== 'asked') return current // one note at a time
    isShown = true
    return { kind: 'offer', note, tag, shownAt, promptsSurvived: 0 }
  })
  if (isShown) await answered($, note, 'shown')
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
  return [
    'Here is a note offered by CSR (claude-self-reflect):',
    '',
    `> ${shown.tag} · ${note.line}`,
    '>',
    `> ${note.text}`,
    '>',
    `> Receipts: ${receipts}.`,
  ].join('\n')
}

// ---------------------------------------------------------------- the dreamer

async function paceOf($: EngineInterface): Promise<Pace> {
  const session = await $.session.id()
  const value = (await $.store.get('pace').catch(() => undefined)) as Pace | undefined
  return value?.session === session ? { ...freshPace(session), ...value } : freshPace(session)
}

const tokensOf = (usage: ModelUsage) =>
  usage.input_tokens + usage.output_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens

// One look, when the pace allows it: a fork of this session asking for one "what if...?".
async function dream($: EngineInterface, lastStep: number): Promise<void> {
  const skip = await count($, 'looksToSkip')
  if (skip > 0) {
    await $.store.set('looksToSkip', skip - 1)
    return
  }
  if (await isBusy($)) return // a note is up: no question would be seen
  let pace = await paceOf($)
  if (!canAfford(pace, lastStep)) return
  const cwd = await $.session.cwd()
  const session = await $.session.id()
  const brief = (await run($, ['--repo', cwd, '--brief', '--sid', session])) ?? ''
  const now = facts(brief)
  const before = await strings($, 'briefFacts')
  if (!hasNews(now, before)) return // nothing new near this work since the last look: 0 tokens
  const seen = await strings($, 'seenDreams')
  const known = await strings($, 'knownDreams')
  const reply = await $.model.fork({ prompt: dreamPrompt(brief, seen, known) })
  const used = 'usage' in reply ? tokensOf(reply.usage) : 0
  await $.store.set('briefFacts', [...before.filter(line => !now.includes(line)), ...now].slice(-FACTS_KEPT))
  pace = { ...(await paceOf($)), looks: pace.looks + 1, dreamer: pace.dreamer + used, lastLook: used || pace.lastLook }
  const parsed = reply.isAnswered ? parseDream(reply.text) : null
  const found = parsed && standsOnThePast(parsed, brief, session) ? parsed : null // another session under it
  if (!found || [...seen, ...known].some(line => sameDream(line, found.line))) {
    await $.store.set('pace', paceAfter(pace, 'none'))
    return
  }
  await $.store.set('pace', { ...pace, dreams: pace.dreams + 1 })
  const at = await $.clock.now()
  const note: Note = {
    id: dreamId(found.line, at),
    kind: 'dream',
    file: '',
    line: found.line,
    title: found.title,
    text: found.text,
    sketch: [],
    symbol: null,
    oid: null,
    conversation: session,
    at,
    tokens: used,
  }
  if (!(await show($, note, 'Dream'))) return
  await remember($, 'seenDreams', found.line, SEEN_KEPT)
  await run($, ['--repo', cwd, '--keep', JSON.stringify({
    kind: 'dream', id: note.id, at: at / 1000, session, line: note.line, title: note.title, text: note.text, tokens: used,
    brief, // what the dreamer read: with the answer that settles this dream, one labelled example
  })])
  void refresh($, cwd)
}

async function dreamsText($: EngineInterface, on: boolean): Promise<string> {
  return paceText(on ? await paceOf($) : null, on)
}

// The /config row, found by its plugin's prefix: a folder-loaded plugin may carry "@inline".
async function dreamsRow($: EngineInterface): Promise<string> {
  const rows = await $.config.list().catch(() => [])
  return rows.find(row => /^csr-companion(@[^.]+)?\.dreams$/.test(row.key))?.key ?? OFF_ROW
}

// Whether a band holds a Button with a hotkey: it is asking the person something.
function asks(node: unknown): boolean {
  if (!node || typeof node !== 'object') return false
  const element = node as { type?: string; props?: { hotkey?: unknown }; children?: unknown[] }
  if (element.type === 'Button' && element.props?.hotkey) return true
  return (element.children ?? []).some(asks)
}

// ---------------------------------------------------------------- hooks

export const register: Register = (on, options) => {
  const dreamsOn = options.dreams === true
  let steps = 0
  let dreaming = false
  // True while another band (You should know's) asks, and the note waits behind it.
  let waiting = false

  on('turn.step', async function* ($, e, next) {
    const result = yield* next(e)
    if (!dreamsOn || e.agentId !== undefined) return result
    // Count what Claude spends: the dreamer's share is measured against it.
    const spent = result?.usage ? tokensOf(result.usage) : 0
    const pace = await paceOf($)
    if (spent) await $.store.set('pace', { ...pace, claude: pace.claude + spent })
    if (++steps >= pace.every && !dreaming) {
      steps = 0
      dreaming = true
      void dream($, spent)
        .catch(() => undefined)
        .finally(() => {
          dreaming = false
        })
    }
    return result
  })

  on('tool.call', async ($, e, next) => {
    if (e.agentId === undefined && FILE_TOOLS.has(e.tool)) {
      const file = 'file_path' in e && typeof e.file_path === 'string'
        ? e.file_path
        : 'notebook_path' in e && typeof e.notebook_path === 'string' ? e.notebook_path : undefined
      if (file) void offerFor($, file, e.tool === 'Read' ? 'You should know' : 'Heads up').catch(() => undefined)
    }
    return next(e)
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the call go on

  on('prompt.submit', async ($, e, next) => {
    await settledInThePane($).catch(() => undefined) // answered there: never counted as typed past
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
        const inARow = (await count($, 'typedPastInARow')) + 1
        await $.store.set('typedPastInARow', inARow)
        await $.store.set('looksToSkip', Math.min(SKIP_MAX, 2 ** (inARow - 1)))
      }
    }
    return next(e)
  }).catch((_$, e, next) => next(e)) // never a guard: a failure here lets the prompt go on

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    const beneath = await next(e)
    if (e.props.hasSurvey) return beneath
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
              onPress={() => void button.onPress()}
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
          <Text dimColor>{shown.tag} · </Text>
          {body}
        </Text>
      </Box>
    )

    let ours
    if (shown.kind === 'offer') {
      ours = (
        <Box flexDirection="column" marginTop={1}>
          {lead(note.line)}
          {buttons([
            { hotkey: '1', label: 'Learn more', onPress: () => update($, view, () => ({ ...shown, kind: 'explained' })) },
            {
              hotkey: '2',
              label: isDream ? 'Already thought of it' : 'Knew this already',
              onPress: async () => {
                if (isDream) await remember($, 'knownDreams', note.line, SEEN_KEPT)
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

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    if (!e.isInteractive) return started
    $.ui.status(undefined) // CSR pins no line under the prompt: the pane carries what it knows
    await $.command.register({
      name: 'csr',
      description:
        'Open the CSR pane beside Claude; "/csr dreams" says how the dreamer is pacing itself; "/csr statusline" moves your status line into the pane',
    })
    void refreshWrapper($).catch(() => undefined) // never blocks or fails the start
    void (async () => {
      const fresh = await refresh($, e.cwd)
      if (fresh?.repo) await openPane($, e.cwd)
      $.clock.every(REFRESH_MS, () => void refresh($, e.cwd))
      $.clock.every(SETTLED_IN_PANE_MS, () => void settledInThePane($).catch(() => undefined))
    })()
    return started
  })

  on('command.run', { command: 'csr' }, async ($, e) => {
    const [sub, choice] = e.args.trim().split(/\s+/)
    if (sub === 'dreams') return { text: await dreamsText($, dreamsOn) }
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

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    const shown = await read($, data)
    const room = Math.max(3, (e.viewport?.rows ?? 24) - 4)
    if (!shown) return <Text dimColor>csr-engine could not be read.</Text>
    const lines: Array<{ text: string; bold?: boolean; dim?: boolean }> = [
      { text: shown.status ?? '', dim: true },
      { text: `${STAR} Dreams`, bold: true },
    ]
    if (!shown.dreams.length) lines.push({ text: dreamsOn ? '  None yet.' : '  Dreams are off: /config, csr-companion, Dreams.', dim: true })
    for (const entry of shown.dreams.slice(0, 6)) {
      const when = new Date(entry.at * 1000).toISOString().slice(5, 16).replace('T', ' ')
      lines.push({ text: `  ${when} · ${entry.answer ?? 'open'}`, dim: true })
      lines.push({ text: `  ${entry.line}` })
    }
    lines.push({ text: 'Changed since your sessions', bold: true })
    for (const change of shown.changes) {
      const latest = change.shifts[0]
      lines.push({ text: `● ${change.talked.slice(5)} ${change.title}`, bold: true })
      if (latest) {
        const names = change.shifts.filter(s => s.oid === latest.oid).map(s => `${MARK[s.verb] ?? ''}${s.symbol ?? '(file)'}`)
        lines.push({ text: `  ${latest.date.slice(5)} ${latest.oid.slice(0, 7)} ${names.join(' ')}`, dim: true })
      }
    }
    lines.push({ text: 'Change log', bold: true })
    for (const [head, group] of byCommit(shown.shifts)) {
      lines.push({ text: head, bold: true })
      for (const shift of group) {
        const file = shift.file.split('/').slice(-2).join('/')
        lines.push({ text: `  ${MARK[shift.verb] ?? ''}${shift.symbol ?? '(whole file)'}  ${file}` })
      }
    }
    return (
      <Box flexDirection="column">
        {lines.slice(0, room).map(line => (
          <Text bold={line.bold} dimColor={line.dim} wrap="truncate-end">
            {line.text}
          </Text>
        ))}
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
  const listed = await $.process.run([herdr, 'pane', 'list'])
  if (listed.exitCode !== 0) return false
  let panes: HerdrPane[] = []
  try {
    panes = JSON.parse(listed.stdout).result?.panes ?? []
  } catch {
    return false
  }
  const tab = panes.find(pane => pane.pane_id === self)?.tab_id
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
  await $.process.run([herdr, 'pane', 'run', id, 'python3', '-I', script($), '--repo', cwd, '--watch', '--sid', session, '--agent', self])
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
  await $.fs.write(path, settings.raw.endsWith('\n') ? `${text}\n` : text)
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
  await $.fs.write(where.wrapper, await $.fs.read(shippedWrapper($)))
  await $.fs.write(where.backup, settings.raw)
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
  if (shipped !== (await $.fs.read(where.wrapper))) await $.fs.write(where.wrapper, shipped)
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
