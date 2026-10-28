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
  bandRowKeys,
  bandRows,
  allQuestions,
  activeProjects,
  digest,
  helpLines,
  hhmm,
  paneTabs,
  stepKey,
  text as seg,
  truncate,
} from '../mod/board.js'
import {
  SELECTED,
  TABS,
  breadcrumb,
  channelsFor,
  findProject,
  rowsFor,
  questionKey,
  viewKey,
} from '../mod/nav.js'
import { readBoard, readTasks, sourceMode } from '../mod/source.js'
import { DELEGATION_RULE, guardReason, projectWork } from '../mod/guard.js'

/** The pane's id, and its tab label. */
const PANE = 'cos'
const PANE_TITLE = 'Chief of staff'
/** The band's render instance, as its ui.focus events name it. */
const BAND = 'above-prompt'
/** How often the board is read while the session is the chief: the fallback
 * for changes the event stream does not carry, such as a new task. */
const REFRESH_MS = 5000
/** A read arriving sooner than this after the last one is skipped. */
const DEBOUNCE_MS = 1000
/** An event is read this long after it arrives, so a burst costs one read. */
const PUSH_DELAY_MS = 300
/** A dropped event stream starts again after this long. */
const EVENTS_RETRY_MS = 5000
/** A Done that the hub has not closed by then shows as still open. A test
 * shortens it with BORANT_COS_DONE_WAIT_S. */
const DONE_WAIT_MS = 120000
let doneWaitMs = DONE_WAIT_MS
/** How many times a band press asks for the pane's keyboard. */
const FOCUS_TRIES = 3

/** Set to `chief` by the role check, and nothing else ever turns the mod on. */
let isChief = false
let config = null
let board = null
let unavailable = false
/** The reason of the last failed read, for a person to see. */
let reason = ''
let updated = ''
let lastReadAt = 0
/** When the last read succeeded, by the session clock, for the header's age. */
let lastOkAt = null
/** Done presses by `<channel>/<task>`: `{ state, at, title, reason }`. */
let pending = {}
/** Ask presses, by question: `{ state: 'sending' | 'sent' | 'failed', owner }`. */
let asked = {}
/**
 * The rule is owed to the conversation: a session that was running when the
 * mod loaded keeps the system prompt it started with, and a compaction can
 * drop what it was told. The first turn after either gets the rule as a row.
 */
let ruleOwed = false
/** A read the event stream asked for is waiting on its timer. */
let pushQueued = false
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
/** The band Button that holds the keyboard, so its whole row draws selected. */
let bandKey = null
/** True for the frame that hands the band's keyboard to the pane. */
let bandHidden = false
/** True while the pane holds the keyboard, as its last drawing said. */
let paneFocused = false
/** The row a mouse click just focused, so the click selects instead of opening. */
let clickMark = null
/** The terminal's width, as last measured while drawing the band. */
let columns = 0

export function register(on) {
  on('session.start', async ($, e, next) => {
    try {
      await ensureStarted($)
    } catch {
      // A mod must never keep a session from starting.
    }
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    try {
      await ensureStarted($)
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
      const rows = bandRows(board, { width, maxRows: e.props?.maxRows, updated: await age($) })
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
      paneFocused = Boolean(e.props && e.props.isFocused)
      return paneBody($, e, width)
    } catch {
      return next(e)
    }
  })

  // The band's ring names the row Button it is on. The whole row draws
  // selected, not only the `[ n ]` chip, and the row clears when the ring
  // leaves the band's elements.
  on('ui.focus', { requestId: BAND }, async ($, e, next) => {
    try {
      if (isChief) {
        const key = typeof e.element === 'string' ? e.element : null
        if (key !== bandKey) {
          bandKey = key
          redraw($)
        }
      }
    } catch {
      // A focus move is never worth an error.
    }
    return next(e)
  })

  // The engine moves the focus ring with Tab, the arrows, and a click; a row
  // taking it is the selection, and a click that is about to press the same
  // row is marked so it selects rather than opens.
  on('ui.focus', { requestId: PANE }, async ($, e, next) => {
    try {
      if (isChief && bandKey) {
        // The band no longer has the keys, so its row is not selected; its
        // normal colors read better than a frozen highlight.
        bandKey = null
        redraw($)
      }
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
      if (e.id === PANE) {
        paneOpen = false
        if (bandKey) {
          bandKey = null
          redraw($)
        }
      }
    } catch {
      // Fall through: the pane closes.
    }
    return next(e)
  })

  on('command.run', { command: 'cos' }, async ($, e) => {
    await ensureStarted($).catch(() => {})
    try {
      if (!isChief) return { text: 'cos: the chief-of-staff mod is off in this session (BORANT_ROLE is not chief)' }
      // A command runs from the prompt, which now has the keys.
      if (bandKey) {
        bandKey = null
        redraw($)
      }
      await refresh($, true)
      const args = String(e.args || '').trim()
      if (args === 'help') return { text: helpText() }
      if (args === 'pane') return openPane($)
      if (!board) return { text: 'cos: board unavailable' + (reason ? ' \u00b7 ' + reason : '') }
      // A docked pane can take up to 45% of the terminal; keep the digest
      // inside the transcript column it leaves, so it never wraps.
      const width = Math.max(48, Math.floor((columns || 96) * 0.55) - 1)
      return { text: digest(board, { project: args || null, width }) }
    } catch {
      return { text: 'cos: board unavailable' + (reason ? ' \u00b7 ' + reason : '') }
    }
  })

  // Typing means the prompt has the keys; the band's ring is no longer there.
  on('prompt.edit', async ($, e, next) => {
    try {
      if (isChief && bandKey) {
        bandKey = null
        redraw($)
      }
    } catch {
      // An edit is never worth an error.
    }
    return next(e)
  })

  // The chief's rule, in the system prompt, where compaction cannot drop it
  // the way it drops a skill. A reload of the mod runs session.start again,
  // so a long session gets it too.
  on('prompt.compose', async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    const out = await next(e)
    if (!isChief) return out
    return { ...out, sections: [...out.sections, { id: 'borant:chief-delegation', text: DELEGATION_RULE, scope: 'session' }] }
  })

  // Project work that reaches the chief's shell goes to the person as a
  // permission prompt, in any mode. Reads, and the Borant CLI, pass.
  on('tool.check', { tool: 'Bash' }, async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    const verdict = await next(e)
    if (!isChief || verdict.decision === 'deny') return verdict
    const why = projectWork(e.input)
    return why ? { decision: 'ask', reason: guardReason(why) } : verdict
  })

  on('session.compact', async ($, e, next) => {
    const out = await next(e)
    if (isChief && !out?.skip) ruleOwed = true
    return out
  })

  // A turn starts when a Borant message arrives, so the board catches up then.
  on('turn.start', async ($, e, next) => {
    try {
      await ensureStarted($)
      if (isChief && ruleOwed && e.agentId === undefined) {
        ruleOwed = false
        await tell($, DELEGATION_RULE)
      }
    } catch {
      // The system prompt still carries the rule.
    }
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

/**
 * Starts the mod once per load. A reload does not always raise session.start
 * (`/reload-plugins` loads the module and goes on), so the hooks a chief needs
 * at once start it too.
 */
let started = false
async function ensureStarted($) {
  if (started) return
  started = true
  await start($)
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
  ruleOwed = true

  let mode = 'cli'
  try {
    mode = sourceMode(await $.env.get('BORANT_COS_BOARD'))
  } catch {
    mode = 'cli'
  }
  config = { mode }

  try {
    const wait = Number(await $.env.get('BORANT_COS_DONE_WAIT_S'))
    if (Number.isFinite(wait) && wait > 0) doneWaitMs = wait * 1000
  } catch {
    doneWaitMs = DONE_WAIT_MS
  }

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
  if (mode === 'cli') followEvents($)
}

/**
 * Follow the host's event stream, and read the board soon after each event
 * that can change it: a task's status, a lease, an agent, a spawn, a move.
 * The stream ends with the module; a stream that drops starts again.
 */
function followEvents($) {
  void (async () => {
    for (;;) {
      try {
        const stream = $.process.spawn({ argv: ['borant', 'events', '--follow', '--json'] })
        let rest = ''
        for await (const piece of stream) {
          if (!piece || piece.stream !== 'stdout') continue
          const lines = (rest + piece.text).split('\n')
          rest = lines.pop() || ''
          if (lines.some(changesBoard)) pushRefresh($)
        }
      } catch {
        // No borant, or an old one: the timer still reads the board.
      }
      try {
        await $.clock.sleep(EVENTS_RETRY_MS)
      } catch {
        return
      }
    }
  })()
}

/** Whether one line of `borant events --json` can change what the board shows. */
function changesBoard(line) {
  let kind = ''
  try {
    kind = String(JSON.parse(line).kind || '')
  } catch {
    return false
  }
  return /^(task|lease|agent|spawn|move|channel)\./.test(kind)
}

/** One read, shortly after a burst of events. */
function pushRefresh($) {
  if (pushQueued) return
  pushQueued = true
  try {
    $.clock.after(PUSH_DELAY_MS, () => {
      pushQueued = false
      refresh($, true)
        .then(async () => {
          await refreshTasks($)
          redraw($)
        })
        .catch(() => {})
    })
  } catch {
    pushQueued = false
  }
}

/** The header's "updated" text: how long ago the last read succeeded. */
async function age($) {
  let now = 0
  try {
    now = await $.clock.now()
  } catch {
    return updated
  }
  if (lastOkAt === null || now < lastOkAt) return updated
  const seconds = Math.floor((now - lastOkAt) / 1000)
  if (seconds < 5) return 'just now'
  if (seconds < 60) return seconds + 's ago'
  if (seconds < 3600) return Math.floor(seconds / 60) + 'm ago'
  return updated
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
  lastOkAt = now
  unavailable = false
  reason = ''
  announceNew($)
  settlePending($, now)
}

/**
 * A pending Done whose item left the board is done: the hub closed the task
 * (or answered it), so its mark goes. One still there after DONE_WAIT_MS
 * shows as still open, and its Done button comes back.
 */
function settlePending($, now) {
  const open = new Set(allQuestions(board).map(questionKey))
  for (const [key, mark] of Object.entries(pending)) {
    if (!open.has(key)) {
      delete pending[key]
      if (mark.state === 'pending') toast($, 'closed: ' + mark.title)
    } else if (mark.state === 'pending' && now - mark.at >= doneWaitMs) {
      const reason = 'not closed within ' + Math.round(doneWaitMs / 1000) + ' s'
      pending[key] = { ...mark, state: 'failed', reason }
      toast($, 'still open (' + reason + '): ' + mark.title)
    }
  }
}

function toast($, text) {
  try {
    $.ui.toast(text)
  } catch {
    // A toast is never worth an error.
  }
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
    pending,
    asked,
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

/**
 * Open the pane from a band press. The band holds the keyboard while its
 * Button runs, and the engine refuses a pane's `focus` then. Draw the band
 * without its controls for one frame, so the keys fall back to the empty
 * composer, then open the pane for real and draw the controls again.
 */
async function openFromBand($) {
  const answer = await openPane($)
  try {
    $.clock.after(50, () => {
      handoff($).catch(() => {})
    })
  } catch {
    // No clock: the pane opens without the keyboard, and ctrl+x tab reaches it.
  }
  return answer
}

/**
 * Drop the band's keys, focus the pane, then draw the band's controls again.
 * A slow frame can leave the band holding the keys when the pane asks, and
 * the engine then opens the pane without them: ask again until the pane's
 * drawing says it has the keyboard.
 */
async function handoff($) {
  try {
    bandHidden = true
    redraw($)
    for (let tries = 0; tries < FOCUS_TRIES; tries += 1) {
      await $.clock.sleep(50 + tries * 100)
      if (!(await takePaneFocus($))) break
      await $.clock.sleep(100)
      if (paneFocused) break
    }
    await focusRow($, selectionOf())
  } finally {
    bandHidden = false
    redraw($)
  }
}

/** The band's 0: the pane takes the keyboard, opened where it was if closed. */
async function focusPane($) {
  if (!paneOpen) return openFromBand($)
  try {
    $.clock.after(0, () => {
      handoff($).catch(() => {})
    })
  } catch {
    // No clock: ctrl+x tab reaches the pane.
  }
  return redraw($)
}

/**
 * The band's j and k: move the band's ring through its row chips, the
 * needs-you row and one per project, as Tab does, wrapping at the ends.
 */
async function bandMove($, delta) {
  const keys = bandRowKeys(bandRows(board, { width: columns || 120 }))
  // With no move recorded, the ring is where autoFocus put it: the first row.
  const key = stepKey(keys, bandKey || keys[0], delta)
  if (!key) return
  try {
    // The mod's own move does not come back through its ui.focus hook, so
    // the row it lands on is recorded here, and the band draws it selected.
    const moved = await $.ui.focus({ requestId: BAND, key })
    if (!moved || !moved.deny) {
      bandKey = key
      redraw($)
    }
  } catch {
    // The band does not hold the keys.
  }
}

/** Ask the engine for the pane's keyboard, and say whether it took. */
async function takePaneFocus($) {
  try {
    const title = breadcrumb(path)
    const opened = await $.ui.open({ id: PANE, title, focus: true, closeOnEscape: true })
    if (opened && opened.isPlaced === false) return false
    paneOpen = true
    paneTitle = title
    return true
  } catch {
    return false
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
  return (allQuestions(board).length ? 1 : 0) + activeProjects(board).length
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
  // ctrl+x tab lands the band's ring on its first row, so the focus shows at
  // once and j moves on from there.
  const first = bandRowKeys(rows)[0]
  const element = (s, key, selected) => {
    if (s.button && bandHidden) return Text({ key, children: [s.text], ...styleProps(s.style) })
    if (s.button) {
      const props = { key: s.button.key, label: s.button.label, onPress: () => press($, s.button) }
      if (s.button.hotkey) props.hotkey = s.button.hotkey
      if (s.button.key === first) props.autoFocus = true
      if (s.button.plain) {
        props.plain = true
        props.dimColor = !selected
      }
      return Button(props)
    }
    const props = { key, children: [s.text], ...styleProps(s.style) }
    if (selected) {
      // On the selection background every cell takes the inverse text color,
      // so dim and colored cells stay readable. Chips keep their background,
      // and the bar keeps its green fill. Bold goes: many terminals draw bold
      // black as grey, which reads grey on grey.
      if (!String(s.text).startsWith('\u2588')) props.color = 'inverseText'
      props.dimColor = false
      props.bold = false
    }
    return Text(props)
  }
  return Box({
    flexDirection: 'column',
    children: rows.map((row, i) => {
      // The row's chip names the row. A row with the keyboard draws on the
      // selected background, not only the chip the engine's ring marks.
      const chip = row.find((s) => s.button && (s.button.kind === 'project' || s.button.kind === 'needs'))
      const selected = Boolean(chip && bandKey === chip.button.key)
      return Box({
        key: prefix + '-row-' + i,
        flexDirection: 'row',
        ...(selected ? { backgroundColor: 'promptBorder' } : {}),
        children: row.map((s, j) => element(s, prefix + '-' + i + '-' + j, selected)),
      })
    }),
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
          const props = { key: s.button.key, label: s.button.label, onPress: () => press($, s.button) }
          // The open tab is the primary Button; the others are dim.
          if (s.button.active) props.variant = 'primary'
          else props.dimColor = true
          if (s.button.plain) props.plain = true
          return Button(props)
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
    // The selection mark takes the gap's one cell, so the row's text stays put.
    cells.push(
      Box({
        key: r.selectable ? 'gap-' + r.key : 'mark-gap-' + i,
        children: [Text({ children: [r.selected ? SELECTED : ' '], ...(r.selected ? styleProps('select') : {}) })],
      }),
    )
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
    // A row of buttons is the buttons alone: its blank text line would put a
    // row between a question and its own buttons.
    if (!r.buttons.length) children.push(Box({ key: 'pane-line-' + i, flexDirection: 'row', children: cells }))
    if (r.buttons.length) {
      children.push(
        Box({
          key: 'pane-buttons-' + i,
          flexDirection: 'row',
          columnGap: 2,
          ...(r.buttonIndent ? { paddingLeft: r.buttonIndent } : {}),
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
  return paneTabs(tab, allQuestions(board), width)
}

/**
 * The footer: the keys as clickable Buttons, so the mouse can move too, and
 * a line of the other keys under them, cut rather than wrapped in a narrow
 * pane.
 */
function footer($, e) {
  const { Box, Text, Button } = $.ui.resolve(e)
  return Box({
    key: 'pane-foot',
    flexDirection: 'column',
    children: [
      Box({
        key: 'pane-foot-keys',
        flexDirection: 'row',
        columnGap: 2,
        children: [
          Button({ key: 'nav-down', label: '\u2193', hotkey: 'j', plain: true, onPress: () => move($, 1) }),
          Button({ key: 'nav-up', label: '\u2191', hotkey: 'k', plain: true, onPress: () => move($, -1) }),
          Button({ key: 'nav-left', label: '\u2190', hotkey: 'h', plain: true, onPress: () => left($) }),
          Button({ key: 'nav-right', label: '\u2192', hotkey: 'l', plain: true, onPress: () => right($) }),
          // esc cannot be a Button hotkey, so its label names the key the way
          // the hotkeys draw theirs.
          Button({ key: 'nav-back', label: 'esc: back', plain: true, onPress: () => escLike($) }),
        ],
      }),
      Text({
        key: 'pane-foot-hint',
        children: ['enter open \u00b7 ctrl+x \u2190\u2192 or drag the edge: width'],
        dimColor: true,
        wrap: 'truncate',
      }),
    ],
  })
}

/** A press on a cell of the band or the pane. */
function press($, button) {
  switch (button.kind) {
    case 'needs':
      tab = 'needs'
      path = []
      return openFromBand($)
    case 'project':
      tab = 'projects'
      path = [{ kind: 'project', project: button.arg }]
      return openFromBand($)
    case 'help':
      tab = 'help'
      path = []
      return openFromBand($)
    case 'band-down':
      return bandMove($, 1)
    case 'band-up':
      return bandMove($, -1)
    case 'band-pane':
      return focusPane($)
    case 'tab-needs':
      return selectTab($, 'needs')
    case 'tab-projects':
      return selectTab($, 'projects')
    case 'tab-help':
      return selectTab($, 'help')
    case 'done':
      return markDone($, allQuestions(board)[button.arg])
    case 'ask':
      return askOwner($, allQuestions(board)[button.arg])
    default:
      return redraw($)
  }
}

/**
 * Tells the chief something without a turn: a user-role row the person does
 * not see as typed, which the model reads with its next request.
 */
async function tell($, text) {
  try {
    await $.session.append({ message: { type: 'user', content: [{ type: 'text', text: '[borant chief mod] ' + text }] } })
  } catch {
    // Nothing to tell it with; the board still shows the ask.
  }
}

/**
 * Ask orchestrator: the mod sends the question to the owner of its channel
 * itself, as the person, and the chief is only told, with a row and no turn.
 * A prompt would have the chief answer it in its own session.
 */
async function askOwner($, q) {
  if (!q) return redraw($)
  const owner = q.owner || (findProject(board, q.project)?.orchestrators || [])[0]?.agent
  const key = questionKey(q)
  if (!owner) {
    asked[key] = { state: 'failed', owner: '' }
    toast($, 'ask: no orchestrator owns ' + q.channel)
    return redraw($)
  }
  asked[key] = { state: 'sending', owner }
  redraw($)
  const text =
    'Chris asks about ' + q.title + ' (' + q.channel + '/' + q.task + '), from the chief\'s board: ' +
    'what does it need from him, and what do you recommend? Answer the chief, who relays it to Chris.'
  let ok = false
  let why = ''
  try {
    const run = await $.process.run(['borant', 'send', owner, text], { timeoutMs: 8000 })
    ok = run.exitCode === 0
    why = String(run.stderr || '').trim().split('\n')[0]
  } catch (err) {
    why = String(err?.message || err)
  }
  asked[key] = { state: ok ? 'sent' : 'failed', owner }
  if (ok) {
    await tell(
      $,
      'The person pressed "Ask orchestrator" on ' + q.title + ' (' + q.channel + '/' + q.task + '). ' +
        'The board sent the question to ' + owner + ' as a direct message. Do not answer it yourself; relay ' + owner + '\'s reply.',
    )
    toast($, 'asked ' + owner + ': ' + truncate(q.title, 40))
  } else {
    toast($, 'ask failed' + (why ? ' (' + truncate(why, 40) + ')' : '') + ': ' + truncate(q.title, 40))
  }
  return redraw($)
}

/**
 * Done: the row shows "marking done…" at once, and the chief gets a prompt
 * that says how to close the task. The hub lets only the channel's owner, a
 * person, or (for a saga gate) an approver close it as done, so the chief
 * closes a gate itself and asks the owner for anything else. The mark goes
 * when the item leaves the board, and turns into "still open" after
 * DONE_WAIT_MS.
 */
async function markDone($, q) {
  if (!q) return redraw($)
  let now = 0
  try {
    now = await $.clock.now()
  } catch {
    now = 0
  }
  const key = questionKey(q)
  pending[key] = { state: 'pending', at: now, title: q.title }
  const text =
    'Chris marked done: ' + q.title + ' (' + q.channel + '/' + q.task + '). ' +
    'Close it as done now: task_close it yourself if it is a saga gate or you own its channel; ' +
    'otherwise send the owner of ' + q.channel + ' a direct message to close it as done, with "Chris marked it done in the board".'
  // The prompt submits when the session is idle; a press must not wait.
  try {
    $.prompt.submit({ text }).catch(() => {
      pending[key] = { ...pending[key], state: 'failed', reason: 'the prompt was not sent' }
      redraw($)
    })
  } catch {
    pending[key] = { ...pending[key], state: 'failed', reason: 'the prompt was not sent' }
  }
  return redraw($)
}

function redraw($) {
  try {
    $.ui.invalidate('ui.render')
  } catch {
    // Nothing to redraw without a surface.
  }
}
