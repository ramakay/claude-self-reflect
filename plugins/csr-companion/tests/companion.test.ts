import { expect, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On, SettingsSource } from 'claude-code'

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
const BRIEF = 'Lines the person wrote in past sessions that sit close to this work:\n- 08-26 (csr-engine): “csr should run on the cross project corpus” [session dc24d328]'
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
  box?: string
  replies?: string[]
  brief?: string
  youShouldKnow?: boolean
  // A file system and settings of the test's own: nothing here reaches the real ones.
  files?: Record<string, string>
  sources?: Partial<Record<SettingsSource, Record<string, unknown>>>
  configDir?: string
  noNode?: boolean
}
const HOME = '/home/u'

function host(on: On, world: World) {
  const runs: string[][] = []
  const statuses: Array<string | undefined> = []
  const opened: string[] = []
  const filled: string[] = []
  const forks: string[] = []
  const kept: Array<Record<string, unknown>> = []
  const configured: Array<{ key: string; value: unknown }> = []
  const store = new Map<string, unknown>()
  const files = new Map<string, string>(Object.entries(world.files ?? {}))
  const writes: string[] = []
  const ok = (stdout: string) => ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.register', async (_$, e) => ({ value: { command: e.name } }))
  on('clock.every', async () => ({ value: undefined }))
  on('clock.now', async () => ({ value: Date.UTC(2026, 9, 9, 12) }))
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
    if (e.argv[0] === 'node' && world.noNode) return { value: { exitCode: 127, stdout: '', stderr: 'node: not found', isStdoutTruncated: false, isStderrTruncated: false } }
    if (e.argv[0] === 'python3' && e.argv.includes('--note')) return ok(JSON.stringify(world.notes ?? []))
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
  return { runs, statuses, opened, filled, forks, kept, store, configured, files, writes }
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

  await steps($, 6) // 12 steps: 1 in 12, within it
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  expect(seen.forks.length).toBe(1)
  expect(seen.forks[0]).toContain(BRIEF)
  expect(seen.forks[0]).toContain('dream: none')
  expect(seen.forks[0]).toContain('You should know')
  expect(await band.find({ text: /Dream · / })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Already thought of it' })).toBeDefined()
  expect(seen.store.get('pace')).toEqual(expect.objectContaining({ looks: 1, dreams: 1, dreamer: LOOK, claude: 12 * LOOK, every: 6 }))
  expect(seen.kept[0]).toEqual(expect.objectContaining({ kind: 'dream', line: DREAM_LINE, tokens: LOOK, session: 'sess-1' }))

  await band.press({ key: 'Learn more' })
  expect(await band.find({ text: 'Notes could follow side branches' })).toBeDefined()
  expect(await band.find({ text: /this look 41,000 tokens/ })).toBeDefined()

  await band.press({ key: 'Ask Claude' })
  expect(seen.filled.length).toBe(1)
  expect(seen.filled[0]?.startsWith(DREAM_LINE)).toBe(true)
  expect(seen.filled[0]).toContain('> **Notes could follow side branches**')
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'answer', answer: 'ask_claude' }))
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(JSON.stringify(await $.command.run(DREAMS))).toContain('1 look, 1 dream')
})

test('a look that finds nothing doubles the wait, and with nothing new near the work there is no look at all', ON, async ($, on) => {
  const world = { herdr: false, repo: true, replies: ['dream: none', 'dream: none'] } as World
  const seen = host(on, world)
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  await settle(() => false)
  expect(seen.forks.length).toBe(1)
  expect(seen.store.get('pace')).toEqual(expect.objectContaining({ looks: 1, dreams: 0, every: 12 }))
  expect(await band.find({ text: /Dream · / })).toBeUndefined()
  expect(seen.kept).toEqual([])

  await steps($, 48) // affordable, but CSR's brief holds nothing the last look lacked
  await settle(() => false)
  expect(seen.forks.length).toBe(1)

  world.brief = `${BRIEF}\n- 10-08 (csr-engine): “the import should retry” [session 3f2a91c0]`
  await steps($, 12)
  await settle(() => seen.forks.length >= 2)
  await settle(() => false)
  expect(seen.forks.length).toBe(2)
  expect(seen.store.get('pace')).toEqual(expect.objectContaining({ looks: 2, every: 24 }))
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

  world.brief = `${BRIEF}\n- 10-08 (csr-engine): “the import should retry” [session 3f2a91c0]`
  await steps($, 6) // skipped: backing off
  await settle(() => false)
  expect(seen.forks.length).toBe(1)

  await steps($, 6) // looks again, but the same dream is not shown twice
  await settle(() => seen.forks.length >= 2)
  await settle(() => false)
  expect(seen.forks.length).toBe(2)
  expect(seen.forks[1]).toContain(DREAM_LINE) // the seen list rides in the prompt
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
})

test('a dream that only echoes this session, or asks about the pane, never reaches you', ON, async ($, on) => {
  const echo = DREAM_REPLY.replace('[session dc24d328]', '[session sess-1]').replace('On 08-26 you asked', 'Today you asked')
  const tool = DREAM_REPLY.replace('What if the notes also read', 'What if the pane also read')
  const seen = host(on, { herdr: false, repo: true, replies: [echo, tool] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  await settle(() => false)
  expect(await band.find({ text: /What if the (notes|pane) also read/ })).toBeUndefined()
  expect(seen.kept.filter(record => record.kind === 'dream')).toEqual([])
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
