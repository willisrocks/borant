// The text helpers: cell widths, cuts, ages, and the order of needs-you questions.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import { ageText, allQuestions, cellsOf, truncate } from '../board.js'

test('a wide character takes two cells, and a cut ends in an ellipsis', () => {
  expect(cellsOf('abc')).toBe(3)
  expect(cellsOf('日本')).toBe(4)
  expect(truncate('Hub backup to object storage', 10)).toBe('Hub backu…')
  expect(cellsOf(truncate('日本語のタイトル', 7))).toBeLessThanOrEqual(7)
})

test('ages read as seconds, minutes, hours, and days', () => {
  expect(ageText(45000)).toBe('45s')
  expect(ageText(42 * 60000)).toBe('42m')
  expect(ageText(64 * 60000)).toBe('1h 4m')
  expect(ageText(3 * 86400000)).toBe('3d')
})

test('a gate comes first, then the oldest question', () => {
  const titles = allQuestions(board).map((q) => q.title)
  expect(titles[0]).toBe('Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(titles).toHaveLength(2)
})
