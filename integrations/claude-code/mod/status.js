// What the board says about itself and about a press: how old the data is,
// whether a read runs now, and where a Done or an Ask is. Pure, like board.js,
// so the tests call it with plain values.
//
// A Done mark is `{ state, at, title, ... }`, keyed by `<channel>/<task>`:
//   sending   the press happened; the prompt to the chief is not in yet
//   closing   the prompt landed at `at`; the hub has not closed the task
//   closed    the task left the board at `closedAt`, closed; the row stays CLOSED_SHOW_MS
//   moved     it left needs-you at `closedAt` but is not closed (`status`)
//   unknown   it left needs-you at `closedAt`, and its status read failed
//   failed    the hub did not close it within the wait (`reason`)
//   unsent    the prompt did not land (`reason`)
// An Ask mark is `{ state: 'sending' | 'sent' | 'failed', owner, at, reason }`.

import { ageText, hhmm, text } from './board.js'

export { ageText }

/** A closed item's row stays this long, so the person sees it land. */
export const CLOSED_SHOW_MS = 10000
/** From this age on, the board is stale. */
export const STALE_MS = 90000

/**
 * What the title row says about the data, as one segment, or null while it is
 * fresh: nothing moves while the board is healthy. `stale · 2m` once no read
 * succeeded for STALE_MS, `✕ board unavailable` when the read fails.
 * `state` is `{ unavailable, ageMs }`; `ageMs` is the time since the last
 * read that succeeded, or null when none did.
 */
export function freshness(state = {}) {
  if (state.unavailable) return text('✕ board unavailable', 'red')
  const age = typeof state.ageMs === 'number' ? state.ageMs : null
  if (age !== null && age >= STALE_MS) return text('stale · ' + ageText(age), 'yellow')
  return null
}

/**
 * The status of a Done or an Ask on one item, as segments, or null when the
 * item shows its buttons. `now` is the session clock.
 */
export function pressStatus(done, ask, now = 0) {
  if (done) {
    if (done.state === 'sending') return [text('◷ sending to the chief…', 'cyan')]
    if (done.state === 'closing') return [text('◷ the chief is closing it, since ' + hhmm(done.at), 'cyan')]
    if (done.state === 'closed') return [text('✓ closed', 'green')]
    if (done.state === 'moved') return [text('→ no longer needs you, but not closed' + (done.status ? ': ' + String(done.status).replace('_', ' ') : ''), 'yellow')]
    if (done.state === 'unknown') return [text('? left the board; its status could not be read', 'yellow')]
    if (done.state === 'failed') return [text('! still open after ' + ageText(done.waitMs || now - done.at), 'red')]
    if (done.state === 'unsent') return [text('! not sent' + (done.reason ? ': ' + done.reason : ''), 'red')]
  }
  if (ask) {
    if (ask.state === 'sending') return [text('◷ asking ' + (ask.owner || 'the orchestrator') + '…', 'cyan')]
    if (ask.state === 'sent') return [text('✓ asked ' + ask.owner + (ask.time ? ' at ' + ask.time : ''), 'green')]
    if (ask.state === 'failed') return [text('! ask failed' + (ask.reason ? ': ' + ask.reason : ''), 'red')]
  }
  return null
}

/**
 * The same status in at most 13 cells, for a band row's fixed slot; the
 * preview shows the full words. Nothing in it counts up, so it never moves.
 */
export function pressStatusShort(done, ask, now = 0) {
  if (done) {
    if (done.state === 'sending') return [text('◷ sending', 'cyan')]
    if (done.state === 'closing') return [text('◷ closing', 'cyan')]
    if (done.state === 'closed') return [text('✓ closed', 'green')]
    if (done.state === 'moved') return [text('→ not closed', 'yellow')]
    if (done.state === 'unknown') return [text('? status', 'yellow')]
    if (done.state === 'failed') return [text('! still open', 'red')]
    if (done.state === 'unsent') return [text('! not sent', 'red')]
  }
  if (ask) {
    if (ask.state === 'sending') return [text('◷ asking', 'cyan')]
    if (ask.state === 'sent') return [text('✓ asked', 'green')]
    if (ask.state === 'failed') return [text('! ask failed', 'red')]
  }
  return null
}

/** Whether a Done press can be made again: no Done is under way. */
export function canDone(done) {
  return !done || done.state === 'failed' || done.state === 'unsent'
}

/**
 * The needs-you items to draw: the board's, and each item a Done took off
 * it in the last CLOSED_SHOW_MS, at the place it had: `closed: true` when
 * the hub closed it, `gone: true` when it left needs-you otherwise. An item
 * that is back on the board is drawn once, as the board has it.
 */
export function withClosed(questions, pending, now = 0) {
  const out = [...questions]
  const open = new Set(questions.map((q) => String(q.channel) + '/' + String(q.task)))
  const ghosts = Object.values(pending || {})
    .filter((m) => ['closed', 'moved', 'unknown'].includes(m.state) && m.question && now - m.closedAt < CLOSED_SHOW_MS)
    .filter((m) => !open.has(String(m.question.channel) + '/' + String(m.question.task)))
    .sort((a, b) => (a.index || 0) - (b.index || 0))
  for (const mark of ghosts) {
    const ghost = mark.state === 'closed' ? { ...mark.question, closed: true } : { ...mark.question, gone: true }
    out.splice(Math.min(mark.index || 0, out.length), 0, ghost)
  }
  return out
}
