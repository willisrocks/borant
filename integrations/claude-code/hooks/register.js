// borant's chief mod (v3). Only a chief session sees any of it: the role comes
// from BORANT_ROLE (see crates/borant/src/hook.rs, session_start). Every other
// session registers no timer, no command, and draws nothing.
//
// The band above the prompt shows, at rest, what needs the person, what is
// offline, what changed since they pressed x, every project with its tasks
// by name, and the PRs that ship next. It takes no key at the prompt: its rows
// are Buttons without hotkeys (a digit hotkey in the band would take a bare
// digit at an empty prompt, Claude Code's rule), and no prompt.edit hook.
//
// The one way in is the band's `ctrl+x b` Button: it names the engine action
// app:cycleDiffBase, whose chord (ctrl+x b by default) presses it from the
// prompt. `/borant` does the same, for when the diff panel holds that chord.
// Either opens the panel with the keys ("keys on Borant"): a list of every
// item and the selected item's detail. The panel's keys are its Buttons'
// hotkeys; the arrows walk the list rows; an answer field (an Input) takes
// every key while it is open. Esc gives the keys back and closes the panel.
//
// Data: `borant board --json`, `borant channel list`, and `borant tasks
// <channel>` for every channel of a project. Actions run the borant CLI.
// The "seen" snapshot (x) lives in $.store, so it lasts across sessions.

import { buildModel, allChannels, find, listItems, snapshot, RECEIPT_MS } from '../mod/model.js'
import { ACTIONS, act, readBoard, readChannels, readTasks, sourceMode } from '../mod/source.js'
import { DELEGATION_RULE, guardReason, projectWork } from '../mod/guard.js'
import {
  S,
  bandLines,
  changeDetail,
  clock,
  helpLines,
  keyLines,
  listEntries,
  needDetail,
  panelLines,
  projectDetail,
  quietDetail,
  rule,
  shipDetail,
  taskDetail,
  taskEntries,
  topLine,
  wrap,
} from '../mod/view.js'

const PANE = 'cos'
const PANE_TITLE = 'Borant'
/** How often the board is read: the fallback for changes no event carries. */
const REFRESH_MS = 5000
/** A read arriving sooner than this after the last one is skipped. */
const DEBOUNCE_MS = 1000
/** An event is read this long after it arrives, so a burst costs one read. */
const PUSH_DELAY_MS = 300
const EVENTS_RETRY_MS = 5000
/** Every channel's tasks are read again after this long, at most. */
const TASKS_MS = 15000
/** The selection's background: the theme's user-message grey. */
const SELECTED_BG = 'userMessageBackground'

let isChief = false
let config = null
let board = null
let channels = []
/** Each channel's tasks, as `borant tasks` printed them. */
const tasks = {}
let tasksAt = 0
let unavailable = false
let reason = ''
let lastOkAt = null
let downSince = null
/** The first read of the board and every channel's tasks has landed. */
let ready = false
let seen = null
let used = true
const marks = { receipts: {}, asked: {}, changes: [] }
let pushQueued = false
let seenNeeds = null
let clockNow = 0

// The panel.
let paneOpen = false
let paneFocused = false
let placement = null
let paneCols = 90
let paneRows = 40
let columns = 0
let maxRows = 10
/** The selected list item, and where the panel is: list, detail, tasks, help. */
let sel = null
let view = 'list'
let viewBeforeHelp = 'list'
let project = null
let taskSel = null
/** An answer or message field: { kind, item, to } while it is open. */
let field = null
const drafts = new Map()
/** The last send of a field: { state: sending | unconfirmed | refused, reason }. */
let sendState = null
/** A confirmation waiting for y or n: { what, item }. */
let confirm = null

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
      const w = widthOf(e)
      maxRows = e.props?.maxRows || maxRows
      if (!board) {
        if (!unavailable) return next(e)
        return tree($, e, [{ parts: [{ segs: [S('Borant ', 'head'), S('✕ ' + (reason || 'board unavailable'), 'r')] }] }, { parts: [{ segs: [S('Run ', 't'), S('borant doctor', 'kb'), S(' in a shell to see why; it names the fix. Your prompt works as usual.', 't')] }] }])
      }
      if (!ready) return tree($, e, [{ parts: [{ segs: [S('Borant ', 'head'), S('reading the swarm…', 'dim')] }] }])
      const m = model()
      const state = unavailable ? 'down' : paneOpen && paneFocused ? 'on' : paneOpen ? 'nokeys' : 'rest'
      const lines = bandLines(m, w, {
        state,
        sel: paneOpen ? sel : null,
        maxRows,
        tip: !used,
        headerOnly: paneOpen && placement === 'inline',
        info: { seenAt: seen ? clock(seen.at) : null, downSince: downSince ? clock(downSince) : null, lastOk: lastOkAt ? clock(lastOkAt) : null, reason },
      })
      return tree($, e, lines)
    } catch {
      return next(e)
    }
  })

  // The footer names the hotkey too.
  on('ui.render', { component: 'PromptHint' }, async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    if (!isChief || paneFocused) return next(e)
    try {
      return next({ ...e, props: { ...e.props, tail: 'ctrl+x b Borant keys' } })
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
      paneOpen = true
      const focused = Boolean(props.isFocused)
      const place = props.placement || null
      if (focused !== paneFocused || place !== placement) {
        paneFocused = focused
        placement = place
        redrawSoon($)
      }
      if (typeof props.bodyColumns === 'number' && props.bodyColumns > 0) paneCols = props.bodyColumns
      if (typeof props.scroll?.bodyRows === 'number' && props.scroll.bodyRows > 0) paneRows = props.scroll.bodyRows
      return tree($, e, panel(), true)
    } catch {
      return next(e)
    }
  })

  // The ring walks the list rows and the answer field only: on a key Button or
  // the close mark, a letter would go to the prompt while the panel looks modal.
  on('ui.focus', { requestId: PANE }, async ($, e, next) => {
    if (!isChief || e.origin?.kind !== 'person') return next(e)
    const el = String(e.element || '')
    if (el.startsWith('row:')) {
      const id = el.slice(4)
      if (id.startsWith('t:')) taskSel = id
      else sel = id
      redrawSoon($)
      return next(e)
    }
    if (el === 'field') return next(e)
    return { deny: 'the panel keeps the ring on its rows; esc gives the keys back' }
  })

  on('ui.close', async ($, e, next) => {
    try {
      if (e.id === PANE) {
        paneOpen = false
        paneFocused = false
        placement = null
        view = 'list'
        field = null
        confirm = null
        redraw($)
      }
    } catch {
      // Fall through: the panel closes.
    }
    return next(e)
  })

  on('command.run', { command: 'borant' }, async ($, e) => {
    await ensureStarted($).catch(() => {})
    if (!isChief) return { text: 'borant: the chief mod is off in this session (BORANT_ROLE is not chief)' }
    const args = String(e.args || '').trim()
    if (args === 'help') return { text: helpLines(80).map((l) => l.parts.flatMap((p) => p.segs || []).map((s) => s.text).join('').trimEnd()).join('\n') }
    await refresh($, true)
    return (await openPanel($)) ? {} : { text: 'borant: the panel did not open' }
  })

  // The chief's rule, in the system prompt, where compaction cannot drop it.
  on('prompt.compose', async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    const out = await next(e)
    if (!isChief) return out
    return { ...out, sections: [...out.sections, { id: 'borant:chief-delegation', text: DELEGATION_RULE, scope: 'session' }] }
  })

  // Project work that reaches the chief's shell is refused, with a reason the
  // model reads. Reads and the Borant CLI pass.
  on('tool.check', async ($, e, next) => {
    await ensureStarted($).catch(() => {})
    const verdict = await next(e)
    if (!isChief || verdict.decision === 'deny') return verdict
    const why = e.tool === 'Bash' ? projectWork(e.input) : null
    if (why) return { decision: 'deny', reason: guardReason(why) }
    return verdict
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
      if (isChief) {
        await refresh($)
        redraw($)
      }
    } catch {
      // A refresh never disturbs a turn.
    }
    return next(e)
  })
}

let ruleOwed = false
let started = false
async function ensureStarted($) {
  if (started) return
  started = true
  await start($)
}

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
    const s = await $.store.get('seen')
    if (s && typeof s === 'object' && typeof s.at === 'number' && s.ids) seen = s
    used = Boolean(await $.store.get('used'))
  } catch {
    // No store: changes start from now.
  }
  try {
    await $.command.register({ name: 'borant', description: 'Give the Borant panel the keys (or /borant help for every key)', argumentHint: '[help]', immediate: true })
  } catch {
    // A taken name only costs the command, not the band.
  }
  try {
    $.clock.every(REFRESH_MS, () => {
      refresh($)
        .then(() => redraw($))
        .catch(() => {})
    })
    $.clock.every(1000, () => {
      // Receipts leave after RECEIPT_MS, and ages count up.
      if (Object.keys(marks.receipts).length) redraw($)
    })
  } catch {
    // Without a timer the board still refreshes on each turn.
  }
  await refresh($, true)
  redraw($)
  if (mode === 'cli') followEvents($)
}

function followEvents($) {
  try {
    $.clock.after(0, () => {
      streamEvents($).catch(() => {})
    })
  } catch {
    // The poll still reads the board.
  }
}

/** Follow the host's event stream; a task event reads that channel again. */
async function streamEvents($) {
  try {
    const stream = $.process.spawn({ argv: ['borant', 'events', '--follow', '--json'] })
    let rest = ''
    for await (const piece of stream) {
      if (!piece || piece.stream !== 'stdout') continue
      const lines = (rest + piece.text).split('\n')
      rest = lines.pop() || ''
      for (const l of lines) {
        const ev = eventOf(l)
        if (!ev) continue
        if (ev.channel && ev.kind.startsWith('task.')) staleChannels.add(ev.channel)
        pushRefresh($)
      }
    }
  } catch {
    // No borant, or an old one: the poll still reads the board.
  }
  try {
    $.clock.after(EVENTS_RETRY_MS, () => {
      streamEvents($).catch(() => {})
    })
  } catch {
    // The poll still reads the board.
  }
}

const staleChannels = new Set()

function eventOf(line) {
  try {
    const v = JSON.parse(line)
    const kind = String(v.kind || '')
    if (!/^(task|lease|agent|spawn|move|channel|change)\./.test(kind)) return null
    return { kind, channel: v.channel || v.context?.channel || null }
  } catch {
    return null
  }
}

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

let reading = null
/** Read the board, the channels, and the tasks. Never throws. */
async function refresh($, force = false) {
  if (!isChief || !config) return
  if (reading) {
    if (!force) return reading
    await reading.catch(() => {})
    if (reading) return reading
  }
  const now = await nowMs($)
  if (!force && now - lastReadAt < DEBOUNCE_MS) return
  lastReadAt = now
  reading = readAll($, now).finally(() => {
    reading = null
  })
  return reading
}
let lastReadAt = -Infinity

async function readAll($, now) {
  const b = await readBoard($, config)
  if (!b.board) {
    if (!unavailable) downSince = now
    unavailable = true
    reason = b.reason || ''
    return
  }
  board = b.board
  unavailable = false
  downSince = null
  reason = ''
  lastOkAt = now
  if (!channels.length || now - tasksAt >= TASKS_MS) {
    const c = await readChannels($, config)
    if (c.channels) channels = c.channels
  }
  const all = allChannels(board, channels)
  const due = now - tasksAt >= TASKS_MS
  for (const ch of all) {
    if (!due && ch in tasks && !staleChannels.has(ch)) continue
    staleChannels.delete(ch)
    const r = await readTasks($, config, ch)
    if (r.tasks) tasks[ch] = r.tasks
  }
  if (due) tasksAt = now
  ready = true
  // The first read with no snapshot takes one: changes start from now.
  if (!seen) {
    seen = snapshot(tasks, now)
    store($, 'seen', seen)
  }
  announceNew($)
}

/** A toast when the board shows a needs-you item that was not there before. */
function announceNew($) {
  const m = model()
  const ids = new Set(m.needs.map((n) => n.id))
  if (seenNeeds) {
    const fresh = m.needs.find((n) => !seenNeeds.has(n.id) && !n.receipt)
    if (fresh) toast($, 'needs you: ' + fresh.ask)
  }
  seenNeeds = ids
}

function model() {
  // A receipt older than RECEIPT_MS is gone for good.
  for (const [id, r] of Object.entries(marks.receipts)) if (clockNow - r.at >= RECEIPT_MS) delete marks.receipts[id]
  return buildModel({ board, channels, tasks, seen, marks, now: clockNow })
}

// ------------------------------------------------------------------ the panel

/** The selected list item, kept on an item that exists. */
function current(m) {
  const items = listItems(m)
  if (!items.length) return null
  const at = items.findIndex((i) => i.id === sel)
  // An item that left the list gives its place to the row now there.
  if (at < 0) sel = items[Math.min(selIndex, items.length - 1)].id
  else selIndex = at
  return find(m, sel)
}
let selIndex = 0

function projectOf(m, name) {
  return m.projects.find((p) => p.name === name) || null
}

/** The lines of the panel as it is now. */
function panel() {
  const m = model()
  const w = paneCols
  const h = Math.max(8, paneRows)
  if (!paneFocused) {
    const cur = current(m)
    return panelLines(w, h, { top: topLine(w, 'nokeys'), entries: (lw) => listEntries(m, lw, null, info()), sel, detail: (dw) => detailOf(m, cur, dw), keys: [] })
  }
  if (view === 'help') return [topLine(w, 'help'), { parts: [{ segs: [] }] }, ...helpLines(w), ...keyBar([['b', 'back', 'back']], w)]
  const cur = current(m)
  if (view === 'tasks') {
    const p = projectOf(m, project)
    if (!p) {
      view = 'list'
      return panel()
    }
    const t = p.tasks.find((x) => 't:' + x.id === taskSel) || p.tasks[0]
    if (t) taskSel = 't:' + t.id
    return panelLines(w, h, { top: topLine(w, 'list', 'all projects › ' + p.name), entries: (lw) => taskEntries(p, lw, taskSel), sel: taskSel, detail: (dw) => (t ? taskDetail(t, p, dw) : []), keys: taskKeys(t, p), listShare: 0.4 })
  }
  if (view === 'detail' && cur) {
    const d = detailOf(m, cur, w)
    return [topLine(w, field ? (sendState?.state === 'unconfirmed' ? 'retry' : 'typing') : 'list', crumbOf(cur)), { parts: [{ segs: [] }] }, ...d.slice(0, h - 6), ...keyBar(field ? [] : [...keysFor(cur), ['b', 'back to the list', 'back']], w)]
  }
  const mode = field ? (sendState?.state === 'unconfirmed' ? 'retry' : 'typing') : 'list'
  return panelLines(w, h, {
    top: topLine(w, mode, field ? crumbOf(cur) : ''),
    entries: (lw) => listEntries(m, lw, sel, info()),
    sel,
    detail: (dw) => detailOf(m, cur, dw),
    keys: field ? [] : confirm ? [['y', 'yes', 'yes'], ['n', 'no', 'no']] : keysFor(cur),
    listShare: field ? 0.15 : 0.45,
  })
}

function info() {
  return { seenAt: seen ? clock(seen.at) : null }
}

const cellsWide = (t) => String(t || '').length

function crumbOf(cur) {
  if (!cur?.item) return ''
  return (cur.item.project || '') + (cur.item.ask ? ' › ' + cur.item.ask.slice(0, 30) : '')
}

function keyBar(keys, w) {
  return keys.length ? [{ parts: [{ segs: rule(w) }] }, ...keyLines(keys, w)] : []
}

/** The detail of the selected item, with the field or the confirmation under it. */
function detailOf(m, cur, w) {
  if (!cur || !cur.item) return [{ parts: [{ segs: [S('Nothing selected.', 'dim')] }] }]
  const below = []
  if (confirm && confirm.item === cur.item.id) {
    below.push({ parts: [{ segs: [] }] }, { parts: [{ segs: rule(w, [S(confirm.what === 'approve' ? 'approve' : 'close as done', 'y')]) }] })
    const q = confirm.what === 'approve' ? 'Approve it? The gate closes as done.' : 'Close this task as done?'
    below.push({ parts: [{ segs: [S(q + '  ', 'yb'), S('y', 'kb'), S(' yes   ', 't'), S('n', 'kb'), S(' no', 't')] }] })
  }
  if (field && field.item === cur.item.id) {
    const draft = drafts.get(field.item) || ''
    below.push({ parts: [{ segs: [] }] }, { parts: [{ segs: rule(w, [S((field.kind === 'answer' ? 'answer to ' : 'message to ') + field.to, 'k')]) }] })
    // The field first, the whole draft under it: a line above the field would
    // move it down as the draft grows, and a moved field loses the keys.
    below.push({ parts: [{ input: { key: 'field', value: draft, placeholder: field.kind === 'answer' ? 'type the answer' : 'type the message', label: field.kind } }] })
    if (cellsWide(draft) > w - 16) for (const l of wrap(draft, w - 2)) below.push({ parts: [{ segs: [S('  ' + l, 'dim')] }] })
    if (sendState?.state === 'sending') below.push({ parts: [{ segs: [S('◷ sending…', 'c')] }] })
    if (sendState?.state === 'unconfirmed') below.push({ parts: [{ segs: [S('◷ not confirmed: ' + sendState.reason + '. ', 'o'), S('enter', 'kb'), S(' checks, then sends only if needed', 't')] }] })
    if (sendState?.state === 'refused') below.push({ parts: [{ segs: [S('! refused: ' + sendState.reason, 'r')] }] })
  }
  if (cur.kind === 'need') return needDetail(cur.item, projectOf(m, cur.item.project), w, { below })
  if (cur.kind === 'quiet') return quietDetail(cur.item, projectOf(m, cur.item.project), w)
  if (cur.kind === 'ship') return shipDetail(cur.item, w)
  if (cur.kind === 'change') return changeDetail(cur.item, w)
  if (cur.kind === 'project') return projectDetail(cur.item, w)
  return []
}

const NAV = [['j', 'down', 'move', 1], ['k', 'up', 'move', -1], ['1', 'needs', 'section', 1], ['2', 'offline', 'section', 2], ['3', 'PRs', 'section', 3], ['4', 'changed', 'section', 4], ['5', 'projects', 'section', 5]]
/** In a narrow panel the keys keep their verb and lose their target, so all of them fit. */
const narrow = () => paneCols < 80
const who = (name) => (narrow() ? '' : ' ' + name)

/** The keys of the selected item, then the moves. */
function keysFor(cur) {
  const keys = []
  const it = cur?.item
  if (cur?.kind === 'need' && it && !it.receipt) {
    if (it.verb === 'approve') keys.push(['y', 'approve', 'approve'])
    if (it.verb === 'answer' || it.verb === 'decide') keys.push(['r', 'answer' + who(it.worker || it.owner || 'the task'), 'answer'])
    if (it.verb === 'unblock') keys.push(['u', 'retry the spawn', 'retry'])
    if (it.owner) keys.push(['a', narrow() ? 'ask' : 'ask ' + it.owner + ' for a recommendation', 'ask'])
    if (it.worker) keys.push(['m', 'message' + who(it.worker), 'message'], ['g', 'attach' + who('to ' + it.worker), 'attach'])
    if (it.task) keys.push(['d', 'close as done', 'done'])
  }
  if (cur?.kind === 'quiet') keys.push(['u', 'resume ' + it.who, 'retry'])
  if (cur?.kind === 'ship') keys.push(['o', 'open ' + it.pr.label, 'pr'], ...(it.who ? [['g', 'attach to ' + it.who, 'attach']] : []))
  if (cur?.kind === 'project') keys.push(['enter', 'its tasks', null])
  if (it?.project && cur?.kind !== 'project') keys.push(['p', 'project' + who(it.project), 'project'])
  if (!narrow()) keys.push(['↑↓', 'move', null])
  keys.push(...NAV)
  if (model().changes.length) keys.push(['x', narrow() ? 'seen' : 'mark changes seen', 'seen'])
  return keys
}

function taskKeys(t, p) {
  const keys = []
  const need = t && model().needs.find((n) => n.task === t.id && !n.receipt)
  if (need) {
    if (need.verb === 'approve') keys.push(['y', 'approve', 'approve'])
    if (need.verb === 'answer' || need.verb === 'decide') keys.push(['r', 'answer' + who(need.worker || need.owner || ''), 'answer'])
    if (need.owner) keys.push(['a', narrow() ? 'ask' : 'ask ' + need.owner + ' for a recommendation', 'ask'])
  }
  if (t?.pr) keys.push(['o', 'open ' + t.pr.label, 'pr'])
  if (t?.who) keys.push(['g', 'attach' + who('to ' + t.who), 'attach'], ['m', 'message' + who(t.who), 'message'])
  if (t && t.state !== 'done') keys.push(['d', 'close as done', 'done'])
  keys.push(['b', narrow() ? 'back' : 'back to all projects', 'back'], ['j', 'down', 'move', 1], ['k', 'up', 'move', -1])
  return keys
}

/** The item a key acts on: the task in the tasks view, else the selected item. */
function target(m) {
  if (view === 'tasks') {
    const p = projectOf(m, project)
    const t = p && p.tasks.find((x) => 't:' + x.id === taskSel)
    // A task that needs the person acts as its needs-you item does.
    const need = t && m.needs.find((n) => n.task === t.id && !n.receipt)
    if (need) return { kind: 'need', item: need }
    if (t) return { kind: 'task', item: { id: taskSel, channel: t.channel, task: t.id, title: t.title, ask: t.title, worker: t.who, owner: p.orchestrators[0]?.agent || '', project: p.name, pr: t.pr, who: t.who } }
  }
  return current(m)
}

/** A press on a Button of the band or the panel. */
async function press($, btn) {
  try {
    await dispatch($, btn.act, btn.arg)
  } catch {
    // A press is never worth an error.
  }
  redraw($)
  ringSoon($)
}

/** After the redraw, put the ring on the selected row: a row that was not
 * drawn yet (scrolled out, or a new view) cannot take it before. */
function ringSoon($) {
  if (!paneOpen || field) return
  try {
    $.clock.after(30, () => {
      const id = view === 'tasks' ? taskSel : sel
      if (id && !field) Promise.resolve($.ui.focus({ requestId: PANE, key: 'row:' + id })).catch(() => {})
    })
  } catch {
    // The row's autoFocus still asks for the ring.
  }
}

async function dispatch($, actName, arg) {
  const m = model()
  if (actName === 'open') return openPanel($)
  if (actName === 'select') {
    if (!paneOpen) {
      sel = arg
      view = 'list'
      return openPanel($)
    }
    if (String(arg).startsWith('t:')) {
      taskSel = arg
      return
    }
    sel = arg
    if (arg.startsWith('p:')) {
      project = arg.slice(2)
      view = 'tasks'
      taskSel = null
      return
    }
    view = view === 'detail' ? 'list' : 'detail'
    return
  }
  if (actName === 'move') return move($, m, arg)
  if (actName === 'section') {
    const first = listItems(m).find((i) => i.section === arg)
    if (first) {
      sel = first.id
      view = 'list'
      focusRow($, sel)
    } else toast($, 'nothing in that section')
    return
  }
  if (actName === 'back') {
    if (view === 'help') view = viewBeforeHelp
    else view = 'list'
    return
  }
  if (actName === 'help') {
    if (view === 'help') view = viewBeforeHelp
    else {
      viewBeforeHelp = view
      view = 'help'
    }
    return
  }
  if (actName === 'seen') {
    tasksAt = 0
    await refresh($, true)
    seen = snapshot(tasks, clockNow)
    marks.changes = []
    store($, 'seen', seen)
    toast($, 'changes marked seen at ' + clock(clockNow))
    return
  }
  if (actName === 'no') {
    confirm = null
    return
  }
  const cur = target(m)
  if (!cur?.item) return
  const it = cur.item
  if (actName === 'project') {
    project = it.project
    view = 'tasks'
    taskSel = null
    return
  }
  if (actName === 'answer' || actName === 'message') {
    const to = actName === 'answer' ? it.worker || it.owner : it.worker || it.who
    field = { kind: actName, item: it.id, to, channel: it.channel, task: it.task }
    sendState = null
    // The ring must move onto the field, or the letters typed next go to the prompt.
    redraw($)
    try {
      $.clock.after(50, () => {
        Promise.resolve($.ui.focus({ requestId: PANE, key: 'field' })).catch(() => {})
      })
    } catch {
      // The field's autoFocus still asks for the ring.
    }
    return
  }
  if (actName === 'approve' || actName === 'done') {
    confirm = { what: actName, item: it.id, channel: it.channel, task: it.task, title: it.ask || it.title }
    return
  }
  if (actName === 'yes' && confirm) {
    const c = confirm
    confirm = null
    const out = await act($, config, ACTIONS.close(c.channel, c.task, c.what === 'approve' ? 'Approved by the person from the chief mod.' : 'Closed as done by the person from the chief mod.'))
    if (out.ok) {
      receipt(m, c.item, (c.what === 'approve' ? '✓ approved ' : '✓ closed ') + clock(clockNow) + (c.what === 'approve' ? ' · the gate is closed' : ''), c.what === 'approve' ? 'approved' : 'done', c.title, c.task)
      staleChannels.add(c.channel)
      await refresh($, true)
    } else toast($, (c.what === 'approve' ? 'approve' : 'close') + ' failed: ' + out.reason)
    return
  }
  if (actName === 'ask') return askOwner($, m, it)
  if (actName === 'pr') {
    const url = it.pr?.url
    if (!url) return toast($, 'no PR on this item')
    const out = await act($, config, ACTIONS.open(url))
    return toast($, out.ok ? 'opened ' + it.pr.label + ' in the browser' : 'could not open ' + it.pr.label + ': ' + out.reason)
  }
  if (actName === 'attach') {
    const agent = it.worker || it.who
    return toast($, 'Borant cannot open a terminal yet. In a terminal, run: borant attach ' + agent)
  }
  if (actName === 'retry') {
    const agent = it.agent || it.who
    const out = await act($, config, ACTIONS.resume(agent))
    toast($, out.ok ? 'resumed ' + agent : 'resume failed: ' + out.reason)
    if (out.ok) await refresh($, true)
  }
}

/** Mark an item done by the person: a receipt row, and a change. */
function receipt(m, id, text, verb, title, task) {
  const index = m.needs.findIndex((n) => n.id === id)
  const item = m.needs[index]
  marks.receipts[id] = { text, at: clockNow, item, index }
  marks.changes.push({ id: 'c:you:' + id + ':' + clockNow, verb, project: item?.project || '', text: (title || '') + ' (you)', who: 'you', at: clockNow, task: task || item?.task || null })
}

function move($, m, delta) {
  if (view === 'tasks') {
    const p = projectOf(m, project)
    const ids = p ? p.tasks.map((t) => 't:' + t.id) : []
    if (!ids.length) return
    const at = Math.max(0, ids.indexOf(taskSel))
    taskSel = ids[(at + delta + ids.length) % ids.length]
    return focusRow($, taskSel)
  }
  const ids = listItems(m).map((i) => i.id)
  if (!ids.length) return
  const at = Math.max(0, ids.indexOf(sel))
  sel = ids[(at + delta + ids.length) % ids.length]
  if (view === 'detail') view = 'list'
  focusRow($, sel)
}

function focusRow($, id) {
  try {
    Promise.resolve($.ui.focus({ requestId: PANE, key: 'row:' + id })).catch(() => {})
  } catch {
    // The selection still shows.
  }
}

/** The answer or message field: Enter sends. */
async function submitField($, value) {
  if (!field) return
  const f = field
  const text = String(value ?? drafts.get(f.item) ?? '').trim()
  if (!text) return
  drafts.set(f.item, text)
  // After an unconfirmed send, check first: a task that left needs-you got it.
  if (sendState?.state === 'unconfirmed' && f.kind === 'answer') {
    await refresh($, true)
    const still = model().needs.some((n) => n.id === f.item && !n.receipt)
    if (!still) return sent($, f, text)
  }
  sendState = { state: 'sending' }
  redraw($)
  const out = await act($, config, f.kind === 'answer' ? ACTIONS.answer(f.channel, f.task, text) : ACTIONS.send(f.to, text))
  if (out.ok) return sent($, f, text)
  sendState = out.timeout ? { state: 'unconfirmed', reason: 'the hub did not reply in 8 s' } : { state: 'refused', reason: out.reason }
  redraw($)
}

async function sent($, f, text) {
  const m = model()
  drafts.delete(f.item)
  field = null
  sendState = null
  if (f.kind === 'answer') {
    const item = m.needs.find((n) => n.id === f.item)
    receipt(m, f.item, '✓ answered ' + clock(clockNow) + ' · sent to ' + f.to, 'answered', item ? item.ask : '', f.task)
  } else toast($, 'sent to ' + f.to + ': ' + text.slice(0, 40))
  if (f.channel) staleChannels.add(f.channel)
  await refresh($, true)
  redraw($)
}

/** Ask: the question goes to the orchestrator as the person; the chief is told. */
async function askOwner($, m, it) {
  const owner = it.owner
  if (!owner) return toast($, 'no orchestrator owns this item')
  const what = it.ask || it.title
  const text = 'Chris asks about "' + what + '" (' + it.channel + (it.task ? '/' + it.task : '') + '), from the chief\'s board: what does it need from him, and what do you recommend? Answer the chief, who relays it to Chris.'
  const out = await act($, config, ACTIONS.send(owner, text))
  if (!out.ok) return toast($, 'ask failed: ' + out.reason)
  marks.asked[it.id] = { owner, time: clock(clockNow) }
  await tell($, 'The person pressed "ask" on "' + what + '" in the board. The board sent the question to ' + owner + ' as a direct message. Do not answer it yourself; relay ' + owner + "'s reply.")
  toast($, 'asked ' + owner + ' about ' + what.slice(0, 40))
}

async function openPanel($) {
  if (!used) {
    used = true
    store($, 'used', true)
  }
  try {
    const term = terminalColumns()
    const opened = await $.ui.open({ id: PANE, title: PANE_TITLE, columns: Math.max(24, Math.floor(term * 0.45)), rows: Math.max(12, Math.min(28, (maxRows || 14) + 10)), focus: true, closeOnEscape: true })
    if (opened && opened.isPlaced === false) return false
    paneOpen = true
    redraw($)
    return true
  } catch {
    return false
  }
}

// ------------------------------------------------------------------ drawing

function terminalColumns() {
  return columns || 120
}

function measure(e) {
  const c = e.viewport && e.viewport.columns
  if (typeof c === 'number' && c > 0 && e.component === 'AbovePrompt' && !paneOpen) columns = c
}

function widthOf(e) {
  const body = e.props && e.props.bodyColumns
  if (typeof body === 'number' && body > 0) return body
  return columns > 0 ? columns - 5 : 96
}

function styleProps(style) {
  switch (style) {
    case 'dim':
      return { dimColor: true }
    case 'faint':
      return { dimColor: true }
    case 'bold':
    case 'head':
      return { bold: true }
    case 'k':
      return { color: 'suggestion' }
    case 'kb':
      return { color: 'suggestion', bold: true }
    case 'keyson':
      return { color: 'suggestion', bold: true, inverse: true }
    case 'y':
      return { color: 'yellow' }
    case 'yb':
      return { color: 'yellow', bold: true }
    case 'r':
      return { color: 'red' }
    case 'rb':
      return { color: 'red', bold: true }
    case 'g':
      return { color: 'green' }
    case 'b':
      return { color: 'blue' }
    case 'c':
      return { color: 'cyan' }
    case 'm':
      return { color: 'magenta' }
    case 'o':
      return { color: '#d7875f' }
    default:
      return {}
  }
}

function texts($, e, segs, keyBase, selected) {
  const { Text } = $.ui.resolve(e)
  // An empty line still takes its row.
  if (!segs.length || segs.every((s) => s.text === '')) return [Text({ key: keyBase + '-0', children: [' '] })]
  return segs.map((s, j) => {
    const props = { key: keyBase + '-' + j, children: [s.text], ...styleProps(s.style) }
    if (selected) {
      props.dimColor = false
      if (!props.color) props.color = 'text'
    }
    return Text(props)
  })
}

/** Lines as a column of row Boxes: text, plain Buttons, and the answer Input. */
function tree($, e, lines, inPane = false) {
  const { Box, Button, Input } = $.ui.resolve(e)
  const ringRow = inPane ? 'row:' + (view === 'tasks' ? taskSel : sel) : null
  return Box({
    flexDirection: 'column',
    children: lines.map((l, i) =>
      Box({
        key: 'line-' + i,
        flexDirection: 'row',
        ...(l.sel ? { backgroundColor: SELECTED_BG } : {}),
        children: (l.parts || []).map((p, j) => {
          const kb = 'l' + i + '-' + j
          if (p.input) {
            return Input({
              key: p.input.key,
              label: p.input.label,
              placeholder: p.input.placeholder,
              value: p.input.value,
              submitLabel: 'send',
              autoFocus: true,
              onInput: (value) => {
                if (field) drafts.set(field.item, value)
                redraw($)
              },
              onSubmit: (value) => submitField($, value),
            })
          }
          if (p.btn) {
            const props = { key: p.btn.key, label: p.btn.label || p.btn.key, plain: true, onPress: () => press($, p.btn) }
            if (p.btn.hotkey) props.hotkey = p.btn.hotkey
            if (p.btn.action) props.action = p.btn.action
            if (!p.btn.hotkey) props.children = texts($, e, p.segs, kb, l.sel)
            if (!field && p.btn.key === ringRow) props.autoFocus = true
            return Button(props)
          }
          return Box({ key: kb, flexDirection: 'row', children: texts($, e, p.segs || [], kb, l.sel) })
        }),
      }),
    ),
  })
}

// ------------------------------------------------------------------ helpers

async function tell($, text) {
  try {
    await $.session.append({ message: { type: 'user', content: [{ type: 'text', text: '[borant chief mod] ' + text }] } })
  } catch {
    // Nothing to tell it with.
  }
}

function toast($, text) {
  try {
    $.ui.toast(text)
  } catch {
    // A toast is never worth an error.
  }
}

function store($, key, value) {
  try {
    $.store.set(key, value).catch(() => {})
  } catch {
    // No store: it lasts this session only.
  }
}

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

function redrawSoon($) {
  try {
    $.clock.after(0, () => redraw($))
  } catch {
    redraw($)
  }
}
