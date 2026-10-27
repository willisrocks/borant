// The board-to-rows mapping, on the fixture and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import {
  allProjects,
  allQuestions,
  bandRows,
  condensedRow,
  countNeedsYou,
  digest,
  epicState,
  progressBar,
  projectSummary,
  rowText,
} from '../board.js'

const updated = '14:32'

test('the board counts what the mockup shows', () => {
  expect(countNeedsYou(board)).toBe(2)
  const [borant, boopmark] = allProjects(board)
  const first = projectSummary(borant)
  expect(first.saga).toBe('Elastic compute')
  expect(first.bar.text).toBe('████░░░░░░ 2/5')
  expect(first.running).toBe(0)
  expect(first.blocked).toBe(2)
  expect(first.needs).toBe(1)
  const second = projectSummary(boopmark)
  expect(second.saga).toBe('no saga')
  expect(second.bar.text).toBe('██████████ 1/1')
  expect(second.needs).toBe(0)
})

test('an epic is done, needs you, blocked, running, or waiting', () => {
  const [borant] = allProjects(board)
  expect(borant.sagas[0].epics.map(epicState)).toEqual(['done', 'done', 'needs', 'blocked', 'waiting'])
  expect(epicState({ status: 'in_progress', counts: {} })).toBe('running')
  expect(epicState({ status: 'stuck', counts: {} })).toBe('needs')
  expect(epicState({ status: 'open', counts: {} })).toBe('waiting')
})

test('the progress bar fills in ten cells, and in eight on a narrow terminal', () => {
  expect(progressBar(2, 5, 10).text).toBe('████░░░░░░ 2/5')
  expect(progressBar(2, 5, 8).text).toBe('███░░░░░ 2/5')
  expect(progressBar(1, 1, 8).text).toBe('████████ 1/1')
  expect(progressBar(0, 0, 10).text).toBe('░░░░░░░░░░ 0/0')
})

test('a gate comes first, then the oldest', () => {
  expect(allQuestions(board).map((q) => q.title)).toEqual([
    'Create Hetzner bucket borant-hub-backup + 1Password item',
    'Revoke old willisrocks gh token?',
  ])
  // The gate wins even when a plain question is older.
  const later = structuredClone(board)
  later.collections[0].needs_you[0].since = '2026-10-26T15:10:00Z'
  expect(allQuestions(later)[0].title).toBe('Create Hetzner bucket borant-hub-backup + 1Password item')
})

test('the expanded band has the title rule, the needs-you row, and one row per project', () => {
  const rows = bandRows(board, { width: 120, updated }).map(rowText)
  expect(rows).toHaveLength(4)
  expect(rows[0]).toContain('chief of staff')
  expect(rows[0]).toContain('2 projects · updated 14:32')
  expect(rows[1]).toContain('[ 1 ]')
  expect(rows[1]).toContain('▲ Needs you (2)')
  expect(rows[1]).toContain('Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(rows[1]).toContain('borant')
  expect(rows[1]).toContain('+1 more')
  expect(rows[2]).toContain('[ 2 ]')
  expect(rows[2]).toContain('Elastic compute')
  expect(rows[2]).toContain('████░░░░░░ 2/5')
  expect(rows[2]).toContain('2 blocked')
  expect(rows[2]).toContain('1 needs you')
  expect(rows[3]).toContain('no saga')
  expect(rows[3]).toContain('1/1')
})

test('the band collapsed to one row is one condensed row', () => {
  const rows = bandRows(board, { width: 120, maxRows: 1, updated })
  expect(rows).toHaveLength(1)
  expect(rowText(rows[0]).trimEnd()).toBe(
    'cos  [ 1 ] ▲ 2 need you  │   borant  2/5 ■ 2 blocked  │   boopmark  1/1 ✓ idle  │  14:32',
  )
  expect(rowText(condensedRow(board, { width: 120, updated })).trimEnd()).toBe(rowText(rows[0]).trimEnd())
})

test('the condensed row says all clear when nothing waits on the person', () => {
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  expect(rowText(condensedRow(calm, { width: 120, updated }))).toContain('✓ all clear')
})

test('every row fits the terminal at any width', () => {
  for (const width of [120, 100, 80, 60, 40]) {
    for (const row of bandRows(board, { width, updated })) {
      const shown = Array.from(rowText(row))
      expect(shown.length).toBeLessThanOrEqual(Math.max(20, width))
    }
  }
})

test('a narrow band drops the padded columns and the ten-cell bar', () => {
  const rows = bandRows(board, { width: 100, updated }).map(rowText)
  expect(rows[2]).toContain('███░░░░░ 2/5')
  expect(rows[2]).toContain('■ 2')
  expect(rows[2]).toContain('▲ 1')
  expect(rows[2]).not.toContain('running')
  expect(rows[1]).toContain('Needs you (2)')
})

test('the digest lists needs you, then each project with its epics', () => {
  const lines = digest(board, { width: 96 }).split('\n')
  expect(lines[0]).toContain('cos · 2 need you · 2 projects')
  const needs = lines.findIndex((l) => l.includes('Needs you (2)'))
  const borant = lines.findIndex((l) => l.includes('borant') && l.includes('Elastic compute'))
  expect(needs).toBeGreaterThan(0)
  expect(borant).toBeGreaterThan(needs)
  expect(lines.join('\n')).toContain('1  Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(lines.join('\n')).toContain('2  Revoke old willisrocks gh token?')
  expect(lines.join('\n')).toContain('✓ Golden image')
  expect(lines.join('\n')).toContain('▲ Hub backup to object storage  waits on you')
  expect(lines.join('\n')).toContain('boopmark')
  expect(lines.join('\n')).toContain('○ Waiting for a goal')
})

test('the digest of one project holds only that project', () => {
  const text = digest(board, { project: 'borant', width: 96 })
  expect(text).toContain('Elastic compute')
  expect(text).not.toContain('boopmark')
  expect(digest(board, { project: 'nope', width: 96 })).toContain('no project nope')
})
