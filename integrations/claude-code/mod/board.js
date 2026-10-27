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

/** A digit hotkey cell, drawn by the renderer as a Button. */
export function hotkey(digit, kind, arg) {
  return {
    text: '[ ' + digit + ' ]',
    style: 'hotkey',
    button: { key: kind + '-' + digit, label: String(digit), hotkey: String(digit), kind, arg },
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
  const filled = t === 0 ? 0 : Math.max(0, Math.min(cells, Math.round((d / t) * cells)))
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
    saga: saga ? saga.title : 'no saga',
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

/** The dim hint in the band's title row: focus the band, move, the digits, help. */
export function keyHint(rows) {
  const digits = digitRange(rows)
  return ['ctrl+x tab focus', 'j/k move', digits ? digits + ' open' : null, '/cos help'].filter(Boolean).join(' \u00b7 ')
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
    row('ctrl+x tab', 'focus the band, then each pane, then the prompt'),
    row('ctrl+x ctrl+a', 'collapse the band to one row, or expand it'),
    row('\u2191 \u2193  j k', 'move the selection through the rows'),
    row('\u2190 \u2192  h l', 'switch tabs; fold or unfold a node; l opens a leaf'),
    row('enter', 'open the selected project, saga, epic, or task'),
    row('esc', 'back one level, and at the top close the pane'),
    row('tab', 'the next control, and shift+tab the one before'),
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
    row('Done', 'submits "Chris marked done: <item>" to the chief'),
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

/** The title row of the band: its name, the key hint, and the help button. */
function ruleCell(label, right, width, hint = '') {
  const help = { text: '[ ? ]', style: 'button', button: { key: 'help', label: '?', kind: 'help' } }
  const build = (tail) => {
    const fixed =
      3 + Array.from(label).length + 1 + Array.from(right).length + Array.from(tail).length + 1 + 5 + 3
    const dashes = Math.max(0, width - fixed)
    const out = [
      text(RULE + RULE + ' ', 'faint'),
      text(label, 'bold'),
      text(' ' + RULE.repeat(dashes), 'faint'),
      text(right, 'dim'),
    ]
    if (tail) out.push(text(tail, 'dim'))
    out.push(text(' '), help, text(' ' + RULE + RULE, 'faint'))
    return { out, dashes }
  }
  // The hint goes before anything else, leaving the rule four cells to itself.
  const full = build(hint ? ' ' + hint : '')
  return hint && full.dashes < 4 ? build('').out : full.out
}

/** The row of the band that shows what waits on the person. */
export function needsRow(questions, width, narrow, digit = 1) {
  const first = questions[0]
  const extra = questions.length - 1
  const context = []
  if (first?.attention_msg) context.push(first.attention_msg)
  if (extra > 0) context.push('+' + extra + ' more')
  const contextText = context.join(' \u00b7 ')
  if (narrow) {
    const head = [
      hotkey(digit, 'needs', 0),
      text(GLYPH.needs, 'yellow'),
      text('Needs you (' + questions.length + ')', 'yellowBold'),
      text(' ' + truncate(first?.title || '', Math.max(8, width - 34))),
      text(' ' + (first?.project || ''), 'bold'),
      text(extra > 0 ? ' +' + extra : '', 'dim'),
    ]
    return fit(head, width)
  }
  return fit(
    layout(
      [
        { ...hotkey(digit, 'needs', 0), width: 5 },
        { text: GLYPH.needs, style: 'yellow', width: 2 },
        { text: 'Needs you (' + questions.length + ')', style: 'yellowBold', width: 15 },
        { text: first?.title || '', flex: 1 },
        { text: badge(first?.project), style: badgeStyle(first?.project, 0), width: 11 },
        { text: ' ' + contextText, style: 'dim' },
      ],
      width,
    ),
    width,
  )
}

/** One row for one project. */
export function projectRow(project, width, narrow, digit, index = 0) {
  const s = projectSummary(project, narrow ? 8 : 10)
  const current = currentEpic(project)
  const epicTitle = current ? current.epic.title : 'no epic yet'
  const state = current ? current.state : 'waiting'
  if (narrow) {
    return fit(
      [
        hotkey(digit, 'project', project?.project),
        text(badge(project), badgeStyle(project, index)),
        text(' ' + s.saga + ' '),
        text(GLYPH[state] + ' ', state),
        text(truncate(epicTitle, Math.max(8, width - 46))),
        text(' ' + s.bar.filled, 'green'),
        text(s.bar.empty, 'faint'),
        text(' ' + s.bar.label),
        s.blocked ? text(' ' + GLYPH.blocked + ' ' + s.blocked, 'red') : text(''),
        s.needs ? text(' ' + GLYPH.needs + ' ' + s.needs, 'yellow') : text(''),
      ],
      width,
    )
  }
  return fit(
    layout(
      [
        { ...hotkey(digit, 'project', project?.project), width: 6 },
        { text: badge(project), style: badgeStyle(project, index), width: 11 },
        { text: s.saga, style: 'bold', width: 17 },
        { text: GLYPH[state], style: state, width: 2 },
        { text: epicTitle, flex: 1 },
        { text: s.bar.filled, style: 'green', width: s.bar.filled.length },
        { text: s.bar.empty, style: 'faint', width: s.bar.empty.length },
        { text: ' ' + s.bar.label + ' ', style: 'plain', width: 5 },
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
  allProjects(board).forEach((project, i) => {
    const s = projectSummary(project, 10)
    parts.push(text('  ' + BRANCH + '  ', 'faint'))
    parts.push(text(badge(project), badgeStyle(project, i)))
    parts.push(text(' ' + s.bar.label + (s.blocked ? ' ' + GLYPH.blocked + ' ' + s.blocked + ' blocked' : ''), s.blocked ? 'red' : 'plain'))
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
  const projects = allProjects(board)
  const narrow = width < 110
  const plural = projects.length === 1 ? 'project' : 'projects'
  const right = ' ' + projects.length + ' ' + plural + ' \u00b7 updated ' + updated + ' '
  // The band numbers one row for what needs you, then one row per project.
  const rowCount = (questions.length ? 1 : 0) + projects.length
  const hint = narrow ? '' : keyHint(rowCount)
  const rows = [fit(ruleCell('chief of staff', right, width, hint), width)]

  if (questions.length) rows.push(needsRow(questions, width, narrow, 1))
  const base = questions.length ? 2 : 1
  projects.forEach((project, i) => {
    rows.push(projectRow(project, width, narrow, Math.min(9, base + i), i))
  })
  return rows.slice(0, maxRows === Infinity ? rows.length : maxRows)
}

/** The short words after an epic's title in the pane. */
export function epicNote(epic, state) {
  if (state === 'done') return 'done'
  if (state === 'needs') return 'waits on you'
  if (state === 'blocked') return 'blocked'
  if (state === 'running') return 'running'
  return 'waiting'
}

/** The header row of the pane's tabs. */
export function paneTabs(tab, questions) {
  return [
    {
      text: '[ Needs you (' + questions.length + ') ]',
      style: 'button',
      width: 20,
      button: { key: 'tab-needs', label: 'Needs you (' + questions.length + ')', kind: 'tab-needs' },
    },
    text('  '),
    { text: '[ Projects ]', style: 'button', width: 12, button: { key: 'tab-projects', label: 'Projects', kind: 'tab-projects' } },
    text('  '),
    { text: '[ Help ]', style: 'button', width: 8, button: { key: 'tab-help', label: 'Help', kind: 'tab-help' } },
    text(tab === 'needs' ? '   needs you' : tab === 'projects' ? '   projects' : '   help', 'dim'),
  ]
}

/** The `/cos` digest: the board in text, as artboard 5 draws it. */
export function digest(board, options = {}) {
  const width = Math.max(48, Math.min(96, options.width || 96))
  const only = options.project || null
  const questions = allQuestions(board)
  const projects = allProjects(board).filter((p) => !only || p.project === only)
  const lines = []
  const head = only ? 'cos \u00b7 ' + only : 'cos \u00b7 ' + questions.length + ' need you \u00b7 ' + allProjects(board).length + ' projects'
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
  for (const project of projects) {
    const s = projectSummary(project, 10)
    lines.push(BRANCH + '  ' + pad(s.project, 10) + ' ' + pad(s.saga, 17) + ' ' + s.bar.text + ' \u00b7 ' + orchestratorLine(project))
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
    if (s.needs === 0 && s.blocked === 0 && s.running === 0) lines.push(BRANCH + '   ' + GLYPH.waiting + ' Waiting for a goal')
    lines.push(BRANCH)
  }
  if (only && projects.length === 0) lines.push(BRANCH + '  no project ' + truncate(only, 30))
  const foot = only ? '/cos pane for the pane' : '/cos <project> for one project \u00b7 /cos pane for the pane'
  lines.push('\u2570\u2500 ' + foot + ' ' + RULE.repeat(Math.max(0, width - foot.length - 4)))
  return lines.join('\n')
}
