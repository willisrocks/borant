// The pane's navigation model: which rows a view draws, which rows fold or
// drill in, the breadcrumb of where the person is, and a task's detail lines.
//
// Pure, like board.js: no mods API here, so the tests call it with the
// fixture and no session. The mod's renderer turns a row into a Button, and a
// row's `open` into the small ▸ button beside it.
//
// A row is: { key, selectable, marker, open, segments, buttons, link }.
//   key        `row-3` while the row can take the selection, else null
//   marker     `▸` a closed node or a leaf to open, `▾` an open node, ` ` nothing
//   open       { key } to fold, or { node } to drill in, else null
//   segments   [{ text, style }], the row's text after the 2-cell marker cell
//   buttons    action Buttons a row draws under its text (Done, Ask)
//   link       { prefix, href, label } a Link a detail line draws

import {
  GLYPH,
  badge,
  badgeStyle,
  epicNote,
  epicState,
  fitEllipsis,
  hhmm,
  pad,
  projectSummary,
  text,
  truncate,
} from './board.js'

/** The pane's tabs, in the order h and l cycle them. */
export const TABS = ['needs', 'projects', 'help']

/** The first breadcrumb segment: the mod itself. */
export const ROOT = 'borant'

const SEP = ' \u203a '
const SELECTED = '\u258c '

/** The key that remembers a node's fold: a project, a saga, or an epic. */
export function foldKey(node) {
  if (node.kind === 'project') return 'project:' + node.project
  if (node.kind === 'saga') return 'saga:' + node.channel
  return 'epic:' + node.channel + ':' + node.id
}

/** Whether a node is open. Projects and sagas start open, epics closed. */
export function isOpen(folds, node) {
  const key = foldKey(node)
  const value = folds ? folds[key] : undefined
  if (typeof value === 'boolean') return value
  return node.kind !== 'epic'
}

/** The view's name, which keys the remembered selection and the fold state. */
export function viewKey(state) {
  const path = (state.path || []).map((n) => foldKey(n)).join('/')
  return (state.tab || 'needs') + (path ? '/' + path : '')
}

/** The breadcrumb the pane's title and its dim top line show. */
export function breadcrumb(path) {
  const parts = [ROOT]
  for (const node of path || []) {
    if (node.kind === 'epic' && node.saga) parts.push(node.saga)
    const label = node.kind === 'project' ? node.project : node.title
    if (label && parts[parts.length - 1] !== label) parts.push(label)
  }
  return parts.join(SEP)
}

/** Every project of the board, flattened. */
export function findProject(board, name) {
  for (const collection of board?.collections || []) {
    for (const project of collection.projects || []) {
      if (project.project === name) return project
    }
  }
  return null
}

/** The channels a view reads for its tasks: the board names no other. */
export function channelsFor(state) {
  const path = state.path || []
  const last = path[path.length - 1]
  if (!last) return []
  if (last.kind === 'epic' || last.kind === 'task') return [last.channel]
  if (last.kind === 'project' && state.board) {
    const project = findProject(state.board, last.project)
    return channelsOf(project)
  }
  return []
}

function channelsOf(project) {
  const out = []
  const add = (channel) => {
    if (channel && !out.includes(channel)) out.push(channel)
  }
  for (const saga of project?.sagas || []) add(saga.channel)
  for (const epic of project?.epics || []) add(epic.channel)
  return out
}

/** The task of a channel's list by id, or null. */
export function taskOf(list, id) {
  return (list || []).find((t) => t.id === id) || null
}

/** The glyph and style of a task's status, as the board draws epics. */
export function taskState(status) {
  if (status === 'closed') return 'done'
  if (status === 'needs_human' || status === 'stuck') return 'needs'
  if (status === 'in_progress') return 'running'
  return 'waiting'
}

/** The words of a task's status. */
export function statusWord(status) {
  const words = {
    open: 'open',
    in_progress: 'in progress',
    needs_human: 'needs you',
    stuck: 'stuck',
    closed: 'closed',
  }
  return words[status] || String(status || '?')
}

/** A board or task Timestamp in local time, to the minute. */
export function localTime(at, fallback = '?') {
  const ms =
    at instanceof Date ? at.getTime() : typeof at === 'string' && at.length >= 16 ? Date.parse(at) : at
  if (typeof ms !== 'number' || !Number.isFinite(ms)) return fallback
  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) return fallback
  const two = (n) => String(n).padStart(2, '0')
  return (
    date.getFullYear() +
    '-' +
    two(date.getMonth() + 1) +
    '-' +
    two(date.getDate()) +
    ' ' +
    two(date.getHours()) +
    ':' +
    two(date.getMinutes())
  )
}

/** A row with the selection mark and its text, for tests and the digest. */
export function rowLine(row) {
  return row.segments.map((s) => s.text).join('')
}

function row(options) {
  return {
    key: null,
    selectable: false,
    marker: ' ',
    open: null,
    segments: [],
    buttons: [],
    link: null,
    ...options,
  }
}

/**
 * The rows of the current view, each fitted to `width` less the marker cell.
 * `state` is `{ tab, path, folds, selection }` and carries the board and the
 * task lists as `board` and `tasks` (channel → Task[] or null).
 */
export function rowsFor(state) {
  const width = Math.max(20, state.width || 60)
  const selection = Number(state.selection) || 0
  const tab = state.tab || 'needs'
  if (tab === 'help') return helpRows(state, width, selection)
  if (tab === 'needs') return needsRows(state, width, selection)
  const last = (state.path || [])[(state.path || []).length - 1]
  if (last && last.kind === 'task') return detailRows(state, last, width)
  if (last && last.kind === 'epic') return epicRows(state, last, width, selection)
  if (last && last.kind === 'project') return projectRows(state, last, width, selection)
  return projectsRows(state, width, selection)
}

/** Fit every row to the cells left beside the marker, before the bar. */
function finishWidth(rows, width) {
  return rows.map((r) => ({ ...r, width: Math.max(1, width - 2) }))
}

/** Number the selectable rows, mark the selection, and fit each row. */
function finish(rows, selection) {
  let index = 0
  return rows.map((original) => {
    const r = { ...original }
    if (r.open && r.open.kind === 'drill' && !r.drill) r.drill = r.open.node
    let segments = r.segments
    if (r.selectable) {
      r.key = 'row-' + index
      if (index === selection) segments = [text(SELECTED, 'select'), ...segments]
      index += 1
    }
    // A link row draws its label and then the Link itself, not a padded cell.
    const fitted = r.link ? segments : fitEllipsis(segments, Math.max(1, r.width || 1))
    return { ...r, segments: fitted }
  })
}

/** A selectable row of `width` cells, before the marker cell. */
function nodeRow(options) {
  return row({ selectable: true, ...options })
}

function indent(depth) {
  return '  '.repeat(depth)
}

/** The Needs you tab: one selectable row per question, then its buttons. */
export function needsRows(state, width, selection) {
  const questions = state.questions || []
  const out = []
  if (questions.length === 0) {
    out.push(row({ segments: [text('  nothing waits on you', 'green')] }))
  }
  questions.forEach((q, i) => {
    const context = [q.project || '', q.attention_msg || (q.gate ? 'gate' : ''), 'since ' + hhmm(q.since, '?')]
      .filter(Boolean)
      .join(' \u00b7 ')
    out.push(
      nodeRow({
        marker: '\u25b8',
        open: { kind: 'drill', node: { kind: 'task', channel: q.channel, id: q.task, title: q.title, project: q.project } },
        segments: [text(GLYPH.needs + ' ', 'yellow'), text(q.title || '', 'yellowBold'), text('  ' + context, 'dim')],
      }),
    )
    out.push(
      row({
        buttons: [
          { key: 'done-' + i, label: 'Done', kind: 'done', arg: i },
          { key: 'ask-' + i, label: 'Ask orchestrator', kind: 'ask', arg: i },
        ],
      }),
    )
    out.push(row({ segments: [text(' ')] }))
  })
  return finish(finishWidth(out, width), selection)
}

/** The Help tab: one selectable line per help line. */
export function helpRows(state, width, selection) {
  const lines = state.help || []
  return finish(
    finishWidth(
      lines.map((line) =>
        line.trim() === ''
          ? row({ segments: [text(' ')] })
          : nodeRow({ segments: [text(line, line.startsWith('  ') ? 'plain' : 'dimBold')] }),
      ),
      width,
    ),
    selection,
  )
}

/** The Projects tab at the root: every project, open by default. */
export function projectsRows(state, width, selection) {
  const out = []
  for (const collection of state.board?.collections || []) {
    for (const project of collection.projects || []) {
      out.push(...projectTree(state, project, 0, width))
    }
  }
  return finish(finishWidth(out, width), selection)
}

/** One project and, while it is open, its saga epics or its standalone epics. */
function projectTree(state, project, depth, width) {
  const node = { kind: 'project', project: project.project }
  const open = isOpen(state.folds, node)
  const s = projectSummary(project, 10)
  const out = [
    nodeRow({
      marker: open ? '\u25be' : '\u25b8',
      open: { kind: 'fold', key: foldKey(node), isOpen: open },
      drill: node,
      segments: [
        text(indent(depth)),
        text(badge(project), badgeStyle(project, 0)),
        text('  ' + truncate(s.saga, 17) + '  ', 'bold'),
        text(s.bar.filled, 'green'),
        text(s.bar.empty, 'faint'),
        text(' ' + s.bar.label + ' epics', 'plain'),
      ],
    }),
  ]
  if (!open) return out
  for (const saga of project.sagas || []) {
    for (const epic of saga.epics || []) out.push(...epicTree(state, epic, saga, depth + 1, width, project))
  }
  for (const epic of project.epics || []) out.push(...epicTree(state, epic, null, depth + 1, width, project))
  return out
}

/** The project view: its sagas, its standalone epics, and its plain tasks. */
export function projectRows(state, node, width, selection) {
  const project = findProject(state.board, node.project)
  const out = []
  if (!project) {
    out.push(row({ segments: [text('  no project ' + truncate(node.project, 24), 'dim')] }))
    return finish(finishWidth(out, width), selection)
  }
  const tasks = state.tasks || {}
  for (const saga of project.sagas || []) {
    const sagaNode = { kind: 'saga', channel: saga.channel, title: saga.title || saga.channel }
    const open = isOpen(state.folds, sagaNode)
    out.push(
      nodeRow({
        marker: open ? '\u25be' : '\u25b8',
        open: { kind: 'fold', key: foldKey(sagaNode), isOpen: open },
        segments: [
          text((saga.state === 'approved' ? GLYPH.done : GLYPH.scheduled) + ' ', saga.state === 'approved' ? 'green' : 'dim'),
          text(saga.title || saga.channel, 'bold'),
          text('  ' + String(saga.state || '') + '  ' + (saga.done || 0) + '/' + (saga.total || 0) + ' epics', 'dim'),
        ],
      }),
    )
    if (open) {
      for (const epic of saga.epics || []) out.push(...epicTree(state, epic, saga, 1, width, project))
    }
  }
  for (const epic of project.epics || []) out.push(...epicTree(state, epic, null, 0, width, project))

  // The board counts a project's plain tasks but names no channel for them.
  const counts = project.tasks || {}
  const gates = new Set((project.sagas || []).map((s) => s.gate).filter(Boolean))
  const plain = []
  for (const channel of channelsOf(project)) {
    const list = tasks[channel]
    if (!Array.isArray(list)) continue
    for (const task of list) {
      if (task.kind === 'epic' || task.parent || gates.has(task.id)) continue
      plain.push({ channel, task })
    }
  }
  out.push(
    row({
      segments: [
        text(
          'plain tasks  ' +
            (counts.open || 0) +
            ' open \u00b7 ' +
            (counts.blocked || 0) +
            ' blocked \u00b7 ' +
            (counts.needs_human || 0) +
            ' need you \u00b7 ' +
            (counts.done || 0) +
            ' done',
          'dimBold',
        ),
      ],
    }),
  )
  const unreadable = channelsOf(project).filter((c) => tasks[c] === null || tasks[c] === undefined)
  if (unreadable.length) out.push(row({ segments: [text('  tasks unavailable', 'faint')] }))
  for (const { channel, task } of plain) {
    out.push(
      nodeRow({
        marker: '\u25b8',
        open: { kind: 'drill', node: { kind: 'task', project: node.project, channel, id: task.id, title: task.title } },
        segments: [text(taskGlyph(task) + ' ', taskState(task.status)), text(task.title || task.id)],
      }),
    )
  }
  return finish(finishWidth(out, width), selection)
}

/** One epic and, while it is open, the child tasks its channel lists. */
function epicTree(state, epic, saga, depth, width, project) {
  const node = { kind: 'epic', channel: epic.channel, id: epic.task, title: epic.title, saga: saga?.title, project: project?.project }
  const open = isOpen(state.folds, node)
  const stateName = epicState(epic)
  const out = [
    nodeRow({
      marker: open ? '\u25be' : '\u25b8',
      open: { kind: 'fold', key: foldKey(node), isOpen: open },
      drill: node,
      segments: [
        text(indent(depth)),
        text(GLYPH[stateName] + ' ', stateName),
        text(truncate(epic.title, 34), stateName === 'done' ? 'doneDim' : 'plain'),
        text('  ' + epicNote(epic, stateName), 'dim'),
      ],
    }),
  ]
  if (!open) return out
  const list = (state.tasks || {})[epic.channel]
  const children = Array.isArray(list) ? list.filter((t) => t.parent === epic.task) : []
  if (list === null) out.push(row({ segments: [text(indent(depth + 1) + '  tasks unavailable', 'faint')] }))
  if (Array.isArray(list) && children.length === 0) {
    out.push(row({ segments: [text(indent(depth + 1) + '  no child tasks', 'dim')] }))
  }
  for (const task of children) {
    out.push(
      nodeRow({
        marker: '\u25b8',
        open: { kind: 'drill', node: { kind: 'task', project: project?.project, channel: epic.channel, id: task.id, title: task.title } },
        segments: [
          text(indent(depth + 1) + '  '),
          text(taskGlyph(task) + ' ', taskState(task.status)),
          text(task.title || task.id),
          text('  ' + statusWord(task.status), taskState(task.status)),
        ],
      }),
    )
  }
  return out
}

/** The epic view: its child tasks, one row each. */
export function epicRows(state, node, width, selection) {
  const list = (state.tasks || {})[node.channel]
  const out = []
  const children = Array.isArray(list) ? list.filter((t) => t.parent === node.id) : []
  out.push(
    row({
      segments: [
        text('epic  ', 'dimBold'),
        text(node.title, 'bold'),
        text('  ' + (Array.isArray(list) ? children.length + ' tasks' : 'tasks unavailable'), 'dim'),
      ],
    }),
  )
  for (const task of children) {
    out.push(
      nodeRow({
        marker: '\u25b8',
        open: { kind: 'drill', node: { kind: 'task', project: node.project, channel: node.channel, id: task.id, title: task.title } },
        segments: [
          text(taskGlyph(task) + ' ', taskState(task.status)),
          text(task.title || task.id),
          text('  ' + statusWord(task.status), taskState(task.status)),
          text(task.claimed_by ? '  ' + task.claimed_by : '', 'dim'),
        ],
      }),
    )
  }
  if (Array.isArray(list) && children.length === 0) {
    out.push(row({ segments: [text('  no child tasks', 'dim')] }))
  }
  return finish(finishWidth(out, width), selection)
}

/** The detail of one task: title, status, owner, blockers, PR, last update. */
export function detailRows(state, node, width) {
  const list = (state.tasks || {})[node.channel]
  const task = taskOf(list, node.id) || (node.task || null)
  const out = []
  const push = (segments, link = null) => out.push(row({ segments, link }))
  if (!task) {
    push([text('tasks unavailable', 'faint')])
    push([text(taskLine('channel', node.channel), 'dim')])
    return finish(finishWidth(out, width), -1)
  }
  push([text(task.title || task.id, 'bold')])
  push([text(taskLine('status', statusWord(task.status)), taskState(task.status))])
  push([text(taskLine('owner', task.claimed_by || 'unclaimed'), task.claimed_by ? 'plain' : 'dim')])
  const blockers = Array.isArray(task.blocked_by) ? task.blocked_by : []
  if (blockers.length === 0) {
    push([text(taskLine('blockers', 'none'), 'dim')])
  }
  blockers.forEach((id, i) => {
    const other = taskOf(list, id)
    const line =
      (i === 0 ? taskLine('blockers', '') : '         ') +
      (other ? other.title + '  ' + statusWord(other.status) : id + '  ?')
    push([text(line, other ? taskState(other.status) : 'dim')])
  })
  const pr = task.properties && typeof task.properties.pr_url === 'string' ? task.properties.pr_url : null
  if (pr) push([text(taskLine('pr', ''), 'dim')], { href: pr, label: pr })
  else push([text(taskLine('pr', 'none'), 'dim')])
  push([text(taskLine('updated', localTime(task.updated)), 'dim')])
  push([text(taskLine('channel', node.channel + ' / ' + (task.id || node.id)), 'dim')])
  return finish(finishWidth(out, width), -1)
}

function taskLine(label, value) {
  return '  ' + pad(label, 9) + value
}

function taskGlyph(task) {
  return GLYPH[taskState(task?.status)]
}
