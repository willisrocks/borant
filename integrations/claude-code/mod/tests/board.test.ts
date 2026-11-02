// The board-to-rows mapping, on the fixture and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import {
  activeProjects,
  allBlocked,
  allProjects,
  allQuestions,
  bandGrid,
  bandItems,
  bandRows,
  condensedRow,
  digest,
  epicState,
  helpLines,
  hhmm,
  isActive,
  openTasksLabel,
  progressBar,
  projectCount,
  projectSummary,
  rowText,
  unblockCommand,
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
  expect(lines[3]).toMatch(/^3: ▲ borant +Hub backup to object storage +██░░░ 2\/5 +■2/)
  expect(lines[4]).toMatch(/^4: ● notes +3 open tasks +●1 +■1/)
  // The idle boopmark has no row.
  expect(lines.join('\n')).not.toContain('boopmark')
  // Each row's chip is a plain Button with its digit, keyed by the item.
  expect(rows.slice(1).map((r) => hotkeys(r)[0])).toEqual(['1', '2', '3', '4'])
  expect(rows.slice(1).map((r) => r.key)).toEqual(bandItems(board).map((i: { key: string }) => i.key))
  // The rows run the full width; the title row ends where the grid ends.
  for (const line of lines.slice(1)) expect(Array.from(line).length).toBe(120)
  expect(lines[0].trimEnd()).toHaveLength(bandGrid(120).row)
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
  expect(rowText(rows[10].cells)).toMatch(/^ {3}▲ p07 /)
  expect(rowText(rows[10].cells).indexOf('█')).toBe(rowText(rows[9].cells).indexOf('█'))
})

test('the title row ends with the mode: 0 enters board mode, and board mode says esc exits', () => {
  const rest = bandRows(board, { width: 140 }) as Row[]
  const title = rowText(rest[0].cells).trimEnd()
  expect(title).toMatch(/0: board mode \(vim: i 0\) ─$/)
  // 0 is the band's own Button, pressed by a bare 0 at the empty prompt.
  expect(hotkeys(rest[0])).toEqual(['0'])
  const selected = bandItems(board)[0].key
  const on = bandRows(board, { width: 140, active: true, selected }) as Row[]
  const boardTitle = rowText(on[0].cells).trimEnd()
  expect(boardTitle).toMatch(/board mode · esc exit ─$/)
  expect(hotkeys(on[0])).toEqual([])
  // Both end where the grid ends, so the summary never moves.
  expect(boardTitle).toHaveLength(title.length)
  expect(boardTitle.slice(0, 40)).toBe(title.slice(0, 40))
  // Board mode fills the selected row; at rest no row is filled.
  expect(on[1].selected).toBe(true)
  expect(rest.some((r) => r.selected)).toBe(false)
  // A stale board says so before the mode; a healthy one says nothing.
  const stale = bandRows(board, { width: 140, stale: { text: 'stale · 2m', style: 'yellow' } }) as Row[]
  expect(rowText(stale[0].cells)).toContain('stale · 2m ─ 0: board mode (vim: i 0) ─')
  expect(title).not.toMatch(/live|reading|polling/)
  for (const width of [240, 200, 110, 100, 80, 60, 40, 30]) {
    for (const active of [false, true]) {
      const row = (bandRows(board, { width, active, selected }) as Row[])[0]
      expect(Array.from(rowText(row.cells)).length).toBeLessThanOrEqual(width)
      // The mode is never cut: its key, or how to leave, shows at every width.
      expect(rowText(row.cells)).toContain(active ? 'board mode · esc exit' : '0: board mode')
    }
  }
})

test('a live update never moves a column: the grid depends on the width alone', () => {
  // Each column of the grid holds its own text: the gaps between them stay
  // blank, and the meta starts at its own cell, whatever the data.
  const starts = (line: string, width: number) => {
    const g = bandGrid(width)
    const cells = Array.from(line)
    const meta = 5 + g.main + 2
    const gaps = [5 + g.main, 5 + g.main + 1, ...(g.tail ? [meta + g.meta, meta + g.meta + 1] : [])]
    return { gaps: gaps.map((i) => cells[i]).join(''), meta: cells[meta] !== ' ' && cells[meta - 1] === ' ' }
  }
  for (const width of [240, 158, 120, 61]) {
    const before = texts(bandRows(board, { width }) as Row[])
    const grown = structuredClone(board)
    const c = grown.collections[0]
    for (const q of c.needs_you) q.title += ' and then a great deal more, so that the band must cut it'
    for (const p of c.projects) {
      p.project += '-with-a-longer-name'
      for (const o of p.orchestrators) o.host = 'a-much-longer-host-' + o.host
      // More of what runs, but no idle project wakes: that adds a row.
      if (p.tasks.running) p.tasks.running += 12
    }
    const after = texts(bandRows(grown, { width }) as Row[])
    // Row by row, every column starts where it started.
    for (let i = 1; i < before.length; i += 1) {
      expect(starts(before[i], width).gaps.trim()).toBe('')
      expect(starts(after[i], width)).toEqual(starts(before[i], width))
    }
    // The title row ends at the same column.
    expect(after[0].trimEnd().length).toBe(before[0].trimEnd().length)
  }
  // A needs row's meta (project · age) starts where a project row's bar does,
  // and its status where the orchestrator does.
  const g = bandGrid(160)
  const status = (item: { kind: string }) => (item.kind === 'needs' ? [{ text: '✓ asked', style: 'green' }] : [])
  const lines = texts(bandRows(board, { width: 160, statusFor: status }) as Row[])
  expect(lines[1].indexOf('borant · ')).toBe(5 + g.main + 2)
  expect(lines[3].indexOf('█')).toBe(5 + g.main + 2)
  expect(lines[1].indexOf('✓ asked')).toBe(5 + g.main + 2 + g.meta + 2)
  expect(lines[3].indexOf('orchestrator · ')).toBe(5 + g.main + 2 + g.meta + 2)
})

test('a long title or name is cut at its column with an ellipsis', () => {
  const long = structuredClone(board)
  long.collections[0].projects[1].project = 'roger-emr-observability-dashboards'
  long.collections[0].needs_you[0].title = 'A'.repeat(200)
  for (const width of [240, 120, 100, 80, 60]) {
    const lines = texts(bandRows(long, { width }) as Row[])
    // Every row ends at the grid's row end, never past the band.
    const end = Math.min(width, bandGrid(width).row)
    for (const line of lines.slice(1)) expect(Array.from(line).length).toBe(end)
    expect(lines.find((l) => l.includes('AAAA'))).toContain('A…')
  }
  // At 240 the name column holds a 34-cell name whole.
  expect(texts(bandRows(long, { width: 240 }) as Row[]).join('\n')).toContain('roger-emr-observability-dashboards ')
  expect(texts(bandRows(long, { width: 100 }) as Row[]).join('\n')).toContain('roger-emr-observ…')
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
  for (const word of ['board mode', '0 ', '1-4', 'j k', 'enter', 'esc', '?', '/cos pane', '/cos help', 'Done', 'Ask']) {
    expect(text).toContain(word)
  }
  expect(text).not.toContain('ctrl+x tab')
})

test('a band taller than its room keeps the title and the selected row in view', () => {
  const items = bandItems(board)
  const rows = bandRows(board, { width: 100, maxRows: 3, selected: items[3].key, active: true }) as Row[]
  expect(rows).toHaveLength(3)
  expect(rowText(rows[0].cells)).toContain('need you')
  expect(rows[1].key).toBe(items[3].key)
  expect(rowText(rows[2].cells)).toContain('↑ 3 more')
  expect(bandRows(board, { width: 100, maxRows: 10 })).toHaveLength(5)
  // At rest the window starts at row 1, so the digits drawn are the ones armed.
  const rest = bandRows(board, { width: 100, maxRows: 3, selected: items[3].key }) as Row[]
  expect(rest[1].key).toBe(items[0].key)
  expect(rowText(rest[2].cells)).toContain('0 then j k')
})

test('a press status never moves a column, and the band draws no Done or Ask button', () => {
  const items = bandItems(board)
  const plain = bandRows(board, { width: 120 }) as Row[]
  const pressed = bandRows(board, {
    width: 120,
    statusFor: (item: { key: string }) => (item.key === items[1].key ? [{ text: '◷ closing', style: 'cyan' }] : []),
  }) as Row[]
  expect(rowText(pressed[1].cells)).toBe(rowText(plain[1].cells))
  expect(rowText(pressed[2].cells).replace('◷ closing', '         ')).toBe(rowText(plain[2].cells))
  expect(pressed.flatMap((r) => r.cells).some((c) => c.button?.kind === 'done' || c.button?.kind === 'ask')).toBe(false)
  // Without a tail (a narrow band) the status takes the meta's place.
  const narrow = texts(bandRows(board, { width: 70, statusFor: () => [{ text: '✓ asked', style: 'green' }] }) as Row[])
  expect(narrow[1]).toContain('✓ asked')
  expect(narrow[1]).not.toContain('borant · ')
})

test('from 140 columns a project row has its orchestrator in the tail', () => {
  const line = texts(bandRows(board, { width: 160 }) as Row[])[3]
  expect(line).toMatch(/■2/)
  expect(line).toContain('orchestrator · online · macbook')
  expect(texts(bandRows(board, { width: 120 }) as Row[])[3]).toContain('orchestrator · o…')
  expect(texts(bandRows(board, { width: 80 }) as Row[])[3]).not.toContain('orchestrator')
})

/** The fixture with one blocked spawn on its collection: the hub lists only
 * the spawns that bind no task, so each is a row of its own. */
function withBlocked(spawn: Record<string, unknown>, base: typeof board = board) {
  const next = structuredClone(base) as typeof board & { collections: { blocked?: unknown[] }[] }
  next.collections[0].blocked = [spawn]
  return next
}

const SPAWN = {
  agent: 'w-rs-closeout',
  channel: 'personal-borant',
  project: 'borant',
  reason: 'missing bundle github-work',
  action: 'add bundle github-work to a host that the spawn may use, or spawn without github-work',
  since: 1793022000000,
  spawner: 'orchestrator',
}

test('a blocked spawn is a row of its own: the refusal, and the command, with no Done or Ask', () => {
  const spawn = { ...SPAWN, action: '`borant resume w-rs-closeout` after fixing the launch error' }
  const b = withBlocked(spawn)
  expect(allBlocked(b).map((s: { agent: string }) => s.agent)).toEqual(['w-rs-closeout'])
  // Questions first, then the spawn, then the projects: one group per kind.
  expect(bandItems(b).map((i: { kind: string }) => i.kind)).toEqual(['needs', 'needs', 'blocked', 'project', 'project'])

  const rows = bandRows(b, { width: 160 }) as Row[]
  const lines = texts(rows)
  expect(lines).toHaveLength(6)
  expect(lines[0]).toContain('3 need you')
  expect(lines[3]).toMatch(/^3: ■ w-rs-closeout · missing bundle github-work +borant · 52m/)
  // The unblock action's command is the tail of the row.
  expect(lines[3]).toContain('→ borant resume w-rs-closeout')
  expect(lines[4]).toMatch(/^4: ▲ borant /)
  // One digit per row, blocked spawns included.
  expect(hotkeys(rows[3])).toEqual(['3'])
  expect(rows[3].cells.some((c) => c.button?.kind === 'done' || c.button?.kind === 'ask')).toBe(false)
  // The meta begins where a needs row's does, so the two groups line up.
  const g = bandGrid(160)
  expect(lines[3].indexOf('borant · 52m')).toBe(5 + g.main + 2)
})

test('the board lists blocked spawns, oldest first, and the digest names the cause and the action', () => {
  const b = withBlocked(SPAWN)
  const older = { ...SPAWN, agent: 'w-queue', since: 1793020000000, reason: 'no host is online', action: 'bring a host online' }
  ;(b.collections[0] as { blocked?: unknown[] }).blocked = [SPAWN, older]
  expect(allBlocked(b).map((s: { agent: string }) => s.agent)).toEqual(['w-queue', 'w-rs-closeout'])

  const text = digest(b, { width: 96 })
  expect(text.split('\n')[0]).toContain('4 need you')
  expect(text).toContain('■ Blocked (2)')
  // The digest prints the line the hub's alarm sends, with the same coarse
  // wait (`1h`, not `1h 25m`), so all three name one cause and one action.
  expect(text).toContain('spawn w-queue blocked 1h: no host is online. Unblock: bring a host online')
  expect(text).toContain('spawn w-rs-closeout blocked 52m: missing bundle github-work. Unblock: add bundle')
  // A named project's digest lists no spawn, as it lists no question.
  expect(digest(b, { project: 'borant', width: 96 })).not.toContain('Blocked (')
})

test('a digest blocked line keeps the cause and the action as narrow as 65 cells', () => {
  const b = withBlocked(SPAWN)
  for (const width of [131, 96, 80, 65]) {
    const lines = digest(b, { width }).split('\n').filter((l) => l.includes('spawn w-rs-closeout'))
    expect(lines).toHaveLength(1)
    expect(lines[0]).toContain('missing')
    expect(lines[0]).toContain('Unblock:')
    expect(Array.from(lines[0]).length).toBeLessThanOrEqual(width)
  }
})

test('the unblock action quotes the command a person can copy, when it has one', () => {
  expect(unblockCommand('`borant resume w-x` after fixing the launch error')).toBe('borant resume w-x')
  expect(unblockCommand('bring a host online')).toBe('')
  expect(unblockCommand(undefined)).toBe('')
})
