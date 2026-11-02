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

/** The glyph of a needs-you item that left the board without a close. */
export const GONE = '\u2013'

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

/** Code points that take no cell: combining marks, a zero-width joiner,
 * variation selectors. */
function zeroWidth(cp) {
  return cp === 0x200d || (cp >= 0x300 && cp <= 0x36f) || (cp >= 0x1ab0 && cp <= 0x1aff) || (cp >= 0x20d0 && cp <= 0x20ff) || (cp >= 0xfe00 && cp <= 0xfe0f) || (cp >= 0xe0100 && cp <= 0xe01ef)
}

/** Ranges a terminal draws two cells wide: CJK, Hangul, full-width forms, and emoji. */
const WIDE = [
  [0x1100, 0x115f], [0x231a, 0x231b], [0x23e9, 0x23ec], [0x23f0, 0x23f0], [0x23f3, 0x23f3], [0x25fd, 0x25fe],
  [0x2614, 0x2615], [0x26a1, 0x26a1], [0x26aa, 0x26ab], [0x26bd, 0x26be], [0x26c4, 0x26c5], [0x26d4, 0x26d4],
  [0x26ea, 0x26ea], [0x26f2, 0x26f5], [0x26fa, 0x26fd], [0x2705, 0x2705], [0x270a, 0x270b], [0x2728, 0x2728],
  [0x274c, 0x274c], [0x274e, 0x274e], [0x2753, 0x2757], [0x2795, 0x2797], [0x27b0, 0x27b0], [0x27bf, 0x27bf],
  [0x2b1b, 0x2b1c], [0x2b50, 0x2b50], [0x2b55, 0x2b55], [0x2e80, 0x303e], [0x3041, 0x33ff], [0x3400, 0x4dbf],
  [0x4e00, 0x9fff], [0xa000, 0xa4cf], [0xac00, 0xd7a3], [0xf900, 0xfaff], [0xfe30, 0xfe4f], [0xff00, 0xff60],
  [0xffe0, 0xffe6], [0x1f300, 0x1f64f], [0x1f680, 0x1f6ff], [0x1f900, 0x1f9ff], [0x1fa70, 0x1faff], [0x20000, 0x3fffd],
]

/** The cells one character takes in a terminal: 0, 1, or 2. */
function charCells(ch) {
  const cp = ch.codePointAt(0)
  if (zeroWidth(cp)) return 0
  return WIDE.some(([a, b]) => cp >= a && cp <= b) ? 2 : 1
}

/** The cells a text takes in a terminal. */
export function cellsOf(value) {
  let n = 0
  for (const ch of String(value)) n += charCells(ch)
  return n
}

/** The head of a text that fits in `room` cells, and the cells it takes. */
function head(value, room) {
  let out = ''
  let used = 0
  for (const ch of String(value)) {
    const w = charCells(ch)
    if (used + w > room) break
    out += ch
    used += w
  }
  return { text: out, used }
}

/** Cut text to `width` cells, with an ellipsis. */
export function truncate(value, width) {
  if (width <= 0) return ''
  if (cellsOf(value) <= width) return String(value)
  if (width === 1) return '…'
  return head(value, width - 1).text + '…'
}

/** Pad text to exactly `width` cells, cutting it when it is longer. */
export function pad(value, width) {
  const cut = truncate(value, width)
  return cut + ' '.repeat(Math.max(0, width - cellsOf(cut)))
}

/** Like `fit`, but a row that does not fit loses its tail with an ellipsis. */
export function fitEllipsis(segments, width) {
  const total = segments.reduce((n, s) => n + cellsOf(s.text), 0)
  if (total <= width) return fit(segments, width)
  const room = Math.max(0, width - 1)
  const out = []
  let used = 0
  for (const s of segments) {
    if (used >= room) break
    const take = head(s.text, room - used)
    if (take.text) {
      out.push({ ...s, text: take.text })
      used += take.used
    }
  }
  out.push(text('…', out.length ? out[out.length - 1].style : 'plain'))
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
    const take = head(s.text, width - used)
    if (take.text) out.push({ ...s, text: take.text })
    used += take.used
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
    'board mode: every key goes to the board until esc',
    row('0', 'enter board mode, from an empty prompt'),
    row(digits, 'select a row; from the prompt it enters too'),
    row('j k', 'move the selection; the preview follows'),
    row('enter', 'details; enter again goes back'),
    row('d', 'Done on the selected needs-you item'),
    row('a', 'Ask its orchestrator'),
    row('?', 'this help; ? or enter goes back'),
    row('esc', 'leave: the keys go back to the prompt'),
    row('vim normal', 'press i, then 0'),
    row('click', 'a row selects it'),
    '',
    'commands',
    row('/cos', 'the board as text'),
    row('/cos <project>', 'one project'),
    row('/cos pane', 'open the preview'),
    row('/cos help', 'this help'),
    '',
    'done and ask',
    row('d', 'the chief closes the item; the row shows'),
    row('', 'sending, closing, closed, or still open'),
    row('a', 'asks the owner; the chief relays the reply'),
  ]
}

// The band: a title row, then one row per needs-you item and one per active
// project. Every row sits on one grid of columns that depends on the band's
// width alone, never on the data, so a live update never moves a column:
//
//   chip  glyph  main ......................  meta ....................  tail ..........
//   1:    ▲      the needs-you title          project · age              a press status
//   7:    ●      project name  its epic       ██░░░ 2/5   ●2  ■1  ▲1     orchestrator
//
// Every text is cut to its column with an ellipsis. A row is a list of cells:
// `{ text, style }`, or a cell with a `button` the renderer draws as a plain
// Button, `N: ` for a row's digit.

const cells = cellsOf
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
  const title = item.kind === 'needs' ? item.q.title : item.project.project
  const button = { key: item.key, label: digit ? '' : '   ', title: String(title || item.key), kind: 'select', arg: item.key, plain: true }
  if (digit && digitKeys) button.hotkey = digit
  else if (digit) button.digit = digit
  return { text: digit ? digit + ': ' : '   ', style: 'hotkey', button }
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

/** The short status a needs row shows in its tail: `◷ closing`, `✓ asked`. */
export const STATUS_SLOT = 13
/** The meta column: `██░░░ 2/5   ●2  ■1  ▲1 `, or `borant · 42m`. A band
 * under 100 cells has the narrow one, with the most urgent count alone. */
export const META = 24
const NARROW_META = 16
/** The widest main column. Past it the rows end early, so the meta never sits
 * far from a short title on a wide terminal. */
export const MAIN_MAX = 100
const GAP = 2

/**
 * The band's columns at one width: `{ main, meta, tail, row }`, in cells.
 * The chip and the glyph take five cells before `main`. A column with no
 * room is 0 and left out: the tail first, then the meta. `row` is where every
 * row ends.
 */
export function bandGrid(width) {
  const meta = width >= 100 ? META : width >= 56 ? NARROW_META : 0
  const tail = width >= 140 ? 32 : width >= 90 ? STATUS_SLOT + 4 : 0
  const main = Math.max(8, Math.min(MAIN_MAX, width - 5 - (meta ? GAP + meta : 0) - (tail ? GAP + tail : 0)))
  return { main, meta, tail, row: 5 + main + (meta ? GAP + meta : 0) + (tail ? GAP + tail : 0) }
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
 * A needs-you row: digit, glyph, title, `project · age`, and the status of a
 * Done or an Ask. Where the band has no tail, the status takes the meta's
 * place while there is one.
 */
function needsRow(item, index, g, o) {
  const { q } = item
  const status = o.statusFor(item) || []
  const age = waited(q, o.board)
  const ageCells = age ? 3 + cells(age) : 0
  const where = [text(truncate(q.project || '?', Math.max(4, g.meta - ageCells)), 'bold'), ...(age ? [text(' · ' + age, 'dim')] : [])]
  const row = [
    chip(item, index, o.digitKeys !== false),
    q.closed ? text(GLYPH.done + ' ', 'green') : q.gone ? text(GONE + ' ', 'dim') : text(GLYPH.needs + ' ', 'yellow'),
    ...cell([text(q.title || '', q.closed || q.gone ? 'dim' : 'yellowBold')], g.main),
  ]
  if (g.meta) row.push(text('  '), ...cell(!g.tail && status.length ? status : where, g.meta))
  if (g.tail) row.push(text('  '), ...cell(status, g.tail))
  return { key: item.key, cells: fit(row, o.width) }
}

/** The orchestrator column of a project row: `orch-borant · busy · hetzner-1`.
 * The state comes before the host, so a long host is what gets cut. */
function orchText(project) {
  const orch = (project?.orchestrators || [])[0]
  if (!orch) return 'no orchestrator'
  const state = orch.online === false ? 'offline' : orch.busy ? 'busy' : 'online'
  return [orch.agent, state, orch.host].filter(Boolean).join(' · ')
}

/**
 * A project's task counts, each in its own four cells: running, blocked,
 * needs you. With `narrow`, the most urgent one alone: needs you, then
 * blocked, then running.
 */
function countCells(project, narrow = false) {
  const c = project?.tasks || {}
  const one = (n, glyph, style) => (n ? text(pad(glyph + n, 4), style) : text('    '))
  const needs = (c.needs_human || 0) + (c.stuck || 0)
  if (narrow) {
    if (needs) return [one(needs, GLYPH.needs, 'yellow')]
    if (c.blocked) return [one(c.blocked, GLYPH.blocked, 'red')]
    return [one(c.running || 0, GLYPH.running, 'blue')]
  }
  return [one(c.running || 0, GLYPH.running, 'blue'), one(c.blocked || 0, GLYPH.blocked, 'red'), one(needs, GLYPH.needs, 'yellow')]
}

/**
 * A project row: digit, the state of its current epic, its name and that
 * epic in the main column, then a five-cell bar, done/total, and the counts
 * in the meta, and its orchestrator in the tail.
 */
function projectRow(item, index, g, o) {
  const p = projectParts(item.project)
  const name = Math.min(34, Math.max(6, Math.floor(g.main * 0.35)))
  const row = [
    chip(item, index, o.digitKeys !== false),
    text(GLYPH[p.state] + ' ', p.state),
    ...cell([text(item.project.project, 'bold')], name),
    text(' '),
    ...cell([text(p.epic)], g.main - name - 1),
  ]
  if (g.meta) row.push(text('  '), ...cell([text(p.filled, 'green'), text(p.empty, 'faint'), text(' ' + pad(p.ratio, 5) + ' '), ...countCells(item.project, g.meta < META)], g.meta))
  if (g.tail) row.push(text('  '), ...cell([text(orchText(item.project), 'dim')], g.tail))
  return { key: item.key, cells: fit(row, o.width) }
}

/**
 * The mode at the title row's end. At rest it is the Button that enters
 * board mode, `0: board mode`, pressed by a bare 0 at the empty prompt; in
 * board mode it says how to leave. Both end at the row's end, and the rule
 * before them takes up the difference, so nothing else moves.
 */
export function modeCells(active) {
  if (active) return [text('board mode', 'modeOn'), text(' · ', 'dim'), text('esc', 'key'), text(' exit', 'dim')]
  return [
    { text: '0: board mode', style: 'hotkey', button: { key: 'board', label: 'board mode', kind: 'board', hotkey: '0', plain: true } },
    // In vim normal mode a bare 0 is vim's, and no mod sees it.
    text(' (vim: i 0)', 'dim'),
  ]
}

/**
 * The title row: `── chief of staff ─ 6 need you · 9 active · 3 idle ───── 0:
 * board mode ─`, as wide as the rows. The mode sits at the end, right
 * aligned, so a count that grows never moves it. A stale board says so just
 * before it. Narrower, the idle count goes first, then the active count.
 */
export function titleRow(board, o) {
  const width = Math.min(o.width, Math.max(o.rowWidth || 0, 48))
  const questions = (o.questions || []).filter((q) => !q.closed && !q.gone)
  const all = allProjects(board).length
  const active = activeProjects(board).length
  const summary = questions.length
    ? [text(questions.length + ' need you', 'yellowBold')]
    : [text(GLYPH.done + ' nothing needs you', 'green')]
  // Board mode holds its rows still: what arrived meanwhile shows after esc.
  // Like the counts, the note goes when the row has no room for it.
  const waiting = o.waiting ? [text(' · +' + o.waiting + ' after esc', 'dim')] : []
  const label = width >= 110 ? 'chief of staff' : 'cos'
  const mode = modeCells(o.active)
  const stale = o.stale ? [o.stale, text(' ' + RULE + ' ', 'faint')] : []
  const build = (counts) => {
    const left = [text(RULE + RULE + ' ', 'faint'), text(label, 'bold'), text(' ' + RULE + ' ', 'faint'), ...summary, ...counts]
    const right = [text(' '), ...stale, ...mode, text(' ' + RULE, 'faint')]
    const dashes = width - span(left) - 1 - span(right)
    return { dashes, row: [...left, text(' ' + RULE.repeat(Math.max(0, dashes)), 'faint'), ...right] }
  }
  const idle = all - active
  const forms = [
    [...waiting, text(' · ' + active + ' active' + (idle ? ' · ' + idle + ' idle' : ''), 'dim')],
    [...waiting, text(' · ' + active + ' active', 'dim')],
    waiting,
    [],
  ]
  for (const counts of forms) {
    const built = build(counts)
    if (built.dashes >= 3) return fit(built.row, o.width)
  }
  // Narrower still: the mode first, so its key is never cut, then the summary.
  return fit([text(RULE + ' ', 'faint'), ...mode, text(' ' + RULE + ' ', 'faint'), ...summary], o.width)
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
  // The mode comes first in the room: its key, or how to leave, is never cut.
  const mode = modeCells(o.active)
  const room = Math.max(0, width - span(mode) - 2)
  return fit([...fitEllipsis(parts, room), text('  '), ...mode], width)
}

/**
 * The band: the title row, then one row per needs-you item and per active
 * project. `o` is `{ width, gridWidth, maxRows, questions, items, waiting,
 * selected, active, statusFor, stale, digitKeys }`: `statusFor(item)` gives a
 * needs row's short press status (mod/status.js), `active` says the board has
 * the keys (board mode), which fills the selected row. `items` replaces the
 * rows bandItems gives (board mode holds them still), `waiting` counts the
 * rows held back, and `gridWidth` lays the columns out narrower than the band
 * (as wide as it is with the preview docked). Each row is `{ key, cells,
 * selected }`.
 */
export function bandRows(board, o = {}) {
  const width = Math.max(20, o.width || 100)
  const maxRows = o.maxRows === undefined || o.maxRows === null ? Infinity : o.maxRows
  const questions = o.questions || allQuestions(board)
  const items = o.items || bandItems(board, questions)
  if (maxRows <= 1) return [{ key: 'band-condensed', cells: condensedRow(board, { ...o, width, questions }) }]
  const selected = o.selected || null
  const g = bandGrid(Math.min(width, o.gridWidth || width))
  const opts = { ...o, width, board, statusFor: o.statusFor || (() => []) }
  const needs = items.filter((i) => i.kind === 'needs')
  const projects = items.filter((i) => i.kind === 'project')
  const body = [...needs.map((item, i) => needsRow(item, i, g, opts)), ...projects.map((item, i) => projectRow(item, needs.length + i, g, opts))]
  const rows = [{ key: 'band-title', cells: titleRow(board, { ...opts, rowWidth: g.row, questions }) }, ...body]
  for (const row of rows) row.selected = Boolean(o.active && row.key === selected)
  // At rest the window starts at the top, so the digits a person reads from
  // the prompt are the ones drawn, and armed; in board mode it follows the
  // selection.
  return windowed(rows, maxRows, o.active ? selected : null, width, o.active)
}

/**
 * The band in at most `maxRows` rows, the title always first: a band taller
 * than its room would let the engine scroll the title out of view. The
 * window keeps the selected row inside, and a last dim row says how many
 * rows are above and below it.
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
  const note = [above ? '↑ ' + above + ' more' : '', below ? '↓ ' + below + ' more' : ''].filter(Boolean).join(' · ')
  // The keys that reach the hidden rows: j and k in board mode; at rest, 0
  // enters board mode and j or k goes on from there.
  const how = active ? 'j k' : '0 then j k'
  return [title, ...shown, { key: 'band-more', cells: fit([text('   ' + note + ' · ' + how, 'dim')], width) }]
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
  const project = Math.max(10, ...projects.map((p) => cellsOf(String(p?.project || '?'))))
  const saga = Math.max(17, ...projects.map((p) => cellsOf(projectSummary(p).saga)))
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
    const room = Math.max(0, width - cellsOf(left))
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
