import { FRAME_40 } from './frame.fixture'
import type { test } from 'claude-code/testing'

export const SUMMARY = JSON.stringify({
  status: '☾ CSR remembered 5× this session · 5 dreams this week',
  dreams: [
    { id: 'a', at: 1794584903, line: 'What if the film showed the what-if questions?', answer: 'ignored' },
    {
      id: 'b',
      at: 1794625576,
      line: 'What if Haiku rewrote each recall question first?',
      title: 'Haiku could translate the question first',
      answer: 'helpful',
    },
  ],
  changes_total: 7,
  changes: [
    {
      conversation: 'c1',
      title: 'Please review last session',
      current: false,
      talked: '2026-11-10',
      commits: 2,
      shifts: [
        {
          file: 'csr-engine/tests/uninstall.rs',
          symbol: 'Scratch::run_env',
          verb: 'changed',
          oid: 'c5da321',
          date: '2026-11-13',
          subject: 'fix(uninstall): remove the model cache',
        },
      ],
    },
  ],
})

// What a wide frame looks like: one row a paragraph, a long one wrapped at 160 columns.
export const FRAME_WIDE = [
  '☾ The dreamer looks as you work.',
  'dreamer 514k of 2M',
  'Same page?',
  "Model's read: 0 of 1 asks missed. You were clear. You gave a full brief; Claude followed its order, told you the installed build is ahead of the branch you",
  'named, and kept building.',
  'Should be a skill / memory / tool',
  'memory: The pane width rule should be a dated memory note. · evidence',
  'skill: A clean probe for a live mod was rebuilt by hand several times. · evidence',
  'Lost and found',
  'Answer the discussion question before any more building.',
  'Story · refreshed 2m ago',
  'A picture-mode draft is not installed.',
  'API credit $59.04 left',
  'CTX 161k/1M · 1 by CSR',
  '$2.36 this week',
  ' t monologue  i figure  o details  b theme  q close',
].join('\n')

export const USAGE = {
  startedAt: 0,
  context: {
    tokens: 330000,
    window: 1000000,
    percent: 33,
    breakdown: {
      categories: [
        { name: 'System prompt', tokens: 9000, color: 'promptBorder', isDeferred: false, kind: 'used' },
        { name: 'Tools', tokens: 36000, color: 'inactive', isDeferred: false, kind: 'used' },
        { name: 'Messages', tokens: 245000, color: 'claude', isDeferred: false, kind: 'used' },
        { name: 'MCP tools (deferred)', tokens: 50000, color: 'subtle', isDeferred: true, kind: 'deferred' },
        { name: 'Free space', tokens: 677000, color: 'text', isDeferred: false, kind: 'free' },
        { name: 'Autocompact buffer', tokens: 33000, color: 'warning', isDeferred: false, kind: 'buffer' },
      ],
      totalTokens: 290000,
      maxTokens: 1000000,
      rawMaxTokens: 1000000,
      percentage: 29,
    },
  },
  rateLimits: [{ kind: 'seven_day', percentUsed: 2, resetsAt: '2026-11-20T00:00:00Z' }],
}

export const PANE_TARGET = {
  plugin: 'csr-pane',
  component: 'Pane',
  requestId: 'self-reflect',
  surface: 'terminal',
  props: {
    title: 'Self-Reflect',
    isFocused: false,
    bodyColumns: 40,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 40 },
    view: {},
  },
} as const

export const BAND_TARGET = {
  plugin: 'csr-pane',
  component: 'AbovePrompt',
  surface: 'terminal',
  props: {
    hasSurvey: false,
    isCollapsed: false,
    isWorking: false,
    maxRows: 6,
    bodyColumns: 60,
    scroll: { offset: 0, bodyRows: 6 },
    view: {},
  },
} as const


// An answer: its stdout (exit 0), null for a child still running at its timeout, or the whole exit.
export type Exit = { exitCode: number; stderr: string; stdout: string }
export type Run = (argv: readonly string[]) => string | Exit | null | Promise<string | Exit | null>
export const answer: Run = argv => (argv.includes('--json') ? SUMMARY : FRAME_40)

export type Extra = { hidden?: boolean; sets?: unknown[][]; opens?: number[]; fills?: string[] }

export function engine(
  on: Parameters<Parameters<typeof test>[1]>[1],
  isPlaced: boolean,
  run: Run = answer,
  extra: Extra = {},
) {
  on('command.register', () => ({ value: { command: 'csr-pane' } }))
  on('store.get', () => ({ value: extra.hidden ? true : undefined }))
  on('store.set', (_, e) => {
    extra.sets?.push([(e as { key: string }).key, (e as { value: unknown }).value])

    return { value: undefined }
  })
  on('fs.stat', () => ({ value: { kind: 'file', size: 1, mtimeMs: 0, isLink: false } }))
  on('session.id', () => ({ value: 'sid-1' }))
  on('session.cwd', () => ({ value: '/repo' }))
  on('session.usage', () => ({ value: USAGE }))
  on('ui.open', () => {
    extra.opens?.push(1)

    return { value: isPlaced ? { isPlaced: true } : { isPlaced: false, reason: 'unasked below 144' } }
  })
  on('ui.panes', () => ({
    value: [{ id: 'self-reflect', title: 'Self-Reflect', isShown: true, isFocused: false, isPlaced }],
  }))
  on('ui.log', () => ({ value: undefined }))
  on('prompt.fill', (_, e) => {
    extra.fills?.push((e as { text: string }).text)

    return { isFilled: true } as never
  })
  // Timers are refused: each test drives refreshes through /csr-pane.
  on('clock.every', () => ({ deny: 'test' }))
  on('clock.after', () => ({ deny: 'test' }))
  on('clock.now', () => ({ value: Date.parse('2026-11-15T12:00:00Z') }))
  on('process.run', async (_, e) => {
    const out = await run(e.argv)

    // A null answer stands for a child still running at its timeout.
    if (out === null) return { deny: 'timed out after 5000 ms' }
    return { value: typeof out === 'string' ? { exitCode: 0, stderr: '', stdout: out } : out }
  })
  on('session.start', (_, e) => ({ cwd: e.cwd }))
}

