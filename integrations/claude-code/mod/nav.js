// The pane's navigation model: the rows each view draws, which rows fold or
// open, the breadcrumb of where the person is, and a task's detail lines.
//
// Pure, like board.js: no mods API here, so the tests call it with the
// fixture and no session. The renderer turns a selectable row's glyph into
// a Button, which carries the engine's focus ring, and paints the selected
// item, every line of it, on the selection background.
//
// The root view is one list: what needs you, as cards, then the projects as
// a tree. A row is:
//   key        `row-3` while the row can take the selection, else null
//   part       the selectable row it belongs to (a card's lines share one), or -1
//   marker     the glyph segment in the row's first cell
//   indent     blank cells before the marker
//   segments   [{ text, style }], the row's text after the marker and a space
//   status     a press's status segments, drawn before `buttons`
//   buttons    Buttons the line draws (Done, Ask, open the task)
//   drill      the node Enter opens, or null
//   fold       the fold key Enter, h, and l toggle, with `isOpen`
//   parentFold the fold of the project an epic row sits under
//   link       { href, label } a Link a detail line draws

import {
  GLYPH,
  activeProjects,
  activeSaga,
  allProjects,
  epicNote,
  epicState,
  fitEllipsis,
  openTasks,
  pad,
  projectCount,
  projectSummary,
  text,
  wrap,
} from './board.js'
import { ageText, canDone, pressStatus } from './status.js'

/** The first breadcrumb segment: the mod itself. */
export const ROOT = 'borant'

const SEP = ' › '

/** The key that remembers a project's fold. */
export function foldKey(node) {
  return 'project:' + node.project
}

/** Whether a project is unfolded. Projects start folded. */
export function isOpen(folds, node) {
  return Boolean(folds && folds[foldKey(node)])
}

/** The view's name, which keys the remembered selection. */
export function viewKey(path) {
  return (path || []).map((n) => n.kind + ':' + (n.id || n.project || '')).join('/') || 'root'
}

/** A needs-you item's name, `<channel>/<task>`, which keys a pending Done. */
export function questionKey(q) {
  return String(q && q.channel) + '/' + String(q && q.task)
}

/** The breadcrumb of a view below the root: the project, a saga, the titles. */
export function breadcrumb(path) {
  const parts = [ROOT]
  const add = (label) => {
    if (label && parts[parts.length - 1] !== label) parts.push(label)
  }
  ;(path || []).forEach((node, i) => {
    if (node.kind === 'help') return add('help')
    if (i === 0) add(node.project)
    if (node.kind === 'epic') add(node.saga)
    add(node.title)
  })
  return parts.join(SEP)
}

/** A project of the board by name, or null. */
export function findProject(board, name) {
  for (const collection of board?.collections || []) {
    for (const project of collection.projects || []) {
      if (project.project === name) return project
    }
  }
  return null
}

/** The channels a view reads for its tasks: an epic's or a task's. */
export function channelsFor(state) {
  const path = state.path || []
  const last = path[path.length - 1]
  if (last && (last.kind === 'epic' || last.kind === 'task')) return [last.channel]
  return []
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


function row(options) {
  return {
    key: null,
    selectable: false,
    cont: false,
    marker: text(' '),
    indent: 0,
    segments: [],
    status: [],
    buttons: [],
    drill: null,
    fold: null,
    link: null,
    ...options,
  }
}

/** A row that takes the selection. */
function nodeRow(options) {
  return row({ selectable: true, ...options })
}

/** A further line of the selectable row before it: it shares its selection. */
function contRow(options) {
  return row({ cont: true, ...options })
}

/**
 * Number the selectable rows, give each line the item it belongs to, mark
 * the selected item, and fit each row to the cells beside its marker.
 */
function finish(rows, selection, width) {
  let index = -1
  return rows.map((r) => {
    const out = { ...r }
    if (r.selectable) {
      index += 1
      out.key = 'row-' + index
    }
    out.part = r.selectable || r.cont ? index : -1
    out.selected = out.part >= 0 && out.part === selection
    const room = Math.max(1, width - out.indent - (out.marker ? 2 : 0))
    out.segments = r.link || r.segments.length === 0 ? r.segments : fitEllipsis(r.segments, room)
    return out
  })
}

/** A row with its text, for tests. */
export function rowLine(r) {
  return r.segments.map((s) => s.text).join('')
}

/**
 * The rows of the current view. `state` is `{ path, folds, selection, width }`
 * with the board, the drawn needs-you items (`questions`), the presses
 * (`pending`, `asked`), the clock (`now`), the help lines, and the task lists
 * (`tasks`: channel to Task[] or null).
 */
export function rowsFor(state) {
  const width = Math.max(24, state.width || 60)
  const selection = Number(state.selection) || 0
  const last = (state.path || [])[(state.path || []).length - 1]
  if (!last) return inboxRows(state, width, selection)
  if (last.kind === 'help') return finish((state.help || []).map((line) => row({ marker: null, segments: [text(line || ' ', line.startsWith('  ') || !line ? 'plain' : 'dimBold')] })), -1, width)
  if (last.kind === 'task') {
    const rows = detailRows(state, last, width)
    const at = (state.questions || []).findIndex((q) => questionKey(q) === last.question)
    const actions = at >= 0 ? cardActions(state, state.questions[at], at, true, width, false) : null
    return actions ? [...rows, ...finish([row({}), row({ marker: null, ...actions })], -1, width)] : rows
  }
  return epicRows(state, last, width, selection)
}

/** The task a card opens, with what the card knows of it while its
 * channel's task list is not read. */
export function cardNode(q) {
  const task = { id: q.task, title: q.title, status: q.status }
  return { kind: 'task', channel: q.channel, id: q.task, title: q.title, project: q.project, question: questionKey(q), task }
}

/**
 * The line of a card's keys, or of a press's status in their place: `d: Done`,
 * `a: Ask orchestrator`, and on the inbox `\u23ce open the task`. Only the
 * selected card draws keys, so d and a are never two hotkeys in the pane.
 */
function cardActions(state, q, i, selected, width, inbox) {
  const mark = (state.pending || {})[questionKey(q)]
  const ask = (state.asked || {})[questionKey(q)]
  const status = pressStatus(mark, ask, state.now || 0) || []
  // Each card's keys are its own, so the focus ring never carries over from
  // the card the selection left.
  const buttons = []
  if (selected && !q.closed) {
    if (canDone(mark)) buttons.push({ key: 'done-' + i, label: mark ? 'Done again' : 'Done', hotkey: 'd', kind: 'done', arg: i })
    if (!ask || ask.state === 'failed') {
      buttons.push({ key: 'ask-' + i, label: width < 44 ? 'Ask' : 'Ask orchestrator', hotkey: 'a', kind: 'ask', arg: i })
    }
    if (inbox) buttons.push({ key: 'open-' + i, label: '\u23ce open the task', kind: 'open', arg: i })
  }
  return status.length || buttons.length ? { status, buttons } : null
}

/** The meta line of a card: project, saga, why, age, owner. */
function cardMeta(q, board) {
  const project = findProject(board, q.project)
  const saga = project ? activeSaga(project) : null
  const age = typeof q.since === 'number' && typeof board?.at === 'number' ? ageText(board.at - q.since) : ''
  return [q.project, saga?.title, q.attention_msg || (q.gate ? 'gate' : ''), age, q.owner].filter(Boolean).join(' · ')
}

/**
 * The root view: NEEDS YOU, one card per item, then PROJECTS, a row per
 * active project and, unfolded, its open epics, then the idle projects.
 * A card is its title (wrapped, never cut), a dim meta line, and on the
 * selected card the line of its keys, or a press's status in their place.
 */
export function inboxRows(state, width, selection) {
  const board = state.board
  const body = width - 2
  const questions = state.questions || []
  const out = []
  if (!board) {
    out.push(row({ marker: null, segments: [text('board unavailable', 'red'), text(state.reason ? ' \u00b7 ' + state.reason : '', 'dim')] }))
    return finish(out, selection, width)
  }
  const open = questions.filter((q) => !q.closed).length
  out.push(row({ marker: null, segments: [text('NEEDS YOU', 'yellowBold'), text(' · ' + open, 'dim')] }))
  if (questions.length === 0) {
    out.push(row({ marker: text(GLYPH.done, 'green'), segments: [text('nothing needs you', 'green')] }))
    out.push(row({}))
  }
  // A card a Done just closed stays a while, checked off, but takes no
  // selection: the selection moves on to the next card, which then has d and a.
  let live = 0
  questions.forEach((q, i) => {
    const selected = !q.closed && selection === live
    if (!q.closed) live += 1
    const first = q.closed ? row : nodeRow
    const more = q.closed ? row : contRow
    const style = q.closed ? 'dim' : 'yellowBold'
    const lines = wrap(q.title || q.task, body)
    out.push(
      first({
        marker: text(q.closed ? GLYPH.done : GLYPH.needs, q.closed ? 'green' : 'yellow'),
        drill: cardNode(q),
        segments: [text(lines[0], style)],
      }),
    )
    for (const line of lines.slice(1)) out.push(more({ segments: [text(line, style)] }))
    // Inline above the prompt the pane gets few rows, so a card there is its
    // title alone, and the selected one adds its meta and keys.
    if (!state.compact || selected) out.push(more({ segments: [text(cardMeta(q, board), 'dim')] }))
    const actions = cardActions(state, q, i, selected, width, true)
    if (actions) out.push(more(actions))
    if (!state.compact) out.push(row({}))
  })
  if (state.compact && questions.length) out.push(row({}))

  out.push(row({ marker: null, segments: [text('PROJECTS', 'bold'), text(' · ' + projectCount(board), 'dim')] }))
  const projects = activeProjects(board)
  if (projects.length === 0) out.push(row({ segments: [text('no project has open work', 'dim')] }))
  out.push(...projectTree(state, projects, body))
  const idle = allProjects(board).filter((p) => !projects.includes(p)).map((p) => p.project)
  if (idle.length) {
    for (const line of wrap(idle.length + ' idle: ' + idle.join(', '), width)) {
      out.push(row({ marker: null, segments: [text(line, 'dim')] }))
    }
  }
  return finish(out, selection, width)
}

/** The counts after a project's bar: `■2 ▲1`. */
function projectCounts(s) {
  const out = []
  if (s.blocked) out.push(text(GLYPH.blocked + s.blocked, 'red'))
  if (s.needs) out.push(text(GLYPH.needs + s.needs, 'yellow'))
  return out.flatMap((c, i) => (i ? [text(' '), c] : [c]))
}

const BAR = 10
const WORD = 12

/**
 * The project rows. The name column takes the longest name, so a name is
 * never cut; the saga column the longest saga that fits, wrapped when it
 * does not; the bar and the counts follow, and what is left goes at the end.
 */
function projectTree(state, projects, body) {
  const summaries = projects.map((p) => ({ project: p, s: projectSummary(p, BAR) }))
  // A name longer than a third of the pane gets a line of its own, whole,
  // rather than a wide column that leaves a gap beside every short name.
  const cap = Math.max(12, Math.floor(body / 3))
  const nameCol = Math.max(0, ...projects.map((p) => Array.from(p.project).length).filter((n) => n <= cap))
  const ratioCol = Math.max(3, ...summaries.map(({ s }) => (s.plain ? 0 : Array.from(s.bar.label).length)))
  const countsCol = Math.max(0, ...summaries.map(({ s }) => projectCounts(s).reduce((n, c) => n + Array.from(c.text).length, 0)))
  const tailW = BAR + 1 + ratioCol + (countsCol ? 2 + countsCol : 0)
  const sagaText = (s) => (s.plain ? 'plain tasks' : s.saga)
  const longest = Math.max(0, ...summaries.map(({ s }) => Array.from(sagaText(s)).length))
  let sagaCol = Math.min(longest, body - nameCol - 4 - tailW)
  const tailInline = sagaCol >= 10
  if (!tailInline) sagaCol = Math.max(8, body - nameCol - 2)
  const out = []
  for (const { project, s } of summaries) {
    const node = { kind: 'project', project: project.project }
    const unfolded = isOpen(state.folds, node)
    const tail = s.plain
      ? [text(pad(GLYPH.running + ' ' + openTasks(project) + ' open', BAR + 1 + ratioCol), 'blue')]
      : [text(s.bar.filled, 'green'), text(s.bar.empty, 'faint'), text(' ' + pad(s.bar.label, ratioCol))]
    const counts = projectCounts(s)
    if (counts.length) tail.push(text('  '), ...counts)
    const sagaLines = wrap(sagaText(s), sagaCol)
    const sagaStyle = s.plain ? 'dim' : 'plain'
    const long = Array.from(project.project).length > nameCol
    const head = {
      marker: text(unfolded ? '\u25be' : '\u25b8', 'dim'),
      fold: foldKey(node),
      isOpen: unfolded,
      project: project.project,
    }
    if (long) out.push(nodeRow({ ...head, segments: [text(project.project, 'bold')] }))
    const lead = (i) => (i === 0 && !long ? [text(pad(project.project, nameCol), 'bold')] : [text(' '.repeat(nameCol))])
    sagaLines.forEach((line, i) => {
      const segments = [...lead(i), text('  '), text(pad(line, sagaCol), sagaStyle)]
      if (tailInline && i === sagaLines.length - 1) segments.push(text('  '), ...tail)
      if (i === 0 && !long) out.push(nodeRow({ ...head, segments }))
      else out.push(contRow({ segments }))
    })
    if (!tailInline) out.push(contRow({ segments: [text(' '.repeat(nameCol + 2)), ...tail] }))
    if (unfolded) out.push(...epicLines(state, project, body))
  }
  return out
}

/** An unfolded project's open epics, its done ones as one line. */
function epicLines(state, project, body) {
  const out = []
  const saga = activeSaga(project)
  const epics = [...(saga?.epics || []).map((e) => ({ epic: e, saga })), ...(project.epics || []).map((e) => ({ epic: e, saga: null }))]
  const parentFold = foldKey({ project: project.project })
  // The title column takes the longest open epic's title that fits, so the
  // state words sit beside the titles, not at the far edge.
  const longest = Math.max(0, ...epics.filter(({ epic }) => epicState(epic) !== 'done').map(({ epic }) => Array.from(epic.title || '').length))
  const titleCol = Math.max(8, Math.min(longest, body - 2 - 2 - WORD))
  let done = 0
  for (const { epic, saga: owner } of epics) {
    const stateName = epicState(epic)
    if (stateName === 'done') {
      done += 1
      continue
    }
    const node = { kind: 'epic', channel: epic.channel, id: epic.task, title: epic.title, saga: owner?.title, project: project.project }
    const lines = wrap(epic.title, titleCol)
    lines.forEach((line, i) => {
      const segments = [text(pad(line, titleCol)), ...(i === 0 ? [text('  '), text(epicNote(epic, stateName), stateName === 'needs' ? 'yellow' : stateName)] : [])]
      if (i === 0) out.push(nodeRow({ indent: 2, marker: text(GLYPH[stateName], stateName), drill: node, parentFold, segments }))
      else out.push(contRow({ indent: 2, segments }))
    })
  }
  if (done) out.push(row({ indent: 2, marker: text(GLYPH.done, 'doneDim'), segments: [text(done + ' done', 'dim')] }))
  if (epics.length === 0) {
    const c = project.tasks || {}
    const counts = (c.open || 0) + ' open · ' + (c.blocked || 0) + ' blocked · ' + ((c.needs_human || 0) + (c.stuck || 0)) + ' need you · ' + (c.done || 0) + ' done'
    out.push(row({ indent: 2, segments: [text('plain tasks: ' + counts, 'dim')] }))
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
      marker: null,
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
        marker: text(taskGlyph(task), taskState(task.status)),
        drill: { kind: 'task', project: node.project, channel: node.channel, id: task.id, title: task.title },
        segments: [
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
  return finish(out, selection, width)
}

/** The detail of one task: title, status, owner, blockers, PR, last update. */
export function detailRows(state, node, width) {
  const list = (state.tasks || {})[node.channel]
  const task = taskOf(list, node.id) || (node.task || null)
  const out = []
  const push = (segments, link = null) => out.push(row({ marker: null, segments, link }))
  if (!task) {
    push([text('tasks unavailable', 'faint')])
    push([text(taskLine('channel', node.channel), 'dim')])
    return finish(out, 0, width)
  }
  // The title holds the selection, so the focus ring has a harmless home:
  // Enter on it does nothing, where on a key it would press Done or Ask.
  wrap(task.title || task.id, width - 2).forEach((line, i) => {
    if (i === 0) out.push(nodeRow({ marker: text(taskGlyph(task), taskState(task.status)), segments: [text(line, 'bold')] }))
    else out.push(contRow({ segments: [text(line, 'bold')] }))
  })
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
  return finish(out, 0, width)
}

function taskLine(label, value) {
  return '  ' + pad(label, 9) + value
}

function taskGlyph(task) {
  return GLYPH[taskState(task?.status)]
}
