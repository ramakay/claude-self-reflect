// `claude plugin test` cases for the generated relay (hooks/register.js), run by
// csr-engine/tests/companion.rs beside a fresh emit. The engine is a script here: `$.http.fetch`
// answers from `engine(path, body)` and `$.process.spawn` is a child the test controls.
import { expect, mock, test } from 'claude-code/testing'
import type { MockClock } from 'claude-code/testing'
import type { On } from 'claude-code'

type Body = Record<string, any>
type Engine = (path: string, body: Body) => Body | undefined | Promise<Body | undefined>
type Child = (spawned: number, clock: MockClock) => AsyncGenerator<{ stream: 'stdout' | 'stderr'; text: string }, { code: number | null; signal: string | null }>

const SID = 'sess-12345678'
const START = { cwd: '/repo', surface: 'terminal' as const, isInteractive: true, startedAt: 0 }
const BAND = {
  plugin: 'csr-companion',
  surface: 'terminal' as const,
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 24, bodyColumns: 100, scroll: { offset: 0, bodyRows: 24 } } as never,
}
const READY = { stream: 'stdout' as const, text: 'ready\n' }
const alive: Child = async function* () {
  yield READY
  await new Promise<void>(() => {})
  return { code: 0, signal: null }
}

// The world beneath the relay: one scripted engine, every `$` call it makes recorded.
function world(on: On, engine: Engine, child: Child = alive, options: { spawnFails?: boolean; fetchFails?: boolean } = {}) {
  const clock = mock.clock(on, { now: 1_000_000 })
  const posts: Array<{ path: string; body: Body; socket: string | undefined }> = []
  const spawned: string[][] = []
  const seen = { toasts: [] as string[], filled: [] as string[], submitted: [] as string[], usage: 0, tools: 0 }
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('session.id', async () => ({ value: SID }))
  on('session.cwd', async () => ({ value: '/repo' }))
  on('http.fetch', async (_$, e) => {
    const init = (e.init ?? {}) as Body
    const path = e.url.replace('http://csr', '')
    const body = JSON.parse(init.body)
    posts.push({ path, body, socket: init.socketPath })
    if (options.fetchFails) return { deny: 'connect ECONNREFUSED' } as never
    const reply = await engine(path, body)
    return { value: { status: 200, ok: true, headers: {}, text: JSON.stringify(reply ?? {}) } }
  })
  on('process.spawn', async function* (_$, e) {
    spawned.push([...e.argv])
    if (options.spawnFails) throw new Error('ENOENT: no such file or directory')
    return { value: yield* child(spawned.length, clock) } as never
  })
  on('ui.toast', async (_$, e) => {
    seen.toasts.push((e as any).text)
    return { value: undefined }
  })
  on('prompt.fill', async (_$, e) => {
    seen.filled.push(e.text)
    return { isFilled: true }
  })
  on('session.usage', async () => {
    seen.usage++
    return { value: { context: { window: 1000 } } } as never
  })
  on('prompt.submit', async (_$, e) => {
    seen.submitted.push(e.text)
    return { text: e.text } as never
  })
  on('tool.call', async () => {
    seen.tools++
    return { result: 'file text' } as never
  })
  on('ui.render', async ($, e) => {
    const { Box, Text } = $.ui.resolve(e)
    return h(Box, {}, h(Text, {}, 'beneath text')) as never
  })
  return { clock, posts, spawned, seen }
}

const only = (posts: Array<{ path: string; body: Body }>, path: string) => posts.filter(p => p.path === path)

test('start spawns the engine on a private socket and posts the start event', async ($, on) => {
  const seen = world(on, () => ({}))
  await $.session.start(START as never)
  expect(seen.spawned).toHaveLength(1)
  expect(seen.spawned[0]?.slice(1, 3)).toEqual(['companion', 'serve'])
  const start = only(seen.posts, '/event')[0]
  expect(start?.body).toMatchObject({ k: 'start', sid: SID, cwd: '/repo', e: { isInteractive: true } })
  expect(start?.socket).toMatch(/\/sess-123-[0-9a-z]+\.sock$/)
  expect(seen.spawned[0]).toContain(start?.socket as string)
})

test('a gate that answers stops the call: the deny comes back and next never runs', async ($, on) => {
  const seen = world(on, (path, body) => (path === '/event' && body.k === 'guard' ? { answer: { deny: 'not on main' } } : {}))
  await $.session.start(START as never)
  const result = await $.tool.call({ tool: 'Bash', command: 'git push origin main' } as never)
  expect(JSON.stringify(result)).toContain('not on main')
  expect(seen.seen.tools).toBe(0)
})

test('a gate that rewrites calls next with the rewrite, then tells the engine what came back', async ($, on) => {
  const seen = world(on, (path, body) => {
    if (path === '/event' && body.k === 'digit') return { id: 'g1', next: { ...body.e, text: 'rewritten' } }
    if (path === '/after' && body.k === 'digit') return { result: { text: 'replaced' } }
    return {}
  })
  await $.session.start(START as never)
  const result = await $.prompt.submit({ text: 'original', origin: { kind: 'human' } } as never)
  expect(seen.seen.submitted).toEqual(['rewritten'])
  const after = only(seen.posts, '/after').find(p => p.body.k === 'digit')
  expect(after?.body).toMatchObject({ id: 'g1', result: { text: 'rewritten' } })
  expect(JSON.stringify(result)).toContain('replaced')
})

test('a reply with calls is run and answered on /step, round after round, until one has none', async ($, on) => {
  const rounds: Body[] = []
  const seen = world(on, (path, body) => {
    if (path === '/event' && body.k === 'start') {
      return { id: 'e1', calls: [{ id: 'c1', m: 'session.usage', a: [] }, { id: 'c2', m: 'no.such', a: [] }] }
    }
    if (path === '/step') {
      rounds.push(body)
      return rounds.length === 1 ? { id: 'e1', calls: [{ id: 'c3', m: 'ui.toast', a: ['hello'] }] } : {}
    }
    return {}
  })
  await $.session.start(START as never)
  expect(seen.seen.usage).toBe(1)
  expect(seen.seen.toasts).toEqual(['hello'])
  expect(rounds).toHaveLength(2)
  expect(rounds[0]).toMatchObject({ id: 'e1', results: [{ id: 'c1', v: { context: { window: 1000 } } }, { id: 'c2' }] })
  expect(rounds[0]?.results[1].err).toContain('unknown call no.such')
  expect(rounds[1]).toMatchObject({ id: 'e1', results: [{ id: 'c3' }] })
})

test('a line the engine pushes on stdout is run as calls and answered on /step', async ($, on) => {
  const child: Child = async function* () {
    yield READY
    yield { stream: 'stdout', text: '{"id":"p1","calls":[{"id":"c1","m":"ui.toast","a":["pu' }
    yield { stream: 'stdout', text: 'shed"]}]}\nnot json\n' }
    await new Promise<void>(() => {})
    return { code: 0, signal: null }
  }
  const seen = world(on, () => ({}), child)
  await $.session.start(START as never)
  await seen.clock.settle()
  expect(seen.seen.toasts).toEqual(['pushed'])
  expect(only(seen.posts, '/step')[0]?.body).toMatchObject({ id: 'p1', results: [{ id: 'c1' }] })
})

test('a render reply is built into elements around the tree beneath, and a press is posted back', async ($, on) => {
  const tree = {
    type: 'Box',
    props: { flexDirection: 'column' },
    children: [{ type: 'Text', props: {}, children: ['from the engine'] }, { type: 'Button', props: { key: 'go', hotkey: '1', plain: true, label: 'Go' }, children: [] }, '$beneath'],
  }
  const seen = world(on, (path, body) => {
    if (path === '/event' && body.k === 'band') return { tree }
    if (path === '/press') return { id: 'pr1', calls: [{ id: 'c1', m: 'prompt.fill', a: [{ text: `pressed ${body.key}` }] }] }
    return {}
  })
  await $.session.start(START as never)
  const band = await $.ui.mount(BAND)
  await seen.clock.settle()
  expect(await band.find({ type: 'Text', text: 'from the engine' })).toBeDefined()
  expect(await band.find({ type: 'Text', text: 'beneath text' })).toBeDefined() // '$beneath' is the original tree
  const asked = only(seen.posts, '/event').find(p => p.body.k === 'band')
  expect(asked?.body.e.component).toBe('AbovePrompt')
  expect(asked?.body.e.beneath).not.toBeNull()
  await band.press({ key: 'go' })
  expect(seen.seen.filled).toEqual(['pressed go'])
  expect(only(seen.posts, '/press')[0]?.body).toMatchObject({ key: 'go' })
})

test('a child that ends is started again after 2 s, on a new socket, and told the start event again', async ($, on) => {
  const child: Child = async function* (n) {
    yield READY
    if (n === 1) return { code: 1, signal: null }
    await new Promise<void>(() => {})
    return { code: 0, signal: null }
  }
  const seen = world(on, () => ({}), child)
  await $.session.start(START as never)
  expect(seen.spawned).toHaveLength(1)
  await seen.clock.advance(2100)
  expect(seen.spawned).toHaveLength(2)
  const sockets = seen.spawned.map(argv => argv[argv.indexOf('--sock') + 1])
  expect(sockets[0]).not.toBe(sockets[1])
  const starts = only(seen.posts, '/event').filter(p => p.body.k === 'start')
  expect(starts).toHaveLength(2)
  expect(starts[0]?.body.respawn).toBeUndefined()
  expect(starts[1]?.body).toMatchObject({ respawn: true, e: { isInteractive: true } })
  expect(starts[1]?.socket).toBe(sockets[1])
})

test('a child that never gets ready is tried 3 times, then one toast says it keeps stopping', async ($, on) => {
  const dies: Child = async function* () {
    return { code: 1, signal: null }
  }
  const seen = world(on, () => ({}), dies)
  await $.session.start(START as never)
  await seen.clock.advance(60_000)
  expect(seen.spawned).toHaveLength(3)
  expect(seen.seen.toasts).toEqual(['CSR engine keeps stopping: run csr-engine setup'])
})

test('an engine that cannot start is tried 3 times, then one toast says it is missing', async ($, on) => {
  const seen = world(on, () => ({}), alive, { spawnFails: true })
  await $.session.start(START as never)
  await seen.clock.advance(60_000)
  expect(seen.spawned).toHaveLength(3)
  expect(seen.seen.toasts).toEqual(['CSR engine missing: npx claude-self-reflect setup'])
})

test('a child that stays up a minute clears the count, so later ends respawn again', async ($, on) => {
  // 1 s, then a minute, then 1 s each: the third short life only comes after the reset.
  const child: Child = async function* (n, clock) {
    yield READY
    await clock.sleep(n === 2 ? 61_000 : 1000)
    return { code: 1, signal: null }
  }
  const seen = world(on, () => ({}), child)
  await $.session.start(START as never)
  await seen.clock.advance(300_000)
  expect(seen.spawned).toHaveLength(5)
  expect(seen.seen.toasts).toEqual(['CSR engine keeps stopping: run csr-engine setup'])
})

test('with the engine unreachable every hook lets the call through', async ($, on) => {
  const seen = world(on, () => ({}), alive, { fetchFails: true })
  await $.session.start(START as never)
  const result = await $.tool.call({ tool: 'Bash', command: 'ls' } as never)
  expect(seen.seen.tools).toBe(1)
  expect(JSON.stringify(result)).toContain('file text')
})

test('a conversation too big for one body is left out of the post, and a small one is sent whole', async ($, on) => {
  const seen = world(on, () => ({}))
  const compacted: Array<{ messages?: unknown[] }> = []
  on('session.compact', async (_$, e) => {
    compacted.push(e as never)
    return { messages: [{ role: 'user', text: 'kept', toolUses: [] }] } as never
  })
  await $.session.start(START as never)
  const small = [{ role: 'user', text: 'one', toolUses: [] }]
  const big = [{ role: 'user', text: 'x'.repeat(4_000_000), toolUses: [] }]
  await $.session.compact({ trigger: 'auto', agentId: 'sub-1', messages: small } as never)
  await $.session.compact({ trigger: 'auto', agentId: 'sub-1', messages: big } as never)
  const asked = only(seen.posts, '/event').filter(p => p.body.k === 'keep')
  expect(asked).toHaveLength(2)
  expect(asked[0]?.body.e.messages).toEqual(small)
  expect(asked[0]?.body.e.messagesOmitted).toBeUndefined()
  expect(asked[1]?.body.e.messages).toBeUndefined()
  expect(asked[1]?.body.e.messagesOmitted).toBe(true)
  expect(asked[1]?.body.e.agentId).toBe('sub-1')
  // the compaction itself still gets its conversation, and is not told it was left out
  expect(compacted).toHaveLength(2)
  expect(compacted[1]?.messages).toHaveLength(1)
  expect('messagesOmitted' in (compacted[1] ?? {})).toBe(false)
})

test('a gated call that fails is still told to the engine, as a deny, and then fails as before', async ($, on) => {
  const seen = world(on, () => ({}))
  on('session.compact', async () => {
    throw new Error('summary failed')
  })
  await $.session.start(START as never)
  const failed = await $.session.compact({ trigger: 'auto', messages: [{ role: 'user', text: 'one', toolUses: [] }] } as never).then(() => false, () => true)
  expect(failed).toBe(true)
  const after = only(seen.posts, '/after').find(p => p.body.k === 'marker')
  expect(typeof after?.body.result.deny).toBe('string')
})
