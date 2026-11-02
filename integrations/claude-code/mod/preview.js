// The preview: the band's selected row in full, as the pane draws it. The
// band drives it: j and k move the band's selection and the pane follows.
// Docked beside the transcript it is a tall card; inline above the prompt (a
// narrow terminal) it is a compact block of four or five lines.
//
// Pure, like board.js: no mods API here, so the tests call it with the
// fixture and no session. A line is a list of cells, `{ text, style }` or a
// cell with a `button` the renderer draws as a plain Button.

import {
  GLYPH,
  GONE,
  cellsOf,
  activeSaga,
  allProjects,
  epicNote,
  epicState,
  fit,
  fitEllipsis,
  hhmm,
  openTasksLabel,
  pad,
  progressBar,
  projectParts,
  text,
  truncate,
  waited,
} from './board.js'

/** A needs-you item's name, `<channel>/<task>`, which keys a Done or an Ask. */
export function questionKey(q) {
  return String(q && q.channel) + '/' + String(q && q.task)
}

/** A project of the board by name, or null. */
export function findProject(board, name) {
  return allProjects(board).find((p) => p.project === name) || null
}

/** The channels a project's sagas and epics live in. */
export function channelsOf(project) {
  const out = []
  const add = (channel) => {
    if (channel && !out.includes(channel)) out.push(channel)
  }
  for (const saga of project?.sagas || []) add(saga.channel)
  for (const epic of project?.epics || []) add(epic.channel)
  return out
}

/** The channels "more" reads for one band row. */
export function channelsFor(board, item) {
  if (!item) return []
  if (item.kind === 'needs') return item.q.channel ? [item.q.channel] : []
  return channelsOf(item.project)
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
  const words = { open: 'open', in_progress: 'in progress', needs_human: 'needs you', stuck: 'stuck', closed: 'closed' }
  return words[status] || String(status || '?')
}

/** A board or task Timestamp in local time, to the minute. */
export function localTime(at, fallback = '?') {
  const ms = typeof at === 'string' && at.length >= 16 ? Date.parse(at) : at
  if (typeof ms !== 'number' || !Number.isFinite(ms)) return fallback
  const date = new Date(ms)
  if (Number.isNaN(date.getTime())) return fallback
  const two = (n) => String(n).padStart(2, '0')
  return date.getFullYear() + '-' + two(date.getMonth() + 1) + '-' + two(date.getDate()) + ' ' + two(date.getHours()) + ':' + two(date.getMinutes())
}

/** Words wrapped to `width` cells; a word longer than a line is cut. */
export function wrap(value, width) {
  const w = Math.max(4, width)
  const lines = []
  let line = ''
  for (const word of String(value || '').split(/\s+/).filter(Boolean)) {
    const next = line ? line + ' ' + word : word
    if (cellsOf(next) <= w) line = next
    else {
      if (line) lines.push(line)
      line = truncate(word, w)
    }
  }
  if (line || lines.length === 0) lines.push(line)
  return lines
}

/** A line drawn on the selection's background: the selected row's own title. */
function selected(line) {
  line.selected = true
  return line
}

/** A long id, short: `01M5ZG…0015`. */
function shortId(id) {
  const s = String(id || '')
  return s.length > 12 ? s.slice(0, 6) + '…' + s.slice(-4) : s
}

const DOT = ' · '

/** A label and its value, as the card's field lines read: `  owner     orchestrator`. */
function field(label, value) {
  return [text('  ' + pad(label, 10), 'dim'), ...value]
}

/** The orchestrator that owns an item, with its host and state when the board knows them. */
function ownerParts(board, q) {
  const project = findProject(board, q.project)
  const orchs = project?.orchestrators || []
  const orch = orchs.find((o) => o.agent === q.owner) || (q.owner ? null : orchs[0])
  const agent = q.owner || orch?.agent
  if (!agent) return [text('no orchestrator', 'dim')]
  // The state comes before the host, so a long host is what gets cut.
  const out = [text(agent, 'bold')]
  if (orch) out.push(text(DOT + (orch.online === false ? 'offline' : orch.busy ? 'busy' : 'online'), orch.online === false ? 'red' : 'dim'))
  if (orch?.host) out.push(text(DOT + orch.host, 'dim'))
  return out
}

/** The saga a project is on and its epics, in the board's order. */
function epicsOf(project) {
  const saga = activeSaga(project)
  return [...(saga?.epics || []), ...(project?.epics || [])]
}

/** One line per epic: glyph, title, and its state in words, in two columns. */
function epicLines(epics, width, indent = '  ') {
  const room = Math.max(8, width - indent.length - 2 - 14)
  const col = Math.min(room, Math.max(8, ...epics.map((e) => cellsOf(e.title || ''))))
  return epics.map((epic) => {
    const state = epicState(epic)
    return [
      text(indent),
      text(GLYPH[state] + ' ', state),
      text(pad(epic.title || '', col), state === 'done' ? 'dim' : 'plain'),
      text('  ' + epicNote(epic, state), state === 'waiting' || state === 'done' ? 'dim' : state),
    ]
  })
}

/** The detail of one task: status, blockers, PR, and last update. */
function taskDetail(list, id, indent = '  ') {
  if (list === undefined) return [[text(indent + 'reading the task…', 'dim')]]
  const task = taskOf(list, id)
  if (!task) return [[text(indent + 'task unavailable', 'dim')]]
  const line = (label, value, style = 'plain') => [text(indent + pad(label, 10), 'dim'), text(value, style)]
  const out = [line('status', statusWord(task.status), taskState(task.status))]
  const blockers = Array.isArray(task.blocked_by) ? task.blocked_by : []
  if (!blockers.length) out.push(line('blockers', 'none', 'dim'))
  blockers.forEach((b, i) => {
    const other = taskOf(list, b)
    out.push(line(i ? '' : 'blockers', other ? other.title + '  ' + statusWord(other.status) : b + '  ?', other ? taskState(other.status) : 'dim'))
  })
  const pr = task.properties && typeof task.properties.pr_url === 'string' ? task.properties.pr_url : null
  out.push(line('pr', pr || 'none', pr ? 'cyan' : 'dim'))
  out.push(line('updated', localTime(task.updated), 'dim'))
  return out
}

/** A task's detail in two lines, for the compact block. */
function compactDetail(list, id) {
  if (list === undefined) return [[text('  reading the task\u2026', 'dim')]]
  const task = taskOf(list, id)
  if (!task) return [[text('  task unavailable', 'dim')]]
  const blockers = (Array.isArray(task.blocked_by) ? task.blocked_by : []).map((b) => taskOf(list, b)?.title || b)
  const pr = task.properties && typeof task.properties.pr_url === 'string' ? task.properties.pr_url : 'no PR'
  return [
    [text('  '), text(statusWord(task.status), taskState(task.status)), text(DOT + (task.claimed_by || 'unclaimed') + DOT + 'updated ' + localTime(task.updated), 'dim')],
    [text('  blockers ' + (blockers.length ? blockers.join(', ') : 'none') + DOT + pr, 'dim')],
  ]
}

/** An epic's child tasks, each with its glyph and status, under the epic. */
function childLines(list, epic, indent) {
  if (list === undefined) return [[text(indent + 'reading tasks…', 'dim')]]
  if (list === null) return [[text(indent + 'tasks unavailable', 'dim')]]
  const children = list.filter((t) => t.parent === epic.task)
  if (!children.length) return [[text(indent + 'no child tasks', 'dim')]]
  return children.map((t) => [
    text(indent),
    text(GLYPH[taskState(t.status)] + ' ', taskState(t.status)),
    text(t.title || t.id),
    text('  ' + statusWord(t.status), taskState(t.status) === 'waiting' ? 'dim' : taskState(t.status)),
  ])
}

/** The header: `needs you · 1 of 6` at the left, the freshness at the right. */
function header(left, right, width) {
  const used = cellsOf(left) + right.reduce((n, s) => n + cellsOf(s.text), 0)
  return [text(left, 'dim'), text(' '.repeat(Math.max(1, width - used))), ...right]
}

/**
 * The card for one band row. The row's own title lines carry `selected`, so
 * the pane draws them on the band's selection background. `s` is `{ board, item, index, count, width,
 * compact, more, tasks, actions, fresh }`: `actions` are the cells of the
 * item's Done and Ask (board.js itemActions), `tasks` the task lists read by
 * channel (undefined while a read runs, null when it failed).
 */
export function previewLines(s) {
  const width = Math.max(24, s.width || 60)
  const item = s.item
  if (!item) return [[text('nothing selected', 'dim')]]
  const lines = item.kind === 'needs' ? needsCard(s, width) : projectCard(s, width)
  return lines.map((line) => {
    const out = line.some((c) => c.button) ? line : fitEllipsis(line, width)
    if (line.selected) out.selected = true
    return out
  })
}

function needsCard(s, width) {
  const { q } = s.item
  const board = s.board
  const project = findProject(board, q.project)
  const saga = activeSaga(project)
  const tasks = s.tasks || {}
  const says = q.attention_msg || (q.gate ? 'the saga gate: approve to start' : 'waits on you')
  const glyph = q.closed ? text(GLYPH.done + ' ', 'green') : q.gone ? text(GONE + ' ', 'dim') : text(GLYPH.needs + ' ', 'yellow')
  const titleStyle = q.closed || q.gone ? 'dim' : 'yellowBold'
  if (s.compact) {
    const where = [q.project, saga?.title, says, waited(q, board), ...ownerParts(board, q).map((p) => p.text.replace(/^ · /, ''))].filter(Boolean)
    const out = [
      selected([glyph, text(truncate(q.title || '', width - 2), titleStyle)]),
      [text('  ' + where.join(DOT), 'dim')],
    ]
    const epics = epicsOf(project)
    if (epics.length) {
      const done = epics.filter((e) => epicState(e) === 'done').length
      const line = [text('  saga: ', 'dim')]
      for (const epic of epics.filter((e) => epicState(e) !== 'done')) {
        const state = epicState(epic)
        line.push(text(GLYPH[state] + ' ', state), text(epic.title + '  '))
      }
      if (done) line.push(text(GLYPH.done + done, 'green'))
      out.push(line)
    }
    // The buttons and keys sit in the block's key line (the renderer's); more
    // takes the place of the saga, as the block has four rows at most.
    // Details show the whole title, wrapped, in place of the one cut line.
    if (s.more) return [...wrap(q.title || '', width - 2).map((line, i) => selected([i ? text('  ') : glyph, text(line, titleStyle)])), ...compactDetail(tasks[q.channel], q.task)]
    return out
  }
  const out = [header('needs you' + DOT + (s.index + 1) + ' of ' + s.count, s.fresh || [], width), [text(' ')]]
  // The title takes two lines, padded or cut, so a title that changes on a
  // refresh never moves the fields under it. Details show all of it.
  const lines = wrap(q.title, width - 2)
  const two = s.more ? lines : lines.length > 2 ? [lines[0], truncate(lines.slice(1).join(' '), width - 2)] : [...lines, ''].slice(0, 2)
  two.forEach((line, i) => out.push(selected([i ? text('  ') : glyph, text(line, titleStyle)])))
  if (s.actions && s.actions.length) out.push([text(' ')], [text('  '), ...s.actions])
  out.push([text(' ')])
  out.push(field('project', [text(q.project || '?', 'bold'), ...(saga ? [text(DOT + saga.title, 'dim')] : [])]))
  out.push(field('says', [text(says)]))
  out.push(field('waiting', [text(waited(q, board) || '?'), text(DOT + 'since ' + hhmm(q.since, '?'), 'dim')]))
  out.push(field('owner', ownerParts(board, q)))
  out.push(field('task', [text(q.channel + ' / ' + shortId(q.task), 'dim')]))
  if (s.more) {
    out.push([text(' ')], [text('the task', 'dimBold')], ...taskDetail(tasks[q.channel], q.task))
  }
  const epics = epicsOf(project)
  if (epics.length) {
    const bar = progressBar(saga ? saga.done : 0, saga ? saga.total : 0, 10)
    out.push([text(' ')])
    out.push([
      text('the saga now  ', 'dimBold'),
      ...(saga ? [text(saga.title + '  '), text(bar.filled, 'green'), text(bar.empty, 'faint'), text(' ' + bar.label)] : [text('epics', 'dim')]),
    ])
    out.push(...epicLines(epics, width))
  }
  return out
}

function projectCard(s, width) {
  const project = s.item.project
  const saga = activeSaga(project)
  const epics = epicsOf(project)
  const parts = projectParts(project, 10)
  const counts = project.tasks || {}
  const tasks = s.tasks || {}
  const orch = (project.orchestrators || [])[0]
  const orchText = orch ? [orch.agent, orch.online === false ? 'offline' : orch.busy ? 'busy' : 'online', orch.host].filter(Boolean).join(DOT) : 'no orchestrator online'
  const countText = [
    counts.running ? counts.running + ' running' : '',
    counts.open ? counts.open + ' open' : '',
    counts.blocked ? counts.blocked + ' blocked' : '',
    (counts.needs_human || 0) + (counts.stuck || 0) ? (counts.needs_human || 0) + (counts.stuck || 0) + ' need you' : '',
    counts.done ? counts.done + ' done' : '',
  ].filter(Boolean)
  const more = (indent) => {
    const out = []
    for (const epic of epics) {
      const state = epicState(epic)
      out.push([text(indent), text(GLYPH[state] + ' ', state), text(epic.title || '', 'bold')])
      out.push(...childLines(tasks[epic.channel], epic, indent + '    '))
    }
    if (!epics.length) out.push([text(indent + openTasksLabel(project), 'dim')])
    return out
  }
  if (s.compact) {
    const bar = parts.ratio ? [text(parts.filled, 'green'), text(parts.empty, 'faint'), text(' ' + parts.ratio + DOT, 'plain')] : []
    const out = [
      selected([text(project.project, 'bold'), text(DOT + (saga ? saga.title : epics.length ? 'no saga' : openTasksLabel(project)), 'plain')]),
      [text('  '), ...bar, text([orchText, ...countText].join(DOT), 'dim')],
    ]
    if (s.more) {
      // The block has room for three lines: the project, its owner and
      // counts, then one line per epic with its child tasks.
      return [
        ...out,
        ...epics.map((epic) => {
          const list = tasks[epic.channel]
          const kids = Array.isArray(list) ? list.filter((t) => t.parent === epic.task) : []
          const state = epicState(epic)
          const line = [text('  ' + GLYPH[state] + ' ', state), text(epic.title + ': ', 'bold')]
          if (list === undefined) line.push(text('reading\u2026', 'dim'))
          else if (!kids.length) line.push(text(list === null ? 'tasks unavailable' : 'no child tasks', 'dim'))
          for (const t of kids) line.push(text(GLYPH[taskState(t.status)] + ' ', taskState(t.status)), text(t.title + '  '))
          return line
        }),
      ]
    }
    // The open epics, as many as fit on two lines, then how many are done.
    const open = epics.filter((e) => epicState(e) !== 'done')
    const done = epics.length - open.length
    let line = [text('  ')]
    let used = 2
    let lines = 0
    for (const epic of open) {
      const state = epicState(epic)
      const cellText = GLYPH[state] + ' ' + epic.title + '   '
      if (used + cellsOf(cellText) > width && line.length > 1) {
        out.push(line)
        lines += 1
        if (lines === 2) break
        line = [text('  ')]
        used = 2
      }
      line.push(text(GLYPH[state] + ' ', state), text(epic.title + '   '))
      used += cellsOf(cellText)
    }
    if (done) line.push(text(GLYPH.done + ' ' + done + ' done', 'dim'))
    if (lines < 2 && line.length > 1) out.push(line)
    return out
  }
  const out = [header('project' + DOT + (s.index + 1) + ' of ' + s.count, s.fresh || [], width), [text(' ')]]
  out.push(selected([text(project.project, 'bold')]))
  if (saga) wrap(saga.title, width - 2).forEach((line) => out.push(selected([text('  ' + line)])))
  if (parts.ratio) out.push([text('  '), text(parts.filled, 'green'), text(parts.empty, 'faint'), text(' ' + parts.ratio + ' epics done', 'dim')])
  out.push([text('  ' + orchText, 'dim')])
  out.push([text(' ')])
  if (s.more) out.push(...more('  '))
  else if (epics.length) out.push(...epicLines(epics, width))
  out.push([text(' ')])
  out.push([text('  ' + pad('tasks', 10), 'dim'), text(countText.join(DOT) || 'none open')])
  return out.map((line) => {
    const fitted = fitEllipsis(line, width)
    if (line.selected) fitted.selected = true
    return fitted
  })
}

/** The help view, as lines. */
export function helpPreview(lines, width) {
  const w = Math.max(24, width)
  const out = []
  for (const line of lines) {
    if (!line.startsWith('  ') || cellsOf(line) <= w) {
      out.push(fitEllipsis([text(line, line.startsWith('  ') ? 'plain' : 'dimBold')], w))
      continue
    }
    // A key row: its words wrap under the words, after the key column.
    const keyCells = 18
    const words = wrap(line.slice(keyCells), w - keyCells)
    words.forEach((part, i) => out.push(fit([text((i ? ' '.repeat(keyCells) : line.slice(0, keyCells)) + part)], w)))
  }
  return out
}
