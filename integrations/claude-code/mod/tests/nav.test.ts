// The pane's navigation model, on the fixture and no session: the inbox's
// cards and project tree, the folds, the breadcrumb, and a task's detail.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import { tasks } from '../fixtures/tasks.js'
import { allQuestions } from '../board.js'
import { breadcrumb, cardNode, channelsFor, detailRows, findProject, foldKey, rowLine, rowsFor, statusWord, taskOf } from '../nav.js'

const ID: Record<number, string> = Object.fromEntries(
  [4, 12, 13, 14, 15, 99].map((n) => [n, '01M5ZG' + String(n).padStart(20, '0')]),
)

type Row = {
  key: string | null
  part: number
  selected: boolean
  marker: { text: string } | null
  indent: number
  segments: { text: string }[]
  buttons: { key: string; hotkey?: string }[]
  status: { text: string }[]
  link: { href: string } | null
}

/** One view's state, with the fixture's board, tasks, and needs-you items. */
function state(extra: Record<string, unknown> = {}) {
  return { board, tasks, questions: allQuestions(board), width: 60, folds: {}, selection: 0, path: [], ...extra }
}

const rows = (extra: Record<string, unknown> = {}) => rowsFor(state(extra)) as Row[]
const text = (list: Row[]) => list.map(rowLine).join('\n')

const epicBackup = {
  kind: 'epic',
  project: 'borant',
  channel: 'personal-borant',
  id: ID[4],
  title: 'Hub backup to object storage',
  saga: 'Elastic compute',
}
const taskBucket = { kind: 'task', project: 'borant', channel: 'personal-borant', id: ID[12], title: 'The bucket' }

test('the fixture tasks hold every task of the fixture board channels', () => {
  expect(Object.keys(tasks)).toEqual([
    'personal-borant',
    'personal-borant-tasks',
    'personal-boopmark',
    'personal-boopmark-tasks',
    'personal-notes-tasks',
  ])
  expect((tasks as Record<string, { id: string }[]>)['personal-borant']).toHaveLength(18)
  expect(taskOf((tasks as Record<string, unknown[]>)['personal-borant'], ID[4]).title).toBe('Hub backup to object storage')
})

test('the inbox is NEEDS YOU, a card per item, then PROJECTS and the idle ones', () => {
  const list = rows()
  const lines = list.map(rowLine)
  expect(lines[0]).toStartWith('NEEDS YOU · 2')
  expect(lines[1]).toStartWith('Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(lines[2]).toContain('borant · Elastic compute · blocks 2 epics')
  expect(lines.findIndex((l) => l.startsWith('PROJECTS · 2 active · 1 idle'))).toBeGreaterThan(2)
  expect(lines[lines.length - 1]).toStartWith('1 idle: boopmark')
  // Two cards and two projects take the selection; projects start folded.
  expect(list.filter((r) => r.key).map((r) => r.key)).toEqual(['row-0', 'row-1', 'row-2', 'row-3'])
  expect(text(list)).not.toContain('Hub backup to object storage')
})

test('the selected card is every line of it, and only it draws d and a', () => {
  const list = rows()
  expect(list.filter((r) => r.selected).length).toBe(3)
  const keys = list.flatMap((r) => r.buttons.map((b) => b.key + ':' + (b.hotkey || '')))
  expect(keys).toEqual(['done-0:d', 'ask-0:a', 'open-0:'])
  // Selecting the second card moves the keys to it.
  const second = rows({ selection: 1 })
  expect(second.findIndex((r) => r.buttons.length)).toBeGreaterThan(second.findIndex((r) => rowLine(r).startsWith('Revoke')))
})

test('a press status takes the keys’ place, on the selected card and the others', () => {
  const q = allQuestions(board)[1]
  const pending = { [q.channel + '/' + q.task]: { state: 'closing', at: 0, title: q.title } }
  const list = rows({ pending, now: 14000 })
  expect(text(list.map((r) => ({ ...r, segments: r.status })))).toContain('the chief is closing it · 14s')
  // While the Done is under way there is no Done to press twice; Ask stays.
  const on = rows({ pending, now: 14000, selection: 1 })
  expect(on.flatMap((r) => r.buttons.map((b) => b.key))).toEqual(['ask-1', 'open-1'])
})

test('an unfolded project lists its open epics with state words, and its done ones as one line', () => {
  const list = rows({ folds: { [foldKey({ project: 'borant' })]: true } })
  const lines = list.map(rowLine)
  expect(lines.some((l) => l.includes('Hub backup to object storage') && l.includes('waits on you'))).toBe(true)
  expect(lines.some((l) => l.includes('2 done'))).toBe(true)
  const notes = rows({ folds: { [foldKey({ project: 'notes' })]: true } })
  expect(text(notes)).toMatch(/plain tasks: \d+ open/)
})

test('every row fits the pane, titles wrap and project names are never cut', () => {
  const long = structuredClone(board)
  long.collections[0].projects[1].project = 'roger-emr-observability-dashboards'
  for (const width of [40, 63, 88]) {
    const list = rowsFor(state({ board: long, questions: allQuestions(long), width })) as Row[]
    for (const r of list) {
      expect(Array.from(rowLine(r)).length).toBeLessThanOrEqual(width - r.indent - (r.marker ? 2 : 0))
    }
    expect(text(list)).toContain('roger-emr-observability-dashboards')
    expect(list.map((r) => rowLine(r).trim()).join(' ')).toContain('Create Hetzner bucket borant-hub-backup + 1Password item')
  }
})

test('an epic view lists its child tasks, and says tasks unavailable', () => {
  const lines = rows({ path: [epicBackup] }).map(rowLine)
  expect(lines[0]).toContain('Hub backup to object storage')
  expect(lines.some((l) => l.includes('The bucket'))).toBe(true)
  expect(text(rows({ path: [epicBackup], tasks: { 'personal-borant': null } }))).toContain('tasks unavailable')
})

test('a task detail shows its title, status, owner, blockers, PR, and last update', () => {
  const list = detailRows(state(), taskBucket, 60) as Row[]
  const all = text(list)
  expect(all).toContain('The bucket')
  expect(all).toContain('w-hubbackup')
  expect(list.find((r) => r.link)?.link?.href).toBe('https://github.com/willisrocks/borant/pull/149')
  expect(all).toMatch(/updated\s+\d{4}-\d{2}-\d{2} \d{2}:\d{2}/)
  // The title alone takes the selection, so Enter there presses nothing.
  expect(list.filter((r) => r.key).map((r) => r.key)).toEqual(['row-0'])
  // Without a task list it says so.
  expect(text(detailRows(state({ tasks: { 'personal-borant': null } }), taskBucket, 60) as Row[])).toContain('tasks unavailable')
})

test('a card opens its task, which shows what the card knows, and its keys', () => {
  const q = allQuestions(board)[1]
  const list = rows({ path: [cardNode(q)], tasks: {} })
  expect(text(list)).toContain('Revoke old willisrocks gh token?')
  expect(text(list)).toContain('needs you')
  expect(list.flatMap((r) => r.buttons.map((b) => b.hotkey))).toEqual(['d', 'a'])
  expect(breadcrumb([cardNode(q)])).toBe('borant › Revoke old willisrocks gh token?')
})

test('the breadcrumb walks the project, the saga, and the task', () => {
  expect(breadcrumb([])).toBe('borant')
  expect(breadcrumb([epicBackup])).toBe('borant › Elastic compute › Hub backup to object storage')
  expect(breadcrumb([epicBackup, taskBucket])).toBe('borant › Elastic compute › Hub backup to object storage › The bucket')
  expect(breadcrumb([{ kind: 'help' }])).toBe('borant › help')
})

test('a view reads the channels it names, and none at the root', () => {
  expect(channelsFor({ path: [] })).toEqual([])
  expect(channelsFor({ path: [epicBackup] })).toEqual(['personal-borant'])
  expect(findProject(board, 'borant')?.sagas[0].title).toBe('Elastic compute')
})

test('status words read plainly', () => {
  expect(statusWord('in_progress')).toBe('in progress')
  expect(statusWord('needs_human')).toBe('needs you')
})

test('a blocker the channel does not list shows its id and a question mark', () => {
  const list = (tasks as Record<string, { id: string; blocked_by: string[] }[]>)['personal-borant'].map((t) =>
    t.id === ID[13] ? { ...t, blocked_by: [ID[99]] } : t,
  )
  const all = text(detailRows(state({ tasks: { 'personal-borant': list } }), { ...taskBucket, id: ID[13], title: 'The restore' }, 60) as Row[])
  expect(all).toContain(ID[99] + '  ?')
})
