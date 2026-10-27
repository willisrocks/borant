// The board's data source. `cli` runs `borant board --json` (the hub read of
// the sagas spec); `fixture` uses the fixture module under mod/fixtures, for a
// session without a hub. Every failure comes back as `{ board: null, reason }`,
// so a session without borant draws "board unavailable" and never an error.

import { board as fixtureBoard } from './fixtures/board.js'

/** `fixture` or `cli`, from BORANT_COS_BOARD. The default is `cli`. */
export function sourceMode(value) {
  return value === 'fixture' ? 'fixture' : 'cli'
}

function reasonOf(error) {
  const message = error && error.message ? error.message : String(error)
  return message.slice(0, 120)
}

/** Parse and check a BoardView. */
export function parseBoard(text) {
  let value
  try {
    value = JSON.parse(text)
  } catch (error) {
    return { board: null, reason: 'not JSON: ' + reasonOf(error) }
  }
  if (!value || typeof value !== 'object' || !Array.isArray(value.collections)) {
    return { board: null, reason: 'not a BoardView' }
  }
  return { board: value, reason: null }
}

/**
 * Read one board. `options` is `{ mode }`, from the plugin's config. Never
 * throws: a missing `borant`, an old one without the command, a timeout, and
 * a bad payload all come back as a reason.
 */
export async function readBoard($, options = {}) {
  if (sourceMode(options.mode) === 'fixture') return { board: fixtureBoard, reason: null }
  try {
    const run = await $.process.run(['borant', 'board', '--json'], { timeoutMs: 8000 })
    if (!run || run.exitCode !== 0) {
      return { board: null, reason: 'borant board exited ' + (run ? run.exitCode : '?') }
    }
    return parseBoard(run.stdout || '')
  } catch (error) {
    return { board: null, reason: reasonOf(error) }
  }
}
