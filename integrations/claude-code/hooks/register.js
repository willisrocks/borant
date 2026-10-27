// borant's chief-of-staff mod, v0.0.1. It draws the board above the prompt,
// a pane with Needs you and Projects, a toast when something new needs the
// person, and the `/cos` digest. Only a chief session sees any of it: the
// role comes from BORANT_ROLE, which is how a chief is started (see
// crates/borant/src/hook.rs, session_start). Every other session registers
// no timer, no command, and draws nothing.
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
  paneHelpRows,
  paneNeedsRows,
  paneProjectRows,
  paneTabs,
  text as seg,
} from '../mod/board.js'
import { readBoard, sourceMode } from '../mod/source.js'

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
let updated = ''
let lastReadAt = 0
/** The needs-you items the last drawing saw; a new one raises a toast. */
let seenNeeds = null
let tab = 'needs'
let selectedProject = null
let selectedQuestion = 0
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
        return tree($, e, [[seg('cos  ', 'dim'), seg('board unavailable', 'faint')]], 'band')
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
      const questions = allQuestions(board)
      const rows = [paneTabs(tab, questions)]
      rows.push([seg('')])
      if (tab === 'needs') {
        if (questions.length === 0) rows.push([seg('  nothing waits on you', 'green')])
        else rows.push(...paneNeedsRows(questions, { width, selectedQuestion }))
        rows.push([seg('')])
        rows.push([seg('  Done tells the project orchestrator. Nothing else waits on you.', 'dim')])
      } else if (tab === 'help') {
        rows.push(...paneHelpRows({ width, rows: rowCount() }))
      } else {
        if (!board) rows.push([seg('  board unavailable', 'faint')])
        else rows.push(...paneProjectRows(board, { width, selectedProject }))
      }
      return tree($, e, rows, 'pane')
    } catch {
      return next(e)
    }
  })

  on('command.run', { command: 'cos' }, async ($, e) => {
    try {
      if (!isChief) return { text: 'cos: the chief-of-staff mod is off in this session (BORANT_ROLE is not chief)' }
      await refresh($, true)
      const args = String(e.args || '').trim()
      if (args === 'help') return { text: helpText() }
      if (args === 'pane') return openPane($)
      if (!board) return { text: 'cos: board unavailable' }
      return { text: digest(board, { project: args || null, width: columns || 96 }) }
    } catch {
      return { text: 'cos: board unavailable' }
    }
  })

  // `?` alone is the help key. A band hotkey is one digit or one lowercase
  // letter, so the band's `?` reads the prompt instead: Enter after it opens
  // the help view and no turn runs.
  on('prompt.submit', async ($, e, next) => {
    try {
      if (isChief && String(e.text || '').trim() === '?') {
        await openPane($, 'help')
        return { drop: 'cos: the help view is in the pane' }
      }
    } catch {
      // A prompt the chief typed always goes on.
    }
    return next(e)
  })

  // A turn starts when a Borant message arrives, so the board catches up then.
  on('turn.start', async ($, e, next) => {
    try {
      if (isChief) {
        await refresh($)
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
        .then(() => $.ui.invalidate('ui.render'))
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
    unavailable = board === null
    return
  }
  board = result.board
  updated = hhmm(board.at, updated)
  unavailable = false
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

async function openPane($, view = null) {
  if (view) tab = view
  // The help view explains the board without one, so it opens on its own.
  if (!board && tab !== 'help') return { text: 'cos: board unavailable' }
  try {
    const opened = await $.ui.open({ id: PANE, title: PANE_TITLE, focus: true, closeOnEscape: true })
    if (opened && opened.isPlaced === false) {
      return { text: 'cos: the pane needs a wider terminal (144 columns)' }
    }
    $.ui.invalidate('ui.render')
    return {}
  } catch {
    return { text: 'cos: the pane did not open' }
  }
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

/** A press on a cell of the band or the pane. */
function press($, button) {
  switch (button.kind) {
    case 'needs':
      selectedQuestion = 0
      return openPane($, 'needs')
    case 'project':
      selectedProject = button.arg
      return openPane($, 'projects')
    case 'help':
      return openPane($, 'help')
    case 'tab-needs':
      tab = 'needs'
      return redraw($)
    case 'tab-projects':
      tab = 'projects'
      return redraw($)
    case 'tab-help':
      tab = 'help'
      return redraw($)
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

