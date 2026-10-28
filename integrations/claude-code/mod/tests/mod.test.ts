// The mod in a session: the role gate, the band, the digest, the pane's
// navigation and buttons, and a board or task source that fails.
import { expect, mock, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import { tasks as fixtureTasks } from '../fixtures/tasks.js'

const SESSION = { surface: 'terminal', isInteractive: true, cwd: '/work' } as const

// What Claude Code passes to a ui.render hook for the band on a 120-column terminal.
const BAND = {
  plugin: 'borant',
  component: 'AbovePrompt',
  requestId: 'above-prompt',
  viewport: { columns: 120, rows: 40, isFullscreen: true },
  props: {
    hasSurvey: false,
    isWorking: false,
    maxRows: 10,
    bodyColumns: 120,
    scroll: { offset: 0, bodyRows: 10 },
    view: {},
  },
} as const

// What Claude Code passes to a ui.render hook for the pane on a wide terminal.
const PANE = {
  plugin: 'borant',
  component: 'Pane',
  requestId: 'cos',
  viewport: { columns: 182, rows: 50, isFullscreen: true },
  props: {
    title: 'Chief of staff',
    isFocused: true,
    bodyColumns: 60,
    placement: 'dock',
    scroll: { offset: 0, bodyRows: 40 },
    view: {},
  },
} as const

// What Claude Code would draw at a site the mod leaves alone.
const ENGINE_ROW = { type: 'Text', props: {}, children: ['drawn by Claude Code'] }

/** The stubs every chief test needs: the clock, the pane, the ring, the scroll. */
function chiefStubs(kit: unknown, opened: string[] = [], closes: string[] = []) {
  // The kit hands test files its own typed `on`, which only registers stubs.
  const on = kit as (name: string, hook: (...args: unknown[]) => unknown) => void

  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opened.push(e.title)
    return { value: { isPlaced: true } }
  })
  on('ui.toast', () => ({ value: undefined }))
  // The mod moves the engine's ring and the pane's window itself.
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  on('ui.close', ($, e) => {
    closes.push(e.id)
    return { value: undefined }
  })
}

test('a session that is not the chief registers no timer, no command, and draws nothing', async ($, on) => {
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
  expect(await ui.find({ type: 'Text', text: /Elastic compute/ })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /chief of staff/ })).toBeUndefined()
  await ui.unmount()

  const answer = await $.command.run({ command: 'cos', args: '' })
  expect(answer.text).toContain('off in this session')
})

test('the chief draws the fixture board in the band, and the digest', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  mock.clock(on)
  let runs = 0
  const toasts: string[] = []
  on('command.register', () => ({ value: undefined }))
  on('process.run', () => {
    runs += 1
    return { value: { exitCode: 0, stdout: '{}', stderr: '' } }
  })
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.open', () => ({ value: { isPlaced: true } }))

  await $.session.start({ ...SESSION })
  // The fixture source never starts borant.
  expect(runs).toBe(0)
  // The first drawing is not news.
  expect(toasts).toEqual([])

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /Elastic compute/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /Needs you \(2\)/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /2 blocked/ })).toBeDefined()
  await ui.unmount()

  const answer = await $.command.run({ command: 'cos', args: '' })
  expect(answer.text).toContain('Needs you (2)')
  expect(answer.text).toContain('Elastic compute')
  expect((await $.command.run({ command: 'cos', args: 'borant' })).text).not.toContain('boopmark')
})

test('a new needs-you item raises one toast, and no more', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
  const toasts: string[] = []
  let payload: unknown = JSON.parse(JSON.stringify(board))
  on('command.register', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: JSON.stringify(payload), stderr: '' } }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })

  await $.session.start({ ...SESSION })
  expect(toasts).toEqual([])

  const next = JSON.parse(JSON.stringify(board)) as typeof board
  next.collections[0].needs_you.push({
    project: 'boopmark',
    channel: 'personal-boopmark',
    task: '01M5ZCS35Z2RWKFF0BCYQWJYW9',
    title: 'Give boopmark a goal',
    status: 'needs_human',
    attention_msg: null,
    since: 1793025600000,
    gate: false,
  } as (typeof board)['collections'][0]['needs_you'][number])
  payload = next

  await clock.advance(15000)
  expect(toasts).toEqual(['needs you: Give boopmark a goal'])
  await clock.advance(15000)
  expect(toasts).toEqual(['needs you: Give boopmark a goal'])
})

test('a board source that fails draws "board unavailable", and nothing else', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  mock.clock(on)
  const toasts: string[] = []
  on('command.register', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 2, stdout: '', stderr: 'error: unrecognized subcommand' } }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })

  await $.session.start({ ...SESSION })

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /board unavailable/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /unrecognized subcommand/ })).toBeDefined()
  await ui.unmount()

  expect((await $.command.run({ command: 'cos', args: '' })).text).toBe(
    'cos: board unavailable \u00b7 borant board exited 2: error: unrecognized subcommand',
  )
  expect(toasts).toEqual([])
})

test('a failed read empties the board and names the reason', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
  let failing = false
  on('command.register', () => ({ value: undefined }))
  on('process.run', () =>
    failing
      ? {
          value: {
            exitCode: 1,
            stdout: '',
            stderr: 'Error: cannot resolve this session; refusing to read the board as the person\n',
          },
        }
      : { value: { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } },
  )
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.toast', () => ({ value: undefined }))

  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /Elastic compute/ })).toBeDefined()
  await ui.unmount()

  failing = true
  // `/cos` reads the board at once, so the failed read lands before the mount.
  expect((await $.command.run({ command: 'cos', args: '' })).text).toContain(
    'refusing to read the board as the person',
  )
  await clock.advance(15000)
  const empty = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(
    await empty.find({ type: 'Text', text: /refusing to read the board/ }),
  ).toBeDefined()
  expect(await empty.find({ type: 'Text', text: /Elastic compute/ })).toBeUndefined()
  await empty.unmount()
})

test('a board source that rejects draws "board unavailable", and nothing else', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  mock.clock(on)
  const toasts: string[] = []
  on('command.register', () => ({ value: undefined }))
  on('process.run', () => ({ deny: 'borant: command not found' }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })

  await $.session.start({ ...SESSION })

  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /board unavailable/ })).toBeDefined()
  await ui.unmount()
  expect(toasts).toEqual([])
})

test('the pane has its tabs, and its buttons submit a prompt instead of calling the hub', async ($, on) => {
  const opened: string[] = []
  const prompts: string[] = []
  chiefStubs(on, opened)
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })

  await $.session.start({ ...SESSION })
  expect(await $.command.run({ command: 'cos', args: 'pane' })).toEqual({})
  expect(opened).toEqual(['borant'])

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await ui.find({ type: 'Button', key: 'tab-needs' })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'tab-projects' })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'tab-help' })).toBeDefined()

  // The Needs you tab opens on the gate, and Done submits its title and ids.
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeDefined()
  await ui.press({ key: 'done-0' })
  expect(prompts).toEqual([
    'Chris marked done: Create Hetzner bucket borant-hub-backup + 1Password item (personal-borant/01M5ZG00000000000000000015)',
  ])

  await ui.press({ key: 'tab-projects' })
  expect(await ui.find({ type: 'Text', text: /Elastic compute/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /Golden image/ })).toBeDefined()

  await ui.press({ key: 'tab-needs' })
  await ui.press({ key: 'ask-1' })
  expect(prompts[1]).toContain('Chris asks the orchestrator: Revoke old willisrocks gh token?')
  await ui.unmount()
})

test('the help view opens from the band button and /cos help', async ($, on) => {
  const opened: string[] = []
  chiefStubs(on, opened)

  await $.session.start({ ...SESSION })

  // The band draws the hint and its help button.
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /ctrl\+x tab focus/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /j\/k move/ })).toBeDefined()
  expect(await band.find({ type: 'Button', key: 'help' })).toBeDefined()
  await band.press({ key: 'help' })
  expect(opened).toEqual(['borant'])
  await band.unmount()

  // The help view lists the keys and the commands.
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /ctrl\+x ctrl\+a/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /\/cos pane/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /Ask orchestrator/ })).toBeDefined()
  await pane.press({ key: 'tab-needs' })
  expect(await pane.find({ type: 'Text', text: /ctrl\+x ctrl\+a/ })).toBeUndefined()
  await pane.press({ key: 'tab-help' })
  expect(await pane.find({ type: 'Text', text: /ctrl\+x ctrl\+a/ })).toBeDefined()
  await pane.unmount()

  // `/cos help` prints the same text, and starts no turn.
  const answer = await $.command.run({ command: 'cos', args: 'help' })
  expect(answer.text).toContain('ctrl+x tab')
  expect(answer.text).toContain('/cos help')
})

test('j, k, and a focus move select the row in every view', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  // The footer names the vim keys, and the first row is the selection.
  const down = await ui.find({ key: 'nav-down' })
  const up = await ui.find({ key: 'nav-up' })
  const leftKey = await ui.find({ key: 'nav-left' })
  const rightKey = await ui.find({ key: 'nav-right' })
  expect(down?.props.hotkey).toBe('j')
  expect(up?.props.hotkey).toBe('k')
  expect(leftKey?.props.hotkey).toBe('h')
  expect(rightKey?.props.hotkey).toBe('l')
  expect(JSON.stringify(await ui.find({ key: 'row-0' })).includes('\u258c')).toBe(true)

  // j and k in the Needs you list.
  await ui.press({ key: 'nav-down' })
  expect(JSON.stringify(await ui.find({ key: 'row-1' })).includes('\u258c')).toBe(true)
  await ui.press({ key: 'nav-up' })
  expect(JSON.stringify(await ui.find({ key: 'row-0' })).includes('\u258c')).toBe(true)

  // When the engine moves the ring (Tab, a click, an arrow where the surface
  // grants one), its ui.focus names the row and the selection follows.
  await $.ui.focus({
    component: 'Pane',
    requestId: 'cos',
    plugin: 'borant',
    element: 'row-1',
    origin: { kind: 'person' },
  })
  expect(JSON.stringify(await ui.find({ key: 'row-1' })).includes('\u258c')).toBe(true)

  // The same keys in the Projects tree.
  await ui.press({ key: 'tab-projects' })
  expect(JSON.stringify(await ui.find({ key: 'row-0' })).includes('\u258c')).toBe(true)
  await ui.press({ key: 'nav-down' })
  expect(JSON.stringify(await ui.find({ key: 'row-1' })).includes('\u258c')).toBe(true)

  // And in the help view.
  await ui.press({ key: 'tab-help' })
  await ui.press({ key: 'nav-down' })
  expect(JSON.stringify(await ui.find({ key: 'row-1' })).includes('\u258c')).toBe(true)
  await ui.unmount()
})

test('h and l fold a node, open it, and switch tabs', async ($, on) => {
  const opened: string[] = []
  chiefStubs(on, opened)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-projects' })
  // The first project is boopmark, open, with its one epic under it.
  expect(await ui.find({ type: 'Text', text: /Move production to Fly\.io/ })).toBeDefined()
  await ui.press({ key: 'nav-left' })
  expect(await ui.find({ type: 'Text', text: /Move production to Fly\.io/ })).toBeUndefined()
  await ui.press({ key: 'nav-right' })
  expect(await ui.find({ type: 'Text', text: /Move production to Fly\.io/ })).toBeDefined()

  // l on an open project opens it: its view lists its epic and its plain tasks.
  await ui.press({ key: 'nav-right' })
  expect(opened).toEqual(['borant', 'borant \u203a boopmark'])
  expect(await ui.find({ type: 'Text', text: /plain tasks/ })).toBeDefined()

  // h at the top of the view goes back, and at the root switches tabs.
  await ui.press({ key: 'nav-left' })
  expect(opened).toEqual(['borant', 'borant \u203a boopmark', 'borant'])
  await ui.press({ key: 'tab-help' })
  await ui.press({ key: 'nav-right' })
  expect(await ui.find({ type: 'Text', text: /ctrl\+x ctrl\+a/ })).toBeUndefined()
  await ui.press({ key: 'nav-left' })
  expect(await ui.find({ type: 'Text', text: /ctrl\+x ctrl\+a/ })).toBeDefined()
  await ui.unmount()
})

test('Enter walks project, epic, and task detail, and esc walks back to close', async ($, on) => {
  const opened: string[] = []
  const closes: string[] = []
  chiefStubs(on, opened, closes)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-projects' })
  // Enter on the selected project opens it.
  await ui.press({ key: 'row-0' })
  expect(opened).toContain('borant \u203a boopmark')
  // Enter on its epic opens the epic's child tasks.
  await ui.press({ key: 'row-0' })
  expect(opened).toContain('borant \u203a boopmark \u203a Move production to Fly.io')
  expect(await ui.find({ type: 'Text', text: /The fly\.toml/ })).toBeDefined()
  // Enter on a child task opens its detail.
  await ui.press({ key: 'row-0' })
  expect(await ui.find({ type: 'Text', text: /The fly\.toml/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /unclaimed/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /updated/ })).toBeDefined()

  // esc goes back one level at a time, then closes the pane at the root.
  await ui.press({ key: 'nav-back' })
  expect(await ui.find({ type: 'Text', text: /unclaimed/ })).toBeUndefined()
  await ui.press({ key: 'nav-back' })
  await ui.press({ key: 'nav-back' })
  expect(closes).toEqual([])
  await ui.press({ key: 'nav-back' })
  expect(closes).toEqual(['cos'])
  await ui.unmount()
})

test('a click selects a row, and a second click opens it', async ($, on) => {
  const opened: string[] = []
  chiefStubs(on, opened)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-projects' })
  // The rows are boopmark, its epic, borant: a first click only selects.
  await ui.press({ key: 'row-2' })
  expect(JSON.stringify(await ui.find({ key: 'row-2' })).includes('\u258c')).toBe(true)
  expect(opened).toEqual(['borant'])
  // A second click opens it: its view has the saga's plain tasks line.
  await ui.press({ key: 'row-2' })
  expect(await ui.find({ type: 'Text', text: /plain tasks  3 open/ })).toBeDefined()

  // The ▸ of a row opens it in one press.
  await ui.press({ key: 'tab-projects' })
  await ui.press({ key: 'open-3' })
  expect(opened).toContain('borant \u203a Elastic compute \u203a Hub backup to object storage')
  await ui.unmount()
})

test('the task detail shows the owner, the blockers, the PR link, and the last update', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-projects' })
  // borant › Elastic compute › Hub backup › The backup job
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-1' })
  await ui.press({ key: 'row-1' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  const text = await ui.find({ type: 'Text', text: /updated\s+\d{4}-\d{2}-\d{2}/ })
  expect(text).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /The backup job/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /w-hubbackup/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /The bucket  open/ })).toBeDefined()
  await ui.unmount()
})

test('a failing borant tasks shows "tasks unavailable" without a throw', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  mock.clock(on)
  const toasts: string[] = []
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', () => ({ value: { isPlaced: true } }))
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) return { value: { exitCode: 2, stdout: '', stderr: 'error: no such channel' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } }
  })

  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-projects' })
  // The borant project, its saga's first epic, which has no task list.
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-1' })
  await ui.press({ key: 'row-1' })
  expect(await ui.find({ type: 'Text', text: /tasks unavailable/ })).toBeDefined()
  expect(toasts).toEqual([])
  await ui.unmount()
})

test('the help lists every key', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-help' })
  for (const text of [
    /\u2191 \u2193/,
    /j k/,
    /h l/,
    /enter/,
    /esc/,
    /tab/,
    /click/,
    /ctrl\+x tab/,
    /ctrl\+x ctrl\+a/,
    /borant tasks <channel>/,
  ]) {
    expect(await ui.find({ type: 'Text', text })).toBeDefined()
  }
  await ui.unmount()
})

test('a channel is read once, and again on the tick only while its view is open', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
  const reads: string[] = []
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', () => ({ value: { isPlaced: true } }))
  on('ui.toast', () => ({ value: undefined }))
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  on('ui.close', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) {
      reads.push(argv[2])
      return { value: { exitCode: 0, stdout: JSON.stringify((fixtureTasks as Record<string, unknown>)['personal-borant']), stderr: '' } }
    }
    return { value: { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } }
  })

  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'tab-projects' })
  // The borant project names one channel: personal-borant.
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  expect(reads).toEqual(['personal-borant'])
  // Its epic lives in the same channel, so the cache answers.
  await ui.press({ key: 'row-1' })
  await ui.press({ key: 'row-1' })
  expect(reads).toEqual(['personal-borant'])

  // The 15 s tick reads the open view's channel again.
  await clock.advance(15000)
  expect(reads).toEqual(['personal-borant', 'personal-borant'])

  // Closed, the tick reads nothing more.
  await ui.press({ key: 'nav-back' })
  await ui.press({ key: 'nav-back' })
  await ui.press({ key: 'nav-back' })
  await clock.advance(15000)
  expect(reads).toEqual(['personal-borant', 'personal-borant'])
  await ui.unmount()
})

test('the band highlights the whole focused row, not only its chip', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.ui.focus({
    component: 'AbovePrompt',
    requestId: 'above-prompt',
    plugin: 'borant',
    element: 'project-1',
    origin: { kind: 'person' },
  })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await ui.find({ key: 'band-row-3' }))?.props.backgroundColor).toBe('promptBorder')
  expect((await ui.find({ key: 'band-row-2' }))?.props.backgroundColor).toBeUndefined()
  // Every cell on the selection background takes a contrasting foreground,
  // including the dim stats and the colored ones.
  const saga = await ui.find({ type: 'Text', text: /Elastic compute/ })
  expect(saga?.props.color).toBe('inverseText')
  expect(saga?.props.dimColor).toBe(false)
  const blocked = await ui.find({ type: 'Text', text: /^2 blocked/ })
  expect(blocked?.props.color).toBe('inverseText')
  // The progress bar's filled cells keep their green on the selection.
  const fills = await ui.findAll({ type: 'Text', text: /^\u2588/ })
  expect(fills.length).toBeGreaterThan(0)
  for (const fill of fills) expect(fill.props.color).toBe('green')

  // When the pane takes the keys the band no longer has them, so its row goes
  // back to normal colors instead of keeping a gray highlight.
  await $.ui.focus({
    component: 'Pane',
    requestId: 'cos',
    plugin: 'borant',
    element: 'row-0',
    origin: { kind: 'person' },
  })
  expect((await ui.find({ key: 'band-row-3' }))?.props.backgroundColor).toBeUndefined()
  await ui.unmount()
})

test('a band press hands the keyboard to the pane it opens', async ($, on) => {
  const opens: { title?: string; focus?: boolean }[] = []
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opens.push({ title: e.title, focus: e.focus })
    return { value: { isPlaced: true } }
  })
  on('ui.toast', () => ({ value: undefined }))
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  on('ui.close', () => ({ value: undefined }))

  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'project-0' })
  // The press opens the pane; the handoff asks for its keyboard once the
  // band has let go, then draws the band's controls again.
  expect(opens[0]).toEqual({ title: 'borant \u203a boopmark', focus: true })
  await clock.advance(200)
  await clock.advance(200)
  expect(opens).toHaveLength(2)
  expect(opens[1]).toEqual({ title: 'borant \u203a boopmark', focus: true })
  expect(await band.find({ type: 'Button', key: 'project-0' })).toBeDefined()
  await band.unmount()
})
