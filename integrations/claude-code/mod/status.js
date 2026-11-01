// What the board says about itself and about a press: how old the data is,
// whether a read runs now, and where a Done or an Ask is. Pure, like board.js,
// so the tests call it with plain values.
//
// A Done mark is `{ state, at, title, ... }`, keyed by `<channel>/<task>`:
//   sending   the press happened; the prompt to the chief is not in yet
//   closing   the prompt landed at `at`; the hub has not closed the task
//   closed    the task left the board at `closedAt`; the row stays CLOSED_SHOW_MS
//   failed    the hub did not close it within the wait (`reason`)
//   unsent    the prompt did not land (`reason`)
// An Ask mark is `{ state: 'sending' | 'sent' | 'failed', owner, at, reason }`.

import { ageText, text } from './board.js'

export { ageText }

/** A closed item's row stays this long, so the person sees it land. */
export const CLOSED_SHOW_MS = 10000
/** The age shows in the freshness word only from this age on. */
export const AGE_SHOWN_MS = 30000
/** From this age on, the board is stale. */
export const STALE_MS = 90000

/**
 * The freshness word of the title row, as one segment: `live`, `live · 45s`,
 * `↻ reading`, `polling · 4s`, `stale · 2m`, or `✕ board unavailable`.
 * `state` is `{ unavailable, reading, live, ageMs }`; `ageMs` is the time
 * since the last read that succeeded, or null when none did.
 */
export function freshness(state = {}) {
  if (state.unavailable) return text('✕ board unavailable', 'red')
  if (state.reading) return text('↻ reading', 'cyan')
  const age = typeof state.ageMs === 'number' ? state.ageMs : null
  if (age !== null && age >= STALE_MS) return text('stale · ' + ageText(age), 'yellow')
  if (state.live) return text(age !== null && age >= AGE_SHOWN_MS ? 'live · ' + ageText(age) : 'live', 'green')
  return text('polling' + (age !== null ? ' · ' + ageText(age) : ''), 'dim')
}

/**
 * The status of a Done or an Ask on one item, as segments, or null when the
 * item shows its buttons. `now` is the session clock.
 */
export function pressStatus(done, ask, now = 0) {
  if (done) {
    if (done.state === 'sending') return [text('◷ sending to the chief…', 'cyan')]
    if (done.state === 'closing') return [text('◷ the chief is closing it · ' + ageText(now - done.at), 'cyan')]
    if (done.state === 'closed') return [text('✓ closed', 'green')]
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
 * preview shows the full words. Null when the item shows its buttons.
 */
export function pressStatusShort(done, ask, now = 0) {
  if (done) {
    if (done.state === 'sending') return [text('◷ sending', 'cyan')]
    if (done.state === 'closing') return [text('◷ closing ' + ageText(now - done.at), 'cyan')]
    if (done.state === 'closed') return [text('✓ closed', 'green')]
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
 * The needs-you items to draw: the board's, and each item a Done closed in
 * the last CLOSED_SHOW_MS, at the place it had, marked `closed: true`.
 */
export function withClosed(questions, pending, now = 0) {
  const out = [...questions]
  const ghosts = Object.values(pending || {})
    .filter((m) => m.state === 'closed' && m.question && now - m.closedAt < CLOSED_SHOW_MS)
    .sort((a, b) => (a.index || 0) - (b.index || 0))
  for (const mark of ghosts) {
    out.splice(Math.min(mark.index || 0, out.length), 0, { ...mark.question, closed: true })
  }
  return out
}
