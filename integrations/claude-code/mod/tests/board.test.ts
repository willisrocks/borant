// The board-to-rows mapping, on the fixture and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import {
  activeProjects,
  allProjects,
  allQuestions,
  bandRowKeys,
  bandRows,
  condensedRow,
  countNeedsYou,
  digest,
  epicState,
  helpLines,
  hhmm,
  isActive,
  openTasksLabel,
  paneHelpRows,
  paneTabs,
  progressBar,
  projectCount,
  projectSummary,
  rowText,
  stepKey,
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

test('a project of plain tasks shows its open tasks, not filler or a grey bar', () => {
  expect(projectSummary(projectOf('notes')).bar.text).toBe('\u2014')
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  calm.collections[0].projects = [projectOf('notes')] as (typeof calm.collections)[0]['projects']
  for (const width of [120, 80]) {
    const row = rowText(bandRows(calm, { width, updated })[1])
    expect(row).toContain('plain tasks')
    expect(row).toContain('\u25cf 3 open tasks')
    expect(row).not.toContain('no saga')
    expect(row).not.toContain('no epic yet')
    expect(row).not.toContain('\u2591')
    expect(row).not.toContain('\u2014')
  }
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
  expect(rows[0]).toContain('2 active · 1 idle · updated 14:32')
  expect(rows[1]).toContain('[ 1 ]')
  expect(rows[1]).toContain('▲ Needs you (2)')
  expect(rows[1]).toContain('Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(rows[1]).toContain('borant')
  expect(rows[1]).toContain('+1 more')
  // The idle boopmark has no row.
  expect(rows.join('\n')).not.toContain('boopmark')
  expect(rows[2]).toContain('[ 2 ]')
  expect(rows[2]).toContain('Elastic compute')
  expect(rows[2]).toContain('████░░░░░░ 2/5')
  expect(rows[2]).toContain('2 blocked')
  expect(rows[2]).toContain('0 needs you')
  expect(rows[3]).toContain('[ 3 ]')
  expect(rows[3]).toContain('notes')
  expect(rows[3]).toContain('3 open tasks')
  expect(rows[3]).toContain('1 running')
})

test('the band collapsed to one row is one condensed row', () => {
  const rows = bandRows(board, { width: 120, maxRows: 1, updated })
  expect(rows).toHaveLength(1)
  expect(rowText(rows[0]).trimEnd()).toBe(
    'cos  [ 1 ] ▲ 2 need you  │   borant  2/5 ■ 2 blocked  │   notes  3 open tasks ■ 1 blocked  │  14:32',
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
  // The ▲ is the state glyph of the current epic; the needs count is 0.
  expect(rows[2]).not.toContain('▲ 0')
  expect(rows[2]).not.toContain('running')
  expect(rows[3]).toContain('3 open tasks')
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
  expect(text).toContain("sends a question about <item> to its channel's owner")
  expect(helpLines({ rows: 1 }).join('\n')).toContain('1 opens a row')

  // The pane draws the same lines, each fitted to its width.
  const rows = paneHelpRows({ rows: 3, width: 60 })
  expect(rows).toHaveLength(helpLines({ rows: 3 }).length)
  for (const row of rows) expect(Array.from(rowText(row)).length).toBe(60)
  expect(rowText(rows[1])).toContain('1-3')
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

test('the project and saga columns take their text before the terminal runs out', () => {
  const long = structuredClone(board)
  const project = long.collections[0].projects[1]
  project.project = 'roger-docs'
  project.sagas[0].title = 'ROG-4199: LiteLLM proxy rollout'
  const wide = bandRows(long, { width: 200, updated }).map(rowText)
  expect(wide[2]).toContain('roger-docs')
  expect(wide[2]).toContain('ROG-4199: LiteLLM proxy rollout')
  expect(wide[2]).toContain('rollout \u25b2')

  // At 120 the full name and saga still fit; the epic gives way first.
  const narrow = bandRows(long, { width: 120, updated }).map(rowText)
  expect(narrow[2]).toContain('roger-docs')
  expect(narrow[2]).toContain('ROG-4199: LiteLLM proxy rollout')
  expect(Array.from(narrow[2]).length).toBe(120)

  // At the wide layout's floor the saga is cut with an ellipsis, not the name.
  const tight = bandRows(long, { width: 110, updated }).map(rowText)
  expect(tight[2]).toContain('roger-docs')
  expect(tight[2]).toContain('\u2026')
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

test('a narrow project row drops its stats whole and keeps an ellipsis', () => {
  // At 55 cells the prefix and the stats cannot both fit; the stats go whole
  // and the epic title is what gives way, with an ellipsis.
  const row = rowText(bandRows(board, { width: 55, updated })[2])
  expect(row).toContain('\u2026')
  expect(row).not.toContain('\u2588')
  expect(row).not.toContain('\u2591')
  expect(row).not.toContain('2/5')
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

test('band j and k walk the row chips, wrap, and start from the help button', () => {
  const rows = bandRows(board, { width: 140, updated })
  const keys = bandRowKeys(rows)
  expect(keys).toEqual(['needs-1', 'project-0', 'project-1'])
  // The title row holds j, k, and 0 as the band's own Buttons.
  const title = rows[0].filter((s: { button?: { hotkey?: string } }) => s.button?.hotkey).map((s: { button: { hotkey: string } }) => s.button.hotkey)
  expect(title).toEqual(['j', 'k', '0'])
  expect(stepKey(keys, 'help', 1)).toBe('needs-1')
  expect(stepKey(keys, 'help', -1)).toBe('project-1')
  expect(stepKey(keys, 'needs-1', 1)).toBe('project-0')
  expect(stepKey(keys, 'project-1', 1)).toBe('needs-1')
  expect(stepKey(keys, 'needs-1', -1)).toBe('project-1')
  expect(stepKey([], 'help', 1)).toBeNull()

  // Where the full hint does not fit, the keys stay.
  const tight = bandRows(board, { width: 100, updated })[0]
  expect(tight.filter((s: { button?: { hotkey?: string } }) => s.button?.hotkey).length).toBe(3)
  // Beside a docked pane the band can be 60 cells: the title shortens, the keys stay.
  const docked = bandRows(board, { width: 60, updated })[0]
  expect(rowText(docked)).toContain('cos')
  expect(rowText(docked)).toContain(updated)
  expect(docked.filter((s: { button?: { hotkey?: string } }) => s.button?.hotkey).length).toBe(3)
  expect(Array.from(rowText(docked)).length).toBe(60)
})

test('the pane tabs keep all three at every width and mark the open one', () => {
  const questions = allQuestions(board)
  for (const width of [80, 40, 30, 24]) {
    const cells = paneTabs('projects', questions, width)
    const buttons = cells.filter((c: { button?: unknown }) => c.button) as { text: string; button: { kind: string; active: boolean } }[]
    expect(buttons.map((b) => b.button.kind)).toEqual(['tab-needs', 'tab-projects', 'tab-help'])
    expect(buttons.filter((b) => b.button.active).map((b) => b.button.kind)).toEqual(['tab-projects'])
    expect(Array.from(rowText(cells)).length).toBeLessThanOrEqual(width)
  }
  expect(rowText(paneTabs('help', questions, 80))).toBe('[ Needs you (2) ]  [ Projects ]  [ Help ]')
  expect(rowText(paneTabs('help', questions, 24))).toBe('Needs 2 · Proj · [Help]')
})
