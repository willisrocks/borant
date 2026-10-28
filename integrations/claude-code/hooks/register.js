// borant's chief-of-staff mod, v0.0.1. It draws the board above the prompt,
// a pane with Needs you, Projects, and Help, a toast when something new needs
// the person, and the `/cos` digest. Only a chief session sees any of it: the
// role comes from BORANT_ROLE, which is how a chief is started (see
// crates/borant/src/hook.rs, session_start). Every other session registers
// no timer, no command, and draws nothing.
//
// The pane navigates the same way everywhere. The engine's own focus ring is
// the selection: Tab and the arrows move it, j and k move it through our row
// Buttons with `$.ui.focus`, Enter presses the focused row Button, Esc reaches
// us as a refused `ui.close`, and h and l fold, drill, and switch tabs. A row
// is one Button, so a click selects it and a second click opens it.
//
// Shapes and rules come from docs/superpowers/specs/2026-10-26-sagas-and-epics-design.md
// and the approved mockup in docs/superpowers/specs/2026-10-27-chief-mod-mockup/.

import {
  bandRows,
  allQuestions,
  allProjects,
  digest,
  helpLines,
  hhmm,
  paneTabs,
  text as seg,
  truncate,
} from '../mod/board.js'
import {
  TABS,
  breadcrumb,
  channelsFor,
  rowsFor,
  viewKey,
} from '../mod/nav.js'
import { readBoard, readTasks, sourceMode } from '../mod/source.js'

/** The pane's id, and its tab label. */
const PANE = 'cos'
const PANE_TITLE = 'Chief of staff'
/** How often the board is read while the session is the chief. */
const REFRESH_MS = 15000
/** A read arriving sooner than this after the last one is skipped. */
const DEBOUNCE_MS = 5000

/** Set to `chief` by the role check, and nothing else ever turns the mod on. */
let isChief = false
let config = null
let board = null
let unavailable = false
/** The reason of the last failed read, for a person to see. */
let reason = ''
let updated = ''
let lastReadAt = 0
/** The needs-you items the last drawing saw; a new one raises a toast. */
let seenNeeds = null
/** The open tab, and where drilling in has gone. */
let tab = 'needs'
let path = []
/** Which nodes are folded, and which row each view's selection sits on. */
let folds = {}
let selections = new Map()
/** One channel's tasks, as `borant tasks` printed them, or null on failure. */
let taskCache = new Map()
/** The pane is open, at this title, and the width it last drew at. */
let paneOpen = false
let paneTitle = ''
let paneWidth = 60
/** The row a mouse click just focused, so the click selects instead of opening. */
let clickMark = null
/** The terminal's width, as last measured while drawing the band. */
let columns = 0

export function register(on) {
  on('session.start', async ($, e, next) => {
    try {
      await start($)
    } catch {
      // A mod must never keep a session from starting.
    }
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    try {
      if (!isChief) return next(e)
      measure(e)
      if (!board) {
        if (!unavailable) return next(e)
        const width = widthOf(e)
        const room = Math.max(8, width - 'cos  board unavailable'.length - 4)
        const detail = reason ? ' \u00b7 ' + truncate(reason, room) : ''
        const row = [seg('cos  ', 'dim'), seg('board unavailable', 'faint'), seg(detail, 'dim')]
        return tree($, e, [row], 'band')
      }
      const width = widthOf(e)
      const rows = bandRows(board, { width, maxRows: e.props?.maxRows, updated })
      return tree($, e, rows, 'band')
    } catch {
      return next(e)
    }
  })

  on('ui.render', { component: 'Pane' }, async ($, e, next) => {
    try {
      if (!isChief || e.requestId !== PANE) return next(e)
      measure(e)
      const width = widthOf(e)
      paneWidth = width
      return paneBody($, e, width)
    } catch {
      return next(e)
    }
  })

  // The engine moves the focus ring with Tab, the arrows, and a click; a row
  // taking it is the selection, and a click that is about to press the same
  // row is marked so it selects rather than opens.
  on('ui.focus', { requestId: PANE }, async ($, e, next) => {
    try {
      if (isChief && typeof e.element === 'string') {
        const key = e.element
        if (key.startsWith('row-')) {
          const ordinal = Number(key.slice(4))
          if (Number.isFinite(ordinal)) {
            const wasSelected = selectionOf() === ordinal
            if (e.origin && e.origin.kind === 'person') {
              const mark = { key, wasSelected }
              clickMark = mark
              try {
                $.clock.after(0, () => {
                  if (clickMark === mark) clickMark = null
                })
              } catch {
                clickMark = null
              }
            }
            setSelection(ordinal)
            redraw($)
          }
        }
      }
    } catch {
      // A focus move is never worth an error.
    }
    return next(e)
  })

  // Esc (with closeOnEscape) and ctrl+x x raise this. Deeper than the root it
  // goes back a level and keeps the pane open; at the root it closes.
  on('ui.close', async ($, e, next) => {
    try {
      if (isChief && e.id === PANE && e.origin && e.origin.kind === 'person' && path.length) {
        await back($)
        return { deny: 'back one level' }
      }
      if (e.id === PANE) paneOpen = false
    } catch {
      // Fall through: the pane closes.
    }
    return next(e)
  })

  on('command.run', { command: 'cos' }, async ($, e) => {
    try {
      if (!isChief) return { text: 'cos: the chief-of-staff mod is off in this session (BORANT_ROLE is not chief)' }
      await refresh($, true)
      const args = String(e.args || '').trim()
      if (args === 'help') return { text: helpText() }
      if (args === 'pane') return openPane($)
      if (!board) return { text: 'cos: board unavailable' + (reason ? ' \u00b7 ' + reason : '') }
      return { text: digest(board, { project: args || null, width: columns || 96 }) }
    } catch {
      return { text: 'cos: board unavailable' + (reason ? ' \u00b7 ' + reason : '') }
    }
  })

  // A turn starts when a Borant message arrives, so the board catches up then.
  on('turn.start', async ($, e, next) => {
    try {
      if (isChief) {
        await refresh($)
        await refreshTasks($)
        $.ui.invalidate('ui.render')
      }
    } catch {
      // Quiet: a refresh never disturbs a turn.
    }
    return next(e)
  })
}

/** The one cheap role check. A chief is started with BORANT_ROLE=chief. */
async function start($) {
  let role
  try {
    role = await $.env.get('BORANT_ROLE')
  } catch {
    role = undefined
  }
  if (role !== 'chief') return
  isChief = true

  let mode = 'cli'
  try {
    mode = sourceMode(await $.env.get('BORANT_COS_BOARD'))
  } catch {
    mode = 'cli'
  }
  config = { mode }

  try {
    await $.command.register({
      name: 'cos',
      description: 'The chief-of-staff board: /cos, /cos <project>, /cos pane, or /cos help',
      argumentHint: '[help|pane|project]',
      immediate: true,
    })
  } catch {
    // A taken name only costs the command, not the band.
  }

  try {
    $.clock.every(REFRESH_MS, () => {
      refresh($)
        .then(async () => {
          await refreshTasks($)
          $.ui.invalidate('ui.render')
        })
        .catch(() => {})
    })
  } catch {
    // Without a timer the board still refreshes on each turn.
  }

  await refresh($, true)
}

/** Read the board and note what is new. Never throws. */
async function refresh($, force = false) {
  if (!isChief || !config) return
  // The mock clock of a test controls this; the timer and the turn hook share it.
  let now = 0
  try {
    now = await $.clock.now()
  } catch {
    now = 0
  }
  if (!force && now - lastReadAt < DEBOUNCE_MS) return
  lastReadAt = now
  const result = await readBoard($, config)
  if (!result.board) {
    // A failed read draws an empty board with the reason, never the stale one:
    // a session that refuses to read as the person must say so.
    board = null
    unavailable = true
    reason = result.reason || ''
    return
  }
  board = result.board
  updated = hhmm(board.at, updated)
  unavailable = false
  reason = ''
  announceNew($)
}

/** A toast when the board shows a needs-you item that was not there before. */
function announceNew($) {
  const questions = allQuestions(board)
  const keys = new Set(questions.map((q) => String(q.channel) + '/' + String(q.task)))
  if (seenNeeds) {
    const fresh = questions.find((q) => !seenNeeds.has(String(q.channel) + '/' + String(q.task)))
    if (fresh) {
      try {
        $.ui.toast('needs you: ' + fresh.title)
      } catch {
        // A toast is never worth an error.
      }
    }
  }
  seenNeeds = keys
}

/** The view's name, which keys its remembered selection. */
function currentKey() {
  return viewKey({ tab, path })
}

function selectionOf() {
  const value = selections.get(currentKey())
  return typeof value === 'number' ? value : 0
}

function setSelection(ordinal) {
  selections.set(currentKey(), Math.max(0, Number(ordinal) || 0))
}

/** The task lists of the cache, by channel: an array, or null on failure. */
function taskLists() {
  const out = {}
  for (const [channel, entry] of taskCache) out[channel] = entry.list
  return out
}

/** The current view's rows, as mod/nav.js builds them. */
function rowsNow() {
  return rowsFor({
    tab,
    path,
    folds,
    board,
    tasks: taskLists(),
    questions: allQuestions(board),
    help: helpLines({ rows: rowCount() }),
    selection: selectionOf(),
    width: paneWidth,
  })
}

function selectableRows(rows) {
  return rows.filter((r) => r.selectable)
}

function rowAt(rows, ordinal) {
  return selectableRows(rows)[ordinal] || null
}

/** Read a view's channels, if the cache does not hold them yet. Never throws. */
async function ensureTasks($, channels) {
  for (const channel of channels) {
    if (taskCache.has(channel)) continue
    const result = await readTasks($, config, channel)
    taskCache.set(channel, { list: result.tasks, reason: result.reason })
  }
}

/** Read again the channels of the open view, on the board's tick. */
async function refreshTasks($) {
  if (!isChief || !config || !paneOpen || path.length === 0) return
  for (const channel of channelsFor({ path, board })) {
    const result = await readTasks($, config, channel)
    taskCache.set(channel, { list: result.tasks, reason: result.reason })
  }
}

/** Read the tasks the view needs and draw it. */
async function land($) {
  await ensureTasks($, channelsFor({ path, board }))
  await setTitle($)
  redraw($)
  await focusRow($, selectionOf())
}

async function setTitle($) {
  if (!paneOpen) return
  const title = breadcrumb(path)
  if (title === paneTitle) return
  try {
    await $.ui.open({ id: PANE, title, closeOnEscape: true })
    paneTitle = title
  } catch {
    // The title is cosmetic.
  }
}

async function openPane($) {
  // The help view explains the board without one, so it opens on its own.
  if (!board && tab !== 'help') return { text: 'cos: board unavailable' }
  try {
    const title = breadcrumb(path)
    const opened = await $.ui.open({ id: PANE, title, focus: true, closeOnEscape: true })
    if (opened && opened.isPlaced === false) {
      return { text: 'cos: the pane needs a wider terminal (144 columns)' }
    }
    paneOpen = true
    paneTitle = title
    await land($)
    return {}
  } catch {
    return { text: 'cos: the pane did not open' }
  }
}

/** Move the selection one row, as j, k, and the arrows do. */
async function move($, delta) {
  const rows = rowsNow()
  const count = selectableRows(rows).length
  if (count === 0) return
  const current = Math.min(selectionOf(), count - 1)
  const next = Math.min(count - 1, Math.max(0, current + delta))
  setSelection(next)
  redraw($)
  await focusRow($, next)
}

/** Land the engine's focus ring and the pane's window on one row. */
async function focusRow($, ordinal) {
  try {
    await $.ui.focus({ requestId: PANE, key: 'row-' + ordinal })
  } catch {
    // A row not drawn yet, or a pane without the keys.
  }
  try {
    await $.ui.scroll({ to: { key: 'row-' + ordinal }, in: PANE, block: 'nearest' })
  } catch {
    // A drawing that fits needs no scroll.
  }
}

/** h and the left arrow: fold an open node, else go back, else the last tab. */
async function left($) {
  const row = rowAt(rowsNow(), selectionOf())
  if (row && row.open && row.open.kind === 'fold' && row.open.isOpen) return toggleFold($, row)
  if (path.length) return back($)
  return switchTab($, -1)
}

/** l and the right arrow: unfold a closed node, else open it, else the next tab. */
async function right($) {
  const row = rowAt(rowsNow(), selectionOf())
  if (row && row.open && row.open.kind === 'fold' && !row.open.isOpen) return toggleFold($, row)
  if (row && row.drill) return drill($, row.drill)
  return switchTab($, 1)
}

/** A tab button, or a band row that opens one: the tab from its root. */
async function selectTab($, name) {
  tab = name
  path = []
  redraw($)
  await setTitle($)
  await ensureTasks($, channelsFor({ path, board }))
  await focusRow($, selectionOf())
}

async function toggleFold($, row) {
  folds[row.open.key] = !row.open.isOpen
  const count = selectableRows(rowsNow()).length
  if (count) setSelection(Math.min(selectionOf(), count - 1))
  redraw($)
  await focusRow($, selectionOf())
}

async function switchTab($, delta) {
  const index = Math.max(0, TABS.indexOf(tab))
  tab = TABS[(index + delta + TABS.length) % TABS.length]
  path = []
  redraw($)
  await setTitle($)
  await focusRow($, Math.min(selectionOf(), Math.max(0, selectableRows(rowsNow()).length - 1)))
}

/** Drill into a project, an epic, or a task. */
async function drill($, node) {
  if (!node) return
  path = [...path, node]
  setSelection(0)
  redraw($)
  await land($)
}

/** Go back one level, out of a drilled-in view, or out of a tab. */
async function back($) {
  path = path.slice(0, -1)
  redraw($)
  await land($)
}

/** Esc, and the footer's esc button: back a level, then close at the root. */
async function escLike($) {
  if (path.length) return back($)
  paneOpen = false
  try {
    await $.ui.close({ id: PANE })
  } catch {
    // A pane that is already gone needs nothing.
  }
  redraw($)
}

/** The ▸ or ▾ beside a row: open a row that drills in, or fold a group. */
async function pressOpen($, row) {
  if (row.drill) return drill($, row.drill)
  if (row.open.kind === 'fold') return toggleFold($, row)
  return redraw($)
}

/**
 * A press on a row: the first one selects it, and pressing it again opens it.
 * Enter drills a project, an epic, or a task in; h and l fold a group. A click
 * focuses the row just before its press, so that first press only selects.
 */
async function pressRow($, row) {
  const ordinal = Number(String(row.key).slice(4))
  if (!Number.isFinite(ordinal)) return
  const activate = async () => {
    if (row.drill) return drill($, row.drill)
    if (row.open && row.open.kind === 'fold') return toggleFold($, row)
    return redraw($)
  }
  const mark = clickMark
  const clicked = mark && mark.key === row.key
  if (clicked) clickMark = null
  const wasSelected = selectionOf() === ordinal
  if (clicked ? mark.wasSelected : wasSelected) return activate()
  setSelection(ordinal)
  redraw($)
  await focusRow($, ordinal)
}

/** The rows the band numbers: the needs-you row, then one per project. */
function rowCount() {
  return (allQuestions(board).length ? 1 : 0) + allProjects(board).length
}

/** The help view as the transcript shows it, the same text as the pane tab. */
function helpText() {
  return helpLines({ rows: rowCount() }).join('\n')
}

function measure(e) {
  const c = e.viewport && e.viewport.columns
  if (typeof c === 'number' && c > 0) columns = c
}

function widthOf(e) {
  const body = e.props && e.props.bodyColumns
  if (typeof body === 'number' && body > 0) return body
  if (columns > 0) return columns
  return 96
}

/** The Text props of one segment's style. */
function styleProps(style) {
  switch (style) {
    case 'dim':
    case 'faint':
    case 'waiting':
      return { dimColor: true }
    case 'dimBold':
      return { dimColor: true, bold: true }
    case 'bold':
      return { bold: true }
    case 'select':
      return { color: 'cyan', bold: true }
    case 'white':
      return { color: 'white', bold: true }
    case 'yellow':
      return { color: 'yellow' }
    case 'yellowBold':
      return { color: 'yellow', bold: true }
    case 'red':
      return { color: 'red' }
    case 'green':
    case 'done':
      return { color: 'green' }
    case 'doneDim':
      return { color: 'green', dimColor: true }
    case 'blue':
    case 'running':
      return { color: 'blue' }
    case 'magenta':
    case 'scheduled':
      return { color: 'magenta' }
    case 'blocked':
      return { color: 'red' }
    case 'needs':
      return { color: 'yellow' }
    case 'badgeBlue':
      return { backgroundColor: 'blue', color: 'black', bold: true }
    case 'badgeMagenta':
      return { backgroundColor: 'magenta', color: 'black', bold: true }
    case 'hotkey':
      return { bold: true }
    default:
      return {}
  }
}

/** The band: rows of segments, a Button where a segment names one. */
function tree($, e, rows, prefix) {
  const { Box, Text, Button } = $.ui.resolve(e)
  const element = (s, key) => {
    if (s.button) {
      const props = { key: s.button.key, label: s.button.label, onPress: () => press($, s.button) }
      if (s.button.hotkey) props.hotkey = s.button.hotkey
      return Button(props)
    }
    return Text({ key, children: [s.text], ...styleProps(s.style) })
  }
  return Box({
    flexDirection: 'column',
    children: rows.map((row, i) =>
      Box({
        key: prefix + '-row-' + i,
        flexDirection: 'row',
        children: row.map((s, j) => element(s, prefix + '-' + i + '-' + j)),
      }),
    ),
  })
}

/** The pane: its tabs, a breadcrumb, the view's rows, and the key footer. */
function paneBody($, e, width) {
  const { Box, Text, Button, Link } = $.ui.resolve(e)
  const children = []
  children.push(
    Box({
      key: 'pane-tabs',
      flexDirection: 'row',
      children: tabCells($, e, width).map((s, i) => {
        if (s.button) {
          return Button({
            key: s.button.key,
            label: s.button.label,
            onPress: () => press($, s.button),
          })
        }
        return Text({ key: 'tab-gap-' + i, children: [s.text], ...styleProps(s.style) })
      }),
    }),
  )
  if (path.length) {
    children.push(Text({ key: 'pane-crumb', children: ['  ' + breadcrumb(path)], dimColor: true }))
  }
  children.push(Text({ key: 'pane-blank', children: [' '] }))

  rowsNow().forEach((r, i) => {
    const cells = []
    if (r.open) {
      cells.push(
        Button({
          key: 'open-' + i,
          label: r.marker === ' ' ? '\u25b8' : r.marker,
          plain: true,
          dimColor: true,
          onPress: () => pressOpen($, r),
        }),
      )
    } else {
      cells.push(Text({ key: 'mark-' + i, children: [r.marker] }))
    }
    cells.push(Text({ key: 'mark-gap-' + i, children: [' '] }))
    if (r.selectable) {
      const props = {
        key: r.key,
        plain: true,
        children: r.segments.map((s) =>
          Text({ children: [s.text], wrap: 'truncate', ...styleProps(s.style) }),
        ),
        onPress: () => pressRow($, r),
      }
      if (selectionOf() === Number(String(r.key).slice(4))) props.autoFocus = true
      cells.push(Button(props))
    } else if (r.link) {
      for (const [j, s] of r.segments.entries()) {
        cells.push(Text({ key: 'pane-row-' + i + '-' + j, children: [s.text], ...styleProps(s.style) }))
      }
      cells.push(Link({ key: 'link-' + i, href: r.link.href, label: r.link.label }))
    } else {
      cells.push(
        Box({
          flexDirection: 'row',
          children: r.segments.map((s, j) =>
            Text({ key: 'pane-row-' + i + '-' + j, children: [s.text], wrap: 'truncate', ...styleProps(s.style) }),
          ),
        }),
      )
    }
    children.push(Box({ key: 'pane-line-' + i, flexDirection: 'row', children: cells }))
    if (r.buttons.length) {
      children.push(
        Box({
          key: 'pane-buttons-' + i,
          flexDirection: 'row',
          columnGap: 2,
          children: r.buttons.map((b) =>
            Button({ key: b.key, label: b.label, onPress: () => press($, b) }),
          ),
        }),
      )
    }
  })

  children.push(Text({ key: 'pane-foot-blank', children: [' '] }))
  children.push(footer($, e))
  return Box({ flexDirection: 'column', children })
}

/** The pane's tab row, fitted to the pane's width. */
function tabCells($, e, width) {
  const cells = paneTabs(tab, allQuestions(board))
  const out = []
  let used = 0
  for (const cell of cells) {
    const room = Math.max(0, width - used)
    if (room === 0) break
    const length = Array.from(cell.text).length
    if (length <= room) {
      out.push(cell)
      used += length
    } else {
      out.push({ ...cell, text: Array.from(cell.text).slice(0, room).join('') })
      used = width
    }
  }
  return out
}

/** The footer: the keys as clickable Buttons, so the mouse can move too. */
function footer($, e) {
  const { Box, Text, Button } = $.ui.resolve(e)
  return Box({
    key: 'pane-foot',
    flexDirection: 'row',
    columnGap: 2,
    children: [
      Button({ key: 'nav-down', label: '\u2193', hotkey: 'j', plain: true, onPress: () => move($, 1) }),
      Button({ key: 'nav-up', label: '\u2191', hotkey: 'k', plain: true, onPress: () => move($, -1) }),
      Button({ key: 'nav-left', label: '\u2190', hotkey: 'h', plain: true, onPress: () => left($) }),
      Button({ key: 'nav-right', label: '\u2192', hotkey: 'l', plain: true, onPress: () => right($) }),
      Button({ key: 'nav-back', label: 'esc', plain: true, dimColor: true, onPress: () => escLike($) }),
      Text({ children: ['enter open \u00b7 tab next \u00b7 esc back'], dimColor: true }),
    ],
  })
}

/** A press on a cell of the band or the pane. */
function press($, button) {
  switch (button.kind) {
    case 'needs':
      tab = 'needs'
      path = []
      return openPane($)
    case 'project':
      tab = 'projects'
      path = [{ kind: 'project', project: button.arg }]
      return openPane($)
    case 'help':
      tab = 'help'
      path = []
      return openPane($)
    case 'tab-needs':
      return selectTab($, 'needs')
    case 'tab-projects':
      return selectTab($, 'projects')
    case 'tab-help':
      return selectTab($, 'help')
    case 'done':
    case 'ask': {
      const q = allQuestions(board)[button.arg]
      if (!q) return redraw($)
      const text =
        button.kind === 'done'
          ? 'Chris marked done: ' + q.title + ' (' + q.channel + '/' + q.task + ')'
          : 'Chris asks the orchestrator: ' + q.title + ' (' + q.channel + '/' + q.task + ')'
      // The prompt submits when the session is idle; a press must not wait.
      try {
        $.prompt.submit({ text }).catch(() => {})
      } catch {
        // Refused only while a turn is stopped; the item stays on the board.
      }
      return redraw($)
    }
    default:
      return redraw($)
  }
}

function redraw($) {
  try {
    $.ui.invalidate('ui.render')
  } catch {
    // Nothing to redraw without a surface.
  }
}
