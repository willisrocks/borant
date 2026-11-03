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

// A board with task lines, as `borant board --json --tasks` gives it.
function linesBoard() {
  const b = JSON.parse(JSON.stringify(board))
  const notes = b.collections[0].projects.find((p: { project: string }) => p.project === 'notes')
  notes.task_lines = [
    { channel: 'personal-notes-tasks', task: 'T1', title: 'Weekly digest', kind: 'task', status: 'in_progress', start: null, waits_on: null, claimed_by: 'w-notes', since: NOW - 40 * 60000, attention_msg: null, version: NOW - 40 * 60000, pr: { url: 'https://github.com/x/notes/pull/7', state: 'open', checks: { pass: {} }, checks_passed: 5, checks_total: 5, review: 'approved', review_requested: false } },
    { channel: 'personal-notes-tasks', task: 'T2', title: 'Archive 2025', kind: 'task', status: 'open', start: 'later', waits_on: 'Weekly digest', claimed_by: null, since: NOW, attention_msg: null, version: NOW, pr: null },
  ]
  notes.recent_done = [{ channel: 'personal-notes-tasks', task: 'T0', title: 'The import', closer: 'w-notes', at: NOW - 60000, pr_url: null }]
  return b
}

test('task lines from the board: start state, PR state in words, and the recent done', () => {
  const m = buildModel({ board: linesBoard(), channels, tasks: {}, seen: null, now: NOW })
  const notes = m.projects.find((p) => p.name === 'notes')
  expect(notes?.tasks.map((t) => [t.title, t.state, t.note])).toEqual([
    ['Weekly digest', 'run', ''],
    ['Archive 2025', 'later', 'after Weekly digest'],
    ['The import', 'done', ''],
  ])
  expect(m.ships[0].pr.words).toBe('ready to merge')
})

test('a claimed task whose worker made no tool call for 30 minutes is silent; no data is not silence', () => {
  const activity = { 'w-notes': { agent: 'w-notes', last_tool_at: NOW - 31 * 60000, last_tool: 'Bash npm test', tool_running_since: null } }
  const m = buildModel({ board: linesBoard(), channels, tasks: {}, seen: null, now: NOW, activity })
  expect(m.quiet.map((q) => [q.verb, q.who])).toContainEqual(['silent', 'w-notes'])
  const running = { 'w-notes': { ...activity['w-notes'], tool_running_since: NOW - 40 * 60000 } }
  expect(buildModel({ board: linesBoard(), channels, tasks: {}, seen: null, now: NOW, activity: running }).quiet.some((q) => q.verb === 'silent')).toBe(false)
  expect(buildModel({ board: linesBoard(), channels, tasks: {}, seen: null, now: NOW, activity: {} }).quiet.some((q) => q.verb === 'silent')).toBe(false)
})

test('the hub feed gives the changes, in words; the person\'s own action shows until the feed has it', () => {
  const feed = [{ seq: 12, at: NOW - 1000, project: 'notes', channel: 'personal-notes-tasks', task: 'T0', verb: 'pr_merged', text: 'PR #7 The import', by: { type: 'agent', id: 'w-notes' } }]
  const marks = { receipts: {}, asked: {}, changes: [{ id: 'c:you', verb: 'answered', project: 'borant', text: 'x (you)', who: 'you', at: NOW, task: 'Q1' }] }
  const m = buildModel({ board: linesBoard(), channels, tasks: {}, seen: { at: NOW - 3600000, ids: {} }, marks, now: NOW, feed })
  expect(m.changes.map((c) => [c.verb, c.who])).toEqual([['answered', 'you'], ['merged', 'w-notes']])
  const fed = [...feed, { seq: 13, at: NOW, project: 'borant', channel: 'personal-borant', task: 'Q1', verb: 'answered', text: 'x', by: 'human' }]
  expect(buildModel({ board: linesBoard(), channels, tasks: {}, seen: { at: NOW - 3600000, ids: {} }, marks, now: NOW, feed: fed }).changes.filter((c) => c.verb === 'answered')).toHaveLength(1)
})
