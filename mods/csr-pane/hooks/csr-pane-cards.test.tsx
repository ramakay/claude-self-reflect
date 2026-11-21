import { describe, expect, test } from 'claude-code/testing'

import { cardRows, columnCount, columnsOf, toCards } from './cards'
import { answer, engine, FRAME_WIDE, PANE_TARGET } from './kit'
import { parseFrame } from './sgr'

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true } as const
const plainOf = (row: { text: string }[]) => row.map(s => s.text).join('')
const wideFrame = (argv: readonly string[]) => (argv.includes('--json') ? answer(argv) : FRAME_WIDE)

describe('the cards', () => {
  const cards = toCards(parseFrame(FRAME_WIDE), 160)

  test('each engine heading is a card, and the key row is gone', () => {
    expect(cards.map(c => c.title)).toEqual([
      '',
      'Same page?',
      'Should be a skill / memory / tool',
      'Lost and found',
      'Story · refreshed 2m ago',
      'Cost and limits',
    ])
    expect(cards.flatMap(c => c.paras.map(plainOf)).some(t => /q close/.test(t))).toBe(false)
  })

  test('a row the engine wrapped is one paragraph again; separate rows stay apart', () => {
    const page = cards[1]!
    expect(page.paras.length).toBe(1)
    expect(plainOf(page.paras[0]!)).toMatch(/ahead of the branch you named, and kept building\.$/)
    expect(cards[2]!.paras.map(plainOf)).toEqual([
      'memory: The pane width rule should be a dated memory note. · evidence',
      'skill: A clean probe for a live mod was rebuilt by hand several times. · evidence',
    ])
    expect(cards[5]!.paras.map(plainOf)).toEqual(['API credit $59.04 left', 'CTX 161k/1M · 1 by CSR', '$2.36 this week'])
  })

  test('a frame with no known heading is one card with every row', () => {
    const one = toCards(parseFrame('first row\nsecond row'), 160)
    expect(one.length).toBe(1)
    expect(one[0]!.paras.map(plainOf)).toEqual(['first row', 'second row'])
  })

  test('the rows of a card count its frame, title and wrapped text', () => {
    expect(cardRows({ title: 'T', paras: [[{ text: 'x'.repeat(50) }]] }, 20)).toBe(6)
    expect(cardRows({ title: '', paras: [[{ text: 'short' }]] }, 20)).toBe(3)
  })

  test('columns: one beside the chat, two or three above the prompt, in reading order', () => {
    expect(columnCount('dock', 200)).toBe(1)
    expect(columnCount('inline', 80)).toBe(1)
    expect(columnCount('inline', 100)).toBe(2)
    expect(columnCount('inline', 160)).toBe(3)
    const blocks = [4, 4, 4, 4].map((rows, node) => ({ rows, node }))
    expect(columnsOf(blocks, 2).map(c => c.map(b => b.node))).toEqual([
      [0, 1],
      [2, 3],
    ])
    expect(columnsOf(blocks, 1).length).toBe(1)
  })
})

describe('the tabs', () => {
  test('the Now tab paints each herdr card as its own picture, and no text card under them', async ($, on) => {
    engine(on, true, wideFrame)
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    for (const key of ['dreamer', 'same', 'should', 'story', 'limits']) expect(await ui.find({ type: 'Image', key: `${key}-0` })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Same page?' })).toBe(undefined)
    expect(await ui.find({ type: 'Text', text: /q close/ })).toBe(undefined)
    await ui.unmount()
  })

  test('with text chosen the Now tab draws each engine heading as a card title', { options: { view: 'text' } }, async ($, on) => {
    engine(on, true, wideFrame)
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Image' })).toBe(undefined)
    expect(await ui.find({ type: 'Text', text: 'Same page?' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Lost and found' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Cost and limits' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /q close/ })).toBe(undefined)
    await ui.unmount()
  })

  test('the Notes tab says in full what the cards cut short', async ($, on) => {
    engine(on, true, wideFrame)
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await ui.press({ key: 'tab-notes' })
    expect(await ui.find({ type: 'Text', text: /named, and kept building/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /skill: A clean probe/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /Answer the discussion question/ })).toBeDefined()
    await ui.unmount()
  })

  test('the Dreams tab draws each dream, and its Button drafts the what-if in the prompt', async ($, on) => {
    const fills: string[] = []
    engine(on, true, answer, { fills })
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await ui.press({ key: 'tab-dreams' })
    expect(await ui.find({ type: 'Text', text: 'Haiku could translate the question first' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /What if Haiku rewrote/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /Nov 1\d · You marked it helpful\./ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /The dreamer looks as you work/ })).toBe(undefined)
    // The newest dream is first.
    await ui.press({ key: 'draft-0' })
    expect(fills).toEqual(['What if Haiku rewrote each recall question first?'])
    await ui.unmount()
  })

  test('the Changed tab draws the sessions whose code changed since', async ($, on) => {
    engine(on, true)
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    await ui.press({ key: 'tab-changes' })
    expect(
      await ui.find({ type: 'Text', text: 'Code from 7 past sessions changed after you talked about it.' }),
    ).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Please review last session' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Talked Nov 10 · 2 commits since' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'changed Scratch::run_env · uninstall.rs' })).toBeDefined()
    await ui.unmount()
  })

  test('above the prompt at 120 columns every card still draws', async ($, on) => {
    engine(on, true, wideFrame)
    await $.session.start(START)
    const inline = { ...PANE_TARGET, props: { ...PANE_TARGET.props, bodyColumns: 120, placement: 'inline' } } as const
    const ui = await $.ui.mount(inline)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Text', text: 'Same page?' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: 'Cost and limits' })).toBeDefined()
    await ui.unmount()
  })
})
