// The mod's data and actions, through the `borant` CLI. `cli` runs borant;
// `fixture` answers from the modules under mod/fixtures, for a session without
// a hub. Nothing here throws: a read that fails comes back as `{ reason }`, so
// a session without borant draws a reason and never an error.
//
// Reads: `borant board --json` (projects, needs-you, blocked spawns),
// `borant channel list` (which project each channel belongs to), and
// `borant tasks <channel>` (one channel's tasks, with claimed_by and
// pr_url). Actions: answer, close, send, resume, and `open` for a PR.

import { board as fixtureBoard } from './fixtures/board.js'
import { channels as fixtureChannels } from './fixtures/channels.js'
import { tasks as fixtureTasks } from './fixtures/tasks.js'

/** `fixture` or `cli`, from BORANT_COS_BOARD. The default is `cli`. */
export function sourceMode(value) {
  return value === 'fixture' ? 'fixture' : 'cli'
}

function reasonOf(error) {
  const message = error && error.message ? error.message : String(error)
  return message.slice(0, 120)
}

/** The first line of a program's stderr, for a reason that a person can read. */
export function firstLine(text) {
  return String(text || '').trim().split('\n')[0].slice(0, 120)
}

function parseJson(text, check, what) {
  let value
  try {
    value = JSON.parse(text)
  } catch (error) {
    return { value: null, reason: 'not JSON: ' + reasonOf(error) }
  }
  if (!check(value)) return { value: null, reason: 'not ' + what }
  return { value, reason: null }
}

/** Parse and check a BoardView. */
export function parseBoard(text) {
  const r = parseJson(text, (v) => v && typeof v === 'object' && Array.isArray(v.collections), 'a BoardView')
  return { board: r.value, reason: r.reason }
}

/** Parse and check a list: a channel list or a task list. */
export function parseList(text, what) {
  return parseJson(text, Array.isArray, what)
}

async function run($, argv, timeoutMs = 8000) {
  try {
    const out = await $.process.run(argv, { timeoutMs })
    if (!out || out.exitCode !== 0) {
      const why = firstLine(out && out.stderr)
      return { ok: false, stdout: '', reason: argv.slice(0, 2).join(' ') + ' exited ' + (out ? out.exitCode : '?') + (why ? ': ' + why : '') }
    }
    return { ok: true, stdout: out.stdout || '', reason: null }
  } catch (error) {
    return { ok: false, stdout: '', reason: reasonOf(error), timeout: /time/i.test(String(error && error.message)) }
  }
}

/**
 * Read the board, with its task lines (`--tasks`: every unfinished task, its
 * start state, its PR state, and the recent done tasks). A borant without
 * `--tasks` refuses the flag; then `options.legacy` is set and the plain board
 * is read, with each channel's tasks read apart. `{ board, reason }`.
 */
export async function readBoard($, options = {}) {
  if (sourceMode(options.mode) === 'fixture') return { board: fixtureBoard, reason: null }
  if (!options.legacy) {
    const out = await run($, ['borant', 'board', '--json', '--tasks'])
    if (out.ok) return parseBoard(out.stdout)
    if (!/unexpected argument|unrecognized|--tasks/.test(out.reason || '')) return { board: null, reason: out.reason }
    options.legacy = true
  }
  const out = await run($, ['borant', 'board', '--json'])
  if (!out.ok) return { board: null, reason: out.reason }
  return parseBoard(out.stdout)
}

/** Each agent's activity, by agent id, from `borant status --json`. `{ agents, reason }`. */
export async function readActivity($, options = {}) {
  if (sourceMode(options.mode) === 'fixture') return { agents: {}, reason: null }
  const out = await run($, ['borant', 'status', '--json'])
  if (!out.ok) return { agents: null, reason: out.reason }
  const r = parseJson(out.stdout, (v) => v && Array.isArray(v.agents), 'a swarm view')
  if (!r.value) return { agents: null, reason: r.reason }
  const agents = {}
  for (const a of r.value.agents) agents[a.agent] = a
  return { agents, reason: null }
}

/**
 * The person-level changes since a mark: `{ seq }` (the last change marked
 * seen) or `{ at }` (a time). `{ changes, at, reason }`; `changes` is null
 * when this borant has no `changes` command.
 */
export async function readChanges($, options = {}, mark) {
  if (sourceMode(options.mode) === 'fixture' || options.noFeed) return { changes: null, reason: null }
  const since = mark && mark.seq !== undefined ? String(mark.seq) : String(mark?.at ?? 0)
  const out = await run($, ['borant', 'changes', '--since', since, '--json'])
  if (!out.ok) {
    if (/unrecognized subcommand|unexpected argument/.test(out.reason || '')) options.noFeed = true
    return { changes: null, reason: out.reason }
  }
  const r = parseJson(out.stdout, (v) => v && Array.isArray(v.changes), 'a changes feed')
  return r.value ? { changes: r.value.changes, at: r.value.at, reason: null } : { changes: null, reason: r.reason }
}

/** One task's history, oldest first: `{ entries, reason }`. */
export async function readHistory($, options = {}, channel, task) {
  if (sourceMode(options.mode) === 'fixture') return { entries: [], reason: null }
  const out = await run($, ['borant', 'task', 'history', channel + '/' + task, '--json'])
  if (!out.ok) return { entries: null, reason: out.reason }
  const r = parseList(out.stdout, 'a task history')
  return { entries: r.value, reason: r.reason }
}

/** Read every channel. `{ channels, reason }`. */
export async function readChannels($, options = {}) {
  if (sourceMode(options.mode) === 'fixture') return { channels: fixtureChannels, reason: null }
  const out = await run($, ['borant', 'channel', 'list'])
  if (!out.ok) return { channels: null, reason: out.reason }
  const r = parseList(out.stdout, 'a channel list')
  return { channels: r.value, reason: r.reason }
}

/** Read one channel's tasks. `{ tasks, reason }`. */
export async function readTasks($, options = {}, channel) {
  if (sourceMode(options.mode) === 'fixture') return { tasks: fixtureTasks[channel] || [], reason: null }
  const out = await run($, ['borant', 'tasks', String(channel)])
  if (!out.ok) return { tasks: null, reason: out.reason }
  const r = parseList(out.stdout, 'a task list')
  return { tasks: r.value, reason: r.reason }
}

/**
 * One action through the CLI. `{ ok, reason, timeout }`: `timeout` is true
 * when the call did not end in time, so the result is not known.
 */
export async function act($, options, argv) {
  if (sourceMode(options.mode) === 'fixture') return { ok: true, reason: null, timeout: false }
  const out = await run($, argv)
  return { ok: out.ok, reason: out.reason, timeout: Boolean(out.timeout) }
}

/** The argv of each action. */
export const ACTIONS = {
  // With a key, a second send of the same draft (after a timeout) sends
  // nothing new; with the version the person saw, a task that changed since
  // is a conflict. An older borant takes neither: answerPlain.
  answer: (channel, task, text, key, version) => ['borant', 'answer', channel + '/' + task, ...(key ? ['--key', key] : []), ...(version ? ['--expect-updated', String(version)] : []), text],
  answerPlain: (channel, task, text) => ['borant', 'answer', channel + '/' + task, text],
  sendTask: (to, text, channel, task) => ['borant', 'send', to, '--task', channel + '/' + task, text],
  attach: (agent) => ['borant', 'attach', agent, '--window'],
  close: (channel, task, message) => ['borant', 'task', 'close', channel + '/' + task, '--reason', 'done', '--message', message],
  send: (to, text) => ['borant', 'send', to, text],
  resume: (agent) => ['borant', 'resume', agent],
  open: (url) => ['open', url],
}
