import { expect, test } from 'claude-code/testing'
import { KEEP_IDS_MAX, KEEP_MAX, keepList } from '../hooks/compact'
import type { Keep, Running } from '../hooks/compact'

const tasks = (count: number): Running[] => Array.from({ length: count }, (_, i) => ({
  id: `agent${String(i).padStart(12, '0')}`,
  type: 'subagent',
  description: 'Investigate the running task and its next step. '.repeat(20),
}))
const keep = (running: Running[]): Keep => ({ running, decisions: [], work: [], rules: [] })

test('r5: running ids share one type word and beat descriptions', () => {
  const running = tasks(18)
  const text = keepList(keep(running))
  expect(text.length).toBeLessThanOrEqual(KEEP_MAX)
  expect(text.match(/subagent/g)?.length).toBe(1)
  for (const task of running) expect(text).toContain(task.id)
})

test('r5: whole ids can exceed the prose cap without carrying descriptions', () => {
  const running = tasks(30)
  const text = keepList(keep(running))
  expect(text.length).toBeGreaterThan(KEEP_MAX)
  expect(text.length).toBeLessThanOrEqual(KEEP_IDS_MAX)
  expect(text).not.toContain('Investigate')
  expect(text.match(/subagent/g)?.length).toBe(1)
  for (const task of running) expect(text).toContain(task.id)
})

test('r5: excess running ids are whole and the remaining count is explicit', () => {
  const running = tasks(100)
  const text = keepList(keep(running))
  const included = running.filter(task => text.includes(task.id))
  expect(text.length).toBeLessThanOrEqual(KEEP_IDS_MAX)
  expect(included.length).toBeGreaterThan(0)
  expect(included.length).toBeLessThan(running.length)
  expect(text).toContain(`+${running.length - included.length} more running (Claude Code lists them after compaction)`)
  for (const id of text.match(/agent\d+/g) ?? []) expect(running.some(task => task.id === id)).toBe(true)
})
