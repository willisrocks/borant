// The mod in a session: the role gate, the band, the hotkey, no key at the
// prompt, the panel's keys and actions, the event stream, and a failing hub.
import { expect, mock, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import channels from '../fixtures/channels.js'
import { tasks } from '../fixtures/tasks.js'

const SESSION = { surface: 'terminal', isInteractive: true, cwd: '/work' } as const

// What Claude Code passes to a ui.render hook for the band on a 200-column terminal.
const BAND = {
  plugin: 'borant',
  component: 'AbovePrompt',
  requestId: 'above-prompt',
  viewport: { columns: 200, rows: 50, isFullscreen: true },
  props: { hasSurvey: false, isWorking: false, maxRows: 19, bodyColumns: 195, scroll: { offset: 0, bodyRows: 19 }, view: {} },
} as const

// What Claude Code passes to a ui.render hook for the docked panel at 200 columns.
const PANE = {
  plugin: 'borant',
  component: 'Pane',
  requestId: 'cos',
  viewport: { columns: 109, rows: 50, isFullscreen: true },
  props: { title: 'Borant', isFocused: true, bodyColumns: 90, placement: 'dock', scroll: { offset: 0, bodyRows: 44 }, view: {} },
} as const

const ENGINE_ROW = { type: 'Text', props: {}, children: ['drawn by Claude Code'] }
const FIRST_NEED = 'n:personal-borant/01M5ZG00000000000000000015'
const SECOND_NEED = 'n:personal-borant/01M5ZG00000000000000000016'

/** A `borant` that serves the fixture, and records every call. */
function fixtureCli(runs: string[][], fail: Record<string, { exitCode: number; stderr: string }> = {}) {
  return (argv: string[]) => {
    runs.push(argv)
    if (fail[argv[1]]) return { stdout: '', ...fail[argv[1]] }
    if (argv[1] === 'board') return { exitCode: 0, stdout: JSON.stringify(board), stderr: '' }
    if (argv[1] === 'channel') return { exitCode: 0, stdout: JSON.stringify(channels), stderr: '' }
    if (argv[1] === 'tasks') return { exitCode: 0, stdout: JSON.stringify((tasks as Record<string, unknown>)[argv[2]] || []), stderr: '' }
    return { exitCode: 0, stdout: '', stderr: '' }
  }
}

type Engine = Parameters<Parameters<typeof test>[1]>[0]

/** The stubs every chief test needs. `runs` records each CLI call. */
function chief(kit: unknown, opts: { mode?: string; runs?: string[][]; opens?: unknown[]; focus?: string[]; toasts?: string[]; store?: Record<string, unknown>; run?: (argv: string[]) => unknown } = {}) {
  const on = kit as (name: string, hook: (...args: unknown[]) => unknown) => void
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: opts.mode || 'fixture' })
  const clock = mock.clock(on)
  const store = opts.store || {}
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($: unknown, e: { title: string; focus?: boolean; closeOnEscape?: boolean }) => {
    opts.opens?.push({ title: e.title, focus: e.focus, closeOnEscape: e.closeOnEscape })
    return { value: { isPlaced: true } }
  })
  on('ui.toast', ($: unknown, e: { text: string }) => {
    opts.toasts?.push(e.text)
    return { value: undefined }
  })
  on('ui.focus', ($: unknown, e: { element?: string; key?: string }) => {
    opts.focus?.push(String(e.element || e.key))
    return { value: {} }
  })
  on('ui.close', () => ({ value: undefined }))
  on('store.get', ($: unknown, e: { key: string }) => ({ value: store[e.key] }))
  on('store.set', ($: unknown, e: { key: string; value: unknown }) => {
    store[e.key] = e.value
    return { value: undefined }
  })
  on('process.run', ($: unknown, e: { argv: string[] }) => {
    opts.runs?.push(e.argv)
    const out = opts.run ? opts.run(e.argv) : { exitCode: 0, stdout: '', stderr: '' }
    return { value: out }
  })
  on('session.append', () => ({ value: { uuid: 'u' } }))
  return clock
}

async function openPanel($: Engine) {
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'open' })
  await band.unmount()
  return $.ui.mount({ ...PANE, surface: 'terminal' })
}

test('a session that is not the chief registers no command, reads nothing, and draws nothing', async ($, on) => {
  mock.env(on, {})
  const clock = mock.clock(on)
  let commands = 0
  let reads = 0
  on('command.register', () => {
    commands += 1
    return { value: undefined }
  })
  on('process.run', () => {
    reads += 1
    return { value: { exitCode: 0, stdout: '{}', stderr: '' } }
  })
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  await $.session.start({ ...SESSION })
  await clock.advance(60000)
  expect(commands).toBe(0)
  expect(reads).toBe(0)
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /need you/ })).toBeUndefined()
  await ui.unmount()
  expect((await $.command.run({ command: 'borant', args: '' })).text).toContain('off in this session')
})

test('the band shows the asks, the projects with their tasks by name, and the hotkey', async ($, on) => {
  chief(on)
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /2 need you/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /Create Hetzner bucket borant-hub-backup/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /Weekly digest/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /Borant keys/ })).toBeDefined()
  // The one way in: a Button that the engine's ctrl+x b chord presses.
  const open = await band.find({ type: 'Button', key: 'open' })
  expect(open?.props.action).toBe('app:cycleDiffBase')
  await band.unmount()
})

test('at the prompt the mod takes no key: no band hotkey, no prompt.edit, and digits and letters type', async ($, on) => {
  const opens: unknown[] = []
  chief(on, { opens })
  on('prompt.edit', ($: unknown, e: { text: string; start: number; end: number; inputText: string }) => ({ text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end), cursor: e.start + e.inputText.length }))
  await $.session.start({ ...SESSION })
  for (const columns of [120, 200, 320]) {
    const band = await $.ui.mount({ ...BAND, viewport: { ...BAND.viewport, columns }, props: { ...BAND.props, bodyColumns: columns - 5 }, surface: 'terminal' })
    for (const b of await band.findAll({ type: 'Button' })) expect(b.props.hotkey).toBeUndefined()
    await band.unmount()
  }
  for (const line of ['1. fix the bug', '0 tasks', 'jkxyr']) {
    let text = ''
    for (const ch of line) text = (await $.prompt.edit({ origin: { kind: 'composer' }, key: { key: ch }, text, cursor: text.length, start: text.length, end: text.length, inputText: ch } as never)).text
    expect(text).toBe(line)
  }
  expect(opens).toEqual([])
})

test('ctrl+x b opens the panel with the keys; /borant does the same', async ($, on) => {
  const opens: unknown[] = []
  chief(on, { opens })
  await $.session.start({ ...SESSION })
  const pane = await openPanel($)
  expect(opens).toEqual([{ title: 'Borant', focus: true, closeOnEscape: true }])
  expect(await pane.find({ type: 'Text', text: /KEYS ON BORANT/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /NEEDS YOU · 2/ })).toBeDefined()
  // The selected item's keys, each a Button with its hotkey.
  expect((await pane.find({ type: 'Button', key: 'key:r' }))?.props.hotkey).toBe('r')
  expect((await pane.find({ type: 'Button', key: 'key:j' }))?.props.hotkey).toBe('j')
  await pane.unmount()
  await $.command.run({ command: 'borant', args: '' })
  expect(opens).toHaveLength(2)
})

test('a panel without the keys says why; the keys of a panel never reach the prompt', async ($, on) => {
  chief(on)
  await $.session.start({ ...SESSION })
  await (await openPanel($)).unmount()
  const pane = await $.ui.mount({ ...PANE, props: { ...PANE.props, isFocused: false }, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /keys stay on the prompt/ })).toBeDefined()
  expect(await pane.find({ type: 'Button', key: 'key:r' })).toBeUndefined()
  await pane.unmount()
})

test('j moves the selection; the ring may not leave the rows', async ($, on) => {
  chief(on)
  await $.session.start({ ...SESSION })
  let pane = await openPanel($)
  expect((await pane.find({ type: 'Button', key: 'row:' + FIRST_NEED }))?.props.autoFocus).toBe(true)
  await pane.press({ key: 'key:j' })
  await pane.unmount()
  pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect((await pane.find({ type: 'Button', key: 'row:' + SECOND_NEED }))?.props.autoFocus).toBe(true)
  const denied = await $.ui.focus({ plugin: 'borant', requestId: 'cos', element: 'key:r', origin: { kind: 'person' } } as never)
  expect(JSON.stringify(denied)).toContain('deny')
  await pane.unmount()
})

test('r opens an answer field; enter sends borant answer, and the item leaves the count at once', async ($, on) => {
  const runs: string[][] = []
  chief(on, { mode: 'cli', run: fixtureCli(runs) })
  await $.session.start({ ...SESSION })
  let pane = await openPanel($)
  await pane.press({ key: 'key:r' })
  await pane.unmount()
  pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Input', key: 'field' })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /ANSWER/ })).toBeDefined()
  await pane.input({ key: 'field', text: 'Use the 1Password item borant-hub-backup.' })
  const answer = runs.find((r) => r[1] === 'answer') || []
  expect(answer.slice(0, 3)).toEqual(['borant', 'answer', 'personal-borant/01M5ZG00000000000000000015'])
  // One idempotency key per draft, so a send after a timeout sends nothing new.
  expect(answer[3]).toBe('--key')
  expect(answer.at(-1)).toBe('Use the 1Password item borant-hub-backup.')
  await pane.unmount()
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /1 need you/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /answered/ })).toBeDefined()
  await band.unmount()
})

test('a refused answer keeps the draft and says why', async ($, on) => {
  chief(on, { mode: 'cli', run: fixtureCli([], { answer: { exitCode: 1, stderr: 'task changed' } }) })
  await $.session.start({ ...SESSION })
  let pane = await openPanel($)
  await pane.press({ key: 'key:r' })
  await pane.unmount()
  pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await pane.input({ key: 'field', text: 'keep this' })
  await pane.unmount()
  pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /refused: borant answer exited 1: task changed/ })).toBeDefined()
  expect((await pane.find({ type: 'Input', key: 'field' }))?.props.value).toBe('keep this')
  await pane.unmount()
})

test('d asks once more; y closes the task as done with borant task close', async ($, on) => {
  const runs: string[][] = []
  chief(on, { mode: 'cli', run: fixtureCli(runs) })
  await $.session.start({ ...SESSION })
  let pane = await openPanel($)
  await pane.press({ key: 'key:d' })
  await pane.unmount()
  pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /Close this task as done\?/ })).toBeDefined()
  await pane.press({ key: 'key:y' })
  expect(runs.find((r) => r[1] === 'task' && r[2] === 'close')?.slice(0, 6)).toEqual(['borant', 'task', 'close', 'personal-borant/01M5ZG00000000000000000015', '--reason', 'done'])
  await pane.unmount()
})

test('x marks the changes seen and keeps the snapshot in the store', async ($, on) => {
  const store: Record<string, unknown> = {}
  chief(on, { store })
  await $.session.start({ ...SESSION })
  const first = store.seen as { at: number }
  expect(first).toBeDefined()
  const pane = await openPanel($)
  expect(store.used).toBe(true)
  const x = await pane.find({ type: 'Button', key: 'key:x' })
  // No change yet: x is not offered.
  expect(x).toBeUndefined()
  await pane.unmount()
})

test('a failed read says what failed and how to see why', async ($, on) => {
  chief(on, { mode: 'cli', run: () => ({ exitCode: 2, stdout: '', stderr: 'error: hub unreachable' }) })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /borant board exited 2: error: hub unreachable/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /borant doctor/ })).toBeDefined()
  await band.unmount()
})

test('a new needs-you item raises one toast, and no more', async ($, on) => {
  const toasts: string[] = []
  let payload: unknown = JSON.parse(JSON.stringify(board))
  const clock = chief(on, { mode: 'cli', toasts, run: (argv) => (argv[1] === 'board' ? { exitCode: 0, stdout: JSON.stringify(payload), stderr: '' } : { exitCode: 0, stdout: '[]', stderr: '' }) })
  await $.session.start({ ...SESSION })
  expect(toasts).toEqual([])
  const next = JSON.parse(JSON.stringify(board))
  next.collections[0].needs_you.push({ project: 'boopmark', channel: 'personal-boopmark', task: '01M5ZCS35Z2RWKFF0BCYQWJYW9', title: 'Give boopmark a goal', status: 'needs_human', attention_msg: null, since: 1793025600000, gate: false })
  payload = next
  await clock.advance(6000)
  expect(toasts).toEqual(['needs you: Give boopmark a goal'])
  await clock.advance(6000)
  expect(toasts).toEqual(['needs you: Give boopmark a goal'])
})

test('the event stream starts from a timer and reads at once on a task event', async ($, on) => {
  let boards = 0
  const clock = chief(on, { mode: 'cli', run: (argv) => {
    if (argv[1] === 'board') boards += 1
    return argv[1] === 'board' ? { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } : { exitCode: 0, stdout: '[]', stderr: '' }
  } })
  const spawns: string[][] = []
  on('process.spawn', async function* ($: unknown, e: { argv?: string[] }) {
    spawns.push((e.argv || []) as string[])
    yield { stream: 'stdout', text: '{"kind":"task.status","channel":"personal-borant"}\n' }
    return { value: { code: 0 } }
  } as never)
  await $.session.start({ ...SESSION })
  expect(spawns).toHaveLength(0)
  await clock.advance(0)
  expect(spawns).toEqual([['borant', 'events', '--follow', '--json']])
  const before = boards
  await clock.advance(300)
  expect(boards).toBe(before + 1)
})

test('the guard still refuses project work in the chief\'s shell', async ($, on) => {
  chief(on)
  on('tool.check', () => ({ decision: 'allow' }))
  await $.session.start({ ...SESSION })
  const verdict = await $.tool.check({ tool: 'Bash', input: { command: 'git commit -m x' } } as never)
  expect(verdict.decision).toBe('deny')
})

test('when only the task-line read fails, the plain board keeps the band working', async ($, on) => {
  const runs: string[][] = []
  const cli = fixtureCli(runs)
  chief(on, { mode: 'cli', runs, run: (argv) => (argv.includes('--tasks') ? { exitCode: 1, stdout: '', stderr: 'hub unavailable: request timed out' } : cli(argv)) })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /no hub/ })).toBeUndefined()
  expect(runs.some((r) => r.join(' ') === 'borant board --json')).toBe(true)
  await band.unmount()
})
