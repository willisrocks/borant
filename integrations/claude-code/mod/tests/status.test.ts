// What the board says about itself and about a press, with plain values.
import { expect, test } from 'claude-code/testing'
import { ageText, freshness, pressStatus, withClosed } from '../status.js'

test('ages read short', () => {
  expect(ageText(45000)).toBe('45s')
  expect(ageText(42 * 60000)).toBe('42m')
  expect(ageText(64 * 60000)).toBe('1h 4m')
  expect(ageText(120 * 60000)).toBe('2h')
  expect(ageText(50 * 3600000)).toBe('2d')
})

test('the freshness word: live, its age after 30 s, reading, polling, stale, unavailable', () => {
  expect(freshness({ live: true, ageMs: 3000 }).text).toBe('live')
  expect(freshness({ live: true, ageMs: 45000 }).text).toBe('live · 45s')
  expect(freshness({ live: true, reading: true, ageMs: 3000 }).text).toBe('↻ reading')
  expect(freshness({ live: false, ageMs: 4000 }).text).toBe('polling · 4s')
  expect(freshness({ live: true, ageMs: 120000 })).toEqual({ text: 'stale · 2m', style: 'yellow' })
  expect(freshness({ unavailable: true }).style).toBe('red')
})

test('a press says where it is, and a closed item keeps its place for a while', () => {
  expect(pressStatus({ state: 'closing', at: 0 }, null, 14000)?.[0].text).toBe('◷ the chief is closing it · 14s')
  expect(pressStatus({ state: 'failed', waitMs: 120000 }, null, 0)?.[0].text).toBe('! still open after 2m')
  expect(pressStatus(null, { state: 'sent', owner: 'orchestrator', time: '14:32' })?.[0].text).toBe('✓ asked orchestrator at 14:32')
  expect(pressStatus(null, null)).toBeNull()

  const a = { channel: 'c', task: 'a', title: 'A' }
  const b = { channel: 'c', task: 'b', title: 'B' }
  const pending = { 'c/a': { state: 'closed', closedAt: 1000, question: a, index: 0 } }
  expect(withClosed([b], pending, 5000).map((q) => [q.title, Boolean(q.closed)])).toEqual([['A', true], ['B', false]])
  expect(withClosed([b], pending, 12000).map((q) => q.title)).toEqual(['B'])
})
