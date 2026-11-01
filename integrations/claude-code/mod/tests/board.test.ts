// The board-to-rows mapping, on the fixture and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import {
  activeProjects,
  allProjects,
  allQuestions,
  condensedRow,
  countNeedsYou,
  digest,
  epicState,
  hhmm,
  isActive,
  openTasksLabel,
  progressBar,
  projectCount,
  projectSummary,
  rowText,
} from '../board.js'
import { bandRows } from '../band.js'

/** One project of the fixture, by name: the board lists projects in name order. */
function projectOf(name: string) {
  const found = allProjects(board).find((p) => p.project === name)
  if (!found) throw new Error('no project ' + name)
  return found
}

test('the board counts what the mockup shows', () => {
  expect(countNeedsYou(board)).toBe(2)
  expect(allProjects(board).map((p) => p.project)).toEqual(['boopmark', 'borant', 'notes'])
  // boopmark's one epic is done and it has no open task: it is idle.
  expect(activeProjects(board).map((p) => p.project)).toEqual(['borant', 'notes'])
  expect(isActive(projectOf('boopmark'))).toBe(false)
  expect(projectCount(board)).toBe('2 active \u00b7 1 idle')
  const first = projectSummary(projectOf('borant'))
  expect(first.saga).toBe('Elastic compute')
  expect(first.bar.text).toBe('████░░░░░░ 2/5')
  expect(first.running).toBe(0)
  expect(first.blocked).toBe(2)
  expect(first.needs).toBe(0)
  const second = projectSummary(projectOf('boopmark'))
  expect(second.saga).toBe('no saga')
  expect(second.bar.text).toBe('██████████ 1/1')
  expect(second.needs).toBe(0)
  const third = projectSummary(projectOf('notes'))
  expect(third.saga).toBe('plain tasks')
  expect(third.plain).toBe(true)
  expect(openTasksLabel(projectOf('notes'))).toBe('3 open tasks')
})

test('a project is active while a saga, an epic, or a plain task is open', () => {
  const p = structuredClone(projectOf('boopmark'))
  expect(isActive(p)).toBe(false)
  p.tasks.needs_human = 1
  expect(isActive(p)).toBe(true)
  p.tasks.needs_human = 0
  p.epics[0].status = 'in_progress'
  expect(isActive(p)).toBe(true)
  const all = structuredClone(board)
  all.collections[0].projects = [projectOf('borant')] as (typeof all.collections)[0]['projects']
  expect(projectCount(all)).toBe('1 project')
})

test('an epic is done, needs you, blocked, running, or waiting', () => {
  // Open epics first, then the closed ones, so the fixture shows them in this
  // order; the mockup drew the closed ones first.
  const epics = projectOf('borant').sagas[0].epics
  expect(epics.map((e: { title: string }) => e.title)).toEqual([
    'Hub backup to object storage',
    'Delete borant-2',
    'Hub move to borant-hub',
    'Golden image',
    'Hub join for pool boxes',
  ])
  expect(epics.map(epicState)).toEqual(['needs', 'blocked', 'waiting', 'done', 'done'])
  expect(epicState({ status: 'in_progress', counts: {} })).toBe('running')
  expect(epicState({ status: 'stuck', counts: {} })).toBe('needs')
  expect(epicState({ status: 'open', counts: {} })).toBe('waiting')
})

test('an integer millisecond at and since draw HH:MM, not the fallback', () => {
  // The hub writes `at` and `since` as Timestamps in integer milliseconds.
  expect(hhmm(1793025120000)).toMatch(/^\d{2}:\d{2}$/)
  // An older fixture still holds an ISO string, and still draws.
  expect(hhmm('2026-10-26T14:32:00Z')).toMatch(/^\d{2}:\d{2}$/)
  expect(hhmm(undefined)).toBe('--:--')
  expect(hhmm(null)).toBe('--:--')
  expect(hhmm('not a time')).toBe('--:--')

  // The digest reads the fixture's numbers.
  const text = digest(board, { width: 96 })
  expect(text).toMatch(/since \d{2}:\d{2}/)
  expect(text).not.toContain('since ?')
})

test('the progress bar fills in ten cells, and in eight on a narrow terminal', () => {
  expect(progressBar(2, 5, 10).text).toBe('████░░░░░░ 2/5')
  expect(progressBar(2, 5, 8).text).toBe('███░░░░░ 2/5')
  expect(progressBar(1, 1, 8).text).toBe('████████ 1/1')
  // A project with no epics has no bar at all: a dash, never a faint block.
  expect(progressBar(0, 0, 10).text).toBe('\u2014')
  expect(progressBar(0, 0, 10).filled).toBe('')
  expect(progressBar(0, 0, 10).empty).not.toContain('\u2591')
})

test('a project of plain tasks shows its open tasks, not filler or a grey bar', () => {
  expect(projectSummary(projectOf('notes')).bar.text).toBe('\u2014')
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  calm.collections[0].projects = [projectOf('notes')] as (typeof calm.collections)[0]['projects']
  for (const width of [120, 80]) {
    const row = rowText(bandRows(calm, { width }).map((r: { segments: unknown[] }) => r.segments)[1])
    if (width > 100) expect(row).toContain('plain tasks')
    expect(row).toContain('\u25cf 3 open tasks')
    expect(row).not.toContain('no saga')
    expect(row).not.toContain('no epic yet')
    expect(row).not.toContain('\u2591')
    expect(row).not.toContain('\u2014')
  }
})

test('a gate comes first, then the oldest', () => {
  expect(allQuestions(board).map((q) => q.title)).toEqual([
    'Create Hetzner bucket borant-hub-backup + 1Password item',
    'Revoke old willisrocks gh token?',
  ])
  // The gate wins even when a plain question is older. The fixture's own
  // questions are not gates: an approved saga has none.
  const later = structuredClone(board)
  later.collections[0].needs_you[0].gate = true
  later.collections[0].needs_you[0].since = 1793027400000
  expect(allQuestions(later)[0].title).toBe('Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(allQuestions(board)[0].gate).toBe(false)
})

test('the condensed row says all clear when nothing waits on the person', () => {
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  expect(rowText(condensedRow(calm, { width: 120 }))).toContain('✓ all clear')
})

test('the digest lists needs you, then each project with its epics', () => {
  const lines = digest(board, { width: 96 }).split('\n')
  expect(lines[0]).toContain('cos · 2 need you · 2 active · 1 idle')
  const needs = lines.findIndex((l) => l.includes('Needs you (2)'))
  const borant = lines.findIndex((l) => l.includes('borant') && l.includes('Elastic compute'))
  expect(needs).toBeGreaterThan(0)
  expect(borant).toBeGreaterThan(needs)
  expect(lines.join('\n')).toContain('1  Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(lines.join('\n')).toContain('2  Revoke old willisrocks gh token?')
  expect(lines.join('\n')).toContain('✓ Golden image')
  expect(lines.join('\n')).toContain('▲ Hub backup to object storage  waits on you')
  expect(lines.join('\n')).not.toContain('boopmark')
  expect(lines.join('\n')).toMatch(/notes +plain tasks +3 open tasks/)
})

test('the digest of one project holds only that project', () => {
  const text = digest(board, { project: 'borant', width: 96 })
  expect(text).toContain('Elastic compute')
  expect(text).not.toContain('boopmark')
  expect(digest(board, { project: 'nope', width: 96 })).toContain('no project nope')
  // A project named on its own shows even when it is idle.
  expect(digest(board, { project: 'boopmark', width: 96 })).toContain('Move production to Fly.io')
})

test('the digest sizes its columns like the band and keeps the saga title', () => {
  const long = structuredClone(board)
  const project = long.collections[0].projects[1]
  project.project = 'roger-docs'
  project.sagas[0].title = 'ROG-4199: LiteLLM proxy rollout'
  for (const width of [200, 120, 96]) {
    const text = digest(long, { width })
    expect(text).toContain('ROG-4199: LiteLLM proxy rollout')
    expect(text).not.toContain('LiteLL\u2026')
  }
})

test('a digest line never wraps, even when the transcript is narrow', () => {
  const long = structuredClone(board)
  const project = long.collections[0].projects[1]
  project.project = 'roger-docs'
  project.sagas[0].title = 'ROG-4199: LiteLLM proxy rollout'
  for (const width of [108, 64, 50]) {
    for (const line of digest(long, { width }).split('\n')) {
      expect(Array.from(line).length).toBeLessThanOrEqual(width)
    }
  }
})

test('the band collapsed to one row is one condensed row', () => {
  expect(rowText(condensedRow(board, { width: 120 })).trimEnd()).toBe(
    'cos  ▲ 2 need you  │  borant 2/5 ■2  │  notes 3 open tasks ■1',
  )
})
