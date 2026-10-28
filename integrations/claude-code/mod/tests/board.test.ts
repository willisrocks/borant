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
  helpLines,
  hhmm,
  paneHelpRows,
  progressBar,
  projectSummary,
  rowText,
} from '../board.js'

const updated = '14:32'

/** One project of the fixture, by name: the board lists projects in name order. */
function projectOf(name: string) {
  const found = allProjects(board).find((p) => p.project === name)
  if (!found) throw new Error('no project ' + name)
  return found
}

test('the board counts what the mockup shows', () => {
  expect(countNeedsYou(board)).toBe(2)
  expect(allProjects(board).map((p) => p.project)).toEqual(['boopmark', 'borant'])
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

  // The band header and the digest read the fixture's numbers.
  const head = rowText(bandRows(board, { width: 120 })[0])
  expect(head).toMatch(/updated \d{2}:\d{2}/)
  expect(head).not.toContain('--:--')
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

test('a project with no epics shows a dash and no grey bar', () => {
  const fresh = structuredClone(projectOf('boopmark'))
  fresh.sagas = []
  fresh.epics = []
  expect(projectSummary(fresh).bar.text).toBe('\u2014')
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  calm.collections[0].projects = [fresh] as (typeof calm.collections)[0]['projects']
  const row = rowText(bandRows(calm, { width: 120, updated })[1])
  expect(row).toContain('\u2014')
  expect(row).not.toContain('\u2591')
})

test('rows past nine carry no second [ 9 ]', () => {
  const many = structuredClone(board)
  const projects: (typeof many.collections)[0]['projects'] = []
  for (let i = 0; i < 10; i += 1) {
    const p = structuredClone(projectOf('borant'))
    p.project = 'p' + String(i).padStart(2, '0')
    projects.push(p)
  }
  many.collections[0].projects = projects
  const rows = bandRows(many, { width: 120, updated })
  // One needs row, then ten project rows: 1 to 9 are numbered, the tenth is not.
  expect(rowText(rows[9])).toContain('[ 9 ]')
  expect(rows.filter((r: unknown[]) => rowText(r).includes('[ 9 ]'))).toHaveLength(1)
  expect(rowText(rows[10])).not.toContain('[')
  expect(rowText(rows[11])).not.toContain('[')
  // Each row is still its own button.
  const keys = rows.flat().flatMap((s: { button?: { kind: string; key: string } }) =>
    s.button?.kind === 'project' ? [s.button.key] : [],
  )
  expect(new Set(keys).size).toBe(10)
  // The hint names the digits that work.
  expect(rowText(bandRows(many, { width: 140, updated })[0])).toContain('1-9 open')
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
  expect(rows[2]).toContain('no saga')
  expect(rows[2]).toContain('1/1')
  expect(rows[3]).toContain('[ 3 ]')
  expect(rows[3]).toContain('Elastic compute')
  expect(rows[3]).toContain('████░░░░░░ 2/5')
  expect(rows[3]).toContain('2 blocked')
  expect(rows[3]).toContain('0 needs you')
})

test('the band collapsed to one row is one condensed row', () => {
  const rows = bandRows(board, { width: 120, maxRows: 1, updated })
  expect(rows).toHaveLength(1)
  expect(rowText(rows[0]).trimEnd()).toBe(
    'cos  [ 1 ] ▲ 2 need you  │   boopmark  1/1 ✓ idle  │   borant  2/5 ■ 2 blocked  │  14:32',
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
  expect(rows[2]).toContain('1/1')
  expect(rows[3]).toContain('███░░░░░ 2/5')
  expect(rows[3]).toContain('■ 2')
  // The ▲ is the state glyph of the current epic; the needs count is 0.
  expect(rows[3]).not.toContain('▲ 0')
  expect(rows[3]).not.toContain('running')
  expect(rows[1]).toContain('Needs you (2)')
})

test('the band title hints the keys, and drops the hint on a narrow terminal', () => {
  const wide = bandRows(board, { width: 140, updated })
  expect(rowText(wide[0])).toContain('chief of staff')
  expect(rowText(wide[0])).toContain('ctrl+x tab focus')
  expect(rowText(wide[0])).toContain('1-3 open')
  expect(rowText(wide[0])).toContain('/cos help')
  expect(wide[0].some((s: { button?: { kind: string } }) => s.button?.kind === 'help')).toBe(true)
  expect(Array.from(rowText(wide[0])).length).toBe(140)

  // A narrow band keeps the name and the help button, and drops the hint.
  const narrow = bandRows(board, { width: 100, updated })
  expect(rowText(narrow[0])).toContain('chief of staff')
  expect(rowText(narrow[0])).not.toContain('ctrl+x tab')
  expect(narrow[0].some((s: { button?: { kind: string } }) => s.button?.kind === 'help')).toBe(true)

  // The digits in the hint follow the rows the band numbers.
  const one = structuredClone(board)
  one.collections[0].needs_you = []
  one.collections[0].projects = [projectOf('borant')] as (typeof one.collections)[0]['projects']
  expect(rowText(bandRows(one, { width: 140, updated })[0])).toContain('1 open')
  one.collections[0].projects = []
  expect(rowText(bandRows(one, { width: 140, updated })[0])).toContain('ctrl+x tab focus')
  expect(rowText(bandRows(one, { width: 140, updated })[0])).not.toContain(' open')
})

test('the help view names every key, every command, and both buttons', () => {
  const text = helpLines({ rows: 3 }).join('\n')
  expect(text).toContain('1-3')
  expect(text).toContain('ctrl+x tab')
  expect(text).toContain('ctrl+x ctrl+a')
  expect(text).toContain('esc')
  expect(text).toContain('/cos')
  expect(text).toContain('/cos <project>')
  expect(text).toContain('/cos pane')
  expect(text).toContain('/cos help')
  expect(text).toContain('[ ? ]')
  expect(text).toContain('Done')
  expect(text).toContain('Ask orchestrator')
  expect(text).toContain('Chris marked done:')
  expect(text).toContain('neither button calls the hub')
  expect(helpLines({ rows: 1 }).join('\n')).toContain('1 opens a row')

  // The pane draws the same lines, each fitted to its width.
  const rows = paneHelpRows({ rows: 3, width: 60 })
  expect(rows).toHaveLength(helpLines({ rows: 3 }).length)
  for (const row of rows) expect(Array.from(rowText(row)).length).toBe(60)
  expect(rowText(rows[1])).toContain('1-3')
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

test('the project and saga columns take their text before the terminal runs out', () => {
  const long = structuredClone(board)
  const project = long.collections[0].projects[1]
  project.project = 'roger-docs'
  project.sagas[0].title = 'ROG-4199: LiteLLM proxy rollout'
  const wide = bandRows(long, { width: 200, updated }).map(rowText)
  expect(wide[3]).toContain('roger-docs')
  expect(wide[3]).toContain('ROG-4199: LiteLLM proxy rollout')
  expect(wide[3]).toContain('rollout \u25b2')

  // At 120 the full name and saga still fit; the epic gives way first.
  const narrow = bandRows(long, { width: 120, updated }).map(rowText)
  expect(narrow[3]).toContain('roger-docs')
  expect(narrow[3]).toContain('ROG-4199: LiteLLM proxy rollout')
  expect(Array.from(narrow[3]).length).toBe(120)

  // At the wide layout's floor the saga is cut with an ellipsis, not the name.
  const tight = bandRows(long, { width: 110, updated }).map(rowText)
  expect(tight[3]).toContain('roger-docs')
  expect(tight[3]).toContain('\u2026')
  for (const row of tight) expect(Array.from(row).length).toBeLessThanOrEqual(110)
})

test('a needs-you title truncates with an ellipsis before its right block', () => {
  // 120 terminal columns leave the band 115 cells, as the engine's `[-]` mark
  // takes five; the title used to run into the project badge there.
  const long = structuredClone(board)
  long.collections[0].projects[1].project = 'roger-docs'
  const row = rowText(bandRows(long, { width: 115, updated })[1])
  expect(row).toContain('\u2026')
  expect(row).toMatch(/\u2026\s+borant/)
  expect(row).not.toContain('item borant')
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
