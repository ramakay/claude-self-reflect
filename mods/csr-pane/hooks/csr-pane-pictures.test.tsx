import { describe, expect, test } from 'claude-code/testing'
import { PANE_V1 } from './fixtures/pane-v1'

import { BAND_TARGET, engine, FRAME_WIDE, PANE_TARGET, SUMMARY, USAGE } from './kit'
import { HERDR_GRACE_MS, pidOf, standsDown } from './herdr'
import { majorOf, parseSummary } from './engine'
import { paint, textWidth } from './paint'
import type { Scene } from './paint'
import { contextOf, csrLine, headlineOf, parsePlan, piecesOf, quotaRows, readTokens, samePageOf, tokens, untilWords } from './pieces'
import { cellsWanted, gridCards, bandRowsFor, bandsOf, MAX_TREE_BYTES, withinTree, buildCards, BAND_ROWS, compacted, frameDelay, pageOf, setPlaced, SPIN_FRAME_MS, SPIN_LIMIT_MS, withBackdrop, motionAt, motionFor, movingAt, recalled, resetMotion, retarget, schemeFrom, setMotion, shapeFor } from './picture'
import { toPng } from './png'
import { setupOf, tabRows, withLive } from './register'
import { columnGlyphs, BODY, COLUMN, STILL, agentsCard, clockOf, codeLabel, fit, limitsCard, timelineCard, wrap } from './scene'
import { parseFrame } from './sgr'
import { toSvg } from './surface'
import { cellText, rowRuns, segmentsOf, toCells } from './cells'
import { DARK, LIGHT } from './theme'
import { emptyFacts, newReader, take, takeText, thin, transcriptPath } from './transcript'
import { toUsage } from './usage'

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true } as const
const NOW = Date.parse('2026-11-15T12:00:00Z')
const T = (min: number) => NOW - min * 60_000
const iso = (ms: number) => new Date(ms).toISOString()

// A frame as the engine writes it at 160 columns, with the budget, the story's code rows and the foot.
const FRAME = [
  '☾ The dreamer looks as you work.',
  'dreamer 607k of 5.9M',
  'Same page?',
  "Model's read: 3 of 9 asks missed. You were unclear on 1. You keep sending screenshots to show what Claude could not see.",
  'Should be a skill / memory / tool',
  'memory: Look at a pane change on a real screen before reporting it. · evidence',
  'skill: A clean probe for a live mod was rebuilt by hand. · evidence',
  'tool: A pane width checker. · evidence',
  'Lost and found',
  'Standing: ask before push, merge, tag or publish; nothing is pushed.',
  'The last look found nothing. 4 of 45 looks end this way.',
  '1 note kept',
  'Story · refreshed 5m ago',
  'The work lives in the `feat/pane-mod` worktree.',
  '+ picture.ts · drawPicture unverified',
  '+ paint.ts · paint verified',
  '+ register.tsx · orbitOf ⌗9d34137',
  '! png.ts · toPng unverified',
  'API credit $59.04 left',
  'CTX 130k/1M · +27k/turn · 3 by CSR',
  '7D 7% · Early · earlier weeks peaked around 78% · CSR 0.1 pts · ◷ 59m · Codex 7D 22%',
  '$3.54 this week',
].join('\n')

const usage = toUsage(USAGE as never)
const plan = parsePlan('{"story":{"by_csr":3,"by_you":0,"payback":1800000}}')
const pieces = () => piecesOf(parseFrame(FRAME), 160, usage, plan, parseSummary(SUMMARY), null, NOW)

const line = (o: object) => JSON.stringify(o)
const reply = (at: number, id: string, ctx: number, content: unknown[] = []) =>
  line({ type: 'assistant', timestamp: iso(at), message: { id, usage: { input_tokens: 10, cache_read_input_tokens: ctx - 10, cache_creation_input_tokens: 0 }, content } })

describe('the pieces, read from what the mod already runs', () => {
  test('the dreamer: its state as the tag, the budget as a share, the last look', () => {
    const d = pieces().dreamer
    expect(d?.title).toBe("Self-Reflect's dreamer")
    expect(d?.tag).toBe('Looks as you work')
    expect(d?.budget).toBe('607k of 5.9M')
    expect(Math.abs((d?.fraction ?? 0) - 607 / 5900)).toBeLessThan(1e-6)
    expect(d?.lastLook).toBe('The last look found nothing. 4 of 45 looks end this way.')
    expect(readTokens('6.3M')).toBe(6_300_000)
  })

  test('Same page?: the numbers first, the model’s words after them', () => {
    expect(pieces().samePage).toEqual({
      read: "Model's read: 3 of 9 asks missed. You were unclear on 1.",
      more: 'You keep sending screenshots to show what Claude could not see.',
    })
    expect(samePageOf("Model's read: 0 of 1 asks missed. You were clear. You gave a full brief.")?.more).toBe('You gave a full brief.')
  })

  test('should-be items, the story with its code rows, and the notes kept', () => {
    const p = pieces()
    expect(p.shouldBe.map(s => s.kind)).toEqual(['memory', 'skill', 'tool'])
    expect(p.shouldBe[0]?.text).toBe('Look at a pane change on a real screen before reporting it.')
    expect(p.story?.age).toBe('5m ago')
    expect(p.story?.notes).toBe('1 note kept')
    expect(p.story?.rows).toEqual([
      { file: 'picture.ts', symbol: 'drawPicture', state: 'unverified', stamp: 'unverified' },
      { file: 'paint.ts', symbol: 'paint', state: 'verified', stamp: 'verified' },
      { file: 'register.tsx', symbol: 'orbitOf', state: 'commit', stamp: '9d34137' },
      { file: 'png.ts', symbol: 'toPng', state: 'unverified', stamp: 'unverified' },
    ])
    expect(p.lost).toEqual(['Standing: ask before push, merge, tag or publish; nothing is pushed.'])
  })

  test('the limits: the engine’s headline, Weekly from Claude Code, Codex from the engine', () => {
    const l = pieces().limits
    // Claude Code's own reading (2%) is newer than the frame's 7%.
    expect(l?.bars).toEqual([
      { name: 'Weekly', percent: 2 },
      { name: 'Codex 7D', percent: 22 },
    ])
    expect(l?.headline).toBe('Early in the week. Weekly is at 2% so far.')
    expect(l?.tone).toBe('quiet')
  })

  test('the headline follows the engine’s cases', () => {
    const rows = (text: string) => quotaRows([text])
    expect(headlineOf(rows('7D 80% · On pace for about 95% by reset'), {}, NOW)?.text).toBe('Close to the limit. Expect about 95% of the Weekly limit by reset.')
    expect(headlineOf(rows('7D 50% · On pace for about 80% by reset'), {}, NOW)?.tone).toBe('amber')
    expect(headlineOf(rows('5H 40% · On pace to run out around 3:10 pm'), {}, NOW)?.text).toBe('On course to run out: 5 Hour limit hit around 3:10 pm.')
    expect(headlineOf(rows('7D 20% · On pace for about 40% by reset'), { '7D': '2026-11-20T18:00:00Z' }, NOW)?.text).toMatch(/^On track\. You should reach \w+day's reset with room to spare\.$/)
    expect(headlineOf(rows('7D 77% · Early · last week peaked at 90%'), {}, NOW)?.text).toBe('Weekly is already at 77% of its limit.')
    expect(rows('Codex 7D 22%')).toEqual([])
    // A mark after the figure (seen in the engine's frame: "13%»") keeps the state readable.
    expect(rows('7D 13%» · Early · earlier weeks peaked around 78% · CSR 0.4 pts')[0]?.early).toBe(true)
    expect(headlineOf(rows('7D 13%» · Early · earlier weeks peaked around 78%'), {}, NOW)?.text).toBe('Early in the week. Weekly is at 13% so far.')
  })

  test('the context: the herdr pane’s part names from the transcript’s split, and the mix', () => {
    const ctx = contextOf(usage, { tools: 60, talk: 30, csr: 10, compacted: true })
    expect(ctx?.parts.map(p => p.name)).toEqual(['Summary + setup', 'Messages', 'Tool results', 'CSR injected'])
    expect(ctx?.parts[0]?.tokens).toBe(45_000)
    expect(Math.round(ctx?.parts[2]?.tokens ?? 0)).toBe(147_000)
    expect(ctx?.free).toBe(677_000)
    expect(ctx?.buffer).toBe(33_000)
    // Tool results are 147k of 290k in use: half or more.
    expect(ctx?.mix?.verdict).toBe('tool-heavy')
    expect(contextOf(usage, null)?.mix?.verdict).toBe('context-heavy')
  })

  test('Self-Reflect’s line is the engine’s csr_row: its compactions, a net saving above zero, its share', () => {
    expect(pieces().csr).toBe('3 compactions · ~1.8M tokens saved (est.) · 0.1 pts of the weekly limit')
    expect(csrLine({ compactions: 0, saved: 0 }, [])).toBe(null)
    // A net loss is the engine's to keep quiet about: it draws no ring for it.
    expect(parsePlan('{"story":{"by_csr":1,"payback":-40000}}')).toEqual({ compactions: 1, saved: -40000 })
    expect(csrLine({ compactions: 1, saved: -40000 }, ['7D 9% · CSR 0 pts'])).toBe('1 compaction')
    expect(parsePlan('not json')).toBe(null)
  })

  test('spans and tokens are written as the engine writes them', () => {
    const at = (s: number) => new Date(NOW + s * 1000).toISOString()
    expect(untilWords(at(2 * 3600 + 5 * 60), NOW)).toBe('2h 05m')
    expect(untilWords(at(37 * 60), NOW)).toBe('37m')
    expect(untilWords(at(3600 + 59 * 60 + 59), NOW)).toBe('1h 59m')
    expect(untilWords(at(-5), NOW)).toBe('0m')
    expect(untilWords(at(2 * 86400 + 20 * 3600), NOW)).toBe('2d 20h')
    expect([tokens(950), tokens(1500), tokens(12_400), tokens(2_793_232), tokens(12_000_000)]).toEqual(['950', '1.5k', '12k', '2.8M', '12M'])
  })

  test('the headline: the soonest to run out; no 7D pace and nothing past its early stretch, no headline', () => {
    const both = quotaRows(['5H 40% · On pace to run out around 11:50 pm · 7D 60% · On pace to run out around 1:10 pm'])
    expect(headlineOf(both, {}, NOW)?.text).toBe('On course to run out: Weekly limit hit around 1:10 pm.')
    expect(headlineOf(quotaRows(['5H 40% · Pace shows after a few readings']), {}, NOW)?.text).toBe('Watching the limits: 5 Hour is at 40% so far.')
    expect(headlineOf(quotaRows(['5H 10% · Early · last window peaked at 40% · 7D 20% · On pace for about 30% by reset']), {}, NOW)?.text).toMatch(/^On track/)
  })
})

describe('the transcript', () => {
  test('a reply counts once, a compaction resets the window, a side chain is not the session', () => {
    const r = newReader('/x.jsonl')
    take(r, line({ type: 'user', timestamp: iso(T(30)), message: { content: 'build the pane' } }))
    take(r, reply(T(29), 'm1', 120_000, [{ type: 'text', text: 'ok' }]))
    take(r, reply(T(29), 'm1', 120_000, [{ type: 'tool_use', name: 'Edit', input: { a: 1 } }]))
    take(r, line({ type: 'user', timestamp: iso(T(28)), message: { content: [{ type: 'tool_result', content: 'x'.repeat(500) }] } }))
    take(r, reply(T(27), 'm2', 150_000, [{ type: 'tool_use', name: 'mcp__claude-self-reflect__csr_reflect_on_past', input: {} }]))
    take(r, line({ type: 'assistant', isSidechain: true, timestamp: iso(T(26)), message: { id: 'side', usage: { input_tokens: 9 } } }))
    expect(r.facts.replies).toEqual([
      [T(29), 120_000],
      [T(27), 150_000],
    ])
    expect(r.facts.edits).toEqual([T(29)])
    expect(r.facts.recalls).toEqual([T(27)])
    expect(r.facts.prompts).toEqual([T(30)])
    expect(r.facts.window.tools).toBe(500)
    take(r, line({ type: 'system', subtype: 'compact_boundary', timestamp: iso(T(20)) }))
    expect(r.facts.compacts).toEqual([T(20)])
    expect(r.facts.window).toEqual({ tools: 0, talk: 0, csr: 0 })
    take(r, line({ type: 'attachment', timestamp: iso(T(19)), attachment: { type: 'hook_success', hookEvent: 'UserPromptSubmit', content: 'recap [[CSR:RECAP]]' } }))
    expect(r.facts.recalls).toEqual([T(27), T(19)])
    expect(r.facts.window.csr).toBe(19)
  })

  test('only whole lines are taken; a read inside the file drops its first line', () => {
    const r = newReader('/x.jsonl')
    const a = reply(T(5), 'a', 1000)
    const b = reply(T(4), 'b', 2000)
    expect(takeText(r, `${a}\n${b.slice(0, 20)}`, false)).toBe(a.length + 1)
    expect(r.facts.replies.length).toBe(1)
    const s = newReader('/x.jsonl')
    takeText(s, `${a}\n${b}\n`, true)
    expect(s.facts.replies.map(x => x[1])).toEqual([2000])
    expect(transcriptPath('/home/me', '/work/my.repo', 'sid')).toBe('/home/me/.claude/projects/-work-my-repo/sid.jsonl')
  })
})

describe('the painted cards', () => {
  const cards = (p = DARK) => buildCards({ pieces: pieces(), facts: null, hints: { same: '4 opens', should: '4 opens' } }, 42, 2.43, p, STILL, NOW)
  const words = (scene: Scene) => scene.shapes.flatMap(s => (s.kind === 'text' ? [s.text] : []))

  test('every herdr card is there, in its order, each a whole number of rows', () => {
    const all = cards()
    expect(all.map(c => c.id)).toEqual(['dreamer', 'same', 'should', 'story', 'limits'])
    for (const c of all) {
      expect(Number.isInteger(c.painted.rows)).toBe(true)
      expect(Math.abs(c.painted.scene.height - c.painted.rows * COLUMN * 2.43)).toBeLessThan(1e-6)
      expect(c.painted.scene.width).toBe(42 * COLUMN)
    }
  })

  test('the cards say the herdr pane’s words, the main claim included', () => {
    const said = cards().flatMap(c => words(c.painted.scene))
    expect(said).toContain("Self-Reflect's dreamer")
    // The state sits on the title row where it fits, else on its own line.
    expect(said.some(w => /^( · )?Looks as you work$/.test(w))).toBe(true)
    expect(said).toContain('607k of 5.9M')
    expect(said).toContain('Same page?')
    expect(said).toContain('1 of 3 · 4 opens')
    expect(said).toContain('memory')
    expect(said).toContain('Where the work stands')
    expect(said).toContain('1 note kept')
    expect(said).toContain('9d34137')
    expect(said).toContain('Codex 7D')
    expect(said).toContain('22%')
    expect(said).toContain('Self-Reflect this session:')
    expect(said.join(' ').replace(/\s+/g, ' ')).toContain('~1.8M tokens saved (est.)')
    expect(said).toContain('context-heavy')
  })

  test('no word is smaller than the caption, and body words are the terminal’s own height', () => {
    const sizes = cards().flatMap(c => c.painted.scene.shapes.flatMap(s => (s.kind === 'text' ? [s.size] : [])))
    expect(Math.min(...sizes)).toBeGreaterThanOrEqual(18)
    expect(BODY).toBeGreaterThanOrEqual(20)
  })

  test('words never run past the card', () => {
    for (const c of cards()) {
      for (const s of c.painted.scene.shapes) {
        if (s.kind !== 'text') continue
        const w = textWidth(s.text, s.size, s.face)
        const left = s.anchor === 'end' ? s.x - w : s.anchor === 'middle' ? s.x - w / 2 : s.x
        expect(left).toBeGreaterThanOrEqual(0)
        expect(left + w).toBeLessThanOrEqual(c.painted.scene.width + 0.5)
      }
    }
  })

  test('light and dark use their own inks', () => {
    const ink = (p: typeof DARK) => cards(p)[0]?.painted.scene.shapes.find(s => s.kind === 'text')?.color
    expect(ink(DARK)).toBe(DARK.ink)
    expect(ink(LIGHT)).toBe(LIGHT.ink)
  })

  test('wrap and fit keep to the width, and say when they cut', () => {
    const lines = wrap('one two three four five six seven eight nine ten', BODY, 'sans', 120, 2)
    expect(lines.length).toBe(2)
    expect(lines[1]?.endsWith('…')).toBe(true)
    expect(fit('a long line of words', BODY, 'sans', 60).endsWith('…')).toBe(true)
  })

  test('the timeline: a point a reply, a drop at the compaction, folded idle time', () => {
    const f = emptyFacts()
    f.prompts = [T(200), T(30)]
    f.replies = [
      [T(199), 100_000],
      [T(195), 300_000],
      [T(29), 80_000],
      [T(2), 120_000],
    ]
    f.compacts = [T(100)]
    f.recalls = [T(28)]
    const card = timelineCard(f, 42 * COLUMN, COLUMN * 2.43, DARK, STILL, NOW)
    expect(words(card.scene)).toContain('This session')
    expect(card.scene.shapes.filter(s => s.kind === 'area').length).toBe(1)
    expect(card.scene.shapes.some(s => s.kind === 'line' && s.color === DARK.pink)).toBe(true)
    const clock = clockOf([...f.prompts, ...f.replies.map(r => r[0])], NOW)
    // Two idle stretches over 20 minutes: 165 and 27 minutes. Each takes a few minutes of width.
    expect(clock.breaks.length).toBe(2)
    expect(clock.at(T(30))).toBeGreaterThan(0.5)
  })
})

describe('pages', () => {
  const card = (id: 'dreamer' | 'timeline', rows: number) => ({ id, painted: { rows, alt: '', scene: { width: 1, height: 1, shapes: [] } } })
  test('the timeline joins the Now page only where every card is wholly in view; Figure is the timeline', () => {
    const cards = [card('dreamer', 40), card('timeline', 12)]
    expect(pageOf(cards, 'now', 52).map(c => c.id)).toEqual(['dreamer', 'timeline'])
    expect(pageOf(cards, 'now', 51).map(c => c.id)).toEqual(['dreamer'])
    expect(pageOf(cards, 'figure', 10).map(c => c.id)).toEqual(['timeline'])
  })

  test('Claude Code’s reading now is the timeline’s last point: a compaction drops at once', () => {
    const f = emptyFacts()
    f.replies = [[T(10), 300_000]]
    f.compacts = [T(1)]
    expect(withLive(f, usage, NOW)?.replies).toEqual([
      [T(10), 300_000],
      [T(1), 300_000],
      [T(1) + 1, 290_000],
      [NOW, 290_000],
    ])
    expect(withLive(f, null, NOW)).toBe(f)
  })

  test('the tab row wraps in a narrow pane, and the rows under it count that', () => {
    expect(tabRows(200)).toBe(1)
    expect(tabRows(42)).toBe(2)
  })

  test('where no timeline can be drawn yet, the Figure tab says so in words', async ($, on) => {
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : FRAME_WIDE))
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await ui.press({ key: 'tab-figure' })
    expect(await ui.find({ type: 'Text', text: /no reply yet/ })).toBeDefined()
    await ui.unmount()
  })
})

describe('motion', () => {
  test('a new reading eases in; the card moves only while it does', () => {
    resetMotion()
    const p = pieces()
    retarget(p, NOW)
    expect(movingAt(NOW).size).toBe(0)
    const next = { ...p, limits: p.limits && { ...p.limits, bars: [{ name: 'Weekly', percent: 40 }] } }
    retarget(next, NOW + 1000)
    expect(movingAt(NOW + 1100).has('limits')).toBe(true)
    const mid = motionAt(NOW + 1200).eased['bar:Weekly'] ?? 0
    expect(mid).toBeGreaterThan(2)
    expect(mid).toBeLessThan(40)
    expect(motionAt(NOW + 3000).eased['bar:Weekly']).toBe(40)
    expect(movingAt(NOW + 3000).size).toBe(0)
  })

  test('a compaction draws its drop in, a recall glows, then both rest', () => {
    resetMotion()
    compacted(NOW)
    recalled(NOW)
    expect(movingAt(NOW + 100).has('timeline')).toBe(true)
    expect(motionAt(NOW + 300).drop).toBeLessThan(1)
    expect(motionAt(NOW + 300).recall).toBeGreaterThan(0)
    expect(movingAt(NOW + 5000).size).toBe(0)
    expect(motionAt(NOW + 5000).drop).toBe(1)
  })

  test('with motion off (auto in iTerm2) a change is drawn where it ends, in one frame', () => {
    expect(motionFor('auto', 'iTerm.app')).toBe(false)
    expect(motionFor('auto', 'ghostty')).toBe(true)
    expect(motionFor('on', 'iTerm.app')).toBe(true)
    resetMotion()
    setMotion(false)
    const p = pieces()
    retarget(p, NOW)
    retarget({ ...p, limits: p.limits && { ...p.limits, bars: [{ name: 'Weekly', percent: 40 }] } }, NOW + 1000)
    compacted(NOW + 1000)
    recalled(NOW + 1000)
    expect(movingAt(NOW + 1050).size).toBe(0)
    expect(motionAt(NOW + 1050).eased['bar:Weekly']).toBe(40)
    resetMotion()
  })

  test('the dreamer’s ring turns only while a look runs', () => {
    resetMotion()
    const p = pieces()
    retarget({ ...p, dreamer: p.dreamer && { ...p.dreamer, dreaming: true } }, NOW)
    expect(movingAt(NOW).has('dreamer')).toBe(true)
    retarget(p, NOW)
    expect(movingAt(NOW).has('dreamer')).toBe(false)
    resetMotion()
  })
})

describe('pixels and the other surfaces', () => {
  test('a card paints to a PNG under the Image limit, the edge of its box soft', () => {
    const card = limitsCard(pieces().limits, pieces().context, pieces().csr, 42 * COLUMN, COLUMN * 2.43, DARK, STILL)
    const frame = paint(card.scene, 14 / COLUMN)
    expect(frame.rgba.length).toBeLessThanOrEqual(2 * 1024 * 1024)
    const png = toPng(frame)
    expect(Array.from(png.slice(0, 8))).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
    expect(png.length).toBeLessThan(frame.rgba.length / 4)
  })

  test('rect, area and moon cover what they should', () => {
    const f = paint(
      {
        width: 40,
        height: 20,
        shapes: [
          { kind: 'rect', x: 2, y: 2, w: 10, h: 10, r: 3, color: 0xffffff },
          { kind: 'area', points: [[20, 10], [30, 4]], base: 18, color: 0x00ff00 },
          { kind: 'moon', x: 34, y: 10, r: 5, bx: 37, by: 7, br: 4, color: 0xff0000 },
        ],
      },
      1,
    )
    const a = (x: number, y: number) => f.rgba[(y * f.width + x) * 4 + 3] ?? 0
    expect(a(7, 7)).toBe(255)
    expect(a(2, 2)).toBeLessThan(128)
    expect(a(28, 15)).toBe(255)
    expect(a(21, 5)).toBe(0)
    expect(a(31, 12)).toBe(255)
    expect(a(37, 7)).toBe(0)
  })

  test('the SVG of a card holds its words and stays under the element’s limit', () => {
    const svg = toSvg(buildCards({ pieces: pieces(), facts: null, hints: { same: '', should: '' } }, 42, 2.2, DARK, STILL, NOW)[4]?.painted.scene ?? { width: 1, height: 1, shapes: [] })
    expect(svg).toContain('>Codex 7D</text>')
    expect(svg).toContain('<rect')
    expect(svg.length).toBeLessThan(131072)
  })

  test('options: auto by default, braille now means cells, the cell shape left to the mod by default', () => {
    expect(setupOf({})).toEqual({ view: 'auto', theme: 'auto', cellShape: undefined, motion: 'auto' })
    expect(setupOf({ view: 'braille', cellShape: 'auto', motion: 'on' })).toEqual({ view: 'cells', theme: 'auto', cellShape: undefined, motion: 'on' })
    expect(setupOf({ view: 'text', theme: 'light', cellShape: '2.4' })).toEqual({ view: 'text', theme: 'light', cellShape: 2.4, motion: 'auto' })
    expect(shapeFor('iTerm.app')).toBe(2.43)
    expect(shapeFor(undefined)).toBe(2.2)
    expect(schemeFrom('0;15')).toBe('light')
  })
})

describe('the pane with pictures', () => {
  test('the desktop app gets one SVG a card', async ($, on) => {
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : argv.includes('--compact-plan') ? '{"story":{"by_csr":2,"payback":900000}}' : FRAME_WIDE))
    await $.session.start({ ...START, surface: 'desktop' })
    const ui = await $.ui.mount({ ...PANE_TARGET, surface: 'desktop' } as never)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect((await ui.findAll({ type: 'Svg' })).length).toBeGreaterThanOrEqual(4)
    expect(await ui.find({ type: 'Image' })).toBe(undefined)
    await ui.unmount()
  })

  test('a narrow pane gets the text cards, not squeezed pictures', async ($, on) => {
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : FRAME_WIDE))
    await $.session.start(START)
    const ui = await $.ui.mount({ ...PANE_TARGET, props: { ...PANE_TARGET.props, bodyColumns: 26 } })
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Image' })).toBe(undefined)
    expect(await ui.find({ type: 'Text', text: 'Same page?' })).toBeDefined()
    await ui.unmount()
  })
})

describe('round 2', () => {
  test('a prompt that brings memory returns at once; the refresh runs on its own', async ($, on) => {
    let started = 0
    // Every engine call hangs: a hook that waited for the refresh would never return.
    engine(on, true, () => {
      started++
      return new Promise<string | null>(() => undefined)
    })
    on('classic.UserPromptSubmit', () => ({ additionalContext: ['CSR recap [[CSR:RECAP]]'] }) as never)
    await $.session.start(START)
    const after = (ms: number, value: string) => new Promise(resolve => (globalThis as unknown as { setTimeout: (f: () => void, ms: number) => void }).setTimeout(() => resolve(value), ms))
    const got = await Promise.race([$.classic.UserPromptSubmit({ prompt: 'hi' } as never), after(1500, 'late')])
    expect(got).not.toBe('late')
    // The refresh did start, and is still hanging, after the hook had returned.
    await after(200, '')
    expect(started).toBeGreaterThan(0)
  })

  test('a tall card is cut into bands on row lines, each under the Image limit', () => {
    const f = emptyFacts()
    f.replies = Array.from({ length: 50 }, (_, i) => [T(100 - i), 1000 * i] as [number, number])
    const card = timelineCard(f, 60 * COLUMN, COLUMN * 2.43, DARK, STILL, NOW)
    const frame = paint(card.scene, 14 / COLUMN)
    const bands = bandsOf('timeline', frame, card.rows)
    expect(bands.reduce((n, b) => n + b.rows, 0)).toBe(card.rows)
    expect(bands.reduce((n, b) => n + b.frame.height, 0)).toBe(frame.height)
    expect(bands.every(b => b.rows <= BAND_ROWS && b.frame.rgba.length <= 2 * 1024 * 1024)).toBe(true)
    expect(bands.map(b => b.key).slice(0, 2)).toEqual(['timeline-0', 'timeline-1'])
    // Each band is about its rows' share of the pixels: the seams sit on row lines.
    for (const b of bands) expect(Math.abs(b.frame.height - (b.rows * frame.height) / card.rows)).toBeLessThan(1)
  })

  test('the aurora runs on across cards: each card paints the page from its own offset', () => {
    const cards = buildCards({ pieces: pieces(), facts: null, hints: { same: '', should: '' } }, 42, 2.43, DARK, STILL, NOW)
    const grounded = withBackdrop(cards, DARK)
    const tops = grounded.map(c => (c.painted.scene.shapes[0] as { top: number }).top)
    let at = 0
    cards.forEach((c, i) => {
      expect(tops[i]).toBe(at)
      at += c.painted.scene.height
    })
    // The last pixel row of one card and the first of the next are the same colour where no card covers them.
    const a = paint(grounded[0]!.painted.scene, 1)
    const b = paint(grounded[1]!.painted.scene, 1)
    const px = (f: typeof a, y: number) => Array.from(f.rgba.slice((y * f.width + 1) * 4, (y * f.width + 1) * 4 + 3))
    const lastA = px(a, a.height - 1)
    const firstB = px(b, 0)
    lastA.forEach((v, i) => expect(Math.abs(v - (firstB[i] ?? 0))).toBeLessThanOrEqual(2))
  })

  test('the ring steps five times a second, stops after its allowance, and never without a seat', () => {
    resetMotion()
    const p = pieces()
    const dreaming = { ...p, dreamer: p.dreamer && { ...p.dreamer, dreaming: true } }
    retarget(dreaming, NOW)
    expect(frameDelay(NOW)).toBe(SPIN_FRAME_MS)
    expect(frameDelay(NOW + SPIN_LIMIT_MS + 1)).toBe(undefined)
    setPlaced(false)
    expect(frameDelay(NOW)).toBe(undefined)
    resetMotion()
  })

  test('the code row keeps the file name and the symbol, cutting the path from the left', () => {
    const file = 'mods/csr-pane/hooks/picture.ts'
    expect(codeLabel(file, 'drawPicture', 2000)).toBe('mods/csr-pane/hooks/picture.ts · drawPicture')
    const tight = codeLabel(file, 'drawPicture', textWidth('…/hooks/picture.ts · drawPicture', 23, 'sans') + 1)
    expect(tight).toBe('…/hooks/picture.ts · drawPicture')
    expect(codeLabel(file, 'drawPicture', textWidth('picture.ts · drawPicture', 23, 'sans') + 1)).toBe('picture.ts · drawPicture')
  })

  test('no line holds one word alone where the words can be balanced', () => {
    const lines = wrap('Early in the week. Weekly is at 9% so far.', 25, 'serif', textWidth('Early in the week. Weekly is at 9% so', 25, 'serif') + 2, 2)
    expect(lines.length).toBe(2)
    expect(lines[1]?.includes(' ')).toBe(true)
  })

  test('the agents row says how many run and the first one’s task', () => {
    const card = agentsCard(['Opus builds pane mod', 'Sonnet reviews'], 42 * COLUMN, COLUMN * 2.43, DARK)
    expect(card.alt).toBe('2 running · Opus builds pane mod (+1)')
  })

  test('the transcript keeps its lists and its ids bounded', () => {
    const r = newReader('/x.jsonl')
    r.facts.prompts = Array.from({ length: 5000 }, (_, i) => i)
    for (let i = 0; i < 2000; i++) r.ids.add(`m${i}`)
    thin(r)
    expect(r.facts.prompts.length).toBeLessThanOrEqual(2501)
    expect(r.facts.prompts.at(-1)).toBe(4999)
    expect(r.ids.size).toBeLessThanOrEqual(512)
    expect(r.ids.has('m1999')).toBe(true)
  })
})

describe('round 3', () => {
  test('in iTerm2 a card is one picture; a card over the Image limit still goes in bands', () => {
    const small = { width: 588, height: 300, rgba: new Uint8Array(588 * 300 * 4) }
    expect(bandRowsFor(small, 9, false)).toBe(9)
    expect(bandRowsFor(small, 9, true)).toBe(BAND_ROWS)
    const huge = { width: 1000, height: 600, rgba: new Uint8Array(1000 * 600 * 4) }
    expect(bandRowsFor(huge, 18, false)).toBe(BAND_ROWS)
  })

  test('a page keeps the PNG bytes of all its pictures under the tree limit, dropping whole cards from the end', () => {
    const b = (id: string, kb: number) => ({ id, source: { png: 'A'.repeat(Math.ceil((kb * 1024 * 4) / 3)) } })
    const page = [b('dreamer', 600), b('limits', 600), b('timeline', 600), b('timeline', 600)]
    const { kept, bytes } = withinTree(page)
    expect(kept.map(k => k.id)).toEqual(['dreamer', 'limits'])
    expect(bytes).toBeLessThanOrEqual(MAX_TREE_BYTES)
    expect(withinTree(page.slice(0, 2)).kept.length).toBe(2)
  })

  test('the real Now page is well under the tree limit', () => {
    const f = emptyFacts()
    f.replies = Array.from({ length: 200 }, (_, i) => [T(300 - i), 1000 * i] as [number, number])
    const cards = withBackdrop(buildCards({ pieces: pieces(), facts: f, hints: { same: '', should: '' }, agents: ['x'] }, 60, 2.43, DARK, STILL, NOW), DARK)
    let total = 0
    for (const c of cards) total += toPng(paint(c.painted.scene, 14 / COLUMN)).length
    expect(total).toBeLessThan(MAX_TREE_BYTES)
  })
})

describe('the cell view (round 4)', () => {
  const model = (facts = null as ReturnType<typeof emptyFacts> | null) => ({ pieces: pieces(), facts, hints: { same: '4 opens', should: '4 opens' }, agents: ['Opus builds'] })
  const gridOf = (p = DARK, facts: ReturnType<typeof emptyFacts> | null = null) =>
    gridCards(model(facts), 42, 2.2, p, STILL, NOW).map(c => ({ id: c.id, cells: toCells(c.painted.scene, COLUMN * 2.2, p.cells) }))
  const find = (g: ReturnType<typeof gridOf>[number], text: string) => cellText(g.cells).findIndex(l => l.includes(text))

  test('every card lands on the grid: one row a line, words in the terminal font, 42 columns', () => {
    const grid = gridOf()
    expect(grid.map(g => g.id)).toEqual(['dreamer', 'same', 'should', 'story', 'limits', 'agents'])
    for (const g of grid) expect(g.cells.columns).toBe(42)
    const dreamer = grid[0]!
    expect(find(dreamer, "Self-Reflect's dreamer")).toBeGreaterThanOrEqual(0)
    expect(find(dreamer, '607k of 5.9M')).toBe(find(dreamer, "Self-Reflect's dreamer") + 2)
  })

  test('words sit on the glass card fill, in the palette colours', () => {
    const g = gridOf()[0]!
    const row = find(g, 'Self-Reflect')
    const col = cellText(g.cells)[row]!.indexOf('S')
    const i = row * g.cells.columns + col
    expect(g.cells.fg[i]).toBe(DARK.ink)
    // The glass over the aurora: darker than the ink, not the terminal default.
    const bg = g.cells.bg[i]!
    expect(bg & 0x01000000).toBe(0)
    expect(((bg >> 16) & 255) + ((bg >> 8) & 255) + (bg & 255)).toBeLessThan(200)
  })

  test('a bar is the lower half of its row, fill then track, so stacked bars stay apart', () => {
    const limits = gridOf().find(g => g.id === 'limits')!
    const lines = cellText(limits.cells)
    const row = lines.findIndex(l => l.includes('Codex 7D'))
    const line = lines[row]!
    const start = line.indexOf('Codex 7D') + 'Codex 7D'.length
    const bar = line.slice(start).replace(/\s*22%\s*$/, '').trim()
    expect(/^▄+$/.test(bar)).toBe(true)
    const first = row * limits.cells.columns + line.indexOf('▄')
    const last = row * limits.cells.columns + line.lastIndexOf('▄')
    expect(limits.cells.fg[first]).toBe(DARK.mint)
    expect(limits.cells.fg[last]).toBe(DARK.track)
  })

  test('the ground is flat: a gap row, a lighter top row, the card fill, a ground column each side', () => {
    const g = gridOf()[0]!.cells
    const at = (r: number, c: number) => g.bg[r * g.columns + c]
    expect(at(0, 10)).toBe(DARK.cells.ground)
    expect(at(1, 10)).toBe(DARK.cells.top)
    expect(at(3, 10)).toBe(DARK.cells.card)
    expect(at(3, 0)).toBe(DARK.cells.ground)
    expect(at(3, g.columns - 1)).toBe(DARK.cells.ground)
    expect(new Set(Array.from(g.glyphs)).has(0x2580)).toBe(false)
  })

  test('title rows are bold: the card is cut into Raster runs and bold Text rows', () => {
    const g = gridOf()[0]!.cells
    const segs = segmentsOf(g, true)
    expect(segs[0]).toEqual({ kind: 'cells', from: 0, rows: 2 })
    expect(segs[1]).toEqual({ kind: 'text', row: 2 })
    expect(rowRuns(g, 2).some(r => r.bold && r.text.includes("Self-Reflect's dreamer"))).toBe(true)
    expect(segmentsOf(g, false)).toEqual([{ kind: 'cells', from: 0, rows: g.rows }])
  })

  test('a timeline column is eighth blocks, bottom up', () => {
    expect(columnGlyphs(0, 4)).toEqual([' ', ' ', ' ', ' '])
    expect(columnGlyphs(1, 4)).toEqual(['█', '█', '█', '█'])
    expect(columnGlyphs(0.5, 4)).toEqual([' ', ' ', '█', '█'])
    expect(columnGlyphs(0.3, 4)).toEqual([' ', ' ', '▂', '█'])
  })

  test('the timeline card on the grid has the chart and its marks', () => {
    const f = emptyFacts()
    f.prompts = [T(60), T(5)]
    f.replies = [[T(59), 100_000], [T(30), 300_000], [T(4), 80_000]]
    f.compacts = [T(10)]
    f.recalls = [T(4)]
    const t = gridOf(DARK, f).find(g => g.id === 'timeline')!
    const text = cellText(t.cells).join('\n')
    expect(text).toContain('█')
    expect(text).toContain('┃')
    expect(text).toContain('╷')
  })

  test('light theme cells use the light inks and grounds', () => {
    const g = gridOf(LIGHT)[0]!
    const row = find(g, 'Self-Reflect')
    const i = row * g.cells.columns + cellText(g.cells)[row]!.indexOf('S')
    expect(g.cells.fg[i]).toBe(LIGHT.ink)
    const bg = g.cells.bg[i]!
    expect(bg).toBe(LIGHT.cells.card)
  })

  test('auto lands on cells in Warp, where a picture frame was refused, or without an Image', () => {
    expect(cellsWanted('auto', undefined, 'WarpTerminal', false, true)).toBe(true)
    expect(cellsWanted('auto', undefined, 'WarpTerminal', true, true)).toBe(false)
    expect(cellsWanted('auto', false, 'iTerm.app', false, true)).toBe(true)
    expect(cellsWanted('auto', undefined, 'ghostty', false, true)).toBe(false)
    expect(cellsWanted('picture', false, 'WarpTerminal', false, true)).toBe(false)
    expect(cellsWanted('cells', true, 'ghostty', false, true)).toBe(true)
  })

  test('with cells chosen the pane draws Rasters (titles too, as Warp needs), and no Image', { options: { view: 'cells' } }, async ($, on) => {
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : FRAME_WIDE))
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Raster', key: 'dreamer-c0' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: "Self-Reflect's dreamer" })).toBe(undefined)
    expect(await ui.find({ type: 'Image' })).toBe(undefined)
    await ui.unmount()
  })
})

describe('round 5', () => {
  test('above the prompt, with cells, the pane is the compact band: weekly, context, Self-Reflect', { options: { view: 'cells' } }, async ($, on) => {
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : argv.includes('--compact-plan') ? '{"story":{"by_csr":2,"payback":900000}}' : FRAME_WIDE))
    await $.session.start(START)
    const ui = await $.ui.mount({ ...PANE_TARGET, props: { ...PANE_TARGET.props, placement: 'inline', bodyColumns: 100, scroll: { offset: 0, bodyRows: 8 } } } as never)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Raster', key: 'band-c' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Same page?' })).toBe(undefined)
    await ui.unmount()
  })
})

describe('round 6', () => {
  test('with cells, the Dreams tab sits on the cell ground too', { options: { view: 'cells' } }, async ($, on) => {
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : FRAME_WIDE))
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await ui.press({ key: 'tab-dreams' })
    const grounds = (await ui.findAll({ type: 'Box' })).filter(b => (b as { props?: { backgroundColor?: string } }).props?.backgroundColor === '#0f1115')
    expect(grounds.length).toBeGreaterThan(0)
    await ui.unmount()
  })
})

describe('round 7: one pane per session', () => {
  const herdrEnv = (on: Parameters<typeof engine>[0], live: boolean, inHerdr = true) => {
    on('env.get', (_, e) => {
      const name = (e as { name: string }).name
      if (name === 'HOME') return { value: '/home/me' }
      if (inHerdr && name === 'HERDR_PANE_ID') return { value: 'w1:p2' }
      return { value: undefined }
    })
    on('fs.read', (_, e) => ((e as { path: string }).path.endsWith('/csr-companion/panes/sid-1.json') ? { value: '{"pid":4242}' } : { deny: 'missing' }) as never)
    return (argv: readonly string[]) =>
      argv[0] === '/bin/ps' ? (live ? '/usr/local/bin/csr-engine companion pane --sid sid-1\n' : '') : argv.includes('--json') ? SUMMARY : FRAME_WIDE
  }

  test('the rule: herdr always quiet, mod never, auto quiet in herdr while its pane is live or may still open', () => {
    expect(standsDown('herdr', { inHerdr: false, live: false }, 0)).toBe(true)
    expect(standsDown('mod', { inHerdr: true, live: true }, 0)).toBe(false)
    expect(standsDown('auto', { inHerdr: false, live: false }, 0)).toBe(false)
    expect(standsDown('auto', { inHerdr: true, live: true }, 999_999)).toBe(true)
    expect(standsDown('auto', { inHerdr: true, live: false }, 1000)).toBe(true)
    expect(standsDown('auto', { inHerdr: true, live: false }, HERDR_GRACE_MS + 1)).toBe(false)
    expect(pidOf('{"pid":77287}')).toBe(77287)
    expect(pidOf('nonsense')).toBe(undefined)
  })

  test('in herdr with its pane live, the mod opens no pane and draws no band', async ($, on) => {
    const opens: number[] = []
    const run = herdrEnv(on, true)
    engine(on, false, run, { opens })
    await $.session.start(START)
    expect(opens.length).toBe(0)
    const band = await $.ui.mount(BAND_TARGET)
    expect(await band.find({ type: 'Button', key: 'open' })).toBe(undefined)
    await band.unmount()
  })

  test('an ask still wins: /csr-pane opens the pane inside herdr', async ($, on) => {
    const opens: number[] = []
    engine(on, true, herdrEnv(on, true), { opens })
    await $.session.start(START)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(opens.length).toBe(1)
  })

  test('outside herdr, or with pane set to mod, the mod opens its pane', { options: { pane: 'mod' } }, async ($, on) => {
    const opens: number[] = []
    engine(on, false, herdrEnv(on, true), { opens })
    await $.session.start(START)
    expect(opens.length).toBe(1)
  })

  test('with pane set to herdr the mod stays quiet even outside herdr', { options: { pane: 'herdr' } }, async ($, on) => {
    const opens: number[] = []
    engine(on, false, herdrEnv(on, false, false), { opens })
    await $.session.start(START)
    expect(opens.length).toBe(0)
  })
})

describe('round 7/8: cards from the engine, and cards the mod has no painter for', () => {
  const golden = PANE_V1 as unknown as { schema: number; cards: unknown[] }
  const withCards = (extra: object) => parseSummary(JSON.stringify({ ...JSON.parse(SUMMARY), ...extra }))
  const frame = FRAME.replace('API credit', 'Last time here · 11m ago\nyou asked: Our CSR pane work has been herdr-first.\nAPI credit')

  test('the old engine (no schema): an unpainted frame section draws as a plain card', () => {
    const p = piecesOf(parseFrame(frame), 160, usage, plan, parseSummary(SUMMARY), null, NOW)
    expect(p.extra.map(c => c.title)).toEqual(['Last time here · 11m ago'])
    expect(p.dreamer?.budget).toBe('607k of 5.9M')
  })

  test('the golden contract (csr-engine contract_tests writes it): the mod paints the engine\'s words, not its own', () => {
    const summary = withCards(golden)
    // No frame at all: the cards come from the contract alone.
    const p = piecesOf(null, 160, usage, plan, summary, null, NOW)
    expect(p.dreamer?.title).toBe("Self-Reflect's dreamer")
    expect(p.dreamer?.budget).toBe('607k of 5.9M')
    expect(p.dreamer?.lastLook).toBe('The last look found nothing. 4 of 45 looks end this way.')
    expect(p.samePage?.read).toBe("Model's read: 3 of 9 asks missed. You were unclear on 1.")
    expect(p.shouldBe.map(x => x.kind)).toEqual(['memory', 'skill'])
    expect(p.story?.notes).toBe('1 note kept')
    expect(p.limits?.headline).toBe('Early in the week. Weekly is at 7% so far.')
    expect(p.limits?.bars).toEqual([
      { name: 'Weekly', percent: 7 },
      { name: 'Codex 7D', percent: 22 },
    ])
    const said = buildCards({ pieces: p, facts: null, hints: { same: '', should: '' } }, 42, 2.43, DARK, STILL, NOW)
      .flatMap(c => c.painted.scene.shapes.flatMap(sh => (sh.kind === 'text' ? [sh.text] : [])))
      .join(' ')
    for (const w of ["Self-Reflect's dreamer", 'Same page?', 'Should be a skill / memory / tool', 'Early in the week.', 'Codex 7D', '22%']) expect(said).toContain(w)
  })

  test('stage 2: the context window and Self-Reflect\'s line come from the engine, as it words them', () => {
    const p = piecesOf(null, 160, null, null, withCards(golden), null, NOW)
    expect(p.context?.parts.map(x => x.name)).toEqual(['Summary + setup', 'Messages', 'Tool results'])
    expect(p.context?.mix?.verdict).toBe('context-heavy')
    expect([p.context?.used, p.context?.max, p.context?.free]).toEqual([130000, 1000000, 870000])
    expect(p.csr).toBe('3 compactions · ~1.8M tokens saved (est.)')
    expect(parseSummary(JSON.stringify({ ...JSON.parse(SUMMARY), herdr_pane_live: true }))?.herdrPaneLive).toBe(true)
  })

  test('a contract card with an id the mod does not know draws plainly in pixels, SVG and cells', () => {
    const summary = withCards({ ...golden, cards: [...golden.cards, { id: 'bench', title: 'Bench run', lines: [{ text: 'p95 1.2 s', style: 'body' }] }] })
    const p = piecesOf(null, 160, usage, plan, summary, null, NOW)
    expect(p.extra.map(c => c.title)).toEqual(['Bench run'])
    const m = { pieces: p, facts: null, hints: { same: '', should: '' } }
    const px = buildCards(m, 42, 2.43, DARK, STILL, NOW)
    const bench = px.find(c => c.painted.alt.startsWith('Bench run'))!
    expect(toSvg(bench.painted.scene)).toContain('>Bench run</text>')
    const grid = gridCards(m, 42, 2.2, DARK, STILL, NOW).find(c => c.id === bench.id)!
    expect(cellText(toCells(grid.painted.scene, COLUMN * 2.2, DARK.cells)).join('\n')).toContain('p95 1.2 s')
  })

  test('a newer major shape: the mod draws what it can and one dim line says to update', () => {
    const p = piecesOf(null, 160, usage, plan, withCards({ ...golden, schema: '2.0' }), null, NOW)
    expect(p.isOutdated).toBe(true)
    const cards = buildCards({ pieces: p, facts: null, hints: { same: '', should: '' } }, 42, 2.43, DARK, STILL, NOW)
    expect(cards.at(-1)?.id).toBe('update')
    expect(cards.some(c => c.id === 'dreamer')).toBe(true)
    expect(majorOf(3)).toBe(3)
    expect(majorOf('x')).toBe(undefined)
  })

  test('with the engine\'s cards the pane asks for no text frame', async ($, on) => {
    const argvs: string[][] = []
    engine(on, true, argv => {
      argvs.push([...argv])
      if (argv.includes('--cards')) return JSON.stringify({ ...JSON.parse(SUMMARY), ...golden })
      return argv.includes('--json') ? SUMMARY : FRAME_WIDE
    })
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    const jsons = argvs.filter(a => a.includes('--json'))
    // The pane asks for the cards; the engine words them, so no text frame is run at all.
    expect(jsons.length).toBe(2)
    expect(jsons.every(a => a.includes('--cards'))).toBe(true)
    expect(argvs.filter(a => a.includes('--frame')).length).toBe(0)
    expect(await ui.find({ type: 'Image', key: 'dreamer-0' })).toBeDefined()
    await ui.unmount()
  })

  test('an engine that refuses --cards: the plain summary and a text frame, and no failure shown', { options: { enginePath: '/old/csr-engine' } }, async ($, on) => {
    const argvs: string[][] = []
    engine(on, true, argv => {
      argvs.push([...argv])
      // The installed engine before the pane contract: its argument parser refuses the flag.
      if (argv.includes('--cards')) return { exitCode: 2, stdout: '', stderr: "error: unexpected argument '--cards' found\n\nUsage: csr-engine companion pane --json" }
      return argv.includes('--json') ? SUMMARY : FRAME_WIDE
    })
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    // Asked once; after the refusal this engine gets plain --json and the frame at every refresh.
    expect(argvs.filter(a => a.includes('--cards')).length).toBe(1)
    expect(argvs.filter(a => a.includes('--json') && !a.includes('--cards')).length).toBe(2)
    expect(argvs.filter(a => a.includes('--frame')).length).toBe(2)
    expect(await ui.find({ type: 'Text', text: /did not answer/ })).toBe(undefined)
    expect(await ui.find({ type: 'Image', key: 'dreamer-0' })).toBeDefined()
    await ui.unmount()
  })

  test('the band alone asks plain --json: the cards are asked only where the pane draws', async ($, on) => {
    const argvs: string[][] = []
    engine(on, false, argv => {
      argvs.push([...argv])
      return argv.includes('--json') ? SUMMARY : FRAME_WIDE
    })
    on('turn.complete', () => ({ text: '' }))
    await $.session.start(START)
    await $.turn.complete({ answer: '', durationMs: 1, reason: 'answer' } as never)
    for (let i = 0; i < 50 && argvs.length === 0; i++) await new Promise(resolve => setTimeout(resolve, 20))
    expect(argvs.length).toBeGreaterThan(0)
    expect(argvs.every(a => !a.includes('--cards'))).toBe(true)
  })

  test('a failure that is not a refusal still says so', { options: { enginePath: '/broken/csr-engine' } }, async ($, on) => {
    engine(on, true, argv => (argv.includes('--cards') ? { exitCode: 1, stdout: '', stderr: 'database is locked' } : FRAME_WIDE))
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Text', text: /did not answer \(exit 1\)/ })).toBeDefined()
    await ui.unmount()
  })
})

describe('round 7: one card style on every tab in the cell view', () => {
  test('Dreams cards are flat cards on the cell ground, the Button still drafts the what-if', { options: { view: 'cells' } }, async ($, on) => {
    const fills: string[] = []
    engine(on, true, argv => (argv.includes('--json') ? SUMMARY : FRAME_WIDE), { fills })
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await ui.press({ key: 'tab-dreams' })
    const boxes = (await ui.findAll({ type: 'Box' })) as { props?: { backgroundColor?: string; borderStyle?: string } }[]
    expect(boxes.some(b => b.props?.backgroundColor === '#272c35')).toBe(true)
    expect(boxes.some(b => b.props?.borderStyle === 'round')).toBe(false)
    await ui.press({ key: 'draft-0' })
    expect(fills.length).toBe(1)
    await ui.unmount()
  })
})
