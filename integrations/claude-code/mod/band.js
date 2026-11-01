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
  const can = st.selectedButtons || {}
  const da = [
    ...(can.d ? [sep(' · '), key('d'), sep(' done')] : []),
    ...(can.a ? [sep(' · '), key('a'), sep(' ask')] : []),
  ]
  const daShort = [...(can.d ? [sep(' · '), key('d')] : []), ...(can.a ? [sep(' · '), key('a')] : [])]
  // With the band's ring (ctrl+x tab, a click) Enter opens and esc leaves;
  // from the prompt after a digit, space opens and any other key types.
  const open = st.ring ? '⏎' : 'space'
  const leave = st.ring ? [key('esc'), sep(' prompt')] : [sep('type to leave')]
  const hints = st.focused
    ? [
        [...jk, sep(' move · '), key(open), sep(' open'), ...da, sep(' · '), ...leave],
        [...jk, sep(' move · '), key(open), sep(' open'), ...da],
        [...jk, sep(' · '), key(open), ...daShort],
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

/** The room the selected row keeps after its title: the widest Done and Ask
 * buttons, or a short status with one-word buttons. */
const BUTTONS = width('✓ asked at 10:41   d: Done   a: Ask')

/**
 * Which buttons a needs-you item shows: `{ d, a }`, each a label or absent.
 * The title row's hint names d and a from the same answer, so a hint never
 * names a key that does nothing.
 */
export function buttonsOf(q, st) {
  if (!q || q.closed) return {}
  const key = questionKey(q)
  const done = (st.pending || {})[key]
  const ask = (st.asked || {})[key]
  if (!pressStatus(done, ask, st.now || 0)) return { d: 'Done', a: 'Ask orchestrator' }
  if (done && canDone(done)) return { d: 'Done again' }
  if (!done && ask && ask.state !== 'sending') return { d: 'Done', a: 'Ask again' }
  return {}
}

/**
 * The status and the buttons of one needs-you item: a press status replaces
 * the buttons in the same place, and Done again or Ask again come back once
 * a press failed or an ask landed. Buttons only when `withButtons`. A first
 * d or a typed at the prompt arms the press: the row asks for it again.
 */
export function actions(q, st, withButtons) {
  const key = questionKey(q)
  if (withButtons && st.armed && st.armed.item === key && !q.closed) {
    const what = st.armed.key === 'd' ? 'Done' : 'Ask ' + (st.armed.owner || 'orchestrator')
    return [text(st.armed.key, 'key'), text(' again: ' + what, 'yellowBold'), text(' · any other key cancels', 'dim')]
  }
  const status = q.closed ? [text(GLYPH.done + ' closed', 'green')] : pressStatus((st.pending || {})[key], (st.asked || {})[key], st.now || 0)
  const out = status ? [...status] : []
  if (!withButtons) return out
  const can = buttonsOf(q, st)
  for (const hotkey of ['d', 'a']) {
    if (!can[hotkey]) continue
    if (out.length) out.push(text('   '))
    out.push(button(hotkey === 'd' ? 'done' : 'ask', hotkey === 'd' ? 'done' : 'ask', [text(can[hotkey], 'plain')], { hotkey, arg: key }))
  }
  return out
}

/**
 * Where the project columns start, as cells from the left: the edges a
 * needs-you context can line up with.
 */
function projectEdges(cols) {
  const edges = []
  let at = CHIP + cols.name + GAP
  if (cols.saga) {
    edges.push(at)
    at += cols.saga + GAP
  }
  edges.push(at)
  at += cols.epic + GAP
  edges.push(at)
  at += cols.bar + 1 + cols.ratio
  if (cols.counts) {
    at += GAP
    edges.push(at)
    at += cols.counts
  }
  if (cols.orch) edges.push(at + GAP)
  return edges
}

/**
 * The needs-you columns: one title width for every row, so the title column
 * never moves, and the context after it. The title column fits the longest
 * title where it can, else four titles in five, and its end lines up with a
 * project column when one is near; it leaves room for the Done and Ask
 * buttons. A cut title shows whole when its row opens.
 */
function needsColumns(questions, w, at, edges) {
  const titles = questions.map((q) => width(q.title || '')).sort((a, b) => a - b)
  const most = titles[titles.length - 1] || 0
  const most80 = titles[Math.max(0, Math.ceil(titles.length * 0.8) - 1)] || 0
  const lead = CHIP + 2
  const cap = Math.max(10, Math.min(Math.floor(w * 0.6), w - lead - GAP - BUTTONS))
  // A project edge as a title width: the context then starts on that edge.
  const snaps = edges.map((e) => e - lead - GAP).filter((t) => t >= 10 && t <= cap)
  const near = (need) => snaps.find((t) => t >= need && t - need <= 20)
  // One title far longer than the rest is cut, and shows whole when open.
  const outlier = most > most80 + 20
  const want = outlier ? most80 : most
  const title = want > cap ? cap : (near(want) ?? want)
  const slot = w - lead - title - GAP
  const fullest = Math.max(0, ...questions.map((q) => width(needsContext(q, at, true))))
  const full = fullest <= slot
  const ctx = Math.min(slot, full ? fullest : Math.max(0, ...questions.map((q) => width(needsContext(q, at, false)))))
  return { title, slot, ctx, full }
}

function needsRows(q, i, st, cols) {
  const key = needsKey(q)
  const selected = st.selected === key
  const isOpen = selected && st.open === key
  const glyph = q.closed ? GLYPH.done : isOpen ? '▾' : GLYPH.needs
  const glyphStyle = q.closed ? 'green' : 'yellow'
  const titleStyle = q.closed ? 'dim' : 'yellowBold'
  const title = q.title || ''
  const shownTitle = truncate(title, cols.title)
  // The chip is the digit and the glyph: the ring's mark stays that small.
  const segments = [...chip(st.ordinal(i), key, 'needs', i, [text(glyph, glyphStyle)]), text(' '), text(pad(shownTitle, cols.title), titleStyle), text(' '.repeat(GAP))]
  const context = needsContext(q, st.at, cols.full)
  // The status and buttons follow the context when both fit, else they take
  // its place; the status gives way first, then the context. The title
  // column stays put.
  const acts = isOpen ? [] : actions(q, st, selected)
  if (!acts.length) {
    segments.push(text(cutContext(context, cols.slot), 'dim'))
  } else if (cols.ctx + GAP + segWidth(acts) <= cols.slot) {
    segments.push(text(pad(cutContext(context, cols.ctx), cols.ctx), 'dim'), text(' '.repeat(GAP)), ...acts)
  } else {
    segments.push(...fitStatus(acts, cols.slot))
  }
  const rows = [{ key: 'row-' + key, segments: fitRow(segments, st.width), selected }]
  if (!isOpen) return rows

  // Open: the whole title, the details, and the buttons on their own line.
  const room = Math.max(10, st.width - INDENT)
  const line = (k, segs) => rows.push({ key: 'row-' + key + '-' + k, segments: fitRow([text(' '.repeat(INDENT)), ...segs], st.width), selected })
  if (shownTitle !== title) wrap(title, room).forEach((l, j) => line('title-' + j, [text(l, titleStyle)]))
  const project = findProject(st.board, q.project)
  const owner = q.owner || (project?.orchestrators || [])[0]?.agent || ''
  const orch = (project?.orchestrators || []).find((o) => o.agent === owner)
  // The row shows the project and attention already when its context is full.
  const detail = [
    ...(cols.full ? [] : [q.project, q.attention_msg]),
    activeSaga(project)?.title,
    owner ? 'owner ' + owner : 'no owner',
    orch?.host,
    orch ? (orch.busy ? 'busy' : orch.online === false ? 'offline' : 'online') : '',
    q.channel,
  ].filter(Boolean)
  wrapParts(detail, room).forEach((l, j) => line('detail-' + j, [text(l, 'dim')]))
  const all = fitStatus(actions(q, st, !q.closed), room)
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

/**
 * Fit a status and its buttons in `room`: the buttons stay whole, and the
 * status (the first, plain segment) gives way, first the owner of an ask
 * (`✓ asked at 10:41`; the open row names it), then the button labels to
 * one word (`a: Ask`), then the status's tail.
 */
function fitStatus(segs, room) {
  if (segWidth(segs) <= room || !segs.length || segs[0].button) return segs
  // An armed press drops its "any other key cancels" first.
  if (segs[0].style === 'key') return segs.slice(0, 2)
  let first = { ...segs[0], text: segs[0].text.replace(/^✓ asked \S+ at /, '✓ asked at ') }
  let rest = segs.slice(1)
  if (segWidth([first, ...rest]) > room) rest = rest.map(shortButton)
  const over = segWidth([first, ...rest]) - room
  if (over > 0) first = { ...first, text: truncate(first.text, Math.max(4, width(first.text) - over)) }
  return [first, ...rest]
}

/** A Done or Ask button with its label cut to one word. */
function shortButton(s) {
  if (!s.button) return s
  const b = s.button
  return button(b.key, b.kind, [text(b.parts.map((p) => p.text).join('').split(' ')[0], 'plain')], { hotkey: b.hotkey, arg: b.arg })
}

/** Join parts with ` · ` into lines of at most `w` cells, breaking between parts. */
function wrapParts(parts, w) {
  const out = []
  let line = ''
  for (const part of parts) {
    const next = line ? line + ' · ' + part : part
    if (width(next) <= w) {
      line = next
      continue
    }
    if (line) out.push(line)
    const pieces = wrap(part, w)
    line = pieces.pop() || ''
    out.push(...pieces)
  }
  if (line) out.push(line)
  return out
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

/** The count kinds, in their column order, as words and as glyphs. */
const COUNTS = [
  { kind: 'running', word: (n) => n + ' running', glyph: null, style: 'plain' },
  { kind: 'blocked', word: (n) => n + ' blocked', glyph: (n) => GLYPH.blocked + n, style: 'red' },
  { kind: 'needs', word: (n) => n + ' needs you', glyph: (n) => GLYPH.needs + n, style: 'yellow' },
]

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
    counts: { running: s.running, blocked: s.blocked, needs: s.needs },
    orch: orch ? [orch.agent, ...(orch.busy ? ['busy'] : [])] : ['no orchestrator'],
  }
}

/** The text of one count, as a word or a glyph count; '' for none. */
function countText(c, n, words) {
  if (!n) return ''
  const f = words ? c.word : c.glyph
  return f ? f(n) : ''
}

/**
 * The count slots: one per kind any row has, each as wide as its widest
 * text, so a kind always sits in the same place and a zero leaves a blank.
 */
function countSlots(cells, words) {
  return COUNTS.map((c) => ({ ...c, width: Math.max(0, ...cells.map((cell) => width(countText(c, cell.counts[c.kind], words)))) })).filter(
    (slot) => slot.width > 0,
  )
}

const slotsWidth = (slots) => slots.reduce((n, s, i) => n + s.width + (i ? GAP : 0), 0)

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
  const orch = most((c) => width(c.orch.join(' · ')))
  const ratio = most((c) => (c.plain ? 0 : width(c.done)))
  const steps = [
    { orch: true, words: true, saga: true, bar: 10 },
    { orch: false, words: true, saga: true, bar: 10 },
    { orch: false, words: false, saga: true, bar: 10 },
    { orch: false, words: false, saga: false, bar: 10 },
    { orch: false, words: false, saga: false, bar: 5 },
  ]
  for (const step of steps) {
    const slots = countSlots(cells, step.words)
    const counts = slotsWidth(slots)
    const sagaMin = step.saga ? Math.min(saga, 20) : 0
    const epicMin = Math.min(epic, 26)
    const orchMin = step.orch ? Math.min(orch, 12) : 0
    const barWidth = step.bar + 1 + ratio
    const fixed = CHIP + name + GAP + (step.saga ? GAP : 0) + GAP + barWidth + (counts ? GAP + counts : 0) + (step.orch ? GAP : 0)
    let room = w - fixed - sagaMin - epicMin - orchMin
    if (room < 0) continue
    // The room goes to the epic, then the orchestrator's name, then the saga
    // (an open row shows the saga whole); what is left goes to the last column.
    const e = epicMin + Math.min(room, epic - epicMin)
    room -= e - epicMin
    const o = step.orch ? orchMin + Math.min(room, orch - orchMin) : 0
    room -= o - orchMin
    const s = step.saga ? sagaMin + Math.min(room, saga - sagaMin) : 0
    room -= s - sagaMin
    return { ...step, name, saga: s, epic: e, orch: step.orch ? o + room : 0, ratio, counts, slots }
  }
  // Even the smallest step does not fit: the name gives way too.
  const slots = countSlots(cells, false)
  const counts = slotsWidth(slots)
  const left = w - (CHIP + GAP + GAP + 6 + ratio + (counts ? GAP + counts : 0))
  const epicWidth = Math.max(8, Math.min(epic, Math.floor(left / 2)))
  return { orch: false, words: false, saga: 0, bar: 5, name: Math.max(6, left - epicWidth), epic: epicWidth, ratio, counts, slots }
}

/** Parts joined with ` · `, whole parts dropped from the end to fit `w`. */
function cutParts(parts, w) {
  for (let n = parts.length; n > 0; n -= 1) {
    const joinedText = parts.slice(0, n).join(' · ')
    if (width(joinedText) <= w) return joinedText
  }
  return truncate(parts[0] || '', w)
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
    segments.push(text(' '.repeat(GAP)))
    cols.slots.forEach((slot, j) => {
      if (j) segments.push(text(' '.repeat(GAP)))
      segments.push(text(pad(countText(slot, cells.counts[slot.kind], cols.words), slot.width), slot.style))
    })
  }
  if (cols.orch) segments.push(text(' '.repeat(GAP)), text(cutParts(cells.orch, cols.orch), 'dim'))
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
  wrapParts(about, room).forEach((l, j) => line('orch-' + j, [text(l, 'dim')]))
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
  // The epics flow across lines, four cells apart; one too long is cut. A
  // project of plain tasks has no epic, and its row already counts them.
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
 * ring, armed, fresh, unavailable, reason, questions, pending, asked, now }`:
 * `selected` and `open` are chip keys, `questions` the needs-you items as
 * drawn (with the ones a Done just closed). With room for one row it is the
 * condensed row.
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
  const base = { ...state, board, width: w, questions, rowCount, selected, focused: Boolean(state.focused), at: stampOf(board?.at) }
  const st = { ...base, selectedButtons: buttonsOf(questions.find((q) => needsKey(q) === selected), base) }
  const rows = [{ key: 'title', segments: titleRow(board, st), selected: false }]
  if (rowCount === 0) {
    rows.push({ key: 'empty', segments: fitRow([text('   no project has open work', 'dim')], w), selected: false })
    return rows
  }
  const cells = projects.map(projectCells)
  const cols = projectColumns(cells, w)
  if (questions.length) {
    const needCols = needsColumns(questions, w, st.at, projects.length ? projectEdges(cols) : [])
    questions.forEach((q, i) => rows.push(...needsRows(q, i, { ...st, ordinal: () => i + 1 }, needCols)))
  }
  projects.forEach((p, i) =>
    rows.push(...projectRows(p, i, { ...st, ordinal: () => questions.length + i + 1 }, cells[i], cols)),
  )
  return rows
}

/** The chip keys of the band's rows, top to bottom: the rows j and k walk. */
export function bandKeys(board, questions) {
  return [...(questions || allQuestions(board)).map(needsKey), ...activeProjects(board).map(projectKey)]
}
