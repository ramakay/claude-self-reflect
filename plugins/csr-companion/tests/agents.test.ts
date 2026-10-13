import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'
import type { AgentState } from '../types'
import {
  admitCompletion, claimCompletions, completionKey, emptyAgentState, fingerprint, isStalled, LEDGER_MAX, markDisposition, normalizePath,
  FILE_BYTES, FILE_EVERY_MS, pendingCompletions, planWrite, projectAgentsFile, reconcileAgentList, reduceAgentEvent, redactAgentValue,
  settleCompletions, stepKey, STALL_MS, utf8Length,
} from '../hooks/agents'
import { completionNews, ENVELOPE_CLOSE, ENVELOPE_OPEN, HANDOFF_INSTRUCTION, parseAgentEnvelope, parseAgentEnvelopes, stripEnvelopes } from '../hooks/agents'
import type { Ledger } from '../hooks/agents'
import { ownWords } from '../hooks/dreamer'

// tests/redaction_table.json, the table bin/private.py is held to (a hooks test cannot import a .json; test_agents.py holds this
// copy to the file, and runs the same table through hooks/agents.ts under node).
const table: Array<[string, string]> = [
  ["use sk-ant-api03-AbCdEf0123456789_-xyz here", "use [redacted] here"],
  ["OPENAI sk-proj-0123456789abcdefghij0123", "OPENAI [redacted]"],
  ["xai-0123456789abcdefghijKLMN", "[redacted]"],
  ["token ghp_0123456789abcdefghijABCDEFGHIJ0123 ok", "token [redacted] ok"],
  ["github_pat_11ABCDEFG0123456789_abcdefghijklmnop", "[redacted]"],
  ["id AKIAIOSFODNN7EXAMPLE and ASIAIOSFODNN7EXAMPLE", "id [redacted] and [redacted]"],
  ["xoxb-1234567890-abcdefghij", "[redacted]"],
  ["curl -H \"Authorization: Bearer abc123.def456-ghi789\" x", "curl -H \"Authorization: Bearer [redacted]\" x"],
  ["authorization: basic dXNlcjpwYXNzd29yZA==", "authorization: basic [redacted]"],
  ["password=hunter2 next", "password=[redacted] next"],
  ["DB_PASSWORD: \"correct horse battery\"", "DB_PASSWORD: \"[redacted]\""],
  ["API_KEY='abc 123'", "API_KEY='[redacted]'"],
  ["{\"token\": \"abc.def\", \"ok\": 1}", "{\"token\": \"[redacted]\", \"ok\": 1}"],
  ["?access_token=abc123&x=1", "?access_token=[redacted]&x=1"],
  ["-----BEGIN RSA PRIVATE KEY-----\nMIIEow\nabc\n-----END RSA PRIVATE KEY----- after", "[redacted] after"],
  ["-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk", "[redacted]"],
  ["eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpM", "[redacted]"],
  ["the token budget is 10% of Claude tokens: 120k", "the token budget is 10% of Claude tokens: 120k"],
  ["sk-short and ask-me-anything and task-12345678901234567890", "sk-short and ask-me-anything and task-12345678901234567890"],
  ["git push origin feat/plugin", "git push origin feat/plugin"],
  ["Basic authentication works", "Basic authentication works"],
  ["Bearer authentication is used here", "Bearer authentication is used here"],
  ["a cancellation token: stop work", "a cancellation token: stop work"],
  ["token: string", "token: string"],
  ["token: string | null;", "token: string | null;"],
  ["password: required", "password: required"],
  ["const secret: Secret<Key> = load()", "const secret: Secret<Key> = load()"],
  ["password: s3cret-swordfish!", "password: [redacted]"],
  ["password=\"correct horse battery\" now", "password=\"[redacted]\" now"],
  ["secret: 'plain words in quotes'", "secret: '[redacted]'"],
  ["send Bearer abc123def456 here", "send Bearer [redacted] here"],
  ["Authorization: Bearer abcdefghij", "Authorization: Bearer [redacted]"],
  ["Authorization: Basic dXNlcjpwYXNzd29yZA== ok", "Authorization: Basic [redacted] ok"],
  ["export TOKEN=abc123", "export TOKEN=[redacted]"],
  ["wrapped status token=abc123 done", "wrapped status token=[redacted] done"]
]

// The test runner has timers; the hooks environment's typings do not declare them.
declare const setTimeout: (fn: () => void, ms: number) => unknown

const T0 = Date.UTC(2026, 9, 13, 12)
const SESSION = 'sess-a'
const USAGE = { input_tokens: 10, output_tokens: 5, cache_creation_input_tokens: 1, cache_read_input_tokens: 100, model: 'opus' }

type World = {
  list?: Array<{ id: string; parentId?: string; type: string; description: string; status: string }>
  spawn?: (e: { tool_use_id: string }) => Promise<unknown> | unknown // what a spawn comes to; the agent a1 on opus where left out
  toolRuns?: { count: number } // how many times the tool itself ran
  breakId?: { on: boolean } // the session id cannot be read: every observer fails
  writes?: Array<{ path: string; text: string; flags: string[] }> // what bin/private.py was asked to keep
  session?: string // the session id; sess-a where left out
}

// A session with a person at it, the agents' tools beneath the plugin, and the clock and store in memory.
function host(on: On, world: World = {}) {
  const clock = mock.clock(on, { now: T0 })
  mock.store(on)
  written = undefined
  on('state.set', async (_$, e, next) => {
    if (e.key === 'agentSession') written = e.value as AgentState
    return next(e)
  })
  on('session.id', async () => {
    if (world.breakId?.on) throw new Error('the session id is gone')
    return { value: world.session ?? SESSION }
  })
  on('session.cwd', async () => ({ value: '/repo' }))
  on('session.surfaces', async () => ({ value: ['terminal'] }) as never)
  on('agent.list', async () => ({ value: world.list ?? [] }) as never)
  on('agent.spawn', async (_$, e) => (world.spawn ? await world.spawn(e) : { model: 'claude-opus-5', agentId: 'a1' }) as never)
  on('tool.call', async () => {
    if (world.toolRuns) world.toolRuns.count++
    return { result: 'ok', text: 'Saved' } as never
  })
  on('env.get', async (_$, e) => ({ value: e.name === 'HOME' ? '/home/u' : undefined }))
  on('process.run', async (_$, e) => {
    if (e.argv[0] === 'python3' && (e.argv[2] ?? '').endsWith('/bin/private.py')) { // private.py write PATH [--redact-json], text on stdin
      world.writes?.push({ path: e.argv[4] as string, text: e.init?.stdin ?? '', flags: e.argv.slice(5) })
    }
    return { value: { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('turn.step', async function* (_$, e) {
    return { turnId: e.turnId, index: e.index, answer: 'Looking at the files', toolUses: [], stopReason: 'end_turn', usage: USAGE } as never
  })
  on('turn.complete', async (_$, e) => ({ text: e.answer }))
  on('turn.start', async (_$, e) => ({ text: e.text }) as never)
  return clock
}

// The agents state as the plugin last wrote it to the host (the test's own hook sees each write on its way to the kit).
let written: AgentState | undefined
const stateOf = async (_$: Engine): Promise<AgentState> => written ?? emptyAgentState('')

async function settle(done: () => boolean | Promise<boolean>) {
  for (let i = 0; i < 200 && !(await done()); i++) await new Promise<void>(resolve => setTimeout(() => resolve(), 5))
}

const spawn = ($: Engine, id: string, extra: Record<string, unknown> = {}) =>
  $.agent.spawn({ tool_use_id: id, prompt: 'Do it.', description: 'Scan the repo', subagentType: 'Explore', model: 'haiku', background: true, fork: false, ...extra } as never)
async function step($: Engine, agentId: string | undefined, turnId: string, index: number) {
  for await (const _ of $.turn.step({ turnId, index, model: 'opus', messageCount: 4, agentId } as never)) void _
}
const finish = ($: Engine, agentId: string, turnId: string, answer = 'Found it.', reason = 'answer') =>
  $.turn.complete({ answer, durationMs: 1000, isAborted: reason === 'aborted', turnId, reason, agentId } as never)
const edit = ($: Engine, agentId: string | undefined, id: string, file = '/repo/src/a.ts') =>
  $.tool.call({ tool: 'Edit', file_path: file, old_string: 'a', new_string: 'b', tool_use_id: id, agentId } as never)

test('spawn_result_supplies_id_and_model', async ($, on) => {
  host(on)
  const result = await spawn($, 'tu-1')
  expect(result).toEqual({ model: 'claude-opus-5', agentId: 'a1' })
  const row = (await stateOf($)).rows.a1!
  expect(row.model).toBe('claude-opus-5') // the resolved model, not the requested "haiku"
  expect(row.type).toBe('Explore')
  expect(row.description).toBe('Scan the repo')
  expect(row.parent).toBe(null)
  expect(row.started).toBe(T0)
  expect(row.status).toBe('running')
  expect((await stateOf($)).bound['tu-1']).toBe('a1')
})

test('a spawn that started nothing makes no row', async ($, on) => {
  host(on, { spawn: () => ({ deny: 'the person said no' }) })
  expect(await spawn($, 'tu-1')).toEqual({ deny: 'the person said no' })
  const state = await stateOf($)
  expect(Object.keys(state.rows)).toEqual([])
  expect(state.spawns).toEqual({})
})

test('events_before_spawn_merge_without_reset', async ($, on) => {
  let release = () => {}
  const gate = new Promise<void>(resolve => (release = resolve))
  host(on, { spawn: async () => { await gate; return { model: 'claude-opus-5', agentId: 'a1' } } })
  const spawning = spawn($, 'tu-1')
  await settle(async () => 'tu-1' in (await stateOf($)).spawns) // recorded before `next` answered
  await edit($, 'a1', 'tool-1')
  await step($, 'a1', 'turn-1', 0)
  await finish($, 'a1', 'turn-1', 'The report.')
  expect(await stateOf($).then(state => state.rows.a1?.completion?.disposition)).toBe('pending')
  release()
  await spawning
  const state = await stateOf($)
  const row = state.rows.a1!
  expect(row.tools).toBe(1)
  expect(row.tokens).toBe(116)
  expect(Object.values(row.paths)).toEqual(['/repo/src/a.ts'])
  expect(row.status).toBe('completed') // the spawn result does not make it run again
  expect(row.completion?.answer).toBe('The report.')
  expect(row.model).toBe('claude-opus-5')
  expect(row.type).toBe('Explore')
  expect(row.started).toBe(T0)
  expect(Object.keys(state.pending)).toEqual([completionKey(SESSION, 'a1', 'turn-1')])
})

test('nested_parent_tree_uses_parentId', async ($, on) => {
  host(on, { spawn: e => ({ model: 'm', agentId: e.tool_use_id === 'tu-1' ? 'a1' : 'a2' }) })
  await spawn($, 'tu-1')
  await spawn($, 'tu-2', { parentAgentId: 'a1', description: 'Child' })
  const rows = (await stateOf($)).rows
  expect(rows.a1!.parent).toBe(null)
  expect(rows.a2!.parent).toBe('a1')
  expect(rows.a2!.description).toBe('Child')
})

test('list_disappearance_is_not_completion', async ($, on) => {
  const world: World = { list: [{ id: 'a1', type: 'Explore', description: 'Scan', status: 'running' }] }
  host(on, world)
  await spawn($, 'tu-1')
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: false, turnId: 'main-1', reason: 'answer' } as never) // a main turn ends: the list is read
  await settle(async () => (await stateOf($)).rows.a1?.listed === true)
  expect((await stateOf($)).rows.a1!.listed).toBe(true)
  world.list = [] // the row is gone from the list
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: false, turnId: 'main-2', reason: 'answer' } as never)
  await settle(async () => (await stateOf($)).rows.a1?.listed === false)
  const state = await stateOf($)
  expect(state.rows.a1!.listed).toBe(false)
  expect(state.rows.a1!.status).toBe('running') // not finished, failed or killed: nothing says so
  expect(state.rows.a1!.completion).toBe(null)
  expect(state.pending).toEqual({})
  // and the pure rule: a row the list never had is made from the list, with no report and no completion
  const only = reconcileAgentList(emptyAgentState(SESSION), [{ id: 'z', type: 'Plan', description: 'x', status: 'completed' }], T0)
  expect(only.rows.z!.status).toBe('completed')
  expect(only.rows.z!.completion).toBe(null)
  expect(only.pending).toEqual({})
})

test('pending_waiting_idle_are_not_stalled', async () => {
  const at = T0
  const row = (status: string, last_at: number) => ({ status, last_at }) as never
  for (const status of ['pending', 'waiting', 'idle', 'completed', 'failed', 'killed', 'unknown']) {
    expect(isStalled(row(status, at - 3 * 60 * 60 * 1000), at)).toBe(false)
  }
  expect(isStalled(row('running', at - STALL_MS + 1), at)).toBe(false)
  expect(isStalled(row('running', at - STALL_MS), at)).toBe(true)
  // a list poll never refreshes last_at: a running row stays stalled however often the list shows it
  let state = reduceAgentEvent(emptyAgentState(SESSION), { kind: 'touch', at, agentId: 'a1', tool: 'Bash' })
  for (let n = 1; n <= 12; n++) {
    state = reconcileAgentList(state, [{ id: 'a1', type: 'Explore', description: 'x', status: 'running' }], at + n * 60_000)
  }
  expect(state.rows.a1!.last_at).toBe(at)
  expect(isStalled(state.rows.a1!, at + 12 * 60_000)).toBe(true)
  // the list says waiting or idle: not stalled, and not running either
  const waiting = reconcileAgentList(state, [{ id: 'a1', type: 'Explore', description: 'x', status: 'waiting' }], at + 13 * 60_000)
  expect(waiting.rows.a1!.status).toBe('waiting')
  expect(isStalled(waiting.rows.a1!, at + 60 * 60_000)).toBe(false)
})

test('resumed_agent_has_new_completion_identity', async ($, on) => {
  host(on)
  await spawn($, 'tu-1')
  await step($, 'a1', 'turn-1', 0)
  await finish($, 'a1', 'turn-1', 'First report.')
  let row = (await stateOf($)).rows.a1!
  expect(row.status).toBe('completed')
  expect(row.completion?.key).toBe(completionKey(SESSION, 'a1', 'turn-1'))
  await step($, 'a1', 'turn-2', 0) // the same agent, run again
  row = (await stateOf($)).rows.a1!
  expect(row.status).toBe('running')
  expect(row.completion).toBe(null) // no report yet for this run
  await finish($, 'a1', 'turn-2', 'Second report.')
  const state = await stateOf($)
  expect(state.rows.a1!.completion?.key).toBe(completionKey(SESSION, 'a1', 'turn-2'))
  expect(state.rows.a1!.completion?.answer).toBe('Second report.')
  expect(Object.keys(state.pending).sort()).toEqual([completionKey(SESSION, 'a1', 'turn-1'), completionKey(SESSION, 'a1', 'turn-2')].sort())
  await finish($, 'a1', 'turn-2', 'Second report.') // the same run reported twice: one identity
  expect(Object.keys((await stateOf($)).pending).length).toBe(2)
})

test('reload_preserves_claimed_completions', async ($, on) => {
  host(on)
  await spawn($, 'tu-1')
  await finish($, 'a1', 'turn-1', 'The report.')
  const key = completionKey(SESSION, 'a1', 'turn-1')
  let ledger: Ledger = admitCompletion({}, key).ledger
  expect(pendingCompletions(await stateOf($), ledger).map(entry => entry.key)).toEqual([key])
  const claim = claimCompletions(ledger, [key])
  ledger = claim.ledger
  expect(claim.claimed).toEqual([key])
  let state = markDisposition(await stateOf($), [key], 'claimed')
  // a reload: the state and the ledger come back from storage, and the module sees the completion again
  state = JSON.parse(JSON.stringify(state)) as AgentState
  ledger = JSON.parse(JSON.stringify(ledger)) as Ledger
  state = reduceAgentEvent(state, { kind: 'complete', at: T0 + 5, agentId: 'a1', turnId: 'turn-1', reason: 'answer', answer: 'The report.' })
  expect(state.pending).toEqual({}) // not pending again
  expect(state.rows.a1!.completion?.disposition).toBe('claimed')
  expect(pendingCompletions(state, ledger)).toEqual([])
  expect(claimCompletions(ledger, [key]).claimed).toEqual([]) // never claimed twice
  expect(admitCompletion(ledger, key)).toEqual({ ledger, fresh: false, full: false }) // nor admitted twice
  ledger = settleCompletions(ledger, [key], 'looked')
  expect(ledger[key]).toBe('looked')
  expect(settleCompletions(ledger, [key], 'failed')[key]).toBe('looked') // a final word stays final
  // a full ledger refuses a new identity and evicts none
  const full: Ledger = Object.fromEntries(Array.from({ length: LEDGER_MAX }, (_, i) => [`k${i}`, 'looked' as const]))
  expect(admitCompletion(full, 'new').full).toBe(true)
  expect(Object.keys(admitCompletion(full, 'new').ledger).length).toBe(LEDGER_MAX)
})

test('observer_failure_never_repeats_next', async ($, on) => {
  const toolRuns = { count: 0 }
  const world: World = { toolRuns, breakId: { on: false } }
  host(on, world)
  world.breakId!.on = true // every observer now fails where it reads the session
  const result = await edit($, 'a1', 'tool-1')
  expect(toolRuns.count).toBe(1) // the tool ran once, and its result is the tool's own
  expect((result as { text?: string }).text).toBe('Saved')
  await edit($, undefined, 'tool-2')
  expect(toolRuns.count).toBe(2)
  expect(await spawn($, 'tu-1')).toEqual({ model: 'claude-opus-5', agentId: 'a1' }) // the spawn answered once, as the tool did
  await step($, 'a1', 'turn-1', 0)
  expect((await finish($, 'a1', 'turn-1')).text).toBe('Found it.')
  world.breakId!.on = false
  expect(Object.keys((await stateOf($)).rows)).toEqual([]) // and nothing was recorded
})

test('tracking_works_with_dreams_and_compaction_off', { options: { dreams: false, autoCompact: 'off' } }, async ($, on) => {
  host(on)
  await spawn($, 'tu-1')
  await edit($, 'a1', 'tool-1', '/repo/src/b.ts')
  await edit($, undefined, 'tool-2', '/repo/src/b.ts') // the main loop wrote the same file
  await step($, 'a1', 'turn-1', 0)
  await finish($, 'a1', 'turn-1', 'Done.')
  const state = await stateOf($)
  expect(state.rows.a1!.tools).toBe(1)
  expect(state.rows.a1!.tokens).toBe(116)
  expect(state.rows.a1!.status).toBe('completed')
  expect(state.main[fingerprint('/repo/src/b.ts')]).toBe('/repo/src/b.ts')
  expect(Object.keys(state.pending).length).toBe(1)
})

test('a failed or refused or interrupted turn is terminal and never pending', async () => {
  let state = emptyAgentState(SESSION)
  state = reduceAgentEvent(state, { kind: 'complete', at: T0, agentId: 'a1', turnId: 't1', reason: 'error', answer: 'partial' })
  state = reduceAgentEvent(state, { kind: 'complete', at: T0, agentId: 'a2', turnId: 't1', reason: 'refusal', answer: 'no' })
  state = reduceAgentEvent(state, { kind: 'complete', at: T0, agentId: 'a3', turnId: 't1', reason: 'aborted', answer: '' })
  state = reduceAgentEvent(state, { kind: 'complete', at: T0, agentId: 'a4', turnId: 't1', reason: 'answer', answer: '   ' })
  expect([1, 2, 3, 4].map(n => state.rows[`a${n}`]!.status)).toEqual(['failed', 'failed', 'killed', 'completed'])
  expect([1, 2, 3, 4].map(n => state.rows[`a${n}`]!.completion?.disposition)).toEqual(['skipped', 'skipped', 'skipped', 'skipped'])
  expect(state.rows.a4!.completion?.answer).toBe('') // an answer with no text is "no report"
  expect(state.pending).toEqual({})
})

test('tool and step observations are counted once', async () => {
  let state = emptyAgentState(SESSION)
  const tool = { kind: 'tool' as const, at: T0, agentId: 'a1', toolUseId: 'u1', tool: 'Edit', ok: true, path: '/r/x.ts' }
  state = reduceAgentEvent(reduceAgentEvent(state, tool), tool)
  const stepped = { kind: 'step' as const, at: T0, agentId: 'a1', turnId: 't', index: 0, answer: 'hi', tokens: 50 }
  state = reduceAgentEvent(reduceAgentEvent(state, stepped), stepped)
  expect(state.rows.a1!.tools).toBe(1)
  expect(state.rows.a1!.tokens).toBe(50)
  expect(stepKey(SESSION, 'a1', 't', 0)).toBe('["sess-a","a1","t",0]')
  expect(stepKey(SESSION, undefined, 't', 3)).toBe('["sess-a",null,"t",3]')
  // a failed or denied edit is not a write
  state = reduceAgentEvent(state, { kind: 'tool', at: T0, agentId: 'a1', toolUseId: 'u2', tool: 'Write', ok: false, path: '/r/y.ts' })
  expect(Object.values(state.rows.a1!.paths)).toEqual(['/r/x.ts'])
})

test('paths are normalized and a state is keyed by its session', async () => {
  expect(normalizePath('src/../lib/./a.ts', '/repo/')).toBe('/repo/lib/a.ts')
  expect(normalizePath('/a//b/../c', '/x')).toBe('/a/c')
})

test('redaction by agents.ts is the helper\'s table, and then some', async () => {
  for (const [input, expected] of table) expect(redactAgentValue(input)).toBe(expected)
  expect(redactAgentValue('hash 0123456789abcdef0123456789abcdef01234567 end')).toBe('hash [redacted] end')
  expect(redactAgentValue('a 0123456789abcdef short')).toBe('a 0123456789abcdef short') // under 32 digits: a commit, not a secret
  expect(redactAgentValue('STRIPE_KEY=live_abc\nnext line')).toBe('STRIPE_KEY=[redacted]\nnext line')
  expect(redactAgentValue('export SIGNING_SECRET: plain\nok')).toBe('export SIGNING_SECRET: [redacted]\nok')
  expect(redactAgentValue('\x1b[31mred\x1b[0m \x1b]0;title\x07 bell\x07 ok')).toBe('red  bell ok')
  expect(redactAgentValue({ a: ['password=hunter2'], n: 3 })).toEqual({ a: ['password=[redacted]'], n: 3 })
  expect(redactAgentValue(redactAgentValue('password=hunter2 and sk-abcdefghijklmnopqrstuv'))).toBe('password=[redacted] and [redacted]')
})

test('the agents file is written at once, then at most every five seconds, terminal events included', async ($, on) => {
  const writes: NonNullable<World['writes']> = []
  const clock = host(on, { writes })
  await spawn($, 'tu-1')
  await settle(() => writes.length === 1)
  expect(writes[0]!.path).toBe('/home/u/.claude-self-reflect/csr-companion/agents/sess-a.json')
  expect(writes[0]!.flags).toEqual(['--redact-json']) // through the helper, as JSON: its strings redacted, then serialized
  for (let n = 0; n < 10; n++) await edit($, 'a1', `tool-${n}`, `/repo/f${n}.ts`)
  await step($, 'a1', 'turn-1', 0)
  await finish($, 'a1', 'turn-1', 'The report.')
  await clock.advance(FILE_EVERY_MS - 1)
  expect(writes.length).toBe(1) // the burst is held back
  await clock.advance(1)
  await settle(() => writes.length === 2)
  await clock.advance(FILE_EVERY_MS * 3)
  expect(writes.length).toBe(2) // and it was one write: nothing changed since
  const file = JSON.parse(writes[1]!.text)
  expect(file.session).toBe(SESSION)
  expect(file.agents[0].tools).toBe(10)
  expect(file.agents[0].status).toBe('completed') // the terminal event reached the file within the interval
  expect(file.agents[0].completion.answer).toBe('The report.')
  expect(utf8Length(writes[1]!.text)).toBeLessThanOrEqual(FILE_BYTES)
})

test('a session id that could name another file is never written', async ($, on) => {
  const writes: NonNullable<World['writes']> = []
  host(on, { writes, session: '../../etc/x' })
  await spawn($, 'tu-1')
  await new Promise<void>(resolve => setTimeout(() => resolve(), 50))
  expect(writes).toEqual([])
})

test('planWrite: the first write at once, the rest one trailing write, a lost timer counts for nothing', async () => {
  let state = reduceAgentEvent(emptyAgentState(SESSION), { kind: 'touch', at: T0, agentId: 'a1', tool: 'Bash' })
  let plan = planWrite(state, T0)
  expect(plan.act).toBe('now')
  state = plan.state
  plan = planWrite(state, T0 + 1000)
  expect([plan.act, plan.wait]).toEqual(['later', FILE_EVERY_MS - 1000])
  state = plan.state
  expect(planWrite(state, T0 + 2000).act).toBe('none') // a timer is due: this change is covered by its write
  expect(planWrite(state, T0 + FILE_EVERY_MS + 1).act).toBe('now') // the timer never fired (a reload): due is in the past
  expect(planWrite(emptyAgentState(SESSION), T0).act).toBe('none') // no agents, no file
})

test('one parent and seven children, and multibyte reports, stay within 16 KB and 12 rows', async () => {
  let state = emptyAgentState(SESSION)
  const rec = (id: string, parent: string | null) => reduceAgentEvent(state, { kind: 'touch', at: T0, agentId: id, tool: 'Bash' }).rows[id]!
  state = { ...state, rows: { p: { ...rec('p', null), type: 'Plan', model: 'opus' } } }
  for (let n = 0; n < 7; n++) {
    state = reduceAgentEvent(state, { kind: 'spawn', at: T0, toolUseId: `t${n}`, description: `Child ${n}`, type: 'Explore', parent: 'p', model: null })
    state = reduceAgentEvent(state, { kind: 'spawned', at: T0, toolUseId: `t${n}`, agentId: `c${n}`, model: 'opus' })
    state = reduceAgentEvent(state, { kind: 'complete', at: T0 + n, agentId: `c${n}`, turnId: 'turn-1', reason: 'answer', answer: '界'.repeat(700) })
  }
  const file = projectAgentsFile(state, T0 + 1000)
  expect(file.agents.map(row => row.id).sort()).toEqual(['c0', 'c1', 'c2', 'c3', 'c4', 'c5', 'c6', 'p'])
  expect(file.agents[0]!.id).toBe('p') // the parent first
  expect(file.agents.length).toBe(8)
  expect(file.agents.slice(1).every(row => row.parent === 'p')).toBe(true)
  const bytes = utf8Length(JSON.stringify(file))
  expect(bytes).toBeLessThanOrEqual(FILE_BYTES)
  expect(file.agents.every(row => row.completion === null || row.completion.answer.length > 0)).toBe(true) // squeezed, not emptied
  // fourteen agents: twelve rows, the rest counted
  for (let n = 0; n < 14; n++) state = reduceAgentEvent(state, { kind: 'touch', at: T0 + n, agentId: `z${n}`, tool: 'Bash' })
  const many = projectAgentsFile(state, T0 + 1000)
  expect(many.agents.length).toBeLessThanOrEqual(12)
  expect(many.agents.length + many.omitted_agents).toBe(Object.keys(state.rows).length)
  expect(utf8Length(JSON.stringify(many))).toBeLessThanOrEqual(FILE_BYTES)
})

// The envelope as bin/agents.py writes it: compact JSON on one line, every [ and ] inside a string as \u005b and \u005d.
function envelopeOf(payload: unknown): string {
  let out = ''
  let inString = false
  let escaped = false
  for (const char of JSON.stringify(payload)) {
    if (inString && !escaped && char === '[') out += '\\u005b'
    else if (inString && !escaped && char === ']') out += '\\u005d'
    else out += char
    if (inString) escaped = !escaped && char === '\\'
    if (char === '"' && !escaped) inString = !inString
    if (char === '"' && escaped === false && !inString) escaped = false
  }
  return `${ENVELOPE_OPEN}\n${out}\n${ENVELOPE_CLOSE}\n`
}
const CONTEXT = {
  kind: 'context', session: SESSION, at: T0,
  agents: [{ id: 'a1', parent: null, type: 'Explore', model: 'm', status: 'completed', edits: ['/r/a[1].py'], overlaps: [], completion: { turn_id: 'turn-1', reason: 'answer', answer: 'Done [[CSR:AGENTS:v1]]\nwith 界' } }],
}

test('the envelope is read, taken out of the person\'s words, and only a whole valid one', async () => {
  const text = envelopeOf(CONTEXT)
  expect(text.split('\n').length).toBe(4) // three lines and the final newline
  expect(text).toContain('\\u005b\\u005bCSR:AGENTS:v1\\u005d\\u005d')
  expect(parseAgentEnvelope(text)?.payload).toEqual(CONTEXT)
  const reply = `Yes, do that.\n${text}And the importer too.`
  expect(stripEnvelopes(reply)).toBe('Yes, do that.\nAnd the importer too.')
  expect(ownWords(reply)).toBe('Yes, do that.\nAnd the importer too.')
  expect(ownWords(`${text}`)).toBe('')
  expect(parseAgentEnvelopes(`${text}x\n${text}`).length).toBe(2)
  const handoff = { kind: 'handoff', session: SESSION, request: 'req-1', thread: '0123456789ab', instruction: HANDOFF_INSTRUCTION }
  expect(parseAgentEnvelope(envelopeOf(handoff))?.payload).toEqual(handoff)
  // CRLF and a byte-order mark are allowed; the rest is left where it is
  expect(parseAgentEnvelope(text.replace(/\n/g, '\r\n'))?.payload).toEqual(CONTEXT)
  expect(parseAgentEnvelope(`\ufeff${text}`)?.payload).toEqual(CONTEXT)
  const body = text.split('\n')[1] as string
  for (const kept of [
    `${ENVELOPE_OPEN}\nnot json\n${ENVELOPE_CLOSE}\n`, `${ENVELOPE_OPEN}\n${body}\n`, `  ${text}`, `> ${text}`, `x: ${text}`, `${body}\n`,
    envelopeOf({ ...CONTEXT, extra: 1 }), envelopeOf({ ...handoff, instruction: 'Do something else.' }), `${ENVELOPE_OPEN}\n${body}\n${body}\n${ENVELOPE_CLOSE}\n`,
    text.replace('v1]]', 'v2]]'), envelopeOf({ ...CONTEXT, pad: 'x'.repeat(20000) }),
  ]) {
    expect(stripEnvelopes(kept)).toBe(kept)
    expect(parseAgentEnvelope(kept)).toBe(null)
  }
  expect(ownWords('Mind [[CSR:AGENTS:v1]] in prose')).toBe('Mind [[CSR:AGENTS:v1]] in prose')
})

test('completion news is the claimed runs the envelope reports, by identity', async () => {
  const envelope = parseAgentEnvelope(envelopeOf(CONTEXT))
  const key = completionKey(SESSION, 'a1', 'turn-1')
  expect(completionNews(envelope, { [key]: 'claimed' }, [key])).toEqual([key])
  expect(completionNews(envelope, { [key]: 'pending' }, [key])).toEqual([]) // not claimed: not this look's
  expect(completionNews(envelope, { [key]: 'looked' }, [key])).toEqual([]) // already looked at
  expect(completionNews(envelope, { [key]: 'claimed' }, [completionKey(SESSION, 'a9', 'turn-1')])).toEqual([])
  expect(completionNews(null, { [key]: 'claimed' }, [key])).toEqual([])
  const failed = envelopeOf({ ...CONTEXT, agents: [{ ...CONTEXT.agents[0], status: 'failed', completion: null }] })
  expect(completionNews(parseAgentEnvelope(failed), { [key]: 'claimed' }, [key])).toEqual([])
})
