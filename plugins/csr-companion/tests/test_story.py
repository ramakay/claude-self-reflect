"""The Shift Map's automatic refresh in the mod (hooks/story.ts gates, register.tsx turn.complete), run with the
installed TypeScript compiler and test_r5's small host. csr-engine is never started: its argv is only recorded."""
import os
import subprocess

from test_r5 import HOST as R5_HOST, ROOT, typescript  # noqa: F401  (typescript is the fixture)


def probe(typescript, name):
    root = os.environ.get('CSR_STORY_SOURCE_ROOT', str(ROOT))
    ran = subprocess.run(['node', '--unhandled-rejections=strict', '-e', HOST, typescript, root, name],
                         capture_output=True, text=True, timeout=30)
    assert ran.returncode == 0, ran.stdout + ran.stderr


def test_story_gates_name_the_first_reason_and_the_numbers_behind_it(typescript):
    probe(typescript, 'gates')


def test_story_refresh_starts_the_engine_once_only_when_opted_in_and_every_gate_passes(typescript):
    probe(typescript, 'refresh')


def test_story_turn_complete_starts_it_after_an_answer_and_never_awaits_it(typescript):
    probe(typescript, 'turn')


def test_story_ink_pane_shows_the_shift_map_in_the_pane_words_and_nothing_without_one(typescript):
    probe(typescript, 'ink')


def test_story_a_second_refresh_waits_for_the_first(typescript):
    probe(typescript, 'concurrent')


def test_story_refresh_fails_closed_when_pane_py_fails_or_is_malformed(typescript):
    probe(typescript, 'failed')


# test_r5's host without its cases, plus hooks/story.ts and the refresh in the probe.
HOST = R5_HOST[:R5_HOST.index('const cases =')].replace(
    'CS, dream, stopSeen,', 'CS, dream, refreshStory, stopSeen,',
).replace(
    "compact: () => load(path.join(root, 'hooks/compact.ts')),",
    "compact: () => load(path.join(root, 'hooks/compact.ts')),\n    story: () => load(path.join(root, 'hooks/story.ts')),",
) + r'''
const NEWS = { story: true, at: 100, messages: 90, messages_since: 60, prompts_since: 3, receipts: 6, calls_24h: 1, usd_24h: 0.01,
  tokens_2h: 0, last_tokens: 1500, liked_2h: 0, transcript: '/t/s1.jsonl' }
const NOW = 1_000_000 * 1000 // ms: well past the telling at 1000 s

function gatesTest() {
  const { decide, messagesDue, allowance, parseNews, refreshArgv } = modules().story()
  assert.equal(decide(NEWS, [], NOW, 0), 'go')
  assert.equal(decide({ story: false }, [], NOW, 0), 'no-story')
  assert.equal(decide({ ...NEWS, at: undefined }, [], NOW, 0), 'no-story')
  assert.equal(decide({ ...NEWS, transcript: null }, [], NOW, 0), 'no-transcript')
  // News: 4 receipts and 2 prompts, both
  assert.equal(decide({ ...NEWS, receipts: 3 }, [], NOW, 0), 'no-news')
  assert.equal(decide({ ...NEWS, prompts_since: 1 }, [], NOW, 0), 'no-news')
  assert.equal(decide({ ...NEWS, receipts: 4, prompts_since: 2, messages_since: 40 }, [], NOW, 0), 'go')
  assert.equal(decide({ ...NEWS, receipts: undefined }, [], NOW, 0), 'no-news')
  // Cooldown: 60 s from the last start, and from the telling itself
  assert.equal(decide(NEWS, [NOW - 59_000], NOW, 0), 'cooldown')
  assert.equal(decide(NEWS, [NOW - 60_000], NOW, 0), 'go')
  assert.equal(decide({ ...NEWS, at: NOW / 1000 - 30 }, [], NOW, 0), 'cooldown')
  // Chapter allowance: 3, one more for each liked shift, 6 at most; starts older than a chapter do not count
  const hour = 60 * 60 * 1000
  assert.deepEqual([0, 1, 2, 3, 4, 9].map(allowance), [3, 4, 5, 6, 6, 6])
  const older = [NOW - 2.5 * hour, NOW - 2.2 * hour, NOW - hour, NOW - 70_000] // two of the four are past the 2 h chapter
  assert.equal(decide({ ...NEWS, messages_since: 80 }, older, NOW, 0), 'go')
  assert.equal(decide({ ...NEWS, messages_since: 79 }, older, NOW, 0), 'spacing', 'one left: the whole 80')
  assert.equal(decide(NEWS, [NOW - hour, NOW - 0.5 * hour, NOW - 70_000], NOW, 0), 'chapter-cap')
  assert.equal(decide({ ...NEWS, liked_2h: 2 }, [NOW - hour, NOW - 0.5 * hour, NOW - 70_000], NOW, 0), 'go')
  // Spacing: 80 usual messages over the refreshes left, scaled by how thin the news is
  assert.equal(messagesDue(4, 3), 40)
  assert.equal(messagesDue(12, 3), 13)
  assert.equal(messagesDue(50, 3), 13, 'news above 12 receipts counts as 12')
  assert.equal(messagesDue(4, 1), 120)
  assert.equal(messagesDue(12, 6), 12, 'never under 12 messages')
  assert.equal(decide({ ...NEWS, receipts: 4, messages_since: 39 }, [], NOW, 0), 'spacing')
  assert.equal(decide({ ...NEWS, receipts: 4, messages_since: 40 }, [], NOW, 0), 'go')
  assert.equal(decide({ ...NEWS, receipts: 4, messages_since: 120 }, [NOW - hour], NOW, 0), 'go')
  assert.equal(decide({ ...NEWS, receipts: 4, messages_since: 59 }, [NOW - hour], NOW, 0), 'spacing', '2 left: 80/2 x 1.5 = 60')
  // Share of Claude's work: 2 tokens in 100, the next call guessed from the last (else 1600)
  assert.equal(decide({ ...NEWS, tokens_2h: 100, last_tokens: 150 }, [], NOW, 10_000), 'share')
  assert.equal(decide({ ...NEWS, tokens_2h: 50, last_tokens: 150 }, [], NOW, 10_000), 'go')
  assert.equal(decide({ ...NEWS, tokens_2h: 0, last_tokens: null }, [], NOW, 80_000), 'go')
  assert.equal(decide({ ...NEWS, tokens_2h: 0, last_tokens: null }, [], NOW, 79_999), 'share', 'no row: the guess is 1600')
  assert.equal(decide({ ...NEWS, tokens_2h: 10 ** 9 }, [], NOW, 0), 'go', 'Claude tokens not counted: the caps stand alone')
  // Day caps
  assert.equal(decide({ ...NEWS, calls_24h: 40 }, [], NOW, 0), 'day-cap')
  assert.equal(decide({ ...NEWS, calls_24h: 39 }, [], NOW, 0), 'go')
  assert.equal(decide({ ...NEWS, usd_24h: 0.1 }, [], NOW, 0), 'day-cap')
  assert.equal(decide({ ...NEWS, usd_24h: null }, [], NOW, 0), 'go', 'an unpriced model: the call cap alone')
  // What pane.py says is checked, not trusted
  assert.deepEqual(parseNews(null), { story: false })
  assert.deepEqual(parseNews('not json'), { story: false })
  assert.deepEqual(parseNews('{"story":"yes"}'), { story: false })
  assert.equal(parseNews(JSON.stringify(NEWS)).receipts, 6)
  assert.deepEqual(refreshArgv('/t/s1.jsonl', '/repo'), ['csr-engine', 'generate-story', '--transcript', '/t/s1.jsonl', '--cwd', '/repo', '--rewrite'])
}

function shifted(w, env, news = NEWS) {
  const { process: original } = w.$
  w.$.env.get = async name => name === 'HOME' ? '/home/test' : env[name]
  w.engineRuns = []
  w.paneCalls = []
  const run = original.run
  w.$.process.run = async (argv, init) => {
    if (argv[0] === 'csr-engine') { w.engineRuns.push(argv); return { exitCode: 0, stdout: '' } }
    if (argv.includes('--story-news')) { w.paneCalls.push(argv); return { exitCode: 0, stdout: JSON.stringify(news) } }
    return run(argv, init)
  }
  return w
}
const ON = { CSR_STORY_REWRITES: '1' }

async function refreshTest() {
  const mod = modules().register()
  // Not opted in: no look at the news, no engine
  let w = shifted(world(), {})
  assert.equal(await mod.probe.refreshStory(w.$), 'off')
  assert.deepEqual([w.paneCalls.length, w.engineRuns.length], [0, 0])
  // The map's kill switch outranks the opt-in
  w = shifted(world(), { ...ON, CSR_NO_STORY_MAP: '1' })
  assert.equal(await mod.probe.refreshStory(w.$), 'off')
  assert.equal(w.engineRuns.length, 0)
  // Opted in, every gate passes: one engine run, this conversation's transcript, and the start is kept
  w = shifted(world(), ON)
  w.advance(0)
  assert.equal(await mod.probe.refreshStory(w.$), 'go')
  await settle(() => w.engineRuns.length === 1)
  assert.deepEqual(w.engineRuns[0], ['csr-engine', 'generate-story', '--transcript', '/t/s1.jsonl', '--cwd', '/repo', '--rewrite'])
  assert.deepEqual(w.paneCalls[0].slice(-5), ['--repo', '/repo', '--story-news', '--sid', 's1'].slice(-5))
  assert.deepEqual(w.stored.get('storyStarts:s1'), [1_000_000])
  // A second turn right after: the cooldown, nothing more starts, nothing more is kept
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(await mod.probe.refreshStory(w.$), 'cooldown')
  assert.equal(w.engineRuns.length, 1)
  assert.deepEqual(w.stored.get('storyStarts:s1'), [1_000_000])
  // A gate that fails starts nothing and keeps nothing
  w = shifted(world(), ON, { ...NEWS, prompts_since: 1 })
  assert.equal(await mod.probe.refreshStory(w.$), 'no-news')
  assert.deepEqual([w.engineRuns.length, w.stored.get('storyStarts:s1')], [0, undefined])
  // The engine failing to start is contained, and the next refresh is not stuck behind it
  w = shifted(world(), ON)
  const run = w.$.process.run
  w.$.process.run = async (argv, init) => { if (argv[0] === 'csr-engine') throw new Error('spawn csr-engine ENOENT'); return run(argv, init) }
  assert.equal(await mod.probe.refreshStory(w.$), 'go')
  await new Promise(resolve => setImmediate(resolve))
  w.advance(5 * 60 * 1000)
  assert.equal(await mod.probe.refreshStory(w.$), 'go', 'the busy mark was cleared')
  await new Promise(resolve => setImmediate(resolve))
  // Claude's counted tokens bound the share: 2 tokens in 100 of 1,000,000 is 20,000
  w = shifted(world(), ON, { ...NEWS, tokens_2h: 19_000, last_tokens: 1500 })
  assert.equal(await mod.probe.refreshStory(w.$), 'share')
  w = shifted(world(), ON, { ...NEWS, tokens_2h: 18_000, last_tokens: 1500 })
  assert.equal(await mod.probe.refreshStory(w.$), 'go')
}

async function turnTest() {
  const mod = modules().register(), hooks = []
  const on = (name, ...args) => { hooks.push({ name, fn: args.at(-1) }); return { catch() {} } }
  mod.register(on, { autoCompact: 'off' })
  const complete = hooks.find(hook => hook.name === 'turn.complete').fn
  let w = shifted(world(), ON)
  w.$.session.surfaces = async () => ['terminal']
  // The engine run never holds the turn: a refresh that does not return still lets turn.complete finish
  const hang = deferred()
  const run = w.$.process.run
  w.$.process.run = async (argv, init) => { if (argv[0] === 'csr-engine') { w.engineRuns.push(argv); await hang.promise; return { exitCode: 0, stdout: '' } } return run(argv, init) }
  let result
  await complete(w.$, { reason: 'answer', answer: 'done', turnId: 't1' }, async () => (result = { ok: true }))
  assert.deepEqual(result, { ok: true })
  await settle(() => w.engineRuns.length === 1)
  // A turn that ended some other way (an interrupt, a tool stop) is no moment for a Haiku call
  const other = shifted(world(), ON)
  other.$.session.surfaces = async () => ['terminal']
  await complete(other.$, { reason: 'interrupt', answer: '', turnId: 't2' }, async () => ({}))
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(other.paneCalls.length + other.engineRuns.length, 0)
  // A subagent's turn is not the person's
  const agent = shifted(world(), ON)
  agent.$.session.surfaces = async () => ['terminal']
  await complete(agent.$, { reason: 'answer', answer: 'report', turnId: 't3', agentId: 'a1' }, async () => ({}))
  await new Promise(resolve => setImmediate(resolve))
  assert.equal(agent.paneCalls.length + agent.engineRuns.length, 0)
  hang.resolve()
}

async function failedTest() {
  const mod = modules().register()
  for (const [name, stdout, exitCode] of [['garbage', 'not json', 0], ['exit', JSON.stringify(NEWS), 1], ['no story', '{"story":false}', 0]]) {
    const w = shifted(world(), ON)
    const run = w.$.process.run
    w.$.process.run = async (argv, init) => argv.includes('--story-news') ? { exitCode, stdout } : run(argv, init)
    const why = await mod.probe.refreshStory(w.$)
    assert.ok(['no-story'].includes(why), `${name}: ${why}`)
    assert.equal(w.engineRuns.length, 0, name)
  }
  const w = shifted(world(), ON)
  const run = w.$.process.run
  w.$.process.run = async (argv, init) => { if (argv.includes('--story-news')) throw new Error('timed out'); return run(argv, init) }
  assert.equal(await mod.probe.refreshStory(w.$), 'no-story', 'a pane.py that times out reads as no news')
  w.$.process.run = run
  assert.equal(await mod.probe.refreshStory(w.$), 'go')
}

async function concurrentTest() {
  const mod = modules().register(), w = shifted(world(), ON), gate = deferred()
  const run = w.$.process.run
  w.$.process.run = async (argv, init) => { if (argv.includes('--story-news')) await gate.promise; return run(argv, init) }
  const first = mod.probe.refreshStory(w.$)
  assert.equal(await mod.probe.refreshStory(w.$), 'busy', 'one refresh at a time')
  gate.resolve()
  assert.equal(await first, 'go')
  await settle(() => w.engineRuns.length === 1)
}

async function inkTest() {
  globalThis.React = { createElement: (type, props, ...children) => ({ type, props, children: children.flat(Infinity) }) }
  const mod = modules().register(), hooks = []
  const on = (name, ...args) => { hooks.push({ name, matcher: args.length === 2 ? args[0] : null, fn: args.at(-1) }); return { catch() {} } }
  mod.register(on, { autoCompact: 'off', dreams: true })
  const render = hooks.find(hook => hook.name === 'ui.render' && hook.matcher?.component === 'Pane').fn
  const words = node => typeof node === 'string' ? [node] : node?.children?.flatMap(words) ?? []
  const draw = async shiftMap => {
    const w = world()
    w.$.ui.resolve = () => ({ Box: 'Box', Button: 'Button', Code: 'Code', Markdown: 'Markdown', Text: 'Text' })
    w.state.set('data', { version: 1, value: { repo: '/repo', total: 0, shifts: [], changes: [], changes_total: 0, dreams: [], dreams_7d: 0,
      dream_tokens_7d: 0, status: null, reminded_7d: 0, dreams_on: true, recalled: 0, git: true, shift_map: shiftMap } })
    return words(await render(w.$, { component: 'Pane', requestId: undefined, props: { bodyColumns: 60 } }, async () => null)).join('\n')
  }
  const text = await draw({ title: 'Shift Map · as of 4 messages ago', rows: ['shift: Changed 5, added 35 symbols in q…', '  changed storage/queries.rs', '  from your edits'] })
  for (const line of ['Shift Map · as of 4 messages ago', 'shift: Changed 5, added 35 symbols in q…', 'changed storage/queries.rs', 'from your edits']) assert.ok(text.includes(line), line)
  for (const gone of ['✓ast', '⌗']) assert.ok(!text.includes(gone), gone)
  for (const none of [null, undefined, { title: 'Shift Map', rows: [] }]) assert.ok(!(await draw(none)).includes('Shift Map'))
}

const cases = { gates: gatesTest, refresh: refreshTest, turn: turnTest, failed: failedTest, concurrent: concurrentTest, ink: inkTest }
Promise.resolve(cases[which]()).catch(error => { console.error(error); process.exit(1) })
'''
