// The chief-of-staff board: a pure mapping from a BoardView (the hub's
// `board` read) to everything the mod draws. No mods API here, so the tests
// call it with a fixture and no session.
//
// A row is a list of segments: { text, style, button? }. `text` is what the
// row shows, `style` picks a color, and `button` marks a cell the renderer
// turns into a Button.
//
// The shape is the one in docs/superpowers/specs/2026-10-26-sagas-and-epics-design.md,
// section "API, MCP, and CLI".

/** The glyph of each epic state, as the mockup draws it. */
export const GLYPH = {
  done: '\u2713',
  running: '\u25cf',
  blocked: '\u25a0',
  needs: '\u25b2',
  waiting: '\u25cb',
  scheduled: '\u25c7',
}

const BRANCH = '\u2502'
const RULE = '\u2500'

/** One segment of a row. */
export function text(text, style = 'plain') {
  return { text: String(text), style }
}

/** The plain text of a row, for tests and for the digest. */
export function rowText(row) {
  return row.map((s) => s.text).join('')
}

/** Cut text to `width` cells, with an ellipsis. */
export function truncate(value, width) {
  if (width <= 0) return ''
  const chars = Array.from(String(value))
  if (chars.length <= width) return String(value)
  if (width === 1) return '\u2026'
  return chars.slice(0, width - 1).join('') + '\u2026'
}

/** Pad text to exactly `width` cells, cutting it when it is longer. */
export function pad(value, width) {
  const cut = truncate(value, width)
  return cut + ' '.repeat(Math.max(0, width - Array.from(cut).length))
}

/** Trim segments to `width` cells and pad the row to full width. */
export function fit(segments, width) {
  const out = []
  let used = 0
  for (const s of segments) {
    if (used >= width) break
    const take = Array.from(s.text).slice(0, width - used).join('')
    if (take) out.push({ ...s, text: take })
    used += Array.from(take).length
  }
  if (used < width) out.push(text(' '.repeat(width - used)))
  return out
}

/** The instant of a board Timestamp, or null. The hub writes integer
 * milliseconds; an older fixture may hold an ISO string, so both read. */
function instant(at) {
  const ms =
    at instanceof Date ? at.getTime() : typeof at === 'string' && at.length >= 16 ? Date.parse(at) : at
  if (typeof ms !== 'number' || !Number.isFinite(ms)) return null
  const date = new Date(ms)
  return Number.isNaN(date.getTime()) ? null : date
}

/** `14:32` in the local time of the session, from a board Timestamp. */
export function hhmm(at, fallback = '--:--') {
  const date = instant(at)
  if (!date) return fallback
  const two = (n) => String(n).padStart(2, '0')
  return two(date.getHours()) + ':' + two(date.getMinutes())
}

/** The milliseconds of a board Timestamp, or null. */
export function stampOf(at) {
  const date = instant(at)
  return date ? date.getTime() : null
}

/** The filled cells and the label of a progress bar, as the mockup draws it. */
export function progressBar(done, total, cells = 10) {
  const d = Math.max(0, Number(done) || 0)
  const t = Math.max(0, Number(total) || 0)
  // A project with no epics has nothing to fill: no bar, and a dash for the
  // count. The blank cells keep the columns of the rows beside it in place.
  if (t === 0) return { filled: '', empty: ' '.repeat(cells), label: '\u2014', text: '\u2014' }
  const filled = Math.max(0, Math.min(cells, Math.round((d / t) * cells)))
  return {
    filled: '\u2588'.repeat(filled),
    empty: '\u2591'.repeat(cells - filled),
    label: d + '/' + t,
    text: '\u2588'.repeat(filled) + '\u2591'.repeat(cells - filled) + ' ' + d + '/' + t,
  }
}

/** Every project of every collection, in the order the board gives. */
export function allProjects(board) {
  const out = []
  for (const collection of board?.collections || []) {
    for (const project of collection.projects || []) out.push(project)
  }
  return out
}

/** The plain tasks of a project that are not done: open, running, or waiting on a person. */
export function openTasks(project) {
  const c = project?.tasks || {}
  return (c.open || 0) + (c.running || 0) + (c.stuck || 0) + (c.needs_human || 0)
}

/** A project has work while a saga or an epic is open, or a plain task is. */
export function isActive(project) {
  if ((project?.sagas || []).some((s) => s.state !== 'closed')) return true
  const epics = [...(project?.epics || []), ...(project?.sagas || []).flatMap((s) => s.epics || [])]
  if (epics.some((e) => e.status !== 'closed')) return true
  return openTasks(project) > 0
}

/** The projects the band, the digest, and the Projects tab list: those with work. */
export function activeProjects(board) {
  return allProjects(board).filter(isActive)
}

/** The project count of a header: `4 active · 3 idle`, or `2 projects` when none is idle. */
export function projectCount(board) {
  const all = allProjects(board).length
  const active = activeProjects(board).length
  if (active === all) return all + (all === 1 ? ' project' : ' projects')
  return active + ' active \u00b7 ' + (all - active) + ' idle'
}

/** What a project without a saga or an epic shows: `3 open tasks`. */
export function openTasksLabel(project) {
  const n = openTasks(project)
  return n + (n === 1 ? ' open task' : ' open tasks')
}

/** Every needs-you question, gates first and then the oldest. */
export function allQuestions(board) {
  const out = []
  for (const collection of board?.collections || []) {
    for (const q of collection.needs_you || []) out.push(q)
  }
  return out.sort((a, b) => {
    const gate = Number(Boolean(b.gate)) - Number(Boolean(a.gate))
    if (gate !== 0) return gate
    const since = (stampOf(a.since) ?? 0) - (stampOf(b.since) ?? 0)
    if (since !== 0) return since
    return String(a.title || '').localeCompare(String(b.title || ''))
  })
}

export function countNeedsYou(board) {
  return allQuestions(board).length
}

/** A project's active saga, the first in the board's order that is not closed. */
export function activeSaga(project) {
  const sagas = project?.sagas || []
  return sagas.find((s) => s.state !== 'closed') || sagas[0] || null
}

/** The state of one epic, as the mockup names it. */
export function epicState(epic) {
  const counts = epic?.counts || {}
  if (epic?.status === 'closed') return 'done'
  if (epic?.status === 'needs_human' || epic?.status === 'stuck') return 'needs'
  if ((counts.needs_human || 0) > 0 || (counts.stuck || 0) > 0) return 'needs'
  if ((counts.blocked || 0) > 0) return 'blocked'
  if (epic?.status === 'in_progress' || (counts.running || 0) > 0) return 'running'
  return 'waiting'
}

/** The epic a project row shows: the one that needs the person, else a working one, else waiting. */
export function currentEpic(project) {
  const epics = [...(activeSaga(project)?.epics || []), ...(project?.epics || [])]
  const rank = { needs: 0, blocked: 1, running: 2, waiting: 3, done: 4 }
  let best = null
  for (const epic of epics) {
    const state = epicState(epic)
    if (!best || rank[state] < rank[best.state]) best = { epic, state }
  }
  return best
}

/** A project's one-line summary: name, saga, bar, and counts. */
export function projectSummary(project, cells = 10) {
  const saga = activeSaga(project)
  const done = saga ? saga.done || 0 : totalDone(project)
  const total = saga ? saga.total || 0 : totalEpics(project)
  const counts = project?.tasks || {}
  return {
    project: project?.project || '?',
    // A project with no saga and no epic has only plain tasks: name them.
    plain: !saga && totalEpics(project) === 0,
    saga: saga ? saga.title : totalEpics(project) ? 'no saga' : 'plain tasks',
    bar: progressBar(done, total, cells),
    running: counts.running || 0,
    blocked: counts.blocked || 0,
    needs: (counts.needs_human || 0) + (counts.stuck || 0),
  }
}

function totalEpics(project) {
  return (project?.epics || []).length
}

function totalDone(project) {
  return (project?.epics || []).filter((e) => epicState(e) === 'done').length
}

/** The orchestrators of a project, as a dim line. */
export function orchestratorLine(project) {
  const orch = (project?.orchestrators || [])[0]
  if (!orch) return 'no orchestrator online'
  const state = orch.busy ? 'busy' : 'online'
  const host = orch.host ? ' \u00b7 ' + orch.host : ''
  return GLYPH.running + ' ' + orch.agent + ' \u00b7 ' + state + host
}

/** The digits that select rows, as the band hints them: `1`, `1-3`, or `` when
 * no row is there. Rows are one per needs-you item, then one per project. */
export function digitRange(rows) {
  const n = Math.max(0, Number(rows) || 0)
  if (n === 0) return ''
  return n === 1 ? '1' : '1-' + Math.min(9, n)
}

/** The key of a needs-you item, and of its Done and Ask marks. */
export function questionKey(q) {
  return String(q && q.channel) + '/' + String(q && q.task)
}

/** A project of the board by name, or null. */
export function findProject(board, name) {
  return allProjects(board).find((p) => p.project === name) || null
}

/**
 * Every key and command of the board, as `/cos help` prints it.
 */
export function helpLines(options = {}) {
  const digits = digitRange(options.rows) || '1'
  const row = (key, what) => '  ' + pad(key, 21) + what
  return [
    'keys (the band above the prompt holds them all)',
    row(digits, 'select a row from an empty prompt: needs-you items, then projects'),
    row('j k', 'move the selection down and up'),
    row('space', 'open the selected row in place, or close it (its digit too)'),
    row('d', 'Done on the selected needs-you item'),
    row('a', 'Ask orchestrator about the selected needs-you item'),
    row('', 'after a digit these keys work from the empty prompt for 30 s;'),
    row('', 'any other key types, and the selection goes'),
    row('ctrl+x tab', 'give the band the keyboard (press tab within 3 s): then'),
    row('', 'enter opens, and esc goes back to the prompt'),
    row('ctrl+x ctrl+a', 'collapse the band to one row, or expand it'),
    row('click', 'a row selects it; a second click opens it'),
    '',
    'commands',
    row('/cos', 'the board as text'),
    row('/cos <project>', 'one project'),
    row('/cos help', 'this help'),
    '',
    'buttons',
    row('d: Done', 'submits "Chris marked done: <item>" to the chief, who closes it;'),
    row('', 'the row shows sending, closing, closed, or still open in its place'),
    row('a: Ask orchestrator', 'sends a question about <item> to its channel\u2019s owner;'),
    row('', 'the chief is only told, and relays the reply'),
    '',
    'title row',
    row('live', 'the event stream runs; the age shows after 30 s'),
    row('\u21bb reading', 'a read runs now'),
    row('polling \u00b7 4s', 'no event stream: a read every 5 s'),
    row('stale \u00b7 2m', 'reads fail; the rows are that old'),
  ]
}

/** The state of a project's plain tasks, for a project with no epic. */
export function plainState(project) {
  const c = project?.tasks || {}
  if ((c.needs_human || 0) + (c.stuck || 0) > 0) return 'needs'
  if ((c.running || 0) > 0) return 'running'
  if ((c.blocked || 0) > 0 && !(c.ready || 0)) return 'blocked'
  return 'waiting'
}

/** The band as one row, for a band with room for one row. */
export function condensedRow(board, options = {}) {
  const width = Math.max(20, options.width || 100)
  const questions = allQuestions(board)
  const parts = []
  parts.push(text('cos  ', 'dimBold'))
  if (questions.length) {
    parts.push(text(GLYPH.needs + ' ' + questions.length + ' need you', 'yellowBold'))
  } else {
    parts.push(text(GLYPH.done + ' all clear', 'green'))
  }
  for (const project of activeProjects(board)) {
    const s = projectSummary(project, 10)
    parts.push(text('  ' + BRANCH + '  ', 'faint'))
    parts.push(text(project.project, 'bold'))
    parts.push(text(' ' + (s.plain ? openTasksLabel(project) : s.bar.label)))
    if (s.blocked) parts.push(text(' ' + GLYPH.blocked + s.blocked, 'red'))
    if (s.needs) parts.push(text(' ' + GLYPH.needs + s.needs, 'yellow'))
  }
  return fit(parts, width)
}

/**
 * The key j (`delta` 1) or k (-1) moves to from `current`, wrapping at the
 * ends. From anything else, such as the help button, j takes the first row
 * and k the last. Null when there is no row.
 */
export function stepKey(keys, current, delta) {
  if (keys.length === 0) return null
  const at = keys.indexOf(current)
  if (at < 0) return delta > 0 ? keys[0] : keys[keys.length - 1]
  return keys[(at + delta + keys.length) % keys.length]
}

/** The short words after an epic's title. */
export function epicNote(epic, state) {
  if (state === 'done') return 'done'
  if (state === 'needs') return 'waits on you'
  if (state === 'blocked') return 'blocked'
  if (state === 'running') return 'running'
  return 'waiting'
}

/**
 * The project and saga columns of the `/cos` digest, sized like the band:
 * the longest text first, then narrowed to what the width leaves after the
 * prefix, the bar, and an eight-cell orchestrator line.
 */
export function digestColumns(projects, width) {
  const project = Math.max(10, ...projects.map((p) => Array.from(String(p?.project || '?')).length))
  const saga = Math.max(17, ...projects.map((p) => Array.from(projectSummary(p).saga).length))
  // The orchestrator line is the tail: it takes what is left, and the
  // project and saga columns get the rest first.
  const budget = width - (3 + 1 + 1 + 14 + 3)
  if (project + saga <= budget) return { project, saga }
  const projectWidth = Math.max(10, Math.min(project, budget - 17))
  return { project: projectWidth, saga: Math.max(17, budget - projectWidth) }
}

/** The `/cos` digest: the board in text, as artboard 5 draws it. */
export function digest(board, options = {}) {
  const width = Math.max(48, options.width || 96)
  const only = options.project || null
  const questions = allQuestions(board)
  // One named project shows even when it is idle; the whole board lists only the active ones.
  const projects = only ? allProjects(board).filter((p) => p.project === only) : activeProjects(board)
  const lines = []
  const head = only ? 'cos \u00b7 ' + only : 'cos \u00b7 ' + questions.length + ' need you \u00b7 ' + projectCount(board)
  lines.push('\u256d\u2500 ' + head + ' ' + RULE.repeat(Math.max(0, width - head.length - 5)))
  lines.push(BRANCH)
  if (!only && questions.length) {
    lines.push(BRANCH + ' ' + GLYPH.needs + ' Needs you (' + questions.length + ')')
    questions.forEach((q, i) => {
      const context = [q.project || '', q.attention_msg || (q.gate ? 'gate' : ''), 'since ' + hhmm(q.since, '?')].filter(Boolean)
      lines.push(BRANCH + '   ' + (i + 1) + '  ' + truncate(q.title || '', Math.max(20, width - 28)) + '  ' + context.join(' \u00b7 '))
    })
    lines.push(BRANCH)
  }
  const columns = digestColumns(projects, width)
  for (const project of projects) {
    const s = projectSummary(project, 10)
    const left = BRANCH + '  ' + pad(s.project, columns.project) + ' ' + pad(s.saga, columns.saga) + ' ' + (s.plain ? openTasksLabel(project) : s.bar.text)
    const room = Math.max(0, width - Array.from(left).length)
    let tail = ''
    if (room >= 4) tail = ' \u00b7 ' + truncate(orchestratorLine(project), room - 3)
    else if (room >= 2) tail = ' \u2026'
    lines.push(left + tail)
    for (const epic of activeSaga(project)?.epics || []) {
      const state = epicState(epic)
      lines.push(BRANCH + '   ' + GLYPH[state] + ' ' + epic.title + '  ' + epicNote(epic, state))
    }
    if (!activeSaga(project)) {
      for (const epic of project?.epics || []) {
        const state = epicState(epic)
        lines.push(BRANCH + '   ' + GLYPH[state] + ' ' + epic.title + '  ' + epicNote(epic, state))
      }
    }
    if (!s.plain && s.needs === 0 && s.blocked === 0 && s.running === 0) lines.push(BRANCH + '   ' + GLYPH.waiting + ' Waiting for a goal')
    lines.push(BRANCH)
  }
  if (only && projects.length === 0) lines.push(BRANCH + '  no project ' + truncate(only, 30))
  const foot = only ? '/cos help for the keys' : '/cos <project> for one project \u00b7 /cos help for the keys'
  lines.push('\u2570\u2500 ' + foot + ' ' + RULE.repeat(Math.max(0, width - foot.length - 4)))
  // A transcript can be narrower than the command's terminal once a pane is
  // docked; a line that does not fit loses its tail with an ellipsis, never
  // wraps outside the tree.
  return lines.map((line) => truncate(line, width)).join('\n')
}
