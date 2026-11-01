// borant's chief-of-staff mod. It draws a one-line ticker above the prompt,
// a pane that is the chief's inbox (what needs you, then the projects), a
// toast when something new needs the person, and the `/cos` digest. Only a
// chief session sees any of it: the role comes from BORANT_ROLE, which is how
// a chief is started (see crates/borant/src/hook.rs, session_start). Every
// other session registers no timer, no command, and draws nothing.
//
// Keys live in one place, the pane. A digit typed at an empty prompt never
// reaches the prompt: the mod takes it (prompt.edit) and opens the pane with
// the keyboard, on that ticker entry. The ticker draws no Button with a digit
// hotkey, since a bare digit at an empty prompt presses a band Button first.
// In the pane the engine's focus ring is the selection: j and k move it with
// `$.ui.focus`, Enter presses the selected row's glyph Button, d and a are
// the hotkeys of the selected card's Buttons, and Esc reaches us as a refused
// `ui.close` until the root, where it closes the pane.
//
// Shapes and rules come from docs/superpowers/specs/2026-10-26-sagas-and-epics-design.md.

import { activeProjects, allQuestions, digest, helpLines, hhmm, text as seg, ticker, truncate } from '../mod/board.js'
import { breadcrumb, cardNode, channelsFor, findProject, foldKey, questionKey, rowsFor, viewKey } from '../mod/nav.js'
import { readBoard, readTasks, sourceMode } from '../mod/source.js'
import { DELEGATION_RULE, guardReason, projectWork } from '../mod/guard.js'
import { CLOSED_SHOW_MS, freshness, withClosed } from '../mod/status.js'

/** The pane's id. */
const PANE = 'cos'
const PANE_TITLE = 'chief of staff'
/** The pane's wanted size: the dock's columns (at most 45% of the terminal),
 * and the inline block's rows below 110 columns. */
const PANE_COLUMNS = 88
const PANE_ROWS = 18
/** The selected item's background, every line of it, to the site's edge. */
const SELECT_BG = 'promptBorder'
/** Below this many terminal columns the engine places only an asked pane:
 * one a command, a prompt, or a press is behind. A digit's prompt.edit is
 * none of those, so there the ticker's entries are band Buttons instead. */
const ASKED_COLUMNS = 144
/** How many times a band press asks for the pane's keyboard. */
const FOCUS_TRIES = 3
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
/** A board read runs now. */
let reading = false
/** The event stream runs, so a change shows within about a second. */
let live = false
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
/** Where drilling in has gone: [] is the inbox, then an epic, a task, or help. */
let path = []
/** Which projects are unfolded, and which row each view's selection sits on. */
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
/** The session clock as of the last draw or read, for the drawing's ages. */
let clockNow = 0
/** The freshness segment as of the last draw. */
let freshNow = null
/** True for the frames that hand the band's keyboard to the pane. */
let bandHidden = false
/** True while the pane holds the keyboard, as its last drawing said. */
let paneFocused = false
/** A Done's prompt took the pane's keyboard; the pane asks again when the
 * chief's turn completes, as the engine does not grant it during the turn. */
let refocusOnIdle = false
/** Where the engine seated the pane: `dock` beside the transcript, or `inline`. */
let placement = 'dock'

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
      if (!board && !unavailable) return next(e)
      return band($, e, ticker(board, { width: widthOf(e), fresh: await fresh($), reason }))
    } catch {
      return next(e)
    }
  })

  on('ui.render', { component: 'Pane' }, async ($, e, next) => {
    try {
      if (!isChief || e.requestId !== PANE) return next(e)
      measure(e)
      clockNow = await nowMs($)
      freshNow = await fresh($)
      paneWidth = widthOf(e)
      paneFocused = Boolean(e.props && e.props.isFocused)
      const seat = (e.props && e.props.placement) || 'dock'
      if (seat !== placement) {
        // A resize moved the pane between the dock and the inline block; its
        // window starts at the top, so bring the selection back into view.
        placement = seat
        $.clock.after(0, () => {
          focusRow($, selectionOf()).catch(() => {})
        })
      }
      return paneBody($, e, paneWidth)
    } catch {
      return next(e)
    }
  })

  // The engine moves the focus ring with Tab and a click; a row glyph taking
  // it is the selection, and a click that is about to press the same row is
  // marked so it selects rather than opens.
  on('ui.focus', { requestId: PANE }, async ($, e, next) => {
    try {
      if (isChief && typeof e.element === 'string' && e.element.startsWith('row-')) {
        const key = e.element
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
        // Esc hands the keyboard back to the prompt as it asks to close; the
        // pane that stays asks for it again.
        await back($, true)
        return { deny: 'back one level' }
      }
      if (e.id === PANE) paneOpen = false
    } catch {
      // Fall through: the pane closes.
    }
    return next(e)
  })

  on('command.run', { command: 'cos' }, async ($, e) => {
    await ensureStarted($).catch(() => {})
    try {
      if (!isChief) return { text: 'cos: the chief-of-staff mod is off in this session (BORANT_ROLE is not chief)' }
      await refresh($, true)
      const args = String(e.args || '').trim()
      if (args === 'help') return { text: helpText() }
      if (args === 'pane') return openAt($, [], 0)
      if (!board) return { text: 'cos: board unavailable' + (reason ? ' · ' + reason : '') }
      // A docked pane can take up to 45% of the terminal; keep the digest
      // inside the transcript column it leaves, so it never wraps.
      const width = Math.max(48, Math.floor((columns || 96) * 0.55) - 1)
      return { text: digest(board, { project: args || null, width }) }
    } catch {
      return { text: 'cos: board unavailable' + (reason ? ' · ' + reason : '') }
    }
  })

  // From 144 columns a digit at an empty prompt is the ticker's: it opens the
  // pane with the keyboard, on that entry, and never lands in the prompt. A
  // paste, a key with a modifier, and a digit with no entry go on as typing.
  on('prompt.edit', async ($, e, next) => {
    try {
      await ensureStarted($)
      // Below 144 columns the band's own digit Buttons answer a bare digit,
      // and only when the edit goes on: the engine presses the Button then.
      const narrow = columns > 0 && columns < ASKED_COLUMNS
      if (isChief && !narrow && e.text === '' && e.key && !e.key.ctrl && !e.key.meta && /^[1-9]$/.test(e.inputText || '')) {
        const n = Number(e.inputText)
        if (n === 1 || activeProjects(board)[n - 2]) {
          // The open starts inside this hook, so the person's key is behind
          // it: the engine places an asked pane at any width, an unasked one
          // only from 144 columns.
          openDigit($, n).catch(() => {})
          return { text: e.text, cursor: e.cursor }
        }
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
  // Borant CLI, pass.
  on('tool.check', { tool: 'Bash' }, async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    const verdict = await next(e)
    if (!isChief || verdict.decision === 'deny') return verdict
    const why = projectWork(e.input)
    return why ? { decision: 'deny', reason: guardReason(why) } : verdict
  })

  on('session.compact', async ($, e, next) => {
    const out = await next(e)
    if (isChief && !out?.skip) ruleOwed = true
    return out
  })

  // The turn a Done started is over: the pane takes the keyboard back.
  on('turn.complete', async ($, e, next) => {
    const out = await next(e)
    try {
      if (isChief && refocusOnIdle && paneOpen) {
        // A dialog the turn raised (a permission prompt) may still hold the
        // keys as the turn ends, so the pane asks a moment later.
        refocusOnIdle = false
        $.clock.after(300, () => {
          if (paneOpen && !paneFocused) handoff($).catch(() => {})
        })
      }
    } catch {
      // The pane stays where it is; a digit focuses it.
    }
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
 * or session hook is aborted when that hook returns. So the stream starts
 * from a timer, and one that ends or is refused starts again from a timer.
 */
function followEvents($) {
  try {
    $.clock.after(0, () => {
      streamEvents($).catch(() => {})
    })
  } catch {
    // Without a timer the board still refreshes on each turn.
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
    // No borant, or an old one: the timer still reads the board.
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
  return freshness({ unavailable: unavailable && !board, reading, live, ageMs })
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
  redraw($)
  let result
  try {
    result = await readBoard($, config)
  } finally {
    reading = false
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

/** The current view's name, which keys its remembered selection. */
function currentKey() {
  return viewKey(path)
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
    path,
    folds,
    board,
    reason,
    tasks: taskLists(),
    questions: drawnQuestions(),
    pending,
    now: clockNow,
    asked,
    help: helpLines({ projects: activeProjects(board).length }),
    selection: selectionOf(),
    width: paneWidth,
    compact: placement === 'inline',
  })
}

/** The needs-you items as drawn: the board's, with the ones a Done just closed. */
function drawnQuestions() {
  return withClosed(allQuestions(board), pending, clockNow)
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
  for (const channel of channelsFor({ path })) {
    const result = await readTasks($, config, channel)
    taskCache.set(channel, { list: result.tasks, reason: result.reason })
  }
}

/** Read the tasks the view needs and draw it; `refocus` asks for the keys. */
async function land($, refocus = false) {
  await ensureTasks($, channelsFor({ path }))
  await setTitle($, refocus)
  redraw($)
  await focusRow($, selectionOf())
}

function titleNow() {
  return path.length ? breadcrumb(path) : PANE_TITLE
}

/** Every open names the same size and Esc: each open sets them anew. */
function paneArgs(extra = {}) {
  return { id: PANE, title: titleNow(), closeOnEscape: true, columns: PANE_COLUMNS, rows: PANE_ROWS, ...extra }
}

async function setTitle($, refocus = false) {
  if (!paneOpen || (titleNow() === paneTitle && !refocus)) return
  try {
    await $.ui.open(paneArgs(refocus ? { focus: true } : {}))
    paneTitle = titleNow()
  } catch {
    // The title is cosmetic.
  }
}

/**
 * Open the pane with the keyboard on `nextPath`, its selection on `ordinal`.
 * The engine grants the keyboard only while the prompt holds it over an empty
 * composer, which is where a digit and `/cos pane` come from.
 */
async function openAt($, nextPath, ordinal) {
  path = nextPath
  setSelection(ordinal)
  try {
    const opened = await $.ui.open(paneArgs({ focus: true }))
    if (opened && opened.isPlaced === false) {
      const why = 'cos: the pane did not open' + (opened.reason ? ' (' + opened.reason + ')' : '')
      toast($, why)
      return { text: why }
    }
    paneOpen = true
    paneTitle = titleNow()
    await land($)
    return {}
  } catch {
    return { text: 'cos: the pane did not open' }
  }
}

/**
 * Where ticker entry `n` opens: 1 what needs you, then each active project,
 * unfolded and selected; `help` the help. Returns the path and the selection.
 */
function aim(n) {
  if (n === 'help') return [[{ kind: 'help' }], 0]
  path = []
  const project = n > 1 ? activeProjects(board)[n - 2] : null
  if (!project) return [[], 0]
  folds[foldKey(project)] = true
  return [[], Math.max(0, selectableRows(rowsNow()).findIndex((r) => r.project === project.project))]
}

/** A digit at the empty prompt: the pane opens with the keyboard at once. */
async function openDigit($, n) {
  const [where, ordinal] = aim(n)
  return openAt($, where, ordinal)
}

/**
 * A press on a ticker entry or the help button. The band holds the keyboard
 * while its Button runs, and the engine refuses a pane's `focus` then. So the
 * band draws without its Buttons for a moment, the keys fall back to the empty
 * prompt, and the pane asks again until its own drawing says it has them.
 */
async function openFromBand($, n) {
  const [where, ordinal] = aim(n)
  const answer = await openAt($, where, ordinal)
  try {
    $.clock.after(50, () => {
      handoff($).catch(() => {})
    })
  } catch {
    // No clock: the pane opens without the keyboard, and ctrl+x tab reaches it.
  }
  return answer
}

/** Ask for the pane's keyboard until its drawing says it has it. */
async function handoff($) {
  try {
    bandHidden = true
    redraw($)
    for (let tries = 0; tries < FOCUS_TRIES; tries += 1) {
      await $.clock.sleep(50 + tries * 100)
      await $.ui.open(paneArgs({ focus: true }))
      await $.clock.sleep(100)
      if (paneFocused) break
    }
    await focusRow($, selectionOf())
  } finally {
    bandHidden = false
    redraw($)
  }
}

/** Move the selection one row, as j and k do. */
async function move($, delta) {
  const count = selectableRows(rowsNow()).length
  if (count === 0) return
  const current = Math.min(selectionOf(), count - 1)
  const next = Math.min(count - 1, Math.max(0, current + delta))
  setSelection(next)
  redraw($)
  await focusRow($, next)
}

/**
 * Land the engine's focus ring on one row, and scroll its whole item in. The
 * ring keeps its place in the tab order across a redraw, and the selected
 * card's keys move with the selection, so the ring is put on the row again
 * once the new drawing is in.
 */
async function focusRow($, ordinal) {
  const ring = () => $.ui.focus({ requestId: PANE, key: 'row-' + ordinal }).catch(() => {})
  try {
    await ring()
    $.clock.after(50, () => {
      if (selectionOf() === ordinal) ring()
    })
  } catch {
    // A row not drawn yet, or a pane without the keys.
  }
  const lines = rowsNow()
  const last = lines.reduce((at, r, i) => (r.part === ordinal ? i : at), -1)
  const first = lines.findIndex((r) => r.part === ordinal)
  for (const at of [last, first]) {
    if (at < 0) continue
    try {
      await $.ui.scroll({ to: { key: 'line-' + at }, in: PANE, block: 'nearest' })
    } catch {
      // A drawing that fits needs no scroll.
    }
  }
}

/** h: fold an open project, or the project an epic sits under, else go back. */
async function left($) {
  const rows = rowsNow()
  const row = rowAt(rows, selectionOf())
  if (row && row.fold && row.isOpen) return toggleFold($, row)
  if (row && row.parentFold) {
    folds[row.parentFold] = false
    const at = selectableRows(rowsNow()).findIndex((r) => r.fold === row.parentFold)
    setSelection(Math.max(0, at))
    redraw($)
    return focusRow($, selectionOf())
  }
  if (path.length) return back($)
}

/** l: unfold a folded project, else open the row. */
async function right($) {
  const row = rowAt(rowsNow(), selectionOf())
  if (row && row.fold && !row.isOpen) return toggleFold($, row)
  if (row && row.drill) return drill($, row.drill)
}

async function toggleFold($, row) {
  folds[row.fold] = !row.isOpen
  const count = selectableRows(rowsNow()).length
  if (count) setSelection(Math.min(selectionOf(), count - 1))
  redraw($)
  await focusRow($, selectionOf())
}

/** Drill into an epic, a task, or the help. */
async function drill($, node) {
  if (!node) return
  path = [...path, node]
  setSelection(0)
  redraw($)
  await land($)
}

/** Go back one level. */
async function back($, refocus = false) {
  path = path.slice(0, -1)
  redraw($)
  await land($, refocus)
}

/** The footer's esc: back a level, then close at the root. */
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

/**
 * A press on a row's glyph: the first one selects it, and pressing it again
 * opens it. Enter presses the selected row, so it opens a card, an epic, or a
 * task, and folds a project. A click focuses the row just before its press,
 * so that first press only selects.
 */
async function pressRow($, row) {
  const ordinal = Number(String(row.key).slice(4))
  if (!Number.isFinite(ordinal)) return
  const activate = async () => {
    if (row.fold) return toggleFold($, row)
    if (row.drill) return drill($, row.drill)
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

/** The help as the transcript shows it, the same text as the pane's help. */
function helpText() {
  return helpLines({ projects: activeProjects(board).length }).join('\n')
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
    case 'key':
      return { color: 'suggestion', bold: true }
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
    case 'doneDim':
      return { color: 'green', dimColor: true }
    case 'blue':
    case 'running':
      return { color: 'blue' }
    default:
      return {}
  }
}

/**
 * A segment's Text props on the selection background: the inverse text
 * color, so dim and colored cells stay readable, and no bold, which many
 * terminals draw as grey. The bar's filled cells keep their green.
 */
function onSelected(style, value = '') {
  if (String(value).startsWith('\u2588')) return styleProps(style)
  return { color: 'inverseText', dimColor: false, bold: false }
}

/**
 * The band: the ticker's one line. From 144 columns its digits are text, and
 * the prompt.edit hook takes a digit; below, each entry is a plain Button
 * with its digit as hotkey (the engine draws `2: ` before it), since only a
 * press opens a pane there. The help button is a Button at every width.
 */
function band($, e, segments) {
  const { Box, Text, Button } = $.ui.resolve(e)
  const terminal = (e.viewport && e.viewport.columns) || columns || 0
  const pressable = !bandHidden && terminal < ASKED_COLUMNS
  const cell = (s, key) => Text({ key, children: [s.text], wrap: 'truncate', ...styleProps(s.style) })
  const children = []
  for (let i = 0; i < segments.length; i += 1) {
    const s = segments[i]
    if (s.button) {
      children.push(bandHidden ? cell({ text: s.text }, 'help') : Button({ key: s.button.key, label: s.button.label, onPress: () => press($, s.button) }))
    } else if (pressable && s.entry) {
      const run = []
      while (i < segments.length && segments[i].entry === s.entry) run.push(segments[i++])
      i -= 1
      children.push(
        Button({
          key: 'tick-' + s.entry,
          plain: true,
          hotkey: String(s.entry),
          children: run.filter((r) => r.style !== 'key').map((r, j) => cell(r, 'tick-' + s.entry + '-' + j)),
          onPress: () => openFromBand($, s.entry),
        }),
      )
    } else {
      children.push(cell(s, 'tick-' + i))
    }
  }
  return Box({ flexDirection: 'row', children })
}

/** The pane: a header with the freshness word, the view's rows, the keys. */
function paneBody($, e, width) {
  const { Box, Text, Button, Link } = $.ui.resolve(e)
  const fresh = freshNow || seg('live', 'green')
  const freshWidth = Array.from(fresh.text).length
  const label = truncate(titleNow(), Math.max(4, width - freshWidth - 2))
  const gap = Math.max(1, width - Array.from(label).length - freshWidth)
  const children = [
    Box({
      key: 'pane-head',
      flexDirection: 'row',
      children: [
        Text({ key: 'head-label', children: [label], bold: true, ...(path.length ? { dimColor: true } : {}) }),
        Text({ key: 'head-gap', children: [' '.repeat(gap)] }),
        Text({ key: 'head-fresh', children: [fresh.text], ...styleProps(fresh.style) }),
      ],
    }),
    Text({ key: 'pane-blank', children: [' '] }),
  ]

  rowsNow().forEach((r, i) => {
    const selected = r.selected
    const props = selected ? (style, value) => onSelected(style, value) : (style) => styleProps(style)
    const cells = []
    if (r.indent) cells.push(Text({ key: 'indent', children: [' '.repeat(r.indent)] }))
    if (r.marker) {
      if (r.selectable) {
        const button = {
          key: r.key,
          plain: true,
          children: [Text({ children: [r.marker.text], ...props(r.marker.style) })],
          onPress: () => pressRow($, r),
        }
        if (selected) button.autoFocus = true
        cells.push(Button(button))
      } else {
        cells.push(Text({ key: 'marker', children: [r.marker.text], ...props(r.marker.style) }))
      }
      cells.push(Text({ key: 'marker-gap', children: [' '] }))
    }
    if (r.status.length || r.buttons.length) {
      r.status.forEach((s, j) => cells.push(Text({ key: 'status-' + j, children: [s.text], ...props(s.style) })))
      r.buttons.forEach((b, j) => {
        if (j > 0 || r.status.length) cells.push(Text({ key: 'button-gap-' + j, children: ['   '] }))
        const button = { key: b.key, label: b.label, plain: true, onPress: () => press($, b) }
        if (b.hotkey) button.hotkey = b.hotkey
        cells.push(Button(button))
      })
    } else {
      r.segments.forEach((s, j) => cells.push(Text({ key: 'seg-' + j, children: [s.text], wrap: 'truncate', ...props(s.style, s.text) })))
      if (r.link) cells.push(Link({ key: 'link', href: r.link.href, label: r.link.label }))
    }
    children.push(
      Box({
        key: 'line-' + i,
        flexDirection: 'row',
        ...(selected ? { backgroundColor: SELECT_BG } : {}),
        children: cells,
      }),
    )
  })

  children.push(Text({ key: 'pane-foot-blank', children: [' '] }))
  children.push(footer($, e))
  return Box({ flexDirection: 'column', children })
}

/** The footer: the pane's keys, as Buttons whose hotkeys they are. */
function footer($, e) {
  const { Box, Text, Button } = $.ui.resolve(e)
  return Box({
    key: 'pane-foot',
    flexDirection: 'row',
    columnGap: 2,
    children: [
      Button({ key: 'nav-down', label: '↓', hotkey: 'j', plain: true, onPress: () => move($, 1) }),
      Button({ key: 'nav-up', label: '↑', hotkey: 'k', plain: true, onPress: () => move($, -1) }),
      Button({ key: 'nav-fold', label: 'fold', hotkey: 'h', plain: true, onPress: () => left($) }),
      Button({ key: 'nav-open', label: 'open', hotkey: 'l', plain: true, onPress: () => right($) }),
      // Esc and Enter cannot be Button hotkeys; Enter presses the selected row.
      Button({ key: 'nav-back', label: 'esc back', plain: true, dimColor: true, onPress: () => escLike($) }),
      Text({ key: 'foot-enter', children: ['⏎ open'], dimColor: true }),
    ],
  })
}

/** A press on a Button of the band or the pane. */
function press($, button) {
  switch (button.kind) {
    case 'help':
      return openFromBand($, 'help')
    case 'done':
      return markDone($, drawnQuestions()[button.arg])
    case 'ask':
      return askOwner($, drawnQuestions()[button.arg])
    case 'open': {
      const q = drawnQuestions()[button.arg]
      return q ? drill($, cardNode(q)) : redraw($)
    }
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
    'Close it as done now: task_close it yourself if it is a saga gate or you own its channel; ' +
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
        redraw($)
        // The submitted prompt takes the keyboard; the pane asks for it back
        // now and again when the chief's turn completes.
        if (paneOpen) {
          refocusOnIdle = true
          await handoff($)
        }
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
