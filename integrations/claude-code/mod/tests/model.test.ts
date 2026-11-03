// The model: what needs the person, tasks by state, changes since the
// snapshot, receipts that leave the counts, and PRs.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import channels from '../fixtures/channels.js'
import { tasks } from '../fixtures/tasks.js'
import { buildModel, find, listItems, projectChannels, snapshot, taskState, RECEIPT_MS } from '../model.js'

const NOW = board.at

function seenAll(at = NOW - 3600000) {
  return snapshot(tasks, at)
}

test('every channel that names a project is read, as well as the board\'s sagas and epics', () => {
  const map = projectChannels(board, channels)
  expect(map.get('borant')).toEqual(['personal-borant', 'personal-borant-tasks'])
  expect(map.get('notes')).toEqual(['personal-notes-tasks'])
})

test('needs-you items carry the ask, why it waits, who, and how long', () => {
  const m = buildModel({ board, channels, tasks, seen: seenAll(), now: NOW })
  expect(m.open).toBe(2)
  expect(m.needs[0]).toMatchObject({ verb: 'answer', project: 'borant', ask: 'Create Hetzner bucket borant-hub-backup + 1Password item', why: 'orchestrator waits', owner: 'orchestrator' })
  expect(m.needs[0].age).toBe(NOW - 1793022600000)
})

test('an open task waiting on an open one is later, and names what it waits on', () => {
  const byId = new Map([['a', { id: 'a', status: 'open', title: 'Hub backup' }]])
  expect(taskState({ id: 'b', status: 'open', blocked_by: ['a'] }, byId, false)).toBe('later')
  expect(taskState({ id: 'b', status: 'open', blocked_by: [] }, byId, false)).toBe('ready')
  expect(taskState({ id: 'b', status: 'open', blocked_by: [] }, byId, true)).toBe('blocked')
  expect(taskState({ id: 'b', status: 'in_progress' }, byId, false)).toBe('run')
})

test('projects list their unfinished tasks by name, with who; an idle project is named', () => {
  const m = buildModel({ board, channels, tasks, seen: seenAll(), now: NOW })
  const notes = m.projects.find((p) => p.name === 'notes')
  expect(notes?.tasks.map((t) => [t.title, t.state, t.who])).toEqual([
    ['Weekly digest', 'run', 'orch-notes'],
    ['Tag the old notes', 'ready', ''],
    ['Archive 2025', 'later', ''],
  ])
  expect(m.idle).toEqual(['boopmark'])
})

test('changes since the snapshot: a task done and new tasks; none before a snapshot', () => {
  expect(buildModel({ board, channels, tasks, seen: null, now: NOW }).changes).toEqual([])
  const seen = seenAll()
  const id = tasks['personal-notes-tasks'][0].id
  seen.ids[id] = 'in_progress'
  const done = { ...tasks, 'personal-notes-tasks': tasks['personal-notes-tasks'].map((t) => (t.id === id ? { ...t, status: 'closed', close_reason: 'done', updated: NOW - 60000 } : t)) }
  done['personal-notes-tasks'].push({ ...tasks['personal-notes-tasks'][1], id: '01M5ZGNEW00000000000000001', title: 'Fresh task', created: NOW - 30000, updated: NOW - 30000 })
  const m = buildModel({ board, channels, tasks: done, seen, now: NOW })
  expect(m.changes.map((c) => [c.verb, c.text])).toEqual([
    ['new', '1 task: Fresh task'],
    ['done', 'Weekly digest'],
  ])
})

test('a receipt keeps its row, leaves the count at once, and goes after RECEIPT_MS', () => {
  const m0 = buildModel({ board, channels, tasks, seen: seenAll(), now: NOW })
  const id = m0.needs[0].id
  const marks = { receipts: { [id]: { text: '✓ answered', at: NOW, item: m0.needs[0], index: 0 } }, asked: {}, changes: [] }
  const m1 = buildModel({ board, channels, tasks, seen: seenAll(), marks, now: NOW + 1000 })
  expect(m1.open).toBe(1)
  expect(m1.needs[0].receipt).toBe('✓ answered')
  const m2 = buildModel({ board, channels, tasks, seen: seenAll(), marks, now: NOW + RECEIPT_MS })
  expect(m2.needs[0].receipt).toBeUndefined()
})

test('open tasks with a PR ship next, and the list holds every item in section order', () => {
  const m = buildModel({ board, channels, tasks, seen: seenAll(), now: NOW })
  expect(m.ships.map((s) => s.pr.label).sort()).toEqual(['#149', '#151'])
  const items = listItems(m)
  expect(items[0].section).toBe(1)
  expect(items.at(-1)?.id).toBe('p:notes')
  expect(find(m, 'p:borant')?.kind).toBe('project')
})
