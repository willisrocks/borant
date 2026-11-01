// The ledger band: the whole board above the prompt, and the one place for
// its keys. A title row, one row per needs-you item, then one row per active
// project. Enter opens the selected row in place. Pure, like board.js: the
// tests call it with a fixture and plain state.
//
// A row is `{ key, segments, selected }`. A segment is `{ text, style }`, or
// a Button cell `{ text, style, button }` where `button` is
// `{ key, kind, arg, hotkey?, parts: [{ text, style }], chrome? }`. A plain
// Button with a hotkey draws `N: <parts>`, so a cell's `text` counts the
// `N: ` the engine draws before its parts.

import {
  GLYPH,
  activeProjects,
  activeSaga,
  allProjects,
  allQuestions,
  condensedRow,
  currentEpic,
  digitRange,
  epicNote,
  epicState,
  findProject,
  openTasksLabel,
  pad,
  plainState,
  projectSummary,
  questionKey,
  stampOf,
  text,
  truncate,
} from './board.js'
import { ageText, canDone, pressStatus } from './status.js'

/** From this many body columns the band says "chief of staff", below it "cos". */
export const WIDE = 110
const RULE = '─'
const GAP = 2
/** `N: ` before a row's first column. */
const CHIP = 3
/** Where an open row's lines start: under the text after the glyph. */
const INDENT = 5

const width = (s) => Array.from(String(s)).length
const segWidth = (segs) => segs.reduce((n, s) => n + width(s.text), 0)

/** The chip key of a needs-you item and of a project row. */
export const needsKey = (q) => 'q:' + questionKey(q)
export const projectKey = (p) => 'p:' + String(p?.project || '?')

/** A Button cell. `hotkey` draws `h: ` before the parts. */
function button(key, kind, parts, opts = {}) {
  const lead = opts.hotkey ? opts.hotkey + ': ' : ''
  return {
    text: lead + parts.map((p) => p.text).join(''),
    style: 'button',
    button: { key, kind, arg: opts.arg, hotkey: opts.hotkey, parts },
  }
}

/** Pad a row of segments to `w` cells, or cut its tail with an ellipsis. */
function fitRow(segments, w) {
  const out = []
  let used = 0
  for (const s of segments) {
    const n = width(s.text)
    if (used + n <= w) {
      out.push(s)
      used += n
      continue
    }
    // A Button never splits: its hotkey must stay whole or go.
    if (!s.button && w - used > 1) {
      out.push({ ...s, text: truncate(s.text, w - used) })
      used = w
    } else if (w - used >= 1) {
      out.push(text('…', 'dim'))
      used += 1
    }
    break
  }
  if (used < w) out.push(text(' '.repeat(w - used)))
  return out
}

/** Split `value` into lines of at most `w` cells, at spaces where it can. */
export function wrap(value, w) {
  const out = []
  let line = ''
  for (const word of String(value).split(/\s+/).filter(Boolean)) {
    let piece = word
    while (width(piece) > w) {
      if (line) {
        out.push(line)
        line = ''
      }
      out.push(Array.from(piece).slice(0, w).join(''))
      piece = Array.from(piece).slice(w).join('')
    }
    if (!piece) continue
    if (!line) line = piece
    else if (width(line) + 1 + width(piece) <= w) line += ' ' + piece
    else {
      out.push(line)
      line = piece
    }
  }
  if (line) out.push(line)
  return out
}

// ---------------------------------------------------------------- title row

/**
 * The title row: `── chief of staff ─ 2 need you · 3 active · 1 idle ──── <hint> ─ <freshness> ─ [ ? ] ─`.
 * The hint at rest names the digits; while the band holds the keys it names
 * j, k, Enter, d, a, and esc, and j and k are its own Buttons.
 */
export function titleRow(board, st) {
  const w = st.width
  const questions = (st.questions || []).filter((q) => !q.closed)
  const all = allProjects(board).length
  const active = activeProjects(board).length
  const label = w >= WIDE ? 'chief of staff' : 'cos'
  const lead = [text(RULE + RULE + ' ', 'faint'), text(label, 'bold'), text(' ' + RULE + ' ', 'faint')]
  const needs = questions.length
    ? text(questions.length + (questions.length === 1 ? ' needs' : ' need') + ' you', 'yellowBold')
    : text(GLYPH.done + ' nothing needs you', 'green')
  const counts = text(' · ' + active + ' active · ' + (all - active) + ' idle', 'dim')

  // The engine draws a Button with chrome as `[ ? ]`.
  const help = () => ({ text: '[ ? ]', style: 'button', button: { key: 'help', kind: 'help', parts: [text('?')], chrome: true } })
  const fresh = [st.fresh || text('polling', 'dim')]
  if (st.unavailable) {
    // Without a board the title row is the reason: no counts, no hint.
    const room = w - segWidth(lead.slice(0, 2)) - 4 - segWidth(fresh) - 3 - 3 - 5 - 2
    if (st.reason && room > 4) fresh.push(text(' · ' + truncate(st.reason, room), 'dim'))
    return fitRow([...lead.slice(0, 2), text(' ' + RULE + RULE + ' ', 'faint'), ...fresh, text(' ' + RULE + ' ', 'faint'), help(), text(' ' + RULE, 'faint')], w)
  }
  const tail = [text(' ' + RULE + ' ', 'faint'), ...fresh, text(' ' + RULE + ' ', 'faint'), help(), text(' ' + RULE, 'faint')]

  const digits = digitRange(st.rowCount)
  const key = (k) => text(k, 'key')
  const sep = (s) => text(s, 'dim')
  const jk = [
    button('band-down', 'band-down', [text('↓', 'plain')], { hotkey: 'j' }),
    text(' '),
    button('band-up', 'band-up', [text('↑', 'plain')], { hotkey: 'k' }),
  ]
  const da = st.needsSelected ? [sep(' · '), key('d'), sep(' done · '), key('a'), sep(' ask')] : []
  // With the band's ring (ctrl+x tab, a click) Enter opens and esc leaves;
  // from the prompt after a digit, space opens and any other key types.
  const open = st.ring ? '⏎' : 'space'
  const leave = st.ring ? [key('esc'), sep(' prompt')] : [sep('type to leave')]
  const hints = st.focused
    ? [
        [...jk, sep(' move · '), key(open), sep(' open'), ...da, sep(' · '), ...leave],
        [...jk, sep(' move · '), key(open), sep(' open'), ...da],
        [...jk, sep(' · '), key(open), ...(st.needsSelected ? [sep(' · '), key('d'), sep(' '), key('a')] : [])],
        jk,
      ]
    : [
        digits ? [key(digits), sep(' select · /cos help')] : [sep('/cos help')],
        digits ? [key(digits), sep(' select')] : [],
        [],
      ]
  // A shorter hint gives way to the counts first: the keys matter more.
  const forms = hints.flatMap((hint) => [{ middle: [needs, counts], hint }, { middle: [needs], hint }])
  for (const form of forms) {
    const right = form.hint.length ? [text(' '), ...form.hint, ...tail] : tail
    const dashes = w - segWidth(lead) - segWidth(form.middle) - 1 - segWidth(right)
    if (dashes >= 3) {
      return fitRow([...lead, ...form.middle, text(' ' + RULE.repeat(dashes), 'faint'), ...right], w)
    }
  }
  return fitRow([...lead, needs, text(' '), ...tail], w)
}

// ------------------------------------------------------------ needs-you rows

/** The dim block after a needs-you title: project, attention, and age. */
function needsContext(q, at, full) {
  const parts = [q.project || '']
  if (full && q.attention_msg) parts.push(q.attention_msg)
  const since = stampOf(q.since)
  if (since !== null && typeof at === 'number') parts.push(ageText(Math.max(0, at - since)))
  return parts.filter(Boolean).join(' · ')
}

/**
 * The status and the buttons of one needs-you item: a press status replaces
 * the buttons in the same place, and Done again or Ask again come back once
 * a press failed or an ask landed. Buttons only when `withButtons`.
 */
export function actions(q, st, withButtons) {
  const key = questionKey(q)
  const done = (st.pending || {})[key]
  const ask = (st.asked || {})[key]
  const status = q.closed ? [text(GLYPH.done + ' closed', 'green')] : pressStatus(done, ask, st.now || 0)
  const out = status ? [...status] : []
  if (!withButtons || q.closed) return out
  const doneButton = (label) => button('done', 'done', [text(label, 'plain')], { hotkey: 'd', arg: key })
  const askButton = (label) => button('ask', 'ask', [text(label, 'plain')], { hotkey: 'a', arg: key })
  const buttons = []
  if (!status) buttons.push(doneButton('Done'), askButton('Ask orchestrator'))
  else if (done && canDone(done)) buttons.push(doneButton('Done again'))
  else if (!done && ask && ask.state !== 'sending') buttons.push(doneButton('Done'), askButton('Ask again'))
  for (const b of buttons) {
    if (out.length) out.push(text('   '))
    out.push(b)
  }
  return out
}

/** The title and context widths of the needs-you rows, and which context form. */
function needsColumns(questions, w, at) {
  const titles = Math.max(0, ...questions.map((q) => width(q.title || '')))
  const full = Math.max(0, ...questions.map((q) => width(needsContext(q, at, true))))
  // The attention words stay only while every title shows whole.
  if (w - CHIP - 2 - GAP - full >= titles) return { title: titles, ctx: full, full: true }
  // Then the title comes first: the context column keeps at most 28% of the
  // row, and a longer context is cut with an ellipsis.
  const short = Math.max(0, ...questions.map((q) => width(needsContext(q, at, false))))
  const ctx = Math.min(short, Math.max(14, Math.floor(w * 0.28)))
  return { title: Math.max(10, Math.min(titles, w - CHIP - 2 - GAP - ctx)), ctx, full: false }
}

function needsRows(q, i, st, cols) {
  const key = needsKey(q)
  const selected = st.selected === key
  const isOpen = selected && st.open === key
  const glyph = q.closed ? GLYPH.done : isOpen ? '▾' : GLYPH.needs
  const glyphStyle = q.closed ? 'green' : 'yellow'
  const titleStyle = q.closed ? 'dim' : 'yellowBold'
  const title = q.title || ''
  const ordinal = st.ordinal(i)
  const context = needsContext(q, st.at, cols.full)
  // The selected row carries its buttons at its end, unless it is open. A
  // long status gives way before the buttons and a 16-cell title do.
  const acts = isOpen ? [] : shorten(actions(q, st, selected), st.width - CHIP - 2 - 16 - GAP)
  const actsWidth = acts.length ? GAP + segWidth(acts) : 0
  let titleWidth = cols.title
  let ctxWidth = cols.ctx
  let showCtx = true
  const over = CHIP + 2 + titleWidth + GAP + ctxWidth + actsWidth - st.width
  if (over > 0) {
    // Give way on this row only: the context column first loses its padding,
    // then the title shrinks, then the context goes.
    ctxWidth = Math.min(ctxWidth, width(context))
    const still = CHIP + 2 + titleWidth + GAP + ctxWidth + actsWidth - st.width
    if (still > 0) titleWidth = Math.max(16, titleWidth - still)
    if (CHIP + 2 + titleWidth + GAP + ctxWidth + actsWidth > st.width) {
      showCtx = false
      titleWidth = Math.max(10, st.width - CHIP - 2 - actsWidth)
    }
  }
  const shownTitle = truncate(title, titleWidth)
  const lead = chip(ordinal, key, 'needs', i, [text(glyph + ' ', glyphStyle), text(shownTitle, titleStyle)])
  const segments = [...lead, text(' '.repeat(titleWidth - width(shownTitle)))]
  if (showCtx) segments.push(text(' '.repeat(GAP)), text(pad(cutContext(context, ctxWidth), ctxWidth), 'dim'))
  if (acts.length) segments.push(text(' '.repeat(GAP)), ...acts)
  const rows = [{ key: 'row-' + key, segments: fitRow(segments, st.width), selected }]
  if (!isOpen) return rows

  // Open: the whole title, the details, and the buttons on their own line.
  const room = Math.max(10, st.width - INDENT)
  const line = (k, segs) => rows.push({ key: 'row-' + key + '-' + k, segments: fitRow([text(' '.repeat(INDENT)), ...segs], st.width), selected })
  if (shownTitle !== title) wrap(title, room).forEach((l, j) => line('title-' + j, [text(l, titleStyle)]))
  const project = findProject(st.board, q.project)
  const owner = q.owner || (project?.orchestrators || [])[0]?.agent || ''
  const orch = (project?.orchestrators || []).find((o) => o.agent === owner)
  const detail = [
    !cols.full || !showCtx ? q.attention_msg : '',
    activeSaga(project)?.title,
    owner ? 'owner ' + owner : 'no owner',
    orch?.host,
    orch ? (orch.busy ? 'busy' : orch.online === false ? 'offline' : 'online') : '',
    q.channel,
  ].filter(Boolean)
  wrap(detail.join(' · '), room).forEach((l, j) => line('detail-' + j, [text(l, 'dim')]))
  const all = shorten(actions(q, st, !q.closed), room)
  if (all.length) line('actions', all)
  return rows
}

/** A context cut to `w` cells keeps its age: `roger-emr-obs… · 3h`. */
function cutContext(context, w) {
  if (width(context) <= w) return context
  const parts = context.split(' · ')
  const tail = parts.length > 1 ? ' · ' + parts[parts.length - 1] : ''
  return truncate(parts.slice(0, -1).join(' · ') || context, Math.max(1, w - width(tail))) + tail
}

/** Cut the status (the first, plain segment) so the segments fit `room`. */
function shorten(segs, room) {
  const over = segWidth(segs) - room
  if (over <= 0 || !segs.length || segs[0].button) return segs
  const first = segs[0]
  return [{ ...first, text: truncate(first.text, Math.max(4, width(first.text) - over)) }, ...segs.slice(1)]
}

/**
 * A row's first cell: `N: ` and the parts, one Button. A row past nine has
 * no digit, so it keeps the three cells blank and the columns stay put.
 */
function chip(ordinal, key, kind, arg, parts) {
  const digit = ordinal >= 1 && ordinal <= 9 ? String(ordinal) : ''
  const cell = button(key, kind, parts, { hotkey: digit || undefined, arg })
  return digit ? [cell] : [text(' '.repeat(CHIP)), cell]
}

// -------------------------------------------------------------- project rows

/** What one project row shows, before the columns are sized. */
function projectCells(p) {
  const s = projectSummary(p, 10)
  const current = currentEpic(p)
  const state = current ? current.state : s.plain ? plainState(p) : 'waiting'
  const orch = (p?.orchestrators || [])[0]
  return {
    name: String(p?.project || '?'),
    saga: s.saga,
    plain: s.plain,
    state,
    epic: current ? current.epic.title : s.plain ? openTasksLabel(p) : 'no epic yet',
    done: s.bar.label,
    summary: s,
    words: [
      s.running ? text(s.running + ' running', 'plain') : null,
      s.blocked ? text(s.blocked + ' blocked', 'red') : null,
      s.needs ? text(s.needs + ' needs you', 'yellow') : null,
    ].filter(Boolean),
    glyphs: [
      s.blocked ? text(GLYPH.blocked + s.blocked, 'red') : null,
      s.needs ? text(GLYPH.needs + s.needs, 'yellow') : null,
    ].filter(Boolean),
    orch: orch ? orch.agent + (orch.busy ? ' · busy' : '') : 'no orchestrator',
  }
}

const joined = (segs) => segs.flatMap((s, i) => (i ? [text('  '), s] : [s]))

/**
 * The column widths of the project rows. Each column starts from its longest
 * text; the orchestrator, the last column, takes what is left, so no gap sits
 * in the middle. With too little room the band drops, in order: the
 * orchestrator, the count words (glyph counts instead), the saga, then half
 * the bar. The saga and the epic shrink to a floor before a step drops.
 */
export function projectColumns(cells, w) {
  const most = (f) => Math.max(0, ...cells.map(f))
  const name = most((c) => width(c.name))
  const saga = most((c) => width(c.saga))
  const epic = most((c) => 2 + width(c.epic))
  const orch = most((c) => width(c.orch))
  const ratio = most((c) => (c.plain ? 0 : width(c.done)))
  const words = most((c) => segWidth(joined(c.words)))
  const glyphs = most((c) => segWidth(joined(c.glyphs)))
  const steps = [
    { orch: true, words: true, saga: true, bar: 10 },
    { orch: false, words: true, saga: true, bar: 10 },
    { orch: false, words: false, saga: true, bar: 10 },
    { orch: false, words: false, saga: false, bar: 10 },
    { orch: false, words: false, saga: false, bar: 5 },
  ]
  for (const step of steps) {
    const counts = step.words ? words : glyphs
    const sagaMin = step.saga ? Math.min(saga, 20) : 0
    const epicMin = Math.min(epic, 26)
    const orchMin = step.orch ? Math.min(orch, 12) : 0
    const barWidth = step.bar + 1 + ratio
    const fixed = CHIP + name + GAP + (step.saga ? GAP : 0) + GAP + barWidth + (counts ? GAP + counts : 0) + (step.orch ? GAP : 0)
    let room = w - fixed - sagaMin - epicMin - orchMin
    if (room < 0) continue
    const e = epicMin + Math.min(room, epic - epicMin)
    room -= e - epicMin
    const s = step.saga ? sagaMin + Math.min(room, saga - sagaMin) : 0
    room -= s - sagaMin
    return { ...step, name, saga: s, epic: e, orch: step.orch ? orchMin + room : 0, ratio, counts }
  }
  // Even the smallest step does not fit: the name gives way too.
  const counts = glyphs
  const left = w - (CHIP + GAP + GAP + 6 + ratio + (counts ? GAP + counts : 0))
  const epicWidth = Math.max(8, Math.min(epic, Math.floor(left / 2)))
  return { orch: false, words: false, saga: 0, bar: 5, name: Math.max(6, left - epicWidth), epic: epicWidth, ratio, counts }
}

function projectRows(p, i, st, cells, cols) {
  const key = projectKey(p)
  const selected = st.selected === key
  const isOpen = selected && st.open === key
  const shownName = truncate(cells.name, cols.name)
  const segments = [...chip(st.ordinal(i), key, 'project', p.project, [text(shownName, 'bold')])]
  segments.push(text(' '.repeat(cols.name - width(shownName) + GAP)))
  if (cols.saga) segments.push(text(pad(cells.saga, cols.saga), cells.plain ? 'dim' : 'plain'), text(' '.repeat(GAP)))
  segments.push(text(GLYPH[cells.state] + ' ', cells.state), text(pad(cells.epic, cols.epic - 2)), text(' '.repeat(GAP)))
  const bar = projectSummary(p, cols.bar).bar
  if (cells.plain) segments.push(text(' '.repeat(cols.bar + 1 + cols.ratio)))
  else segments.push(text(bar.filled, 'green'), text(bar.empty, 'faint'), text(' ' + pad(bar.label, cols.ratio)))
  if (cols.counts) {
    const counts = joined(cols.words ? cells.words : cells.glyphs)
    segments.push(text(' '.repeat(GAP)), ...counts, text(' '.repeat(cols.counts - segWidth(counts))))
  }
  if (cols.orch) segments.push(text(' '.repeat(GAP)), text(truncate(cells.orch, cols.orch), 'dim'))
  const rows = [{ key: 'row-' + key, segments: fitRow(segments, st.width), selected }]
  if (!isOpen) return rows

  // Open: the whole saga name, the orchestrator line, and the epics.
  const room = Math.max(10, st.width - INDENT)
  const line = (k, segs) => rows.push({ key: 'row-' + key + '-' + k, segments: fitRow([text(' '.repeat(INDENT)), ...segs], st.width), selected })
  if (!cells.plain && activeSaga(p)) wrap(cells.saga, room).forEach((l, j) => line('saga-' + j, [text(l, 'bold')]))
  const orch = (p?.orchestrators || [])[0]
  const s = cells.summary
  const about = [
    orch ? orch.agent + ' · ' + (orch.busy ? 'busy' : orch.online === false ? 'offline' : 'online') : 'no orchestrator online',
    orch?.host,
    s.running ? s.running + ' running' : '',
    s.blocked ? s.blocked + ' blocked' : '',
    s.needs ? s.needs + ' need you' : '',
  ].filter(Boolean)
  wrap(about.join(' · '), room).forEach((l, j) => line('orch-' + j, [text(l, 'dim')]))
  const epics = [...(activeSaga(p)?.epics || []), ...(p?.epics || [])]
  const items = []
  let done = 0
  for (const epic of epics) {
    const state = epicState(epic)
    if (state === 'done') {
      done += 1
      continue
    }
    items.push([text(GLYPH[state] + ' ', state), text(epic.title || ''), text('  ' + epicNote(epic, state), state === 'waiting' ? 'dim' : state)])
  }
  if (done) items.push([text(GLYPH.done + ' ' + done + ' done', 'done')])
  if (!epics.length) items.push([text(GLYPH[cells.state] + ' ', cells.state), text(openTasksLabel(p))])
  // The epics flow across lines, four cells apart; one too long is cut.
  let current = []
  let n = 0
  for (const item of items) {
    const w = segWidth(item)
    if (current.length && n + 4 + w > room) {
      line('epics-' + rows.length, current)
      current = []
      n = 0
    }
    if (current.length) {
      current.push(text('    '))
      n += 4
    }
    current.push(...item)
    n += w
  }
  if (current.length) line('epics-' + rows.length, current)
  return rows
}

// ---------------------------------------------------------------------- band

/**
 * The band's rows. `st` holds `{ width, maxRows, selected, open, focused,
 * fresh, unavailable, reason, questions, pending, asked, now }`: `selected`
 * and `open` are chip keys, `questions` the needs-you items as drawn (with
 * the ones a Done just closed). With room for one row it is the condensed row.
 */
export function bandRows(board, state = {}) {
  const w = Math.max(40, state.width || 100)
  const maxRows = state.maxRows === undefined || state.maxRows === null ? Infinity : state.maxRows
  if (maxRows <= 1) return [{ key: 'condensed', segments: condensedRow(board, { width: w }), selected: false }]
  const questions = state.questions || allQuestions(board)
  const projects = activeProjects(board)
  const rowCount = questions.length + projects.length
  const keys = [...questions.map(needsKey), ...projects.map(projectKey)]
  const selected = keys.includes(state.selected) ? state.selected : null
  const st = {
    ...state,
    board,
    width: w,
    questions,
    rowCount,
    selected,
    focused: Boolean(state.focused),
    needsSelected: Boolean(selected && selected.startsWith('q:') && !questions.find((q) => needsKey(q) === selected)?.closed),
    at: stampOf(board?.at),
  }
  const rows = [{ key: 'title', segments: titleRow(board, st), selected: false }]
  if (rowCount === 0) {
    rows.push({ key: 'empty', segments: fitRow([text('   no project has open work', 'dim')], w), selected: false })
    return rows
  }
  if (questions.length) {
    const cols = needsColumns(questions, w, st.at)
    questions.forEach((q, i) => rows.push(...needsRows(q, i, { ...st, ordinal: () => i + 1 }, cols)))
  }
  const cells = projects.map(projectCells)
  const cols = projectColumns(cells, w)
  projects.forEach((p, i) =>
    rows.push(...projectRows(p, i, { ...st, ordinal: () => questions.length + i + 1 }, cells[i], cols)),
  )
  return rows
}

/** The chip keys of the band's rows, top to bottom: the rows j and k walk. */
export function bandKeys(board, questions) {
  return [...(questions || allQuestions(board)).map(needsKey), ...activeProjects(board).map(projectKey)]
}
