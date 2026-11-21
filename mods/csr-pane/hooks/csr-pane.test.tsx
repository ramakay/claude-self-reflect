import { describe, expect, test } from 'claude-code/testing'

import { FRAME_40 } from './frame.fixture'
import { answer, BAND_TARGET, engine, PANE_TARGET, USAGE } from './kit'
import { applySgr, parseFrame, parseLine, trimFrame } from './sgr'
import { barCells, labelRows, sentence, toUsage } from './usage'

const plainOf = (row: { text: string }[]) => row.map(s => s.text).join('')

describe('the SGR parser', () => {
  test('a recorded frame parses with no escape left', () => {
    const rows = parseFrame(FRAME_40)
    const all = rows.map(plainOf)
    expect(all[0]).toBe('☾ The dreamer looks as you work.')
    expect(all[all.length - 1]).toBe('$1.01 this week')
    expect(all.some(t => /q close/.test(t))).toBe(false)
    expect(all.some(t => t.includes('\x1b'))).toBe(false)
    // The body colour is left to the theme; a departure keeps its own.
    expect(rows[0][0].color).toBe(undefined)
    const csr = rows.flat().find(s => s.text.startsWith('CSR 0.04'))
    expect(csr?.color).toBe('#ff97d1')
  })

  test('codes it knows set style; codes it does not are ignored', () => {
    expect(applySgr({}, '1;2;31')).toEqual({ bold: true, dim: true, color: '#cd3131' })
    expect(applySgr({ bold: true }, '0')).toEqual({})
    expect(applySgr({}, '38;5;196')).toEqual({ color: '#ff0000' })
    expect(applySgr({}, '48;2;1;2;3;4;53')).toEqual({})
    expect(parseLine('\x1b[1mHi\x1b[0m \x1b[?25lthere\x1b]0;title\x07   ')).toEqual([
      { text: 'Hi', bold: true },
      { text: ' there' },
    ])
  })

  test('the trim drops blank rows at both ends and the key row', () => {
    const raw = ['\x1b[48;2;1;1;1m    \x1b[0m', 'one', '', 'two', '   ', ' t monologue  i figure  b theme  q close', ''].join(
      '\n',
    )
    expect(trimFrame(raw)).toEqual(['one', '', 'two'])
    expect(trimFrame('keep q closed doors')).toEqual(['keep q closed doors'])
  })
})

describe('the figure', () => {
  test('the bar fills the body and the labels name every segment', () => {
    const usage = toUsage(USAGE as never)
    expect(usage?.segments.map(s => s.name)).toEqual([
      'System prompt',
      'Tools',
      'Messages',
      'Free space',
      'Autocompact buffer',
    ])
    expect(sentence(usage!)).toBe('Context 290k of 1M used. 677k free.')
    for (const cols of [24, 40, 60]) {
      const cells = barCells(usage!.segments, cols)
      expect(cells.reduce((a, b) => a + b, 0)).toBe(cols)
      expect(cells.every(c => c >= 1)).toBe(true)
      const rows = labelRows(usage!.segments, cols)
      const labels = rows.flat().map(l => l.text)
      for (const s of usage!.segments) expect(labels.some(l => l.startsWith(s.name + ' '))).toBe(true)
      for (const row of rows) expect(row.map(l => l.text).join(' · ').length <= cols || row.length === 1).toBe(true)
    }
  })
})

describe('the band', () => {
  test('shows one row with the status when the pane has no seat', async ($, on) => {
    engine(on, false)
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    const ui = await $.ui.mount(BAND_TARGET)
    const row = await ui.find({ type: 'Text', text: /Self-Reflect remembered/ })
    expect(row).toBeDefined()
    expect(await ui.find({ type: 'Button', text: /Open pane/ })).toBeDefined()
    await ui.unmount()
  })

  test('draws nothing of its own when the pane is placed', async ($, on) => {
    engine(on, true)
    on('ui.render', ($, e) => {
      const { Text } = $.ui.resolve(e)

      return <Text>engine band</Text>
    })
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    const ui = await $.ui.mount(BAND_TARGET)
    expect(await ui.find({ type: 'Button', text: /Open pane/ })).toBe(undefined)
    expect((await ui.find({ type: 'Text', text: /engine band/ }))?.text).toBe('engine band')
    await ui.unmount()
  })
})

describe('the pane', () => {
  test('with the drawing off, the Now tab draws the context card and the engine frame', { options: { view: 'text' } }, async ($, on) => {
    engine(on, true)
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Text', text: 'Context 290k of 1M used. 677k free.' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /Messages 245k/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /Weekly limit/ })).toBe(undefined)
    expect(await ui.find({ type: 'Text', text: /The dreamer looks as you work/ })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /q close/ })).toBe(undefined)
    // The dreams have their own tab.
    expect(await ui.find({ type: 'Text', text: /What if Haiku rewrote/ })).toBe(undefined)
    await ui.unmount()
  })

  test('a failed engine keeps the last frame and says so in words', async ($, on) => {
    let fail = false
    engine(on, true, argv => {
      if (fail) return null

      return answer(argv)
    })
    await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    fail = true
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Image', key: 'dreamer-0' })).toBeDefined()
    expect(await ui.find({ type: 'Text', text: /did not answer \(timeout\)/ })).toBeDefined()
    await ui.unmount()
  })
})
