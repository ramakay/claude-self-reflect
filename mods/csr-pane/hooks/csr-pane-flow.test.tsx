import { describe, expect, test } from 'claude-code/testing'

import { answer, BAND_TARGET, engine, PANE_TARGET } from './kit'
import type { Extra } from './kit'

const START = { cwd: '/repo', surface: 'terminal', isInteractive: true } as const
const TURN = { answer: '', durationMs: 1, reason: 'answer' } as never

const wait = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))

/** Polls until the check holds or 1 s has passed. */
async function until(check: () => boolean): Promise<void> {
  for (let i = 0; i < 50 && !check(); i++) await wait(20)
}

/** The engine fake: every argv the process hook received is kept. */
function spy(extra: Extra = {}) {
  const argvs: string[][] = []
  const run = (argv: readonly string[]) => {
    argvs.push([...argv])

    return answer(argv)
  }

  return { argvs, run, extra }
}

describe('the band presses', () => {
  test('Open pane opens the pane and refreshes', async ($, on) => {
    const { argvs, run, extra } = spy({ opens: [] })
    engine(on, false, run, extra)
    await $.session.start(START)
    // The start refreshes nothing here (timers are refused), so any run below is the press.
    const before = argvs.length
    const opensBefore = extra.opens!.length
    const ui = await $.ui.mount(BAND_TARGET)
    await ui.press({ key: 'open' })
    await until(() => argvs.some(a => a.includes('--json')))
    expect(extra.opens!.length).toBe(opensBefore + 1)
    expect(argvs.length).toBeGreaterThan(before)
    expect(argvs.some(a => a.includes('--json'))).toBe(true)
    await ui.unmount()
  })

  test('hide sets the hidden state and writes the store', async ($, on) => {
    const { run, extra } = spy({ sets: [] })
    engine(on, false, run, extra)
    await $.session.start(START)
    const ui = await $.ui.mount(BAND_TARGET)
    expect(await ui.find({ type: 'Button', text: /hide/ })).toBeDefined()
    await ui.press({ key: 'hide' })
    expect(extra.sets).toContainEqual(['band-hidden', true])
    // The band draws nothing of its own once hidden.
    expect(await ui.find({ type: 'Button', text: /Open pane/ })).toBe(undefined)
    await ui.unmount()
  })
})

describe('turn.complete', () => {
  test('starts a refresh when the pane is placed', async ($, on) => {
    const { argvs, run, extra } = spy()
    engine(on, true, run, extra)
    on('turn.complete', () => ({ text: '' }))
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    expect(argvs.length).toBe(0)
    await $.turn.complete(TURN)
    await until(() => argvs.length > 0)
    expect(argvs.some(a => a.includes('--json'))).toBe(true)
    await ui.unmount()
  })

  test('starts no engine when the pane is not placed and the band is hidden', async ($, on) => {
    const { argvs, run, extra } = spy({ hidden: true })
    engine(on, false, run, extra)
    on('turn.complete', () => ({ text: '' }))
    await $.session.start(START)
    await $.turn.complete(TURN)
    await wait(300)
    expect(argvs.length).toBe(0)
  })

  test('runs only the --json call when the pane is not placed and the band shows', async ($, on) => {
    const { argvs, run, extra } = spy()
    engine(on, false, run, extra)
    on('turn.complete', () => ({ text: '' }))
    await $.session.start(START)
    await $.turn.complete(TURN)
    await until(() => argvs.length > 0)
    await wait(100)
    expect(argvs.length).toBe(1)
    expect(argvs[0]).toContain('--json')
    expect(argvs[0]).not.toContain('--frame')
  })
})

describe('the engine call', () => {
  test('asks for one wide frame, whatever width the body has', async ($, on) => {
    const { argvs, run, extra } = spy()
    engine(on, true, run, extra)
    await $.session.start(START)
    const wider = { ...PANE_TARGET, props: { ...PANE_TARGET.props, bodyColumns: 63 } }
    const first = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    const frameArgv = () => argvs.filter(a => a.includes('--frame')).map(a => a[a.indexOf('--cols') + 1])
    expect(frameArgv().at(-1)).toBe('160')
    await first.unmount()
    const second = await $.ui.mount(wider)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(frameArgv().at(-1)).toBe('160')
    await second.unmount()
  })

  test('a userConfig enginePath is the program that runs', { options: { enginePath: '/opt/csr/csr-engine' } }, async ($, on) => {
    const { argvs, run, extra } = spy()
    engine(on, true, run, extra)
    await $.session.start(START)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(argvs.length).toBeGreaterThan(0)
    expect(argvs.every(a => a[0] === '/opt/csr/csr-engine')).toBe(true)
  })
})

describe('the figure', () => {
  test('with the drawing off and no frame, the figure draws the weekly limit itself', { options: { view: 'text' } }, async ($, on) => {
    engine(on, true, argv => (argv.includes('--frame') ? null : answer(argv)))
    await $.session.start(START)
    const ui = await $.ui.mount(PANE_TARGET)
    await $.command.run({ command: 'csr-pane', args: '' } as never)
    expect(await ui.find({ type: 'Text', text: /Weekly limit: 2% used/ })).toBeDefined()
    await ui.unmount()
  })
})
