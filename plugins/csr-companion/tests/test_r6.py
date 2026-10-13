"""Exercise round-six hook races in the source-loaded host from round five."""
import os
import subprocess

import pytest

from test_r5 import HOST as R5_HOST, ROOT, typescript


def probe(typescript, name):
    root = os.environ.get('CSR_R6_SOURCE_ROOT', str(ROOT))
    ran = subprocess.run(['node', '--unhandled-rejections=strict', '-e', HOST, typescript, root, name],
                         capture_output=True, text=True, timeout=20)
    assert ran.returncode == 0, ran.stdout + ran.stderr


def test_r6_unquoted_newlines_separate_commit_invocations(typescript):
    probe(typescript, 'commit')


def test_r6_detached_dream_refresh_contains_rejection(typescript):
    probe(typescript, 'refresh')


def test_r6_detached_note_button_contains_pane_rejection(typescript):
    probe(typescript, 'button')


def test_r6_turn_replaces_preparing_compaction_status(typescript):
    probe(typescript, 'turn')


@pytest.mark.parametrize('failure', ['holder', 'exit', 'malformed', 'throw'])
def test_r6_lease_loss_cancels_timer_before_fork_settles(typescript, failure):
    probe(typescript, failure)


# Reuse the host, without running its round-five cases or changing production exports.
HOST = R5_HOST[:R5_HOST.index('const cases =')].replace(
    'CS, dream, stopSeen,', 'CS, dream, compactNow, turnStarted, stopSeen,',
) + r'''
function multilineTest() {
  const { commitDir } = modules().register().probe
  for (const boundary of ['\n', '\n\n', ';', '&&', '||']) {
    assert.equal(commitDir(`git -C /left commit -m first${boundary}git -C /right commit -m second`, '/start'), null, boundary)
    assert.equal(commitDir(`git -C /left commit -m first${boundary}git -C /left commit -m second`, '/start'), null, 'Even one repository can have ambiguous invocations')
  }
  assert.equal(commitDir('git -C /left status\ngit -C /right commit -m second', '/start'), '/right')
  assert.equal(commitDir('git -C /left commit -m first\ngit -C /right status', '/start'), '/left')
  for (const quote of ["'", '"']) {
    assert.equal(commitDir(`git -C /left commit -m ${quote}first\ngit -C /right commit -m second${quote}`, '/start'), '/left')
  }
}
async function refreshRejectionTest() {
  const mod = modules().register(), w = world(), original = w.$.process.run
  let rejected = false
  w.$.process.run = async argv => {
    if (argv.includes('--json')) {
      rejected = true
      throw new Error('The pane refresh timed out')
    }
    return original(argv)
  }
  await mod.probe.dream(w.$, 100, 6)
  await settle(() => rejected)
  assert.ok(w.records.some(row => row.kind === 'dream'), 'The dream succeeded before its detached refresh')
  assert.ok(w.timers.every(timer => timer.cancelled))
  await new Promise(resolve => setImmediate(resolve))
}
async function buttonRejectionTest() {
  globalThis.React = { createElement: (type, props, ...children) => ({ type, props, children: children.flat(Infinity) }) }
  const mod = modules().register(), w = world(), hooks = []
  const on = (name, ...args) => { hooks.push({ name, matcher: args.length === 2 ? args[0] : null, fn: args.at(-1) }); return { catch() {} } }
  mod.register(on, { autoCompact: 'off', dreams: true })
  await mod.probe.dream(w.$, 100, 6)
  await new Promise(resolve => setImmediate(resolve))
  w.$.ui.resolve = () => ({ Box: 'Box', Button: 'Button', Code: 'Code', Markdown: 'Markdown', Text: 'Text' })
  const render = hooks.find(hook => hook.name === 'ui.render' && hook.matcher?.component === 'AbovePrompt').fn
  const tree = await render(w.$, { component: 'AbovePrompt', props: {} }, async () => null)
  const find = node => node?.props?.label === 'Dismiss' ? node : node?.children?.map(find).find(Boolean)
  const button = find(tree)
  assert.ok(button, 'A shown dream has a Dismiss button')
  let rejected = false
  w.$.process.run = async argv => {
    assert.ok(argv.some(arg => /\/(pane|shifts)\.py$/.test(arg))) // the dev mod's copy is shifts.py
    rejected = true
    throw new Error('The pane answer timed out')
  }
  button.props.onPress()
  await settle(() => rejected)
  await new Promise(resolve => setImmediate(resolve))
}
async function supersededPreparationTest() {
  const mod = modules().register(), w = world(), gate = deferred(), hooks = []
  const on = (name, ...args) => { hooks.push({ name, fn: args.at(-1) }); return { catch() {} } }
  mod.register(on, { autoCompact: 'auto' })
  let preparing = false, compactions = 0
  const original = w.$.process.run
  w.$.process.run = async argv => {
    if (argv[0] === 'git') { preparing = true; await gate.promise }
    return original(argv)
  }
  w.$.session.compact = async () => { compactions++; return { tokensAfter: 10000 } }
  const preparation = mod.probe.compactNow(w.$, armed('done'), 'unattended', mod.probe.CS.settingStamp)
  await settle(() => preparing)
  assert.equal(w.plan().state, 'compacting')
  assert.match(w.state.get('infinite').value.plan, /^Compacting/)
  const version = mod.probe.CS.planVersion
  await hooks.find(hook => hook.name === 'turn.start').fn(w.$, {}, async () => ({}))
  assert.ok(mod.probe.CS.planVersion > version, 'The new turn owns the replacement')
  assert.notEqual(w.plan().state, 'compacting', 'Disk status must change before the turn returns')
  assert.doesNotMatch(w.state.get('infinite').value.plan ?? '', /^Compacting/, 'Session status must change before the turn returns')
  const replacement = [...w.files.values()].at(-1)
  await hooks.find(hook => hook.name === 'turn.complete').fn(w.$, { reason: 'aborted' }, async () => ({}))
  gate.resolve()
  await preparation
  assert.equal(compactions, 0)
  assert.equal(mod.probe.CS.compacting, false)
  assert.equal(mod.probe.CS.turnRunning, false)
  assert.equal([...w.files.values()].at(-1), replacement, 'Stale preparation cannot replace the new status')
  assert.doesNotMatch(w.state.get('infinite').value.plan ?? '', /^Compacting/)
}
async function lostTimerTest() {
  const mod = modules().register(), w = world(), gate = w.holdFork()
  const original = w.$.process.run
  let fail = false, renewals = 0
  w.$.process.run = async argv => {
    if (argv.includes('renew')) {
      renewals++
      if (fail) {
        if (which === 'throw') throw new Error('The lease helper timed out')
        return { exitCode: which === 'exit' ? 1 : 0, stdout: which === 'malformed' ? 'bad JSON' : '{"held":false}' }
      }
    }
    return original(argv)
  }
  const job = mod.probe.dream(w.$, 100, 6)
  await settle(() => w.forkCalls() === 1)
  const timer = w.timers.find(timer => timer.ms === 60000)
  assert.ok(timer)
  fail = true
  timer.fn()
  await new Promise(resolve => setImmediate(resolve))
  assert.ok(timer.cancelled, 'The first lost renewal must cancel while the fork is still pending')
  const stoppedAt = renewals
  for (let n = 0; n < 10; n++) timer.fn()
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(renewals, stoppedAt)
  gate.resolve()
  await job
  assert.equal(w.records.filter(row => row.kind === 'dream' || row.kind === 'look').length, 0)
  assert.equal(w.state.get('view')?.value?.note?.kind, undefined)
  assert.equal(w.stored.get('seenDreams'), undefined)
}
const cases = { commit: multilineTest, refresh: refreshRejectionTest, button: buttonRejectionTest, turn: supersededPreparationTest,
  holder: lostTimerTest, exit: lostTimerTest, malformed: lostTimerTest, throw: lostTimerTest }
Promise.resolve().then(() => cases[which]()).catch(error => { console.error(error); process.exitCode = 1 })
'''
