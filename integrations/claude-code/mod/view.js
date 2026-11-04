// The chief mod's views: pure functions from the model (mod/model.js) to
// lines that register.js turns into Box, Text, Button, and Input elements.
// No mods API here, so the tests call it with plain values.
//
// A line is `{ parts, sel }`. A part is `{ segs }` (text), `{ segs, btn }` (a
// plain Button around the text: `btn` is `{ key, act, arg, hotkey?, action? }`),
// or `{ input }` (the answer field). A segment is `{ text, style }`. Every
// line is exactly as wide as asked, so no column moves and nothing wraps.

import { ageText, cellsOf, truncate } from './board.js'

export const S = (text, style = 't') => ({ text: String(text), style })

export const NEED_MARK = { unblock: ['⊘', 'r'], decide: ['■', 'r'], answer: ['▲', 'y'], approve: ['▲', 'y'] }
export const TASK_MARK = { wait: ['▲', 'y'], stuck: ['■', 'r'], run: ['●', 'b'], silent: ['◌', 'o'], ready: ['○', 't'], blocked: ['⊘', 'r'], later: ['◷', 'dim'], done: ['✓', 'g'] }
export const SHIP_WORDS = { run: 'in progress', wait: 'waits on you', stuck: 'stuck', ready: 'open, nobody on it', blocked: 'blocked', later: 'later' }
export const PROJECT_MARK = { stuck: ['■', 'r'], blocked: ['⊘', 'r'], wait: ['▲', 'y'], offline: ['◌', 'o'], silent: ['◌', 'o'], run: ['●', 'b'] }

export function width(segs) {
  return segs.reduce((n, s) => n + cellsOf(s.text), 0)
}

/** Cut or pad segments to exactly `w` cells; a cut ends in "…". */
export function fit(segs, w) {
  const out = []
  let used = 0
  for (const s of segs) {
    const cw = cellsOf(s.text)
    if (used + cw <= w) {
      out.push(s)
      used += cw
      continue
    }
    const room = w - used
    if (room > 0) {
      const cut = truncate(s.text, room)
      out.push({ ...s, text: cut })
      used += cellsOf(cut)
    }
    break
  }
  if (used < w) out.push(S(' '.repeat(w - used)))
  return out
}

/** Cells of fixed widths: [[segs, w], ...]; w 0 takes the rest. One blank cell between cells. */
export function cols(cells, total) {
  const fixed = cells.reduce((n, [, w]) => n + (w || 0), 0)
  return cells.flatMap(([segs, w], i) => {
    const cw = w || Math.max(0, total - fixed)
    return i === cells.length - 1 ? fit(segs, cw) : [...fit(segs, Math.max(0, cw - 1)), S(' ')]
  })
}

/** Left and right on one line of `w` cells. */
export function spread(left, right, w) {
  return [...fit(left, Math.max(0, w - width(right))), ...right]
}

export function rule(w, label = null) {
  if (!label) return [S('─'.repeat(w), 'faint')]
  return [S('── ', 'faint'), ...label, S(' ' + '─'.repeat(Math.max(0, w - width(label) - 4)), 'faint')]
}

/** Chips on one line of `w` cells; what does not fit becomes "+n". */
export function fitChips(list, w, gap = '  ') {
  const out = []
  let used = 0
  let i = 0
  for (; i < list.length; i++) {
    const sep = out.length ? [S(gap)] : []
    const rest = list.length - i - 1
    const tail = rest ? cellsOf(gap + '+' + rest) : 0
    if (used + width(sep) + width(list[i]) + tail > w) break
    out.push(...sep, ...list[i])
    used += width(sep) + width(list[i])
  }
  if (!out.length && list.length) {
    // Not even the first chip fits: cut it, so a name shows and not "+n" alone.
    const rest = list.length - 1
    const tail = rest ? gap + '+' + rest : ''
    return [...fit(list[0], Math.max(4, w - cellsOf(tail))), ...(rest ? [S(tail, 'dim')] : [])]
  }
  if (i < list.length) out.push(S((out.length ? gap : '') + '+' + (list.length - i), 'dim'))
  return out
}

/** Chips over at most `max` lines of `w` cells, after a lead. */
export function chipLines(list, w, max, lead = []) {
  const lines = []
  let i = 0
  while (i < list.length && lines.length < max) {
    const line = lines.length ? [S(' '.repeat(width(lead)))] : [...lead]
    let used = width(line)
    let n = 0
    const last = lines.length === max - 1
    while (i < list.length) {
      const sep = n ? [S('   ')] : []
      const tail = last && list.length - i - 1 ? 9 : 0
      if (used + width(sep) + width(list[i]) + tail > w) break
      line.push(...sep, ...list[i])
      used += width(sep) + width(list[i])
      i++
      n++
    }
    if (!n) {
      // One chip wider than the line: cut it.
      line.push(...list[i])
      i++
    }
    if (last && i < list.length) line.push(S('   +' + (list.length - i) + ' more', 'dim'))
    lines.push(fit(line, w))
  }
  return lines
}

/** Wrap text to `w` cells. */
export function wrap(text, w) {
  const out = []
  let line = ''
  for (const word of String(text).split(/\s+/).filter(Boolean)) {
    if (line && cellsOf(line + ' ' + word) > w) {
      out.push(line)
      line = word
    } else line = line ? line + ' ' + word : word
  }
  if (line) out.push(line)
  return out.length ? out : ['']
}

const age = (ms) => (ms === null || ms === undefined ? '' : ageText(ms))
const text = (segs) => ({ parts: [{ segs }] })
const line = (parts, sel = false) => ({ parts, sel })

// ---------------------------------------------------------------- the band

/** The hotkey as the band and the footer show it. */
export function hotParts(w) {
  const open = { segs: [S('ctrl+x b', 'kb')], btn: { key: 'open', act: 'open', action: 'app:cycleDiffBase', label: 'ctrl+x b' } }
  const tail = w >= 150 ? [S(' or ', 'dim'), S('/borant', 'kb'), S(' Borant keys', 'dim')] : [S(' · ', 'dim'), S('/borant', 'kb')]
  return [open, { segs: tail }]
}

const quietWords = (m) => {
  const silent = m.quiet.filter((q) => q.verb === 'silent').length
  const offline = m.quiet.length - silent
  return silent ? silent + ' silent, ' + offline + ' offline' : offline + ' offline'
}

/** The band's first line. state: rest | on | nokeys | down */
export function header(m, w, state = 'rest', info = {}) {
  const counts = [
    S(m.open + ' need you', 'yb'),
    S('  ·  ', 'faint'),
    S(quietWords(m), m.quiet.length ? 'o' : 'dim'),
    S('  ·  ', 'faint'),
    S(m.changes.length + (w < 90 ? ' new' : ' changed since ' + (info.seenAt || '…')), 'c'),
  ]
  const more = w >= 150 ? [S('  ·  ', 'faint'), S(m.projects.length + ' projects with work, ' + m.agents + ' agents', 'dim')] : []
  // Two blank cells before the right side, so a cut left side never touches it.
  const gap = { segs: [S('  ')] }
  const hot = [gap, ...hotParts(w)]
  const right = (parts) => width(parts.flatMap((p) => p.segs))
  if (state === 'on') {
    const r = [gap, { segs: w >= 150 ? [S('esc', 'kb'), S(' keys back to the prompt', 'dim')] : [S('esc', 'kb'), S(' close', 'dim')] }]
    return line([{ segs: fit([S(' KEYS ON BORANT ', 'keyson'), S('  '), ...counts], w - right(r)) }, ...r])
  }
  if (state === 'down') {
    return line([{ segs: fit([S('Borant ', 'head'), S('✕ no hub since ' + (info.downSince || '?') + ': ', 'rb'), S(info.reason || 'the read failed', 'r'), S('  ·  showing the last good read, ' + (info.lastOk || '?'), 'o')], w - right(hot)) }, ...hot])
  }
  if (state === 'nokeys') {
    const r = [gap, { segs: [S('keys on the prompt', 'o')] }, ...hot]
    return line([{ segs: fit([S('Borant ', 'head'), ...counts], w - right(r)) }, ...r])
  }
  return line([{ segs: fit([S('Borant ', 'head'), ...counts, ...more], w - right(hot)) }, ...hot])
}

function needCells(n, w) {
  const narrow = w < 150
  const [g, st] = n.receipt ? ['✓', 'g'] : NEED_MARK[n.verb]
  const ask = n.receipt ? [S(n.receipt, 'g')] : [...(n.asked ? [S('asked ' + n.asked.owner + ' ' + n.asked.time + ' · ', 'c')] : []), S(n.ask, 't'), ...(n.detail ? [S(' · ' + n.detail, 'dim')] : [])]
  const cells = [
    [[S(g + ' ', st), S(n.verb, n.receipt ? 'dim' : st)], 10],
    [[S(n.project, n.receipt ? 'dim' : 'bold')], 16],
    [ask, 0],
  ]
  if (!narrow) cells.push([[S(n.receipt ? '' : n.why, st)], 19])
  cells.push([[S(n.who, 'dim')], narrow ? 13 : 19])
  cells.push([[S(n.receipt ? '' : age(n.age).padStart(3), 'dim')], 4])
  return cols(cells, w)
}

function quietCells(q, w) {
  return cols([[[S('◌ ', 'o'), S(q.verb, 'o')], 10], [[S(q.project, 'bold')], 16], [[S(q.ask, 'dim')], 0], ...(w >= 150 ? [[[], 19]] : []), [[S(q.who, 'dim')], w < 150 ? 13 : 19], [[S(age(q.age).padStart(3), 'dim')], 4]], w)
}

/** A change's mark and color: + new, ! a failure, ✓ the rest. */
export function changeMark(verb) {
  if (verb === 'new') return ['+', 'c']
  if (/failed|blocked|offline/.test(verb)) return ['!', 'r']
  return ['✓', 'g']
}
const changeChip = (c) => [S(changeMark(c.verb)[0] + ' ', changeMark(c.verb)[1]), S(c.project + ' ', 'bold'), S(c.verb + ' ', changeMark(c.verb)[1]), S(c.text, 'dim')]

function changeLines(m, w, max, info) {
  if (!m.changes.length) return [fit([S('changed  ', 'c'), S(info.seenAt ? 'nothing since ' + info.seenAt : 'nothing yet: x in the panel marks what you saw', 'dim')], w)]
  return chipLines(m.changes.map(changeChip), w, max, [S('changed  ', 'c')])
}

/** A project's line(s): working tasks (who, how long), open tasks by name, its PR. */
function projectLines(p, w, opts = {}) {
  const narrow = w < 150
  const active = p.tasks.filter((t) => ['wait', 'stuck', 'run', 'silent'].includes(t.state))
  const work = active.map((t) => [S(TASK_MARK[t.state][0] + ' ', TASK_MARK[t.state][1]), S(t.title, 't'), S(' ' + (t.who || 'nobody') + (t.age !== null ? ' ' + age(t.age) : ''), 'dim')])
  const open = [
    ...p.tasks.filter((t) => t.state === 'ready').map((t) => [S('○ ', 't'), S(t.title, t.isNew ? 'c' : 't')]),
    ...p.tasks.filter((t) => t.state === 'blocked').map((t) => [S('⊘ ', 'r'), S(t.title, 'dim')]),
    ...p.tasks.filter((t) => t.state === 'later').map((t) => [S('◷ ', 'dim'), S(t.title, 'dim')]),
  ]
  const [g, gs] = PROJECT_MARK[p.mark]
  const nameW = 18
  const openW = w < 90 ? 0 : narrow ? 30 : 44
  const prW = narrow ? 0 : 26
  const workW = w - nameW - openW - prW
  // The same PR the global list puts first for this project.
  const pr = (opts.ships || []).find((s) => s.project === p.name) || p.tasks.find((t) => t.pr && t.state !== 'done')
  const rows = opts.wide && work.length ? chipLines(work, workW - 1, 4) : [work.length ? fitChips(work, workW - 1) : [S('nobody working' + (p.orchOffline ? ': orchestrator offline' : ''), p.orchOffline ? 'o' : 'dim')]]
  return rows.map((wk, i) => {
    const cells = [
      [i ? [] : [S(g + ' ', gs), S(p.name, 'bold')], nameW],
      [wk, workW],
    ]
    if (openW) cells.push([i ? [] : open.length ? fitChips(open, openW - 1, ' ') : [S('nothing unassigned', 'faint')], openW])
    if (prW) cells.push([i ? [] : pr ? [S('⇡ ' + pr.pr.label + ' ', 'm'), ...(pr.pr.words && pr.pr.words !== 'open' ? [S(pr.pr.words, prStyle(pr.pr.words))] : [S(pr.title, 'dim')])] : [S('no open PR', 'faint')], prW])
    return cols(cells, w)
  })
}

/** The color of a PR's state. */
export function prStyle(words) {
  if (/ready|merged|pass|approved/.test(words)) return 'g'
  if (/failed|changes/.test(words)) return 'r'
  if (/review/.test(words)) return 'y'
  return 'dim'
}

function projectHead(w) {
  const narrow = w < 150
  const cells = [[[S('  PROJECT', 'dim')], 18], [[S('WORKING NOW: task, who, for how long', 'dim')], 0]]
  if (w >= 90) cells.push([[S('OPEN: ○ ready  ⊘ blocked  ◷ later', 'dim')], narrow ? 30 : 44])
  if (!narrow) cells.push([[S('OPEN PR', 'dim')], 26])
  return cols(cells, w)
}

/**
 * The band's lines at `w` cells. `opts`: { state, sel, maxRows, info, tip }.
 * Under 150 cells: needs you, one line of changes, projects. From 250:
 * triage and PRs on the left, projects on the right.
 */
export function bandLines(m, w, opts = {}) {
  const { state = 'rest', sel = null, info = {}, maxRows = 99 } = opts
  const out = [header(m, w, state, info)]
  if (opts.tip) out.push(text(spread([S('New: ', 'kb'), S('ctrl+x b', 'kb'), S(' (or ', 't'), S('/borant', 'kb'), S(') gives Borant the keys: the panel opens. ', 't'), S('h', 'kb'), S(' there lists every key, ', 't'), S('esc', 'kb'), S(' gives them back.', 't')], [S('shown until you use it', 'faint')], w)))
  if (opts.headerOnly) return out
  const btn = (id) => ({ key: 'band:' + id, act: 'select', arg: id })
  const row = (id, segs) => line([{ segs, btn: btn(id) }], sel === id)
  if (w >= 250) {
    const lw = Math.floor((w - 3) * 0.5)
    const rw = w - 3 - lw
    const left = [
      ...m.needs.map((n) => ({ id: n.id, segs: needCells(n, lw) })),
      ...m.quiet.map((q) => ({ id: q.id, segs: quietCells(q, lw) })),
      ...changeLines(m, lw, 2, info).map((segs) => ({ segs })),
      ...m.ships.slice(0, 4).map((s) => ({ id: s.id, segs: cols([[[S('⇡ ', 'm'), S('open PR', 'm')], 10], [[S(s.project, 'bold')], 16], [[S(s.pr.label + ' ', 'm'), S(s.title, 't')], 0], [[S(s.pr.words && s.pr.words !== 'open' ? s.pr.words : SHIP_WORDS[s.state] || '', s.pr.words && s.pr.words !== 'open' ? prStyle(s.pr.words) : 'dim')], 19], [[S(s.who, 'dim')], 19], [[], 4]], lw) })),
    ]
    const right = [{ segs: projectHead(rw) }, ...m.projects.flatMap((p) => projectLines(p, rw, { wide: true, ships: m.ships }).map((segs, i) => ({ id: i ? null : 'p:' + p.name, segs }))), { segs: fit([S('  idle: ' + (m.idle.join(', ') || 'none'), 'faint')], rw) }]
    for (let i = 0; i < Math.max(left.length, right.length); i++) {
      const l = left[i] || { segs: fit([], lw) }
      const r = right[i] || { segs: fit([], rw) }
      const parts = [l.id ? { segs: l.segs, btn: btn(l.id) } : { segs: l.segs }, { segs: [S(' │ ', 'faint')] }, r.id ? { segs: r.segs, btn: btn(r.id) } : { segs: r.segs }]
      out.push(line(parts, (l.id && l.id === sel) || (r.id && r.id === sel)))
    }
    return out.slice(0, maxRows)
  }
  out.push(...m.needs.map((n) => row(n.id, needCells(n, w))))
  if (w >= 150) out.push(...m.quiet.map((q) => row(q.id, quietCells(q, w))))
  out.push(...changeLines(m, w, w >= 150 ? 2 : 1, info).map(text))
  if (w >= 150) out.push(text(projectHead(w)))
  for (const p of m.projects) projectLines(p, w, { ships: m.ships }).forEach((segs, i) => out.push(i ? text(segs) : row('p:' + p.name, segs)))
  if (w >= 150) out.push(text(fit([S('  idle: ' + (m.idle.join(', ') || 'none'), 'faint')], w)))
  if (out.length > maxRows) {
    const cut = out.slice(0, maxRows - 1)
    cut.push(text(fit([S('  +' + (out.length - maxRows + 1) + ' more rows: ctrl+x b shows them all', 'dim')], w)))
    return cut
  }
  return out
}

// ---------------------------------------------------------------- the panel

/** The panel's top line. mode: list | typing | retry | help | down | nokeys.
 * Every key it names is a Button with that hotkey: a key no Button holds
 * goes to the prompt, and takes the keys with it. */
export function topLine(w, mode = 'list', crumb = '') {
  if (mode === 'nokeys') return text(fit([S('keys stay on the prompt. ', 'o'), S('Over an empty prompt, ', 't'), S('ctrl+x b', 'kb'), S(' gives them to Borant.', 't')], w))
  const key = (k, label, act) => ({ segs: [S(k, 'kb'), S(': ' + label, 'dim')], btn: { key: 'key:' + k, act, hotkey: k, label } })
  const chip = { segs: [S(' KEYS ON BORANT ', 'keyson')] }
  let left
  let right
  if (mode === 'typing' || mode === 'retry') {
    left = [S(' ANSWER ', 'keyson'), S('  ' + crumb, 'dim')]
    right = [{ segs: mode === 'retry' ? [S('enter', 'kb'), S(' checks, then sends  ', 'dim'), S('esc', 'kb'), S(' keeps the draft', 'dim')] : [S('enter', 'kb'), S(' sends  ', 'dim'), S('esc', 'kb'), S(' keeps the draft', 'dim')] }]
    return line([{ segs: fit(left, w - width(right[0].segs)) }, ...right])
  }
  if (mode === 'help') {
    right = [key('h', 'back', 'help'), { segs: [S('  ')] }, { segs: [S('esc', 'kb'), S(' close', 'dim')] }]
    return line([chip, { segs: fit([S('  all keys', 'dim')], w - width(chip.segs) - width(right.flatMap((p) => p.segs))) }, ...right])
  }
  right = [{ segs: [S('  ')] }, key('h', 'all keys', 'help'), { segs: [S('  ')] }, { segs: [S('esc', 'kb'), S(' close', 'dim')] }]
  return line([chip, { segs: fit([S('  ' + crumb, 'dim')], w - width(chip.segs) - width(right.flatMap((p) => p.segs))) }, ...right])
}

/** The panel's list: one line per item, under numbered section heads. Each entry: { id?, line }. */
export function listEntries(m, w, sel, info = {}) {
  const out = []
  const head = (n, t, st, right = []) => out.push({ line: text(spread([S(n + ' ', 'kb'), S(t, st)], right, w)) })
  const item = (id, segs) => out.push({ id, line: line([{ segs: fit(segs, w), btn: { key: 'row:' + id, act: 'select', arg: id } }], id === sel) })
  head(1, 'NEEDS YOU · ' + m.open, 'yb')
  for (const n of m.needs) {
    const [g, st] = n.receipt ? ['✓', 'g'] : NEED_MARK[n.verb]
    item(n.id, cols([[[S(' ' + g + ' ', st), S(n.verb, n.receipt ? 'dim' : st)], 11], [[S(n.project, n.receipt ? 'dim' : 'bold')], 15], [n.receipt ? [S(n.receipt, 'g')] : [...(n.asked ? [S('asked ' + n.asked.time + ' · ', 'c')] : []), S(n.ask, 't')], 0], [[S(n.receipt ? '' : age(n.age).padStart(3), 'dim')], 4]], w))
  }
  if (!m.needs.length) out.push({ line: text(fit([S('   nothing needs you', 'faint')], w)) })
  head(2, 'QUIET · ' + m.quiet.length, 'o')
  for (const q of m.quiet) item(q.id, cols([[[S(' ◌ ', 'o'), S(q.verb, 'o')], 11], [[S(q.project, 'bold')], 15], [[S(q.ask, 'dim')], 0]], w))
  head(3, 'OPEN PRS · ' + m.ships.length, 'm')
  for (const s of m.ships) item(s.id, cols([[[S(' ⇡ ', 'm'), S(s.pr.label, 'm')], 11], [[S(s.title, 't')], 0], ...(s.pr.words && s.pr.words !== 'open' ? [[[S(s.pr.words, prStyle(s.pr.words))], 18]] : []), [[S(s.project, 'dim')], 15]], w))
  head(4, 'CHANGED SINCE ' + (info.seenAt || '…') + ' · ' + m.changes.length, 'c', m.changes.length ? [S('x', 'kb'), S(' marks them seen', 'dim')] : [])
  for (const c of m.changes) item(c.id, cols([[[S(' ' + changeMark(c.verb)[0] + ' ', changeMark(c.verb)[1]), S(c.verb, changeMark(c.verb)[1])], 13], [[S(c.project, 'bold')], 15], [[S(c.text, 'dim')], 0], [[S(age(c.age).padStart(3), 'dim')], 4]], w))
  if (!m.changes.length) out.push({ line: text(fit([S('   nothing new', 'faint')], w)) })
  head(5, 'PROJECTS · ' + m.projects.length + ' with work' + (m.idle.length ? ' · idle: ' + m.idle.join(', ') : ''), 'dim')
  for (const p of m.projects) {
    const n = (...s) => p.tasks.filter((t) => s.includes(t.state)).length
    const parts = [
      n('wait', 'stuck') ? [n('wait', 'stuck') + ' need you', 'y'] : null,
      n('run', 'silent') ? [n('run', 'silent') + ' working', 'b'] : null,
      n('silent') ? [n('silent') + ' silent', 'o'] : null,
      n('ready') ? [n('ready') + ' ready', 't'] : null,
      n('blocked') ? [n('blocked') + ' blocked', 'r'] : null,
      n('later') ? [n('later') + ' later', 'dim'] : null,
    ].filter(Boolean)
    const [g, gs] = PROJECT_MARK[p.mark]
    item('p:' + p.name, [S(' ' + g + ' ', gs), S(p.name.padEnd(16), 'bold'), ...parts.flatMap(([t, st], i) => [S(i ? ' · ' : '', 'faint'), S(t, st)])])
  }
  return out
}

/** A project's tasks as list entries (the tasks view), each selectable as `t:<id>`. */
export function taskEntries(p, w, sel) {
  const out = [{ line: text(fit([S(p.name + ': every task', 'head')], w)) }]
  const groups = [
    ['NEEDS YOU', ['wait', 'stuck'], 'yb'],
    ['WORKING NOW', ['run', 'silent'], 'b'],
    ['READY, NOBODY ON IT', ['ready'], 'bold'],
    ['BLOCKED', ['blocked'], 'r'],
    ['LATER: WAITS ON SOMETHING', ['later'], 'dim'],
    ['DONE RECENTLY', ['done'], 'g'],
  ]
  for (const [title, states, st] of groups) {
    const list = p.tasks.filter((t) => states.includes(t.state))
    if (!list.length) continue
    out.push({ line: text(fit([S(title + ' · ' + list.length, st)], w)) })
    for (const t of list) {
      const [g, gs] = TASK_MARK[t.state]
      const id = 't:' + t.id
      const segs = cols([[[S(' ' + g + ' ', gs), S(t.title, ['later', 'done'].includes(t.state) ? 'dim' : 't'), ...(t.isNew ? [S(' new', 'c')] : [])], 0], ...(w >= 70 ? [[[S(t.who || (t.state === 'done' ? '' : 'nobody'), t.who ? 'dim' : 'faint')], 17]] : []), [[S(age(t.age).padStart(3), 'dim')], 4]], w)
      out.push({ id, line: line([{ segs, btn: { key: 'row:' + id, act: 'select', arg: id } }], id === sel) })
    }
  }
  return out
}

/** The entries that fit in `h` lines, kept around the selection, with "more items" marks. */
export function windowed(entries, sel, h) {
  if (entries.length <= h) return entries.map((e) => e.line)
  const at = Math.max(0, entries.findIndex((e) => e.id === sel))
  const start = Math.max(0, Math.min(at - Math.floor(h / 3), entries.length - h))
  const shown = entries.slice(start, start + h).map((e) => e.line)
  const items = (a) => a.filter((e) => e.id).length
  if (start > 0) shown[0] = text([S('   ↑ ' + items(entries.slice(0, start + 1)) + ' more items', 'faint')])
  if (start + h < entries.length) shown[shown.length - 1] = text([S('   ↓ ' + items(entries.slice(start + h - 1)) + ' more items', 'faint')])
  return shown
}

const fact = (k, v, st = 't') => text([S(k.padEnd(10), 'dim'), S(v, st)])
/** A fact whose value wraps under itself, for a note or a question. */
const facts = (k, v, w, st = 't') => wrap(v, Math.max(10, w - 10)).map((l, i) => text([S((i ? '' : k).padEnd(10), 'dim'), S(l, st)]))

/** The detail of a needs-you item. */
export function needDetail(n, p, w, extra = {}) {
  const title = { unblock: 'BLOCKED SPAWN', decide: 'STUCK: NEEDS YOUR DECISION', answer: 'NEEDS YOUR ANSWER', approve: 'NEEDS YOUR APPROVAL' }[n.verb]
  const done = { unblock: 'RETRIED', decide: 'ANSWERED', answer: 'ANSWERED', approve: 'APPROVED' }[n.verb]
  const [, st] = NEED_MARK[n.verb]
  const out = [
    text(spread([S(n.receipt ? '✓ ' + done : NEED_MARK[n.verb][0] + ' ' + title, n.receipt ? 'g' : st === 'y' ? 'yb' : 'rb')], [S(n.project + (p ? ' · ' + p.collection : ''), 'dim')], w)),
    text([S(n.receipt ? n.receipt : n.verb === 'unblock' ? 'blocked for ' + age(n.age) : 'waits for you ' + age(n.age) + (n.since ? ', since ' + clock(n.since) : ''), n.receipt ? 'g' : st)]),
    text([]),
    ...wrap(n.ask, w).map((l) => text([S(l, 'head')])),
  ]
  if (n.detail) out.push(text([]), ...wrap(n.detail, w).map((l) => text([S(l, 't')])))
  out.push(text([]))
  if (n.why && !n.receipt) out.push(fact('why now', n.why))
  if (n.worker) out.push(fact('worker', n.worker + activityText(n.activity)))
  if (n.owner && n.owner !== n.worker) out.push(fact(n.verb === 'unblock' ? 'spawner' : 'owner', n.owner + (p && p.orchestrators.some((o) => o.agent === n.owner) ? ' (orchestrator), ' + (p.orchestrators.find((o) => o.agent === n.owner).online ? 'online' : 'offline') : '')))
  if (n.task) out.push(fact('task', n.channel + '/' + n.task, 'dim'))
  if (n.asked) out.push(fact('asked', n.asked.owner + ' at ' + n.asked.time + '; the reply comes to your chief', 'c'))
  // A confirmation or a field comes before the history: it must stay in view.
  for (const l of extra.below || []) out.push(l)
  out.push(...historyLines(extra.history, w))
  return out
}

/** A worker's last activity, from the hub (borant status): " · last tool call 40s ago: Bash npm test". */
export function activityText(a) {
  if (!a) return ''
  if (a.tool_running_since) return ' · running a tool for ' + age(Date.now() - a.tool_running_since) + (a.last_tool ? ': ' + a.last_tool : '')
  if (a.last_tool_at) return ' · last tool call ' + age(Date.now() - a.last_tool_at) + ' ago' + (a.last_tool ? ': ' + a.last_tool : '')
  return ''
}

/** "14:35" in the session's local time. */
export function clock(at) {
  const d = new Date(at)
  return String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0')
}

export function quietDetail(q, p, w) {
  if (q.verb === 'silent') {
    const a = q.activity || {}
    return [
      text(spread([S('◌ SILENT WORKER', 'o')], [S(q.project + (p ? ' · ' + p.collection : ''), 'dim')], w)),
      text([S('claimed, no tool call for ' + age(q.age), 'o')]),
      text([]),
      ...wrap(q.ask, w).map((l) => text([S(l, 'head')])),
      text([]),
      fact('worker', q.worker),
      ...(a.last_tool ? facts('last tool', a.last_tool + (a.last_tool_at ? ' (' + clock(a.last_tool_at) + ')' : ''), w) : []),
      ...(a.last_text ? facts('last text', '"' + a.last_text + '"' + (a.last_text_at ? ' (' + clock(a.last_text_at) + ')' : ''), w) : []),
      fact('owner', q.owner),
    ]
  }
  return [
    text(spread([S('◌ ORCHESTRATOR OFFLINE', 'o')], [S(q.project + (p ? ' · ' + p.collection : ''), 'dim')], w)),
    text([S(q.who + ' has no session', 'o')]),
    text([]),
    ...wrap(q.ask, w).map((l) => text([S(l, 'head')])),
    text([]),
    ...(p ? p.tasks.filter((t) => t.state !== 'done').slice(0, 8).map((t) => text(fit([S(' ' + TASK_MARK[t.state][0] + ' ', TASK_MARK[t.state][1]), S(t.title, 't')], w))) : []),
  ]
}

export function shipDetail(s, w) {
  return [
    text(spread([S('⇡ OPEN PR', 'm')], [S(s.project, 'dim')], w)),
    text([S(s.pr.words && s.pr.words !== 'open' ? s.pr.words + '; the task: ' + (SHIP_WORDS[s.state] || s.state) : 'the task: ' + (SHIP_WORDS[s.state] || s.state) + '; checks and review not known yet', s.pr.words && s.pr.words !== 'open' ? prStyle(s.pr.words) : 'b')]),
    text([]),
    ...wrap(s.pr.label + ' ' + s.title, w).map((l) => text([S(l, 'head')])),
    text([]),
    fact('worker', s.who || 'nobody'),
    ...(s.pr.checks ? [fact('checks', s.pr.checks)] : []),
    ...(s.pr.review ? [fact('review', s.pr.review)] : []),
    fact('link', s.pr.url, 'dim'),
  ]
}

export function changeDetail(c, w) {
  return [
    text(spread([S(changeMark(c.verb)[0] + ' ' + c.verb.toUpperCase(), changeMark(c.verb)[1])], [S(c.project, 'dim')], w)),
    text([S(c.at ? clock(c.at) + ', ' + age(c.age) + ' ago' : '', 'dim')]),
    text([]),
    ...wrap(c.text, w).map((l) => text([S(l, 'head')])),
    ...(c.who ? [text([]), fact('by', c.who)] : []),
  ]
}

/** A project's detail: every task by state (as taskEntries, without the selection). */
export function projectDetail(p, w) {
  // A hung orchestrator took no tool call and no text for the hub's
  // `hung_minutes`, or came back from a sleep with its turn frozen: the line
  // says so, and `borant restart <agent>` is the way out.
  const orchs = p.orchestrators.map((o) => {
    if (!o.online) return o.agent + ' offline'
    return o.agent + ' ' + (o.busy ? 'online, busy' : 'online') + (o.hung ? ', hung?' : '')
  })
  const orch = orchs.join(', ')
  const out = [text(fit([S(p.name, 'head'), S('  ' + p.collection + ' · ' + (orch || 'no orchestrator'), 'dim')], w))]
  if (p.saga) out.push(text(fit([S('saga ' + (p.saga.title || p.saga.channel) + ': ' + p.saga.done + ' of ' + p.saga.total + ' epics done', 'dim')], w)))
  out.push(text([]))
  // Text only: a Button here would let the ring leave the list.
  for (const e of taskEntries(p, w, null).slice(1)) out.push({ ...e.line, sel: false, parts: e.line.parts.map((x) => ({ segs: x.segs })) })
  return out
}

export function taskDetail(t, p, w, extra = {}) {
  const [g, st] = TASK_MARK[t.state]
  const words = { wait: 'NEEDS YOU', stuck: 'STUCK', run: 'WORKING', silent: 'SILENT', ready: 'READY, NOBODY ON IT', blocked: 'BLOCKED', later: 'LATER', done: 'DONE' }
  return [
    text(spread([S(g + ' ' + words[t.state], st)], [S(p.name + ' · ' + p.collection, 'dim')], w)),
    text([S(t.age !== null ? 'last update ' + age(t.age) + ' ago' : '', 'dim')]),
    text([]),
    ...wrap(t.title, w).map((l) => text([S(l, 'head')])),
    text([]),
    fact('worker', t.who || 'nobody'),
    ...(t.pr ? [fact('PR', t.pr.label + (t.pr.words && t.pr.words !== 'open' ? ' · ' + t.pr.words : '') + ' · ' + t.pr.url)] : []),
    ...(t.note ? facts('note', t.note, w) : []),
    fact('task', t.channel + '/' + t.id, 'dim'),
    ...(extra.below || []),
    ...historyLines(extra.history, w),
  ]
}

/** The last entries of a task's history, one wrapped line each, under a head. */
function historyLines(history, w) {
  if (!history || !history.length) return []
  return [text([]), text([S('HISTORY', 'dim')]), ...history.slice(-6).flatMap((h) => wrap(h.text, Math.max(10, w - 7)).map((l, i) => text([S((i ? '' : clock(h.at)).padEnd(7), 'dim'), S(l, 't')])))]
}

/** Every key, for h. */
export function helpLines(w) {
  // A long description wraps under itself, so a narrow panel shows every key's meaning.
  const k = (key, what) => wrap(what, Math.max(10, w - 14)).map((l, i) => text(fit([S(' ' + (i ? '' : key).padEnd(13), 'kb'), S(l, 't')], w)))
  return [].concat(
    text([S('ALL KEYS', 'head')]),
    text([]),
    text([S('At the prompt. Borant takes no other key there.', 'dim')]),
    k('ctrl+x b', 'give Borant the keys; the panel opens'),
    k('/borant', 'the same, when ctrl+x b does nothing'),
    text(fit([S('               (the diff panel holds ctrl+x b, or you bound it to another key)', 'dim')], w)),
    text([]),
    text([S('In the panel', 'dim')]),
    k('↑ ↓   j k', 'move'),
    k('1 2 3 4 5', 'jump to: needs you, quiet, open PRs, changed, projects'),
    k('enter  b', "open a project's tasks  ·  back"),
    k('r  y  n  d', 'answer  ·  approve  ·  no  ·  close as done'),
    k('a  m', 'ask the orchestrator for a recommendation  ·  message the worker'),
    k('o  g  p', 'open the PR  ·  how to attach to the agent  ·  the project'),
    k('u', 'retry a blocked spawn, or resume an offline orchestrator'),
    k('x', 'mark the changes seen'),
    k('h', 'this list (h or b closes it)'),
    k('esc', 'give the keys back to the prompt and close the panel'),
    text([]),
    text([S('In an answer field every key types, except:', 'dim')]),
    k('enter  esc', 'send  ·  keep the draft and close'),
    text([]),
    text([S('Marks', 'dim')]),
    text(fit([S(' ▲', 'y'), S(' needs you   ', 't'), S('■', 'r'), S(' stuck   ', 't'), S('⊘', 'r'), S(' blocked   ', 't'), S('◌', 'o'), S(' silent or offline   ', 't'), S('●', 'b'), S(' working', 't')], w)),
    text(fit([S(' ○', 't'), S(' ready, nobody on it   ', 't'), S('◷', 'dim'), S(' later   ', 't'), S('⇡', 'm'), S(' open PR   ', 't'), S('✓', 'g'), S(' done', 't')], w)),
    text([]),
    text([S('Notes', 'dim')]),
    ...wrap('ctrl+x b is the default chord. Borant cannot read your key bindings.', w - 1).map((l) => text([S(' ' + l, 't')])),
    ...wrap('Claude Code gives the panel the keys only over an empty prompt.', w - 1).map((l) => text([S(' ' + l, 't')])),
    ...wrap('"Changed since" holds until you press x. Closing the panel marks nothing seen.', w - 1).map((l) => text([S(' ' + l, 't')])),
  )
}

/** Key Buttons over as many lines as they need: `[[hotkey, label, act, arg], ...]`; a key with no hotkey is text. */
export function keyLines(keys, w) {
  const chips = keys.map(([k, label, act, arg]) => {
    const segs = [S(k, 'kb'), S(act ? ': ' + label : ' ' + label, 't')]
    return act ? { segs, btn: { key: 'key:' + k, act, arg, hotkey: k, label } } : { segs }
  })
  const lines = []
  let cur = []
  let used = 0
  for (const c of chips) {
    const cw = width(c.segs) + (cur.length ? 3 : 0)
    if (used + cw > w && cur.length) {
      lines.push(cur)
      cur = []
      used = 0
    }
    if (cur.length) cur.push({ segs: [S('   ')] })
    cur.push(c)
    used += width(c.segs) + (cur.length > 1 ? 3 : 0)
  }
  if (cur.length) lines.push(cur)
  // Every line: a hotkey whose Button is not drawn goes to the prompt.
  return lines.map((parts) => {
    const used2 = width(parts.flatMap((p) => p.segs))
    return line(used2 < w ? [...parts, { segs: [S(' '.repeat(w - used2))] }] : parts)
  })
}

/**
 * The panel: the top line, the list and the detail (side by side from 120
 * cells, else stacked), and the key lines at the bottom, in `h` lines.
 */
export function panelLines(w, h, { top, entries, sel, detail, keys, listShare = 0.45 }) {
  const foot = keys.length ? [text(rule(w)), ...keyLines(keys, w)] : []
  const bodyH = Math.max(3, h - 2 - foot.length)
  const body = []
  if (w >= 120) {
    const lw = Math.floor(w * 0.44)
    const rw = w - lw - 3
    const l = windowed(entries(lw), sel, bodyH)
    const d = cutDetail(detail(rw), rw, bodyH)
    for (let i = 0; i < bodyH; i++) {
      const left = l[i] || text(fit([], lw))
      const right = d[i] || text([])
      body.push(line([...left.parts.map((p) => ({ ...p, segs: fit(p.segs || [], lw) })).slice(0, 1), { segs: [S(' │ ', 'faint')] }, ...right.parts.map((p) => (p.input ? p : { ...p, segs: fit(p.segs || [], rw) }))], left.sel))
    }
  } else {
    const all = entries(w)
    const listH = Math.max(3, Math.min(all.length, Math.floor(bodyH * listShare)))
    body.push(...windowed(all, sel, listH), text(rule(w)), ...cutDetail(detail(w), w, bodyH - listH - 1))
  }
  const rows = [top, text([]), ...body]
  while (rows.length < h - foot.length) rows.push(text([]))
  return [...rows.slice(0, h - foot.length), ...foot]
}

/** A detail cut to `h` lines says how many lines are left. */
function cutDetail(d, w, h) {
  if (d.length <= h) return d
  return [...d.slice(0, h - 1), text(fit([S('↓ ' + (d.length - h + 1) + ' more lines · ', 'faint'), S('enter', 'kb'), S(' shows all', 'faint')], w))]
}
