import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On, SettingsSource } from 'claude-code'
import { facts, freshChapter, median, spacing } from '../hooks/pace'
import { addTerm, commands, gitVerdict, isBareDigit, NO_VERIFY, RELEASE } from '../hooks/coach'

// The test runner has timers; the hooks environment's typings do not declare them.
declare const setTimeout: (fn: () => void, ms: number) => unknown

const ROWS = [
  { file: 'src/setup.rs', symbol: 'handle', verb: 'changed', oid: 'fdaba2308c80', date: '2026-09-21', subject: 'fix(installer)' },
  { file: 'src/lib.rs', symbol: null, verb: 'changed', oid: 'fdaba2308c80', date: '2026-09-21', subject: 'fix(installer)' },
]
const SHIFTS = {
  repo: '/repo',
  total: 2,
  status: '☾ CSR · 0 dreams/7d · 1 changed since · reminded agent 3×/7d · 0 tok',
  reminded_7d: 3,
  shifts: ROWS,
  changes_total: 1,
  changes: [{ conversation: 'c1', title: 'remove the dream statusline', current: false, talked: '2026-09-20', commits: 1, shifts: ROWS }],
  dreams: [],
  dreams_7d: 0,
  dream_tokens_7d: 0,
}
const NOTE = {
  id: 'shift:src/hooks/mod.rs:ca5655a841fb:d8fe3a96',
  kind: 'shift',
  file: 'src/hooks/mod.rs',
  line: '`dispatch_hook` in mod.rs changed in ca5655a on 09-17, after your 09-14 session worked on it.',
  title: 'Your 09-14 work on dispatch_hook has changed since',
  text: 'On 09-14 you asked “switch to main”. That session worked on `dispatch_hook`.',
  sketch: ['09-14  ● your session', '09-17  ▲ ca5655a  dispatch_hook'],
  symbol: 'dispatch_hook',
  oid: 'ca5655a841fb',
  conversation: 'd8fe3a96-e6f8',
  at: 1,
}
const NOW = Date.UTC(2026, 9, 9, 12)
const MESSAGES = [{ role: 'user', text: 'one', toolUses: [] }, { role: 'assistant', text: 'done', toolUses: [] }]
const BRIEF = 'Things the person built in other sessions that came up in this work:\n* skill notes-sync: built over 2 sessions, latest 08-26 [session dc24d328]'
const DREAM_LINE = 'What if the notes also read your side branches, so this week’s work gets notes before it reaches main?'
const DREAM_REPLY = [
  `dream: ${DREAM_LINE}`,
  'explain:',
  '**Notes could follow side branches**',
  '* CSR only checks main, which last moved on 09-21.',
  '* On 08-26 you asked for CSR across projects [session dc24d328].',
].join('\n')
const USAGE = { input_tokens: 900, output_tokens: 100, cache_creation_input_tokens: 0, cache_read_input_tokens: 40_000 }

type World = {
  herdr: boolean
  repo: boolean
  notes?: unknown[]
  coach?: unknown[]
  clock?: { now: number } // moved by a test; NOW where left out
  unreadable?: string[] // files that are there but fail to read
  box?: string
  replies?: string[]
  brief?: string
  youShouldKnow?: boolean
  // A file system and settings of the test's own: nothing here reaches the real ones.
  files?: Record<string, string>
  sources?: Partial<Record<SettingsSource, Record<string, unknown>>>
  configDir?: string
  noNode?: boolean
  forkGate?: Promise<void> // a fork that answers only once this settles
  git?: Record<string, string> // what `git -C dir <args>` prints, keyed by the args; missing ones fail
  compactFails?: boolean
  lost?: unknown[] | string // what pane.py --lost prints (a string as is, 'hangs' a timeout); undefined makes it fail
}
const HOME = '/home/u'
const MARKER = `${HOME}/.claude-self-reflect/csr-companion/compacting/sess-1.json`

function host(on: On, world: World) {
  const runs: string[][] = []
  const statuses: Array<string | undefined> = []
  const opened: string[] = []
  const filled: string[] = []
  const forks: string[] = []
  const kept: Array<Record<string, unknown>> = []
  const compacted: Array<string | undefined> = [] // the instructions each compaction reached core with
  const configured: Array<{ key: string; value: unknown }> = []
  const store = new Map<string, unknown>()
  const files = new Map<string, string>(Object.entries(world.files ?? {}))
  const writes: string[] = []
  const ok = (stdout: string) => ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.register', async (_$, e) => ({ value: { command: e.name } }))
  on('clock.every', async () => ({ value: undefined }))
  on('clock.now', async () => ({ value: world.clock?.now ?? NOW }))
  const toasts: string[] = []
  on('ui.toast', async (_$, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  const compacting: Array<string | undefined> = [] // the compaction marker as each compaction ran
  on('session.compact', async (_$, e) => {
    compacting.push(files.get(MARKER))
    compacted.push(e.instructions)
    if (world.compactFails) throw new Error('summary failed')
    return { messages: e.messages } as never
  })
  on('session.id', async () => ({ value: 'sess-1' }))
  on('session.cwd', async () => ({ value: '/repo' }))
  on('store.get', async (_$, e) => ({ value: store.get(e.key) }))
  on('store.set', async (_$, e) => {
    store.set(e.key, e.value)
    return { value: undefined }
  })
  on('store.delete', async (_$, e) => {
    store.delete(e.key)
    return { value: undefined }
  })
  on('fs.read', async (_$, e) => {
    if (e.path.endsWith('/csr-companion/bin/statusline.cjs')) return { value: SHIPPED } // the plugin's own copy
    if (world.unreadable?.includes(e.path)) return { deny: `EACCES: ${e.path}` }
    const text = files.get(e.path)
    return text === undefined ? { deny: `ENOENT: ${e.path}` } : { value: text }
  })
  on('fs.exists', async (_$, e) => ({ value: files.has(e.path) }))
  on('fs.write', async (_$, e) => {
    files.set(e.path, e.text)
    writes.push(e.path)
    return { value: undefined }
  })
  on('settings.read', async (_$, e) => ({ value: world.sources?.[e.source ?? 'user'] ?? {} }))
  on('prompt.read', async () => ({ value: { text: world.box ?? '', cursor: 0 } }))
  on('prompt.fill', async (_$, e) => {
    filled.push(e.text)
    return { isFilled: true }
  })
  on('prompt.submit', async (_$, e) => ({ text: e.text }))
  on('tool.call', async () => ({ result: 'file text' }) as never)
  // Each step of Claude's re-reads about as much as one look of the dreamer's.
  on('turn.step', async function* (_$, e) {
    return { turnId: e.turnId, index: e.index, answer: '', toolUses: [], stopReason: 'end_turn', usage: { ...USAGE, model: 'opus' } } as never
  })
  on('config.list', async () => ({ value: [{ key: 'csr-companion.dreams', label: 'Dreams', kind: 'boolean', value: false }] }) as never)
  on('config.set', async (_$, e) => {
    configured.push({ key: e.key, value: e.value })
    return { value: e.value }
  })
  on('model.fork', async (_$, e) => {
    forks.push(e.prompt)
    if (world.forkGate) await world.forkGate
    const text = world.replies?.[forks.length - 1] ?? 'dream: none'
    return { value: { isAnswered: true, text, usage: USAGE } }
  })
  // The band beneath: empty, or You should know's note with its own 4: Disable.
  on('ui.render', async ($, e) => {
    const { Box, Button } = $.ui.resolve(e)
    if (!world.youShouldKnow) return h(Box, {}) as never
    return h(Box, {}, h(Button, { key: 'ysk-disable', hotkey: '4', plain: true, label: 'Disable', onPress: () => undefined })) as never
  })
  on('env.get', async (_$, e) => {
    if (world.herdr && e.name === 'HERDR_PANE_ID') return { value: 'w2:p1' }
    if (world.herdr && e.name === 'HERDR_BIN_PATH') return { value: '/bin/herdr' }
    if (e.name === 'HOME') return { value: HOME }
    if (e.name === 'CLAUDE_CONFIG_DIR') return { value: world.configDir }
    return { value: undefined }
  })
  on('process.run', async (_$, e) => {
    runs.push([...e.argv])
    if (e.argv[0] === 'rm') {
      for (const path of e.argv.slice(2)) files.delete(path)
      return ok('')
    }
    if (e.argv[0] === 'git') {
      const said = world.git?.[e.argv.slice(3).join(' ')]
      return said === undefined ? { value: { exitCode: 1, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } } : ok(said)
    }
    if (e.argv[0] === 'node' && world.noNode) return { value: { exitCode: 127, stdout: '', stderr: 'node: not found', isStdoutTruncated: false, isStderrTruncated: false } }
    if (e.argv[0] === 'python3' && e.argv.includes('--note')) return ok(JSON.stringify(world.notes ?? []))
    if (e.argv[0] === 'python3' && e.argv.includes('--coach')) return ok(JSON.stringify(world.coach ?? []))
    if (e.argv[0] === 'python3' && e.argv.includes('--lost')) {
      if (world.lost === 'hangs') { // process.run rejects past timeoutMs; without the 2 s limit it 'answers' instead
        if (e.init?.timeoutMs !== 2000) return ok(JSON.stringify([{ text: 'no time limit passed', state: 'lost' }]))
        return { deny: 'timed out after 2000 ms' }
      }
      if (typeof world.lost === 'string') return ok(world.lost)
      return world.lost ? ok(JSON.stringify(world.lost)) : { value: { exitCode: 1, stdout: '', stderr: 'boom', isStdoutTruncated: false, isStderrTruncated: false } }
    }
    if (e.argv[0] === 'python3' && e.argv.includes('--brief')) return ok(world.brief ?? BRIEF)
    if (e.argv[0] === 'python3' && e.argv.includes('--keep')) {
      kept.push(JSON.parse(e.argv[e.argv.indexOf('--keep') + 1] ?? '{}'))
      return ok('{}')
    }
    if (e.argv[0] === 'python3') {
      return ok(JSON.stringify(world.repo ? SHIFTS : { repo: null, total: 0, shifts: [], changes: [], changes_total: 0, dreams: [], status: null }))
    }
    if (e.argv[2] === 'get') return ok(JSON.stringify({ result: { pane: { pane_id: e.argv[3], tab_id: 'w2:t1', workspace_id: 'w2' } } }))
    if (e.argv[2] === 'list') {
      if (e.argv[3] !== '--workspace' || e.argv[4] !== 'w2') return ok(JSON.stringify({ result: { panes: [] } })) // only Claude's own space
      return ok(JSON.stringify({ result: { panes: [
        { pane_id: 'w2:p1', tab_id: 'w2:t1' },
        { pane_id: 'w2:p2', tab_id: 'w2:t1', label: 'Shifts' },
      ] } }))
    }
    if (e.argv[2] === 'split') return ok(JSON.stringify({ result: { pane: { pane_id: 'w2:p9' } } }))
    return ok('{}')
  })
  on('ui.status', async (_$, e) => {
    statuses.push(e.text)
    return { value: undefined }
  })
  on('ui.open', async (_$, e) => {
    opened.push(e.id)
    return { value: { isPlaced: true } }
  })
  return { runs, statuses, opened, filled, forks, kept, store, configured, files, writes, compacting, toasts, compacted }
}

async function settle(done: () => boolean | Promise<boolean>) {
  for (let i = 0; i < 200 && !(await done()); i++) await new Promise<void>(resolve => setTimeout(() => resolve(), 5))
}

// Steps of Claude's main loop (or a subagent's, with agentId).
async function steps($: Engine, n: number, agentId?: string) {
  for (let index = 0; index < n; index++) {
    const stream = $.turn.step({ turnId: 'turn-1', index, model: 'opus', messageCount: 4, agentId } as never)
    for await (const _ of stream) void _
  }
}

const START = { cwd: '/repo', surface: 'terminal' as const, isInteractive: true, startedAt: 0 }
const BAND = {
  plugin: 'csr-companion',
  surface: 'terminal' as const,
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 24, bodyColumns: 100, scroll: { offset: 0, bodyRows: 24 } } as never,
}
const DREAMS = { command: 'csr', args: 'dreams', origin: { kind: 'human' } } as never
const ON = { options: { dreams: true } }
const LOOK = 41_000 // USAGE, all four counts

test('session start in herdr: no status line, old pane closed, new split watches this session', async ($, on) => {
  const seen = host(on, { herdr: true, repo: true })
  await $.session.start(START as never)
  await settle(() => seen.runs.some(argv => argv.includes('--watch')))

  expect(seen.statuses).toContain(undefined) // CSR pins no line under the prompt
  expect(seen.statuses).not.toContain(SHIFTS.status)
  expect(seen.runs).toContainEqual(['/bin/herdr', 'pane', 'close', 'w2:p2'])
  expect(seen.runs.find(argv => argv[2] === 'split')).toEqual(expect.arrayContaining(['--pane', 'w2:p1', '--direction', 'right', '--no-focus']))
  expect(seen.runs).toContainEqual(['/bin/herdr', 'pane', 'rename', 'w2:p9', 'CSR'])
  const watcher = seen.runs.find(argv => argv.includes('--watch'))
  expect(watcher?.slice(0, 4)).toEqual(['/bin/herdr', 'pane', 'run', 'w2:p9'])
  expect(watcher?.slice(-4)).toEqual(['--sid', 'sess-1', '--agent', 'w2:p1']) // a second click on a row types into Claude's pane
  expect(seen.runs.find(argv => argv[0] === 'python3')).toEqual(expect.arrayContaining(['--json', '--sid', 'sess-1']))
  expect(seen.opened).toEqual([])
})

test('session start outside herdr opens the Claude Code pane instead', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await $.session.start(START as never)
  await settle(() => seen.opened.length > 0)

  expect(seen.opened).toEqual(['csr'])
  expect(seen.runs.some(argv => argv[0] === '/bin/herdr' || argv[0] === 'herdr')).toBe(false)
})

test('outside a git repo: no status text, no pane opened', async ($, on) => {
  const seen = host(on, { herdr: true, repo: false })
  await $.session.start(START as never)
  await settle(() => seen.statuses.length > 0)
  await settle(() => false)

  expect(seen.statuses.length).toBeGreaterThan(0)
  expect(seen.statuses.every(text => text === undefined)).toBe(true)
  expect(seen.runs.some(argv => argv[2] === 'split')).toBe(false)
  expect(seen.opened).toEqual([])
})

test('an edit to a file CSR remembers offers one note; Learn more opens it; Chat hands it to Claude', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, notes: [NOTE] })
  const band = await $.ui.mount(BAND)
  await $.tool.call({ tool: 'Edit', file_path: '/repo/src/hooks/mod.rs', old_string: 'a', new_string: 'b' } as never)
  await settle(async () => (await band.find({ text: /changed in ca5655a/ })) !== undefined)

  expect(await band.find({ text: /Heads up/ })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Learn more' })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Knew this already' })).toBeDefined()
  expect(seen.runs.find(argv => argv.includes('--note'))).toEqual(expect.arrayContaining(['--note', '/repo/src/hooks/mod.rs']))

  await band.press({ key: 'Learn more' })
  expect(await band.find({ text: NOTE.title })).toBeDefined()
  expect(await band.find({ text: /0 model tokens/ })).toBeDefined()

  await band.press({ key: 'Chat in main session' })
  expect(seen.filled.length).toBe(1)
  expect(seen.filled[0]).toContain('Here is a note offered by CSR')
  expect(seen.filled[0]).toContain(NOTE.line)
  expect(seen.filled[0]).toContain('commit ca5655a')
  expect(await band.find({ text: /changed in ca5655a/ })).toBeUndefined()
})

test('a note is shown once: Knew this already keeps it away for good', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, notes: [NOTE] })
  const band = await $.ui.mount(BAND)
  await $.tool.call({ tool: 'Read', file_path: '/repo/src/hooks/mod.rs' } as never)
  await settle(async () => (await band.find({ text: /You should know/ })) !== undefined)

  await band.press({ key: 'Knew this already' })
  expect(seen.store.get('known')).toEqual([NOTE.id])
  expect(await band.find({ text: /changed in ca5655a/ })).toBeUndefined()

  await $.tool.call({ tool: 'Read', file_path: '/repo/other/mod.rs' } as never)
  await settle(() => seen.runs.filter(argv => argv.includes('--note')).length >= 2)
  await settle(() => false)
  expect(await band.find({ text: /changed in ca5655a/ })).toBeUndefined()
})

test('a subagent reading the file raises no note, and an unanswered note clears after two prompts', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, notes: [NOTE] })
  const band = await $.ui.mount(BAND)
  await $.tool.call({ tool: 'Read', file_path: '/repo/src/hooks/mod.rs', agentId: 'sub-1' } as never)
  await settle(() => false)
  expect(seen.runs.some(argv => argv.includes('--note'))).toBe(false)

  await $.tool.call({ tool: 'Read', file_path: '/repo/src/hooks/mod.rs' } as never)
  await settle(async () => (await band.find({ text: /changed in ca5655a/ })) !== undefined)
  await $.prompt.submit({ text: 'one', origin: { kind: 'human' } } as never)
  expect(await band.find({ text: /changed in ca5655a/ })).toBeDefined()
  await $.prompt.submit({ text: 'two', origin: { kind: 'human' } } as never)
  await settle(async () => (await band.find({ text: /changed in ca5655a/ })) === undefined)
  expect(await band.find({ text: /changed in ca5655a/ })).toBeUndefined()
  const answers = seen.store.get('answers') as Array<{ answer: string }>
  expect(answers.map(a => a.answer)).toEqual(['shown', 'ignored'])
})

test('dreams are off by default, as You should know is: no look, however long Claude works', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  await steps($, 30)
  await settle(() => false)
  expect(seen.forks).toEqual([])
  const said = JSON.stringify(await $.command.run(DREAMS))
  expect(said).toContain('Dreams are off')
  expect(said).toContain('/config')
})

test('on: the first look waits until Claude has spent 10 tokens for each one it costs; subagents do not count', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  const band = await $.ui.mount(BAND)
  await steps($, 6) // 6 steps of Claude's: a look would be 1 in 6, over the share
  await steps($, 12, 'sub-1')
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(seen.kept[0]).toEqual(expect.objectContaining({ kind: 'chapter', why: 'start', left: 5, of: 5, session: 'sess-1' }))
  expect(seen.kept.slice(1)).toEqual([expect.objectContaining({ kind: 'look', outcome: 'budget', every: 6, session: 'sess-1' })])

  await steps($, 6) // 12 steps: 1 in 12, within it
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  expect(seen.forks.length).toBe(1)
  expect(seen.forks[0]).toContain(BRIEF)
  expect(seen.forks[0]).toContain('dream: none')
  expect(seen.forks[0]).toContain('You should know')
  expect(await band.find({ text: /Dream · / })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Already thought of it' })).toBeDefined()
  expect(seen.store.get('pace')).toEqual(expect.objectContaining({ looks: 1, dreams: 1, dreamer: LOOK, claude: 12 * LOOK }))
  // 4 left in the chapter, spread over the rest of a usual one: (120 - 12) / 4 = 27 steps
  expect(seen.kept.find(record => record.kind === 'dream')).toEqual(expect.objectContaining({
    line: DREAM_LINE, tokens: LOOK, session: 'sess-1', outcome: 'dream', left: 4, of: 5, every: 27,
  }))

  await band.press({ key: 'Learn more' })
  expect(await band.find({ text: 'Notes could follow side branches' })).toBeDefined()
  expect(await band.find({ text: /this look 41,000 tokens/ })).toBeDefined()

  await band.press({ key: 'Ask Claude' })
  expect(seen.filled.length).toBe(1)
  expect(seen.filled[0]?.startsWith(DREAM_LINE)).toBe(true)
  expect(seen.filled[0]).toContain('> **Notes could follow side branches**')
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'answer', answer: 'ask_claude' }))
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(seen.store.get('chapter')).toEqual(expect.objectContaining({ shown: 1, adopted: 1 })) // adopted: its place back
  const said = JSON.stringify(await $.command.run(DREAMS))
  expect(said).toContain('1 look, 1 dream')
  expect(said).toContain('5 of 6 dreams left in this chapter')
})

test('a look that finds nothing does not slow the next, and with nothing new about the work there is no look at all', ON, async ($, on) => {
  const world = { herdr: false, repo: true, replies: ['dream: none', 'dream: none'] } as World
  const seen = host(on, world)
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  await settle(() => false)
  expect(seen.forks.length).toBe(1)
  expect(seen.store.get('pace')).toEqual(expect.objectContaining({ looks: 1, dreams: 0 }))
  expect(await band.find({ text: /Dream · / })).toBeUndefined()
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([ // step 6 waits for its share; step 12 finds nothing
    expect.objectContaining({ outcome: 'budget', every: 6, tokens: 0 }),
    expect.objectContaining({ outcome: 'none', every: 22, tokens: LOOK, left: 5 }), // (120 - 12) / 5, not doubled
  ])

  const quiet = () => seen.kept.filter(record => record.outcome === 'quiet').length
  await steps($, 30) // step 42, past the next look (34): affordable, but CSR's brief holds nothing new
  await settle(() => quiet() >= 2) // 34 and 40
  expect(seen.forks.length).toBe(1)
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'look', outcome: 'quiet', tokens: 0 }))

  world.brief = `${BRIEF}\n- 10-08 (csr-engine): “the import should retry” [session 3f2a91c0]` // a past line: not news
  await steps($, 12)
  await settle(() => quiet() >= 4) // 46 and 52
  expect(seen.forks.length).toBe(1)
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'look', outcome: 'quiet' }))

  world.brief = `${world.brief}\n* plugin retry-queue: built over 1 session, latest 10-08 [session 3f2a91c0]`
  await steps($, 12)
  await settle(() => seen.store.get('pace') !== undefined && (seen.store.get('pace') as { looks: number }).looks >= 2)
  expect(seen.forks.length).toBe(2)
  expect(seen.store.get('pace')).toEqual(expect.objectContaining({ looks: 2 }))
})

test('only the brief\'s "* " lines, about the work in hand, count as news', async () => {
  const brief = [
    'Things the person built in other sessions that came up in this work:',
    '* skill chartkit: built over 2 sessions, latest 10-09 [session 5c1d0e77]',
    '  “Chart styles for reports”',
    'Lines the person wrote in past sessions that sit close to this work:',
    '- 06-10 (csr-engine): “the long-term plan” [session 9b2e4a10]',
    'Dreams already offered here, and what the person answered:',
    '- 10-09: “What if…?” [ask_claude]',
  ].join('\n')
  expect(facts(brief)).toEqual(['* skill chartkit: built over 2 sessions, latest 10-09 [session 5c1d0e77]'])
})

test('the dreams left are spread over the rest of a usual chapter', async () => {
  const chapter = { ...freshChapter('s', 0), steps: 12, shown: 1 }
  expect(spacing(chapter, 120)).toBe(27)
  expect(spacing({ ...chapter, steps: 200 }, 120)).toBe(6) // a long chapter: no less than 6
  expect(spacing({ ...chapter, shown: 5 }, 120)).toBe(0) // none left
  expect(spacing({ ...chapter, shown: 5, adopted: 1 }, 120)).toBe(108) // one adopted: its place back
  expect(median([90, 150, 120])).toBe(120)
  expect(median([])).toBe(120)
})

test('a chapter holds five dreams: with none left there is no look until a compaction starts the next', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  await $.ui.mount(BAND)
  seen.store.set('chapter', { session: 'sess-1', started: NOW, steps: 0, due: 6, looks: 5, shown: 5, adopted: 0 })
  await steps($, 60)
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(seen.kept.filter(record => record.kind === 'look').map(record => record.outcome)).toEqual(['spent'])

  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never)
  await settle(() => seen.kept.some(record => record.kind === 'chapter'))
  expect(seen.kept.find(record => record.kind === 'chapter')).toEqual(expect.objectContaining({ why: 'compact', left: 5, of: 5 }))
  expect(seen.store.get('chapterSteps')).toEqual([60]) // how long your chapters run, learned
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks.length).toBe(1)
})

test("each chapter's first look is taken even when CSR's notes hold nothing new; later ones wait for news", ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: ['dream: none', 'dream: none', 'dream: none'] })
  await $.ui.mount(BAND)
  seen.store.set('briefFacts', facts(BRIEF)) // the dreamer has seen every note already
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks.length).toBe(1) // the chapter's fresh look

  const quiet = () => seen.kept.filter(record => record.outcome === 'quiet').length
  await steps($, 30) // the next look is due at 34: nothing new, so no fork
  await settle(() => quiet() >= 1)
  expect(seen.forks.length).toBe(1)

  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never)
  await steps($, 12)
  await settle(() => seen.forks.length >= 2)
  expect(seen.forks.length).toBe(2) // a new chapter: a fresh look again
})

test('a look still running when a compaction starts the next chapter leaves that chapter alone', ON, async ($, on) => {
  let open = () => undefined as void
  const world = { herdr: false, repo: true, replies: [DREAM_REPLY], forkGate: new Promise<void>(done => (open = done)) }
  const seen = host(on, world)
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never)
  await settle(() => seen.kept.some(record => record.kind === 'chapter' && record.why === 'compact'))
  const fresh = seen.store.get('chapter')
  open()
  await settle(() => seen.kept.some(record => record.outcome === 'stale'))
  expect(seen.store.get('chapter')).toEqual(fresh) // due, looks and shown as the compaction set them
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(seen.kept.find(record => record.outcome === 'stale')).toEqual(expect.objectContaining({ tokens: expect.any(Number) }))
})

test('a compaction folds the dream that is up: its context is gone', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  await $.session.compact({ trigger: 'manual', messages: MESSAGES } as never)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) === undefined)
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(seen.kept.filter(record => record.kind === 'answer')).toEqual([expect.objectContaining({ answer: 'expired' })])
  expect(seen.store.get('chapter')).toEqual(expect.objectContaining({ shown: 0, steps: 0 }))
})

test('a dream typed past is never asked again, and the dreamer backs off', ON, async ($, on) => {
  const world = { herdr: false, repo: true, replies: [DREAM_REPLY, DREAM_REPLY, DREAM_REPLY] } as World
  const seen = host(on, world)
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)

  await $.prompt.submit({ text: 'one', origin: { kind: 'human' } } as never)
  await $.prompt.submit({ text: 'two', origin: { kind: 'human' } } as never)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) === undefined)
  expect(seen.store.get('looksToSkip')).toBe(1)
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'answer', answer: 'ignored' }))

  world.brief = `${BRIEF}\n* plugin retry-queue: built over 1 session, latest 10-08 [session 3f2a91c0]`
  await steps($, 27) // step 39, when the next look is due: skipped, backing off
  await settle(() => false)
  expect(seen.forks.length).toBe(1)
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'look', outcome: 'skip' }))

  await steps($, 20) // step 59: looks again, but the same dream is not shown twice
  await settle(() => seen.forks.length >= 2)
  await settle(() => false)
  expect(seen.forks.length).toBe(2)
  expect(seen.forks[1]).toContain(DREAM_LINE) // the seen list rides in the prompt
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
})

test('a dream about the pane that cites no other session still reaches you: you judge it, not a gate', ON, async ($, on) => {
  // The one dream acted on (10-09, asked Claude): about the pane, no receipt. A gate once kept it out.
  const pigment = [
    "dream: What if the pane took its colours from pigment's nyt inks, the palette your 10.x deck already uses?",
    'explain:',
    "**The pane could share the deck's palette**",
    "* We're tuning the pane's colours by hand.",
  ].join('\n')
  const seen = host(on, { herdr: false, repo: true, replies: [pigment] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /pigment's nyt inks/ })) !== undefined)
  expect(await band.find({ text: /pigment's nyt inks/ })).toBeDefined()
  expect(seen.kept.filter(record => record.kind === 'dream').length).toBe(1)
  expect(seen.kept.some(record => record.outcome === 'ungrounded')).toBe(false)
})

test('a dream answered in the pane leaves the band, and is never counted as typed past', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  const id = seen.kept.find(record => record.kind === 'dream')?.id
  seen.files.set(`${HOME}/.claude-self-reflect/csr-companion/dreams.jsonl`,
    `${JSON.stringify({ kind: 'dream', id, line: DREAM_LINE })}\n${JSON.stringify({ kind: 'answer', id, answer: 'helpful', at: 1, by: 'pane' })}\n`)

  await $.prompt.submit({ text: 'one', origin: { kind: 'human' } } as never)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) === undefined)
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(seen.kept.filter(record => record.kind === 'answer')).toEqual([]) // the pane logged it, once
  expect(seen.store.get('looksToSkip') ?? 0).toBe(0)
  expect(seen.store.get('answers')).toEqual(expect.arrayContaining([expect.objectContaining({ id, answer: 'helpful' })]))
})

test("a dream about CSR's own dream cycle still reaches you: only the pane and the dreamer are off limits", ON, async ($, on) => {
  const work = DREAM_REPLY.replace('What if the notes also read', 'What if the dream cycle also read')
  const seen = host(on, { herdr: false, repo: true, replies: [work] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the dream cycle also read/ })) !== undefined)
  expect(await band.find({ text: /What if the dream cycle also read/ })).toBeDefined()
  expect(seen.kept.filter(record => record.kind === 'dream').length).toBe(1)
})

test('the dim 4 on a dream turns dreams off, after a second press', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)

  await band.press({ key: 'Disable' })
  expect(seen.configured).toEqual([])
  expect(await band.find({ type: 'Button', text: 'Press again to turn dreams off' })).toBeDefined()

  await band.press({ key: 'Press again to turn dreams off' })
  expect(seen.configured).toEqual([{ key: 'csr-companion.dreams', value: false }])
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'answer', answer: 'turn_off' }))
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
})

test('a dream waits while You should know asks, so its 4 can never turn the other band off', ON, async ($, on) => {
  const world = { herdr: false, repo: true, replies: [DREAM_REPLY], youShouldKnow: true } as World
  const seen = host(on, world)
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  await settle(() => false)
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(await band.find({ key: 'ysk-disable' })).toBeDefined()

  // Prompts typed while the dream was hidden are not "typed past".
  await $.prompt.submit({ text: 'one', origin: { kind: 'human' } } as never)
  await $.prompt.submit({ text: 'two', origin: { kind: 'human' } } as never)
  expect(seen.kept.some(entry => entry.answer === 'ignored')).toBe(false)
  expect(seen.store.get('looksToSkip')).toBeUndefined()

  world.youShouldKnow = false
  await band.redraw() // as You should know's band redraws when its note clears
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  expect(await band.find({ key: 'Disable' })).toBeDefined()
  expect(await band.find({ key: 'ysk-disable' })).toBeUndefined()
})

// ---------------------------------------------------------------- /csr statusline

const USER = `${HOME}/.claude/settings.json`
const STABLE = `${HOME}/.claude-self-reflect/csr-companion/statusline.cjs`
const BACKUP = `${HOME}/.claude-self-reflect/csr-companion/settings.before-statusline.json`
const SHIPPED = '#!/usr/bin/env node\n// the wrapper, as this version ships it\n'
const ORIGINAL = 'node "/x/y.js"'
const WRAPPED = `node "${STABLE}" --sh 'node "/x/y.js"'`
// statusLine sits between other keys, 4-space as a hand-kept file may be: the
// rewrite keeps every key in its place and writes 2-space JSON.
const settingsOf = (statusLine?: Record<string, unknown>) => ({
  model: 'opus',
  ...(statusLine ? { statusLine } : {}),
  hooks: { Stop: [] },
  theme: 'dark',
})
const fileOf = (statusLine?: Record<string, unknown>) => `${JSON.stringify(settingsOf(statusLine), null, 4)}\n`
const lineOf = (command: string) => ({ type: 'command', command, padding: 0 })
const written = (statusLine?: Record<string, unknown>) => `${JSON.stringify(settingsOf(statusLine), null, 2)}\n`

async function csr($: Engine, args: string): Promise<string> {
  const ran = await $.command.run({ command: 'csr', args, origin: { kind: 'human' } } as never)
  return ran.text ?? ''
}

test('/csr statusline previews the change and writes nothing', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf(lineOf(ORIGINAL)) } })
  const said = await csr($, 'statusline')

  expect(said).toContain(`  ${ORIGINAL}\n`)
  expect(said).toContain(`  ${WRAPPED}\n`)
  expect(said).toContain('No project, local, --settings or policy statusLine wins over yours.')
  expect(said).toContain('Next: /csr statusline on')
  expect(seen.writes).toEqual([])
  expect(seen.files.get(USER)).toBe(fileOf(lineOf(ORIGINAL)))
  expect(seen.store.has('statusline')).toBe(false)
})

test('/csr statusline on copies the wrapper out of the plugin, backs up the file, wraps only the command', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf(lineOf(ORIGINAL)) } })
  const said = await csr($, 'statusline on')

  expect(said).toContain(`now runs:\n  ${WRAPPED}`)
  expect(seen.files.get(STABLE)).toBe(SHIPPED)
  expect(seen.files.get(BACKUP)).toBe(fileOf(lineOf(ORIGINAL))) // the raw text, as it was
  expect(seen.files.get(USER)).toBe(written(lineOf(WRAPPED)))
  expect(Object.keys(JSON.parse(seen.files.get(USER) ?? '{}'))).toEqual(['model', 'statusLine', 'hooks', 'theme'])
  expect(seen.writes).toEqual([STABLE, BACKUP, USER])
  expect(seen.writes.some(path => path.includes('/csr-companion/bin/'))).toBe(false) // never into the plugin's folder
  expect(seen.store.get('statusline')).toEqual({ original: ORIGINAL, wrapped: WRAPPED })
})

test('/csr statusline on without node changes nothing: the line would go blank', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, noNode: true, files: { [USER]: fileOf(lineOf(ORIGINAL)) } })
  const said = await csr($, 'statusline on')

  expect(said).toContain("node didn't run here. Nothing changed.")
  expect(seen.writes).toEqual([])
  expect(seen.files.get(USER)).toBe(fileOf(lineOf(ORIGINAL)))
})

test('/csr statusline on twice changes nothing the second time', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf(lineOf(ORIGINAL)) } })
  await csr($, 'statusline on')
  const after = new Map(seen.files)
  const writes = seen.writes.length

  const said = await csr($, 'statusline on')
  expect(said).toContain('already wrapped')
  expect(seen.writes.length).toBe(writes)
  expect(seen.files).toEqual(after)
  expect(await csr($, 'statusline')).toContain('already wrapped') // the preview says so too
})

test('/csr statusline off puts the exact original back', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf(lineOf(ORIGINAL)) } })
  await csr($, 'statusline on')
  const said = await csr($, 'statusline off')

  expect(said).toContain(`runs your command again:\n  ${ORIGINAL}`)
  expect(seen.files.get(USER)).toBe(written(lineOf(ORIGINAL)))
  expect(seen.store.has('statusline')).toBe(false)

  const writes = seen.writes.length
  expect(await csr($, 'statusline off')).toContain("isn't wrapped") // not wrapped now: nothing to undo
  expect(seen.writes.length).toBe(writes)
})

test('no statusLine, or no settings file: nothing to move, nothing written', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf() } })
  expect(await csr($, 'statusline')).toContain('nothing to move')
  expect(await csr($, 'statusline on')).toContain('nothing to move')
  expect(await csr($, 'statusline off')).toContain('nothing to put back')
  seen.files.set(USER, fileOf({ type: 'command', padding: 0 }))
  expect(await csr($, 'statusline on')).toContain('nothing to move')
  seen.files.delete(USER)
  expect(await csr($, 'statusline on')).toContain('nothing to move')
  expect(seen.writes).toEqual([])
  expect(seen.store.has('statusline')).toBe(false)
})

test('a statusLine in project settings is reported: it wins over the user one', async ($, on) => {
  const project = { statusLine: { type: 'command', command: 'bash .claude/statusline.sh' } }
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf(lineOf(ORIGINAL)) }, sources: { project } })
  const said = await csr($, 'statusline')

  expect(said).toContain('wins over the user settings')
  expect(said).toContain('project settings (.claude/settings.json): bash .claude/statusline.sh')
  expect(said).not.toContain('No project')
  expect(seen.writes).toEqual([])
  expect(await csr($, 'statusline on')).toContain('project settings (.claude/settings.json): bash .claude/statusline.sh')
})

test('a command with single quotes survives on and off, from the store or read back off the command', async ($, on) => {
  const quoted = `printf '%s|%s' "it's" b`
  const wrapped = String.raw`node "/home/u/.claude-self-reflect/csr-companion/statusline.cjs" --sh 'printf '\''%s|%s'\'' "it'\''s" b'`
  const seen = host(on, { herdr: false, repo: true, files: { [USER]: fileOf(lineOf(quoted)) } })

  await csr($, 'statusline on')
  expect(JSON.parse(seen.files.get(USER) ?? '{}').statusLine.command).toBe(wrapped)
  await csr($, 'statusline off')
  expect(seen.files.get(USER)).toBe(written(lineOf(quoted)))

  await csr($, 'statusline on')
  seen.store.delete('statusline') // a store that lost it: the --sh word is unquoted instead
  await csr($, 'statusline off')
  expect(seen.files.get(USER)).toBe(written(lineOf(quoted)))
})

test('off also unwraps the argv form set up by hand, in a CLAUDE_CONFIG_DIR settings file', async ($, on) => {
  const config = `${HOME}/alt`
  const real = `node "${HOME}/.claude/hooks/statusline.js"`
  const byHand = `node "${HOME}/.claude/hooks/csr-pane-statusline.js" ${real}`
  const seen = host(on, { herdr: false, repo: true, configDir: config, files: { [`${config}/settings.json`]: fileOf(lineOf(byHand)) } })

  expect(await csr($, 'statusline on')).toContain('already wrapped')
  expect(await csr($, 'statusline off')).toContain(real)
  expect(seen.files.get(`${config}/settings.json`)).toBe(written(lineOf(real)))
  await csr($, 'statusline on')
  expect(JSON.parse(seen.files.get(`${config}/settings.json`) ?? '{}').statusLine.command).toBe(`node "${STABLE}" --sh '${real}'`)
  expect(seen.files.has(USER)).toBe(false)
})

test('session start brings a stale stable wrapper up to date, and never makes one', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await $.session.start(START as never)
  await settle(() => seen.opened.length > 0)
  await settle(() => false)
  expect(seen.writes).toEqual([])

  seen.files.set(STABLE, '// an older wrapper\n')
  await $.session.start(START as never)
  await settle(() => seen.files.get(STABLE) === SHIPPED)
  expect(seen.files.get(STABLE)).toBe(SHIPPED)
  expect(seen.writes).toEqual([STABLE])
})

test('a compaction leaves a marker while it runs and removes it when it ends', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await $.session.compact({ trigger: 'manual', messages: MESSAGES } as never)

  expect(JSON.parse(seen.compacting[0] ?? '{}')).toEqual({ at: NOW / 1000, trigger: 'manual' })
  expect(seen.files.has(MARKER)).toBe(false)
})

test('the marker goes also when the compaction fails', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, compactFails: true })
  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never).catch(() => undefined)

  expect(seen.compacting.length).toBeGreaterThan(0)
  expect(seen.compacting.every(marker => JSON.parse(marker ?? '{}').trigger === 'auto')).toBe(true)
  expect(seen.files.has(MARKER)).toBe(false)
})

test('a precompute or a subagent leaves no marker', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await $.session.compact({ trigger: 'precompute', messages: MESSAGES } as never)
  await $.session.compact({ trigger: 'manual', agentId: 'sub-1', messages: MESSAGES } as never)

  expect(seen.compacting).toEqual([undefined, undefined])
  expect(seen.writes.some(path => path.includes('/compacting/'))).toBe(false)
})

// ---------------------------------------------------------------- coaching and the push guard

const TERM_NOTE = {
  id: 'term:quokka',
  kind: 'term',
  file: '',
  line: 'You said “quokka” in 2 sessions and Claude looked it up each time. Define it once?',
  title: 'Define “quokka” once',
  text: '- quokka: said in 2 sessions, each time Claude looked it up (e.g. “call quokka now”). Edit this line to say what you mean.',
  sketch: [],
  symbol: null,
  oid: null,
  conversation: '',
  at: 1,
}
const CLAUDE_MD = `${HOME}/.claude/CLAUDE.md`
const HUMAN = { kind: 'composer' }

test('a coaching note names a word you said in 2 sessions, writes nothing, and adds the line only on 1', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, coach: [TERM_NOTE], files: { [CLAUDE_MD]: '# Prefs\n\n## Core\n- be brief\n' } })
  const band = await $.ui.mount(BAND)
  await $.session.start(START as never)
  await settle(async () => (await band.find({ text: /You said “quokka”/ })) !== undefined)

  expect(await band.find({ text: /Coaching/ })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Add to CLAUDE.md' })).toBeDefined()
  expect(seen.runs.find(argv => argv.includes('--coach'))).toEqual(expect.arrayContaining(['--coach']))
  expect(seen.writes.includes(CLAUDE_MD)).toBe(false) // shown is not written

  await band.press({ key: 'Add to CLAUDE.md' })
  expect(seen.files.get(CLAUDE_MD)).toBe(`# Prefs\n\n## Core\n- be brief\n\n## Terms\n${TERM_NOTE.text}\n`)
  expect(seen.toasts.some(text => text.includes('CLAUDE.md'))).toBe(true)
  expect(await band.find({ text: /You said “quokka”/ })).toBeUndefined()
  expect(seen.store.get('coached')).toEqual(['term:quokka'])

  await $.session.start(START as never) // once: the same word is not offered again
  await settle(() => false)
  expect(await band.find({ text: /You said “quokka”/ })).toBeUndefined()
})

test('dismissing a coaching note leaves CLAUDE.md alone', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, coach: [TERM_NOTE], files: { [CLAUDE_MD]: '# Prefs\n' } })
  const band = await $.ui.mount(BAND)
  await $.session.start(START as never)
  await settle(async () => (await band.find({ text: /You said “quokka”/ })) !== undefined)
  await band.press({ key: 'Dismiss' })
  expect(await band.find({ text: /You said “quokka”/ })).toBeUndefined()
  expect(seen.files.get(CLAUDE_MD)).toBe('# Prefs\n')
})

test('addTerm puts a line under an existing Terms heading, makes one if missing, and never repeats', () => {
  expect(addTerm('# A\n', '- x: y')).toBe('# A\n\n## Terms\n- x: y\n')
  expect(addTerm('', '- x: y')).toBe('## Terms\n- x: y\n')
  expect(addTerm('# A\n## Terms\n- a: b\n\n## Next\ntext\n', '- x: y')).toBe('# A\n## Terms\n- a: b\n- x: y\n\n## Next\ntext\n')
  expect(addTerm('# A\n## Terms\n- a: b\n', '- x: y')).toBe('# A\n## Terms\n- a: b\n- x: y\n')
  expect(addTerm('## Terms\n- x: y\n', '- x: y')).toBe('## Terms\n- x: y\n')
})

test('a bare digit just after a band note is held with a toast; the same digit again within 10 s goes through', async ($, on) => {
  const clock = { now: NOW }
  const seen = host(on, { herdr: false, repo: true, notes: [NOTE], clock })
  const band = await $.ui.mount(BAND)
  await $.tool.call({ tool: 'Read', file_path: '/repo/src/hooks/mod.rs' } as never)
  await settle(async () => (await band.find({ text: /changed in ca5655a/ })) !== undefined)
  expect(isBareDigit(' 2 ')).toBe(true)

  clock.now = NOW + 2_000
  const first = await $.prompt.submit({ text: '2', origin: HUMAN } as never)
  expect(first.drop).toContain('looks like a band hotkey')
  expect(seen.toasts).toEqual([first.drop])
  expect(first.drop).toContain('send it again within 10 s')

  clock.now = NOW + 8_000 // 6 s later: the same digit goes through
  const again = await $.prompt.submit({ text: '2', origin: HUMAN } as never)
  expect(again.drop).toBeUndefined()
  expect(again.text).toBe('2')

  clock.now = NOW + 9_000 // held is spent: a third one is held again
  expect((await $.prompt.submit({ text: '2', origin: HUMAN } as never)).drop).toBeDefined()
  expect(seen.store.get('answers')).toEqual([expect.objectContaining({ answer: 'shown' })]) // nothing counted as typed past
})

test('a digit sent again after 10 s is held again; more than a digit, or no band, or 30 s later, is never held', async ($, on) => {
  const clock = { now: NOW }
  host(on, { herdr: false, repo: true, notes: [NOTE], clock })
  const band = await $.ui.mount(BAND)
  const plain = await $.prompt.submit({ text: '2', origin: HUMAN } as never) // no band yet
  expect(plain.drop).toBeUndefined()

  await $.tool.call({ tool: 'Read', file_path: '/repo/src/hooks/mod.rs' } as never)
  await settle(async () => (await band.find({ text: /changed in ca5655a/ })) !== undefined)
  clock.now = NOW + 1_000
  expect((await $.prompt.submit({ text: '2', origin: HUMAN } as never)).drop).toBeDefined()
  clock.now = NOW + 12_000 // past the 10 s window
  expect((await $.prompt.submit({ text: '2', origin: HUMAN } as never)).drop).toBeDefined()
  for (const text of ['22', '2 please', 'yes', '']) expect((await $.prompt.submit({ text, origin: HUMAN } as never)).drop).toBeUndefined()
  clock.now = NOW + 31_000 // the band showed over 30 s ago
  expect((await $.prompt.submit({ text: '3', origin: HUMAN } as never)).drop).toBeUndefined()
})

test('the push guard: --no-verify and release/v* are denied; every other push goes through', async ($, on) => {
  const clock = { now: NOW }
  host(on, { herdr: false, repo: true, clock })
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string; result?: unknown }>

  expect((await bash('git commit --no-verify -m wip')).deny).toBe(NO_VERIFY)
  expect((await bash('git push origin HEAD:release/v9.5.9')).deny).toBe(RELEASE)
  expect((await bash('git status')).deny).toBeUndefined()
  expect((await bash('echo "git push origin main"')).deny).toBeUndefined()

  expect((await bash('git push origin main')).deny).toBeUndefined() // no word needed: Claude's work is never held
  expect((await bash('git push -u origin feat/x')).deny).toBeUndefined()
  expect((await bash('git push origin HEAD:release/v9')).deny).toBe(RELEASE) // release/v* is never let through
})

test('a push that names no branch is denied when git says it lands on release/v*', async ($, on) => {
  const branch = { 'symbolic-ref -q --short HEAD': 'release/v10.1.0\n' }
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  host(on, { herdr: false, repo: true, git: branch })
  for (const c of ['git push', 'git push -u origin HEAD', 'git push origin']) expect((await bash(c)).deny).toBe(RELEASE)
  expect((await bash('git push origin HEAD:feat/x')).deny).toBeUndefined() // the words name the branch
})

test('upstream and configured push refspecs count; a git that cannot answer lets the push go on', async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  const world = { herdr: false, repo: true, git: { 'symbolic-ref -q --short HEAD': 'feat/x\n' } as Record<string, string> }
  host(on, world)
  expect((await bash('git push')).deny).toBeUndefined()
  world.git['rev-parse --abbrev-ref --symbolic-full-name @{upstream}'] = 'origin/release/v9\n'
  expect((await bash('git push')).deny).toBe(RELEASE)
  delete world.git['rev-parse --abbrev-ref --symbolic-full-name @{upstream}']
  world.git['config --get-regexp ^remote\\..*\\.push$'] = 'remote.origin.push refs/heads/*:refs/heads/*\n'
  expect((await bash('git push')).deny).toBe(RELEASE)
  world.git = {}
  expect((await bash('git push')).deny).toBeUndefined()
})

test('gitVerdict reads git as a command word, not git in words', () => {
  const kind = (c: string) => gitVerdict(c)?.kind ?? null
  const reason = (c: string) => {
    const v = gitVerdict(c)
    return v?.kind === 'deny' ? v.reason : null
  }
  for (const c of ['git commit --no-verify -m x', 'git push --no-verify', 'git -C /r merge --no-verify topic', 'cd r && git commit -n -m x'])
    expect(reason(c)).toBe(NO_VERIFY)
  for (const c of [
    'git push origin release/v9.5.9', 'git push origin HEAD:release/v9', 'git push origin refs/heads/release/v9',
    'git push origin +HEAD:refs/heads/release/v9', 'git push --all', 'git push --mirror origin', 'git push -u origin release/v10',
    'git -C /repo push origin HEAD:release/v1', 'FOO=1 git push origin release/v2', 'bash -c "git push origin release/v3"',
    "git push origin 'refs/heads/*:refs/heads/*'", 'git push --dry-run --no-dry-run origin HEAD:release/v1',
    "sh -ce 'git push origin release/v1'", 'env -i git push origin release/v1', 'sudo -u me git push origin release/v1',
    'echo "$(git push origin release/v1)"', "bash <<'EOF'\ngit push origin release/v1\nEOF",
    "bash -o pipefail -c 'git push origin release/v1'", 'x=`git push origin release/v1`',
  ])
    expect(reason(c)).toBe(RELEASE)
  for (const c of ['git commit --verify --no-verify -m x', 'git push -o ci.skip --no-verify origin main']) expect(reason(c)).toBe(NO_VERIFY)
  // where it lands is the repository's to say: the hook asks git (see the next test)
  for (const c of ['git push', 'git push origin', 'git push origin HEAD', 'git push -u origin HEAD', 'cd /r && git push'])
    expect(kind(c)).toBe('check')
  expect(gitVerdict('cd /r && git push', 0, '/elsewhere')).toEqual({ kind: 'check', dir: '/r', remote: null })
  expect(gitVerdict('git -C sub push origin HEAD', 0, '/r')).toEqual({ kind: 'check', dir: '/r/sub', remote: 'origin' })
  for (const c of ['git push origin main', 'git push -u origin feature/x', 'git push -o --dry-run origin main', 'echo hi && git push origin backport/x', 'sudo git push origin main', '(git push origin main)', 'sh -c \'git push origin main\'', 'git push origin main 2>&1 | tail -3'])
    expect(kind(c)).toBe('ask')
  for (const c of [
    'ls', 'git status', 'git log --oneline', 'git commit -m "git push --no-verify"', 'git commit -m "mention release/v9 push"',
    'echo git push origin main', 'echo "git push origin main"', 'grep "git push" notes.md', 'git push --dry-run origin main',
    "cat <<'EOF'\ngit push origin release/v9\ngit commit --no-verify\nEOF", 'echo hi # git push origin main',
    'git pull origin release/v9', 'git checkout release/v9', 'git commit -m "x" --amend', 'git stash push',
    "echo sh -c 'git push origin release/v1'", 'git commit -m --no-verify', 'git commit --no-verify --verify -m x',
    "cat <<'EOF'\nbash -c 'git push origin release/v1'\nEOF",
  ])
    expect(kind(c)).toBe(null)
  expect(commands('a "b c" && d; e | f')).toEqual([['a', 'b c'], ['d'], ['e'], ['f']])
})

test('commit -n is read as git reads it: in a cluster, never inside an option value or after --', () => {
  const kind = (c: string) => gitVerdict(c)?.kind ?? null
  for (const c of ['git commit -an -m x', 'git commit -nm x', 'git commit -m x -n']) expect(kind(c)).toBe('deny')
  for (const c of ['git commit -uno -m x', 'git commit -m n', 'git commit -mn', 'git commit -m x -- -n', 'git commit -F notes -S'])
    expect(kind(c)).toBe(null)
})

test('a CLAUDE.md that is there but cannot be read is never written over', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, coach: [TERM_NOTE], files: { [CLAUDE_MD]: '# Prefs\n' }, unreadable: [CLAUDE_MD] })
  const band = await $.ui.mount(BAND)
  await $.session.start(START as never)
  await settle(async () => (await band.find({ text: /You said “quokka”/ })) !== undefined)
  await band.press({ key: 'Add to CLAUDE.md' })
  expect(seen.files.get(CLAUDE_MD)).toBe('# Prefs\n')
  expect(seen.writes.includes(CLAUDE_MD)).toBe(false)
  expect(await band.find({ text: /Couldn't write your CLAUDE.md/ })).toBeDefined()
})

const THREADS = [
  { text: 'Awaiting the user’s go on the push guard.', state: 'lost', waiting: true },
  { text: 'Fix the importer: remove the old cache.', state: 'open', waiting: false },
  { text: 'Try the cache countdown in the masthead.', state: 'lost', waiting: false, later: true },
]
const REMINDERS = "After the summary, add a section headed 'Reminders from CSR (not tasks)'"

test('prep-compact hands the summary only what you set aside for later, as reminders, never as tasks', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, lost: THREADS })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'manual', instructions: 'keep the plan', messages: MESSAGES } as never)
  const told = seen.compacted.at(-1) ?? ''
  expect(told.startsWith(`keep the plan\n\n${REMINDERS.slice(0, -1)}`)).toBe(true)
  expect(told).toContain('- Try the cache countdown in the masthead.')
  expect(told).not.toContain('push guard') // open and lost threads stay out: Lost and found must see them drop
  expect(told).not.toContain('Fix the importer')
  expect(told).not.toContain('Pending Tasks')
  const asked = seen.runs.find(run => run.includes('--lost'))
  expect(asked).toEqual(expect.arrayContaining(['python3', '-I', '--repo', '/repo', '--lost', '--sid', 'sess-1']))
})

test('prep-compact holds the block to eight threads and 1200 characters', async ($, on) => {
  const many = Array.from({ length: 20 }, (_, k) => ({ text: `Thread ${k} ${'x'.repeat(200)}`, state: 'lost', waiting: false, later: true }))
  const seen = host(on, { herdr: false, repo: true, lost: many })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never)
  const told = seen.compacted.at(-1) ?? ''
  expect(told.length).toBeLessThanOrEqual(1200)
  expect(told.split('\n').length - 1).toBeLessThanOrEqual(9) // the heading takes two lines, then eight threads at most
  expect(told).toContain('Thread 0 ')
})

test('prep-compact fails open: a pane.py that fails, or has no threads, leaves the compaction as it was', async ($, on) => {
  const failing = host(on, { herdr: false, repo: true })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'manual', instructions: 'keep the plan', messages: MESSAGES } as never)
  expect(failing.compacted.at(-1)).toBe('keep the plan')
  await $.session.compact({ trigger: 'manual', messages: MESSAGES } as never)
  expect(failing.compacted.at(-1)).toBeUndefined()
})

test('prep-compact leaves a compaction with no threads to carry unchanged', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, lost: [] })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'manual', instructions: 'keep the plan', messages: MESSAGES } as never)
  expect(seen.compacted.at(-1)).toBe('keep the plan')
})

test('prep-compact fails open on a pane.py that times out or prints no JSON, and passes its two-second limit', async ($, on) => {
  const hung = host(on, { herdr: false, repo: true, lost: 'hangs' })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'manual', instructions: 'keep the plan', messages: MESSAGES } as never)
  expect(hung.compacted.at(-1)).toBe('keep the plan')
})

test('prep-compact leaves a compaction unchanged when pane.py prints something that is not JSON', async ($, on) => {
  const garbled = host(on, { herdr: false, repo: true, lost: 'Traceback (most recent call last)' })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never)
  expect(garbled.compacted.at(-1)).toBeUndefined()
  expect(garbled.compacted.length).toBe(1)
})

test('prep-compact also carries the threads into a compaction made ahead of time', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, lost: THREADS })
  await $.session.start(START as never)
  await $.session.compact({ trigger: 'precompute', messages: MESSAGES } as never)
  expect(seen.compacted.at(-1) ?? '').toContain('Reminders from CSR (not tasks)')
})
