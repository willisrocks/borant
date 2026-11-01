// The chief-of-staff board: a pure mapping from a BoardView (the hub's
// `board` read) to the band's ticker, the digest, and the help. No mods API
// here, so the tests call it with a fixture and no session.
//
// A row is a list of segments: { text, style, button? }. `text` is what the
// row shows, `style` picks a color, and `button` marks a cell the renderer
// turns into a Button. The pane's rows are built in nav.js.
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

/** A project's one-line summary: saga, bar, and counts. */
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

/** The digits that open the pane, as the help names them: `1`, `1-4`. The
 * ticker numbers what needs you 1, then one number per active project. */
export function digitRange(projects) {
  const n = 1 + Math.max(0, Number(projects) || 0)
  return n === 1 ? '1' : '1-' + Math.min(9, n)
}

/**
 * The help as plain text: every key and command of the board. The pane's
 * help view and `/cos help` draw these lines, so they always agree.
 */
export function helpLines(options = {}) {
  const digits = digitRange(options.projects)
  const row = (key, what) => '  ' + pad(key, 17) + what
  return [
    'keys at an empty prompt',
    row(digits, 'open the pane with the keyboard, on that ticker entry:'),
    row('', '1 what needs you, then one number per project'),
    row('', 'below 144 columns, the digits the ticker shows'),
    row('/cos pane', 'open the pane with the keyboard, at the top'),
    '',
    'keys in the pane',
    row('j k', 'move the selection, through needs you and the projects'),
    row('d', 'Done on the selected item'),
    row('a', 'Ask its orchestrator about the selected item'),
    row('enter', 'open the selected task or epic; fold or unfold a project'),
    row('h l', 'fold or unfold a project; l also opens'),
    row('esc', 'back one level, and at the top close the pane'),
    row('↑ ↓', 'scroll the pane (Claude Code)'),
    row('ctrl+x ← →', 'widen or narrow the docked pane; or drag its edge'),
    row('click', 'a row’s glyph selects it; a second click opens it'),
    '',
    'commands',
    row('/cos', 'the board as text'),
    row('/cos <project>', 'one project'),
    row('/cos pane', 'open the pane'),
    row('/cos help', 'this help'),
    '',
    'buttons',
    row('[ ? ]', "the ticker's help button: this view"),
    row('Done', 'submits "Chris marked done: <item>" to the chief, who closes it;'),
    row('', 'the item says where it is until the hub closes it'),
    row('Ask orchestrator', "sends a question about <item> to its channel's owner;"),
    row('', 'the chief is only told, and relays the reply'),
    '',
    'the ticker’s last word: how fresh the board is',
    row('live', 'the event stream runs; the age shows from 30 s'),
    row('↻ reading', 'a read runs now'),
    row('polling · 4s', 'no event stream; the board is read every 5 s'),
    row('stale · 2m', 'no read has worked for 90 s'),
  ]
}

/** Split text into lines of at most `width` cells, at spaces where it can. */
export function wrap(value, width) {
  const w = Math.max(1, width)
  const out = []
  let line = ''
  for (const word of String(value || '').split(/\s+/).filter(Boolean)) {
    let rest = word
    // A word longer than a line is split where the line ends.
    while (Array.from(rest).length > w) {
      if (line) {
        out.push(line)
        line = ''
      }
      out.push(Array.from(rest).slice(0, w).join(''))
      rest = Array.from(rest).slice(w).join('')
    }
    if (!rest) continue
    const next = line ? line + ' ' + rest : rest
    if (Array.from(next).length <= w) line = next
    else {
      out.push(line)
      line = rest
    }
  }
  if (line || out.length === 0) out.push(line)
  return out
}

function cells(segments) {
  return segments.reduce((n, s) => n + Array.from(s.text).length, 0)
}

/** The glyph counts of a project: `■2 ▲1`, and `●3` for a plain one. */
export function glyphCounts(project) {
  const s = projectSummary(project)
  return [
    s.plain && openTasks(project) ? text(GLYPH.running + openTasks(project), 'blue') : null,
    s.blocked ? text(GLYPH.blocked + s.blocked, 'red') : null,
    s.needs ? text(GLYPH.needs + s.needs, 'yellow') : null,
  ].filter(Boolean)
}

/**
 * The band: one line, a ticker of what needs you and each active project,
 * each with the digit that opens the pane on it, then the freshness word and
 * the help button. Projects that do not fit go as `+N more`. `fresh` is the
 * freshness segment; the result is segments, the help one with a `button`.
 */
export function ticker(board, options = {}) {
  const width = Math.max(20, options.width || 100)
  const fresh = options.fresh || text('live', 'green')
  const help = { text: '[ ? ]', style: 'button', button: { key: 'help', label: '?', kind: 'help' } }
  const label = width >= 110 ? 'chief of staff' : 'cos'
  const sep = () => text(' │ ', 'faint')
  const key = (digit) => text(digit + ': ', 'key')
  const head = [text(label, 'bold'), text('  ')]
  const tail = [sep(), fresh, text('  '), help]
  if (!board) {
    // The reason takes what the label, the word, ` \u00b7 `, and `  [ ? ]` leave.
    const room = width - cells(head) - cells([fresh, help]) - 5
    const why = options.reason && room > 4 ? text(' \u00b7 ' + truncate(options.reason, room), 'dim') : null
    return fit([...head, fresh, ...(why ? [why] : []), text('  '), help], width)
  }
  const n = allQuestions(board).length
  // Each entry's segments name the digit they open, so the band can draw an
  // entry as one Button where it must (see the band in hooks/register.js).
  const entry = (digit, segments) => segments.map((s) => ({ ...s, entry: digit }))
  const entries = [
    entry(
      1,
      n
        ? [key(1), text(GLYPH.needs + ' ' + n + (n === 1 ? ' needs you' : ' need you'), 'yellowBold')]
        : [key(1), text(GLYPH.done + ' nothing needs you', 'green')],
    ),
  ]
  const projects = activeProjects(board)
  projects.forEach((project, i) => {
    const s = projectSummary(project)
    const digit = i + 2
    const segments = [
      ...(digit <= 9 ? [key(digit)] : []),
      text(project.project, 'bold'),
      ...(s.plain ? [] : [text(' ' + s.bar.label)]),
      ...glyphCounts(project).flatMap((c) => [text(' '), c]),
    ]
    entries.push(digit <= 9 ? entry(digit, segments) : segments)
  })
  if (projects.length === 0) entries.push([text(projectCount(board), 'dim')])
  const out = [...head, ...entries[0]]
  let used = cells(out)
  const room = width - cells(tail)
  // `+N more` carries the digit of the first project it hides, so that
  // digit is named on screen and opens the pane there.
  const more = (i) => {
    const rest = entries.length - i
    const digit = i + 1
    return digit <= 9 ? entry(digit, [key(digit), text('+' + rest + ' more', 'dim')]) : [text('+' + rest + ' more', 'dim')]
  }
  for (let i = 1; i < entries.length; i += 1) {
    const after = i + 1 < entries.length ? cells([sep(), ...more(i + 1)]) : 0
    const need = cells([sep(), ...entries[i]])
    if (used + need + after > room) {
      out.push(sep(), ...more(i))
      used += cells([sep(), ...more(i)])
      break
    }
    out.push(sep(), ...entries[i])
    used += need
  }
  return fit([...out, ...tail], width)
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
