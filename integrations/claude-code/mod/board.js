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

/** Like `fit`, but a row that does not fit loses its tail with an ellipsis. */
export function fitEllipsis(segments, width) {
  const total = segments.reduce((n, s) => n + Array.from(s.text).length, 0)
  if (total <= width) return fit(segments, width)
  const room = Math.max(0, width - 1)
  const out = []
  let used = 0
  for (const s of segments) {
    if (used >= room) break
    const take = Array.from(s.text).slice(0, room - used).join('')
    if (take) {
      out.push({ ...s, text: take })
      used += Array.from(take).length
    }
  }
  out.push(text('\u2026', out.length ? out[out.length - 1].style : 'plain'))
  used += 1
  if (used < width) out.push(text(' '.repeat(width - used)))
  return out
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
function stamp(at) {
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
    const since = (stamp(a.since) ?? 0) - (stamp(b.since) ?? 0)
    if (since !== 0) return since
    return String(a.title || '').localeCompare(String(b.title || ''))
  })
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

/** A project's one-line summary: badge, saga, bar, and counts. */
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

/** The digits that select rows, as the band hints them: `1`, `1-9`, or `` when
 * no row is there. Rows are one per needs-you item, then one per project. */
export function digitRange(rows) {
  const n = Math.max(0, Number(rows) || 0)
  if (n === 0) return ''
  return n === 1 ? '1' : '1-' + Math.min(9, n)
}

/**
 * The help as plain text: every key and command of the board. The pane's
 * help view and `/cos help` draw these lines, so they always agree.
 */
export function helpLines(options = {}) {
  const digits = digitRange(options.rows) || '1-9'
  const row = (key, what) => '  ' + pad(key, 16) + what
  return [
    'keys',
    row(digits, 'select a row and open the preview; it takes the keys'),
    row('', 'digits work only from an empty prompt'),
    row('j k', 'move the selection; the preview follows'),
    row('d', 'Done on the selected needs-you item'),
    row('a', 'Ask its orchestrator'),
    row('enter', 'more: a project’s tasks, or a task’s status, blockers, PR'),
    row('esc', 'back to the prompt; a docked preview stays, an inline one closes'),
    row('click', 'a row selects it; the selected row again shows more'),
    row('ctrl+x tab', 'the band takes the keys (the same keys work there)'),
    row('ctrl+x x', 'close the preview'),
    '',
    'commands',
    row('/cos', 'the board as text'),
    row('/cos <project>', 'one project'),
    row('/cos pane', 'open the preview'),
    row('/cos help', 'this help'),
    '',
    'buttons',
    row('Done', 'tells the chief to close the item; the row shows each step:'),
    row('', 'sending, the chief is closing it, closed, or still open'),
    row('Ask', 'sends a question to the channel’s owner; the chief relays it'),
  ]
}

// The band: a title row, then one row per needs-you item and one per active
// project. Every row is a list of cells: `{ text, style }`, or a cell with a
// `button` the renderer draws as a plain Button, `N: ` for a row's digit.

const cells = (value) => Array.from(String(value)).length
const span = (segments) => segments.reduce((n, s) => n + cells(s.text), 0)

/** The band's rows, in order: each needs-you item, then each active project. */
export function bandItems(board, questions = allQuestions(board)) {
  const out = questions.map((q) => ({ kind: 'needs', key: 'q:' + String(q.channel) + '/' + String(q.task), q }))
  for (const project of activeProjects(board)) out.push({ kind: 'project', key: 'p:' + project.project, project })
  return out
}

/**
 * A row's chip: a plain Button that draws `1: `; past nine, three blank cells.
 * With `digitKeys` the digit is the Button's hotkey, so a bare digit at an
 * empty prompt presses it. Without, the digit is only drawn, and the mod's
 * prompt.edit hook takes the digit instead (see register.js).
 */
export function chip(item, index, digitKeys = true) {
  const digit = index < 9 ? String(index + 1) : ''
  const button = { key: item.key, label: digit ? '' : '   ', kind: 'select', arg: item.key, plain: true }
  if (digit && digitKeys) button.hotkey = digit
  else if (digit) button.digit = digit
  return { text: digit ? digit + ': ' : '   ', style: 'hotkey', button }
}

/** A key the hint names: the letter in the accent color, then its word. */
function keyText(key, word) {
  return [text(key, 'key'), text(word ? ' ' + word : '', 'dim')]
}

/** A Button with a hotkey, drawn plain: `d: Done`. */
function keyButton(key, label, kind, hotkey) {
  return { text: hotkey + ': ' + label, style: 'hotkey', button: { key, label, kind, hotkey, plain: true } }
}

/** The age of a needs-you item when the board was read: `42m`. */
export function waited(q, board) {
  const at = stamp(board?.at)
  const since = stamp(q?.since)
  if (at === null || since === null) return ''
  return ageText(at - since)
}

/** `45s`, `42m`, `1h 4m`, `3d`. */
export function ageText(ms) {
  const s = Math.max(0, Math.floor((Number(ms) || 0) / 1000))
  if (s < 60) return s + 's'
  const m = Math.floor(s / 60)
  if (m < 60) return m + 'm'
  const h = Math.floor(m / 60)
  if (h < 24) return h + 'h' + (m % 60 ? ' ' + (m % 60) + 'm' : '')
  return Math.floor(h / 24) + 'd'
}

/**
 * Where a needs-you item stands, in place of its buttons: the Done or Ask
 * status, and the Done button while one can be pressed. `status` is what
 * pressStatus (mod/status.js) gave, `done` the Done mark; `buttons` draws the
 * buttons at all (only the selected item has them, as hotkeys are per site);
 * `statusCap` the most cells the status takes.
 */
export function itemActions(status, canPress, done, buttons, askLabel = 'Ask', statusCap = Infinity) {
  const out = []
  // A long status ("asked orch-roger-emr-observability-dashboards at 10:25")
  // is cut where room is short; the preview card shows it whole.
  if (status) out.push(...(span(status) > statusCap ? fitEllipsis(status, statusCap) : status))
  if (!buttons) return out
  if (canPress) {
    if (out.length) out.push(text('  '))
    out.push(keyButton('done', done ? 'Done again' : 'Done', 'done', 'd'))
  }
  if (!status) out.push(text('  '), keyButton('ask', askLabel, 'ask', 'a'))
  return out
}

/** The cells a needs-you row keeps for its buttons or its status, on every
 * row, so a press never moves a column: `d: Done  a: Ask` is 15. */
export const ACTION_SLOT = 17
/** The slot of a band that draws no buttons: a short status, `◷ closing 12m`. */
export const STATUS_SLOT = 13

/**
 * A band row's buttons or short status (pressStatusShort), in ACTION_SLOT
 * cells at most. `buttons` draws the buttons (the selected item only, as a
 * hotkey is one per site); a Done that failed offers `d: Done again`.
 */
export function bandActions(status, canPress, done, buttons) {
  if (!buttons || !canPress) return status || []
  if (!status) return [keyButton('done', 'Done', 'done', 'd'), text('  '), keyButton('ask', 'Ask', 'ask', 'a')]
  if (done) return [text('! ', 'red'), keyButton('done', 'Done again', 'done', 'd')]
  const withDone = [...status, text('  '), keyButton('done', 'Done', 'done', 'd')]
  return span(withDone) <= ACTION_SLOT ? withDone : status
}

/** The state of a project's plain tasks, for a project with no epic. */
export function plainState(project) {
  const c = project?.tasks || {}
  if ((c.needs_human || 0) + (c.stuck || 0) > 0) return 'needs'
  if ((c.running || 0) > 0) return 'running'
  if ((c.blocked || 0) > 0 && !(c.ready || 0)) return 'blocked'
  return 'waiting'
}

/** A project row's parts: the epic it is on, its state, bar, ratio, counts. */
export function projectParts(project, barCells = 5) {
  const s = projectSummary(project, barCells)
  const current = currentEpic(project)
  const epic = current ? current.epic.title : s.plain ? openTasksLabel(project) : 'no epic yet'
  const state = current ? current.state : s.plain ? plainState(project) : 'waiting'
  const counts = []
  if (s.blocked) counts.push(text(GLYPH.blocked + s.blocked, 'red'))
  if (s.needs) counts.push(text((counts.length ? ' ' : '') + GLYPH.needs + s.needs, 'yellow'))
  return {
    epic,
    state,
    filled: s.plain ? '' : s.bar.filled,
    empty: s.plain ? ' '.repeat(barCells) : s.bar.empty,
    ratio: s.plain ? '' : s.bar.label,
    counts,
  }
}

/** Cut segments to `width` and pad them to it exactly. */
function cell(segments, width) {
  return fitEllipsis(segments, Math.max(0, width)).filter((s) => s.text !== '')
}

/**
 * The rows of the needs-you items: digit, glyph, title, `project · age`, and
 * the action slot. Every row has the same columns, so the slot and its
 * buttons sit at one place whichever row is selected and whatever a press
 * shows. The title takes the room first, after `project · age` for all
 * but the longest; a name is cut before its age. Leftover space goes after
 * the slot, at the row's end.
 */
function needsRows(items, start, o) {
  if (!items.length) return []
  const metas = items.map(({ q }) => ({ name: q.project || '?', age: waited(q, o.board) }))
  const metaLen = (m) => cells(m.name) + (m.age ? 3 + cells(m.age) : 0)
  const longestMeta = Math.max(...metas.map(metaLen))
  const longestTitle = Math.max(8, ...items.map(({ q }) => cells(q.title || '')))
  // The slot holds buttons only where the band draws them (`reserveActions`);
  // elsewhere a short status, at most STATUS_SLOT cells.
  const slot = o.reserveActions ? ACTION_SLOT : STATUS_SLOT
  const room = o.width - 5 - 2 - 2 - slot
  // `project · age` fits all but the longest one in full, in at most 40% of
  // the room; the longest is cut at its name. More room goes to it first.
  const sorted = metas.map(metaLen).sort((a, b) => b - a)
  const metaMin = Math.min(longestMeta, Math.max(14, Math.min(sorted[1] ?? sorted[0], Math.floor(room * 0.4))))
  const meta = Math.max(metaMin, Math.min(longestMeta, room - longestTitle))
  const title = Math.max(12, Math.min(longestTitle, room - meta))
  return items.map((item, i) => {
    const { q } = item
    const m = metas[i]
    const ageCells = m.age ? 3 + cells(m.age) : 0
    const glyph = q.closed ? text(GLYPH.done + ' ', 'green') : text(GLYPH.needs + ' ', 'yellow')
    const metaCells = [text(truncate(m.name, Math.max(4, meta - ageCells)), 'bold'), ...(m.age ? [text(' · ' + m.age, 'dim')] : [])]
    const row = [
      chip(item, start + i, o.digitKeys !== false),
      glyph,
      ...cell([text(q.title || '', q.closed ? 'dim' : 'yellowBold')], title),
      text('  '),
      ...cell(metaCells, meta),
      text('  '),
      ...fitEllipsis(o.actionsFor(item), slot).filter((c) => c.text !== '' || c.button),
    ]
    return { key: item.key, cells: fit(row, o.width), used: 5 + title + 2 + meta + 2 + slot }
  })
}

/** The orchestrator column of a wide project row: `orch-borant · hetzner-1 · busy`. */
function orchText(project) {
  const orch = (project?.orchestrators || [])[0]
  if (!orch) return 'no orchestrator'
  const state = orch.online === false ? 'offline' : orch.busy ? 'busy' : 'online'
  return [orch.agent, orch.host, state].filter(Boolean).join(' · ')
}

/** The task counts of a wide project row, each in its own column: running, blocked, needs you. */
function countCells(project) {
  const c = project?.tasks || {}
  const one = (n, glyph, style) => (n ? text(pad(glyph + n, 4), style) : text('    '))
  return [
    one(c.running || 0, GLYPH.running, 'blue'),
    one(c.blocked || 0, GLYPH.blocked, 'red'),
    one((c.needs_human || 0) + (c.stuck || 0), GLYPH.needs, 'yellow'),
  ]
}

/**
 * The rows of the active projects: digit, name, the current epic with its
 * glyph, a five-cell bar, done/total, and the counts that need a look. The
 * name column takes the longest name; the epic gives way first. From 150
 * columns a row also has running, blocked, and needs-you counts in columns,
 * and the orchestrator with its host.
 */
function projectRows(items, start, o) {
  if (!items.length) return []
  const wide = o.width >= 150
  const parts = items.map(({ project }) => projectParts(project))
  const longestName = Math.max(4, ...items.map(({ project }) => cells(project.project)))
  const longestEpic = Math.max(4, ...parts.map((p) => cells(p.epic)))
  const ratio = Math.max(0, ...parts.map((p) => cells(p.ratio)))
  const counts = wide ? 12 : Math.max(0, ...parts.map((p) => span(p.counts)))
  const orch = wide ? Math.max(...items.map(({ project }) => cells(orchText(project)))) : 0
  const tail = 2 + 5 + 1 + ratio + (counts ? 2 + counts : 0) + (orch ? 2 + orch : 0)
  let name = longestName
  let epic = Math.min(longestEpic, o.width - 3 - name - 1 - 2 - tail)
  if (epic < 12) {
    // The epic gives way first, down to twelve cells; then the names are cut.
    epic = Math.min(longestEpic, 12)
    name = Math.max(6, Math.min(longestName, o.width - 3 - 1 - 2 - epic - tail))
  }
  return items.map((item, i) => {
    const p = parts[i]
    const row = [
      chip(item, start + i, o.digitKeys !== false),
      ...cell([text(item.project.project, 'bold')], name),
      text(' '),
      text(GLYPH[p.state] + ' ', p.state),
      ...cell([text(p.epic)], epic),
      text('  '),
      text(p.filled, 'green'),
      text(p.empty, 'faint'),
      text(' '),
      ...cell([text(p.ratio)], ratio),
    ]
    if (wide) row.push(text('  '), ...countCells(item.project), text('  '), text(orchText(item.project), 'dim'))
    else if (counts) row.push(text('  '), ...cell(p.counts, counts))
    return { key: item.key, cells: fit(row.filter((s) => s.text !== ''), o.width), used: 3 + name + 3 + epic + tail }
  })
}

/**
 * The hint in the title row. At rest it names the digits; while the board has
 * the keys (the preview pane, or the band after a click or ctrl+x tab) it
 * names every key. j and k are the band's own Buttons, so they work there too.
 */
export function bandHint(o, form) {
  if (!o.active) return o.rows ? [text(digitRange(o.rows) + ' select', 'dim')] : []
  const j = keyButton('band-down', '↓', 'band-down', 'j')
  const k = keyButton('band-up', '↑', 'band-up', 'k')
  const sep = text(' · ', 'dim')
  if (form === 'keys') return [j, text(' '), k]
  const out = [j, text(' '), k, text(' move', 'dim')]
  if (form === 'full' && o.needs) out.push(sep, ...keyText('d', 'done'), sep, ...keyText('a', 'ask'))
  out.push(sep, ...keyText('⏎', 'more'), sep, ...keyText('esc', 'prompt'))
  return out
}

/**
 * The title row: `── chief of staff ─ 2 need you · 3 active · 1 idle ──── <hint>
 * ─ <freshness> ─ [ ? ] ─`. Forms are tried from the fullest; the first that
 * leaves the rule four cells is drawn.
 */
export function titleRow(board, o) {
  const full = o.width
  const questions = (o.questions || []).filter((q) => !q.closed)
  const all = allProjects(board).length
  const active = activeProjects(board).length
  const summary = questions.length
    ? [text(questions.length + ' need you', 'yellowBold')]
    : [text(GLYPH.done + ' nothing needs you', 'green')]
  const counts = (idle) =>
    idle === null ? text('') : text(' · ' + active + ' active' + (idle && all - active ? ' · ' + (all - active) + ' idle' : ''), 'dim')
  const help = { text: '[ ? ]', style: 'button', button: { key: 'help', label: '?', kind: 'help' } }
  const label = full >= 110 ? 'chief of staff' : 'cos'
  const fresh = o.fresh || []
  let width = full
  const build = (idle, hint, freshParts) => {
    const left = [text(RULE + RULE + ' ', 'faint'), text(label, 'bold'), text(' ' + RULE + ' ', 'faint'), ...summary, counts(idle)]
    const right = [...(hint.length ? [text(' '), ...hint, text(' ' + RULE, 'faint')] : []), text(' '), ...freshParts, text(' ' + RULE + ' ', 'faint'), help, text(' ' + RULE, 'faint')]
    const dashes = width - span(left) - 1 - span(right)
    return { dashes, row: [...left, text(' ' + RULE.repeat(Math.max(0, dashes)), 'faint'), ...right] }
  }
  // With `fitTo` (where the rows end) the row is as wide as its fullest form
  // or the rows, whichever is wider, up to the full width.
  if (o.fitTo) width = Math.min(full, Math.max(o.fitTo, width - build(true, bandHint(o, 'full'), fresh).dashes + 4))
  const forms = [
    [true, bandHint(o, 'full'), fresh],
    [true, bandHint(o, 'short'), fresh],
    [false, bandHint(o, 'short'), fresh],
    [false, bandHint(o, 'keys'), fresh],
    [false, bandHint(o, 'keys'), fresh.slice(0, 1)],
    [false, [], fresh.slice(0, 1)],
    [null, [], fresh.slice(0, 1)],
  ]
  for (const [idle, hint, freshParts] of forms) {
    const built = build(idle, hint, freshParts)
    if (built.dashes >= 4) return fit(built.row, width)
  }
  const last = forms[forms.length - 1]
  return fitEllipsis(build(last[0], last[1], last[2]).row, width)
}

/** The band as one row, for a band with room for one row. */
export function condensedRow(board, o) {
  const width = Math.max(20, o.width || 100)
  const questions = (o.questions || allQuestions(board)).filter((q) => !q.closed)
  const parts = [text('cos  ', 'dimBold')]
  parts.push(questions.length ? text(GLYPH.needs + ' ' + questions.length + ' need you', 'yellowBold') : text(GLYPH.done + ' nothing needs you', 'green'))
  for (const project of activeProjects(board)) {
    const p = projectParts(project)
    parts.push(text('  ' + BRANCH + '  ', 'faint'), text(project.project, 'bold'), text(' ' + (p.ratio || openTasksLabel(project))))
    if (p.counts.length) parts.push(text(' '), ...p.counts)
  }
  const fresh = o.fresh || []
  const room = width - span(fresh) - 2
  return fit([...fitEllipsis(parts, room), text('  '), ...fresh], width)
}

/**
 * The band: the title row, then one row per needs-you item and per active
 * project. `o` is `{ width, maxRows, questions, selected, active, fill,
 * actionsFor, reserveActions, fresh, digitKeys }`; `actionsFor(item)` gives the cells
 * after an item's columns (mod/status.js decides them). Each row is
 * `{ key, cells, selected }`.
 */
export function bandRows(board, o = {}) {
  const width = Math.max(20, o.width || 100)
  const maxRows = o.maxRows === undefined || o.maxRows === null ? Infinity : o.maxRows
  const questions = o.questions || allQuestions(board)
  const items = bandItems(board, questions)
  if (maxRows <= 1) return [{ key: 'band-condensed', cells: condensedRow(board, { ...o, width, questions }) }]
  const selected = o.selected || null
  const needs = items.filter((i) => i.kind === 'needs')
  const projects = items.filter((i) => i.kind === 'project')
  const selectedItem = items.find((i) => i.key === selected)
  const opts = {
    ...o,
    width,
    board,
    actionsFor: o.actionsFor || (() => []),
    needs: Boolean(selectedItem && selectedItem.kind === 'needs' && !selectedItem.q.closed),
    rows: items.length,
  }
  const body = [...needsRows(needs, 0, opts), ...projectRows(projects, needs.length, opts)]
  // The title row ends where the rows do, so its hint and freshness sit by
  // the content, not across an empty stretch at the terminal's far edge.
  const used = Math.max(0, ...body.map((r) => r.used || 0))
  const rows = [{ key: 'band-title', cells: titleRow(board, { ...opts, fitTo: Math.max(used, 100), questions }) }, ...body]
  for (const row of rows) row.selected = Boolean(o.fill && row.key === selected)
  // At rest the window starts at the top, so the digits a person reads from
  // the prompt are the ones drawn, and armed; with the keys on the board it
  // follows the selection.
  return windowed(rows, maxRows, o.fill ? selected : null, width, o.active)
}

/**
 * The band in at most `maxRows` rows, the title always first: a band taller
 * than its room would let the engine scroll the title, and the data's age
 * with it, out of view. The window keeps the selected row inside, and a last
 * dim row says how many rows are above and below it.
 */
export function windowed(rows, maxRows, selected, width, active = false) {
  if (rows.length <= maxRows) return rows
  const [title, ...body] = rows
  const room = Math.max(1, maxRows - 2)
  const at = selected ? Math.max(0, body.findIndex((r) => r.key === selected)) : 0
  const start = Math.max(0, Math.min(at - Math.floor(room / 2), body.length - room))
  const shown = body.slice(start, start + room)
  if (maxRows < 3) return [title, ...shown]
  const above = start
  const below = body.length - start - shown.length
  const note = [above ? '\u2191 ' + above + ' more' : '', below ? '\u2193 ' + below + ' more' : ''].filter(Boolean).join(' \u00b7 ')
  // The keys that reach the hidden rows: j and k while the board has the
  // keys; at rest a digit selects a drawn row and j or k goes on from there.
  const how = active ? 'j k' : digitRange(shown.length + above) + ' then j k'
  return [title, ...shown, { key: 'band-more', cells: fit([text('   ' + note + ' \u00b7 ' + how, 'dim')], width) }]
}

/** The short words after an epic's title in the pane. */
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
  const foot = only ? '/cos pane for the pane' : '/cos <project> for one project \u00b7 /cos pane for the pane'
  lines.push('\u2570\u2500 ' + foot + ' ' + RULE.repeat(Math.max(0, width - foot.length - 4)))
  // A transcript can be narrower than the command's terminal once a pane is
  // docked; a line that does not fit loses its tail with an ellipsis, never
  // wraps outside the tree.
  return lines.map((line) => truncate(line, width)).join('\n')
}
