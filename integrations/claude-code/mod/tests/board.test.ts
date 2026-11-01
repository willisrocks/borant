// The board-to-rows mapping, on the fixture and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import {
  bandActions,
  activeProjects,
  allProjects,
  allQuestions,
  bandItems,
  bandRows,
  condensedRow,
  digest,
  epicState,
  helpLines,
  hhmm,
  isActive,
  itemActions,
  openTasksLabel,
  progressBar,
  projectCount,
  projectSummary,
  rowText,
} from '../board.js'

type Cell = { text: string; button?: { key: string; kind: string; hotkey?: string } }
type Row = { key: string; cells: Cell[]; selected?: boolean }

/** The band's rows as plain text. */
function texts(rows: Row[]) {
  return rows.map((r) => rowText(r.cells))
}

function hotkeys(row: Row) {
  return row.cells.flatMap((c) => (c.button?.hotkey ? [c.button.hotkey] : []))
}

/** One project of the fixture, by name: the board lists projects in name order. */
function projectOf(name: string) {
  const found = allProjects(board).find((p) => p.project === name)
  if (!found) throw new Error('no project ' + name)
  return found
}

test('the board counts what the mockup shows', () => {
  expect(allQuestions(board)).toHaveLength(2)
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

test('the band has a title row, one row per needs-you item, and one per active project', () => {
  const rows = bandRows(board, { width: 120 }) as Row[]
  const lines = texts(rows)
  expect(lines).toHaveLength(5)
  expect(lines[0]).toContain('chief of staff')
  expect(lines[0]).toContain('2 need you · 2 active · 1 idle')
  expect(lines[1]).toMatch(/^1: ▲ Create Hetzner bucket borant-hub-backup \+ 1Password item +borant · /)
  expect(lines[2]).toMatch(/^2: ▲ Revoke old willisrocks gh token\? +borant/)
  expect(lines[3]).toMatch(/^3: borant +▲ Hub backup to object storage +██░░░ 2\/5  ■2/)
  expect(lines[4]).toMatch(/^4: notes +● 3 open tasks +■1/)
  // The idle boopmark has no row, and no project wears a colored badge.
  expect(lines.join('\n')).not.toContain('boopmark')
  // Each row's chip is a plain Button with its digit, keyed by the item.
  expect(rows.slice(1).map((r) => hotkeys(r)[0])).toEqual(['1', '2', '3', '4'])
  expect(rows.slice(1).map((r) => r.key)).toEqual(bandItems(board).map((i: { key: string }) => i.key))
  // The rows run the full width; the title row ends where they end.
  for (const line of lines.slice(1)) expect(Array.from(line).length).toBe(120)
  expect(Array.from(lines[0]).length).toBeLessThanOrEqual(120)
})

test('rows past nine keep their columns with a blank chip and no hotkey', () => {
  const many = structuredClone(board)
  const projects: (typeof many.collections)[0]['projects'] = []
  for (let i = 0; i < 10; i += 1) {
    const p = structuredClone(projectOf('borant'))
    p.project = 'p' + String(i).padStart(2, '0')
    projects.push(p)
  }
  many.collections[0].projects = projects
  const rows = bandRows(many, { width: 120 }) as Row[]
  // Two needs rows, then ten projects: 1 to 9 have a digit, the last three none.
  expect(hotkeys(rows[9])).toEqual(['9'])
  expect(hotkeys(rows[10])).toEqual([])
  expect(rowText(rows[10].cells)).toMatch(/^ {3}p07 /)
  expect(rowText(rows[10].cells).indexOf('▲')).toBe(rowText(rows[9].cells).indexOf('▲'))
  expect(rowText(rows[0].cells)).toContain('1-9 select')
})

test('the title row names the keys while the board has them, and fits every width', () => {
  const rest = bandRows(board, { width: 140 }) as Row[]
  expect(rowText(rest[0].cells)).toContain('1-4 select')
  expect(hotkeys(rest[0])).toEqual([])
  const selected = bandItems(board)[0].key
  const buttons = (item: { key: string }) => bandActions({ done: true, ask: true, buttons: item.key === selected, arg: item.key })
  const active = bandRows(board, { width: 160, active: true, selected, actionsFor: buttons, fresh: [{ text: 'live', style: 'green' }] }) as Row[]
  const title = rowText(active[0].cells)
  expect(title).toContain('j: ↓ k: ↑ move · d done · a ask · ⏎ more · esc prompt')
  // A key is named only when the selected row draws its button: not while a
  // Done is under way, and only a after an Ask failed.
  const closing = bandRows(board, { width: 160, active: true, selected, actionsFor: () => [{ text: '◷ closing 4s', style: 'cyan' }] }) as Row[]
  expect(rowText(closing[0].cells)).toContain('j: ↓ k: ↑ move · ⏎ more')
  expect(rowText(closing[0].cells)).not.toMatch(/d done|a ask/)
  const askFailed = (item: { key: string }) =>
    bandActions({ status: [{ text: '! ask failed', style: 'red' }], done: true, ask: true, askAgain: true, buttons: item.key === selected, arg: item.key })
  const again = bandRows(board, { width: 160, active: true, selected, reserveActions: true, actionsFor: askFailed }) as Row[]
  expect(rowText(again[1].cells)).toContain('! a: Ask again')
  expect(rowText(again[0].cells)).toContain('a ask')
  expect(rowText(again[0].cells)).not.toContain('d done')
  expect(title).toMatch(/live ─ \[ \? \] ─$/)
  // j and k are the band's own Buttons, so the keys the hint names reach it.
  expect(hotkeys(active[0])).toEqual(['j', 'k'])
  for (const width of [200, 110, 100, 80, 60, 40, 30]) {
    const row = (bandRows(board, { width, active: true, selected, fresh: [{ text: 'live' }] }) as Row[])[0]
    expect(Array.from(rowText(row.cells)).length).toBeLessThanOrEqual(width)
    // j and k are how the keys reach the band: every width keeps them whole.
    expect(hotkeys(row)).toEqual(['j', 'k'])
    if (width >= 60) {
      expect(rowText(row.cells)).toContain(width >= 110 ? 'chief of staff' : 'cos')
      expect(row.cells.some((c) => c.button?.kind === 'help')).toBe(true)
    }
  }
})

test('the selected item alone draws Done and Ask, and its columns do not move', () => {
  const items = bandItems(board)
  const actionsFor = (selected: string) => (item: { key: string }) => bandActions({ done: true, ask: true, buttons: item.key === selected, arg: item.key })
  const first = bandRows(board, { width: 120, selected: items[0].key, fill: true, reserveActions: true, actionsFor: actionsFor(items[0].key) }) as Row[]
  const second = bandRows(board, { width: 120, selected: items[1].key, fill: true, reserveActions: true, actionsFor: actionsFor(items[1].key) }) as Row[]
  expect(rowText(first[1].cells)).toContain('d: Done  a: Ask')
  expect(rowText(first[2].cells)).not.toContain('d: Done')
  expect(first.map((r) => hotkeys(r)).flat().filter((k) => k === 'd')).toHaveLength(1)
  expect(first[1].selected).toBe(true)
  expect(second[2].selected).toBe(true)
  // The project after the title starts at the same column either way.
  expect(rowText(first[2].cells).indexOf('borant')).toBe(rowText(second[2].cells).indexOf('borant'))
})

test('a project name is never cut while the epic can give way', () => {
  const long = structuredClone(board)
  long.collections[0].projects[1].project = 'roger-emr-observability-dashboards'
  for (const width of [120, 100, 80]) {
    const line = texts(bandRows(long, { width }) as Row[])[3]
    expect(line).toContain('roger-emr-observability-dashboards')
    expect(Array.from(line).length).toBe(width)
  }
  // At 60 the epic is down to its floor, so the name gives way, with an ellipsis.
  const tight = texts(bandRows(long, { width: 60 }) as Row[])[3]
  expect(tight).toContain('…')
  expect(Array.from(tight).length).toBe(60)
})

test('an empty board is one title row, and a collapsed band one row', () => {
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  calm.collections[0].projects = [projectOf('boopmark')] as (typeof calm.collections)[0]['projects']
  const rows = texts(bandRows(calm, { width: 120 }) as Row[])
  expect(rows).toHaveLength(1)
  expect(rows[0]).toContain('✓ nothing needs you · 0 active · 1 idle')
  const one = bandRows(board, { width: 120, maxRows: 1 }) as Row[]
  expect(one).toHaveLength(1)
  expect(rowText(one[0].cells)).toContain('▲ 2 need you')
  expect(rowText(condensedRow(board, { width: 120 }))).toContain('borant 2/5 ■2')
})

test('the help names the keys and the commands', () => {
  const text = helpLines({ rows: 4 }).join('\n')
  for (const word of ['1-4', 'j k', 'enter', 'esc', 'ctrl+x tab', '/cos pane', '/cos help', 'Done', 'Ask']) {
    expect(text).toContain(word)
  }
})

test('a band taller than its room keeps the title and the selected row in view', () => {
  const items = bandItems(board)
  const rows = bandRows(board, { width: 100, maxRows: 3, selected: items[3].key, fill: true }) as Row[]
  expect(rows).toHaveLength(3)
  expect(rowText(rows[0].cells)).toContain('need you')
  expect(rows[1].key).toBe(items[3].key)
  expect(rowText(rows[2].cells)).toContain('↑ 3 more')
  expect(bandRows(board, { width: 100, maxRows: 10 })).toHaveLength(5)
  // At rest the window starts at row 1, so the digits drawn are the ones armed.
  const rest = bandRows(board, { width: 100, maxRows: 3, selected: items[3].key }) as Row[]
  expect(rest[1].key).toBe(items[0].key)
})

test('a press status never moves a column: every needs row keeps one slot', () => {
  const items = bandItems(board)
  const status = [{ text: '✓ asked', style: 'green' }]
  const plain = bandRows(board, { width: 120, reserveActions: true }) as Row[]
  const pressed = bandRows(board, {
    width: 120,
    reserveActions: true,
    actionsFor: (item: { key: string }) => (item.key === items[1].key ? bandActions({ status, done: true, buttons: true, arg: item.key }) : []),
  }) as Row[]
  // Row 1's title and project stay where they were when row 2 shows a status.
  expect(rowText(pressed[1].cells)).toBe(rowText(plain[1].cells))
  const at = rowText(pressed[2].cells).indexOf('✓ asked')
  const buttons = bandRows(board, {
    width: 120,
    reserveActions: true,
    actionsFor: (item: { key: string }) => (item.key === items[0].key ? bandActions({ done: true, ask: true, buttons: true, arg: item.key }) : []),
  }) as Row[]
  expect(at).toBe(rowText(buttons[1].cells).indexOf('d: Done'))
  // The slot holds the short status and the Done button together.
  expect(rowText(pressed[2].cells)).toContain('✓ asked  d: Done')
  // A Done that failed offers Done again, in the same slot.
  const cells = (p: object) => bandActions(p).map((c: { text: string }) => c.text).join('')
  expect(cells({ status: [{ text: '! still open', style: 'red' }], done: true, doneAgain: true, ask: true, buttons: true })).toBe('! d: Done again')
  // An Ask that failed can be sent again, in the same slot.
  expect(cells({ status: [{ text: '! ask failed', style: 'red' }], done: true, ask: true, askAgain: true, buttons: true })).toBe('! a: Ask again')
  // Each button carries the key of its item, which the press acts on.
  expect(bandActions({ done: true, ask: true, buttons: true, arg: 'q:c/t' }).filter((c: { button?: { arg?: string } }) => c.button).map((c: { button: { arg: string } }) => c.button.arg)).toEqual(['q:c/t', 'q:c/t'])
  // The preview's line: the full status, then Done again and Ask again.
  expect(itemActions({ status: [{ text: '! ask failed', style: 'red' }], done: true, ask: true, askAgain: true, buttons: true }, 'Ask orchestrator').filter((c: { button?: unknown }) => c.button).map((c: { button: { label: string } }) => c.button.label)).toEqual(['Done', 'Ask again'])
})

test('from 150 columns a project row adds its counts and its orchestrator', () => {
  const line = texts(bandRows(board, { width: 160 }) as Row[])[3]
  expect(line).toMatch(/■2/)
  expect(line).toContain('orchestrator · macbook')
  expect(texts(bandRows(board, { width: 120 }) as Row[])[3]).not.toContain('orchestrator · macbook')
})
