// The pane's navigation model, on the fixture and no session: the rows each
// view draws, the folds, the breadcrumb, and a task's detail.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import { tasks } from '../fixtures/tasks.js'
import {
  breadcrumb,
  channelsFor,
  detailRows,
  epicRows,
  findProject,
  foldKey,
  rowLine,
  rowsFor,
  statusWord,
  taskOf,
} from '../nav.js'

const ID: Record<number, string> = Object.fromEntries(
  [4, 12, 13, 14, 15, 99].map((n) => [n, '01M5ZG' + String(n).padStart(20, '0')]),
)

/** One view's state, with the fixture's board and tasks. */
function state(extra: Record<string, unknown> = {}) {
  return { board, tasks, width: 60, folds: {}, selection: 0, tab: 'projects', path: [], ...extra }
}

const projectBorant = { kind: 'project', project: 'borant' }
const epicBackup = {
  kind: 'epic',
  project: 'borant',
  channel: 'personal-borant',
  id: ID[4],
  title: 'Hub backup to object storage',
  saga: 'Elastic compute',
}
const taskBucket = {
  kind: 'task',
  project: 'borant',
  channel: 'personal-borant',
  id: ID[12],
  title: 'The bucket',
}

test('the fixture tasks hold every task of the fixture board channels', () => {
  expect(Object.keys(tasks)).toEqual([
    'personal-borant',
    'personal-borant-tasks',
    'personal-boopmark',
    'personal-boopmark-tasks',
  ])
  // The saga channel: the closed gate, five epics, and their children.
  expect((tasks as Record<string, { id: string }[]>)['personal-borant']).toHaveLength(18)
  expect(taskOf((tasks as Record<string, unknown[]>)['personal-borant'], ID[4]).title).toBe(
    'Hub backup to object storage',
  )
  expect(taskOf((tasks as Record<string, unknown[]>)['personal-borant'], ID[15]).status).toBe('needs_human')
  // The project channel's plain tasks, which the board counts but does not list.
  expect((tasks as Record<string, { parent: string | null }[]>)['personal-borant-tasks']).toHaveLength(15)
})

test('the Projects tab draws a project tree, and folds a project', () => {
  const open = rowsFor(state()) as { key: string; marker: string; segments: { text: string }[] }[]
  const lines = open.map(rowLine)
  expect(lines.some((l: string) => l.includes('no saga'))).toBe(true)
  expect(lines.some((l: string) => l.includes('Elastic compute'))).toBe(true)
  expect(lines.some((l: string) => l.includes('Hub backup to object storage'))).toBe(true)
  expect(open.filter((r) => r.key).length).toBe(8)
  expect(open[0].marker).toBe('\u25be')

  const folded = rowsFor(state({ folds: { [foldKey(projectBorant)]: false } })) as typeof open
  expect(folded.map(rowLine).some((l: string) => l.includes('Hub backup to object storage'))).toBe(false)
  expect(folded[2].marker).toBe('\u25b8')
})

test('a project view lists its saga and epics, and folds the saga', () => {
  const rows = rowsFor(state({ path: [projectBorant] })) as { key: string; segments: { text: string }[] }[]
  const lines = rows.map(rowLine)
  expect(lines.some((l: string) => l.includes('Elastic compute'))).toBe(true)
  expect(lines.some((l: string) => l.includes('2/5 epics'))).toBe(true)
  expect(lines.some((l: string) => l.includes('Hub backup to object storage'))).toBe(true)
  expect(lines.some((l: string) => l.includes('plain tasks  3 open'))).toBe(true)
  // The gate is a saga task, not a plain task of the project.
  expect(lines.some((l: string) => l.includes('Approve the scope'))).toBe(false)

  const saga = { kind: 'saga', channel: 'personal-borant', title: 'Elastic compute' }
  const folded = rowsFor(state({ path: [projectBorant], folds: { [foldKey(saga)]: false } })) as typeof rows
  expect(folded.map(rowLine).some((l: string) => l.includes('Hub backup to object storage'))).toBe(false)
})

test('an epic view lists its child tasks, and says tasks unavailable', () => {
  const rows = rowsFor(state({ path: [projectBorant, epicBackup] })) as { key: string; segments: { text: string }[] }[]
  const lines = rows.map(rowLine)
  expect(lines[0]).toContain('Hub backup to object storage')
  expect(lines).toHaveLength(6)
  expect(lines.some((l: string) => l.includes('The bucket'))).toBe(true)
  expect(lines.some((l: string) => l.includes('Create Hetzner bucket'))).toBe(true)

  const failed = rowsFor(
    state({ path: [projectBorant, epicBackup], tasks: { 'personal-borant': null } }),
  ) as typeof rows
  expect(failed.map(rowLine).some((l: string) => l.includes('tasks unavailable'))).toBe(true)
})

test('a task detail shows its title, status, owner, blockers, PR, and last update', () => {
  const rows = detailRows(state(), taskBucket, 60) as { key: string | null; segments: { text: string }[]; link: { href: string } | null }[]
  const text = rows.map(rowLine).join('\n')
  expect(text).toContain('The bucket')
  expect(text).toContain('open')
  expect(text).toContain('w-hubbackup')
  expect(text).toContain('none')
  const link = rows.find((r) => r.link)
  expect(link?.link?.href).toBe('https://github.com/willisrocks/borant/pull/149')
  expect(text).toMatch(/updated\s+\d{4}-\d{2}-\d{2} \d{2}:\d{2}/)
  expect(rows.every((r) => r.key === null)).toBe(true)

  // The backup job is blocked by the bucket, so its title and state show.
  const blocked = rowsFor(state({ path: [projectBorant, epicBackup, { ...taskBucket, id: ID[14], title: 'The backup job' }] })) as typeof rows
  const blockedText = blocked.map(rowLine).join('\n')
  expect(blockedText).toContain('The backup job')
  expect(blockedText).toContain('The bucket  open')
})

test('a task detail with no task list says tasks unavailable', () => {
  const rows = detailRows(state({ tasks: { 'personal-borant': null } }), taskBucket, 60) as { segments: { text: string }[] }[]
  expect(rows.map(rowLine).join('\n')).toContain('tasks unavailable')
})

test('the breadcrumb walks the project, the saga, and the task', () => {
  expect(breadcrumb([])).toBe('borant')
  expect(breadcrumb([projectBorant])).toBe('borant')
  expect(breadcrumb([{ kind: 'project', project: 'boopmark' }])).toBe('borant \u203a boopmark')
  expect(breadcrumb([projectBorant, epicBackup])).toBe('borant \u203a Elastic compute \u203a Hub backup to object storage')
  expect(breadcrumb([projectBorant, epicBackup, taskBucket])).toBe(
    'borant \u203a Elastic compute \u203a Hub backup to object storage \u203a The bucket',
  )
})

test('a view reads the channels the board names for it, and none at the root', () => {
  expect(channelsFor({ path: [] })).toEqual([])
  expect(channelsFor({ path: [projectBorant], board })).toEqual(['personal-borant'])
  expect(channelsFor({ path: [projectBorant, epicBackup], board })).toEqual(['personal-borant'])
  expect(findProject(board, 'borant')?.sagas[0].title).toBe('Elastic compute')
})

test('every row fits the pane at any width, and the help is selectable', () => {
  const help = ['keys', '  \u2191 \u2193  j k     move the selection', '', 'commands', '  /cos  the board']
  for (const width of [40, 60, 90]) {
    const rows = rowsFor(state({ tab: 'help', help, width })) as { key: string | null; segments: { text: string }[] }[]
    expect(rows.filter((r) => r.key).length).toBe(4)
    for (const row of rows) expect(Array.from(rowLine(row)).length).toBe(width - 2)
  }
  const needs = rowsFor(state({ tab: 'needs', path: [], questions: [] })) as { segments: { text: string }[] }[]
  expect(needs.map(rowLine).join('\n')).toContain('nothing waits on you')
})

test('status words read plainly', () => {
  expect(statusWord('in_progress')).toBe('in progress')
  expect(statusWord('needs_human')).toBe('needs you')
  expect(statusWord('closed')).toBe('closed')
})

test('a blocker the channel does not list shows its id and a question mark', () => {
  const list = (tasks as Record<string, { id: string; blocked_by: string[] }[]>)['personal-borant'].map((t) =>
    t.id === ID[13] ? { ...t, blocked_by: [ID[99]] } : t,
  )
  const rows = detailRows(
    state({ tasks: { 'personal-borant': list } }),
    { ...taskBucket, id: ID[13], title: 'The restore' },
    60,
  ) as { segments: { text: string }[] }[]
  const text = rows.map(rowLine).join('\n')
  expect(text).toContain('The restore')
  expect(text).toContain(ID[99] + '  ?')
})
