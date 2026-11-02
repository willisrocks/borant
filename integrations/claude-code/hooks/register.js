// borant's chief-of-staff mod. It draws the board above the prompt, a preview
// of the selected row, a toast when something new needs the person, and the
// `/cos` digest. Only a chief session sees any of it: the role comes from
// BORANT_ROLE, which is how a chief is started (see
// crates/borant/src/hook.rs, session_start). Every other session registers no
// timer, no command, and draws nothing.
//
// Board mode holds the keyboard, as a vim mode does. A bare 0 at the empty
// prompt (the band's `0: board mode` Button) or a row's digit opens the
// preview pane with the keys, and the pane's only focusable element is an
// Input: while it has the focus, Claude Code gives every printable key to it
// and none to the prompt. The mod reads each key from the Input's change
// (1-9 select, j k move, d Done, a Ask, ? help) and Enter from its submit
// (details). Esc gives the keys back to the prompt and closes the pane. Tab
// and Shift+Tab move the focus off the Input, and the band and the pane then
// say that board mode is off.
//
// The digit reaches the mod two ways. From 144 columns the prompt.edit hook
// takes it and opens the preview. Below 144 the engine places only a pane
// that a press or a command asked for, so there each band row's chip carries
// its digit as a hotkey, and the press opens the preview.
//
// Docked (a wide terminal) the preview is a tall card beside the transcript;
// inline (a narrow one) it is a compact block over the band, open only while
// it has the keys.

import { allQuestions, bandItems, bandRows, digest, helpLines, text as seg, truncate, hhmm } from '../mod/board.js'
import { channelsFor, helpPreview, previewLines, questionKey, findProject } from '../mod/preview.js'
import { readBoard, readTasks, sourceMode } from '../mod/source.js'
import { DELEGATION_RULE, guardReason, projectWork } from '../mod/guard.js'
import { CLOSED_SHOW_MS, canDone, freshness, pressStatus, pressStatusShort, withClosed } from '../mod/status.js'

/** The preview pane's id and title. */
const PANE = 'cos'
const PANE_TITLE = 'chief of staff'
/** The docked preview's width, as asked: wide enough for a card's title and
 * fields. Claude Code caps it at 45% of the terminal, and a width the person
 * dragged or keyed wins over it. */
const PANE_COLUMNS = 80
/** Where the band keeps this many columns beside the docked preview (a
 * terminal of about 186 columns and up), it lays its columns out that wide
 * at rest too, so entering board mode moves no column. Narrower, a band that
 * wide would waste half the terminal at rest, so board mode lays it out
 * again. */
const DOCKED_GRID = 100
/** From this width a fullscreen terminal docks the preview beside the transcript. */
const DOCK_COLUMNS = 110
/** A docked preview narrower than this draws the compact block, as inline. */
const NARROW_PANE = 52
/** From this width the board-mode field names its keys in words. */
const WORDY_KEYS = 64
/** The selection's background, in the band and in the preview: the theme's
 * user-message grey, darker than promptBorder, so yellow titles, colored
 * glyphs and blue keys keep their colors and stay readable on it. */
const SELECTED_BG = 'userMessageBackground'
/** Below this many terminal columns the engine places only an asked pane (a
 * command, a prompt, or a press behind it). A digit taken by prompt.edit is
 * none of those, so there the band's chips keep the digits as hotkeys. */
const ASKED_COLUMNS = 144
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
/** How long after asking for the keys the mod checks that the pane has them. */
const FOCUS_CHECK_MS = 400
/** An inline preview that lost the keys closes after this long. */
const INLINE_IDLE_MS = 400
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
/** The id of the chief's turn that runs a Done's prompt, while it runs: no
 * tool may ask the person in it. Set at turn.start, which carries the prompt. */
let doneTurnId = null
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
/** The selected band row, by its key, and its place, for when it goes. When
 * it leaves the board the row now at its place is shown, but takes no Done
 * or Ask until the person selects a row again (`selMoved`). */
let selKey = null
let selIndex = 0
let selMoved = false
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
/** Board mode's key field: it is drawn afresh, empty, after each key (a new
 * key), and the text each field held last tells a new key from an old one. */
let fieldGen = 0
const fieldSeen = new Map()
/** Board-mode keys run one after another, in the order typed. Each board
 * mode has its own number: a key still queued when board mode ends is dropped. */
let keyChain = Promise.resolve()
let boardGen = 0
/** Board mode's rows, held still while it lasts: each band row as last seen,
 * in the order it had when board mode began. A row that leaves the board
 * stays, marked; a new one waits until esc. Null at rest. */
let frozen = null
/** The band's width while the preview was docked beside it, at a terminal
 * width: at rest the band lays its columns out that wide, so entering board
 * mode moves no column. */
let dockSeen = null
/** Board reads in the order they started: a slow one that ends after a newer
 * one is dropped. */
let readSeq = 0
let appliedSeq = 0
/** The terminal, as last measured while drawing the band. */
let columns = 0
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
        const row = [seg('── ', 'faint'), seg(label, 'bold'), seg(' ─ ', 'faint'), freshness({ unavailable: true })]
        if (reason) row.push(seg(' · ' + truncate(reason, room), 'dim'))
        return tree($, e, [{ key: 'band-title', cells: row }])
      }
      const cur = current()
      if (!cur.item && paneOpen && !helpShown) closeSoon($, () => !current().item && !helpShown)
      if (paneOpen && placement === 'dock') dockSeen = { columns, width }
      const docks = e.viewport?.isFullscreen === true && columns >= DOCK_COLUMNS
      const rows = bandRows(board, {
        width,
        gridWidth: docks && dockedBandWidth(width) >= DOCKED_GRID ? dockedBandWidth(width) : width,
        maxRows: e.props?.maxRows,
        questions: drawnQuestions(),
        items: frozen ? items() : undefined,
        waiting: waitingRows(),
        selected: cur.item ? cur.item.key : null,
        active: active(),
        statusFor: (item) => shortStatus(item),
        stale: await stale($),
        digitKeys: !promptDigits(),
      })
      return tree($, e, rows, cur.item ? cur.item.key : null)
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
        setFocused(focused)
        placement = place
        // The band's selection and hint follow where the keys are.
        redrawSoon($)
      }
      paneWidth = typeof props.bodyColumns === 'number' && props.bodyColumns > 0 ? props.bodyColumns : 60
      if (place === 'inline' && !focused) {
        // Inline, the preview is there only in board mode.
        closeSoon($, () => placement === 'inline' && !paneFocused)
      }
      return paneBody($, e, await paneLines($), focused)
    } catch {
      return next(e)
    }
  })

  // In board mode the ring stays on the key field: Tab or Shift+Tab would put
  // it on the pane's close mark, where a letter goes to the prompt while the
  // board still looks modal. Esc still gives the keys back.
  on('ui.focus', { requestId: PANE }, async ($, e, next) => {
    if (isChief && e.origin?.kind === 'person' && !String(e.element || '').startsWith('board-keys-')) {
      return { deny: 'board mode keeps the keys on its field; esc gives them back' }
    }
    return next(e)
  })

  // Esc in board mode, ctrl+x x, or the close mark: the preview goes, and
  // "more" with it.
  on('ui.close', async ($, e, next) => {
    try {
      if (e.id === PANE) {
        boardGen += 1
        paneOpen = false
        setFocused(false)
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
      if (isChief && paneFocused) {
        setFocused(false)
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
    if (doneTurnId && verdict.decision === 'ask' && e.agentId === undefined) return { decision: 'deny', reason: DONE_TURN_REASON }
    return verdict
  })

  on('turn.complete', async ($, e, next) => {
    const out = await next(e)
    if (e.agentId === undefined && e.turnId === doneTurnId) doneTurnId = null
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
      // A Done's own turn, not one it was queued behind: the guard holds in
      // it, and the chief has the prompt, so the item is closing from now.
      // Claude Code wraps a plugin's prompt in words of its own ("The borant
      // plugin sent a message:" before it, a note after it), so the turn's
      // text holds the prompt. Each Done's prompt names its task.
      if (isChief && e.agentId === undefined) {
        const text = String(e.text || '')
        const done = Object.entries(pending).find(([, m]) => m.state === 'sending' && text.includes(m.prompt))
        doneTurnId = done ? e.turnId : null
        if (done) {
          pending[done[0]] = { ...done[1], state: 'closing', at: await nowMs($) }
          redraw($)
        }
      }
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

/** `stale · 2m` once no read succeeded for a while, or null: a healthy board
 * says nothing about itself, so nothing on it moves. */
async function stale($) {
  const now = await nowMs($)
  return freshness({ ageMs: lastOkAt === null ? null : Math.max(0, now - lastOkAt) })
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
  const seq = ++readSeq
  const result = await readBoard($, config)
  // A read that started before one already applied is older news.
  if (seq < appliedSeq) return
  appliedSeq = seq
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
  // A row a Done closed stays CLOSED_SHOW_MS and then goes: that is no news.
  // The check runs before any await, so no drawing can move the selection
  // onto the row now in its place first (current() also marks that move).
  const closedRow = Object.values(pending).some((m) => ['closed', 'moved', 'unknown'].includes(m.state) && 'q:' + questionKey(m.question) === selKey)
  if (selKey && !liveItems().some((i) => i.key === selKey)) {
    // Board mode keeps the row, marked; at rest the next row takes its place.
    if (!items().some((i) => i.key === selKey)) selMoved = true
    if (!closedRow) toast($, 'the selected item left the board')
  }
  await settlePending($, now)
}

/**
 * A Done whose item left the board landed: the hub closed the task (or
 * answered it). Its row stays CLOSED_SHOW_MS as closed, then goes. One the
 * hub has not closed DONE_WAIT_MS after the prompt landed shows as still open,
 * and its Done button comes back.
 */
async function settlePending($, now) {
  const open = new Set(allQuestions(board).map(questionKey))
  // An Ask's mark goes with its item.
  for (const key of Object.keys(asked)) if (!open.has(key)) delete asked[key]
  for (const [key, mark] of Object.entries(pending)) {
    if (mark.state === 'closed' || mark.state === 'moved' || mark.state === 'unknown') {
      // Back on the board: it needs the person again, with its keys.
      if (open.has(key) || now - mark.closedAt >= CLOSED_SHOW_MS) delete pending[key]
    } else if (!open.has(key)) {
      if (mark.state === 'unsent') {
        delete pending[key]
      } else {
        // The item left needs-you. That is closed only when the task is: a
        // task that went back to work leaves the list too.
        const status = await taskStatus($, mark.question)
        const state = status === 'closed' ? 'closed' : status ? 'moved' : 'unknown'
        pending[key] = { ...mark, state, status, closedAt: now }
        toast($, { closed: 'closed: ', moved: 'no longer needs you: ', unknown: 'left the board, status unknown: ' }[state] + mark.title)
      }
    } else if (mark.state === 'closing' && now - mark.at >= doneWaitMs) {
      pending[key] = { ...mark, state: 'failed', waitMs: doneWaitMs, reason: 'not closed within ' + Math.round(doneWaitMs / 1000) + ' s' }
      toast($, 'still open: ' + mark.title)
    }
  }
}

/** A task's status as the hub has it now, or null when the read fails. */
async function taskStatus($, q) {
  const result = await readTasks($, config, q.channel)
  taskCache.set(q.channel, result.tasks)
  const task = (result.tasks || []).find((t) => t.id === q.task)
  return task ? task.status : null
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

function liveItems() {
  return board ? bandItems(board, drawnQuestions()) : []
}

/** The band's rows: the board's, or in board mode the rows it began with. */
function items() {
  const live = liveItems()
  if (!frozen || !board) return live
  const now = new Map(live.map((i) => [i.key, i]))
  frozen = frozen.map((old) => {
    const fresh = now.get(old.key)
    if (fresh) return fresh
    // It left the board: it keeps its place, marked, until esc.
    if (old.kind === 'needs' && !old.q.closed && !old.q.gone) return { ...old, q: { ...old.q, gone: true } }
    return old
  })
  return frozen
}

/** How many rows board mode holds back: new on the board since it began. */
function waitingRows() {
  if (!frozen || !board) return 0
  const held = new Set(frozen.map((i) => i.key))
  return liveItems().filter((i) => !held.has(i.key) && !(i.kind === 'needs' && (i.q.closed || i.q.gone))).length
}

/** Where the keys are: board mode holds the rows still while it lasts. */
function setFocused(focused) {
  if (focused && !paneFocused) frozen = liveItems()
  if (!focused) frozen = null
  paneFocused = focused
}

/** The band's width with the preview docked: as seen, or the band's width
 * now less the preview and its border. */
function dockedBandWidth(width) {
  if (dockSeen && dockSeen.columns === columns) return dockSeen.width
  if (paneOpen && placement === 'dock') return width
  return width - paneColumns() - 1
}

/** The docked preview's width to ask for. */
function paneColumns() {
  return Math.max(24, Math.min(PANE_COLUMNS, Math.floor(columns * 0.45)))
}

/** The selected row: the one recorded, or the row now at its place when it
 * went, which takes no Done or Ask until the person selects again. */
function current() {
  const list = items()
  if (!list.length) return { item: null, index: -1, count: 0 }
  let index = list.findIndex((i) => i.key === selKey)
  if (index < 0) {
    index = Math.max(0, Math.min(selIndex, list.length - 1))
    if (selKey) selMoved = true
    selKey = list[index].key
  }
  selIndex = index
  return { item: list[index], index, count: list.length }
}

/** Whether the board has the keys: board mode, the pane's key field focused. */
function active() {
  return paneFocused
}

/** The preview draws the compact block: inline, or in a narrow dock. */
function compactPane() {
  return placement === 'inline' || paneWidth < NARROW_PANE
}

/** Whether a digit at the empty prompt reaches prompt.edit: from 144 columns. */
function promptDigits() {
  return columns >= ASKED_COLUMNS
}

/** The marks of a needs-you item: its Done, its Ask, and whether it is open. */
function marks(item) {
  const key = questionKey(item.q)
  return { mark: pending[key], ask: asked[key], open: !item.q.closed && !item.q.gone }
}

/** A band row's short status, in its fixed tail. */
function shortStatus(item) {
  if (!item || item.kind !== 'needs') return []
  const { mark, ask } = marks(item)
  return pressStatusShort(mark, ask, clockNow) || (item.q.closed ? [seg('✓ closed', 'green')] : item.q.gone ? [seg('left the board', 'dim')] : [])
}

/** The preview's full status of the item: `◷ the chief is closing it, since 14:32`. */
function fullStatus(item) {
  if (!item || item.kind !== 'needs') return []
  const { mark, ask } = marks(item)
  return pressStatus(mark, ask, clockNow) || (item.q.closed ? [seg('✓ closed', 'green')] : item.q.gone ? [seg('left the board; the row goes after esc', 'dim')] : [])
}

/**
 * What d and a can do on the selected item now: Done while no Done is under
 * way, Ask while no Ask is under way or sent and no Done is under way, each
 * again after it failed. Neither on a project, a closed item, or a row that
 * moved into the place of one that left the board.
 */
function presses(item) {
  if (!item || item.kind !== 'needs' || selMoved) return { done: false, ask: false }
  const { mark, ask, open } = marks(item)
  const done = open && canDone(mark)
  return { done, ask: done && (!ask || ask.state === 'failed') }
}

function select(key) {
  const list = items()
  const index = list.findIndex((i) => i.key === key)
  if (index < 0) return
  selKey = key
  selIndex = index
  selMoved = false
  helpShown = false
}

/** A row's digit at the prompt or a click on it: select it and enter board mode. */
async function chipPress($, key) {
  select(key)
  redraw($)
  await enterBoard($)
  if (more) detailsSoon($)
}

/**
 * 0 at the empty prompt: board mode on the selected row. Claude Code gives
 * the pane the keys only from an empty prompt that holds them: not from text
 * in the prompt, and not while the band holds them (after ctrl+x tab). Then
 * the person is told how, rather than left with a preview that has no keys.
 */
async function enterBoard($) {
  helpShown = false
  redraw($)
  if (!(await openPane($, true))) return
  try {
    $.clock.after(FOCUS_CHECK_MS, () => {
      if (paneOpen && !paneFocused) toast($, 'board mode needs the keys at an empty prompt: esc, then 0')
    })
  } catch {
    // Without a clock the pane still says board mode is off.
  }
}

/**
 * The key field's text changed: the characters after what that field held
 * last are new keys. The field is drawn afresh after each key, so it stays
 * empty; a key typed before that redraw lands in the old field, and its text
 * there tells it apart. Backspace types nothing.
 */
function fieldInput($, value, e) {
  const field = (e && e.element) || ''
  const typed = inserted(fieldSeen.get(field) || '', value)
  fieldSeen.set(field, value)
  if (fieldSeen.size > 8) fieldSeen.delete(fieldSeen.keys().next().value)
  if (!typed) return keyChain
  fieldGen += 1
  redraw($)
  // A key queued in one board mode never runs in the next one.
  const mode = boardGen
  for (const key of Array.from(typed)) keyChain = keyChain.then(() => (mode === boardGen ? boardKey($, key) : undefined)).catch(() => {})
  return keyChain
}

/**
 * What one edit put into a field: the text between what the old and the new
 * value share at their start and at their end. A key typed after Left lands
 * in the middle, and only that key is new.
 */
function inserted(before, after) {
  const a = Array.from(before)
  const b = Array.from(after)
  let lead = 0
  while (lead < a.length && lead < b.length && a[lead] === b[lead]) lead += 1
  let trail = 0
  while (trail < a.length - lead && trail < b.length - lead && a[a.length - 1 - trail] === b[b.length - 1 - trail]) trail += 1
  return b.slice(lead, b.length - trail).join('')
}

/** One key in board mode. A key the board does not use does nothing. */
async function boardKey($, key) {
  if (/^[1-9]$/.test(key)) {
    const item = items()[Number(key) - 1]
    if (!item) return
    select(item.key)
    redraw($)
    if (more) detailsSoon($)
    return
  }
  if (key === 'j') return move($, 1)
  if (key === 'k') return move($, -1)
  if (key === '?') {
    helpShown = !helpShown
    return redraw($)
  }
  if (key !== 'd' && key !== 'a') return
  const { item } = current()
  const can = presses(item)
  // Done and Ask run beside the keys: a slow send never holds up j or k.
  if (key === 'd' && can.done) return void markDone($, pressedQuestion($, item.key)).catch(() => {})
  if (key === 'a' && can.ask) return void askOwner($, pressedQuestion($, item.key)).catch(() => {})
  toast($, (key === 'd' ? 'd: Done' : 'a: Ask') + ' needs a needs-you item, selected, with no Done under way')
}

/** Enter in board mode: details, or back from details or the help. Like a
 * key, an Enter still queued when board mode ends is dropped. */
function fieldSubmit($) {
  const mode = boardGen
  keyChain = keyChain.then(() => (mode !== boardGen ? undefined : helpShown ? backFromHelp($) : toggleMore($))).catch(() => {})
  return keyChain
}

/** Read the details' task lists beside the keys: a slow read never holds up
 * the next key, and draws when it lands. */
function detailsSoon($) {
  ensureTasks($)
    .then(() => redraw($))
    .catch(() => {})
}

/** j and k: the next or the previous row, wrapping at the ends. Details stay open. */
async function move($, delta) {
  const list = items()
  if (!list.length) return
  const { index } = current()
  select(list[(index + delta + list.length) % list.length].key)
  redraw($)
  if (more) detailsSoon($)
}

/** Enter: the preview's details, read on first use. */
async function toggleMore($) {
  helpShown = false
  more = !more
  redraw($)
  if (more) detailsSoon($)
}

/**
 * Open (or retitle) the preview. With `focus` it is board mode: it takes the
 * keys (a press at the empty prompt leaves them with the prompt, which can
 * hand them on), and Esc gives them back and closes it.
 */
async function openPane($, focus) {
  try {
    const rows = Math.max(3, paneLinesCount())
    const opened = await $.ui.open({ id: PANE, title: PANE_TITLE, rows, columns: paneColumns(), ...(focus ? { focus: true, closeOnEscape: true } : {}) })
    if (opened && opened.isPlaced === false) return false
    paneOpen = true
    return true
  } catch {
    return false
  }
}

/** Close the preview soon, if the last `still()` asked holds then. */
let closePending = false
let closeWhen = () => false
function closeSoon($, still) {
  closeWhen = still
  if (closePending) return
  closePending = true
  try {
    $.clock.after(INLINE_IDLE_MS, () => {
      closePending = false
      if (!paneOpen || !closeWhen()) return
      paneOpen = false
      placement = null
      setFocused(false)
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
  const compact = compactPane()
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
    actions: fullStatus(item),
    fresh: [],
  })
}

/** How many lines the inline preview wants, with its key line. */
function paneLinesCount() {
  if (helpShown) return helpLines({ rows: items().length }).length + 1
  const { item, index, count } = current()
  if (!item) return 1
  return previewLines({ board, item, index, count, width: paneWidth, compact: true, more, tasks: taskLists(), actions: [] }).length + 2
}

/** The help as the transcript shows it, the same text as the preview's help view. */
function helpText() {
  return helpLines({ rows: items().length }).join('\n')
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
      return { color: 'suggestion' }
    case 'modeOn':
      return { color: 'suggestion', bold: true, inverse: true }
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

/** The band: its rows, the selected one on the selection background. A
 * row's chip and its text are one Button, so a click anywhere on the row
 * selects it. */
function tree($, e, rows, selectedKey) {
  const { Box } = $.ui.resolve(e)
  // ctrl+x tab lands the ring on the selected row, so Enter acts on it.
  const ring = rows.some((r) => r.key === selectedKey) ? selectedKey : rows[1] ? rows[1].key : null
  return Box({
    flexDirection: 'column',
    children: rows.map((row, i) => {
      const [first, ...rest] = row.cells
      const focus = row.key === ring ? { autoFocus: true } : {}
      const children =
        first && first.button && first.button.kind === 'select'
          ? [rowButton($, e, row, first.button, rest.map((s, j) => element($, e, s, 'band-' + i + '-' + (j + 1), row.selected)), focus)]
          : row.cells.map((s, j) => element($, e, s, 'band-' + i + '-' + j, row.selected, s.button && s.button.key === ring ? focus : {}))
      return Box({
        key: 'band-row-' + i,
        flexDirection: 'row',
        ...(row.selected ? { backgroundColor: SELECTED_BG } : {}),
        children,
      })
    }),
  })
}

/** A band row as one plain Button: its digit (a hotkey, or drawn as one),
 * then the row's cells as its children. */
function rowButton($, e, row, b, cells, extra) {
  const { Text, Button } = $.ui.resolve(e)
  const props = { key: b.key, label: b.title || b.key, plain: true, onPress: () => press($, b), ...extra }
  if (b.hotkey) props.hotkey = b.hotkey
  const lead = b.digit ? [Text({ key: 'digit', children: [b.digit + ':'], ...styleProps('key') }), ' '] : b.hotkey ? [] : ['   ']
  props.children = [...lead, ...cells]
  return Button(props)
}

/**
 * The keys board mode takes, for the key field: the words where there is
 * room, the letters alone where there is not. d and a only for an item they
 * act on.
 */
function keysHint() {
  const can = presses(current().item)
  if (paneWidth < WORDY_KEYS) return ['1-9 j k', can.done ? 'd' : '', can.ask ? 'a' : '', '?'].filter(Boolean).join(' ') + ' · esc exit'
  const keys = ['1-9 j k move']
  if (can.done) keys.push('d done')
  if (can.ask) keys.push('a ask')
  keys.push('? help', 'esc exit')
  return keys.join(' · ')
}

/**
 * The preview: board mode's key field, then its lines. The field is the
 * pane's only focusable element, so every key reaches it: a Button here
 * would let Up, Down, or Tab move the focus onto it, and a letter typed then
 * would go to the prompt. Inline, the item's status sits under the field.
 */
function paneBody($, e, lines, focused) {
  const { Box, Text, Input } = $.ui.resolve(e)
  const compact = compactPane()
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
  const field = Input({
    key: 'board-keys-' + fieldGen,
    label: focused ? 'board mode' : 'board mode off',
    placeholder: focused ? keysHint() : '0 at an empty prompt turns it on',
    value: '',
    submitLabel: helpShown || more ? 'back' : 'details',
    autoFocus: true,
    onInput: (value, ev) => fieldInput($, value, ev),
    onSubmit: () => fieldSubmit($),
  })
  const status = compact && !helpShown ? fullStatus(current().item) : []
  const statusLine = status.length ? [Box({ key: 'pane-status', flexDirection: 'row', children: status.map((s, j) => element($, e, s, 'pane-status-' + j)) })] : []
  if (compact) return Box({ flexDirection: 'column', children: [field, ...statusLine, ...children] })
  return Box({ flexDirection: 'column', children: [field, Text({ key: 'pane-field-gap', children: [' '] }), ...children] })
}

async function backFromHelp($) {
  helpShown = false
  redraw($)
}

/** A press on a cell of the band: a row's chip, or the board-mode Button. */
function press($, button) {
  switch (button.kind) {
    case 'select':
      return chipPress($, button.arg)
    case 'board':
      return enterBoard($)
    default:
      return redraw($)
  }
}

/**
 * The item a Done or Ask button was drawn for, by its key. When it left the
 * board between the drawing and the press, the press does nothing and says
 * so: it never acts on the row that moved into its place.
 */
function pressedQuestion($, key) {
  const item = items().find((i) => i.key === key)
  if (item && item.kind === 'needs' && !item.q.closed) return item.q
  toast($, 'that item is no longer on the board')
  return null
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
  // One Ask at a time: a second a while it is sending, or after it was sent, does nothing.
  if (asked[key] && asked[key].state !== 'failed') return redraw($)
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
  // A second d before the first is in does nothing: the mark is set at once.
  if (!canDone(pending[key])) return redraw($)
  const index = allQuestions(board).findIndex((x) => questionKey(x) === key)
  const text =
    'Chris marked done: ' + q.title + ' (' + q.channel + '/' + q.task + '). ' +
    'Close it as done now, with the borant MCP tools, not the shell: task_close it yourself if it is a saga gate or you own its channel; ' +
    'otherwise send the owner of ' + q.channel + ' a direct message to close it as done, with "Chris marked it done in the board".'
  // It stays "sending" until the chief's turn with this prompt starts
  // (turn.start), which can wait behind a turn already running.
  pending[key] = { state: 'sending', at: clockNow, title: q.title, question: q, index: Math.max(0, index), prompt: text }
  const unsent = (reason) => {
    if (pending[key] && pending[key].state === 'sending') pending[key] = { ...pending[key], state: 'unsent', reason }
    redraw($)
  }
  // The prompt submits when the session is idle and resolves once it is in; a
  // press must not wait for that.
  try {
    $.prompt
      .submit({ text })
      .then((out) => {
        if (out && out.drop) return unsent('the prompt was refused')
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
