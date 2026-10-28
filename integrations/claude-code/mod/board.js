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

/**
 * A hotkey cell, drawn by the renderer as a Button. Rows past nine have no
 * hotkey, so the cell is blank and the row stays clickable; `key` keeps the
 * Buttons of a band unique when two rows show the same digit.
 */
export function hotkey(digit, kind, arg, key) {
  const shown = digit === null || digit === undefined ? '' : String(digit)
  // The engine draws a hotkey chip as `[ 9 ]`; a row with no hotkey keeps the
  // chip's five cells blank, so the columns of the other rows stay put.
  const button = { key: key || kind + '-' + (shown || 'row'), label: shown || '     ', kind, arg }
  if (shown) button.hotkey = shown
  return {
    text: shown ? '[ ' + shown + ' ]' : ' '.repeat(5),
    style: 'hotkey',
    button,
  }
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

/**
 * Lay out cells across `width` cells. A cell is
 * `{ text, style, width }` with a fixed width, or `{ text, style, flex: 1 }`
 * to share what the fixed cells leave.
 */
export function layout(cells, width, gap = 0) {
  const cellWidth = (c) => (c.flex ? 0 : c.width || Array.from(String(c.text === undefined ? '' : c.text)).length)
  const fixed = cells.reduce((n, c) => n + cellWidth(c) + gap, -gap)
  const flexCount = cells.filter((c) => c.flex).length
  const left = Math.max(0, width - fixed)
  const flexWidth = flexCount ? Math.max(1, Math.floor(left / flexCount)) : 0
  const out = []
  cells.forEach((c, i) => {
    if (i > 0 && gap > 0) out.push(text(' '.repeat(gap)))
    const w = c.flex ? flexWidth : cellWidth(c)
    if (c.button) {
      out.push({ text: pad(c.text || '', w), style: c.style, button: c.button })
    } else {
      out.push({ text: pad(c.text === undefined ? '' : c.text, w), style: c.style })
    }
  })
  return out
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

/** A project's badge, colored per project, as the mockup draws it. */
export function badge(project) {
  const name = typeof project === 'string' ? project : project?.project
  return ' ' + (name || '?') + ' '
}

export function badgeStyle(project, index) {
  return index % 2 === 0 ? 'badgeBlue' : 'badgeMagenta'
}

/** The digits that open rows, as the band hints them: `1`, `1-3`, or `` when
 * no row is there. Rows are the needs-you row, then one per project. */
export function digitRange(rows) {
  const n = Math.max(0, Number(rows) || 0)
  if (n === 0) return ''
  return n === 1 ? '1' : '1-' + Math.min(9, n)
}

/**
 * A key of the band's title row, drawn as a plain Button (`j: \u2193`). Its hotkey
 * presses it while the band holds the keyboard; `0` also from an empty prompt.
 */
export function navKey(key, label, kind) {
  return {
    text: key + ': ' + label,
    style: 'hotkey',
    button: { key: kind, label, kind, hotkey: key, plain: true },
  }
}

/**
 * The hint in the band's title row: focus the band, move, the digits, the
 * pane, help. j, k, and 0 are Buttons, so the keys it names are the band's
 * own. `form` is `full`, `short` (no words for moving or help), or `keys`
 * (the Buttons alone), for a band with less room.
 */
export function keyHint(rows, form = 'full') {
  const digits = digitRange(rows)
  const keys = [navKey('j', '\u2193', 'band-down'), text(' '), navKey('k', '\u2191', 'band-up')]
  const pane = navKey('0', 'pane', 'band-pane')
  if (form === 'keys') return [...keys, text(' ' + (digits ? digits + ' \u00b7 ' : ''), 'dim'), pane]
  const full = form === 'full'
  return [
    text('ctrl+x tab focus \u00b7 ', 'dim'),
    ...keys,
    text((full ? ' move' : '') + ' \u00b7 ' + (digits ? digits + ' open \u00b7 ' : ''), 'dim'),
    pane,
    ...(full ? [text(' \u00b7 /cos help', 'dim')] : []),
  ]
}

/**
 * The help view as plain text: every key and command of the board. The pane's
 * Help tab and `/cos help` draw these lines, so they always agree.
 */
export function helpLines(options = {}) {
  const digits = digitRange(options.rows) || '1'
  const row = (key, what) => '  ' + pad(key, 17) + what
  return [
    'keys',
    row(digits, (digits === '1' ? '1 opens a row' : digits + ' open a row') + ': 1 needs you, then one per project'),
    row('', 'from an empty prompt or the band; the pane takes the keyboard'),
    row('0', 'focus the pane, and open it if it is closed'),
    row('ctrl+x tab', 'focus the band, then each pane, then the prompt'),
    row('', 'press tab within 3 s of ctrl+x (Claude Code\u2019s chord limit)'),
    row('ctrl+x ctrl+a', 'collapse the band to one row, or expand it'),
    row('j k', 'move the selection through the rows, in the band and the pane'),
    row('h l', 'switch tabs; fold or unfold a node; l opens a leaf'),
    row('enter', 'open the selected project, saga, epic, or task'),
    row('esc', 'back one level, and at the top close the pane'),
    row('tab', 'the next control, and shift+tab the one before'),
    row('\u2191 \u2193', 'scroll the pane (Claude Code); the footer arrows move the selection'),
    row('', 'from an empty prompt, \u2193 goes to Claude Code\u2019s footer, not the band'),
    row('ctrl+x \u2190 \u2192', 'widen or narrow the pane (Claude Code); or drag its edge'),
    row('click', 'a row selects it; a second click or its \u25b8 opens it'),
    '',
    'commands',
    row('/cos', 'the board as text'),
    row('/cos <project>', 'one project'),
    row('/cos pane', 'open the pane'),
    row('/cos help', 'this help'),
    '',
    'buttons',
    row('[ ? ]', "the band's help button: this view"),
    row('Done', 'submits "Chris marked done: <item>" to the chief, who closes it'),
    row('', 'the row shows "marking done\u2026" until the hub closes it'),
    row('Ask orchestrator', 'submits a question about <item> to the chief'),
    row('', 'neither button calls the hub'),
    '',
    'the pane',
    row('project', 'opens its sagas, epics, and plain tasks'),
    row('epic', 'opens its child tasks'),
    row('task', 'its status, owner, blockers, PR, and last update'),
    row('', 'the breadcrumb in the title shows where you are'),
    row('', 'borant tasks <channel> reads a view; tasks unavailable if it fails'),
  ]
}

/** The rows of the pane's Help tab. */
export function paneHelpRows(options = {}) {
  const width = Math.max(20, options.width || 60)
  return helpLines(options).map((line) => fit([text(line, line.startsWith('  ') ? 'plain' : 'dimBold')], width))
}

/**
 * The title row of the band: its name, the counts and age, the key hint, and
 * the help button. `forms` (`{ label, right, hint }`) are tried in order; the
 * first that leaves the rule four cells is drawn. The last ones keep j, k, and
 * 0 with a shorter name and age, so the keys a person reads are always drawn;
 * if even that does not fit, the last form goes without its hint.
 */
function ruleCell(width, forms) {
  const help = { text: '[ ? ]', style: 'button', button: { key: 'help', label: '?', kind: 'help' } }
  const build = (label, right, tail) => {
    const tailLength = tail.reduce((n, s) => n + Array.from(s.text).length, 0)
    const fixed = 3 + Array.from(label).length + 1 + Array.from(right).length + tailLength + 1 + 5 + 3
    const dashes = Math.max(0, width - fixed)
    const out = [
      text(RULE + RULE + ' ', 'faint'),
      text(label, 'bold'),
      text(' ' + RULE.repeat(dashes), 'faint'),
      text(right, 'dim'),
      ...tail,
    ]
    out.push(text(' '), help, text(' ' + RULE + RULE, 'faint'))
    return { out, dashes }
  }
  for (const form of forms) {
    const built = build(form.label, form.right, form.hint.length ? [text(' '), ...form.hint] : [])
    if (built.dashes >= 4) return built.out
  }
  const last = forms[forms.length - 1]
  return build(last.label, last.right, []).out
}

/**
 * The widths of the project and saga columns of a wide band. Each column
 * starts from the longest text it holds, so a name or a saga title is not cut
 * while room is free, then narrows to what the terminal leaves after the
 * hotkey, the state glyph, the bar, and the stats. The project keeps 11 cells
 * and the saga 17, with one blank cell before the glyph; the epic title gets
 * at least eight when the width allows.
 */
export function bandColumns(projects, width) {
  const project = Math.max(11, ...projects.map((p) => Array.from(badge(p)).length))
  const saga = Math.max(17, ...projects.map((p) => Array.from(projectSummary(p).saga).length))
  // hotkey 6, saga gap 1, glyph 2, bar 10, ratio 5, stats 11, 11, 13.
  const reserved = 6 + 1 + 2 + 10 + 5 + 11 + 11 + 13
  const room = Math.max(11 + 17, width - reserved - 8)
  if (project + saga <= room) return { project, saga }
  // The project name comes first: it names the row. The saga gives way after.
  const projectWidth = Math.max(11, Math.min(project, room - 17))
  return { project: projectWidth, saga: Math.max(17, room - projectWidth) }
}

/** The row of the band that shows what waits on the person. */
export function needsRow(questions, width, narrow, digit = 1, columns = { project: 11, saga: 17 }) {
  const first = questions[0]
  const extra = questions.length - 1
  const context = []
  if (first?.attention_msg) context.push(first.attention_msg)
  if (extra > 0) context.push('+' + extra + ' more')
  const contextText = context.join(' \u00b7 ')
  if (narrow) {
    const prefix = [
      hotkey(digit, 'needs', 0),
      text(GLYPH.needs, 'yellow'),
      text('Needs you (' + questions.length + ')', 'yellowBold'),
    ]
    const prefixWidth = prefix.reduce((n, s) => n + Array.from(s.text).length, 0)
    const extraParts = []
    if (first?.project) extraParts.push(text(' ' + first.project, 'bold'))
    if (extra > 0) extraParts.push(text(' +' + extra, 'dim'))
    let extraWidth = extraParts.reduce((n, s) => n + Array.from(s.text).length, 0)
    if (width - prefixWidth - extraWidth < 9) {
      // The project and the "+n" are one block: drop both or keep both.
      extraParts.length = 0
      extraWidth = 0
    }
    const titleWidth = Math.max(0, width - prefixWidth - extraWidth - 1)
    const head = [...prefix]
    if (titleWidth > 0) head.push(text(' ' + truncate(first?.title || '', titleWidth)))
    head.push(...extraParts)
    return fitEllipsis(head, width)
  }
  return fit(
    layout(
      [
        { ...hotkey(digit, 'needs', 0), width: 5 },
        { text: GLYPH.needs, style: 'yellow', width: 2 },
        { text: 'Needs you (' + questions.length + ')', style: 'yellowBold', width: 15 },
        { text: first?.title || '', flex: 1 },
        // One blank cell, so a full title truncates with an ellipsis before the
        // badge instead of running into it.
        { text: '', width: 1 },
        { text: badge(first?.project), style: badgeStyle(first?.project, 0), width: columns.project },
        { text: ' ' + contextText, style: 'dim' },
      ],
      width,
    ),
    width,
  )
}

/** The state of a project's plain tasks, for a project with no epic. */
export function plainState(project) {
  const c = project?.tasks || {}
  if ((c.needs_human || 0) + (c.stuck || 0) > 0) return 'needs'
  if ((c.running || 0) > 0) return 'running'
  if ((c.blocked || 0) > 0 && !(c.ready || 0)) return 'blocked'
  return 'waiting'
}

/** One row for one project. */
export function projectRow(project, width, narrow, digit, index = 0, columns = { project: 11, saga: 17 }) {
  const s = projectSummary(project, narrow ? 8 : 10)
  const current = currentEpic(project)
  const epicTitle = current ? current.epic.title : s.plain ? openTasksLabel(project) : 'no epic yet'
  const state = current ? current.state : s.plain ? plainState(project) : 'waiting'
  // A project of plain tasks has no epics to count, so its bar is blank.
  const bar = s.plain
    ? { filled: '', empty: ' '.repeat(s.bar.filled.length + s.bar.empty.length), label: '' }
    : s.bar
  if (narrow) {
    const head = [
      hotkey(digit, 'project', project?.project, 'project-' + index),
      text(badge(project), badgeStyle(project, index)),
      text(' ' + s.saga + ' '),
      text(GLYPH[state] + ' ', state),
    ]
    const prefixWidth = head.reduce((n, s) => n + Array.from(s.text).length, 0)
    const stats = [
      ...(s.plain ? [] : [text(' ' + s.bar.filled, 'green'), text(s.bar.empty, 'faint'), text(' ' + s.bar.label)]),
      s.blocked ? text(' ' + GLYPH.blocked + ' ' + s.blocked, 'red') : null,
      s.needs ? text(' ' + GLYPH.needs + ' ' + s.needs, 'yellow') : null,
    ].filter(Boolean)
    const statsWidth = stats.reduce((n, s) => n + Array.from(s.text).length, 0)
    // The stats are one block: keep them whole and leave the epic at least
    // eight cells, or drop them and give the epic the room.
    const keepStats = prefixWidth + statsWidth + 8 <= width
    const epicWidth = Math.max(0, width - prefixWidth - (keepStats ? statsWidth : 0))
    return fitEllipsis(
      [...head, text(truncate(epicTitle, epicWidth)), ...(keepStats ? stats : [])],
      width,
    )
  }
  return fit(
    layout(
      [
        { ...hotkey(digit, 'project', project?.project, 'project-' + index), width: 6 },
        { text: badge(project), style: badgeStyle(project, index), width: columns.project },
        { text: s.saga, style: 'bold', width: columns.saga },
        // The state glyph never touches the saga title, even when it is cut.
        { text: ' ', width: 1 },
        { text: GLYPH[state], style: state, width: 2 },
        { text: epicTitle, flex: 1 },
        { text: bar.filled, style: 'green', width: bar.filled.length },
        { text: bar.empty, style: 'faint', width: bar.empty.length },
        { text: ' ' + bar.label + ' ', style: 'plain', width: 5 },
        { text: s.running + ' running', style: s.running ? 'plain' : 'dim', width: 11 },
        { text: s.blocked + ' blocked', style: s.blocked ? 'red' : 'dim', width: 11 },
        { text: s.needs + ' needs you', style: s.needs ? 'yellow' : 'dim', width: 13 },
      ],
      width,
    ),
    width,
  )
}

/** The band as one row, for a band with room for one row. */
export function condensedRow(board, options = {}) {
  const width = Math.max(20, options.width || 100)
  const updated = options.updated || hhmm(board?.at, '')
  const questions = allQuestions(board)
  const parts = []
  parts.push(text('cos  ', 'dimBold'))
  if (questions.length) {
    parts.push(hotkey(1, 'needs', 0), text(' ' + GLYPH.needs + ' ' + questions.length + ' need you', 'yellowBold'))
  } else {
    parts.push(text(GLYPH.done + ' all clear', 'green'))
  }
  activeProjects(board).forEach((project, i) => {
    const s = projectSummary(project, 10)
    parts.push(text('  ' + BRANCH + '  ', 'faint'))
    parts.push(text(badge(project), badgeStyle(project, i)))
    parts.push(text(' ' + (s.plain ? openTasksLabel(project) : s.bar.label) + (s.blocked ? ' ' + GLYPH.blocked + ' ' + s.blocked + ' blocked' : ''), s.blocked ? 'red' : 'plain'))
    if (s.needs === 0 && s.blocked === 0) parts.push(text(' ' + GLYPH.done + ' idle', 'dim'))
  })
  if (updated) parts.push(text('  ' + BRANCH + '  ' + updated, 'dim'))
  return fit(parts, width)
}

/**
 * The band above the prompt: a title rule, the needs-you row, then one row per
 * project. With room for one row it is the condensed row instead.
 */
export function bandRows(board, options = {}) {
  const width = Math.max(20, options.width || 100)
  const maxRows = options.maxRows === undefined || options.maxRows === null ? Infinity : options.maxRows
  const updated = options.updated || hhmm(board?.at, '')
  if (maxRows <= 1) return [condensedRow(board, { width, updated })]

  const questions = allQuestions(board)
  // A project with no open work stays off the band; the header counts it as idle.
  const projects = activeProjects(board)
  const narrow = width < 110
  const columns = narrow ? null : bandColumns(projects, width)
  const right = ' ' + projectCount(board) + ' \u00b7 updated ' + updated + ' '
  // The band numbers one row for what needs you, then one row per project.
  const rowCount = (questions.length ? 1 : 0) + projects.length
  const name = 'chief of staff'
  const forms = [
    ...(narrow ? [] : ['full', 'short'].map((form) => ({ label: name, right, hint: keyHint(rowCount, form) }))),
    { label: name, right, hint: keyHint(rowCount, 'keys') },
    { label: 'cos', right: ' ' + updated + ' ', hint: keyHint(rowCount, 'keys') },
  ]
  const rows = [fit(ruleCell(width, forms), width)]

  if (questions.length) rows.push(needsRow(questions, width, narrow, 1, columns))
  const base = questions.length ? 2 : 1
  projects.forEach((project, i) => {
    // The digits run 1 to 9; a tenth row keeps its place and loses its digit.
    const ordinal = base + i
    rows.push(projectRow(project, width, narrow, ordinal <= 9 ? ordinal : '', i, columns))
  })
  return rows.slice(0, maxRows === Infinity ? rows.length : maxRows)
}

/** The keys of the band's row chips, top to bottom: the rows j and k walk. */
export function bandRowKeys(rows) {
  return rows
    .map((row) => row.find((s) => s.button && (s.button.kind === 'project' || s.button.kind === 'needs')))
    .filter(Boolean)
    .map((s) => s.button.key)
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

/** The short words after an epic's title in the pane. */
export function epicNote(epic, state) {
  if (state === 'done') return 'done'
  if (state === 'needs') return 'waits on you'
  if (state === 'blocked') return 'blocked'
  if (state === 'running') return 'running'
  return 'waiting'
}

/**
 * The pane's tab row, fitted to `width`: every tab stays, with shorter
 * labels and then without brackets when room runs out. The open tab is the
 * primary Button and the others are dim, so the row says which one is open.
 */
export function paneTabs(tab, questions, width = Infinity) {
  const n = questions.length
  const forms = [
    { labels: ['Needs you (' + n + ')', 'Projects', 'Help'], gap: '  ' },
    { labels: ['Needs (' + n + ')', 'Projects', 'Help'], gap: ' ' },
    { labels: ['Needs ' + n, 'Proj', 'Help'], gap: ' ' },
    { labels: ['Needs ' + n, 'Proj', 'Help'], gap: ' \u00b7 ', plain: true },
  ]
  const kinds = ['needs', 'projects', 'help']
  const build = (form) => {
    const out = []
    form.labels.forEach((label, i) => {
      if (i > 0) out.push(text(form.gap, 'dim'))
      out.push({
        // A plain Button draws no variant, so the open tab wears brackets there.
        text: form.plain ? (tab === kinds[i] ? '[' + label + ']' : label) : '[ ' + label + ' ]',
        style: 'button',
        button: { key: 'tab-' + kinds[i], label: form.plain && tab === kinds[i] ? '[' + label + ']' : label, kind: 'tab-' + kinds[i], active: tab === kinds[i], plain: form.plain },
      })
    })
    return out
  }
  const cellsWidth = (cells) => cells.reduce((sum, c) => sum + Array.from(c.text).length, 0)
  for (const form of forms) {
    const cells = build(form)
    if (cellsWidth(cells) <= width) return cells
  }
  return build(forms[forms.length - 1])
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
