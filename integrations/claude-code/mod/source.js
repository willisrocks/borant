// The board's data source. `cli` runs `borant board --json` (the hub read of
// the sagas spec); `fixture` uses the fixture module under mod/fixtures, for a
// session without a hub. Every failure comes back as `{ board: null, reason }`,
// so a session without borant draws "board unavailable" and never an error.
//
// The drill-in views also read one channel's tasks with `borant tasks
// <channel>`: `readTasks` has the same never-throw shape, `{ tasks, reason }`,
// and the fixture answers from mod/fixtures/tasks.js.

import { board as fixtureBoard } from './fixtures/board.js'
import { tasks as fixtureTasks } from './fixtures/tasks.js'

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

/** Parse and check the task list of one channel: an array of Task records. */
export function parseTasks(text) {
  let value
  try {
    value = JSON.parse(text)
  } catch (error) {
    return { tasks: null, reason: 'not JSON: ' + reasonOf(error) }
  }
  if (!Array.isArray(value)) return { tasks: null, reason: 'not a task list' }
  return { tasks: value, reason: null }
}

/**
 * Read one channel's tasks with `borant tasks <channel>`. `options` is
 * `{ mode }`, from the plugin's config, and the fixture answers from
 * mod/fixtures/tasks.js. Never throws: a missing `borant`, an old one without
 * the command, an unknown channel, a timeout, and a bad payload all come back
 * as `{ tasks: null, reason }`, which the pane draws as `tasks unavailable`.
 */
export async function readTasks($, options = {}, channel) {
  if (sourceMode(options.mode) === 'fixture') {
    return { tasks: fixtureTasks[channel] || [], reason: null }
  }
  try {
    const run = await $.process.run(['borant', 'tasks', String(channel)], { timeoutMs: 8000 })
    if (!run || run.exitCode !== 0) {
      return { tasks: null, reason: 'borant tasks exited ' + (run ? run.exitCode : '?') }
    }
    return parseTasks(run.stdout || '')
  } catch (error) {
    return { tasks: null, reason: reasonOf(error) }
  }
}
