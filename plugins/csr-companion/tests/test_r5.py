"""Run the hook regressions with the installed TypeScript compiler and a small host."""
import os
from pathlib import Path
import shutil
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[1]


@pytest.fixture(scope='module')
def typescript():
    if not shutil.which('node'):
        pytest.skip('Node is unavailable')
    candidates = [parent / 'node_modules/typescript/lib/typescript.js' for parent in ROOT.parents]
    if shutil.which('npm'):
        found = subprocess.run(['npm', 'root', '-g'], capture_output=True, text=True, check=True)
        base = Path(found.stdout.strip())
        candidates.extend([base / 'typescript/lib/typescript.js'])
        candidates.extend(base.glob('*/node_modules/typescript/lib/typescript.js'))
        candidates.extend(base.glob('@*/*/node_modules/typescript/lib/typescript.js'))
    for candidate in candidates:
        if candidate.is_file():
            return str(candidate)
    pytest.skip('An installed TypeScript compiler is unavailable')


def probe(typescript, name):
    root = os.environ.get('CSR_R5_SOURCE_ROOT', str(ROOT))
    ran = subprocess.run(['node', '-e', HOST, typescript, root, name], capture_output=True, text=True, timeout=20)
    assert ran.returncode == 0, ran.stdout + ran.stderr


def test_r5_lease_renews_and_discards_a_lost_look(typescript):
    probe(typescript, 'lease')


def test_r5_keep_ids_are_compact_whole_and_bounded(typescript):
    probe(typescript, 'keep')


def test_r5_subagent_ownership_survives_module_reload(typescript):
    probe(typescript, 'ownership')


def test_r5_only_successful_edits_own_paths(typescript):
    probe(typescript, 'edits')


def test_r5_commit_uses_its_own_command_directory(typescript):
    probe(typescript, 'commit')


def test_r5_delayed_write_cannot_restore_a_cancelled_plan(typescript):
    probe(typescript, 'write')


def test_r5_stale_countdown_cannot_erase_a_newer_plan(typescript):
    probe(typescript, 'stale')


HOST = r'''
const assert = require('node:assert/strict')
const fs = require('node:fs')
const path = require('node:path')
const vm = require('node:vm')
const os = require('node:os')
const childProcess = require('node:child_process')
const ts = require(process.argv[1])
const root = process.argv[2]
const which = process.argv[3]
const copy = value => value === undefined ? undefined : JSON.parse(JSON.stringify(value))
const kit = {
  atom: (ref, initial) => ({ ...ref, initial }),
  read: async ($, ref) => (await $.state.get(ref)).value ?? copy(ref.initial),
  update: async ($, ref, fn) => {
    for (;;) {
      const read = await $.state.get(ref)
      const value = fn(read.value ?? copy(ref.initial))
      if ((await $.state.set(ref, value, { ifVersion: read.version })).isSet) return value
    }
  },
}
function modules() {
  const cache = new Map()
  function load(filename) {
    if (cache.has(filename)) return cache.get(filename).exports
    const mod = { exports: {} }
    cache.set(filename, mod)
    let source = fs.readFileSync(filename, 'utf8')
    if (filename.endsWith('/register.tsx')) source += '\nexport const probe = { CS, dream, stopSeen, keepOf, commitDir, showPlan, startCountdown, settingChanged };'
    const js = ts.transpileModule(source, { compilerOptions: {
      module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.React,
    }, fileName: filename }).outputText
    const requireLocal = name => {
      if (name === 'claude-code') return kit
      if (!name.startsWith('.')) throw new Error(`Unexpected module ${name}`)
      const base = path.resolve(path.dirname(filename), name)
      return load(['.ts', '.tsx', '/index.ts'].map(ext => base + ext).find(file => fs.existsSync(file)))
    }
    vm.runInThisContext(`(function(require,module,exports){${js}\n})`, { filename })(requireLocal, mod, mod.exports)
    return mod.exports
  }
  return {
    register: () => load(path.join(root, 'hooks/register.tsx')),
    compact: () => load(path.join(root, 'hooks/compact.ts')),
  }
}
const deferred = () => {
  let resolve
  const promise = new Promise(done => { resolve = done })
  return { promise, resolve }
}
async function settle(until) {
  for (let n = 0; n < 300 && !until(); n++) await new Promise(resolve => setImmediate(resolve))
  assert.ok(until(), 'The expected asynchronous step was not reached')
}
function world() {
  const state = new Map(), stored = new Map(), files = new Map(), timers = [], runs = [], records = []
  let now = 1_000_000, fork = null, leaseHeld = true, forkCalls = 0
  const $ = {
    plugin: { root },
    state: {
      get: async ref => copy(state.get(ref.key) ?? { version: 0 }),
      set: async (ref, value, options) => {
        const version = state.get(ref.key)?.version ?? 0
        if (options && options.ifVersion !== version) return { isSet: false, version }
        state.set(ref.key, { value: copy(value), version: version + 1 })
        return { isSet: true, version: version + 1 }
      },
    },
    store: { get: async key => copy(stored.get(key)), set: async (key, value) => { stored.set(key, copy(value)) } },
    env: { get: async name => name === 'HOME' ? '/home/test' : undefined },
    session: { id: async () => 's1', cwd: async () => '/repo', model: async () => 'opus', surfaces: async () => ['terminal'] },
    clock: {
      now: async () => now,
      every: (ms, fn) => { const timer = { ms, fn, cancelled: false, cancel() { this.cancelled = true } }; timers.push(timer); return timer },
      after: (ms, fn) => { const timer = { ms, fn, cancelled: false, cancel() { this.cancelled = true } }; timers.push(timer); return timer },
    },
    fs: { write: async (name, text) => { files.set(name, text) } },
    ui: { invalidate() {}, log() {} },
    model: { fork: async () => {
      forkCalls++
      if (fork) await fork.promise
      return { isAnswered: true, text: 'dream: What if the notes read side branches before main?\nexplain:\n**Follow side branches**\n* A separate branch can carry today\'s work.' }
    } },
    process: { run: async (argv, init) => {
      runs.push(argv)
      let stdout = '{}'
      if (argv.some(arg => arg.endsWith('/private.py'))) await $.fs.write(argv[4], init.stdin) // bin/private.py write PATH, text on stdin
      else if (argv.some(arg => arg.endsWith('/lease.py'))) stdout = JSON.stringify({ held: leaseHeld })
      else if (argv.includes('--brief')) stdout = '* The current work has a side branch.'
      else if (argv.includes('--keep')) records.push(JSON.parse(argv[argv.indexOf('--keep') + 1]))
      else if (argv[0] === 'git') return { exitCode: 1, stdout: '' }
      return { exitCode: 0, stdout }
    } },
  }
  stored.set('pace:s1', { session: 's1', claude: 1000000, dreamer: 0, looks: 0, dreams: 0, lastLook: 0 })
  stored.set('chapter:s1', { session: 's1', started: now, steps: 6, due: 6, looks: 0, shown: 0, adopted: 0, gen: 1 })
  return { $, state, stored, files, timers, runs, records,
    holdFork: () => (fork = deferred()), forkCalls: () => forkCalls,
    loseLease: () => { leaseHeld = false }, advance: ms => { now += ms },
    plan: () => JSON.parse([...files.values()].at(-1) ?? 'null'),
  }
}
function toolHook(mod) {
  const hooks = []
  const on = (name, ...args) => { hooks.push({ name, matcher: args.length === 2 ? args[0] : null, fn: args.at(-1) }); return { catch() {} } }
  mod.register(on, { autoCompact: 'auto' })
  return hooks.find(row => row.name === 'tool.call' && row.matcher?.tool?.includes('AskUserQuestion')).fn
}
const armed = answer => ({ ctx: 280000, payback: null, seam: { isSeam: true, why: 'done', question: false }, mode: 'attended', forced: true, floor: null, answer })

async function leaseTest() {
  const mod = modules().register(), w = world(), gate = w.holdFork()
  const job = mod.probe.dream(w.$, 100, 6)
  await settle(() => w.forkCalls() === 1)
  const heartbeat = w.timers.find(timer => timer.ms > 0 && timer.ms < 15 * 60 * 1000)
  assert.ok(heartbeat, 'An unresolved look needs a lease renewal timer')
  w.advance(heartbeat.ms)
  await heartbeat.fn()
  await settle(() => w.runs.some(argv => argv.includes('renew')))
  w.loseLease()
  w.advance(heartbeat.ms)
  await heartbeat.fn()
  await new Promise(resolve => setImmediate(resolve))
  gate.resolve()
  await job
  assert.equal(w.records.filter(row => row.kind === 'dream' || row.kind === 'look').length, 0)
  assert.equal(w.state.get('view')?.value?.note?.kind, undefined)
  assert.equal(w.stored.get('seenDreams'), undefined)
  assert.ok(heartbeat.cancelled)

  // The final ownership check also catches loss between timer ticks.
  const last = modules().register(), x = world(), reply = x.holdFork()
  const ending = last.probe.dream(x.$, 100, 6)
  await settle(() => x.forkCalls() === 1)
  x.loseLease()
  reply.resolve()
  await ending
  assert.equal(x.records.filter(row => row.kind === 'dream' || row.kind === 'look').length, 0)
  assert.equal(x.state.get('view')?.value?.note?.kind, undefined)

  for (const failure of ['exit', 'malformed', 'throw', 'missing']) {
    const mod = modules().register(), w = world(), gate = w.holdFork()
    const original = w.$.process.run
    let failed = false
    w.$.process.run = async (argv, init) => {
      if (failed && argv.includes('renew')) {
        if (failure === 'throw') throw new Error('The lease helper timed out')
        if (failure === 'missing') throw new Error('spawn python3 ENOENT')
        return { exitCode: failure === 'exit' ? 1 : 0, stdout: failure === 'exit' ? '{"held":true}' : 'not JSON' }
      }
      return original(argv, init)
    }
    const job = mod.probe.dream(w.$, 100, 6)
    await settle(() => w.forkCalls() === 1)
    failed = true
    const timer = w.timers.find(timer => timer.ms <= 60000)
    assert.ok(timer)
    await timer.fn()
    await new Promise(resolve => setImmediate(resolve))
    failed = false
    gate.resolve()
    await job
    assert.equal(w.records.filter(row => row.kind === 'dream' || row.kind === 'look').length, 0, failure)
    assert.equal(w.state.get('view')?.value?.note?.kind, undefined, failure)
  }

  // Ownership can change after the answer is parsed, while its receipt is checked.
  const publication = modules().register(), p = world()
  p.$.model.fork = async () => ({ isAnswered: true, text: 'dream: What if notes followed side branches?\nexplain:\n**Read the branch**\n* The branch changed in [commit abc1234].' })
  const processRun = p.$.process.run
  let receiptChecked = false
  p.$.process.run = async (argv, init) => {
    if (argv.includes('cat-file')) {
      receiptChecked = true
      p.loseLease()
      return { exitCode: 0, stdout: '' }
    }
    return processRun(argv, init)
  }
  await publication.probe.dream(p.$, 100, 6)
  assert.ok(receiptChecked)
  assert.equal(p.records.filter(row => row.kind === 'dream' || row.kind === 'look').length, 0)
  assert.equal(p.state.get('view')?.value?.note?.kind, undefined)
  assert.equal(p.stored.get('seenDreams'), undefined)

  // A delayed read cannot record the result after a later renewal fails.
  const remembering = modules().register(), r = world(), memory = deferred()
  const storeGet = r.$.store.get
  let pausedMemory = false
  r.$.store.get = async key => {
    if (key === 'seenDreams' && r.state.get('view')?.value?.note?.kind === 'dream') {
      pausedMemory = true
      await memory.promise
    }
    return storeGet(key)
  }
  const remembered = remembering.probe.dream(r.$, 100, 6)
  await settle(() => pausedMemory)
  assert.equal(r.records.filter(row => row.kind === 'dream').length, 0)
  r.loseLease()
  const renew = r.timers.find(timer => timer.ms <= 60000)
  assert.ok(renew)
  await renew.fn()
  await new Promise(resolve => setImmediate(resolve))
  memory.resolve()
  await remembered
  assert.equal(r.stored.get('seenDreams'), undefined)
  assert.equal(r.records.filter(row => row.kind === 'dream').length, 0)

  const outcome = modules().register(), o = world(), clockGate = deferred()
  o.$.model.fork = async () => ({ isAnswered: true, text: 'dream: none' })
  const clockNow = o.$.clock.now
  let outcomeClocks = 0, pausedOutcome = false
  o.$.clock.now = async () => {
    if (o.stored.get('chapter:s1')?.looks === 1 && ++outcomeClocks === 2) {
      pausedOutcome = true
      await clockGate.promise
    }
    return clockNow()
  }
  const recorded = outcome.probe.dream(o.$, 100, 6)
  await settle(() => pausedOutcome)
  o.loseLease()
  const outcomeRenew = o.timers.find(timer => timer.ms <= 60000)
  assert.ok(outcomeRenew)
  await outcomeRenew.fn()
  await new Promise(resolve => setImmediate(resolve))
  clockGate.resolve()
  await recorded
  assert.equal(o.records.filter(row => row.kind === 'look').length, 0)

  // A live look retains the real helper's lease beyond the busy expiry.
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), 'csr-r5-lease-'))
  try {
    const mod = modules().register(), w = world(), gate = w.holdFork()
    const original = w.$.process.run
    let latest = null
    w.$.env.get = async () => folder
    w.$.process.run = async (argv, init) => {
      if (!argv.some(arg => arg.endsWith('/lease.py'))) return original(argv, init)
      latest = argv
      const ran = childProcess.spawnSync(argv[0], argv.slice(1), { encoding: 'utf8' })
      return { exitCode: ran.status, stdout: ran.stdout }
    }
    const job = mod.probe.dream(w.$, 100, 6)
    await settle(() => w.forkCalls() === 1)
    const timer = w.timers.find(timer => timer.ms <= 60000)
    assert.ok(timer)
    for (let elapsed = 0; elapsed <= 16 * 60000; elapsed += timer.ms) {
      w.advance(timer.ms)
      const before = latest
      await timer.fn()
      await settle(() => latest !== before)
      const contender = [...latest]
      contender[3] = 'take'
      contender[contender.indexOf('--root') + 1] = '/other/plugin'
      const ran = childProcess.spawnSync(contender[0], contender.slice(1), { encoding: 'utf8' })
      assert.equal(ran.status, 0, ran.stderr)
      assert.equal(JSON.parse(ran.stdout).held, false, `Busy lease lost after ${elapsed} ms`)
    }
    gate.resolve()
    await job
    assert.ok(w.records.some(row => row.kind === 'dream'))
  } finally {
    fs.rmSync(folder, { recursive: true, force: true })
  }
}
function keepTest() {
  const { keepList, KEEP_MAX } = modules().compact()
  const running = count => Array.from({ length: count }, (_, i) => ({ id: `agent${String(i).padStart(12, '0')}`, type: 'subagent', description: 'A description '.repeat(30) }))
  for (const count of [18, 30]) {
    const tasks = running(count), text = keepList({ running: tasks, decisions: [], work: [], rules: [] })
    tasks.forEach(task => assert.ok(text.includes(task.id), task.id))
    assert.equal((text.match(/subagent/g) ?? []).length, 1)
    assert.ok(text.length <= Math.max(KEEP_MAX, tasks.map(task => task.id).join(',').length + 180), text.length)
  }
  const tasks = running(100), text = keepList({ running: tasks, decisions: [], work: [], rules: [] })
  const listed = tasks.filter(task => text.includes(task.id))
  assert.ok(listed.length > 0 && listed.length < tasks.length)
  assert.ok(text.length <= 1200, text.length)
  assert.ok(text.includes(`+${tasks.length - listed.length} more running (Claude Code lists them after compaction)`))
  const fragments = text.match(/agent\d+/g) ?? []
  assert.ok(fragments.every(id => tasks.some(task => task.id === id)), 'No id may be cut')
}
async function ownershipTest() {
  const w = world(), first = modules().register(), hook = toolHook(first)
  await hook(w.$, { tool: 'Bash', tool_use_id: 'sub-tool', agentId: 'agent-parent', command: 'sleep 30', run_in_background: true }, async () => ({ text: 'Command running in the background with ID: bchild001' }))
  const tasks = [{ id: 'bchild001', type: 'shell' }]
  for (const tool of ['Agent', 'Workflow', 'Monitor']) {
    const id = `${tool.toLowerCase()}-child`
    await hook(w.$, { tool, tool_use_id: id, agentId: 'agent-parent' }, async () => ({ text: `Agent launched with agentId: ${id}` }))
    tasks.push({ id, type: tool.toLowerCase() })
  }
  tasks.push({ id: 'bunknown1', type: 'shell' }, { id: 'agent-parent', type: 'subagent' })
  await first.probe.stopSeen(w.$, tasks)
  assert.deepEqual((await first.probe.keepOf(w.$, '')).running.map(task => task.id), ['bunknown1', 'agent-parent'])
  const reloaded = modules().register()
  await reloaded.probe.stopSeen(w.$, tasks)
  assert.deepEqual((await reloaded.probe.keepOf(w.$, '')).running.map(task => task.id), ['bunknown1', 'agent-parent'])
}
async function editsTest() {
  const mod = modules().register(), w = world(), hook = toolHook(mod)
  for (const agentId of [undefined, 'child']) {
    for (const tool of ['Edit', 'Write']) {
      const file_path = `${agentId ?? 'main'}-${tool}.ts`
      for (const result of [{ text: 'not found', isError: true }, { deny: 'Permission denied' }]) {
        await hook(w.$, { tool, file_path, tool_use_id: file_path, agentId }, async () => result)
        assert.ok(!mod.probe.CS.wrote.has(`/repo/${file_path}`), `${file_path} failed`)
      }
      await hook(w.$, { tool, file_path, tool_use_id: file_path, agentId }, async () => ({ text: 'Saved' }))
      assert.ok(mod.probe.CS.wrote.has(`/repo/${file_path}`), `${file_path} succeeded`)
    }
  }
}
function commitTest() {
  const { commitDir } = modules().register().probe
  assert.equal(commitDir('git -C /repo status && git -C /other commit -m wip', '/start'), '/other')
  assert.equal(commitDir('git -C /repo status && cd /other && git commit -m wip', '/start'), '/other')
  assert.equal(commitDir('cd /other && git status && git commit -m wip', '/start'), '/other')
  assert.equal(commitDir('git commit -m wip', '/start'), '/start')
  assert.equal(commitDir('git -C /left commit -m wip || git -C /right commit -m wip', '/start'), null)
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), 'csr-r5-commit-'))
  try {
    const start = path.join(folder, 'start'), physical = path.join(folder, 'physical')
    for (const repo of [start, physical]) {
      const init = childProcess.spawnSync('git', ['init', '-q', repo], { encoding: 'utf8' })
      assert.equal(init.status, 0, init.stderr)
    }
    fs.mkdirSync(path.join(physical, 'nested'))
    fs.symlinkSync(path.join(physical, 'nested'), path.join(start, 'link'))
    const top = directory => {
      const ran = childProcess.spawnSync('git', ['-C', directory, 'rev-parse', '--show-toplevel'], { cwd: start, encoding: 'utf8' })
      assert.equal(ran.status, 0, ran.stderr)
      return fs.realpathSync(ran.stdout.trim())
    }
    const invoked = top('link/..')
    assert.equal(invoked, fs.realpathSync(physical))
    assert.equal(top(commitDir('git -C link/.. commit -m wip', start)), invoked)
    assert.equal(top(commitDir('cd link/.. && git commit -m wip', start)), fs.realpathSync(start))
  } finally {
    fs.rmSync(folder, { recursive: true, force: true })
  }
}
async function writeTest() {
  for (const cancel of [true, false]) {
    const mod = modules().register(), w = world(), gate = deferred()
    let blocked = false
    w.$.fs.write = async (name, text) => {
      if (JSON.parse(text).state === 'countdown') { blocked = true; await gate.promise }
      w.files.set(name, text)
    }
    mod.probe.CS.armed = armed('old')
    const old = mod.probe.showPlan(w.$, 'countdown', 'old countdown')
    await settle(() => blocked)
    const newer = cancel ? mod.probe.settingChanged(w.$, 'off') : mod.probe.showPlan(w.$, 'planned', 'newer plan')
    await new Promise(resolve => setImmediate(resolve))
    gate.resolve()
    await Promise.all([old, newer])
    assert.equal(w.plan().state, cancel ? 'none' : 'planned')
    assert.equal(w.plan().text, cancel ? null : 'newer plan')
    if (cancel) assert.equal(mod.probe.CS.armed, null)
  }
}
async function staleTest() {
  for (const changed of [true, false]) {
    const mod = modules().register(), w = world(), gate = deferred()
    let blocked = false
    const now = w.$.clock.now
    w.$.clock.now = async () => {
      if (!blocked) { blocked = true; await gate.promise }
      return now()
    }
    const old = mod.probe.startCountdown(w.$, armed('old'), mod.probe.CS.settingStamp)
    await settle(() => blocked)
    if (changed) {
      await mod.probe.settingChanged(w.$, 'off')
      await mod.probe.settingChanged(w.$, 'auto')
    }
    const at = armed('new')
    await mod.probe.startCountdown(w.$, at, mod.probe.CS.settingStamp)
    assert.equal(w.plan().state, 'countdown')
    const before = [...w.files.values()].at(-1)
    gate.resolve()
    await old
    assert.equal([...w.files.values()].at(-1), before)
    assert.equal(mod.probe.CS.armed, at)
  }
}
const cases = { lease: leaseTest, keep: keepTest, ownership: ownershipTest, edits: editsTest, commit: commitTest, write: writeTest, stale: staleTest }
Promise.resolve().then(() => cases[which]()).catch(error => { console.error(error); process.exitCode = 1 })
'''
