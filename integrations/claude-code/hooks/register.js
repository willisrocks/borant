// borant's chief-of-staff mod. It draws the board in the band above the
// prompt, raises a toast when something new needs the person, and answers
// `/cos`. Only a chief session sees any of it: the role comes from
// BORANT_ROLE, which is how a chief is started (see
// crates/borant/src/hook.rs, session_start). Every other session registers
// no timer, no command, and draws nothing.
//
// The band is the one place for the keys (the "ledger band" design). A digit
// at an empty prompt presses that row's chip and the band takes the keys with
// the row selected; j and k move, Enter opens the row in place (one at a
// time), d and a act on a selected needs-you item, and esc goes back to the
// prompt. The selection is recorded here: the mod's own `$.ui.focus` does not
// come back through its `ui.focus` hook.
//
// Shapes and rules come from docs/superpowers/specs/2026-10-26-sagas-and-epics-design.md.

import { allQuestions, activeProjects, digest, findProject, helpLines, hhmm, questionKey, stepKey, truncate } from '../mod/board.js'
import { bandKeys, bandRows } from '../mod/band.js'
import { readBoard, sourceMode } from '../mod/source.js'
import { DELEGATION_RULE, guardReason, projectWork } from '../mod/guard.js'
import { CLOSED_SHOW_MS, canDone, freshness, withClosed } from '../mod/status.js'

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
/** Keys typed into the empty prompt act on the band this long after the last one. */
const COMPOSER_KEYS_MS = 30000
/** How often the mod checks that the band's ring still holds the keys. */
const RING_PROBE_MS = 500

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
/** The selected row's chip key while the band holds the keys, else null. */
let selected = null
/** Where the selected row sat, for when it leaves the board. */
let selectedAt = 0
/** The row open in place, a chip key; only ever the selected row. */
let open = null
/** The band's ring holds the keyboard (ctrl+x tab, a click), as ui.focus said. */
let bandHasKeys = false
/** The element the band's ring is on, as last moved or reported. */
let ringKey = null
/** Counts band keys, so an old idle timer knows a newer key came. */
let touches = 0
/** The chip a person's click just put the ring on, so that press selects. */
let clicked = null
/** The terminal's width, as last measured while drawing the band. */
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
      if (!isChief || (!board && !unavailable)) return next(e)
      measure(e)
      clockNow = await nowMs($)
      keepSelection($)
      const rows = bandRows(board, {
        width: widthOf(e),
        maxRows: e.props?.maxRows,
        selected,
        open,
        focused: selected !== null,
        // From the prompt (after a digit) the keys differ: space opens, and
        // any other key types. Only with the ring do Enter and esc work.
        ring: bandHasKeys,
        fresh: await fresh($),
        unavailable: !board,
        reason,
        questions: drawnQuestions(),
        pending,
        asked,
        now: clockNow,
      })
      // Without a board only the title row says why.
      return tree($, e, board ? rows : rows.slice(0, 1))
    } catch {
      return next(e)
    }
  })

  // The band's ring names the row chip it is on, and that row is the
  // selection. The ring taking the band (ctrl+x tab, a click, autoFocus)
  // selects; the ring leaving the band's elements clears it.
  on('ui.focus', { requestId: BAND }, async ($, e, next) => {
    try {
      if (isChief) {
        const key = typeof e.element === 'string' ? e.element : null
        // A plugin's own move names no element here; only the person's empty
        // one (the ring leaving the band's elements) means the keys went.
        if (key === null && e.origin?.kind === 'plugin') return next(e)
        setRing($, key !== null)
        ringKey = key
        if (key === null && selected !== null) {
          leaveBand($)
        } else if (isRowKey(key) && key !== selected) {
          if (e.origin && e.origin.kind === 'person') markClick($, key)
          if (e.origin?.kind === 'person' || selected === null) {
            if (open !== key) open = null
            selected = key
            redraw($)
            // The redraw adds j, k, Done, and Ask ahead of rows, and the ring
            // keeps its place in the order, not its row: put it back.
            $.clock.after(0, () => {
              reveal($, key).catch(() => {})
            })
          }
        }
      }
    } catch {
      // A focus move is never worth an error.
    }
    return next(e)
  })

  on('command.run', { command: 'cos' }, async ($, e) => {
    await ensureStarted($).catch(() => {})
    try {
      if (!isChief) return { text: 'cos: the chief-of-staff mod is off in this session (BORANT_ROLE is not chief)' }
      // A command runs from the prompt, which now has the keys.
      leaveBand($)
      await refresh($, true)
      const args = String(e.args || '').trim()
      if (args === 'help') return { text: helpText() }
      if (args === 'pane') return { text: 'cos: the board is the band above the prompt now; ' + digitsHint() + '/cos help for the keys' }
      if (!board) return { text: 'cos: board unavailable' + (reason ? ' · ' + reason : '') }
      const width = Math.max(48, (columns || 96) - 2)
      return { text: digest(board, { project: args || null, width }) }
    } catch {
      return { text: 'cos: board unavailable' + (reason ? ' · ' + reason : '') }
    }
  })

  // A digit at an empty prompt presses a row's chip, but the engine leaves
  // the keys in the composer (only the person gives a site the keyboard). So
  // while a row is selected and the prompt is empty, the band's keys typed
  // there act on the band and are consumed. Any other edit is typing: the
  // prompt has the keys, and the selection goes.
  on('prompt.edit', async ($, e, next) => {
    try {
      if (isChief && selected !== null) {
        const key = composerKey(e)
        if (key && BAND_KEYS.has(key)) {
          await bandKey($, key)
          return { text: e.text, cursor: e.cursor }
        }
        leaveBand($)
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
        redraw($)
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
      description: 'The chief-of-staff board: /cos, /cos <project>, or /cos help',
      argumentHint: '[help|project]',
      immediate: true,
    })
  } catch {
    // A taken name only costs the command, not the band.
  }

  try {
    $.clock.every(REFRESH_MS, () => {
      refresh($)
        .then(() => redraw($))
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
        live = true
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
        .then(() => redraw($))
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


/** The needs-you items as drawn: the board's, with the ones a Done just closed. */
function drawnQuestions() {
  return withClosed(allQuestions(board), pending, clockNow)
}

function isRowKey(key) {
  return typeof key === 'string' && (key.startsWith('q:') || key.startsWith('p:'))
}

/** A click puts the ring on a row just before it presses it: that press selects. */
function markClick($, key) {
  clicked = key
  try {
    $.clock.after(0, () => {
      if (clicked === key) clicked = null
    })
  } catch {
    clicked = null
  }
}

/** Whether the band's ring holds the keyboard; a change redraws the hint. */
function setRing($, holds) {
  if (holds === bandHasKeys) return
  bandHasKeys = holds
  redraw($)
  if (holds) watchRing($)
}

/**
 * Esc hands the keys back to the prompt without a ui.focus event, so while
 * the ring holds them the mod asks for them again every half second: the
 * engine refuses a site that lost them, and the band leaves its selection.
 */
let watching = false
function watchRing($) {
  if (watching) return
  watching = true
  const tick = () => {
    if (!bandHasKeys || selected === null) {
      watching = false
      return
    }
    $.ui
      .focus({ requestId: BAND, key: ringKey || selected })
      .then((out) => {
        if (out && out.deny) {
          watching = false
          bandHasKeys = false
          leaveBand($)
          return
        }
        $.clock.after(RING_PROBE_MS, tick)
      })
      .catch(() => {
        watching = false
      })
  }
  try {
    $.clock.after(RING_PROBE_MS, tick)
  } catch {
    watching = false
  }
}

/** The band's keys as typed into an empty composer. */
// Enter and Esc never reach prompt.edit (Enter submits; Esc is the editor's),
// so from the prompt a space opens the row.
const BAND_KEYS = new Set(['j', 'k', 'd', 'a', ' ', 'space'])

/** The one plain key of an edit to an empty prompt, or null. */
function composerKey(e) {
  if (e.text !== '' || !e.key || e.key.ctrl || e.key.meta || e.key.shift) return null
  return String(e.key.key || '')
}

/** One band key, from the band's own Buttons or from the empty composer. */
function bandKey($, key) {
  touch($)
  if (key === 'j') return bandMove($, 1).catch(() => {})
  if (key === 'k') return bandMove($, -1).catch(() => {})
  if (key === ' ' || key === 'space') return pressRow($, selected).catch(() => {})
  const q = selected && selected.startsWith('q:') ? questionOf(selected.slice(2)) : null
  if (!q) return
  if (key === 'd' && canDone(pending[questionKey(q)])) return markDone($, q).catch(() => {})
  // Ask waits on `borant send`; the key returns at once and the row shows it.
  if (key === 'a' && !(asked[questionKey(q)]?.state === 'sending')) void askOwner($, q).catch(() => {})
}

/**
 * Keys typed into the composer act on the band only for a while after the
 * last band key, so a prompt typed later never starts with a stray Done.
 */
function touch($) {
  const mark = ++touches
  try {
    $.clock.after(COMPOSER_KEYS_MS, () => {
      if (mark === touches && !bandHasKeys) leaveBand($)
    })
  } catch {
    // Without a clock the selection stays until the next typed key.
  }
}

/** The prompt has the keys again: no row is selected, and none is open. */
function leaveBand($) {
  touches += 1
  if (selected === null && open === null) return
  selected = null
  open = null
  redraw($)
}

/**
 * A selected row that left the board (a Done closed it) hands the selection
 * to the row now in its place, so the keys keep working.
 */
function keepSelection($) {
  if (selected === null || !board) return
  const keys = bandKeys(board, drawnQuestions())
  if (keys.includes(selected)) {
    selectedAt = keys.indexOf(selected)
    return
  }
  selected = keys[Math.min(selectedAt, keys.length - 1)] || null
  open = null
  if (selected === null) return
  const key = selected
  try {
    $.clock.after(0, () => {
      reveal($, key).catch(() => {})
    })
  } catch {
    // The ring stays where the engine put it.
  }
}

/** Select a row: draw it selected, and put the ring and the window on it. */
async function select($, key) {
  if (open !== key) open = null
  selected = key
  touch($)
  redraw($)
  await reveal($, key)
}

async function reveal($, key) {
  // The engine keeps the ring at its place in the band's order through a
  // redraw, and the selected row's Done and Ask move the rows under it, so
  // the ring is put on the row again once the new drawing is in.
  // The answer also says who has the keys: the engine refuses a site that
  // does not hold them, as after a digit typed at the prompt.
  const focus = async () => {
    const out = await $.ui.focus({ requestId: BAND, key }).catch(() => ({ deny: 'no band' }))
    if (!(out && out.deny)) ringKey = key
    setRing($, !(out && out.deny))
  }
  try {
    await focus()
    $.clock.after(80, () => {
      if (selected === key) focus()
    })
  } catch {
    // The band does not hold the keys.
  }
  try {
    await $.ui.scroll({ to: { key }, in: BAND, block: 'nearest' })
  } catch {
    // A band that fits needs no scroll.
  }
}

/**
 * A row's chip: a digit, Enter on the ring, or a click. The first press
 * selects the row; pressing the selected row again opens it in place, or
 * closes it.
 */
async function pressRow($, key) {
  if (clicked === key) {
    clicked = null
    return select($, key)
  }
  if (selected !== key) return select($, key)
  open = open === key ? null : key
  redraw($)
  await reveal($, key)
}

/** j and k: the next or the previous row, wrapping at the ends. */
async function bandMove($, delta) {
  const keys = bandKeys(board, drawnQuestions())
  const key = stepKey(keys, selected || keys[0], delta)
  if (key) await select($, key)
}

/** The needs-you item of a Done or an Ask button, by its question key. */
function questionOf(key) {
  return drawnQuestions().find((q) => questionKey(q) === key && !q.closed) || null
}

/** A press on a Button of the band. */
function press($, button) {
  switch (button.kind) {
    case 'needs':
    case 'project':
      return pressRow($, button.key)
    case 'band-down':
      return bandKey($, 'j')
    case 'band-up':
      return bandKey($, 'k')
    case 'help':
      return toast($, 'keys: ' + digitsHint() + 'j k move · ⏎ open · d done · a ask · esc prompt · /cos help')
    case 'done':
      return markDone($, questionOf(button.arg))
    case 'ask':
      return askOwner($, questionOf(button.arg))
    default:
      return redraw($)
  }
}

/** The rows the digits select: one per needs-you item, then one per project. */
function rowCount() {
  return allQuestions(board).length + activeProjects(board).length
}

function digitsHint() {
  const n = Math.min(9, rowCount())
  return n ? (n === 1 ? '1' : '1-' + n) + ' select · ' : ''
}

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
    case 'magenta':
    case 'scheduled':
      return { color: 'magenta' }
    default:
      return {}
  }
}

/**
 * The band: one Box per row, a Button where a segment names one. A selected
 * row, with its open lines, draws on the selection background to the edge.
 */
function tree($, e, rows) {
  const { Box, Text, Button } = $.ui.resolve(e)
  const chips = rows.flatMap((r) => r.segments.filter((s) => s.button && isRowKey(s.button.key)).map((s) => s.button.key))
  // The ring starts on the selected row when the band takes the keys.
  const first = chips.includes(selected) ? selected : chips[0]
  const textProps = (style, onSelection, bar) => {
    const props = styleProps(style)
    if (!onSelection) return props
    // On the selection background every cell takes the inverse text color,
    // so dim and colored cells stay readable; the bar keeps its green fill.
    // Bold goes: many terminals draw bold black as grey, grey on grey.
    return { ...props, ...(bar ? {} : { color: 'inverseText' }), dimColor: false, bold: false }
  }
  const element = (s, key, onSelection) => {
    const b = s.button
    if (!b) return Text({ key, children: [s.text], ...textProps(s.style, onSelection, s.text.startsWith('█')) })
    const label = b.parts.map((p) => p.text).join('')
    if (b.chrome) return Button({ key: b.key, label, onPress: () => press($, b) })
    const props = {
      key: b.key,
      label,
      plain: true,
      children: b.parts.map((p, i) => Text({ key: b.key + '-' + i, children: [p.text], ...textProps(p.style, onSelection) })),
      onPress: () => press($, b),
    }
    if (b.hotkey) props.hotkey = b.hotkey
    if (b.key === first) props.autoFocus = true
    return Button(props)
  }
  return Box({
    flexDirection: 'column',
    children: rows.map((row) =>
      Box({
        key: 'band-' + row.key,
        flexDirection: 'row',
        ...(row.selected ? { backgroundColor: 'promptBorder' } : {}),
        children: row.segments.map((s, j) => element(s, row.key + '-' + j, row.selected)),
      }),
    ),
  })
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
