// borant's chief-of-staff mod. It draws the board above the prompt, a preview
// of the selected row, a toast when something new needs the person, and the
// `/cos` digest. Only a chief session sees any of it: the role comes from
// BORANT_ROLE, which is how a chief is started (see
// crates/borant/src/hook.rs, session_start). Every other session registers no
// timer, no command, and draws nothing.
//
// The band drives the preview. A digit at the empty prompt selects a row and
// opens the preview pane with the keyboard; j and k move the band's
// selection and the preview follows; d and a act on the selected item; Enter
// shows more; Esc gives the keys back to the prompt and the preview stays.
// A bare digit cannot give the band itself the keyboard (the engine gives
// that only to ctrl+x tab or a click), so the preview pane carries the same
// keys: wherever the keys are, they do the same thing.
//
// The digit reaches the mod two ways. From 144 columns the prompt.edit hook
// takes it and opens the preview. Below 144 the engine places only a pane
// that a press or a command asked for, so there each band row's chip carries
// its digit as a hotkey, and the press opens the preview.
//
// Docked (a wide terminal) the preview is a tall card beside the transcript;
// inline (a narrow one) it is a compact block over the band, open only while
// it has the keys.

import {
  allQuestions,
  bandActions,
  bandItems,
  bandRows,
  digest,
  helpLines,
  itemActions,
  text as seg,
  truncate,
  hhmm,
} from '../mod/board.js'
import { channelsFor, helpPreview, previewLines, questionKey, findProject } from '../mod/preview.js'
import { readBoard, readTasks, sourceMode } from '../mod/source.js'
import { DELEGATION_RULE, guardReason, projectWork } from '../mod/guard.js'
import { CLOSED_SHOW_MS, canDone, freshness, pressStatus, pressStatusShort, withClosed } from '../mod/status.js'

/** The preview pane's id and title. */
const PANE = 'cos'
const PANE_TITLE = 'chief of staff'
/** The selection's background, in the band and in the preview: the theme's
 * user-message grey, darker than promptBorder, so yellow titles, colored
 * glyphs and blue keys keep their colors and stay readable on it. */
const SELECTED_BG = 'userMessageBackground'
/** Below this many terminal columns the engine places only an asked pane (a
 * command, a prompt, or a press behind it). A digit taken by prompt.edit is
 * none of those, so there the band's chips keep the digits as hotkeys. */
const ASKED_COLUMNS = 144
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
/** An inline preview that lost the keys closes after this long. */
const INLINE_IDLE_MS = 400
/** "↻ reading" shows at least this long, so a fast read is still seen. */
const READING_SHOWN_MS = 600
/** The band's ring goes back to the selected row this long after a redraw:
 * the engine keeps the ring by its place in the band, not by its element. */
const RING_SETTLE_MS = 80
/** What the chief reads when a tool in a Done's turn would ask the person. */
const DONE_TURN_REASON =
  'Chief mod: refused. This turn comes from a Done the person pressed on the board, and they are not at the prompt to approve tools. ' +
  'Close the task with the borant MCP tools (task_close, or send to its owner). If that does not work, say so in one line; do not ask the person to allow anything.'

/** Set to `chief` by the role check, and nothing else ever turns the mod on. */
let isChief = false
let config = null
let board = null
let unavailable = false
/** The reason of the last failed read, for a person to see. */
let reason = ''
let lastReadAt = 0
/** When the last read succeeded, by the session clock, for the header's age. */
let lastOkAt = null
/** Done presses by `<channel>/<task>`, as mod/status.js names their states. */
let pending = {}
/** Ask presses, by question: `{ state: 'sending' | 'sent' | 'failed', owner, time, reason }`. */
let asked = {}
/** A board read runs now, and when the last one started (session clock). */
let reading = false
let readStartedAt = null
/** The event stream runs, so a change shows within about a second. */
let live = false
/** The chief's turn runs a Done's prompt: no tool may ask the person in it. */
let doneTurn = false
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
/** The selected band row, by its key, and its place, for when it goes. */
let selKey = null
let selIndex = 0
/** Enter's "more" in the preview, and the help view in place of the preview. */
let more = false
let helpShown = false
/** One channel's tasks, as `borant tasks` printed them, or null on failure. */
let taskCache = new Map()
/** The preview pane: open, where it sits, whether it holds the keys, its width. */
let paneOpen = false
let placement = null
let paneFocused = false
let paneWidth = 60
/** The band holds the keys (after ctrl+x tab or a click). */
let bandFocused = false
/** The terminal, as last measured while drawing the band. */
let columns = 0
let fullscreen = true
/** The session clock as of the last draw, for the drawing's ages. */
let clockNow = 0

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
      clockNow = await nowMs($)
      const width = widthOf(e)
      if (!board) {
        if (!unavailable) return next(e)
        const label = width >= 110 ? 'chief of staff' : 'cos'
        const room = Math.max(8, width - label.length - 31)
        const row = [seg('── ', 'faint'), seg(label, 'bold'), seg(' ─ ', 'faint'), await fresh($)]
        if (reason) row.push(seg(' · ' + truncate(reason, room), 'dim'))
        return tree($, e, [{ key: 'band-title', cells: row }])
      }
      const cur = current()
      if (!cur.item && paneOpen && !helpShown) closeSoon($, () => !current().item && !helpShown)
      const rows = bandRows(board, {
        width,
        maxRows: e.props?.maxRows,
        questions: drawnQuestions(),
        selected: cur.item ? cur.item.key : null,
        active: active(),
        fill: active(),
        reserveActions: docked() || bandFocused,
        actionsFor: (item) => bandActionsFor(item, cur.item, docked() || bandFocused),
        fresh: [await fresh($)],
        digitKeys: !promptDigits(),
      })
      return tree($, e, rows)
    } catch {
      return next(e)
    }
  })

  on('ui.render', { component: 'Pane' }, async ($, e, next) => {
    try {
      if (!isChief || e.requestId !== PANE) return next(e)
      measure(e)
      clockNow = await nowMs($)
      const props = e.props || {}
      // A drawing of the pane means it is open, also after the module reloads.
      paneOpen = true
      if (board && !helpShown && !current().item) closeSoon($, () => !current().item && !helpShown)
      const focused = Boolean(props.isFocused)
      const place = props.placement || null
      if (focused !== paneFocused || place !== placement) {
        paneFocused = focused
        placement = place
        // The band's selection and hint follow where the keys are.
        redrawSoon($)
      }
      paneWidth = typeof props.bodyColumns === 'number' && props.bodyColumns > 0 ? props.bodyColumns : 60
      if (place === 'inline' && !focused && !bandFocused) {
        // Inline, the preview is the band's while the keys are on the board.
        closeSoon($, () => placement === 'inline' && !paneFocused && !bandFocused)
      }
      return paneBody($, e, await paneLines($), focused)
    } catch {
      return next(e)
    }
  })

  // The band's ring: it holds the keys while the ring is on one of our
  // elements, and lets them go when the ring leaves (Esc, or the prompt).
  on('ui.focus', { requestId: BAND }, async ($, e, next) => {
    try {
      if (isChief) {
        const has = typeof e.element === 'string'
        if (has !== bandFocused) {
          bandFocused = has
          redraw($)
          // ctrl+x tab puts the ring on the band's first element, which may be
          // a key of the title row: move it to the selected row, so Enter acts
          // on that row.
          if (has) ringToSelected($)
        }
      }
    } catch {
      // A focus move is never worth an error.
    }
    return next(e)
  })

  // ctrl+x x, or the close mark: the preview goes, and "more" with it.
  on('ui.close', async ($, e, next) => {
    try {
      if (e.id === PANE) {
        paneOpen = false
        paneFocused = false
        placement = null
        more = false
        helpShown = false
        redraw($)
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
      if (bandFocused) {
        bandFocused = false
        redraw($)
      }
      await refresh($, true)
      const args = String(e.args || '').trim()
      if (args === 'help') return { text: helpText() }
      if (args === 'pane') {
        if (!board) return { text: 'cos: board unavailable' }
        helpShown = false
        return (await openPane($, true)) ? {} : { text: 'cos: the preview did not open' }
      }
      if (!board) return { text: 'cos: board unavailable' + (reason ? ' · ' + reason : '') }
      // A docked pane can take up to 45% of the terminal; keep the digest
      // inside the transcript column it leaves, so it never wraps.
      const width = Math.max(48, Math.floor((columns || 96) * 0.55) - 1)
      return { text: digest(board, { project: args || null, width }) }
    } catch {
      return { text: 'cos: board unavailable' + (reason ? ' · ' + reason : '') }
    }
  })

  // From 144 columns a digit at an empty prompt is the board's: it selects its
  // row and opens the preview with the keys, and never lands in the prompt. A
  // paste, a key with a modifier, and a digit with no row go on as typing.
  // Below 144 the band's chips carry the digits as hotkeys instead.
  // Typing means the prompt has the keys; the board no longer has them.
  on('prompt.edit', async ($, e, next) => {
    try {
      if (isChief && promptDigits() && e.text === '' && e.key && !e.key.ctrl && !e.key.meta && /^[1-9]$/.test(e.inputText || '')) {
        const item = items()[Number(e.inputText) - 1]
        if (item) {
          // The open starts inside this hook, while the prompt holds the keys
          // over an empty composer, so the preview can take them.
          select(item.key)
          redraw($)
          openPane($, true).catch(() => {})
          return { text: e.text, cursor: e.cursor }
        }
      }
      if (isChief && (bandFocused || paneFocused)) {
        bandFocused = false
        paneFocused = false
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

  // Project work that reaches the chief's shell is refused, in any mode, with
  // a reason the model reads; the person gets no prompt. Reads, and the
  // Borant CLI, pass. In the turn a Done press started, any call that would
  // ask the person is refused too: they pressed a key on the board and are
  // not at the prompt to approve it.
  on('tool.check', async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    const verdict = await next(e)
    if (!isChief || verdict.decision === 'deny') return verdict
    const why = e.tool === 'Bash' ? projectWork(e.input) : null
    if (why) return { decision: 'deny', reason: guardReason(why) }
    if (doneTurn && verdict.decision === 'ask' && e.agentId === undefined) return { decision: 'deny', reason: DONE_TURN_REASON }
    return verdict
  })

  on('turn.complete', async ($, e, next) => {
    const out = await next(e)
    if (e.agentId === undefined) doneTurn = false
    return out
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
 *
 * A spawn lives as long as the hook that starts it: one started in a render
 * or session hook ends when that hook returns. So the stream starts from a
 * timer, and one that ends or is refused starts again from a timer.
 */
function followEvents($) {
  try {
    $.clock.after(0, () => {
      streamEvents($).catch(() => {})
    })
  } catch {
    // Without a timer the 5 s poll still reads the board.
  }
}

async function streamEvents($) {
  try {
    const stream = $.process.spawn({ argv: ['borant', 'events', '--follow', '--json'] })
    live = true
    redraw($)
    let rest = ''
    for await (const piece of stream) {
      if (!piece || piece.stream !== 'stdout') continue
      const lines = (rest + piece.text).split('\n')
      rest = lines.pop() || ''
      if (lines.some(changesBoard)) pushRefresh($)
    }
  } catch {
    // No borant, or an old one: the 5 s poll still reads the board.
  }
  live = false
  redraw($)
  try {
    $.clock.after(EVENTS_RETRY_MS, () => {
      streamEvents($).catch(() => {})
    })
  } catch {
    // The 5 s poll still reads the board.
  }
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

/** The title row's freshness word: live, reading, polling, stale, or unavailable. */
async function fresh($) {
  let now = null
  try {
    now = await $.clock.now()
  } catch {
    now = null
  }
  const ageMs = now === null || lastOkAt === null ? null : Math.max(0, now - lastOkAt)
  const shown = reading || (now !== null && readStartedAt !== null && now - readStartedAt < READING_SHOWN_MS)
  return freshness({ unavailable: unavailable && !board, reading: shown, live, ageMs })
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
  reading = true
  readStartedAt = now
  redraw($)
  let result
  try {
    result = await readBoard($, config)
  } finally {
    reading = false
    // The word stays READING_SHOWN_MS from the start; one redraw then takes it off.
    try {
      $.clock.after(READING_SHOWN_MS, () => redraw($))
    } catch {
      // Without a clock the next drawing takes it off.
    }
  }
  if (!result.board) {
    // A failed read draws an empty board with the reason, never the stale one:
    // a session that refuses to read as the person must say so.
    board = null
    unavailable = true
    reason = result.reason || ''
    return
  }
  board = result.board
  lastOkAt = now
  unavailable = false
  reason = ''
  announceNew($)
  settlePending($, now)
}

/**
 * A Done whose item left the board landed: the hub closed the task (or
 * answered it). Its row stays CLOSED_SHOW_MS as closed, then goes. One the
 * hub has not closed DONE_WAIT_MS after the prompt landed shows as still open,
 * and its Done button comes back.
 */
function settlePending($, now) {
  const open = new Set(allQuestions(board).map(questionKey))
  for (const [key, mark] of Object.entries(pending)) {
    if (mark.state === 'closed') {
      if (now - mark.closedAt >= CLOSED_SHOW_MS) delete pending[key]
    } else if (!open.has(key)) {
      if (mark.state === 'unsent') {
        delete pending[key]
      } else {
        pending[key] = { ...mark, state: 'closed', closedAt: now }
        toast($, 'closed: ' + mark.title)
      }
    } else if (mark.state === 'closing' && now - mark.at >= doneWaitMs) {
      pending[key] = { ...mark, state: 'failed', waitMs: doneWaitMs, reason: 'not closed within ' + Math.round(doneWaitMs / 1000) + ' s' }
      toast($, 'still open: ' + mark.title)
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


/** The needs-you items as drawn: the board's, with the ones a Done just closed. */
function drawnQuestions() {
  return withClosed(allQuestions(board), pending, clockNow)
}

function items() {
  return board ? bandItems(board, drawnQuestions()) : []
}

/** The selected row: the one recorded, or the row now at its place when it went. */
function current() {
  const list = items()
  if (!list.length) return { item: null, index: -1, count: 0 }
  let index = list.findIndex((i) => i.key === selKey)
  if (index < 0) {
    index = Math.max(0, Math.min(selIndex, list.length - 1))
    selKey = list[index].key
  }
  selIndex = index
  return { item: list[index], index, count: list.length }
}

/** Whether the board has the keys: the preview pane, or the band itself. */
function active() {
  return paneFocused || bandFocused
}

/** Whether a digit at the empty prompt reaches prompt.edit: from 144 columns. */
function promptDigits() {
  return columns >= ASKED_COLUMNS
}

/** Whether the preview docks beside the transcript, or sits inline. */
function docked() {
  if (placement) return placement === 'dock'
  return fullscreen && columns >= 110
}

/**
 * The cells after a needs-you item's columns: its Done or Ask status, and the
 * Done and Ask buttons on the selected item when `buttons` (a hotkey is one
 * per site, so only one item has them).
 */
function actionsFor(item, selected, buttons, askLabel = 'Ask orchestrator', statusCap = Infinity) {
  if (!item || item.kind !== 'needs') return []
  const key = questionKey(item.q)
  const mark = pending[key]
  const status = pressStatus(mark, asked[key], clockNow)
  const mine = Boolean(buttons && selected && selected.key === item.key && !item.q.closed && active())
  return itemActions(status, canDone(mark) && !item.q.closed, mark, mine, askLabel, statusCap)
}

/** A band row's buttons or short status, in the row's fixed slot. */
function bandActionsFor(item, selected, buttons) {
  if (!item || item.kind !== 'needs') return []
  const key = questionKey(item.q)
  const mark = pending[key]
  const mine = Boolean(buttons && selected && selected.key === item.key && !item.q.closed && active())
  return bandActions(pressStatusShort(mark, asked[key], clockNow), canDone(mark) && !item.q.closed, mark, mine)
}

function select(key) {
  const list = items()
  const index = list.findIndex((i) => i.key === key)
  if (index < 0) return
  if (key !== selKey) more = false
  selKey = key
  selIndex = index
  helpShown = false
}

/** A row's digit or a click on it: select it and show it; the selected row again shows more. */
async function chipPress($, key) {
  if (active() && key === selKey && !helpShown) return toggleMore($)
  select(key)
  redraw($)
  await openPane($, true)
}

/** j and k: the next or the previous row, wrapping at the ends. */
async function move($, delta) {
  const list = items()
  if (!list.length) return
  const { index } = current()
  select(list[(index + delta + list.length) % list.length].key)
  redraw($)
  // With the keys in the band (ctrl+x tab), a docked preview opens beside it
  // and follows; it does not take the keys.
  if (bandFocused && !paneOpen && docked()) await openPane($, false)
  // The ring follows, so Enter in the band acts on the selected row.
  if (bandFocused) ringToSelected($)
  if (more) await ensureTasks($)
}

/**
 * Put the band's ring on the selected row's chip once the band has drawn: the
 * engine keeps the ring by its place in the band, and a redraw that adds or
 * drops a row's Done and Ask moves every chip after them. A mod's own focus
 * move does not come back through its ui.focus hook.
 */
function ringToSelected($) {
  try {
    $.clock.after(RING_SETTLE_MS, () => {
      const { item } = current()
      if (!bandFocused || !item) return
      $.ui.focus({ requestId: BAND, key: item.key }).catch(() => {})
    })
  } catch {
    // Without a clock the ring stays where the engine put it.
  }
}

/** Enter: the preview's "more", read on first use. */
async function toggleMore($) {
  helpShown = false
  more = !more
  redraw($)
  if (!paneOpen) await openPane($, false)
  if (more) {
    await ensureTasks($)
    redraw($)
  }
}

/** The help view in the preview. */
async function showHelp($) {
  helpShown = true
  redraw($)
  await openPane($, true)
}

/**
 * Open (or retitle) the preview. With `focus` it takes the keys: a press at
 * the empty prompt leaves them with the prompt, which can hand them on.
 */
async function openPane($, focus) {
  try {
    const rows = Math.max(3, paneLinesCount())
    const opened = await $.ui.open({ id: PANE, title: PANE_TITLE, rows, ...(focus ? { focus: true } : {}) })
    if (opened && opened.isPlaced === false) return false
    paneOpen = true
    return true
  } catch {
    return false
  }
}

/** Close the preview soon, if `still()` holds then. */
let closePending = false
function closeSoon($, still) {
  if (closePending) return
  closePending = true
  try {
    $.clock.after(INLINE_IDLE_MS, () => {
      closePending = false
      if (!paneOpen || !still()) return
      paneOpen = false
      placement = null
      paneFocused = false
      more = false
      $.ui.close({ id: PANE }).catch(() => {})
      redraw($)
    })
  } catch {
    // Without a clock the preview stays; ctrl+x x closes it.
    closePending = false
  }
}

/** Read the channels "more" shows for the selected row, if the cache lacks them. */
async function ensureTasks($) {
  const { item } = current()
  for (const channel of channelsFor(board, item)) {
    if (taskCache.has(channel)) continue
    const result = await readTasks($, config, channel)
    taskCache.set(channel, result.tasks)
  }
}

/** Read again the channels "more" shows, on the board's tick. */
async function refreshTasks($) {
  if (!isChief || !config || !paneOpen || !more) return
  for (const channel of channelsFor(board, current().item)) {
    const result = await readTasks($, config, channel)
    taskCache.set(channel, result.tasks)
  }
}

function taskLists() {
  const out = {}
  for (const [channel, list] of taskCache) out[channel] = list
  return out
}

/** The preview's lines, as the pane draws them now. */
async function paneLines($) {
  const width = paneWidth
  if (helpShown) return helpPreview(helpLines({ rows: items().length }), width)
  if (!board) return [[seg('board unavailable' + (reason ? ' · ' + reason : ''), 'dim')]]
  const { item } = current()
  const compact = placement === 'inline'
  // The header counts within the row's group: `needs you · 2 of 6`, `project · 3 of 9`.
  const group = items().filter((i) => item && i.kind === item.kind)
  const index = group.findIndex((i) => i.key === item?.key)
  const count = group.length
  return previewLines({
    board,
    item,
    index,
    count,
    width,
    compact,
    more,
    tasks: taskLists(),
    actions: actionsFor(item, item, true, 'Ask orchestrator', Infinity),
    fresh: [await fresh($)],
  })
}

/** How many lines the inline preview wants, with its key line. */
function paneLinesCount() {
  if (helpShown) return helpLines({ rows: items().length }).length + 1
  const { item, index, count } = current()
  if (!item) return 1
  return previewLines({ board, item, index, count, width: paneWidth, compact: true, more, tasks: taskLists(), actions: [] }).length + 1
}

/** The help as the transcript shows it, the same text as the preview's help view. */
function helpText() {
  return helpLines({ rows: items().length }).join('\n')
}

function measure(e) {
  const c = e.viewport && e.viewport.columns
  if (typeof c === 'number' && c > 0) columns = c
  if (e.viewport && typeof e.viewport.isFullscreen === 'boolean') fullscreen = e.viewport.isFullscreen
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
    case 'key':
      return { color: 'suggestion' }
    case 'yellow':
    case 'needs':
      return { color: 'yellow' }
    case 'yellowBold':
      return { color: 'yellow', bold: true }
    case 'red':
    case 'blocked':
      return { color: 'red' }
    case 'cyan':
      return { color: 'cyan' }
    case 'green':
    case 'done':
      return { color: 'green' }
    case 'blue':
    case 'running':
      return { color: 'blue' }
    default:
      return {}
  }
}

/** One cell: a plain Button where it names one, else a Text. */
function element($, e, s, key, selected = false, extra = {}) {
  const { Text, Button } = $.ui.resolve(e)
  if (s.button) {
    const b = s.button
    const props = { key: b.key, label: b.label, onPress: () => press($, b), ...extra }
    if (b.hotkey) props.hotkey = b.hotkey
    if (b.plain) props.plain = true
    // A digit that is not a hotkey draws as one does: in the accent color, a colon.
    if (b.digit) {
      props.label = 'row ' + b.digit
      props.children = [Text({ children: [b.digit + ':'], ...styleProps('key') }), ' ']
    }
    return Button(props)
  }
  const props = { key, children: [s.text], ...styleProps(s.style) }
  // On the selection background a dim cell is drawn at full strength; every
  // other cell keeps its own color, so a selected needs row stays yellow and
  // a red or blue glyph keeps its meaning. A cell with no color of its own
  // takes the theme's text color: on a row background it would draw dark.
  if (selected) {
    props.dimColor = false
    if (!props.color) props.color = 'text'
  }
  return Text(props)
}

/** The band: its rows, the selected one on the selection background. */
function tree($, e, rows) {
  const { Box } = $.ui.resolve(e)
  const selected = rows.find((r) => r.selected)
  // ctrl+x tab lands the ring on the selected row's digit, so Enter acts on it.
  const ring = selected ? selected.key : rows[1] ? rows[1].key : null
  return Box({
    flexDirection: 'column',
    children: rows.map((row, i) =>
      Box({
        key: 'band-row-' + i,
        flexDirection: 'row',
        ...(row.selected ? { backgroundColor: SELECTED_BG } : {}),
        children: row.cells.map((s, j) =>
          element($, e, s, 'band-' + i + '-' + j, row.selected, s.button && s.button.key === ring ? { autoFocus: true } : {}),
        ),
      }),
    ),
  })
}

/**
 * The preview: its key line, then its lines. The key line comes first in
 * both places because the engine keeps the pane's ring by position: when a
 * project (no Done or Ask) follows a needs-you item, a ring after the
 * buttons would land on another key, and Enter would press it. Inline the
 * key line also carries the item's Done and Ask, after the keys.
 */
function paneBody($, e, lines, focused) {
  const { Box, Text, Button } = $.ui.resolve(e)
  const inline = placement === 'inline'
  // The selected row's title lines draw on the band row's own background, so
  // the two read as one selection.
  const children = lines.map((line, i) =>
    Box({
      key: 'pane-line-' + i,
      flexDirection: 'row',
      ...(line.selected ? { backgroundColor: SELECTED_BG } : {}),
      children: line.map((s, j) => element($, e, s, 'pane-' + i + '-' + j, Boolean(line.selected))),
    }),
  )
  const label = helpShown ? '\u23ce back' : more ? '\u23ce less' : '\u23ce more'
  const { item } = current()
  const actions = inline && !helpShown ? actionsFor(item, item, true, 'Ask orchestrator', 36) : []
  const keys = Box({
    key: 'pane-keys',
    flexDirection: 'row',
    children: [
      Button({ key: 'pane-down', label: '\u2193', hotkey: 'j', plain: true, onPress: () => move($, 1) }),
      Text({ key: 'pane-keys-gap', children: [' '] }),
      Button({ key: 'pane-up', label: '\u2191', hotkey: 'k', plain: true, onPress: () => move($, -1) }),
      Text({ key: 'pane-keys-move', children: [' move \u00b7 '], dimColor: true }),
      Button({ key: 'pane-more', label, plain: true, autoFocus: true, onPress: () => (helpShown ? backFromHelp($) : toggleMore($)) }),
      Text({ key: 'pane-keys-esc', children: [' \u00b7 esc prompt'], dimColor: true }),
      ...(actions.length ? [Text({ key: 'pane-keys-sep', children: ['   '] })] : []),
      ...actions.map((s, j) => element($, e, s, 'pane-act-' + j, false)),
    ],
  })
  // The key line names keys only the pane can take: drawn while it holds them.
  if (!focused) return Box({ flexDirection: 'column', children })
  if (inline) return Box({ flexDirection: 'column', children: [keys, ...children] })
  return Box({ flexDirection: 'column', children: [keys, Text({ key: 'pane-keys-gap-line', children: [' '] }), ...children] })
}

async function backFromHelp($) {
  helpShown = false
  redraw($)
}

/** A press on a cell of the band or the preview. */
function press($, button) {
  switch (button.kind) {
    case 'select':
      return chipPress($, button.arg)
    case 'band-down':
      return move($, 1)
    case 'band-up':
      return move($, -1)
    case 'help':
      return showHelp($)
    case 'done':
      return markDone($, selectedQuestion())
    case 'ask':
      return askOwner($, selectedQuestion())
    default:
      return redraw($)
  }
}

/** The selected needs-you item, if it is one that can still be acted on. */
function selectedQuestion() {
  const { item } = current()
  return item && item.kind === 'needs' && !item.q.closed ? item.q : null
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
  asked[key] = ok ? { state: 'sent', owner, time: hhmm(await nowMs($)) } : { state: 'failed', owner, reason: truncate(why, 40) }
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
  const key = questionKey(q)
  const index = allQuestions(board).findIndex((x) => questionKey(x) === key)
  pending[key] = { state: 'sending', at: await nowMs($), title: q.title, question: q, index: Math.max(0, index) }
  const text =
    'Chris marked done: ' + q.title + ' (' + q.channel + '/' + q.task + '). ' +
    'Close it as done now, with the borant MCP tools, not the shell: task_close it yourself if it is a saga gate or you own its channel; ' +
    'otherwise send the owner of ' + q.channel + ' a direct message to close it as done, with "Chris marked it done in the board".'
  const unsent = (reason) => {
    if (pending[key] && pending[key].state === 'sending') pending[key] = { ...pending[key], state: 'unsent', reason }
    redraw($)
  }
  // The prompt submits when the session is idle and resolves once it is in; a
  // press must not wait for that.
  try {
    $.prompt
      .submit({ text })
      .then(async (out) => {
        if (out && out.drop) return unsent('the prompt was refused')
        if (pending[key] && pending[key].state === 'sending') {
          pending[key] = { ...pending[key], state: 'closing', at: await nowMs($) }
        }
        doneTurn = true
        redraw($)
      })
      .catch(() => unsent('the prompt was not sent'))
  } catch {
    unsent('the prompt was not sent')
  }
  return redraw($)
}

/** The session clock, or 0 without one. */
async function nowMs($) {
  try {
    return await $.clock.now()
  } catch {
    return 0
  }
}

function redraw($) {
  try {
    $.ui.invalidate('ui.render')
  } catch {
    // Nothing to redraw without a surface.
  }
}

/** A redraw from inside a drawing, once it is done. */
function redrawSoon($) {
  try {
    $.clock.after(0, () => redraw($))
  } catch {
    redraw($)
  }
}
