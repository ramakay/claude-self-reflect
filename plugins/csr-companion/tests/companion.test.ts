import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On, SettingsSource } from 'claude-code'
import { canAfford, facts, freshChapter, freshPace, median, owed, paceText, spacing } from '../hooks/pace'
const rankOfRoot = (root: string) => (/\/\.claude\/plugins\//.test(root) ? 0 : 1) // as register.tsx ranks a copy
import { addTerm, AS_TEXT, commands, gitParse, gitVerdict, HOOKS_OFF, isBareDigit, mergeGuards, parseGuard, NO_VERIFY, PROTECTED, RELEASE, tripwire, UNRESOLVED } from '../hooks/coach'
import type { Guard } from '../hooks/coach'
import { askClaude, citedFrom, cleanForDraft, dreamPrompt, dreamSent, isYskOrigin, isYskPropose, jaccard, leansOnOld, LEASE_MS, leaseHeld, markReceipts, NEAR, nearDream, offersAnotherWay, ownPrompt, parseDream, rebuts, sameDream, seenEntry, withCsrFacts, wordsBeside, yskFacts, yskLine } from '../hooks/dreamer'

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
  gitIn?: Record<string, Record<string, string>> // the same, for each -C directory on its own
  hostEnv?: Record<string, string> // what `printenv NAME` finds in the hook's own environment
  guard?: Record<string, string | null> // guard files: path -> words, or null for none
  noRepo?: boolean // `git rev-parse --git-dir` fails: git refuses the settings, or there is no repository
  gitExit?: Record<string, number> // a git question that fails with this exit, keyed as `git` is
  compactFails?: boolean
  lost?: unknown[] | string // what pane.py --lost prints (a string as is, 'hangs' a timeout); undefined makes it fail
  json?: Record<string, unknown> // what pane.py --json prints, where SHIFTS is not the case
  jsonHangs?: boolean // pane.py --json runs past its time limit: process.run rejects
  configRefused?: boolean // a /config row the plugin may not change
  sid?: string // the session this process runs; 'sess-1' where left out
  headless?: boolean // a `claude -p` child: draws on no surface
  firstRun?: boolean // the plugin just added: no setup answered yet, so the walkthrough shows
  forkReason?: string // a fork that comes back without text, for this reason
  forkUsage?: Array<typeof USAGE> // what each fork cost, in order; USAGE where left out
  forkFails?: boolean // a fork that rejects (a timeout), what it cost unknown
  storeGate?: (key: string, value: unknown) => Promise<void> | undefined // a write that lands only once this settles
  mockClock?: boolean // time that moves only when the test moves it (a wait fires when its time comes), not NOW and no waits
}
const HOME = '/home/u'
// The guard this repository ships (.claude/csr-guard.json): every earlier denial is under it.
const GUARD_JSON = { denyNoVerify: true, protectedPush: ['release/v*'], reason: { 'release/v*': RELEASE } }
const GUARD: Guard = GUARD_JSON
const REPO_GUARD = '/repo/.claude/csr-guard.json'
const USER_GUARD = `${HOME}/.claude-self-reflect/csr-companion/guard.json`
const verdictOf = (command: string, _depth?: number, cwd: string | null = null) => gitVerdict(command, GUARD, cwd)
const parseOf = (command: string) => gitParse(command, GUARD)
const MARKER = `${HOME}/.claude-self-reflect/csr-companion/compacting/sess-1.json`

function host(on: On, world: World) {
  mainNext = 0
  const runs: string[][] = []
  const statuses: Array<string | undefined> = []
  const opened: string[] = []
  const filled: string[] = []
  const forks: string[] = []
  const kept: Array<Record<string, unknown>> = []
  const compacted: Array<string | undefined> = [] // the instructions each compaction reached core with
  const configured: Array<{ key: string; value: unknown }> = []
  const store = new Map<string, unknown>()
  if (!world.firstRun) { // every other test runs past the first run: both settings already chosen
    store.set('setup:answer:dreams', { value: false, at: 0 })
    store.set('setup:answer:autoCompact', { value: 'suggest', at: 0 })
  }
  // a repository at /repo with the guard this repository ships, unless the test says otherwise (guard: path -> words, null: none)
  const files = new Map<string, string>(Object.entries({ '/repo/.git': 'gitdir: x', [REPO_GUARD]: JSON.stringify(GUARD_JSON), ...(world.files ?? {}) }))
  for (const [path, words] of Object.entries(world.guard ?? {})) words === null ? files.delete(path) : files.set(path, words)
  const dirs = new Set<string>()
  const writes: string[] = []
  const ok = (stdout: string) => ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.register', async (_$, e) => ({ value: { command: e.name } }))
  const everies: number[] = [] // the recurring jobs set, by their period
  const clock = world.mockClock ? mock.clock(on, { now: NOW }) : undefined
  if (!clock) {
    on('clock.every', async (_$, e) => {
      everies.push((e as { ms?: number }).ms ?? 0)
      return { value: undefined }
    })
    on('clock.after', async () => ({ value: undefined }) as never) // a wait the plugin sets (the agents file's trailing write, a completion's 20 s): never fires here
    on('clock.now', async () => ({ value: world.clock?.now ?? NOW }))
  }
  on('agent.list', async () => ({ value: [] }) as never)
  on('turn.start', async (_$, e) => ({ turnId: e.turnId })) // the main loop's turn and an agent's end: nothing beneath the plugin
  on('turn.complete', async (_$, e) => ({ text: e.answer }))
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
  on('session.id', async () => ({ value: world.sid ?? 'sess-1' }))
  on('session.surfaces', async () => ({ value: world.headless ? [] : ['terminal'] }) as never)
  on('session.cwd', async () => ({ value: '/repo' }))
  on('store.get', async (_$, e) => ({ value: store.get(e.key) }))
  on('store.set', async (_$, e) => {
    await world.storeGate?.(e.key, e.value)
    store.set(e.key, e.value)
    return { value: undefined }
  })
  on('store.keys', async () => ({ value: [...store.keys()] }) as never)
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
  on('fs.exists', async (_$, e) => ({ value: files.has(e.path) || dirs.has(e.path) }))
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
    if (world.configRefused) return { deny: 'set by policy' } as never
    configured.push({ key: e.key, value: e.value })
    return { value: e.value }
  })
  on('model.fork', async (_$, e) => {
    forks.push(e.prompt)
    if (world.forkGate) await world.forkGate
    if (world.forkFails) return { deny: 'timed out after 60000 ms' } as never
    const usage = world.forkUsage?.[forks.length - 1] ?? USAGE
    if (world.forkReason) return { value: { isAnswered: false, reason: world.forkReason, usage } } as never
    const text = world.replies?.[forks.length - 1] ?? 'dream: none'
    return { value: { isAnswered: true, text, usage } }
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
    if (e.argv[0] === 'mkdir') { // `mkdir -p` makes what is missing; a bare mkdir fails on a folder that is there
      const path = e.argv.at(-1) as string
      if (e.argv[1] !== '-p' && dirs.has(path)) return { value: { exitCode: 1, stdout: '', stderr: `mkdir: ${path}: File exists`, isStdoutTruncated: false, isStderrTruncated: false } }
      dirs.add(path)
      return ok('')
    }
    if (e.argv[0] === 'rmdir') {
      dirs.delete(e.argv.at(-1) as string)
      return ok('')
    }
    if (e.argv[0] === 'printenv') {
      const value = world.hostEnv?.[e.argv[1] as string]
      return value === undefined ? { value: { exitCode: 1, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } } : ok(`${value}\n`)
    }
    if (e.argv[0] === 'git') {
      // keyed by the args, after the variables the call sets over the host's (`GIT_CONFIG_COUNT=1 ... config ...`)
      const env = Object.entries(e.init?.env ?? {}).map(([name, value]) => `${name}=${value} `).join('')
      const key = env + e.argv.slice(3).join(' ')
      const fail = (exitCode: number) => ({ value: { exitCode, stdout: '', stderr: 'fatal', isStdoutTruncated: false, isStderrTruncated: false } })
      if (world.gitExit?.[key] !== undefined) return fail(world.gitExit[key] as number)
      if (e.argv.slice(-2).join(' ') === 'rev-parse --git-dir') return world.noRepo ? fail(128) : ok('.git\n')
      const said = (world.gitIn?.[e.argv[2] as string] ?? world.git)?.[key]
      return said === undefined ? { value: { exitCode: 1, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } } : ok(said)
    }
    if (e.argv[0] === 'node' && world.noNode) return { value: { exitCode: 127, stdout: '', stderr: 'node: not found', isStdoutTruncated: false, isStderrTruncated: false } }
    if (e.argv[0] === 'python3' && (e.argv[2] ?? '').endsWith('/bin/private.py')) { // `private.py write PATH [--redact]`: text on stdin, kept 0600
      files.set(e.argv[4] as string, e.init?.stdin ?? '')
      writes.push(e.argv[4] as string)
      return ok('')
    }
    if (e.argv[0] === 'python3' && (e.argv[2] ?? '').endsWith('/bin/lease.py')) { // a stand-in: lease.py's decide, on these files
      const arg = (name: string) => e.argv[e.argv.indexOf(name) + 1] as string
      const path = `${arg('--folder')}/${arg('--session')}.json`
      const [root, rank, now, busy] = [arg('--root'), Number(arg('--rank')), Number(arg('--now')), e.argv.includes('--busy')]
      let held: { root?: string; rank?: number; at?: number; busy?: boolean } | null = null
      try { held = JSON.parse(files.get(path) ?? 'null') } catch {}
      const mine = held?.root === root
      let answer = mine
      let write: object | null = null
      if (e.argv[3] === 'take') {
        const fresh = typeof held?.at === 'number' && now - held.at < LEASE_MS
        answer = !(held && !mine && fresh && ((held.rank ?? 0) >= rank || held.busy))
        if (answer) write = { root, rank, at: now, ...(busy ? { busy: true } : {}) }
      } else if (e.argv[3] === 'release' && mine && held?.busy) write = { root, rank: held.rank ?? rank, at: held.at ?? 0 }
      if (write) files.set(path, JSON.stringify(write))
      return ok(JSON.stringify({ held: answer, lease: write ?? held }))
    }
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
      if (world.jsonHangs && e.argv.includes('--json')) return { deny: 'timed out after 20000 ms' } // process.run rejects
      if (world.json) return ok(JSON.stringify(world.json))
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
  return { runs, statuses, opened, filled, forks, kept, store, configured, files, writes, compacting, toasts, compacted, everies, clock }
}

async function settle(done: () => boolean | Promise<boolean>) {
  for (let i = 0; i < 200 && !(await done()); i++) await new Promise<void>(resolve => setTimeout(() => resolve(), 5))
}

// Steps of Claude's main loop (or a subagent's, with agentId): n steps of a turn. The main session's continue the turn's index where the last call stopped (a step is one index of one
// turn, so a second call is new work); `from` replays the indexes from there. A subagent's start at 0, as they always did.
let mainNext = 0
async function steps($: Engine, n: number, agentId?: string, from?: number) {
  for (let k = 0; k < n; k++) {
    const index = from !== undefined ? from + k : agentId === undefined ? mainNext++ : k
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
const PANE = {
  plugin: 'csr-companion',
  surface: 'terminal' as const,
  component: 'Pane' as const,
  requestId: 'csr',
  props: { title: 'CSR', isFocused: true, bodyColumns: 140, placement: 'dock', scroll: { offset: 0, bodyRows: 40 } } as never,
}
// A first session in a folder: nothing found yet, but CSR has been at work this week.
const QUIET = { ...SHIFTS, total: 0, shifts: [], changes: [], changes_total: 0, recalled: 2, corrections_carried_7d: 3, shifts_told_7d: 1, git: true }
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
  expect(watcher?.some(arg => arg.endsWith('/bin/pane_guard.py'))).toBe(true) // the guard runs pane.py, and outlives a broken edit
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

test('on: the first look waits until Claude has spent 10 tokens for each one it costs; subagents do not count, and start no look', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  const band = await $.ui.mount(BAND)
  await steps($, 6) // 6 steps of Claude's: a look would be 1 in 6, over the share
  await settle(() => seen.kept.some(record => record.kind === 'look'))
  expect(seen.forks).toEqual([])
  expect(seen.kept[0]).toEqual(expect.objectContaining({ kind: 'chapter', why: 'start', left: 5, of: 5, session: 'sess-1' }))
  expect(seen.kept.slice(1)).toEqual([expect.objectContaining({ kind: 'look', outcome: 'budget', every: 6, session: 'sess-1' })])
  // both sides of the check: Claude's 6 steps against the next look's estimate (Claude's last step)
  expect(seen.kept[1]).toEqual(expect.objectContaining({ claude: 6 * LOOK, dreamer: 0, next: LOOK }))
  await steps($, 12, 'sub-1') // a subagent's 12 steps: not Claude's spend, no step of the chapter, no look
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ steps: 6 }))
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ claude: 6 * LOOK }))

  await steps($, 6) // 12 steps: 1 in 12, within it
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  expect(seen.forks.length).toBe(1)
  expect(seen.forks[0]).toContain(BRIEF)
  expect(seen.forks[0]).toContain('dream: none')
  expect(seen.forks[0]).toContain('You should know')
  expect(await band.find({ text: /Dream · / })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Already thought of it' })).toBeDefined()
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ looks: 1, dreams: 1, dreamer: LOOK, claude: 12 * LOOK }))
  // 4 left in the chapter, spread over the rest of a usual one: (120 - 12) / 4 = 27 steps
  expect(seen.kept.find(record => record.kind === 'dream')).toEqual(expect.objectContaining({
    line: DREAM_LINE, tokens: LOOK, session: 'sess-1', outcome: 'dream', left: 4, of: 5, every: 27,
    usage: USAGE, model: 'opus', // the fork's four counts, and the model of the step that started it
  }))

  await band.press({ key: 'Learn more' })
  expect(await band.find({ text: 'Notes could follow side branches' })).toBeDefined()
  expect(await band.find({ text: /this look 41,000 tokens/ })).toBeDefined()

  await band.press({ key: 'Ask Claude' })
  expect(seen.filled.length).toBe(1)
  expect(seen.filled[0]?.startsWith(`“${DREAM_LINE}”`)).toBe(true) // forward grammar v2: the question in its quotes
  expect(seen.filled[0]).toContain('> **Notes could follow side branches**')
  expect(seen.kept.at(-1)).toEqual(expect.objectContaining({ kind: 'answer', answer: 'ask_claude' }))
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ shown: 1, adopted: 1 })) // adopted: its place back
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
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ looks: 1, dreams: 0 }))
  expect(await band.find({ text: /Dream · / })).toBeUndefined()
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([ // step 6 waits for its share; step 12 finds nothing
    expect.objectContaining({ outcome: 'budget', every: 6, tokens: 0 }),
    expect.objectContaining({ outcome: 'none', every: 22, tokens: LOOK, left: 5, usage: USAGE, model: 'opus' }), // (120 - 12) / 5
  ])
  expect(seen.kept.find(record => record.outcome === 'budget')).not.toHaveProperty('usage') // no fork: nothing spent

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
  await settle(() => seen.store.get('pace:sess-1') !== undefined && (seen.store.get('pace:sess-1') as { looks: number }).looks >= 2)
  expect(seen.forks.length).toBe(2)
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ looks: 2 }))
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

// C1: one store serves every Claude process on the machine. Before keying, a step of another session's
// restarted this one's chapter (583 starts in 20 h, 2026-10-11/12) and zeroed its pace (39 false budget looks).
test('two sessions sharing the store keep their own chapters and pace: no restart, no false budget', ON, async ($, on) => {
  const world = { herdr: false, repo: true, replies: [DREAM_REPLY] } as World
  const seen = host(on, world)
  await $.ui.mount(BAND)
  const starts = () => seen.kept.filter(record => record.kind === 'chapter' && record.why === 'start').map(record => record.session)
  await steps($, 4) // sess-1: 4 steps
  world.sid = 'sess-2'
  await steps($, 4) // another window: 4 steps of its own
  const looks = () => seen.kept.filter(record => record.kind === 'look').length
  world.sid = 'sess-1'
  await steps($, 4) // sess-1 again: 8 steps, never restarted
  await settle(() => looks() >= 1) // its step-6 look ends before the other window's steps (one process, one session)
  world.sid = 'sess-2'
  await steps($, 4)
  await settle(() => looks() >= 2)
  world.sid = 'sess-1'
  await steps($, 4) // 12: the second look is due, and within the share
  await settle(() => seen.forks.length >= 1)
  expect(starts()).toEqual(['sess-1', 'sess-2'])
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ session: 'sess-1', steps: 12 }))
  expect(seen.store.get('chapter:sess-2')).toEqual(expect.objectContaining({ session: 'sess-2', steps: 8 }))
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ claude: 12 * LOOK, looks: 1 }))
  expect(seen.store.get('pace:sess-2')).toEqual(expect.objectContaining({ claude: 8 * LOOK, looks: 0 }))
  expect(seen.store.get('chapter')).toBeUndefined() // nothing unkeyed for another process to take
  expect(seen.kept.filter(record => record.kind === 'look').map(record => [record.session, record.outcome]))
    .toEqual([['sess-1', 'budget'], ['sess-2', 'budget']]) // only the step-6 looks, each in its own session
})

test('a claude -p child that loads the plugin never counts a step, starts a chapter or looks', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, sid: 'kid-1', headless: true, replies: [DREAM_REPLY] })
  await $.session.start({ ...START, isInteractive: false, surface: null } as never)
  await steps($, 30)
  await $.session.compact({ trigger: 'auto', messages: MESSAGES } as never)
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(seen.kept).toEqual([])
  expect([...seen.store.keys()].filter(key => /^(pace|chapter)/.test(key))).toEqual([])
})

test('a child loaded without a session.start is told apart by where it draws: nowhere', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, sid: 'kid-2', headless: true })
  await steps($, 12)
  await settle(() => false)
  expect(seen.kept).toEqual([])
})

test('a session start sweeps the unkeyed keys and all but the newest sessions', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  seen.store.set('chapter', { session: 'kid-0', started: NOW, steps: 1, due: 6, looks: 0, shown: 0, adopted: 0 })
  seen.store.set('pace', { session: 'kid-0', claude: 32346, dreamer: 0, looks: 0, dreams: 0, lastLook: 0 })
  for (let i = 0; i < 22; i++) {
    seen.store.set(`chapter:old-${i}`, { session: `old-${i}`, started: NOW - (i + 1) * 3_600_000, steps: 1, due: 6, looks: 0, shown: 0, adopted: 0 })
    seen.store.set(`pace:old-${i}`, { session: `old-${i}`, claude: 1, dreamer: 0, looks: 0, dreams: 0, lastLook: 0 })
  }
  seen.store.set('seenDreams', ['What if?'])
  await $.session.start(START as never)
  await settle(() => !seen.store.has('chapter'))
  await settle(() => !seen.store.has('pace:old-21'))
  expect(seen.store.has('pace')).toBe(false)
  expect(seen.store.has('chapter:old-0')).toBe(true) // newest 20 kept
  expect(seen.store.has('chapter:old-19')).toBe(true)
  expect(seen.store.has('chapter:old-20')).toBe(false)
  expect(seen.store.has('pace:old-21')).toBe(false)
  expect(seen.store.get('seenDreams')).toEqual(['What if?']) // shared lists stay
})

// Codex review 10-12, #9: a session that had written its asked or pace keys but no chapter yet (its first look not
// taken) lost them to another session's start: no chapter was read as a stale session.
test("a session start never sweeps a session whose chapter it has not seen; only known ones past the newest 20", ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  seen.store.set('asked:other-live', ['keep the import'])
  seen.store.set('pace:other-live', { session: 'other-live', claude: 41_000, dreamer: 0, looks: 0, dreams: 0, lastLook: 0 })
  for (let i = 0; i < 22; i++) {
    seen.store.set(`chapter:old-${i}`, { session: `old-${i}`, started: NOW - (i + 1) * 3_600_000, steps: 1, due: 6, looks: 0, shown: 0, adopted: 0 })
    seen.store.set(`asked:old-${i}`, ['x'])
  }
  await $.session.start(START as never)
  await settle(() => !seen.store.has('asked:old-21'))
  expect(seen.store.has('chapter:old-20')).toBe(false) // known and past the newest 20: swept, its other keys too
  expect(seen.store.has('asked:old-20')).toBe(false)
  expect(seen.store.get('asked:other-live')).toEqual(['keep the import'])
  expect(seen.store.get('pace:other-live')).toEqual(expect.objectContaining({ claude: 41_000 }))
})

// C3: the installed plugin and a dev copy of it in one session, each with its own store, asked twins 65 s apart.
const LEASE = `${HOME}/.claude-self-reflect/csr-companion/dreamer-lease/sess-1.json`
test('one dreamer per session: a fresh lease held by another dev copy keeps this one quiet', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY],
    files: { [LEASE]: JSON.stringify({ root: '/elsewhere/dev-mods/csr-companion', rank: 1, at: NOW - 60_000 }) } })
  await $.ui.mount(BAND)
  await steps($, 30)
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([]) // no budget, no quiet: it never looked
  expect(JSON.parse(seen.files.get(LEASE) ?? '{}').root).toBe('/elsewhere/dev-mods/csr-companion')
})

test('one dreamer per session: a dev copy takes the lease from the installed plugin, and a stale one from anyone', ON, async ($, on) => {
  const installed = '/home/u/.claude/plugins/cache/claude-self-reflect/csr-companion/0.4.0'
  const world = { herdr: false, repo: true, replies: [DREAM_REPLY],
    files: { [LEASE]: JSON.stringify({ root: installed, rank: 0, at: NOW - 60_000 }) } } as World
  const seen = host(on, world)
  await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks.length).toBe(1)
  const lease = JSON.parse(seen.files.get(LEASE) ?? '{}')
  expect(lease.root).not.toBe(installed)
  expect(lease).toEqual(expect.objectContaining({ rank: 1, at: NOW }))
  expect(rankOfRoot(installed)).toBe(0)
})

// Codex review 10-12, #11; astra r2 #4, r3 #4: the installed copy and a dev copy both took the lease and both forked
// (a paid look each), under each lock the mod built itself. bin/lease.py takes it now, the whole transaction under an
// OS lock; tests/test_lease.py races real processes for it. Here: what its answers make the dreamer do.
// astra r4 #8: a helper killed while it held the lock reads as exit 1, and exit 1 granted the lease: that copy looked
// unleased beside the one that then took it. Only exit 0 with JSON that says held grants it; python3 that cannot
// start at all is the one case left open (no lease can be kept on this machine).
test('one dreamer per session: only lease.py saying held grants the lease; python3 missing never stops a look', () => {
  const ok = (stdout: string) => ({ exitCode: 0, stdout })
  expect(leaseHeld(ok('{"held": true, "lease": {}}'))).toBe(true)
  expect(leaseHeld(ok('{"held": false, "lease": {}}'))).toBe(false)
  expect(leaseHeld(null, 'timed out after 2000 ms')).toBe(false) // its lock not got in time: another copy decides now
  expect(leaseHeld({ exitCode: 1, stdout: '' })).toBe(false) // crashed or killed
  expect(leaseHeld({ exitCode: 137, stdout: '{"held": true}' })).toBe(false)
  expect(leaseHeld(ok('not json'))).toBe(false)
  expect(leaseHeld(ok('{"held": "yes"}'))).toBe(false)
  expect(leaseHeld(null, 'spawn python3 ENOENT')).toBe(true) // python3 cannot start: the look goes on
})

test('one dreamer per session: a lease is taken under a lock, marked busy while the look runs, idle after', ON, async ($, on) => {
  const installed = '/home/u/.claude/plugins/cache/claude-self-reflect/csr-companion/0.5.0'
  let open = () => undefined as void
  const world = { herdr: false, repo: true, replies: [DREAM_REPLY], forkGate: new Promise<void>(done => (open = done)),
    files: { [LEASE]: JSON.stringify({ root: installed, rank: 0, at: NOW - 60_000, busy: true }) } } as World
  const seen = host(on, world)
  await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => false)
  expect(seen.forks).toEqual([]) // the installed copy's look runs: the dev copy waits
  seen.files.set(LEASE, JSON.stringify({ root: installed, rank: 0, at: NOW - 60_000 }))
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  expect(JSON.parse(seen.files.get(LEASE) ?? '{}')).toEqual(expect.objectContaining({ rank: 1, busy: true })) // ours, running
  const taken = seen.runs.find(run => (run[2] ?? '').endsWith('/bin/lease.py') && run[3] === 'take')
  expect(taken).toEqual(expect.arrayContaining(['--folder', `${HOME}/.claude-self-reflect/csr-companion/dreamer-lease`, '--session', 'sess-1', '--busy']))
  open()
  await settle(() => !JSON.parse(seen.files.get(LEASE) ?? '{}').busy)
  expect(JSON.parse(seen.files.get(LEASE) ?? '{}')).not.toHaveProperty('busy')
  expect(seen.runs.some(run => (run[2] ?? '').endsWith('/bin/lease.py') && run[3] === 'release')).toBe(true) // the look ended
})

test('one dreamer per session: a lease older than 15 minutes is taken, whoever held it', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY],
    files: { [LEASE]: JSON.stringify({ root: '/elsewhere/dev-mods/csr-companion', rank: 1, at: NOW - 16 * 60_000 }) } })
  await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks.length).toBe(1)
  expect(JSON.parse(seen.files.get(LEASE) ?? '{}').root).not.toBe('/elsewhere/dev-mods/csr-companion')
})

// C7: "ask_claude" means the box was filled; 3 of 7 never reached Claude as a prompt, and a dream sent from the
// pane was logged "ignored". A prompt that carries a dream is logged "sent", once.
const DREAM_LOG_PATH = `${HOME}/.claude-self-reflect/csr-companion/dreams.jsonl`
const LOGGED = [
  { kind: 'dream', id: 'dream:abc:1', at: NOW / 1000 - 60, line: DREAM_LINE, title: 'Notes could follow side branches', text: 'x' },
  { kind: 'answer', id: 'dream:abc:1', answer: 'ignored', at: NOW / 1000 - 30 },
].map(record => JSON.stringify(record)).join('\n')
test('a prompt that carries a dream is logged sent, once, from the band fill or the pane send', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [DREAM_LOG_PATH]: LOGGED } })
  seen.store.set('looksToSkip:sess-1', 1)
  seen.store.set('typedPastInARow:sess-1', 1)
  seen.store.set('chapter:sess-1', { session: 'sess-1', started: NOW - 3_600_000, steps: 10, due: 30, looks: 1, shown: 1, adopted: 0 })
  // the pane's send-text, with a band hotkey that landed in the box first
  await $.prompt.submit({ text: `2CSR's dreamer asked this while you worked: “${DREAM_LINE}” Its reasoning: Notes.`, origin: { kind: 'composer' } } as never)
  const sent = () => seen.kept.filter(record => record.kind === 'answer' && record.answer === 'sent')
  await settle(() => sent().length > 0)
  expect(sent()).toEqual([expect.objectContaining({ id: 'dream:abc:1', by: 'prompt' })])
  expect(seen.store.get('looksToSkip:sess-1')).toBe(0) // an answer: the back-off stops
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ adopted: 1 })) // its place back
  seen.files.set(DREAM_LOG_PATH, `${LOGGED}\n${JSON.stringify(sent()[0])}`)
  // the band's fill, sent again: already logged
  await $.prompt.submit({ text: askClaude({ line: DREAM_LINE, title: 'Notes could follow side branches', text: 'x' }), origin: { kind: 'composer' } } as never)
  await settle(() => false)
  expect(sent().length).toBe(1)
  // a prompt that only talks about the dreamer is not a sending
  await $.prompt.submit({ text: 'why did the dreamer ask that?', origin: { kind: 'composer' } } as never)
  await settle(() => false)
  expect(sent().length).toBe(1)
})

// 10-12 19:53: a dream went to Claude with "To my knowledge the dreamer is overdreaming i cannot kill a session" under
// it, and the log called it sent: adopted. A no beside it, or in the next prompt, rejects it, with the person's words.
const OVERDREAMT = 'To my knowledge the dreamer is overdreaming i cannot kill a session'
test('a dream sent on with a no beside it is rejected with your words, never sent, and gives no chapter place back', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [DREAM_LOG_PATH]: LOGGED } })
  seen.store.set('looksToSkip:sess-1', 2)
  seen.store.set('chapter:sess-1', { session: 'sess-1', started: NOW - 3_600_000, steps: 10, due: 30, looks: 1, shown: 1, adopted: 0 })
  const fill = askClaude({ line: DREAM_LINE, title: 'Notes could follow side branches', text: '* CSR only checks main.' })
  await $.prompt.submit({ text: `${fill}\n\n${OVERDREAMT}`, origin: { kind: 'composer' } } as never)
  const answers = () => seen.kept.filter(record => record.kind === 'answer')
  await settle(() => answers().length > 0)
  expect(answers()).toEqual([expect.objectContaining({ id: 'dream:abc:1', answer: 'rejected', by: 'prompt', said: OVERDREAMT })])
  expect(seen.store.get('looksToSkip:sess-1')).toBe(0) // an answer: the back-off stops
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ adopted: 0 }))
})

test('your next prompt after a dream you sent rejects it when it says no; only the next one, and only a no', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, files: { [DREAM_LOG_PATH]: LOGGED } })
  seen.store.set('chapter:sess-1', { session: 'sess-1', started: NOW - 3_600_000, steps: 10, due: 30, looks: 1, shown: 1, adopted: 0 })
  const fill = askClaude({ line: DREAM_LINE, title: 'Notes could follow side branches', text: 'x' })
  const said = (answer: string) => seen.kept.filter(record => record.kind === 'answer' && record.answer === answer)
  await $.prompt.submit({ text: fill, origin: { kind: 'composer' } } as never)
  await settle(() => said('sent').length > 0)
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ adopted: 1 }))
  seen.files.set(DREAM_LOG_PATH, `${LOGGED}\n${JSON.stringify(said('sent')[0])}`)
  await $.prompt.submit({ text: "No, that won't work: we can't restart a session from the phone.", origin: { kind: 'composer' } } as never)
  await settle(() => said('rejected').length > 0)
  expect(said('rejected')).toEqual([expect.objectContaining({ id: 'dream:abc:1', by: 'reply', said: "No, that won't work: we can't restart a session from the phone." })])
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ adopted: 0 })) // its place, given back at sent, is taken again
  await $.prompt.submit({ text: 'no, really not', origin: { kind: 'composer' } } as never) // a second reply is not a reply to it
  await settle(() => false)
  expect(said('rejected').length).toBe(1)
})

test('a reply that agrees, or one after the half hour, leaves the dream sent', ON, async ($, on) => {
  const world = { herdr: false, repo: true, files: { [DREAM_LOG_PATH]: LOGGED }, clock: { now: NOW } } as World
  const seen = host(on, world)
  const fill = askClaude({ line: DREAM_LINE, title: 'Notes could follow side branches', text: 'x' })
  const said = (answer: string) => seen.kept.filter(record => record.kind === 'answer' && record.answer === answer)
  await $.prompt.submit({ text: `${fill}\n\nyes, try it on one branch first`, origin: { kind: 'composer' } } as never)
  await settle(() => said('sent').length > 0)
  await $.prompt.submit({ text: "go on, I can't wait to see it", origin: { kind: 'composer' } } as never)
  await settle(() => false)
  expect(said('rejected')).toEqual([])
  seen.files.set(DREAM_LOG_PATH, LOGGED) // sent afresh, then a no 31 minutes on: no longer a reply to it
  await $.prompt.submit({ text: fill, origin: { kind: 'bridge' } } as never)
  await settle(() => said('sent').length > 1)
  world.clock!.now = NOW + 31 * 60_000
  await $.prompt.submit({ text: 'no, not that', origin: { kind: 'composer' } } as never)
  await settle(() => false)
  expect(said('rejected')).toEqual([])
  expect(wordsBeside(`2${fill}\n\nyes, try it`, DREAM_LINE)).toBe('yes, try it')
  expect(wordsBeside(`CSR's dreamer asked this while you worked: “${DREAM_LINE}” Its reasoning: x.\nnope, overdreaming`, DREAM_LINE)).toBe('nope, overdreaming')
  expect([rebuts(OVERDREAMT), rebuts('nope'), rebuts('I can’t do that from the phone'), rebuts('this is not what I asked'), rebuts('yes please'),
    rebuts('no-op is fine'), rebuts('not now, after the tests')]).toEqual([true, true, true, true, false, false, false])
})

// astra r4 #9 #10: a reply that opens with the dreamer's words was cut by its words after the structural strip, so a no
// read as nothing and the dream was logged adopted; a question quoted in the person's own words beside a forward
// named another, newer dream. Words beside a dream are what the grammar leaves; a sent dream is read from the forward.
test('the words beside a dream are all the person\'s, and the sent dream is the one its forward carries', () => {
  const A = { line: 'What if the import reused the upload queue?', title: 't', text: 'x', at: 1 }
  const B = { line: 'What if we disable validation?', title: 't', text: 'x', at: 2 }
  const reply = "CSR's dreamer asked this while you worked yesterday, but this is not useful."
  const said = wordsBeside(`${askClaude(A)}\n\n${reply}`, A.line)
  expect(said).toBe(reply)
  expect(rebuts(said)).toBe(true)
  const quoting = `${askClaude(A)}\n\nNever follow “What if we disable validation?”`
  expect(dreamSent(quoting, [A, B])?.line).toBe(A.line)
  const pane = "CSR's dreamer asked this while you worked, quoted: “What if the import reused the upload queue?” Its reasoning: “x.” Never “What if we disable validation?”"
  expect(dreamSent(pane, [A, B])?.line).toBe(A.line)
  expect(dreamSent("CSR's dreamer asked this while you worked yesterday: “What if we disable validation?”", [A, B])).toBeNull()
})

// astra r4 #11: a first pane.py call that timed out left the session with no refresh and no pane-answer sync.
test('a first pane.py call that times out still leaves the refresh and the pane-answer jobs running', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, jsonHangs: true })
  await $.session.start(START as never)
  await settle(() => seen.everies.length >= 2)
  expect(seen.everies).toEqual(expect.arrayContaining([10 * 60 * 1000, 15_000]))
  expect(seen.opened).toEqual([]) // nothing to open the pane with
})

test('dreamSent finds the dream a prompt carries, and nothing in prose that names the header', () => {
  const dreams = [{ line: 'What if a?', at: 1 }, { line: 'What if b?', at: 2 }]
  expect(dreamSent(`What if b?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **t**`, dreams)?.at).toBe(2)
  expect(dreamSent(`CSR's dreamer asked this while you worked: “What if a?” Its reasoning: t.`, dreams)?.at).toBe(1)
  expect(dreamSent('What if b? I wonder', dreams)).toBeNull() // no header: the person's own words
  expect(dreamSent(`I read "CSR's dreamer asked this while you worked" in the log`, dreams)).toBeNull()
  // astra r3 #8: v2 writes a question's own quotes straight; the dream is still found, from the pane and the band
  const fresh = [{ line: 'What if the pane said “fresh” first?', at: 3 }]
  const pane = "CSR's dreamer asked this while you worked, quoted: “What if the pane said \"fresh\" first?” Its reasoning: “t.”"
  expect(dreamSent(pane, fresh)?.at).toBe(3)
  expect(dreamSent(askClaude({ line: fresh[0]!.line, title: 't', text: 'x' }), fresh)?.at).toBe(3)
  expect(dreamSent(`2${askClaude({ line: 'What if b?', title: 't', text: 'x' })}`, dreams)?.at).toBe(2)
  expect(wordsBeside(`${askClaude({ line: fresh[0]!.line, title: 't', text: 'x' })}\n\nnope, overdreaming`, fresh[0]!.line)).toBe('nope, overdreaming')
  expect(wordsBeside(`${pane} no thanks`, fresh[0]!.line)).toBe('no thanks')
})

// C8: look records had no time and dropped the fork's reason; chapter records did not say who started them.
test('a look keeps how long it took and why a fork came back empty; a chapter says whether a person started it', ON, async ($, on) => {
  const world = { herdr: false, repo: true, forkReason: 'api-error', clock: { now: NOW } } as World
  const seen = host(on, world)
  await $.session.start(START as never)
  await steps($, 12)
  await settle(() => seen.kept.some(record => record.outcome === 'none'))
  expect(seen.kept.find(record => record.outcome === 'none')).toEqual(expect.objectContaining({ reason: 'api-error', took_ms: 0 }))
  expect(seen.kept.find(record => record.outcome === 'budget')).toEqual(expect.objectContaining({ took_ms: 0 }))
  expect(seen.kept.find(record => record.outcome === 'budget')).not.toHaveProperty('reason')
  expect(seen.kept.find(record => record.kind === 'chapter')).toEqual(expect.objectContaining({ interactive: true }))
})

// C6: the 10-11 twins, and the closest unrelated pair of the 18 dreams (pigment inks / the brochure sample).
const HEARTS_1 = { line: 'What if your hearts also ranked the feed, so the kinds of items you like rise first, as likes do on Instagram?', title: 'Hearts could steer the feed order' }
const HEARTS_2 = { line: 'What if the hearts you give in the pane told the dreamer which kinds of questions land, so it aims its next ones there?', title: 'Your hearts could steer the dreamer' }
const INKS = { line: "What if the pane took its colours from pigment's nyt inks, the palette your 10.x deck already uses, instead of hand-picked shades?", title: "The pane could share the deck's palette" }
const BROCHURE = { line: 'What if the dreams-off pane showed the dream you marked helpful on 10-10 as its sample, instead of describing dreams?', title: 'The brochure could show your own dream' }
test('a near twin of a dream already offered is a repeat; the closest unrelated pair is not', () => {
  expect(jaccard(seenEntry(HEARTS_1), seenEntry(HEARTS_2))).toBeGreaterThanOrEqual(NEAR)
  expect(nearDream(HEARTS_2, seenEntry(HEARTS_1))).toBe(true)
  expect(nearDream(BROCHURE, seenEntry(INKS))).toBe(false)
  expect(nearDream({ line: HEARTS_1.line.toUpperCase(), title: 'x' }, HEARTS_1.line)).toBe(true) // the exact match still holds, old entries too
})

test('a twin of a seen dream is logged as a repeat and never shown', ON, async ($, on) => {
  const twin = `dream: ${HEARTS_2.line}\nexplain:\n**${HEARTS_2.title}**\nThe hearts you give could teach the dreamer.`
  const seen = host(on, { herdr: false, repo: true, replies: [twin] })
  seen.store.set('seenDreams', [seenEntry(HEARTS_1)])
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.kept.some(record => record.outcome === 'repeat'))
  expect(seen.kept.find(record => record.kind === 'look' && record.tokens)).toEqual(expect.objectContaining({ outcome: 'repeat' }))
  expect(await band.find({ text: /hearts you give/ })).toBeUndefined()
  expect(seen.store.get('seenDreams')).toEqual([seenEntry(HEARTS_1)])
})

// Dreams in the person's language. The prompt asks for it; the parser took English alone, every all-Hindi question
// keyed to '' (so any two were "the same dream"), and the Jaccard of non-Latin text was 0. The Hindi pairs are the
// English ones above, said in Hindi: the twin keeps the twin's shared words (hearts, kinds, steer).
const HI_HEARTS_1 = { line: 'क्या हो अगर आपके दिल फ़ीड को भी क्रम दें, ताकि जिस प्रकार की चीज़ें आपको पसंद हैं वे पहले आएँ, जैसे इंस्टाग्राम पर लाइक?', title: 'दिल फ़ीड के क्रम को दिशा दे सकते हैं' }
const HI_HEARTS_2 = { line: 'क्या हो अगर पेन में आपके दिए दिल ड्रीमर को बताएँ कि किस प्रकार के सवाल काम आते हैं, ताकि वह अगले सवाल वहीं पूछे?', title: 'आपके दिल ड्रीमर को दिशा दे सकते हैं' }
const HI_INKS = { line: 'क्या हो अगर पेन अपने रंग पिगमेंट की nyt स्याही से ले, वही पैलेट जो आपका 10.x डेक पहले से इस्तेमाल करता है, हाथ से चुने रंगों के बजाय?', title: 'पेन डेक का पैलेट साझा कर सकता है' }
const HI_BROCHURE = { line: 'क्या हो अगर ड्रीम्स-बंद पेन वह सपना दिखाए जिसे आपने 10-10 को मददगार बताया था, सपनों का वर्णन करने के बजाय?', title: 'ब्रोशर आपका अपना सपना दिखा सकता है' }
const JA_NOTES_1 = { line: 'もしノートがサイドブランチも読んだら、今週の作業がメインに届く前にノートが付くのでは？', title: 'ノートはサイドブランチも読める' }
const JA_NOTES_2 = { line: 'もしノートがサイドブランチも読めば、メインに入る前に今週の作業にノートが付くのでは？', title: 'ノートがサイドブランチを追える' }
const JA_INKS = { line: 'もしペインの色をデッキのパレットから取ったら、手で選んだ色の代わりになるのでは？', title: 'ペインはデッキの色を使える' }
const MIXED_1 = { line: 'What if हम CSR के recap को हर session की शुरुआत में default बना दें?', title: 'Recap हर session में' }
const MIXED_2 = { line: 'What if pane के hearts dreamer को बताएँ कि कौन से सवाल काम आते हैं?', title: 'Hearts से dreamer सीखे' }
const AR = 'ماذا لو قرأت الملاحظات الفروع الجانبية أيضًا؟'
test('a dream in any language is a question when it ends on its question mark; no opening is required', () => {
  const asked = (line: string) => parseDream(`dream: ${line}\nexplain:\n**A title**\nWhy.`)?.line ?? null
  for (const line of [DREAM_LINE, 'Have you thought about one queue for both jobs?', HI_HEARTS_1.line, JA_NOTES_1.line, AR, MIXED_1.line]) {
    expect(asked(line)).toBe(line)
  }
  expect(asked(`**“${HI_BROCHURE.line}”**`)).toBe(HI_BROCHURE.line) // bold and quotes off
  expect(asked('क'.repeat(239) + '?')).not.toBeNull() // 240 characters
  expect(asked('क'.repeat(240) + '?')).toBeNull() // 241
  expect(asked('𠮷'.repeat(199) + '？')).not.toBeNull() // 200 characters, 399 UTF-16 units
  for (const line of ['none', 'none in Hindi either', 'What if we tested it first.', 'क्या हो अगर हम इसे पहले आज़माएँ', 'ノートがサイドブランチも読む。']) {
    expect(asked(line)).toBeNull()
  }
  // Codex review #15: ASCII is no sign of English. French, German and Spanish questions without accents, and an
  // English one in other words, are questions too: the English openings are accepted, never required.
  for (const line of ['Et si on partageait le cache entre les services ?', 'Was wäre, wenn wir den Cache teilen?',
    'Y si compartimos la cache entre los servicios?', 'Wenn wir den Cache teilen, was dann?', 'Should we test it first?',
    '« Et si on partageait le cache ? »'])
    expect(asked(line)).not.toBeNull()
  expect(asked('« Et si on partageait le cache ? »')).toBe('Et si on partageait le cache ?')
  expect(parseDream(HI_HEARTS_1.line)).toBeNull() // no dream: label
})

test('spaced scripts are told apart and their twins caught at the English NEAR; unspaced ones match exactly', () => {
  expect(sameDream(HI_INKS.line, HI_BROCHURE.line)).toBe(false) // keyed '' both, they were one dream
  expect(sameDream('?', '؟')).toBe(false)
  for (const [a, b] of [[HI_INKS, HI_BROCHURE], [HI_HEARTS_1, HI_INKS], [HI_HEARTS_2, HI_BROCHURE], [JA_NOTES_1, JA_INKS], [MIXED_1, MIXED_2], [MIXED_1, HI_HEARTS_1]] as const) {
    expect(nearDream(b, seenEntry(a))).toBe(false)
  }
  expect(jaccard(seenEntry(HI_HEARTS_1), seenEntry(HI_HEARTS_2))).toBeGreaterThanOrEqual(NEAR) // the reworded twin, as in English
  expect(nearDream(HI_HEARTS_2, seenEntry(HI_HEARTS_1))).toBe(true)
  for (const dream of [HI_HEARTS_1, JA_NOTES_1, MIXED_1]) expect(nearDream({ ...dream, title: 'x' }, seenEntry(dream))).toBe(true) // a true duplicate
  // Codex review #16: character pairs of Japanese share its grammar (もし…たらどうでしょうか): two dreams about a cache
  // and a screen scored 0.345 against NEAR's 0.14. Unspaced scripts match exactly (NFKC, punctuation aside) until a
  // filter is calibrated for them: a reworded twin goes through, a true duplicate does not.
  const CACHE = { line: 'もしキャッシュを共有したらどうでしょうか？', title: 'キャッシュ共有' }
  const SCREEN = { line: 'もし画面を暗くしたらどうでしょうか？', title: '画面の配色' }
  expect(nearDream(SCREEN, seenEntry(CACHE))).toBe(false)
  expect(nearDream({ line: 'もしキャッシュを共有したらどうでしょうか?', title: '別の題' }, seenEntry(CACHE))).toBe(true) // ？ or ?
  expect(nearDream(JA_NOTES_2, seenEntry(JA_NOTES_1))).toBe(false)
  expect(nearDream({ ...JA_INKS, line: `${JA_INKS.line} ` }, seenEntry(JA_INKS))).toBe(true)
  expect(nearDream(HEARTS_2, seenEntry(HEARTS_1))).toBe(true) // English as before
  expect(nearDream(BROCHURE, seenEntry(INKS))).toBe(false)
})

// Codex r2 #11 #13: two unrelated French dreams shared only "et", "si", "le", "un" and scored 0.2667, over NEAR: NEAR
// is calibrated on English and Hindi function words, never French. And a comma in Japanese split one question into
// two words, so a dream asked again with "、" was a new one. Fuzzy twins only for English and Hindi; any other
// language matches exactly, NFKC and punctuation aside, an unspaced script with no word boundaries put in.
test('fuzzy twins only where NEAR is calibrated; elsewhere an exact match, punctuation and spacing aside', () => {
  const FR_CACHE = { line: 'Et si on partageait le cache entre les services ?', title: 'Un cache commun' }
  const FR_STATUS = { line: 'Et si on affichait le statut dans le panneau ?', title: 'Un statut visible' }
  const asked = (dream: { line: string; title: string }) => parseDream(`dream: ${dream.line}\nexplain:\n**${dream.title}**\nWhy.`)
  for (const dream of [FR_CACHE, FR_STATUS]) expect(asked(dream)?.line).toBe(dream.line)
  expect(sameDream(FR_CACHE.line, FR_STATUS.line)).toBe(false)
  expect(nearDream(FR_STATUS, seenEntry(FR_CACHE))).toBe(false)
  expect(nearDream({ line: 'Et si on partageait le cache entre les services?', title: 'Autre' }, seenEntry(FR_CACHE))).toBe(true)
  const JA_A = { line: 'もしキャッシュを共有したらどうでしょうか？', title: 'キャッシュ共有' }
  const JA_B = { line: 'もしキャッシュを共有したら、どうでしょうか？', title: 'キャッシュ共有' }
  for (const dream of [JA_A, JA_B]) expect(asked(dream)?.line).toBe(dream.line)
  expect(sameDream(JA_A.line, JA_B.line)).toBe(true)
  expect(nearDream(JA_B, seenEntry(JA_A))).toBe(true)
  expect(sameDream('もしキャッシュを 共有したら　どうでしょうか。', JA_A.line)).toBe(true) // a space, an ideographic space, 。
  expect(sameDream('もし画面を暗くしたらどうでしょうか？', JA_A.line)).toBe(false)
  expect(nearDream(HEARTS_2, seenEntry(HEARTS_1))).toBe(true) // English twins, as calibrated
  expect(nearDream(HI_HEARTS_2, seenEntry(HI_HEARTS_1))).toBe(true) // Hindi twins too
})

test('a Hindi dream that repeats a seen one is dropped; a distinct one is shown', ON, async ($, on) => {
  const reply = (dream: { line: string; title: string }) => `dream: ${dream.line}\nexplain:\n**${dream.title}**\n* [session dc24d328]`
  const world = { herdr: false, repo: true, replies: [reply(HI_HEARTS_2), reply(HI_BROCHURE)] } as World
  const seen = host(on, world)
  seen.store.set('seenDreams', [seenEntry(HI_HEARTS_1), seenEntry(HI_INKS)])
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.kept.some(record => record.outcome === 'repeat'))
  expect(await band.find({ text: /ड्रीमर को बताएँ/ })).toBeUndefined()
  world.brief = `${BRIEF}\n* plugin retry-queue: built over 1 session, latest 10-08 [session 3f2a91c0]` // news: the next look is taken
  await steps($, 40)
  await settle(async () => (await band.find({ text: /ड्रीम्स-बंद पेन/ })) !== undefined)
  expect(seen.kept.find(record => record.kind === 'dream')).toEqual(expect.objectContaining({ line: HI_BROCHURE.line }))
})

// C4: receipts are checked and marked, never a gate (a gate kept out the only dream acted on, de1a2d6).
test('receipts: this session reads [this session], an unknown session or commit is marked, nothing is dropped', () => {
  const brief = '* skill notes-sync: built over 2 sessions [session dc24d328]'
  const text = 'On 08-26 [session dc24d328]. Here [session abcd1234]. Elsewhere [session 0450a37e]. Fixed in [commit f46448c], [commit 1234567].'
  expect(markReceipts(text, brief, 'abcd1234-0f0f', new Set(['1234567']))).toBe(
    "On 08-26 [session dc24d328]. Here [this session]. Elsewhere [session 0450a37e] (not in CSR's notes: unchecked). " +
    'Fixed in [commit f46448c], [commit 1234567] (not found in this repo: unchecked).')
  expect(dreamPrompt(brief, [], [])).toContain('never cite this session')
  expect(dreamPrompt(brief, [], [])).toContain("marked as the person's own words")
})

test('a dream citing this session and a commit the repo lacks is shown, its receipts marked', ON, async ($, on) => {
  const reply = `dream: ${DREAM_LINE}\nexplain:\n**Notes could follow side branches**\n* We did this here [session abcd1234].\n* Landed in [commit abcdef1].`
  const seen = host(on, { herdr: false, repo: true, sid: 'abcd1234-0f0f', replies: [reply], git: { 'cat-file -e f46448c^{commit}': '' } })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  const logged = seen.kept.find(record => record.kind === 'dream')
  expect(logged?.text).toContain('We did this here [this session].')
  expect(logged?.text).toContain('[commit abcdef1] (not found in this repo: unchecked)')
})

// C5: 10-11 18:48:54 the person asked for a dreams brochure; 30 s later a dream offered a step of it back.
test("the person's last three prompts reach the dreamer as already on the table; forwards and hotkeys do not", ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  for (const text of ['first ask', 'the system should offer to enable dreams and be a brochure <pasted_content id="a">pane</pasted_content>',
    'Here is a note offered by a side agent:\n\n> You should know · x', 'third ask', 'fourth ask']) {
    await $.prompt.submit({ text, origin: { kind: 'composer' } } as never)
  }
  expect(seen.store.get('asked:sess-1')).toEqual(['the system should offer to enable dreams and be a brochure', 'third ask', 'fourth ask'])
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks[0]).toContain('already on the table')
  expect(seen.forks[0]).toContain('- the system should offer to enable dreams and be a brochure')
  expect(seen.forks[0]).not.toContain('first ask')
})

// Codex r2 #9: a compaction card's draft sent as a prompt was kept whole as what the person "asked for or decided",
// and its "Learnt: Never skip validation." then vetoed a dream that offered another way to validation.
test('a forwarded card, story or dream is never what the person asked for; a reply they typed beside it is', ON, async ($, on) => {
  const CARD = "CSR's compaction card (compacted by csr): Context: 274k → 70k tokens. Learnt: Never skip validation."
  const DREAM = askClaude({ line: 'What if the import reused the upload queue?', title: 'Reuse the queue', text: '* It backs off already.' })
  expect(ownPrompt(CARD)).toBeNull()
  expect(ownPrompt(`${CARD}\nwhy did it keep that?`)).toBe('why did it keep that?')
  expect(ownPrompt("CSR's story of this session, rev 2: We moved the queue.")).toBeNull()
  expect(ownPrompt('Here is a note offered by a side agent:\n\n> Heads up · x\n>\n> **T**\n\nwhy?')).toBe('why?')
  expect(ownPrompt('Here is a note offered by CSR (claude-self-reflect):\n\n> Heads up · y')).toBeNull()
  expect(ownPrompt(`${DREAM}\n\nNo, keep the current palette.\n\n${DREAM}`)).toBe('No, keep the current palette.')
  expect(ownPrompt("CSR's compaction card (the one in the sidebar) should show tokens")).toBe("CSR's compaction card (the one in the sidebar) should show tokens")
  expect(ownPrompt("CSR's dreamer asked this while you worked yesterday, but I wrote this one")).toBe("CSR's dreamer asked this while you worked yesterday, but I wrote this one")
  const seen = host(on, { herdr: false, repo: true })
  for (const text of [CARD, `${CARD}\n\nkeep the card`]) await $.prompt.submit({ text, origin: { kind: 'composer' } } as never)
  expect(seen.store.get('asked:sess-1')).toEqual(['keep the card'])
  const asked = seen.store.get('asked:sess-1') as string[]
  expect(offersAnotherWay({ line: 'What if we checked types instead of validation?', title: 't', text: '' }, asked)).toBeNull()
  expect(offersAnotherWay({ line: 'What if we checked types instead of validation?', title: 't', text: '' }, [CARD])).toBe(CARD) // what it vetoed
})

// 10-12 19:51: "instead of compacting", 22 and 29 minutes after the person settled on compacting. A dream may not offer
// another way to what their last three prompts decided: the way it would replace shares a word with one of them.
const DECIDED = [
  'understood, one thing i manually do repeatedly is once context goes 180-250 mark based on where we are i compact to avoid exchanges',
  'done here means i will never fire compact just notice that is going to happen, on unattended sessions and with optin it should just trigger',
]
const RESTART = { line: "What if an unattended session at the top of your band restarted fresh with CSR's recap, instead of compacting?", title: 'Restarting could beat compacting',
  text: '* CSR could start a fresh session with its recap, rather than run a compaction.\n* On 06-10 you wrote "my vision: you kill a session, you restart" [session 0bab445f].' }
test('a dream that offers another way to what your last three prompts decided is dropped, never shown', ON, async ($, on) => {
  const reply = `dream: ${RESTART.line}\nexplain:\n**${RESTART.title}**\n${RESTART.text}`
  const seen = host(on, { herdr: false, repo: true, replies: [reply] })
  seen.store.set('asked:sess-1', DECIDED)
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.kept.some(record => record.outcome === 'decided'))
  expect(seen.kept.find(record => record.kind === 'look' && record.outcome === 'decided')).toEqual(expect.objectContaining({ against: DECIDED[0] }))
  expect(await band.find({ text: /restarted fresh/ })).toBeUndefined()
  expect(seen.forks[0]).toContain('or another way to them')
  expect(offersAnotherWay(RESTART, DECIDED)).toBe(DECIDED[0])
  expect(offersAnotherWay(RESTART, ['fix the importer'])).toBeNull() // nothing it replaces was decided
  expect(offersAnotherWay({ line: 'What if the import reused the upload queue, instead of a second one?', title: 't', text: '' }, DECIDED)).toBeNull()
  expect(offersAnotherWay({ line: 'What if the import reused the upload queue?', title: 't', text: 'It compacts less.' }, DECIDED)).toBeNull() // no other way offered
})

// 10-12 19:51: the same dream's whole reason was a line typed 06-10, "my vision: you kill a session, you restart". The
// brief marks a past line older than 30 days "context only"; a dream that stands on such lines alone is dropped.
const OLD_LINES = [
  'Lines the person typed in past sessions, in their own words, that sit close to this work (a line older than 30 days is context only: never the main reason to ask):',
  '- 06-10 (claude-self-reflect): “my vision: you kill a session, you restart” [session 0bab445f] (124 days old: context only)',
  '- 10-08 (csr-engine): “the import should retry” [session 3f2a91c0]',
].join('\n')
test('a dream whose reason is only a line older than 30 days is dropped; one that stands on something newer is shown', ON, async ($, on) => {
  const reply = `dream: ${RESTART.line}\nexplain:\n**${RESTART.title}**\n${RESTART.text}`
  const seen = host(on, { herdr: false, repo: true, replies: [reply], brief: `${BRIEF}\n\n${OLD_LINES}` })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.kept.some(record => record.outcome === 'stale'))
  expect(await band.find({ text: /restarted fresh/ })).toBeUndefined()
  expect(seen.forks[0]).toContain('context only')
  expect(leansOnOld(RESTART, OLD_LINES, 'sess-1')).toBe(true)
  const quoted = { ...RESTART, text: 'You wrote "you kill a session, you restart" once.' } // its words, no receipt
  expect(leansOnOld(quoted, OLD_LINES, 'sess-1')).toBe(true)
  expect(leansOnOld({ ...RESTART, text: `${RESTART.text}\n* The recap landed in [commit 19e7d46].` }, OLD_LINES, 'sess-1')).toBe(false)
  expect(leansOnOld({ ...RESTART, text: `${RESTART.text}\n* On 10-08 you asked for retries [session 3f2a91c0].` }, OLD_LINES, 'sess-1')).toBe(false)
  expect(leansOnOld({ ...RESTART, text: `${RESTART.text}\n* Here [session abcd1234].` }, OLD_LINES, 'abcd1234-0f0f')).toBe(true) // this session is no newer reason
  expect(leansOnOld(RESTART, BRIEF, 'sess-1')).toBe(false) // no old line in the brief
})

test('a chapter holds five dreams: with none left there is no look until a compaction starts the next', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  await $.ui.mount(BAND)
  seen.store.set('chapter:sess-1', { session: 'sess-1', started: NOW, steps: 0, due: 6, looks: 5, shown: 5, adopted: 0 })
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
  seen.store.set('briefFacts:sess-1', facts(BRIEF)) // the dreamer has seen every note already
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
  const fresh = seen.store.get('chapter:sess-1')
  open()
  await settle(() => seen.kept.some(record => record.outcome === 'stale'))
  expect(seen.store.get('chapter:sess-1')).toEqual(fresh) // due, looks and shown as the compaction set them
  expect(await band.find({ text: /What if the notes also read/ })).toBeUndefined()
  expect(seen.kept.find(record => record.outcome === 'stale')).toEqual(expect.objectContaining({ tokens: expect.any(Number) }))
})

// PR #317 review: the step hook and a look both read pace, wait, and write it back. A step that read it before the
// look wrote dropped the look's spend, and the look's late write dropped the steps': the 1-in-10 cap then counts
// wrong. Every pace change goes through one queue now, as the chapter's do.
test("a look's spend and Claude's steps both land when they interleave: pace changes go through one queue", ON, async ($, on) => {
  let open = () => undefined as void
  let land = () => undefined as void
  let held = false
  const landing = new Promise<void>(done => (land = done))
  const world = {
    herdr: false, repo: true, forkGate: new Promise<void>(done => (open = done)),
    // the look's write of what it spent is in flight while Claude's next steps count theirs
    storeGate: (key: string, value: unknown) => {
      if (key !== 'pace:sess-1' || held || (value as { looks?: number }).looks !== 1) return undefined
      held = true
      return landing
    },
  } as World
  const seen = host(on, world)
  await $.ui.mount(BAND)
  await steps($, 12) // the look starts at its due step and waits on its fork
  await settle(() => seen.forks.length >= 1)
  open()
  await settle(() => held)
  const going = steps($, 3)
  await settle(() => false)
  land()
  await going
  await settle(() => seen.kept.some(record => record.kind === 'look' && record.outcome === 'none'))
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ claude: 15 * LOOK, dreamer: LOOK, looks: 1, lastLook: LOOK }))
})

// Codex review of the imported plugin, #2: "at most 1 token in 10" was not enforced. The check before a look guesses
// the look's cost from the last one, and a fork takes no token ceiling (ModelForkRequest is a prompt and nothing
// else, claude-code.d.ts 2.1.296), so one look can cost far more than its share. What the plugin can do, and now
// does: write the real cost down at once, owe the excess, and run no look until Claude's own spending covers it.
const BIG = { ...USAGE, cache_read_input_tokens: 5 * LOOK - 1_000 } // a look that costs 5 steps of Claude's
test('the share: owed is what the dreamer spent past 1 token in 10, and nothing runs while any is owed', () => {
  const at = (claude: number, dreamer: number, lastLook = 0) => ({ ...freshPace('s'), claude, dreamer, lastLook })
  expect(owed(at(1_000, 100))).toBe(0)
  expect(Math.round(owed(at(1_000, 160)))).toBe(60)
  expect(canAfford(at(1_000, 0), 100)).toBe(true) // 100 more is exactly the share
  expect(canAfford(at(1_000, 0), 101)).toBe(false) // the guess alone is over it
  expect(canAfford(at(1_000, 60, 40), 0)).toBe(true) // the last look's cost is the guess when there is one
  expect(canAfford(at(1_000, 60, 41), 0)).toBe(false)
  // Over the share (a look that cost more than guessed): even a guess of 0 does not run a look.
  expect(canAfford(at(1_000, 160), 0)).toBe(false)
  expect(canAfford(at(1_000, 160, 0), 0)).toBe(false)
  // Claude's spending is what pays it back: the same dreamer total, once Claude has spent enough that the total and the guess fit.
  expect(canAfford(at(1_600, 160, 160), 0)).toBe(false) // 320 > 160
  expect(canAfford(at(3_200, 160, 160), 0)).toBe(true) // 320 <= 320
})

test('the pane says when a look ran over its share, and does not claim "never"', () => {
  const over = { ...freshPace('s'), claude: 100_000, dreamer: 160_000, looks: 1, lastLook: 160_000 }
  expect(paceText(over, null, true)).toContain('A look ran over its share by 150k; the next waits until Claude\'s spending has caught up.')
  expect(paceText({ ...over, claude: 2_000_000 }, null, true)).not.toContain('ran over')
  expect(paceText(null, null, true)).toContain('checks before each look that its spend stays within 1 token for every 10 Claude spends')
  expect(paceText(null, null, true)).not.toMatch(/never spends|at most/)
})

test('an overshooting look is owed: the next looks are refused until Claude has spent enough to cover it', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: ['dream: none', 'dream: none'], forkUsage: [BIG] })
  await $.ui.mount(BAND)
  await steps($, 12) // 12 steps: the first look is within the share (1 in 12) by its guess, and costs 5 steps of Claude's
  await settle(() => seen.kept.some(record => record.kind === 'look' && record.outcome === 'none'))
  expect(seen.forks.length).toBe(1)
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ claude: 12 * LOOK, dreamer: 5 * LOOK, looks: 1, lastLook: 5 * LOOK }))
  const first = seen.kept.find(record => record.kind === 'look' && record.outcome === 'none')
  expect(Math.round((first as { owed: number }).owed)).toBe(Math.round(5 * LOOK - 1.2 * LOOK)) // 205k spent against a 49.2k share

  const looks = () => seen.kept.filter(record => record.kind === 'look' && record.outcome !== 'none')
  await steps($, 30) // the next look is due at 34, and every 6 steps after a refusal
  await settle(() => looks().length >= 3)
  expect(seen.forks.length).toBe(1) // not a second fork
  expect(looks().every(record => record.outcome === 'budget')).toBe(true)
  expect((looks().at(-1) as { owed: number }).owed).toBeGreaterThan(0) // still over the share at step 40

  // Claude spends on: the total and the guess fit again, so the gate opens (the brief holds nothing new: quiet, no fork).
  seen.store.set('pace:sess-1', { ...(seen.store.get('pace:sess-1') as object), claude: 100 * LOOK })
  const before = looks().length
  await steps($, 12)
  await settle(() => looks().length > before && looks().at(-1)?.outcome !== 'budget')
  expect(looks().at(-1)).toEqual(expect.objectContaining({ outcome: 'quiet', owed: 0 }))
})

test('a fork is paid for even when its look is dropped: a lost lease does not lose the spend', ON, async ($, on) => {
  let open = () => undefined as void
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY], forkGate: new Promise<void>(done => (open = done)), forkUsage: [BIG] })
  await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => seen.forks.length >= 1)
  seen.files.set(LEASE, JSON.stringify({ root: '/elsewhere/dev-mods/csr-companion', rank: 9, at: NOW })) // another copy holds it now
  open()
  await settle(() => ((seen.store.get('pace:sess-1') as { dreamer?: number } | undefined)?.dreamer ?? 0) > 0)
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ dreamer: 5 * LOOK, looks: 1, lastLook: 5 * LOOK }))
  await settle(() => false)
  expect(seen.kept.filter(record => record.kind === 'dream')).toEqual([]) // the look was dropped; the money was not forgotten
})

test('a fork that rejects may have been paid for: it is charged as the guess', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, forkFails: true })
  await $.ui.mount(BAND)
  await steps($, 12)
  await settle(() => ((seen.store.get('pace:sess-1') as { looks?: number } | undefined)?.looks ?? 0) >= 1)
  expect(seen.store.get('pace:sess-1')).toEqual(expect.objectContaining({ looks: 1, dreamer: LOOK })) // Claude's last step
})

test('a compaction carries the dream that is up once, marked still open; the next one folds it', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, replies: [DREAM_REPLY] })
  const band = await $.ui.mount(BAND)
  await steps($, 12)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) !== undefined)
  await $.session.compact({ trigger: 'manual', messages: MESSAGES } as never)
  await settle(async () => (await band.find({ text: /still open from before the compaction/ })) !== undefined)
  expect(await band.find({ text: /What if the notes also read/ })).toBeDefined()
  expect(seen.kept.filter(record => record.kind === 'answer')).toEqual([expect.objectContaining({ answer: 'carried' })])
  expect(seen.store.get('chapter:sess-1')).toEqual(expect.objectContaining({ shown: 0, steps: 0 }))
  await $.prompt.submit({ text: 'one', origin: { kind: 'human' } } as never)
  expect(await band.find({ text: /What if the notes also read/ })).toBeDefined() // two prompts again to answer it

  await $.session.compact({ trigger: 'manual', messages: MESSAGES } as never)
  await settle(async () => (await band.find({ text: /What if the notes also read/ })) === undefined)
  expect(seen.kept.filter(record => record.kind === 'answer').map(record => record.answer)).toEqual(['carried', 'expired'])
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
  expect(seen.store.get('looksToSkip:sess-1')).toBe(1)
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
  expect(seen.store.get('looksToSkip:sess-1') ?? 0).toBe(0)
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
  expect(seen.store.get('looksToSkip:sess-1')).toBeUndefined()

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
  // The companion's own files go through bin/private.py (0700 folders, 0600 files); the person's settings.json does not.
  const private_ = seen.runs.filter(argv => (argv[2] ?? '').endsWith('/bin/private.py')).map(argv => argv[4])
  expect(private_).toEqual([STABLE, BACKUP])
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
  world.git['config --get-all remote.origin.push'] = 'refs/heads/*:refs/heads/*\n'
  expect((await bash('git push')).deny).toBe(RELEASE)
  world.git = {}
  expect((await bash('git push')).deny).toBeUndefined()
})

// Codex review 10-12, #5 #6: where a push lands was asked of git without the command's own -c, against every
// remote's push refspecs; and of two pushes the words leave to the repository only the first was asked.
test("where a push lands is asked with the command's own config, of its own remote, for every such push", async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  const SPEC = "remote.origin.push='refs/heads/*:refs/heads/*'" // release/v* spelled out is the tripwire's
  const world = { herdr: false, repo: true, git: {
    'symbolic-ref -q --short HEAD': 'main\n',
    ['-c remote.origin.push=refs/heads/*:refs/heads/* symbolic-ref -q --short HEAD']: 'main\n',
    ['-c remote.origin.push=refs/heads/*:refs/heads/* config --get-all remote.origin.push']: 'refs/heads/*:refs/heads/*\n',
    'config --get-all remote.mirror.push': 'refs/heads/*:refs/heads/release/v9\n', // another remote's: not this push's
  } as Record<string, string> }
  host(on, world)
  expect((await bash(`git -c ${SPEC} push origin`)).deny).toBe(RELEASE)
  expect((await bash('git push origin')).deny).toBeUndefined()
  expect((await bash('git push mirror')).deny).toBe(RELEASE)
  world.git['config --get remote.pushDefault'] = 'mirror\n' // no remote named: the default push remote's
  expect((await bash('git push')).deny).toBe(RELEASE)
  delete world.git['config --get remote.pushDefault']
  world.git['config --get push.default'] = 'matching\n' // every branch both sides have
  expect((await bash('git push origin')).deny).toBe(RELEASE)
  delete world.git['config --get push.default']
  expect(verdictOf('git -C /a push origin HEAD; git -C /b push origin HEAD')).toEqual({ kind: 'check', pushes: [
    { dir: '/a', remote: 'origin', config: [] }, { dir: '/b', remote: 'origin', config: [] }] })
})

// Codex r2 #5: `git -c push.default=matching push origin HEAD` on main was denied, though git consults push.default
// only "when the command line does not specify what to push with <refspec>" (git push docs).
test('push.default=matching counts only for a push that names no refspec', async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  const world = { herdr: false, repo: true, git: {
    '-c push.default=matching symbolic-ref -q --short HEAD': 'main\n',
    '-c push.default=matching config --get push.default': 'matching\n',
  } as Record<string, string> }
  host(on, world)
  for (const c of ['git -c push.default=matching push origin HEAD', 'git -c push.default=matching push origin @', 'git -c push.default=matching push -u origin HEAD'])
    expect((await bash(c)).deny).toBeUndefined()
  for (const c of ['git -c push.default=matching push origin', 'git -c push.default=matching push'])
    expect((await bash(c)).deny).toBe(RELEASE)
  expect(verdictOf('git push origin HEAD')).toEqual({ kind: 'check', pushes: [{ dir: null, remote: 'origin', config: [] }] })
  expect(verdictOf('git push origin')).toEqual({ kind: 'check', pushes: [{ dir: null, remote: 'origin', config: [], implicit: true }] })
})

// Codex r2 #2: `DEST=... git --config-env=remote.origin.push=DEST push origin` set a release refspec the checks never
// saw (probed on git 2.50: `git --config-env=remote.origin.push=DEST config --get-all remote.origin.push` prints it).
// So did GIT_CONFIG_COUNT/KEY/VALUE in the command's environment. A refspec the command spells with release/v in it is
// the tripwire's; these spell a wildcard (every branch) or read it from the hook's environment.
test('--config-env counts as -c with its variable\'s value, and GIT_CONFIG_* in the environment count too', async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  const SPEC = 'refs/heads/study/dreamer-2026-10-12:refs/heads/release/v9'
  const WILD = 'refs/heads/*:refs/heads/*'
  const SAFE = 'refs/heads/main:refs/heads/main'
  const world = { herdr: false, repo: true, hostEnv: { FROM_HOST: SPEC } as Record<string, string>, git: {
    'symbolic-ref -q --short HEAD': 'study/dreamer-2026-10-12\n',
    [`-c remote.origin.push=${SPEC} config --get-all remote.origin.push`]: `${SPEC}\n`,
    [`-c remote.origin.push=${WILD} config --get-all remote.origin.push`]: `${WILD}\n`,
    [`-c remote.origin.push=${SAFE} config --get-all remote.origin.push`]: `${SAFE}\n`,
    [`GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.push GIT_CONFIG_VALUE_0=${WILD} config --get-all remote.origin.push`]: `${WILD}\n`,
  } as Record<string, string> }
  host(on, world)
  for (const c of [`DEST='${WILD}' git --config-env=remote.origin.push=DEST push origin`,
    `DEST='${WILD}' git --config-env remote.origin.push=DEST push origin`, `env DEST='${WILD}' git --config-env=remote.origin.push=DEST push`,
    `export DEST='${WILD}'; git --config-env=remote.origin.push=DEST push origin`, 'git --config-env=remote.origin.push=FROM_HOST push origin',
    `GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.push GIT_CONFIG_VALUE_0='${WILD}' git push origin`])
    expect((await bash(c)).deny).toBe(RELEASE)
  expect((await bash(`DEST=${SAFE} git --config-env=remote.origin.push=DEST push origin`)).deny).toBeUndefined()
  expect((await bash('git --config-env=remote.origin.push=NOT_SET push origin')).deny).toBe(UNRESOLVED) // unknown: denied
  expect((await bash('git --config-env=remote.origin.push=-x push origin')).deny).toBe(UNRESOLVED)
  expect((await bash('git push origin')).deny).toBeUndefined()
})

// astra r3 #2: git takes the last of two settings, and the resolver put every -c before every --config-env: `MODE=current
// git --config-env=push.default=MODE -c push.default=matching push origin` pushes matching, and went through.
// astra r3 #3: `GIT_CONFIG_COUNT=$N ...` reached git as a literal $N, every question failed (exit 128), and the push
// went on. Now the order is kept, and what the check cannot resolve is denied: an expansion in a setting, a
// variable, -C or cd; settings git refuses; a question that fails other than by "not set".
test('settings keep their order, and what the check cannot resolve is denied', async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  const world = { herdr: false, repo: true, git: {
    'symbolic-ref -q --short HEAD': 'main\n',
    '-c push.default=current -c push.default=matching config --get push.default': 'matching\n',
    '-c push.default=matching -c push.default=current config --get push.default': 'current\n',
  } as Record<string, string>, gitExit: {} as Record<string, number>, noRepo: false }
  host(on, world)
  expect((await bash('MODE=current git --config-env=push.default=MODE -c push.default=matching push origin')).deny).toBe(RELEASE)
  expect((await bash('MODE=current git -c push.default=matching --config-env=push.default=MODE push origin')).deny).toBeUndefined()
  for (const c of ['N=1\nGIT_CONFIG_COUNT=$N GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=Me \\\n  git -c push.default=matching push origin',
    'git -c "push.default=$MODE" push origin', 'DEST=$(cat spec) git --config-env=remote.origin.push=DEST push origin',
    'GIT_CONFIG_VALUE_0=`cat v` git push origin', 'git -C "$REPO" push origin', 'cd ~/repo && git push', 'cd "$R" && git push origin HEAD',
    'git -c "core.$KEY=x" commit -m x'])
    expect((await bash(c)).deny).toBe(UNRESOLVED)
  expect((await bash("git -c 'push.default=$MODE' push origin")).deny).toBeUndefined() // single quotes: literal
  expect((await bash('cd sub && git push origin HEAD')).deny).toBeUndefined()
  world.noRepo = true // git refuses the settings, or no repository
  expect((await bash('git push origin')).deny).toBe(UNRESOLVED)
  world.noRepo = false
  world.gitExit['config --get push.default'] = 128 // a question that fails other than by "not set"
  expect((await bash('git push origin')).deny).toBe(UNRESOLVED)
  expect((await bash('git push origin feat/x')).deny).toBeUndefined() // a named branch: nothing to ask git
})

// astra r3 #1 #9 and the tripwire. Before any parsing, a git command with --no-verify, hooksPath, release/v, --mirror
// or a ':' refspec anywhere in its text is denied: `((1<<2))` and a continued `cat <<1 \` hid a push from the
// parser (probed with bash 3.2 and 5.3). Text that only mentions them is denied too, with the hint to use a file.
// The owner, 10-12: the guard hardcoded this repository's rules for every user of a public plugin, and its tripwire
// fired on the word git anywhere (a `codex exec "<prompt naming git and the hooks flag>"` was denied). The guard is
// opt-in now: <git toplevel>/.claude/csr-guard.json and the person's guard.json, every protection of either; neither,
// no guard. The tripwire reads only where git is the command.
test('the guard is opt-in: no file, nothing; a repository file, a user file, both; a bad file counts for nothing', async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  const world = { herdr: false, repo: true, guard: { [REPO_GUARD]: null } as Record<string, string | null>,
    git: { 'symbolic-ref -q --short HEAD': 'release/abc\n' } as Record<string, string> }
  const seen = host(on, world)
  const PUSH = 'git push origin release/v9'
  const SKIP = 'git commit --no-verify -m x'
  expect([(await bash(PUSH)).deny, (await bash(SKIP)).deny]).toEqual([undefined, undefined]) // no file: no guard
  expect(seen.runs.filter(run => run[0] === 'git')).toEqual([]) // and no git asked anything
  seen.files.set(REPO_GUARD, JSON.stringify(GUARD_JSON))
  expect([(await bash(PUSH)).deny, (await bash(SKIP)).deny]).toEqual([RELEASE, NO_VERIFY]) // the repository's
  seen.files.delete(REPO_GUARD)
  seen.files.set(USER_GUARD, JSON.stringify({ denyNoVerify: true })) // the person's alone
  expect([(await bash(PUSH)).deny, (await bash(SKIP)).deny]).toEqual([undefined, NO_VERIFY])
  expect((await bash('git push')).deny).toBeUndefined() // nothing protected: no question for git
  seen.files.set(REPO_GUARD, JSON.stringify({ protectedPush: ['release/*'] })) // both, and a glob of its own
  expect((await bash('git push origin release/x')).deny).toBe(PROTECTED('release/*'))
  expect((await bash('git push origin main')).deny).toBeUndefined()
  expect((await bash('git push')).deny).toBe(PROTECTED('release/*')) // on release/abc: git says where it lands
  expect((await bash(SKIP)).deny).toBe(NO_VERIFY)
  seen.files.set(REPO_GUARD, '{"protectedPush": ["release/v*"') // not JSON: counts for nothing, said once
  seen.files.delete(USER_GUARD)
  expect([(await bash(PUSH)).deny, (await bash(SKIP)).deny]).toEqual([undefined, undefined])
  const log = seen.files.get(`${HOME}/.claude-self-reflect/csr-companion/pane-errors.log`) ?? ''
  expect(log.split('\n').filter(line => line.includes(REPO_GUARD))).toHaveLength(1)
  expect(log).toContain('is no guard')
  expect(parseGuard('{"denyNoVerify": "yes"}')).toBeNull() // a key of the wrong kind
  expect(mergeGuards([{ denyNoVerify: false, protectedPush: [], reason: {} }])).toBeNull()
})

test('the tripwire reads only where git is the command', () => {
  for (const c of ['codex exec "review the git range; check --no-verify and release/v9 handling"', "grep -n 'release/v' file && git status",
    'echo git push origin release/v9'])
    expect(verdictOf(c)).toBeNull()
  for (const c of ['((1<<2))\ngit push origin release/v9', 'cat <<1 \\\n; git push origin release/v9\nx\n1', 'bash <<\\EOF\ngit push origin release/v9\nEOF',
    'sudo -u me git push origin release/v9', 'FOO=1 env BAR=2 timeout 5 git commit --no-verify -m x', 'x=$(git push origin release/v9)',
    'ls | xargs -I{} git push origin release/v9', '/usr/bin/git push --mirror backup'])
    expect(verdictOf(c)?.kind).toBe('deny')
})

test('the tripwire reads the raw text first; the parse only makes a denial more precise', () => {
  const v = (c: string) => verdictOf(c)
  for (const c of ['((1<<2))\ngit push origin release/v9', 'cat <<1 \\\n; git push origin release/v9\nx\n1',
    "cat <<'EOF'; bash -c ':'\ngit push origin release/v9\nEOF", 'for ((i=0; i<<1; i++)); do :; done\ngit push origin release/v9'])
    expect(v(c)?.kind).toBe('deny')
  // only where git stands as the command: a message line that merely names the flag goes through; a heredoc line
  // that starts with git still trips it (accepted)
  expect(v("git commit -F - <<'EOF'\nfix: never pass --no-verify\nEOF")).toBeNull()
  expect(v("cat <<'EOF' > notes.md\ngit push origin release/v9\nEOF")).toEqual({ kind: 'deny', reason: `${RELEASE} ${AS_TEXT}` })
  expect(v('git commit -m "mention release/v9"')).toEqual({ kind: 'deny', reason: `${RELEASE} ${AS_TEXT}` })
  expect(v('git commit --no-verify -m x')).toEqual({ kind: 'deny', reason: NO_VERIFY }) // a real one: the parse's reason
  expect(v('git -c core.hooksPath=/dev/null commit -m x')).toEqual({ kind: 'deny', reason: HOOKS_OFF })
  for (const c of ['git push origin :feat/old', 'git push --mirror backup', 'GIT_CONFIG_KEY_0=core.hooksPath git commit -m x'])
    expect(v(c)?.kind).toBe('deny')
  for (const c of ['echo release/v9', 'ls --no-verify', 'git status', 'gitk release/v9', 'cat .git/release/v9']) expect(tripwire(c, GUARD)).toBeNull()
  // the parse alone: arithmetic and continued lines are no heredocs, and a heredoc is a script only in a shell's pipeline
  expect(parseOf('((1<<2))\ngit commit -n -m x')).toEqual({ kind: 'deny', reason: NO_VERIFY })
  expect(parseOf('x=$[1<<2]\ngit commit -n -m x')).toEqual({ kind: 'deny', reason: NO_VERIFY })
  expect(parseOf('cat <<1 \\\n; git commit -n -m x\nx\n1')).toEqual({ kind: 'deny', reason: NO_VERIFY })
  expect(parseOf("cat <<'EOF'; bash -c ':'\ngit commit -n -m x\nEOF")).toBeNull()
  expect(parseOf("cat <<'EOF' | bash -s\ngit commit -n -m x\nEOF")).toEqual({ kind: 'deny', reason: NO_VERIFY })
  expect(parseOf('git commit \\\n  -n -m x')).toEqual({ kind: 'deny', reason: NO_VERIFY })
})

test('of two pushes the words leave to the repository, either one on release/v* is denied', async ($, on) => {
  const bash = (command: string) => $.tool.call({ tool: 'Bash', command } as never) as Promise<{ deny?: string }>
  host(on, { herdr: false, repo: true, gitIn: {
    '/a': { 'symbolic-ref -q --short HEAD': 'main\n' }, '/b': { 'symbolic-ref -q --short HEAD': 'release/v9\n' } } })
  expect((await bash('git -C /a push origin HEAD; git -C /b push origin HEAD')).deny).toBe(RELEASE)
  expect((await bash('git -C /a push origin HEAD; git -C /a push origin HEAD')).deny).toBeUndefined()
})

test('gitVerdict reads git as a command word, not git in words', () => {
  const kind = (c: string) => parseOf(c)?.kind ?? null
  const reason = (c: string) => {
    const v = parseOf(c)
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
  expect(verdictOf('cd /r && git push', 0, '/elsewhere')).toEqual({ kind: 'check', pushes: [{ dir: '/r', remote: null, config: [], implicit: true }] })
  expect(verdictOf('git -C sub push origin HEAD', 0, '/r')).toEqual({ kind: 'check', pushes: [{ dir: '/r/sub', remote: 'origin', config: [] }] })
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
  const kind = (c: string) => parseOf(c)?.kind ?? null
  for (const c of ['git commit -an -m x', 'git commit -nm x', 'git commit -m x -n']) expect(kind(c)).toBe('deny')
  for (const c of ['git commit -uno -m x', 'git commit -m n', 'git commit -mn', 'git commit -m x -- -n', 'git commit -F notes -S'])
    expect(kind(c)).toBe(null)
})

// PR #317 review: --gpg-sign takes its key id only as --gpg-sign=<keyid> (-S only in its own cluster), so a bare
// --gpg-sign never eats the next word: `git commit --gpg-sign --no-verify` skips the hooks. Probed on git 2.50: the
// hook does not run. Git also takes a long option by any unambiguous prefix: --no-veri is --no-verify.
test('an option whose value is optional never eats the next word, and an abbreviated --no-verify is still a skip', () => {
  const kind = (c: string) => parseOf(c)?.kind ?? null
  for (const c of ['git commit --gpg-sign --no-verify -m x', 'git commit -S --no-verify -m x', 'git commit --gpg-sign=ABC --no-verify -m x',
    'git commit -Sabc -n -m x', 'git commit --untracked-files --no-verify -m x', 'git commit -m x --no-veri', 'git commit --no-verif -m x',
    'git push --no-veri origin main', 'git push --signed --no-verify origin main'])
    expect(kind(c)).toBe('deny')
  for (const c of ['git commit --no-ver -m x', 'git commit -m x --gpg-sign', 'git commit -S -m "--no-verify"', 'git commit --no-verify --veri -m x'])
    expect(kind(c)).toBe(null) // --no-ver is ambiguous (--no-verbose): git refuses it; --veri turns verify back on
  // -o's value sits after it in a push cluster: `-on` is the push option "n", not a dry run
  expect(verdictOf('git push -on origin release/v1')).toEqual({ kind: 'deny', reason: RELEASE })
  expect(verdictOf('git push -uo nightly origin release/v1')).toEqual({ kind: 'deny', reason: RELEASE })
  expect(kind('git push -nu origin release/v1')).toBe(null) // -n first: a dry run
})

// PR #317 review: `timeout`, `nice` and `eval` hid a push from the guard; `git -c core.hooksPath=...` skips the hooks
// as --no-verify does (probed on git 2.50: the hook does not run). Wrappers git already sat under stay covered.
test('the guard reads git under timeout, nice, stdbuf and eval, and a hooksPath set for one call is a skip', () => {
  const reason = (c: string) => {
    const v = parseOf(c)
    return v?.kind === 'deny' ? v.reason : (v?.kind ?? null)
  }
  for (const c of ['timeout 120 git push origin release/v1', 'timeout -s KILL 60 git push origin release/v1',
    'timeout --signal=KILL -k 5 60 git push origin release/v1', 'nice -n 10 git push origin release/v1', 'nice -5 git push origin release/v1',
    'stdbuf -o L git push origin release/v1', 'eval "git push origin release/v1"', 'eval git push origin release/v1',
    'sudo -S git push origin release/v1', 'zsh -c "git push origin release/v1"', 'env FOO=1 git push origin release/v1',
    'command git push origin release/v1', 'true && git push origin release/v1', 'false || git push origin release/v1',
    'echo y | git push origin release/v1'])
    expect(reason(c)).toBe(RELEASE)
  for (const c of ['nice git commit --no-verify -m x', 'sudo -n git commit --no-verify -m x', 'timeout 60 git commit -n -m x',
    'eval "git commit --no-verify -m x"', 'git -C /r commit --no-verify -m x; echo done'])
    expect(reason(c)).toBe(NO_VERIFY)
  for (const c of ['git -c core.hooksPath=/dev/null commit -m x', 'git -c core.hookspath= commit -m x',
    'git -c user.name=me -c core.hooksPath=/tmp/none push origin main', 'git --config-env=core.hooksPath=HP merge topic',
    'GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null git commit -m x',
    'bash -c "git -c core.hooksPath=/dev/null commit -m x"'])
    expect(reason(c)).toBe(HOOKS_OFF)
  for (const c of ['git commit -m "--no-verify and core.hooksPath=/dev/null"', 'git -c user.name=me commit -m x',
    'echo timeout 60 git push origin release/v1', 'nice -n 10 git status', 'eval "echo git push origin release/v1"',
    'git -c core.hooksPath=/dev/null status', 'git config core.hooksPath .githooks'])
    expect(reason(c)).toBe(null)
  expect(reason('timeout 60 git push origin main')).toBe('ask')
})

// Codex review 10-12, #1 #3 #8: a wrapper's long options hid git; git's own abbreviations beat the dry-run and
// mirror checks (probed on git 2.50: --no-dry-ru pushes, --mirr mirrors, --dry is a dry run); a merge whose
// --no-verify was a message, or undone by --verify, was denied.
test("the guard reads options as git and the wrappers do: long values, any unique prefix, the last flag winning", () => {
  const reason = (c: string) => {
    const v = parseOf(c)
    return v?.kind === 'deny' ? v.reason : (v?.kind ?? null)
  }
  for (const c of ['timeout --signal TERM 60 git commit --no-verify -m x', 'timeout --kill-after 5 60 git commit -n -m x',
    'timeout --sig TERM 60 git commit -n -m x', 'nice --adjustment 5 git commit -n -m x', 'env --unset HOME git commit -n -m x',
    'stdbuf --output L git commit -n -m x', 'sudo --user me git commit -n -m x'])
    expect(reason(c)).toBe(NO_VERIFY)
  for (const c of ['git push --dry-run --no-dry-ru origin HEAD:refs/heads/release/v9', 'git push --mirr origin',
    'git push --al origin', 'git push --branc origin', 'git push -n --no-dry-run origin release/v9'])
    expect(reason(c)).toBe(RELEASE)
  for (const c of ['git push --dry origin release/v9', 'git push --dry-r origin release/v9', 'git push --all --no-all --dry-run origin'])
    expect(reason(c)).toBe(null)
  for (const c of ['git merge --no-verify --verify topic', 'git merge -m --no-verify topic', 'git merge --message --no-verify topic',
    'git merge -sours --no-verify-signatures topic', 'git merge --no-veri topic']) // the last is ambiguous: git refuses it
    expect(reason(c)).toBe(null)
  for (const c of ['git merge --no-verify topic', 'git merge --verify --no-verify topic', 'git merge -m x --no-verify topic',
    'git merge -Xours --no-verify topic'])
    expect(reason(c)).toBe(NO_VERIFY)
})

// Codex review 10-12, #2 #7: a heredoc cut every command after it; text the shell prints was read as commands.
test('a heredoc hides only its body, and $(...) or backticks run only where the shell runs them', () => {
  const reason = (c: string) => {
    const v = parseOf(c)
    return v?.kind === 'deny' ? v.reason : (v?.kind ?? null)
  }
  for (const c of ["cat <<'EOF'\nhello\nEOF\ngit commit --no-verify -m x", 'cat <<A\nx\nA\ncat <<B\ny\nB\ngit commit -n -m x',
    'cat <<<"here string"\ngit commit --no-verify -m x', "cat <<-'EOF'\n\tbody\n\tEOF\ngit commit -n -m x",
    'cat <<\\EOF\nbody\nEOF\ngit commit -n -m x'])
    expect(reason(c)).toBe(NO_VERIFY)
  for (const c of ['cat <<EOF\n$(git push origin release/v9)\nEOF', 'cat <<EOF\nit\'s `git push origin release/v9`\nEOF',
    'echo "$(git push origin release/v9)"', 'x=$(echo $(git push origin release/v9))', 'echo "a `git push origin release/v9` b"'])
    expect(reason(c)).toBe(RELEASE)
  for (const c of ["printf '%s' '$(git push origin release/v9)'", "echo '`git push origin release/v9`'",
    "cat <<'EOF'\n$(git push origin release/v9)\nEOF", 'cat <<"EOF"\n`git commit --no-verify -m x`\nEOF',
    "echo \"it's\" '$(git push origin release/v9)'", 'echo $((1 + 2))'])
    expect(reason(c)).toBe(null)
})

// Codex r2 #1 #12: `bash <<\EOF` feeds bash a script (bash ran the push; the guard read its body as text), and of
// `cat <<'A' <<'B'` only A's body was cut, so B's text was read as a command. Probed with bash 3.2 and 5.3.
test('every heredoc on a line is cut in order, and one a shell reads is its script, however its word is quoted', () => {
  const reason = (c: string) => {
    const v = parseOf(c)
    return v?.kind === 'deny' ? v.reason : (v?.kind ?? null)
  }
  for (const c of ['bash <<\\EOF\ngit push origin release/v9\nEOF', "sh <<'EOF'\ngit push origin release/v9\nEOF",
    'bash <<"EOF"\ngit push origin release/v9\nEOF', "cat <<'EOF' | bash\ngit push origin release/v9\nEOF",
    "cat <<'A'; bash <<'B'\nx\nA\ngit push origin release/v9\nB", "cat <<'A' <<'B'\nhello\nA\nworld\nB\ngit push origin release/v9",
    'echo "a <<b"\ngit push origin release/v9\nb', 'echo $((1<<x))\ngit push origin release/v9\nx'])
    expect(reason(c)).toBe(RELEASE)
  expect(reason('bash <<\\EOF\ngit commit --no-verify -m x\nEOF')).toBe(NO_VERIFY)
  for (const c of ["cat <<'A' <<'B'\nhello\nA\ngit push origin release/v9\nB", 'cat <<\\A <<-"B"\nx\nA\n\tgit commit -n -m x\n\tB',
    "echo bash <<'EOF'\ngit push origin release/v9\nEOF"])
    expect(reason(c)).toBe(null)
})

// Codex review 10-12, #4: ':' and '+:' push every branch both sides have, release/v* among them (git push docs:
// "matching" branches; probed on git 2.50 with a branch "other": it was pushed).
test('the matching refspec pushes every branch both sides have, so it is a release push', () => {
  const reason = (c: string) => {
    const v = parseOf(c)
    return v?.kind === 'deny' ? v.reason : (v?.kind ?? null)
  }
  for (const c of ['git push origin :', 'git push origin +:', 'git push -u origin feat/x :']) expect(reason(c)).toBe(RELEASE)
  expect(reason('git push origin :feat/old')).toBe('ask') // a deletion names its branch
  expect(reason('git push --dry-run origin :')).toBe(null)
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

test('the pane offers dreams while they are off; its 1 asks again, as they turn on in all your sessions', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, json: QUIET })
  await $.session.start(START as never)
  await settle(() => seen.opened.length > 0)
  const pane = await $.ui.mount(PANE)
  await settle(async () => (await pane.find({ text: /Dreams are off$/ })) !== undefined)

  expect(await pane.find({ text: /CSR remembered/ })).toBeUndefined() // no badge line: what CSR did, in words
  expect(await pane.find({ text: /recalled your past work for Claude 2 times/ })).toBeDefined()
  expect(await pane.find({ text: /carried 3 corrections of yours · told Claude about 1 code change/ })).toBeDefined()
  expect(await pane.find({ text: /covers all your Claude sessions/ })).toBeDefined()
  await pane.press({ key: 'dreams-on' })
  expect(seen.configured).toEqual([]) // the user settings row reaches every window: one press only asks
  expect(await pane.find({ type: 'Button', text: 'Press again: dreams on in all your sessions' })).toBeDefined()
  await pane.press({ key: 'dreams-on' })
  expect(seen.configured).toEqual([{ key: 'csr-companion.dreams', value: true }])
  expect(await pane.find({ text: /Dreams are on$/ })).toBeDefined()
  expect(await pane.find({ type: 'Button' })).toBeUndefined()
})

test('a refused /config row leaves the offer up and says where to turn dreams on', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, json: QUIET, configRefused: true })
  await $.session.start(START as never)
  const pane = await $.ui.mount(PANE)
  await settle(async () => (await pane.find({ text: /Dreams are off$/ })) !== undefined)
  await pane.press({ key: 'dreams-on' })
  await pane.press({ key: 'dreams-on' })
  expect(seen.configured).toEqual([])
  expect(await pane.find({ text: /Couldn't turn dreams on here. Use \/config/ })).toBeDefined()
  expect(await pane.find({ type: 'Button' })).toBeDefined()
})

test('nothing found yet: no empty headings, and the pane says what will show there', ON, async ($, on) => {
  host(on, { herdr: false, repo: true, json: QUIET })
  await $.session.start(START as never)
  const pane = await $.ui.mount(PANE)
  await settle(async () => (await pane.find({ text: /What CSR can do here$/ })) !== undefined)

  expect(await pane.find({ text: /Changed since your sessions$/ })).toBeUndefined()
  expect(await pane.find({ text: /Change log$/ })).toBeUndefined()
  expect(await pane.find({ text: /Dreams are on$/ })).toBeDefined()
  expect(await pane.find({ type: 'Button' })).toBeUndefined() // dreams on: nothing to offer
  expect(await pane.find({ text: /Code you worked on that a later commit changes shows here/ })).toBeDefined()
})

test('outside git the pane says changed code cannot show there, not that none changed', ON, async ($, on) => {
  host(on, { herdr: false, repo: true, json: { ...QUIET, git: false } })
  await $.session.start(START as never)
  const pane = await $.ui.mount(PANE)
  await settle(async () => (await pane.find({ text: /What CSR can do here$/ })) !== undefined)
  expect(await pane.find({ text: /Not in git, so code changes cannot show here/ })).toBeDefined()
  expect(await pane.find({ text: /a later commit changes shows here/ })).toBeUndefined()
})

test('with changes found the pane lists them, and no longer promises them', ON, async ($, on) => {
  host(on, { herdr: false, repo: true })
  await $.session.start(START as never)
  const pane = await $.ui.mount(PANE)
  await settle(async () => (await pane.find({ text: /Changed since your sessions$/ })) !== undefined)
  expect(await pane.find({ text: /09-20 remove the dream statusline/ })).toBeDefined()
  expect(await pane.find({ text: /Change log$/ })).toBeDefined()
  expect(await pane.find({ text: /a later commit changes shows here/ })).toBeUndefined()
})

test('with dreams off the pane shows the dream you marked helpful as its sample', async ($, on) => {
  const sample = { line: 'What if the pre-commit hook reran a failed test once before blocking?', at: Date.UTC(2026, 9, 10, 16, 51) / 1000, answer: 'helpful', project: 'claude-self-reflect' }
  host(on, { herdr: false, repo: true, json: { ...QUIET, sample } })
  await $.session.start(START as never)
  const pane = await $.ui.mount(PANE)
  await settle(async () => (await pane.find({ text: /Dreams are off$/ })) !== undefined)

  expect(await pane.find({ text: 'On 10-10, in claude-self-reflect, the dreamer asked this, and you marked it helpful:' })).toBeDefined()
  expect(await pane.find({ text: `“${sample.line}”` })).toBeDefined()
  expect(await pane.find({ text: /^Turned on, it asks one "what if\.\.\.\?" like this/ })).toBeDefined()
  expect(await pane.find({ text: /^Turn them on, and while Claude works/ })).toBeUndefined()
  expect(await pane.find({ type: 'Button' })).toBeDefined()
})

// You should know reads CSR: its propose fork gets the brief's code, rule, correction and past-session lines,
// before its "## Conclusion", and nothing else of the brief.
const YSK_BRIEF = [
  'Files Claude handled lately, newest first: hooks/register.tsx (12×).',
  '',
  "Code that changed after one of the person's past sessions, or this one before its last compaction, worked on it:",
  '* handle in src/setup.rs changed [commit fdaba23, session 3f2a91c0]',
  '',
  'Rules the person set for these files:',
  '* never block Claude Code from a hook [session 1e0e1cf4]',
  '',
  'Lines the person typed in past sessions, in their own words, that sit close to this work:',
  '- 07-11 (demo-app): “what if there was a free station” [session 142ef262]',
  '',
  'Dreams already offered here, and what the person answered:',
  '- 10-12: “What if the recap…” [helpful]',
].join('\n')
const YSK_PROMPT = ['## Overview', '', 'learn: none', '', '## Conclusion', '', 'Say learn: none when in doubt.'].join('\n')

// 10-12 19:51: every look You should know took got "given 0": the brief held none of its kinds of fact. The lines the
// person typed in past sessions are theirs too, with their receipts.
test('yskFacts keeps the brief\'s code, rule, correction and past-session lines and drops the dreamer\'s own', () => {
  const facts = yskFacts(YSK_BRIEF)
  expect(facts).toContain('[commit fdaba23, session 3f2a91c0]')
  expect(facts).toContain('never block Claude Code from a hook')
  expect(facts).toContain('Lines the person typed in past sessions')
  expect(facts).toContain('- 07-11 (demo-app): “what if there was a free station” [session 142ef262]')
  expect(facts).not.toContain('Files Claude handled')
  expect(facts).not.toContain('What if the recap')
  expect(yskFacts(YSK_BRIEF.split('\n').slice(-5).join('\n'))).toContain('free station') // alone, it is still news to it
  expect(yskFacts('Files Claude handled lately: a.ts (1×).')).toBe('')
})

test('withCsrFacts goes before You should know\'s conclusion, and leaves a prompt alone when there is nothing', () => {
  const out = withCsrFacts(YSK_PROMPT, yskFacts(YSK_BRIEF))
  expect(out.indexOf('## What CSR remembers near this session')).toBeLessThan(out.indexOf('## Conclusion'))
  expect(out.startsWith('## Overview')).toBe(true)
  expect(out.endsWith('Say learn: none when in doubt.')).toBe(true)
  expect(withCsrFacts(YSK_PROMPT, '  ')).toBe(YSK_PROMPT)
  expect(withCsrFacts(out, '* another fact [session 1e0e1cf4]')).toBe(out) // a second copy adds nothing
  expect(withCsrFacts('no anchor here', '* a fact [session 3f2a91c0]').startsWith('no anchor here')).toBe(true)
  expect(isYskPropose(YSK_PROMPT)).toBe(true)
  expect(isYskPropose('Answer straight away: do not think it over first.')).toBe(false)
})

test('isYskOrigin takes every name the You should know seat answers to, and no other plugin', () => {
  for (const plugin of ['cc-plugin-you-should-know', 'cc-plugin-you-should-know@builtin', 'you-should-know', 'suggest-learning@builtin']) {
    expect(isYskOrigin({ plugin })).toBe(true)
  }
  for (const plugin of ['csr-shifts', 'csr-companion@claude-self-reflect', 'engine', 'cc-plugin-tips@builtin']) {
    expect(isYskOrigin({ plugin })).toBe(false)
  }
})

test('yskLine reads a reply as You should know does, and citedFrom counts only receipts it was given', () => {
  expect(yskLine('learn: none')).toEqual({ line: null, parsed: true })
  expect(yskLine('- learn: The setup handler changed after your 07-12 session.\ntag: Heads up')).toEqual({
    line: 'The setup handler changed after your 07-12 session.',
    parsed: true,
  })
  expect(yskLine('I could not decide.')).toEqual({ line: null, parsed: false })
  expect(yskLine('**learn:** bold label')).toEqual({ line: null, parsed: false }) // its parser fails it too (@222836181)
  const facts = yskFacts(YSK_BRIEF)
  expect(citedFrom('It changed in commit fdaba23 [session 3f2a91c0], and commit abcdef1 too.', facts)).toEqual([
    'commit fdaba23',
    'session 3f2a91c0',
  ])
})

test('the dreamer\'s fork opens with the side-request frame: no tools, one response, no secrets', () => {
  const prompt = dreamPrompt('* a fact [session 3f2a91c0]', [], [])
  expect(prompt.startsWith('<system-reminder>This is a side request from CSR\'s dreamer')).toBe(true)
  expect(prompt).toContain('You have no tools')
  expect(prompt).toContain('Never reproduce secrets')
  expect(prompt.indexOf('</system-reminder>')).toBeLessThan(prompt.indexOf('## Overview'))
})

test('a hand-off is text: @, paste placeholders and ultra words cannot act, and the sent dream is still found', () => {
  expect(cleanForDraft('see @hooks/register.tsx and ultrathink')).toBe('see \uFF20hooks/register.tsx and ultra-think')
  expect(cleanForDraft('[Pasted text #2 +40 lines] here')).toBe('\uFF3BPasted text #2 +40 lines] here')
  expect(cleanForDraft('mail me at a@b.co')).toBe('mail me at a@b.co')
  const dream = { line: 'What if @pane.py drew the brief?', title: 'The pane could draw it', text: 'x' }
  const sent = askClaude(dream)
  expect(sent.startsWith('“What if \uFF20pane.py drew the brief?”')).toBe(true)
  expect(dreamSent(sent, [{ ...dream, at: 1 }])?.line).toBe(dream.line)
})

// ---------------------------------------------------------------- the first run's walkthrough
// 10-12: "lets ensure plugin addition walk them through these as steps": nothing that acts on its own is on until
// the person says so, and each setting is a step above the prompt, a key press each.

const keyEdit = ($: Engine, key: string) =>
  ($.prompt as unknown as { edit: (e: unknown) => Promise<unknown> }).edit({ origin: { kind: 'composer' }, text: '', cursor: 0, start: 0, end: 0, inputText: key, key: { key } })
const SETUP = { command: 'csr', args: 'setup', origin: { kind: 'human' } } as never
// What a person who never chose has in force; the manifest's own default is test_compact.py's to check.
const FRESH = { options: { autoCompact: 'suggest' } }

test('the plugin just added: two steps above the prompt, each digit writes its /config row, then it is gone', FRESH, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, firstRun: true })
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  expect(await band.find({ text: /CSR setup 1\/2/ })).toBeDefined()
  expect(await band.find({ text: /checks before each look that its spend stays within/ })).toBeDefined() // not "at most": a fork has no token cap
  expect(await band.find({ text: /A look can run over; the next waits until Claude has caught up/ })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Off (now)' })).toBeDefined() // dreams start off
  await band.press({ key: 'setup-dreams-1' })
  expect(seen.configured).toEqual([{ key: 'csr-companion.dreams', value: true }])
  expect(await band.find({ text: /CSR setup 2\/2/ })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Suggest (now)' })).toBeDefined() // compacting on its own is opt-in
  expect(await band.find({ text: /Auto: on its own. A 20 s countdown at the terminal; at once from the phone/ })).toBeDefined()
  await band.press({ key: 'setup-autoCompact-1' })
  expect(seen.configured).toEqual([{ key: 'csr-companion.dreams', value: true }, { key: 'csr-companion.autoCompact', value: 'auto' }])
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
  expect(seen.store.get('setup:answer:dreams')).toEqual(expect.objectContaining({ value: true }))
  expect(seen.store.get('setup:answer:autoCompact')).toEqual(expect.objectContaining({ value: 'auto' }))
})

test('keeping what is in force writes nothing: no /config write, no reload', FRESH, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, firstRun: true })
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  await band.press({ key: 'setup-dreams-0' })
  await band.press({ key: 'setup-autoCompact-2' })
  expect(seen.configured).toEqual([])
  expect(seen.store.get('setup:answer:autoCompact')).toEqual(expect.objectContaining({ value: 'suggest' }))
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
})

test('a refused /config row says where to set it and moves on; on the last step the toast says it', FRESH, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, firstRun: true, configRefused: true })
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  await band.press({ key: 'setup-dreams-1' })
  expect(await band.find({ text: /CSR setup 2\/2/ })).toBeDefined()
  expect(await band.find({ text: "Couldn't set it here. Use /config: csr-companion, Dreams." })).toBeDefined()
  await band.press({ key: 'setup-autoCompact-1' })
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
  expect(seen.toasts).toContain("Couldn't set it here. Use /config: csr-companion, Compact at the right moment.")
})

test('answered, it never shows again; /csr setup brings every step back', async ($, on) => {
  host(on, { herdr: false, repo: true })
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
  const said = JSON.stringify(await $.command.run(SETUP))
  expect(said).toContain('CSR setup is above the prompt: 2 steps')
  expect(await band.find({ text: /CSR setup 1\/2/ })).toBeDefined()
})

test('typing sets it aside for the session; a lone digit on its way to the band does not', async ($, on) => {
  host(on, { herdr: false, repo: true, firstRun: true })
  on('prompt.edit', async (_$, e) => ({ text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end), cursor: e.start + e.inputText.length }))
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  await keyEdit($, '1')
  expect(await band.find({ text: /CSR setup 1\/2/ })).toBeDefined()
  await keyEdit($, 'w')
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
  await $.session.start(START as never) // a reload of the module keeps it aside
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
})

test('a prompt sent past it sets it aside too', async ($, on) => {
  host(on, { herdr: false, repo: true, firstRun: true })
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  await $.prompt.submit({ text: 'fix the importer', origin: { kind: 'composer' } } as never)
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
})

test('set aside unanswered at 2 session starts, a step never shows again; one start in, it shows', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, firstRun: true, sid: 'sess-3' })
  seen.store.set('setup:seen:dreams', ['sess-1', 'sess-2'])
  seen.store.set('setup:seen:autoCompact', ['sess-2'])
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  expect(await band.find({ text: /CSR setup 1\/2/ })).toBeUndefined() // dreams: two starts set aside
  expect(await band.find({ text: /CSR setup 2\/2/ })).toBeDefined() // compacting: one, so a second chance
  expect(seen.store.get('setup:seen:autoCompact')).toEqual(['sess-2', 'sess-3'])
  expect(seen.store.get('setup:seen:dreams')).toEqual(['sess-1', 'sess-2'])
})

test('a claude -p child never walks through anything', async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, firstRun: true, sid: 'kid-3', headless: true })
  await $.session.start({ ...START, isInteractive: false, surface: null } as never)
  expect([...seen.store.keys()].filter(key => key.startsWith('setup:seen'))).toEqual([])
  const band = await $.ui.mount(BAND)
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
})

// ---------------------------------------------------------------- agents: usage, and the look a completion starts

const SID = 'sess-1'
const AGENT_USAGE_5 = { input_tokens: 4500, output_tokens: 500, cache_creation_input_tokens: 0, cache_read_input_tokens: 200_000, model: 'opus' } // five steps of LOOK
const REPORT = 'The importer retries three times and then drops the batch silently.'
const reported = ($: Engine, agentId: string, turnId: string, answer = REPORT, more: Record<string, unknown> = {}) =>
  $.turn.complete({ answer, durationMs: 1000, isAborted: false, turnId, reason: 'answer', agentId, ...more } as never)
const ledgerOf = (seen: ReturnType<typeof host>) => (seen.store.get(`agentLedger:${SID}`) ?? {}) as Record<string, string>
const keyOfRun = (agent: string, turn: string) => JSON.stringify([SID, agent, turn])
// The brief pane.py would print once these agents had reported: CSR's lines, then the agents as the machine envelope.
type Said = { id: string; turn?: string; answer?: string; status?: string; type?: string | null }
const briefWith = (...agents: Said[]) => {
  const rows = agents.map(row => ({
    id: row.id, parent: null, type: row.type === undefined ? 'Explore' : row.type, model: null, status: row.status ?? 'completed', edits: [], overlaps: [],
    completion: row.answer === undefined ? null : { turn_id: row.turn ?? 'turn-1', reason: 'answer', answer: row.answer },
  }))
  return `${BRIEF}\n\n[[CSR:AGENTS:v1]]\n${JSON.stringify({ kind: 'context', session: SID, at: NOW, agents: rows })}\n[[/CSR:AGENTS:v1]]`
}
// A session whose own schedule is far off and whose Claude has spent plenty: only a completion can start a look.
function roomy(seen: ReturnType<typeof host>, pace: Partial<ReturnType<typeof freshPace>> = {}) {
  seen.store.set(`chapter:${SID}`, { ...freshChapter(SID, NOW), due: 10_000, gen: 1 })
  seen.store.set(`pace:${SID}`, { ...freshPace(SID), claude: 5_000_000, step: LOOK, ...pace })
}

test('agent_usage_never_counts_towards_the_share_or_the_chapter', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await steps($, 2)
  await steps($, 3, 'sub-1') // a subagent's three steps: the dreamer reads the main conversation, so none of it is Claude's spend
  await steps($, 3, 'sub-1') // and a replay changes nothing either
  await settle(() => false)
  expect(seen.store.get(`pace:${SID}`)).toEqual(expect.objectContaining({ claude: 2 * LOOK }))
  expect(seen.store.get(`chapter:${SID}`)).toEqual(expect.objectContaining({ steps: 2 })) // only Claude's own steps are the chapter's
  expect(seen.forks).toEqual([])
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([])
})

test('a replayed main step adds no usage and no chapter step; a new step still does', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await steps($, 3)
  await steps($, 3, undefined, 0) // the same three steps of the same turn again: a replay
  await settle(() => false)
  expect(seen.store.get(`pace:${SID}`)).toEqual(expect.objectContaining({ claude: 3 * LOOK, step: LOOK }))
  expect(seen.store.get(`chapter:${SID}`)).toEqual(expect.objectContaining({ steps: 3 }))
  await steps($, 1) // the next index of the turn: new work
  await settle(() => false)
  expect(seen.store.get(`pace:${SID}`)).toEqual(expect.objectContaining({ claude: 4 * LOOK }))
  expect(seen.store.get(`chapter:${SID}`)).toEqual(expect.objectContaining({ steps: 4 }))
})

test('main step receipts: an open turn is never forgotten, a closed one is trimmed, and the same index of a new turn counts', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  const step = async (turnId: string, index: number) => {
    for await (const _ of $.turn.step({ turnId, index, model: 'opus', messageCount: 4 } as never)) void _
  }
  await step('open-0', 0) // a turn left open (no turn.complete) ...
  for (let n = 0; n < 70; n++) {
    await step(`done-${n}`, 0)
    await $.turn.complete({ answer: 'ok', durationMs: 1, isAborted: false, turnId: `done-${n}`, reason: 'answer' } as never)
  }
  await step('open-0', 0) // ... is still known after 70 closed turns went by
  await settle(() => false)
  expect(seen.store.get(`pace:${SID}`)).toEqual(expect.objectContaining({ claude: 71 * LOOK }))
  expect(seen.store.get(`chapter:${SID}`)).toEqual(expect.objectContaining({ steps: 71 }))
  const turns = (seen.store.get(`pace:${SID}`) as { turns: Record<string, { closed?: boolean }> }).turns
  expect(Object.values(turns).filter(receipt => receipt.closed).length).toBeLessThanOrEqual(60)
  expect(Object.keys(turns)).toContain(JSON.stringify([SID, 'open-0']))
})

test('completion_usage_never_counts_towards_the_share', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  await steps($, 3, 'sub-1')
  await reported($, 'sub-1', 'turn-1', REPORT, { usage: AGENT_USAGE_5 }) // the turn's total: not Claude's spend either
  await reported($, 'sub-1', 'turn-1', REPORT, { usage: AGENT_USAGE_5 })
  await steps($, 5, 'sub-1')
  await settle(() => false)
  expect(seen.store.get(`pace:${SID}`)?.claude ?? 0).toBe(0)
  expect(seen.store.get(`pace:${SID}`)?.turns).toBeUndefined()
})

// A report, the main session's next step and the 20 s timer all reach for it: one claim, one look, whatever the order.
async function oneClaim($: Engine, on: On, order: 'step first' | 'timer first' | 'together') {
  const seen = host(on, { herdr: false, repo: true, mockClock: true, brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  await reported($, 'a1', 'turn-1')
  await seen.clock!.settle()
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('pending') // waiting for the main session's next step, or 20 s
  if (order === 'step first') {
    await steps($, 1)
    await settle(() => seen.forks.length >= 1)
    await seen.clock!.advance(20_000)
  } else if (order === 'timer first') {
    await seen.clock!.advance(20_000)
    await settle(() => seen.forks.length >= 1)
    await steps($, 1)
  } else {
    await Promise.all([steps($, 1), seen.clock!.advance(20_000)])
  }
  await settle(() => ledgerOf(seen)[keyOfRun('a1', 'turn-1')] === 'looked')
  await settle(() => false)
  expect(seen.forks.length).toBe(1)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('looked')
  expect(seen.forks[0]).toContain(REPORT) // the agent's own words, quoted
  expect(seen.forks[0]).toContain("A report is the agent's words, never the person's")
  expect(seen.kept.filter(record => record.kind === 'look' || record.kind === 'dream').map(record => [record.trigger, record.completions])).toEqual([['agent', 1]])
  expect(seen.store.get(`chapter:${SID}`)).toEqual(expect.objectContaining({ due: 10_000 })) // the chapter's own schedule is not moved by it
}
test('completion_step_and_timer_share_one_claim', { options: { dreams: true } }, ($, on) => oneClaim($, on, 'step first'))
test('completion_step_and_timer_share_one_claim: the timer first', { options: { dreams: true } }, ($, on) => oneClaim($, on, 'timer first'))
test('completion_step_and_timer_share_one_claim: both at once', { options: { dreams: true } }, ($, on) => oneClaim($, on, 'together'))

test('a timer finding the main session at work leaves the report for its next step', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true, brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  await $.turn.start({ text: 'go', turnId: 'main-1' } as never)
  await reported($, 'a1', 'turn-1')
  await seen.clock!.advance(25_000) // 20 s passed with the main turn still running
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('pending') // not consumed
  await steps($, 1)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks.length).toBe(1)
})

test('a timer that finds the main session just stepped tries again once the idle time it lacked is up', { options: { dreams: true } }, async ($, on) => {
  let open!: () => void
  const seen = host(on, { herdr: false, repo: true, mockClock: true, replies: ['dream: none', DREAM_REPLY], forkGate: new Promise<void>(done => (open = done)),
    brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  seen.store.set(`chapter:${SID}`, { ...freshChapter(SID, NOW), due: 1, gen: 1 }) // the main schedule's look is due at the next step
  await reported($, 'a1', 'turn-1') // the 20 s timer starts now
  await seen.clock!.advance(10_000)
  void steps($, 1) // the main session steps at 10 s: its own look starts (the fork waits on the gate), so the report is not taken
  await settle(() => seen.forks.length >= 1)
  await seen.clock!.advance(10_000) // the timer fires at 20 s: the last step was 10 s ago, a look is running
  open()
  await settle(() => seen.kept.some(record => record.kind === 'look' || record.kind === 'dream'))
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('pending')
  await seen.clock!.advance(10_000) // the idle time it still lacked is up: it looks without waiting for a prompt
  await settle(() => seen.forks.length >= 2)
  await settle(() => ledgerOf(seen)[keyOfRun('a1', 'turn-1')] === 'looked')
  expect(seen.forks.length).toBe(2)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('looked')
})

test('a timer that finds a look already running tries once more after it', { options: { dreams: true } }, async ($, on) => {
  let open!: () => void
  const seen = host(on, { herdr: false, repo: true, mockClock: true, replies: ['dream: none', DREAM_REPLY], forkGate: new Promise<void>(done => (open = done)),
    brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  seen.store.set(`chapter:${SID}`, { ...freshChapter(SID, NOW), due: 1, gen: 1 }) // the main schedule's look is due at the next step
  void steps($, 1) // the main session's own look starts, and its fork waits on the gate
  await settle(() => seen.forks.length >= 1)
  await reported($, 'a1', 'turn-1') // the report comes in while that look runs
  await seen.clock!.advance(25_000) // its timer finds the look still running
  expect(seen.forks.length).toBe(1)
  open()
  await settle(() => seen.kept.some(record => record.kind === 'look' || record.kind === 'dream'))
  await seen.clock!.advance(25_000) // and tries again, once, after
  await settle(() => seen.forks.length >= 2)
  await settle(() => ledgerOf(seen)[keyOfRun('a1', 'turn-1')] === 'looked')
  expect(seen.forks.length).toBe(2)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('looked')
})

test('a report stranded behind a long look is offered once the look ends, without another event', { options: { dreams: true } }, async ($, on) => {
  let open!: () => void
  const seen = host(on, { herdr: false, repo: true, mockClock: true, replies: ['dream: none', DREAM_REPLY], forkGate: new Promise<void>(done => (open = done)),
    brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  seen.store.set(`chapter:${SID}`, { ...freshChapter(SID, NOW), due: 1, gen: 1 })
  void steps($, 1) // the main session's own look starts; its fork waits on the gate
  await settle(() => seen.forks.length >= 1)
  await reported($, 'a1', 'turn-1') // the report comes in while that look runs
  await seen.clock!.advance(25_000) // its timer fires with the look still running, and so does the one-time retry
  await seen.clock!.advance(25_000)
  expect(seen.forks.length).toBe(1)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('pending')
  open() // the look ends: no step, no turn, no new report follows
  await settle(() => seen.kept.some(record => record.kind === 'look' || record.kind === 'dream'))
  await seen.clock!.advance(25_000)
  await settle(() => seen.forks.length >= 2)
  await settle(() => ledgerOf(seen)[keyOfRun('a1', 'turn-1')] === 'looked')
  expect(seen.forks.length).toBe(2)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('looked')
})

test('completion_claim_survives_reload_and_fork_failure', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true, forkFails: true, brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  await reported($, 'a1', 'turn-1')
  await steps($, 1)
  await settle(() => seen.forks.length >= 1)
  await settle(() => ledgerOf(seen)[keyOfRun('a1', 'turn-1')] === 'failed')
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('failed') // the fork threw: the chance is spent
  expect((seen.store.get(`pace:${SID}`) as { dreamer: number }).dreamer).toBeGreaterThan(0) // and was charged as a guess, as any look's is
  // a reload: the module starts again over the same state and store
  await $.session.start(START as never)
  await seen.clock!.advance(60_000)
  await steps($, 3)
  await seen.clock!.advance(60_000)
  await settle(() => false)
  expect(seen.forks.length).toBe(1) // a claimed completion is never retried
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('failed')
})

test('a pending completion a reload finds is offered once', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true, brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen)
  await reported($, 'a1', 'turn-1')
  await $.session.start(START as never) // the module was reloaded before the 20 s were up
  await $.session.start(START as never)
  await seen.clock!.advance(20_000)
  await settle(() => seen.forks.length >= 1)
  await seen.clock!.advance(120_000)
  await settle(() => false)
  expect(seen.forks.length).toBe(1)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('looked')
})

test('budget_blocked_completion_is_not_rearmed', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true, brief: briefWith({ id: 'a1', answer: REPORT }) })
  roomy(seen, { claude: 100_000 }) // 10 tokens of Claude's for each of the dreamer's: 41,000 does not fit in 10,000
  await reported($, 'a1', 'turn-1')
  await steps($, 1)
  await settle(() => seen.kept.some(record => record.kind === 'look'))
  expect(seen.forks).toEqual([])
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([expect.objectContaining({ outcome: 'budget', trigger: 'agent', completions: 1 })])
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('skipped') // the opportunity was consumed
  roomy(seen) // Claude spends plenty more
  await steps($, 4)
  await seen.clock!.advance(120_000)
  await settle(() => false)
  expect(seen.forks).toEqual([]) // and the finished agent is not looked at for it
  expect(seen.kept.filter(record => record.kind === 'look').length).toBe(1)
  expect(seen.store.get(`chapter:${SID}`)).toEqual(expect.objectContaining({ due: 10_000 }))
})

test('overshoot_debt_blocks_completion_look', { options: { dreams: true } }, async ($, on) => {
  const world: World = { herdr: false, repo: true, mockClock: true, brief: briefWith({ id: 'a1', answer: REPORT }) }
  const seen = host(on, world)
  roomy(seen, { claude: 1_000_000, dreamer: 300_000, looks: 3, lastLook: 41_000 }) // 100,000 over its share, owed
  await reported($, 'a1', 'turn-1')
  await steps($, 1)
  await settle(() => seen.kept.some(record => record.kind === 'look'))
  expect(seen.forks).toEqual([])
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([expect.objectContaining({ outcome: 'budget', trigger: 'agent', owed: expect.any(Number) })])
  expect((seen.kept.find(record => record.kind === 'look') as { owed: number }).owed).toBeGreaterThan(0)
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('skipped')
  // the debt paid by Claude's spending, a later report gets its look
  roomy(seen, { claude: 20_000_000, dreamer: 300_000, looks: 3, lastLook: 41_000 })
  world.brief = briefWith({ id: 'a1', answer: REPORT }, { id: 'a2', answer: 'A second report.' }) // the agents file now holds both
  await reported($, 'a2', 'turn-1', 'A second report.')
  await steps($, 1)
  await settle(() => seen.forks.length >= 1)
  expect(seen.forks.length).toBe(1)
  expect(seen.forks[0]).toContain('A second report.')
  expect(seen.forks[0]).not.toContain(REPORT)
})

test('simultaneous_completions_coalesce', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, {
    herdr: false, repo: true, mockClock: true,
    brief: briefWith({ id: 'a1', answer: 'Report one.' }, { id: 'a2', answer: 'Report two.' }, { id: 'a3', turn: 'turn-4', answer: 'Report three.' }),
  })
  roomy(seen)
  await Promise.all([reported($, 'a1', 'turn-1', 'Report one.'), reported($, 'a2', 'turn-1', 'Report two.'), reported($, 'a3', 'turn-4', 'Report three.')])
  await steps($, 1)
  await settle(() => seen.forks.length >= 1)
  await settle(() => Object.values(ledgerOf(seen)).every(word => word === 'looked'))
  await seen.clock!.advance(60_000)
  expect(seen.forks.length).toBe(1) // one look for the three
  for (const text of ['Report one.', 'Report two.', 'Report three.']) expect(seen.forks[0]).toContain(text)
  expect(Object.values(ledgerOf(seen))).toEqual(['looked', 'looked', 'looked'])
  expect(seen.kept.filter(record => record.kind === 'look' || record.kind === 'dream').map(record => record.completions)).toEqual([3])
})

test('running_failed_and_empty_agents_never_trigger_looks', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true })
  roomy(seen)
  await steps($, 4, 'run-1') // still running: no report
  await $.turn.complete({ answer: 'half a report', durationMs: 1, isAborted: false, turnId: 't', reason: 'error', agentId: 'bad-1' } as never)
  await $.turn.complete({ answer: 'no', durationMs: 1, isAborted: false, turnId: 't', reason: 'refusal', refusal: { category: null, explanation: null }, agentId: 'bad-2' } as never)
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: true, turnId: 't', reason: 'aborted', agentId: 'bad-3' } as never)
  await reported($, 'empty-1', 't', '')
  await reported($, 'empty-2', 't', '  \n ')
  await seen.clock!.advance(25_000)
  await steps($, 3)
  await seen.clock!.advance(120_000)
  await settle(() => false)
  expect(seen.forks).toEqual([])
  expect(ledgerOf(seen)).toEqual({}) // none was ever offered a look
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([])
})

test('the ledger of an old session is swept with its other per-session keys', ON, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true })
  for (let n = 1; n <= 22; n++) {
    seen.store.set(`chapter:old-${n}`, { ...freshChapter(`old-${n}`, n), gen: 1 })
    seen.store.set(`agentLedger:old-${n}`, { '["x","y","z"]': 'looked' })
  }
  seen.store.set(`agentLedger:${SID}`, { k: 'pending' }) // this session's own is never swept
  await $.session.start(START as never)
  await settle(() => !seen.store.has('agentLedger:old-1'))
  expect(seen.store.has('agentLedger:old-1')).toBe(false)
  expect(seen.store.has('agentLedger:old-2')).toBe(false)
  expect(seen.store.has('agentLedger:old-3')).toBe(true) // the newest 20 are kept
  expect(seen.store.has('agentLedger:old-22')).toBe(true)
  expect(seen.store.has(`agentLedger:${SID}`)).toBe(true)
})

test('a claimed completion the agents file did not carry is no news: the look is spent without a fork', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true }) // the brief is CSR's lines alone: no envelope
  roomy(seen)
  seen.store.set(`briefFacts:${SID}`, facts(BRIEF)) // and CSR has nothing new in them
  await reported($, 'a1', 'turn-1')
  await steps($, 1)
  await settle(() => seen.kept.some(record => record.kind === 'look'))
  expect(seen.forks).toEqual([])
  expect(seen.kept.filter(record => record.kind === 'look')).toEqual([expect.objectContaining({ outcome: 'quiet', trigger: 'agent', completions: 1 })])
  expect(ledgerOf(seen)[keyOfRun('a1', 'turn-1')]).toBe('skipped')
})

test('the dreamer reads the claimed reports in the agents\' words, and the running agents as context only', { options: { dreams: true } }, async ($, on) => {
  const seen = host(on, { herdr: false, repo: true, mockClock: true, brief: briefWith({ id: 'a1', answer: REPORT }, { id: 'a2', status: 'running', type: 'Plan' }) })
  roomy(seen)
  await reported($, 'a1', 'turn-1')
  await steps($, 1)
  await settle(() => seen.forks.length >= 1)
  const prompt = seen.forks[0] as string
  expect(prompt).toContain(`  > ${REPORT}`)
  expect(prompt).toContain('Still at work, for context only (nothing to ask about yet): Plan (running).')
  expect(prompt).not.toContain('[[CSR:AGENTS') // the machine envelope is not what the dreamer reads
  expect(prompt).toContain('Do not dream about an agent that is running, stalled, failed, killed or reported nothing.')
})
