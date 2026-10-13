import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'
import type { On } from 'claude-code'
import {
  actWhy, BAND, COUNTDOWN_MS, decide, endsOnQuestion, KEEP_HEADER, KEEP_MAX, keepList, lastQuestion, modeOf, noticeLine, NOTICE_HEADER,
  paybackPrompts, paybackText, paybackTurns, planText, resumeOf, RESUME_LATE_MS, ruleIn, seamOf, settingOf,
} from '../hooks/compact'
import { askClaude, ownWords, SEND_MOST, stripForwards } from '../hooks/dreamer'

// Compact at the right moment: the owner's band (181k-269k), seams, attended and unattended, the keep list and
// the notice. Every number the plugin acts on below comes from the mined habit data described in hooks/compact.ts.

declare const setTimeout: (fn: () => void, ms: number) => unknown

const NOW = Date.UTC(2026, 9, 12, 12)
const HOME = '/home/u'
const PLAN_FILE = `${HOME}/.claude-self-reflect/csr-companion/compact-plan/sess-1.json`
const BAND_UI = {
  plugin: 'csr-companion',
  surface: 'terminal' as const,
  component: 'AbovePrompt' as const,
  props: { hasSurvey: false, isWorking: false, maxRows: 24, bodyColumns: 100, scroll: { offset: 0, bodyRows: 24 } } as never,
}
const PLAN = { ctx: 214_000, floor: 65_000, measured: true, payback: 8.2, per_turn: 17, worth: true, trend: 20_000, story: null }
const AGENT = { id: 'a427c126', type: 'subagent', status: 'running', description: 'Explore CapCut draft automation' }
const MONITOR = { id: 'bm2f100q', type: 'monitor', status: 'running', description: 'reel build phases' }
const LANDED = 'Done. Codex committed 6369e6b on the study branch; the gates are green.'

type World = {
  ctx: number
  headless?: boolean
  plan?: Record<string, unknown> | null // what pane.py --compact-plan prints
  lost?: unknown[]
  git?: Record<string, string> // what `git -C dir <args>` prints, keyed by the args
  skip?: string // a hook vetoes the compaction
  hold?: (argv: readonly string[]) => Promise<void> | undefined // a process.run that answers only once this settles
  said?: (e: { tool: string; command?: string }) => string // what a tool call answers
  holdHome?: () => Promise<void> | undefined // a HOME lookup that answers only once this settles
}

function world(on: On, w: World) {
  const kept: Array<Record<string, unknown>> = []
  const compacted: Array<string | undefined> = []
  const runs: string[][] = []
  const files = new Map<string, string>()
  const clock = mock.clock(on, { now: NOW })
  const session = mock.session(on)
  const ok = (stdout: string) => ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })
  const fail = { value: { exitCode: 1, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  on('session.id', async () => ({ value: 'sess-1' }))
  on('session.cwd', async () => ({ value: '/repo' }))
  on('session.surfaces', async () => ({ value: w.headless ? [] : ['terminal'] }) as never)
  on('session.usage', async () => ({ value: { startedAt: 0, context: { tokens: w.ctx, window: 1_000_000, percent: Math.round(w.ctx / 10_000) }, rateLimits: [] } }) as never)
  on('env.get', async (_$, e) => {
    if (e.name === 'HOME') await w.holdHome?.()
    return { value: e.name === 'HOME' ? HOME : undefined }
  })
  on('fs.write', async (_$, e) => {
    files.set(e.path, e.text)
    return { value: undefined }
  })
  on('fs.read', async (_$, e) => (files.has(e.path) ? { value: files.get(e.path) ?? '' } : { deny: `ENOENT: ${e.path}` }))
  on('fs.exists', async (_$, e) => ({ value: files.has(e.path) }))
  on('process.run', async (_$, e) => {
    runs.push([...e.argv])
    await w.hold?.(e.argv)
    if (e.argv[0] === 'git') {
      const said = w.git?.[e.argv.slice(3).join(' ')]
      return said === undefined ? fail : ok(said)
    }
    if ((e.argv[2] ?? '').endsWith('/bin/private.py')) { // `private.py write PATH [--redact]`: text on stdin, kept 0600
      files.set(e.argv[4] as string, e.init?.stdin ?? '')
      return ok('')
    }
    if (e.argv.includes('--compact-plan')) return ok(JSON.stringify(w.plan === undefined ? { ...PLAN, ctx: w.ctx } : w.plan))
    if (e.argv.includes('--keep')) {
      kept.push(JSON.parse(e.argv[e.argv.indexOf('--keep') + 1] ?? '{}'))
      return ok('{}')
    }
    if (e.argv.includes('--lost')) return ok(JSON.stringify(w.lost ?? []))
    return ok('{}')
  })
  // The conversation the engine would compact: the kit asks the session for it.
  const MESSAGES = [{ role: 'user', text: 'one', toolUses: [] }, { role: 'assistant', text: 'done', toolUses: [] }]
  on('session.messages', async () => ({ value: MESSAGES }) as never)
  on('session.compact', async (_$, e) => {
    compacted.push(e.instructions)
    if (w.skip) return { skip: w.skip } as never
    return { messages: e.messages?.length ? e.messages.slice(-1) : MESSAGES.slice(-1), tokensBefore: w.ctx, tokensAfter: 21_000 } as never
  })
  on('classic.Stop', async () => ({}) as never)
  on('tool.call', async (_$, e) => { const said = w.said?.(e as never) ?? ''; return ({ result: said, text: said }) as never })
  on('turn.start', async (_$, e) => ({ turnId: e.turnId }))
  on('turn.complete', async (_$, e) => ({ text: e.answer }))
  on('prompt.submit', async (_$, e) => ({ text: e.text }))
  on('prompt.edit', async (_$, e) => ({ text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end), cursor: e.start + e.inputText.length }))
  on('ui.render', async ($, e) => {
    const { Box } = $.ui.resolve(e)
    return h(Box, {}) as never
  })
  const compactsLogged = () => kept.filter(record => record.kind === 'compact')
  const plan = () => {
    const text = files.get(PLAN_FILE)
    return text ? (JSON.parse(text) as { state: string; text: string | null }) : null
  }
  const appended = () => session.appended().map(row => row.message.content.map(block => ('text' in block ? block.text : '')).join(''))
  return { kept, compacted, runs, files, clock, compactsLogged, plan, appended }
}

async function settle(done: () => boolean) {
  for (let i = 0; i < 200 && !done(); i++) await new Promise<void>(resolve => setTimeout(() => resolve(), 5))
}

// One main-loop turn: it starts, the Stop lists the background work in flight, it ends with this answer.
let turns = 0
async function turn($: Engine, answer: string, running: unknown[] = [], agentId?: string) {
  const turnId = `turn-${++turns}`
  if (!agentId) await $.turn.start({ text: '', turnId } as never)
  if (!agentId) await $.classic.Stop({ stop_hook_active: false, background_tasks: running } as never)
  await $.turn.complete({ answer, durationMs: 1000, isAborted: false, turnId, reason: 'answer', ...(agentId ? { agentId } : {}) } as never)
}
// prompt.edit raises at run time; the kit's typings leave it off the prompt noun.
const edit = ($: Engine, e: Record<string, unknown>) => ($.prompt as unknown as { edit: (e: unknown) => Promise<unknown> }).edit(e)
const typed = ($: Engine, text = 'go on') => $.prompt.submit({ text, origin: { kind: 'composer' } } as never)
const fromPhone = ($: Engine, text = 'keep going') => $.prompt.submit({ text, origin: { kind: 'bridge' } } as never)

// The tests that drive the engine are about auto, which a person opts into; suggest and off say so themselves.
const AUTO = { options: { autoCompact: 'auto' } }

// ---------------------------------------------------------------- the rules, without an engine

test('the defaults are the owner\'s mined habit: 181k to 269k, a 20 s countdown; compacting on its own is opt-in', () => {
  expect(BAND).toEqual({ low: 180_589, high: 269_178, drift: 407_443 })
  expect(COUNTDOWN_MS).toBe(20_000)
  expect(settingOf(undefined)).toBe('suggest') // a person who never chose is offered a compaction, never given one
  expect(settingOf('auto')).toBe('auto')
  expect(settingOf('off')).toBe('off')
  expect(settingOf('sometimes')).toBe('suggest')
})

test('mode: a prompt typed at the terminal in the last 10 minutes is attended; the phone or 10 minutes of quiet is not', () => {
  const loaded = NOW - 60_000
  expect(modeOf({ kind: 'composer', at: NOW - 9 * 60_000 }, NOW, loaded)).toBe('attended')
  expect(modeOf({ kind: 'composer', at: NOW - 10 * 60_000 }, NOW, loaded)).toBe('unattended')
  expect(modeOf({ kind: 'bridge', at: NOW - 1000 }, NOW, loaded)).toBe('unattended')
  expect(modeOf(null, NOW, loaded)).toBe('attended') // just loaded: someone may be there
  expect(modeOf(null, NOW, NOW - 11 * 60_000)).toBe('unattended')
})

test('a seam: nothing running in the background (a monitor aside) and no question; the strongest sign is its why', () => {
  const end = { ctx: 214_000, answer: LANDED, asking: false, running: [] }
  expect(seamOf(end)).toEqual({ isSeam: true, why: 'commit 6369e6b landed', question: false, commit: '6369e6b', running: 0 })
  expect(seamOf({ ...end, answer: 'All 81 tests pass.' }).why).toBe('gates passed')
  expect(seamOf({ ...end, answer: 'Updated the pane.', edits: 2 }).why).toBe('2 edits saved')
  expect(seamOf({ ...end, answer: 'Read it.' }).why).toBe('nothing running in the background')
  expect(seamOf({ ...end, running: [MONITOR] }).isSeam).toBe(true)
  expect(seamOf({ ...end, running: [AGENT] })).toEqual(expect.objectContaining({ isSeam: false, wait: '1 agent running' }))
  expect(seamOf({ ...end, running: null })).toEqual(expect.objectContaining({ isSeam: false, wait: 'background work unknown' }))
  expect(seamOf({ ...end, answer: 'Merge it now?' })).toEqual(expect.objectContaining({ isSeam: false, question: true, wait: 'your answer' }))
  expect(seamOf({ ...end, asking: true })).toEqual(expect.objectContaining({ isSeam: false, question: true }))
})

// 10-12 20:53: the row read "Compacting · 278k · nothing running in the background" over the running Fable agent.
test('why it acts: at a seam its strongest sign; past the edge without one, the edge and what it compacts over', () => {
  const end = { ctx: 278_000, answer: 'Still going.', asking: false, running: [{ id: 'a95e', type: 'subagent', status: 'running', description: 'Fable' }] }
  expect(actWhy(seamOf(end as never))).toBe('over 269k · 1 agent kept')
  expect(actWhy(seamOf({ ...end, running: [] } as never))).toBe('nothing running in the background')
  expect(actWhy(seamOf({ ...end, running: [], answer: 'Push it?' } as never))).toBe('over 269k · question kept')
  expect(actWhy(seamOf({ ...end, running: null } as never))).toBe('over 269k · background work unknown')
})

test('a question is the last character, markdown and quotes aside; its last sentence is what is kept', () => {
  expect(endsOnQuestion('Should I install it now?**')).toBe(true)
  expect(endsOnQuestion('Done. (Was that the plan?)')).toBe(true)
  expect(endsOnQuestion('Is it? Yes, it is.')).toBe(false)
  expect(lastQuestion('Both gates pass. Should I push the **study branch**?')).toBe('Should I push the study branch?')
  expect(lastQuestion('Done.')).toBeNull()
})

test('decide: nothing below the band or when it would not pay back; a plan until a seam; forced past the high edge', () => {
  const seam = seamOf({ ctx: 0, answer: 'ok', asking: false, running: [] })
  const busy = seamOf({ ctx: 0, answer: 'ok', asking: false, running: [AGENT] })
  const asked = seamOf({ ctx: 0, answer: 'ok?', asking: false, running: [] })
  const base = { setting: 'auto' as const, mode: 'attended' as const, payback: { requests: 8, perTurn: null }, seam }
  expect(decide({ ...base, ctx: 150_000 }).act).toBe('none')
  expect(decide({ ...base, ctx: 214_000, payback: null }).act).toBe('none')
  // the gate is in turns: 16 requests unmeasured are taken as 16 turns; at 17 requests a turn, 300 are 17.6
  expect(decide({ ...base, ctx: 214_000, payback: { requests: 16, perTurn: null } }).act).toBe('none')
  expect(decide({ ...base, ctx: 214_000, payback: { requests: 300, perTurn: 17 } }).act).toBe('none')
  expect(decide({ ...base, ctx: 214_000, payback: { requests: 22, perTurn: 17 } }).act).toBe('countdown')
  expect(decide({ ...base, ctx: 214_000, seam: busy })).toEqual({ act: 'plan', wait: '1 agent running', forced: false })
  expect(decide({ ...base, ctx: 214_000 }).act).toBe('countdown')
  expect(decide({ ...base, ctx: 214_000, mode: 'unattended' }).act).toBe('compact')
  expect(decide({ ...base, ctx: 280_000, seam: busy })).toEqual({ act: 'countdown', forced: true })
  expect(decide({ ...base, ctx: 280_000, seam: asked }).act).toBe('plan')
  expect(decide({ ...base, ctx: 230_000, seam: asked, mode: 'unattended' }).act).toBe('plan')
  expect(decide({ ...base, ctx: 280_000, seam: asked, mode: 'unattended' })).toEqual({ act: 'compact', forced: true })
  expect(decide({ ...base, ctx: 224_000, cancelledAt: 214_000 })).toEqual({ act: 'plan', wait: 'cancelled · waits for 234k', forced: false })
  expect(decide({ ...base, ctx: 234_000, cancelledAt: 214_000 }).act).toBe('countdown')
  expect(decide({ ...base, ctx: 214_000, setting: 'suggest' }).act).toBe('offer')
  expect(decide({ ...base, ctx: 300_000, setting: 'off' }).act).toBe('none')
  // pane.py did not answer: in the band nothing acts; past the high edge the owner's own limit is enough
  expect(decide({ ...base, ctx: 214_000, payback: undefined }).act).toBe('none')
  expect(decide({ ...base, ctx: 280_000, payback: undefined, mode: 'unattended' })).toEqual({ act: 'compact', forced: true })
})

test('the keep list: header first, the owner\'s order, under 600 characters however much there is', () => {
  const small = keepList({ running: [{ id: 'a427c126', type: 'subagent', description: 'Explore CapCut' }], decisions: ['Push the study branch?'], work: ['branch study/x at ea7d83f', '2 uncommitted files (a.ts, b.py)'], rules: ['never use --no-verify'] })
  expect(small.startsWith(KEEP_HEADER)).toBe(true)
  expect(small.length).toBeLessThanOrEqual(KEEP_MAX)
  const order = ['Running in the background: subagent a427c126', 'Waiting on the owner: Push the study branch?', 'Unfinished work: branch study/x at ea7d83f', "The owner's rules this session: never use --no-verify"]
  const at = order.map(part => small.indexOf(part))
  expect(at.every(i => i > 0)).toBe(true)
  expect([...at].sort((a, b) => a - b)).toEqual(at)
  const long = 'x'.repeat(900)
  const big = keepList({ running: Array.from({ length: 9 }, (_, i) => ({ id: `agent-${i}`, type: 'subagent', description: long })), decisions: [long, long], work: [long], rules: [long, long, long] })
  expect(big.startsWith(KEEP_HEADER)).toBe(true)
  expect(big.length).toBeLessThanOrEqual(KEEP_MAX)
  expect(big).toContain('1. Running in the background: subagent agent-0')
  expect(big).toContain('4. The owner')
  const none = keepList({ running: [], decisions: [], work: [], rules: [] })
  expect(none.startsWith(KEEP_HEADER)).toBe(true)
})

// 10-13 04:50, this session's own compaction: (2) the fair-share cut ended mid-description, "bpd9kf6h3 (real scrub on
// scratch clone…", and the live subagent a66d38d8d43ee1dad after it was lost. Ids first, never cut.
test('every running id is in the keep list whole; descriptions get only the room the ids leave', () => {
  const long = 'real scrub on a scratch clone of the database, then the gates, then the report '.repeat(4)
  const running = [['shell', 'b02336hzp'], ['shell', 'bpd9kf6h3'], ['subagent', 'a66d38d8d43ee1dad'], ['subagent', 'a5f814bf6883609c3'], ['workflow', 'wf_88a43643']]
    .map(([type, id]) => ({ type: type as string, id: id as string, description: long }))
  const keep = keepList({ running, decisions: [long, long], work: [long], rules: [long, long] })
  expect(keep.length).toBeLessThanOrEqual(KEEP_MAX)
  for (const task of running) expect(keep).toMatch(new RegExp(`\\b${task.id}\\b`)) // the type word is said once per group
  expect(keep.split('\n')[1]?.startsWith('1. Running in the background: shell b02336hzp')).toBe(true)
  expect(keep).toContain('Waiting on the owner:') // the other sections still get the rest
  const many = Array.from({ length: 30 }, (_, i) => ({ type: 'subagent', id: `a${String(i).padStart(16, '0')}`, description: long }))
  const crowded = keepList({ running: many, decisions: [long], work: [], rules: [] })
  for (const task of many) expect(crowded).toContain(task.id) // past KEEP_MAX the ids still go in whole
})

test('the notice opens with the header csr-engine\'s guard holds, and says what was kept', () => {
  const line = noticeLine(312_000, 66_000, { requests: 13.2, perTurn: 17.2 }, { running: [{ id: 'a', type: 'subagent', description: '' }, { id: 'b', type: 'subagent', description: '' }], decisions: ['x?'], work: [], rules: [] })
  expect(line).toBe('CSR compacted at 312k → 66k · pays back in ~13 API calls (~1 prompt) · kept: 2 running agents, 1 open decision')
  expect(line.startsWith(NOTICE_HEADER)).toBe(true)
  expect(planText(214_000, { requests: 4, perTurn: null }, undefined, false)).toBe('Compact planned · 214k of your 181k–269k band · pays back in ~4 API calls · waiting for a seam')
})

// 10-12: "2 of 14 requests to pay back, no clue still": API calls, then the prompts they take, rounded up.
test('the payback is said in API calls and in your prompts, as pane.py says it', () => {
  expect(paybackText(null)).toBe('')
  expect(paybackText({ requests: 13.2, perTurn: 17 })).toBe('pays back in ~13 API calls (~1 prompt)')
  expect(paybackText({ requests: 13.6, perTurn: 8.9 })).toBe('pays back in ~14 API calls (~2 prompts)')
  expect(paybackText({ requests: 52, perTurn: 17 })).toBe('pays back in ~52 API calls (~4 prompts)')
  expect(paybackText({ requests: 1, perTurn: null })).toBe('pays back in ~1 API call')
  expect(paybackText({ requests: 0.4, perTurn: 5 })).toBe('pays back in ~1 API call (~1 prompt)')
  expect(paybackPrompts({ requests: 14, perTurn: 14 })).toBe(1)
  expect(paybackPrompts({ requests: 14.1, perTurn: 14 })).toBe(2) // a prompt under way counts whole
  expect(paybackPrompts({ requests: 14, perTurn: null })).toBe(null)
  expect(paybackTurns({ requests: 34, perTurn: 17 })).toBe(2)
  expect(paybackTurns({ requests: 34, perTurn: null })).toBe(34) // unmeasured: a request taken as a turn
})

test('a binding sentence of the owner\'s is kept as a rule; other prompts are not', () => {
  expect(ruleIn('Looks good. Never push to release/v* from here. Thanks')).toBe('Never push to release/v* from here.')
  expect(ruleIn('ship it')).toBeNull()
})

// Codex review 10-12, #12: a forwarded dream's quoted reasoning "We must disable retries before migrating." became
// "The owner's rules this session: > We must disable retries before migrating." in the summary's keep list.
const FORWARD = askClaude({ line: 'What if we migrated the queue first?', title: 'Migrate the queue first',
  text: '* We must disable retries before migrating.\n* Never run both queues at once.' })
test("what CSR or a side agent wrote and the person forwarded is never one of the owner's rules; their own words are", () => {
  expect(ruleIn(FORWARD)).toBeNull()
  expect(ruleIn(`${FORWARD}\n\nNo. Never migrate on a Friday.`)).toBe('Never migrate on a Friday.')
  expect(ruleIn("CSR's dreamer asked this while you worked: “What if we always cached it?” Its reasoning: We must cache.")).toBeNull()
  expect(ruleIn('Here is a note offered by a side agent:\n> You must not push from the phone.')).toBeNull()
  expect(ruleIn("CSR's compaction card (compacted by csr): Context: 274k → 70k tokens. Learnt: Never skip validation.")).toBeNull()
  expect(ruleIn('> Never trust this quote.\nAlways run the gates.')).toBe('Always run the gates.')
  const keep = keepList({ running: [], decisions: [], work: [], rules: ['> We must disable retries before migrating.', 'Always run the gates.'] })
  expect(keep).toContain("The owner's rules this session: Always run the gates.")
  expect(keep).not.toContain('disable retries')
})

// Codex r2 #6 #7 #8: a sentence about the card or opening with the dreamer's words is the person's, and every
// forward in a prompt goes while the words between and after them stay. The forms are the ones pane.py and
// dreamer.ts write (compact_section, dream_send, askClaude), as csr-engine reads them (strip_csr_forwards).
test("forwards are read by the grammar that writes them: talk about them is the owner's, every one of them goes", () => {
  expect(ruleIn("CSR's compaction card (compacted by csr) is wrong; never drop my pending task."))
    .toBe("CSR's compaction card (compacted by csr) is wrong; never drop my pending task.")
  expect(ruleIn("Please change CSR's compaction card (the one in the sidebar). Never round the tokens.")).toBe('Never round the tokens.')
  expect(ruleIn("CSR's dreamer asked this while you worked yesterday, but I must write today's question myself."))
    .toBe("CSR's dreamer asked this while you worked yesterday, but I must write today's question myself.")
  const CARD = "CSR's compaction card (compacted by csr): Context: 274k → 70k tokens. Learnt: Never skip validation."
  expect(ruleIn(`${CARD}\nNever compact on a Friday.`)).toBe('Never compact on a Friday.') // one line: the next is yours
  expect(ruleIn(`2${CARD}`)).toBeNull() // a band hotkey before it is no word of yours
  for (const stage of ['compacting soon', 'compacting', 'compaction skipped']) expect(ruleIn(`CSR's compaction card (${stage}): Never mind.`)).toBeNull()
  expect(ruleIn("CSR's story of this session, rev 3: We must never ship on Fridays.")).toBeNull()
  const PANE = "CSR's dreamer asked this while you worked: “What if we always cached it?” Its reasoning: We must cache."
  expect(ruleIn(`Look at this: ${PANE}\nNever cache secrets.`)).toBe('Never cache secrets.')
  // the second forward, and the words between and after the two
  expect(ruleIn(`${FORWARD}\n\nNo, that is fine.\n\n${FORWARD}`)).toBeNull()
  expect(ruleIn(`${FORWARD}\n\nNo, that is fine.\n\n${FORWARD}\n\nAlways ask before migrating.`)).toBe('Always ask before migrating.')
  expect(ruleIn('Here is a note offered by a side agent:\n\n> You should know · You must not push from the phone.\n\nAlways push from the laptop.'))
    .toBe('Always push from the laptop.')
})

// astra #4 #9 #10 (the 10.x twin's review, the same parser here): a one-line send ate the reply typed after it on its
// line; a card after the person's words on its line was kept whole; a band forward sharing a paragraph with a line of
// theirs kept its question or reasoning.
test('a send is a span of its line, and a band forward goes line by line', () => {
  const head = "CSR's dreamer asked this while you worked: “What if?” Its reasoning: "
  const cut = `${head}${'x'.repeat(699 - Array.from(head).length)}…` // v1, cut by one_line at 700
  expect(Array.from(cut).length).toBe(SEND_MOST.pane)
  expect(ownWords(`${cut} Keep the deployment frozen.`)).toBe('Keep the deployment frozen.')
  expect(ruleIn(`${cut} Never deploy on a Friday.`)).toBe('Never deploy on a Friday.')
  expect(ownWords("Please check this: CSR's compaction card (compacting): Summarizing 278k.")).toBe('Please check this:')
  const band = "What if we reuse the palette?\n\nCSR's dreamer asked this while you worked. Its reasoning:\n\n> **Reuse**\n> Shared colours."
  expect(ownWords(`Keep the deployment frozen.\n${band}`)).toBe('Keep the deployment frozen.')
  expect(stripForwards(`${band}\nNo, keep the current palette.`)).toBe('No, keep the current palette.')
})

// Forward grammar v2, its required tests on this side; test_pane.py emits the
// same forms from pane.py's real emitters. astra r3 #5: a closing quote was searched past a known cut; #6: a v1 body
// opening on a quoted phrase read as a closed envelope; #7: the band's question took the person's words on its line.
test('forward grammar v2: each kind round-trips to exactly the reply; v1 is never searched for a closing quote', () => {
  const pane = "CSR's dreamer asked this while you worked, quoted: “What if the pane said \"fresh\" first?” Its reasoning: “Say \"fresh\". It reads oddly.”"
  const card = "CSR's compaction card (compacted by csr), quoted: “Context: 274k → 70k tokens. Learnt: Never skip validation.”"
  const story = "CSR's story of this session, rev 2, quoted: “We moved the \"queue\".”"
  for (const sent of [pane, card, story]) {
    expect(ownWords(`Before: ${sent} after, keep it frozen.`)).toBe('Before: after, keep it frozen.')
    expect(ownWords(`2${sent}`)).toBe('')
    expect(ownWords(`${sent}\nmine`)).toBe('mine')
  }
  expect(ruleIn(`${card} Never compact on a Friday.`)).toBe('Never compact on a Friday.')
  const long = `CSR's compaction card (compacting), quoted: “${'y'.repeat(1200 - 47)}…”`
  expect(Array.from(long).length).toBe(SEND_MOST.card)
  expect(ownWords(`${long} mine`)).toBe('mine')
  expect(ownWords("CSR's compaction card (compacting), quoted: “カードの本文。”　次は？")).toBe('次は？') // CJK body
  // v2 band: words before the question on its line are the person's; a question with its own quotes round-trips
  for (const line of ['What if we reuse the palette?', 'What if “Queue” moved?']) {
    const fill = askClaude({ line, title: 'Reuse “it”', text: '* Shared “colours”.' })
    expect(stripForwards(`Keep the deployment frozen. ${fill}\nNo, keep the current palette.`)).toBe('Keep the deployment frozen.\n\nNo, keep the current palette.')
    expect(stripForwards(`${fill}\n\nNo.`)).toBe('No.')
    expect(stripForwards(`2${fill}`)).toBe('')
  }
  // v1 (header then ':'), each kind: cut to one_line's cut or the end of its line, whatever quotes its body opens on
  expect(ownWords("CSR's compaction card (compacting): “Queue” was moved. Never skip validation.")).toBe('')
  expect(ruleIn("CSR's compaction card (compacting): “Queue” was moved. Never skip validation.")).toBeNull()
  expect(ownWords("CSR's dreamer asked this while you worked: “What if?” Its reasoning: “Queue” moved. Never wait.\nmine")).toBe('mine')
  expect(ownWords("CSR's story of this session, rev 3: “Queue” moved. Never wait.")).toBe('')
  expect(ownWords("CSR's compaction card (compacting): 「キュー」が移動。")).toBe('')
  const v1 = "CSR's dreamer asked this while you worked: “What if?” Its reasoning: "
  const at700 = `${v1}“${'x'.repeat(698 - Array.from(v1).length)}…` // astra r3 #5: exactly 700, cut
  expect(Array.from(at700).length).toBe(700)
  expect(ownWords(`${at700} Keep “my words” exactly.`)).toBe('Keep “my words” exactly.')
  const card1200 = `CSR's compaction card (compacting): ${'z'.repeat(1199 - 36)}…`
  expect(Array.from(card1200).length).toBe(1200)
  expect(ownWords(`${card1200} Keep “my words” exactly.`)).toBe('Keep “my words” exactly.')
})

// Codex r2 #10: "Please explain this example:" over a fenced `# Never run validation in this sample` became "The
// owner's rules this session"; so did the same sentence in a <pasted_content> block left open.
test("a code example, or a pasted or injected block left open, is never one of the owner's rules", () => {
  expect(ruleIn('Please explain this example:\n```sh\n# Never run validation in this sample\n```')).toBeNull()
  expect(ruleIn('Please explain this example:\n~~~\n# Never run validation in this sample\n~~~')).toBeNull()
  expect(ruleIn('Look:\n````md\n```\nnever nest these\n```\n````')).toBeNull() // a longer fence holds a shorter one
  expect(ruleIn('Look at this:\n```\n# You must always pass --force here\n')).toBeNull() // never closed: to the end
  expect(ruleIn('```js\n// never\n```\nNever ship on a Friday.')).toBe('Never ship on a Friday.')
  expect(ruleIn('Please explain this example: <pasted_content id="1">Never run validation in this sample.')).toBeNull()
  expect(ruleIn('ok <system-reminder>You must always answer in French.')).toBeNull()
  expect(ruleIn('<pasted_content id="1">x</pasted_content> Always run the gates.')).toBe('Always run the gates.')
  const keep = keepList({ running: [], decisions: [], work: [], rules: [ruleIn('Please explain this example:\n```sh\n# Never run validation in this sample\n```') ?? ''] })
  expect(keep).not.toContain("The owner's rules")
})

test("a forwarded dream's reasoning never reaches the summary as the owner's rule", AUTO, async ($, on) => {
  const seen = world(on, { ctx: 300_000 })
  await typed($, FORWARD)
  await fromPhone($)
  await turn($, 'Still going.', [AGENT])
  expect(seen.compacted.length).toBe(1)
  expect(seen.compacted[0]).not.toContain('disable retries')
  expect(seen.compacted[0]).not.toContain("The owner's rules")
})

// ---------------------------------------------------------------- the hooks, against the engine

test('below the band: no plan, no pane.py, no band, no compaction', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 150_000 })
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  expect(seen.runs.some(argv => argv.includes('--compact-plan'))).toBe(false)
  expect(seen.compactsLogged()).toEqual([])
  expect(seen.compacted).toEqual([])
  expect(seen.plan()).toBeNull()
  expect(await band.find({ text: /Compact/ })).toBeUndefined()
})

test('in the band, not at a seam: the plan row says what it waits for, and nothing acts', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, 'Launched the review agent.', [AGENT])
  expect(seen.plan()).toEqual(expect.objectContaining({ state: 'planned', text: 'Compact planned · 214k of your 181k–269k band · pays back in ~8 API calls (~1 prompt) · waiting: 1 agent running' }))
  expect(seen.runs.find(argv => argv.includes('--compact-plan'))).toEqual(expect.arrayContaining(['--sid', 'sess-1', '--ctx', '214000']))
  expect(seen.compactsLogged()).toEqual([expect.objectContaining({ event: 'plan', act: 'plan', mode: 'attended', ctx_before: 214_000, payback: 8.2, per_turn: 17, wait: '1 agent running' })])
  expect(seen.compactsLogged()[0]?.seam).toEqual(expect.objectContaining({ is: false, running: 1 }))
  expect(seen.compacted).toEqual([])
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
})

test('a compaction that would not pay back within 15 turns is not planned', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000, plan: { ...PLAN, payback: 300 } }) // 300 requests at 17 a turn: 17.6 turns
  await typed($)
  await turn($, LANDED)
  expect(seen.plan()).toBeNull()
  expect(seen.compacted).toEqual([])
})

test('attended, at a seam: a 20 s countdown with Compact now and Cancel, then the compaction with the keep list', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000, git: { 'rev-parse --show-toplevel': '/repo', 'status --porcelain': ' M hooks/compact.ts\n?? bin/x.py', 'log --format=%h @{u}..HEAD': 'ea7d83f', 'rev-parse --short HEAD': 'ea7d83f', 'symbolic-ref -q --short HEAD': 'study/dreamer' },
    said: e => (e.command?.startsWith('git commit') ? '[study/dreamer 6369e6b] wip' : '') })
  const band = await $.ui.mount(BAND_UI)
  for (const file of ['/repo/hooks/compact.ts', '/repo/bin/x.py']) await $.tool.call({ tool: 'Write', file_path: file, content: 'x', tool_use_id: file } as never)
  await $.tool.call({ tool: 'Bash', command: 'git commit -m wip', tool_use_id: 'commit-1' } as never) // this session committed here
  await typed($, 'Never use --no-verify here. Carry on with the plan.')
  await turn($, LANDED)
  expect(await band.find({ text: 'Compacting in 20s · 214k · commit 6369e6b landed' })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Compact now' })).toBeDefined()
  expect(await band.find({ type: 'Button', text: 'Cancel' })).toBeDefined()
  expect(seen.plan()?.state).toBe('countdown')
  expect(seen.compacted).toEqual([])

  await seen.clock.advance(COUNTDOWN_MS)
  await settle(() => seen.compactsLogged().some(record => record.event === 'compact'))
  expect(seen.compacted.length).toBe(1)
  const instructions = seen.compacted[0] ?? ''
  expect(instructions.startsWith(KEEP_HEADER)).toBe(true)
  expect(instructions.length).toBeLessThanOrEqual(KEEP_MAX)
  expect(instructions).toContain('branch study/dreamer at ea7d83f')
  expect(instructions).toContain('2 uncommitted files (hooks/compact.ts, bin/x.py)')
  expect(instructions).toContain('not pushed: ea7d83f')
  expect(instructions).toContain('Never use --no-verify here.')
  expect(seen.compactsLogged().map(record => record.event)).toEqual(['plan', 'countdown', 'start', 'compact'])
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({
    by: 'countdown', mode: 'attended', outcome: 'compacted', ctx_before: 214_000, ctx_after: 21_000, floor: 65_000, payback: 8.2, per_turn: 17,
  }))
  expect(typeof seen.compactsLogged().at(-1)?.took_ms).toBe('number')
  expect(seen.compactsLogged().at(-1)?.seam).toEqual(expect.objectContaining({ is: true, why: 'commit 6369e6b landed', commit: '6369e6b' }))
  expect(seen.appended()).toEqual(['CSR compacted at 214k → 65k · pays back in ~8 API calls (~1 prompt) · kept: unfinished work'])
  expect(seen.plan()?.state).toBe('none')
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
})

test('0 cancels: no compaction, and at the same context it keeps waiting for 20k more', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  await band.press({ key: 'compact-cancel' })
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  await seen.clock.advance(COUNTDOWN_MS)
  expect(seen.compacted).toEqual([])
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'cancel', by: 'press', ctx_before: 214_000 }))
  expect(seen.plan()?.text).toContain('cancelled · waits for 234k')

  await turn($, LANDED) // the same context: still waiting
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  expect(seen.plan()?.text).toContain('cancelled · waits for 234k')
})

test('1 on the band compacts at once, before the countdown runs out', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  await band.press({ key: 'compact-now' })
  await settle(() => seen.compacted.length > 0)
  expect(seen.compacted.length).toBe(1)
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'compact', by: 'press' }))
  await seen.clock.advance(COUNTDOWN_MS)
  expect(seen.compacted.length).toBe(1) // the countdown went with it
})

test('typing or sending a prompt cancels the countdown; a lone digit for the band does not', AUTO, async ($, on) => {
  const w: World = { ctx: 214_000 }
  const seen = world(on, w)
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  await edit($, { origin: { kind: 'composer' }, text: '', cursor: 0, start: 0, end: 0, inputText: '1', key: { key: '1' } })
  expect(await band.find({ text: /Compacting in/ })).toBeDefined() // a band hotkey on its way
  await edit($, { origin: { kind: 'composer' }, text: '', cursor: 0, start: 0, end: 0, inputText: 'w', key: { key: 'w' } })
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'cancel', by: 'typing' }))

  w.ctx = 224_000 // 10k more: not enough after a cancel
  await turn($, LANDED)
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  w.ctx = 236_000 // 22k more, at a seam: armed again
  await turn($, LANDED)
  expect(await band.find({ text: /Compacting in 20s · 236k/ })).toBeDefined()
  await typed($, 'one more thing')
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'cancel', by: 'prompt', ctx_before: 236_000 }))
  await seen.clock.advance(COUNTDOWN_MS)
  expect(seen.compacted).toEqual([])
})

test('a turn that starts during the countdown ends it; the next turn end decides again', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  await $.turn.start({ text: '', turnId: 'tn-1' } as never)
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'skip', reason: 'a turn started' }))
  await seen.clock.advance(COUNTDOWN_MS)
  expect(seen.compacted).toEqual([])
})

test('past the high edge it acts at the next turn end, seam or not: a countdown attended, at once unattended', AUTO, async ($, on) => {
  const w: World = { ctx: 280_000 }
  const seen = world(on, w)
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, 'Started two agents.', [AGENT])
  expect(await band.find({ text: /Compacting in 20s · 280k/ })).toBeDefined()
  expect(seen.compactsLogged().find(record => record.event === 'countdown')).toEqual(expect.objectContaining({ forced: true }))
  await band.press({ key: 'compact-cancel' })

  await fromPhone($)
  await turn($, 'Still running.', [AGENT]) // cancelled at 280k: past the edge too, it waits for 20k more
  expect(seen.compacted).toEqual([])
  w.ctx = 302_000
  await turn($, 'Still running.', [AGENT])
  expect(seen.compacted.length).toBe(1)
  expect(seen.compacted[0]).toContain('Running in the background: subagent a427c126 (Explore CapCut draft automation)')
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'compact', by: 'unattended', mode: 'unattended', forced: true }))
})

test('a question waits for the owner; unattended past the high edge it no longer holds, and is kept', AUTO, async ($, on) => {
  const w: World = { ctx: 280_000 }
  const seen = world(on, w)
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, 'Both gates pass. Should I push the study branch?')
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  expect(seen.plan()?.text).toContain('waiting: your answer')

  await fromPhone($)
  w.ctx = 230_000 // in the band, from the phone: a question still holds
  await turn($, 'Should I push the study branch?')
  expect(seen.compacted).toEqual([])
  w.ctx = 290_000 // past the edge: it compacts, and the question is kept for the owner
  await turn($, 'Should I push the study branch?')
  expect(seen.compacted.length).toBe(1)
  expect(seen.compacted[0]).toContain('Waiting on the owner: Should I push the study branch?')
  expect(seen.appended()[0]).toContain('kept: 1 open decision')
})

test('unattended at a seam in the band: it compacts at once, no countdown, and leaves one line the phone shows', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  const band = await $.ui.mount(BAND_UI)
  await fromPhone($)
  await turn($, LANDED)
  expect(seen.compacted.length).toBe(1)
  expect(seen.compactsLogged().map(record => record.event)).toEqual(['plan', 'start', 'compact'])
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  const notice = seen.appended()
  expect(notice.length).toBe(1)
  expect(notice[0]?.startsWith(NOTICE_HEADER)).toBe(true)
  expect(notice[0]).toBe('CSR compacted at 214k → 65k · pays back in ~8 API calls (~1 prompt)')
})

// 10-12 21:17: when CSR compacts while you are away, the line the phone shows says what waits for you in Lost and
// found, so the threads stay out of the summary and you still know to look.
test('the notice counts the threads waiting on you in Lost and found; set aside, quiet or answered ones are not', AUTO, async ($, on) => {
  const lost = [
    { text: "Wait for agent ab763f58e5be44c02's report.", state: 'lost', waiting: true, hidden: null, later: false, rank: 0 },
    { text: 'Push the study branch when you say go.', state: 'open', waiting: true, hidden: null, later: false, rank: 2 },
    { text: 'Approve the install?', state: 'lost', waiting: true, hidden: 'later', later: true, rank: 0 },
    { text: 'Say go on the guard.', state: 'answered', waiting: true, hidden: null, later: false, rank: 4 },
    { text: 'Try the cache countdown in the masthead.', state: 'lost', waiting: false, hidden: null, later: false, rank: 1 },
  ]
  const seen = world(on, { ctx: 300_000, lost })
  await fromPhone($)
  await turn($, 'Still going.', [AGENT])
  expect(seen.appended()).toEqual(['CSR compacted at 300k → 65k · pays back in ~8 API calls (~1 prompt) · kept: 1 running agent · 2 waiting in Lost and found'])
  expect(seen.compactsLogged().find(record => record.event === 'compact')).toEqual(expect.objectContaining({ lost_waiting: 2 }))
  expect(noticeLine(300_000, 65_000, null, { running: [], decisions: [], work: [], rules: [] }, 1)).toBe('CSR compacted at 300k → 65k · 1 waiting in Lost and found')
  expect(noticeLine(300_000, 65_000, null, { running: [], decisions: [], work: [], rules: [] }, 0)).toBe('CSR compacted at 300k → 65k')
})

test('ten minutes without a prompt at the terminal is unattended: no countdown', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  await typed($)
  await seen.clock.advance(11 * 60_000)
  await $.prompt.submit({ text: '<task-notification><status>completed</status></task-notification>', origin: { kind: 'task-notification' } } as never)
  await turn($, LANDED)
  expect(seen.compacted.length).toBe(1)
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'compact', mode: 'unattended', by: 'unattended' }))
})

// 10-12 20:53: the keep list brought back three Lost and found threads from 10-11 and 10-12 morning, one waiting
// on agent ab763f58e5be44c02, long gone. A later summary had dropped them. A thread set aside for later reaches the
// summary by prep-compact's hook, on the engine's trigger 'plugin', which this kit's compaction does not carry.
// 10-13 04:50: (1) the keep list named shells b02336hzp, b81nce4ei and bpd9kf6h3 as running; they were a subagent's,
// and Claude Code's own task list showed one main-thread shell. (3) It carried "7 uncommitted files (src/engine.rs,
// mcp/mod.rs, .agents/, ...)" from another session in the same checkout; this one never touched them.
test("the keep list holds the main thread's tasks and this session's changes, never a subagent's shells or another session's", AUTO, async ($, on) => {
  const seen = world(on, { ctx: 300_000, git: { 'rev-parse --show-toplevel': '/repo', 'status --porcelain': ' M src/engine.rs\n M mcp/mod.rs\n?? .agents/\n M hooks/compact.ts',
    'log --format=%h @{u}..HEAD': 'fdaba23', 'rev-parse --short HEAD': 'fdaba23', 'symbolic-ref -q --short HEAD': 'main' },
  said: e => e.tool === 'Agent' ? 'Async agent launched successfully.\nagentId: a66d38d8d43ee1dad (internal ID)' : (e as { run_in_background?: boolean }).run_in_background
    ? `Command running in background with ID: ${(e as { tool_use_id?: string }).tool_use_id === 'main-shell' ? 'bmain0001' : (e as { tool_use_id?: string }).tool_use_id}` : '' })
  await $.tool.call({ tool: 'Agent', description: 'scrub review', prompt: 'x', tool_use_id: 'agent-1' } as never)
  await $.tool.call({ tool: 'Bash', command: 'make', run_in_background: true, description: 'main build', tool_use_id: 'main-shell' } as never)
  for (const id of ['b02336hzp', 'b81nce4ei', 'bpd9kf6h3']) // the subagent's own shells
    await $.tool.call({ tool: 'Bash', command: 'sleep 60', run_in_background: true, agentId: 'a5f814bf6883609c3', tool_use_id: id } as never)
  await $.tool.call({ tool: 'Edit', file_path: '/repo/hooks/compact.ts', old_string: 'a', new_string: 'b', tool_use_id: 'edit-1' } as never)
  const tasks = [{ id: 'a66d38d8d43ee1dad', type: 'subagent', status: 'running', description: 'scrub review' },
    { id: 'bmain0001', type: 'shell', status: 'running', description: 'main build' },
    ...['b02336hzp', 'b81nce4ei', 'bpd9kf6h3'].map(id => ({ id, type: 'shell', status: 'running', description: 'subagent shell' }))]
  await fromPhone($)
  await turn($, 'Still going.', tasks)
  const told = seen.compacted.at(-1) ?? ''
  expect(told).toContain('subagent a66d38d8d43ee1dad')
  expect(told).toContain('shell bmain0001')
  for (const id of ['b02336hzp', 'b81nce4ei', 'bpd9kf6h3']) expect(told).not.toContain(id)
  expect(told).toContain('1 uncommitted file (hooks/compact.ts)') // the file this session wrote, no other
  expect(told).not.toContain('engine.rs')
  expect(told).not.toContain('not pushed') // no commit of this session's here
  const start = seen.compactsLogged().find(record => record.event === 'start') as { keep?: { running?: string[] } } | undefined
  expect(start?.keep?.running?.length).toBe(2)
})

test('another session\'s changes alone: no unfinished work section at all', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 300_000, git: { 'rev-parse --show-toplevel': '/repo', 'status --porcelain': ' M src/engine.rs\n M mcp/mod.rs\n?? .agents/',
    'log --format=%h @{u}..HEAD': 'fdaba23', 'rev-parse --short HEAD': 'fdaba23', 'symbolic-ref -q --short HEAD': 'main' } })
  await fromPhone($)
  await turn($, 'Still going.', [AGENT])
  const told = seen.compacted.at(-1) ?? ''
  expect(told).not.toContain('Unfinished work')
  expect(told).not.toContain('branch main')
})

test('Lost and found stays out of the keep list: waiting or set aside for later, no thread is an open decision', AUTO, async ($, on) => {
  const lost = [
    { text: "Wait for agent ab763f58e5be44c02's timeout-fix report.", state: 'lost', waiting: true, hidden: null, later: false },
    { text: 'Try the cache countdown in the masthead.', state: 'lost', waiting: false, hidden: 'later', later: true },
  ]
  const seen = world(on, { ctx: 300_000, lost })
  await fromPhone($)
  await turn($, 'Still going.', [AGENT])
  expect(seen.compacted.length).toBe(1)
  expect(seen.compacted[0]).not.toContain('ab763f58e5be44c02')
  expect(seen.compacted[0]).not.toContain('Waiting on the owner')
  expect(seen.compacted[0]).not.toContain('Try the cache countdown')
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ kept: expect.objectContaining({ decisions: 0 }) }))
})

// The engine runs CSR's own $.session.compact with trigger 'plugin' (SessionCompactArgs), through every hook but the
// calling one, so prep-compact's runs too. This kit leaves the trigger and the messages out, and prep-compact's
// matcher skips it: the call is made here as the engine makes it, CSR's keep list as its instructions.
test("CSR's own compaction carries the threads set aside for later after its keep list, on the engine's trigger 'plugin'", AUTO, async ($, on) => {
  const lost = [
    { text: "Wait for agent ab763f58e5be44c02's timeout-fix report.", state: 'lost', waiting: true, hidden: null, later: false },
    { text: 'Try the cache countdown in the masthead.', state: 'lost', waiting: false, hidden: 'later', later: true },
  ]
  const seen = world(on, { ctx: 300_000, lost })
  const keep = keepList({ running: [{ id: 'a427c126', type: 'subagent', description: 'Explore CapCut' }], decisions: [], work: [], rules: [] })
  const messages = [{ role: 'user', text: 'one', toolUses: [] }, { role: 'assistant', text: 'done', toolUses: [] }]
  await $.session.compact({ trigger: 'plugin', instructions: keep, messages } as never)
  const told = seen.compacted.at(-1) ?? ''
  expect(told.startsWith(`${keep}\n\n`)).toBe(true) // the keep list first: its header keeps it out of CSR's memory
  expect(told).toContain("add a section headed 'Reminders from CSR (not tasks)'")
  expect(told).toContain('- Try the cache countdown in the masthead.')
  expect(told).not.toContain('ab763f58e5be44c02') // waiting, not set aside: Lost and found must see it go quiet
  expect(seen.runs.find(run => run.includes('--lost'))).toEqual(expect.arrayContaining(['--repo', '/repo', '--lost', '--sid', 'sess-1']))
})

// 10-12: "if you trigger the compact, make it a card ... and stream the info as needed": the pane's card reads these.
test('the card streams: what it keeps is logged before the summary runs, and the result carries it again', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 300_000, git: { 'rev-parse --show-toplevel': '/repo', 'status --porcelain': ' M src/engine.rs', 'rev-parse --short HEAD': 'fdaba23', 'symbolic-ref -q --short HEAD': 'main' } })
  await $.tool.call({ tool: 'Edit', file_path: '/repo/src/engine.rs', old_string: 'a', new_string: 'b', tool_use_id: 'edit-1' } as never)
  await fromPhone($)
  await turn($, 'Still going.', [AGENT])
  const events = seen.compactsLogged().map(record => record.event)
  expect(events.indexOf('start')).toBeGreaterThan(-1)
  expect(events.indexOf('start')).toBeLessThan(events.indexOf('compact'))
  const start = seen.compactsLogged().find(record => record.event === 'start')
  const rows = { running: ['subagent a427c126 · Explore CapCut draft automation'], decisions: [], work: ['branch main at fdaba23', '1 uncommitted file (src/engine.rs)'], rules: [] }
  expect(start).toEqual(expect.objectContaining({ why: 'over 269k · 1 agent kept', ctx_before: 300_000, keep: rows }))
  expect(seen.compactsLogged().find(record => record.event === 'compact')).toEqual(expect.objectContaining({ why: 'over 269k · 1 agent kept', keep: rows }))
})

test('a pane.py that does not answer never stops the high edge: past it, from the phone, it still compacts', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 300_000, plan: null })
  await fromPhone($)
  await turn($, 'Still going.', [AGENT])
  expect(seen.compacted.length).toBe(1)
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'compact', forced: true, payback: null }))
  expect(seen.appended()[0]).toBe('CSR compacted at 300k → 21k · kept: 1 running agent')
})

test('a compaction a hook vetoes is logged as a skip, and the plan says so', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000, skip: 'another plugin keeps this one' })
  await fromPhone($)
  await turn($, LANDED)
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'skip', outcome: 'skipped', reason: 'another plugin keeps this one' }))
  expect(seen.plan()?.text).toContain('the compaction did not run')
  expect(seen.appended()).toEqual([])
})

test('never in a subagent\'s turn, and never in a claude -p child', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 290_000 })
  await fromPhone($)
  await turn($, LANDED, [], 'sub-1')
  expect(seen.compacted).toEqual([])
  expect(seen.compactsLogged()).toEqual([])
})

test('a claude -p child draws nowhere, and never plans or compacts', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 290_000, headless: true })
  await fromPhone($)
  await turn($, LANDED)
  expect(seen.compacted).toEqual([])
  expect(seen.compactsLogged()).toEqual([])
  expect(seen.plan()).toBeNull()
})

test('suggest: the band offers it and only a press compacts', { options: { autoCompact: 'suggest' } }, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  const band = await $.ui.mount(BAND_UI)
  await fromPhone($)
  await turn($, LANDED)
  expect(seen.compacted).toEqual([])
  expect(await band.find({ text: 'Compact now? · 214k · pays back in ~8 API calls (~1 prompt) · commit 6369e6b landed' })).toBeDefined()
  expect(seen.plan()?.text).toContain('waiting: your press')
  await seen.clock.advance(5 * 60_000)
  expect(seen.compacted).toEqual([])
  await band.press({ key: 'compact-now' })
  await settle(() => seen.compacted.length > 0)
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'compact', by: 'press', setting: 'suggest' }))
})

test('off: no plan, no band, no compaction, however far past the band', { options: { autoCompact: 'off' } }, async ($, on) => {
  const seen = world(on, { ctx: 400_000 })
  await fromPhone($)
  await turn($, LANDED)
  expect(seen.runs.some(argv => argv.includes('--compact-plan'))).toBe(false)
  expect(seen.compacted).toEqual([])
  expect(seen.plan()).toBeNull()
})

// The first run's walkthrough waits under a countdown: its 20 s are short, and its 1 and 0 are the ones up.
test('a countdown wins over the setup walkthrough, which comes back once the countdown is cancelled', AUTO, async ($, on) => {
  world(on, { ctx: 214_000 })
  const store = new Map<string, unknown>()
  on('store.get', async (_$, e) => ({ value: store.get(e.key) }))
  on('store.set', async (_$, e) => {
    store.set(e.key, e.value)
    return { value: undefined }
  })
  on('store.keys', async () => ({ value: [...store.keys()] }) as never)
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.register', async (_$, e) => ({ value: { command: e.name } }))
  on('ui.open', async () => ({ value: { isPlaced: true } }))
  on('ui.status', async () => ({ value: undefined }))
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true, startedAt: 0 } as never)
  const band = await $.ui.mount(BAND_UI)
  expect(await band.find({ text: /CSR setup 1\/2/ })).toBeDefined()
  await turn($, LANDED)
  expect(await band.find({ text: /Compacting in 20s/ })).toBeDefined()
  expect(await band.find({ text: /CSR setup/ })).toBeUndefined()
  await band.press({ key: 'compact-cancel' })
  expect(await band.find({ text: /CSR setup 1\/2/ })).toBeDefined()
})

// 10-12 15:30: a sync of the dev mod reloaded the module at a turn end, its 20 s timer dropped with it, and the band
// sat at "Compacting in 0s"; its 1 had nothing to compact with. What it would compact with now rides in the view.
test('after a reload a countdown runs on for what is left of it, an offer stands; one with nothing to compact with goes', () => {
  const armed = { ctx: 225_000 }
  expect(resumeOf(null, NOW)).toEqual({ act: 'none' })
  expect(resumeOf({ kind: 'countdown', endsAt: NOW + 18_000 }, NOW)).toEqual({ act: 'drop' }) // a view from before armed rode in it
  expect(resumeOf({ kind: 'countdown', endsAt: NOW + 18_000, armed }, NOW)).toEqual({ act: 'countdown', leftMs: 18_000 })
  expect(resumeOf({ kind: 'countdown', endsAt: NOW - 5_000, armed }, NOW)).toEqual({ act: 'countdown', leftMs: 0 }) // due: at once
  expect(resumeOf({ kind: 'countdown', endsAt: NOW - RESUME_LATE_MS - 1, armed }, NOW)).toEqual({ act: 'drop' }) // long past
  expect(resumeOf({ kind: 'offer', armed }, NOW)).toEqual({ act: 'offer' })
})

// Codex review 10-12, #10: a countdown started under auto ran out and compacted after the person chose suggest or off.
// A change in /config converts it at once (suggest: an offer, off: nothing); the module the change reloads resumes it
// as the setting now says; and the timer itself asks for auto when it runs out.
test('after a reload a countdown resumes as the setting now says: auto runs it, suggest offers it, off drops it', () => {
  const view = { kind: 'countdown' as const, endsAt: NOW + 18_000, armed: { ctx: 225_000 } }
  expect(resumeOf(view, NOW, 'auto')).toEqual({ act: 'countdown', leftMs: 18_000 })
  expect(resumeOf(view, NOW, 'suggest')).toEqual({ act: 'offer' })
  expect(resumeOf(view, NOW, 'off')).toEqual({ act: 'drop' })
  expect(resumeOf({ kind: 'offer', armed: { ctx: 225_000 } }, NOW, 'off')).toEqual({ act: 'drop' })
  expect(resumeOf({ kind: 'offer', armed: { ctx: 225_000 } }, NOW, 'suggest')).toEqual({ act: 'offer' })
})

test('a countdown the person turns to suggest in /config becomes an offer; turned off, it goes; neither compacts', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  on('config.set', async (_$, e) => ({ value: e.value }))
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  expect(await band.find({ text: /Compacting in 20s/ })).toBeDefined()
  await $.config.set({ key: 'csr-companion.autoCompact', value: 'suggest' } as never)
  expect(await band.find({ text: /^Compact now\?/ })).toBeDefined() // converted before the change resolves
  await seen.clock.advance(COUNTDOWN_MS + 5_000)
  expect(seen.compacted).toEqual([])
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  await $.config.set({ key: 'csr-companion.autoCompact', value: 'off' } as never)
  expect(await band.find({ text: /^Compact now\?/ })).toBeUndefined()
  await seen.clock.advance(5 * 60_000)
  expect(seen.compacted).toEqual([])
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'cancel', by: 'setting' }))
})

// Codex r2 #3: Auto, unattended, 220k at a seam: the person turned Auto off while the compaction was being prepared
// (the plan being logged, or the keep list being read from git), and it compacted anyway, with the setting off.
async function flipWhilePreparing($: Engine, on: On, pausedAt: 'plan' | 'git status', setting: 'off' | 'suggest') {
  let release = () => undefined as void
  const gate = new Promise<void>(done => (release = done))
  let paused = false
  const at = (argv: readonly string[]) => pausedAt === 'plan'
    ? argv.includes('--keep') && (JSON.parse(argv[argv.indexOf('--keep') + 1] ?? '{}') as { event?: string }).event === 'plan'
    : argv[0] === 'git' && argv.includes('status')
  const seen = world(on, { ctx: 220_000, git: { 'rev-parse --show-toplevel': '/repo', 'status --porcelain': ' M a.ts' }, hold: argv => (!paused && at(argv) ? ((paused = true), gate) : undefined) })
  on('config.set', async (_$, e) => ({ value: e.value }))
  await fromPhone($)
  const ended = turn($, LANDED)
  await settle(() => paused)
  expect(paused).toBe(true)
  await $.config.set({ key: 'csr-companion.autoCompact', value: setting } as never)
  release()
  await ended
  await settle(() => seen.compactsLogged().some(record => record.event === 'skip'))
  expect(seen.compacted).toEqual([])
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ event: 'skip', reason: `the setting changed to ${setting}` }))
}
test('Auto turned off while the plan is logged: it never compacts, and the log says why', AUTO, ($, on) => flipWhilePreparing($, on, 'plan', 'off'))
test('Auto turned off while the keep list is read: it never compacts', AUTO, ($, on) => flipWhilePreparing($, on, 'git status', 'off'))
test('Auto turned to suggest while the plan is logged: it never compacts on its own', AUTO, ($, on) => flipWhilePreparing($, on, 'plan', 'suggest'))

// astra r3 #10: Auto, attended, 220k at a seam: Off chosen while the plan was logged still installed the countdown,
// its timer with it; it ran out at zero with nothing to compact. Off leaves no countdown and no timer.
test('Off chosen while a countdown is prepared: no countdown, no timer, nothing compacts', AUTO, async ($, on) => {
  let release = () => undefined as void
  const gate = new Promise<void>(done => (release = done))
  let paused = false
  const isPlan = (argv: readonly string[]) =>
    argv.includes('--keep') && (JSON.parse(argv[argv.indexOf('--keep') + 1] ?? '{}') as { event?: string }).event === 'plan'
  const seen = world(on, { ctx: 220_000, hold: argv => (!paused && isPlan(argv) ? ((paused = true), gate) : undefined) })
  on('config.set', async (_$, e) => ({ value: e.value }))
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  const ended = turn($, LANDED)
  await settle(() => paused)
  await $.config.set({ key: 'csr-companion.autoCompact', value: 'off' } as never)
  release()
  await ended
  await settle(() => seen.compactsLogged().some(record => record.event === 'skip'))
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
  expect(seen.compactsLogged().map(record => record.event)).toEqual(['plan', 'skip'])
  expect(seen.compactsLogged().at(-1)).toEqual(expect.objectContaining({ reason: 'the setting changed to off' }))
  await seen.clock.advance(COUNTDOWN_MS + 5_000)
  expect(seen.compacted).toEqual([])
  expect(await band.find({ text: /Compacting in|Compact now/ })).toBeUndefined()
})

// astra r4 #12: the countdown's plan waited on HOME while Off took the countdown down; it then wrote "countdown" over
// Off's "none", and the pane showed "Compacting in 0s" for up to two hours. A plan a newer one overtook writes nothing.
test('a countdown plan that finishes after Off took it down writes nothing over Off', AUTO, async ($, on) => {
  let release = () => undefined as void
  const gate = new Promise<void>(done => (release = done))
  let paused = false
  const seen = world(on, { ctx: 220_000, holdHome: () => (!paused && seen.compactsLogged().some(record => record.event === 'plan') ? ((paused = true), gate) : undefined) })
  on('config.set', async (_$, e) => ({ value: e.value }))
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  const ended = turn($, LANDED)
  await settle(() => paused) // the countdown is up, its plan waits on HOME
  await $.config.set({ key: 'csr-companion.autoCompact', value: 'off' } as never)
  release()
  await ended
  await settle(() => false)
  expect(seen.plan()?.state).toBe('none')
  expect(seen.compactsLogged().map(record => record.event)).toEqual(['plan', 'cancel'])
  await seen.clock.advance(COUNTDOWN_MS + 5_000)
  expect(seen.compacted).toEqual([])
  expect(await band.find({ text: /Compacting in/ })).toBeUndefined()
})

test('a session start in the middle of a countdown leaves it running: one compaction, when it runs out', AUTO, async ($, on) => {
  const seen = world(on, { ctx: 214_000 })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  on('command.register', async (_$, e) => ({ value: { command: e.name } }))
  on('ui.open', async () => ({ value: { isPlaced: true } }))
  on('ui.status', async () => ({ value: undefined }))
  const band = await $.ui.mount(BAND_UI)
  await typed($)
  await turn($, LANDED)
  expect(await band.find({ text: /Compacting in 20s/ })).toBeDefined()
  await seen.clock.advance(5_000)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true, startedAt: 0 } as never)
  expect(await band.find({ text: /Compacting in 15s/ })).toBeDefined()
  await seen.clock.advance(COUNTDOWN_MS - 5_000)
  await settle(() => seen.compactsLogged().some(record => record.event === 'compact'))
  expect(seen.compacted.length).toBe(1)
  expect(seen.compactsLogged().map(record => record.event)).toEqual(['plan', 'countdown', 'start', 'compact'])
})
