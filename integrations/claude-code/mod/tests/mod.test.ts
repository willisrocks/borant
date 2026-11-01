// The mod in a session: the role gate, the band and its keys, the digest,
// Done and Ask, and a board source that fails.
import { expect, mock, test } from 'claude-code/testing'
import board from '../fixtures/board.js'

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

// What Claude Code would draw at a site the mod leaves alone.
const ENGINE_ROW = { type: 'Text', props: {}, children: ['drawn by Claude Code'] }

/** The stubs every chief test needs: the clock, the pane, the ring, the scroll. */
function chiefStubs(kit: unknown, opened: string[] = [], closes: string[] = [], focused: string[] = [], toasts: string[] = []) {
  // The kit hands test files its own typed `on`, which only registers stubs.
  const on = kit as (name: string, hook: (...args: unknown[]) => unknown) => void

  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opened.push(e.title)
    return { value: { isPlaced: true } }
  })
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  // The mod moves the engine's ring and the pane's window itself.
  on('ui.focus', ($, e) => {
    const element = e.element || (e as { key?: string }).key
    if (element) focused.push(element)
    return { value: {} }
  })
  on('ui.scroll', () => ({ value: {} }))
  on('ui.close', ($, e) => {
    closes.push(e.id)
    return { value: undefined }
  })
  return clock
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
  expect(await ui.find({ type: 'Text', text: /2 need you/ })).toBeDefined()
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
    await empty.find({ type: 'Text', text: /cannot resolve this session/ }),
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

const GATE = 'q:personal-borant/01M5ZG00000000000000000015'
const TOKEN = 'q:personal-borant/01M5ZG00000000000000000016'

test('a digit selects its row, and the band then holds j, k, d, and a', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  // At rest: digits on the rows, no selection, and the hint names the digits.
  expect((await ui.find({ type: 'Button', key: GATE }))?.props.hotkey).toBe('1')
  expect((await ui.find({ type: 'Button', key: 'p:notes' }))?.props.hotkey).toBe('4')
  expect(await ui.find({ type: 'Text', text: /1-4/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'done' })).toBeUndefined()
  expect(await ui.find({ type: 'Button', key: 'band-down' })).toBeUndefined()

  await ui.press({ key: GATE })
  // The whole row draws selected, and only it carries Done and Ask.
  expect((await ui.find({ key: 'band-row-' + GATE }))?.props.backgroundColor).toBe('promptBorder')
  expect((await ui.find({ key: 'band-row-' + TOKEN }))?.props.backgroundColor).toBeUndefined()
  expect((await ui.find({ type: 'Button', key: 'done' }))?.props.hotkey).toBe('d')
  expect((await ui.find({ type: 'Button', key: 'ask' }))?.props.hotkey).toBe('a')
  expect((await ui.find({ type: 'Button', key: 'band-down' }))?.props.hotkey).toBe('j')
  expect((await ui.find({ type: 'Button', key: 'band-up' }))?.props.hotkey).toBe('k')
  expect(await ui.find({ type: 'Text', text: /type to leave/ })).toBeDefined()

  // j and k move the selection, and wrap; a project row has no Done.
  await ui.press({ key: 'band-down' })
  expect((await ui.find({ key: 'band-row-' + TOKEN }))?.props.backgroundColor).toBe('promptBorder')
  await ui.press({ key: 'band-down' })
  expect((await ui.find({ key: 'band-row-p:borant' }))?.props.backgroundColor).toBe('promptBorder')
  expect(await ui.find({ type: 'Button', key: 'done' })).toBeUndefined()
  await ui.press({ key: 'band-up' })
  await ui.press({ key: 'band-up' })
  await ui.press({ key: 'band-up' })
  expect((await ui.find({ key: 'band-row-p:notes' }))?.props.backgroundColor).toBe('promptBorder')

  // The ring leaving the band hands the keys back to the prompt.
  await $.ui.focus({ component: 'AbovePrompt', requestId: 'above-prompt', origin: { kind: 'person' } })
  expect((await ui.find({ key: 'band-row-p:notes' }))?.props.backgroundColor).toBeUndefined()
  expect(await ui.find({ type: 'Button', key: 'band-down' })).toBeUndefined()
  await ui.unmount()
})

test('after a digit, the band keys typed at the empty prompt act on the band and are consumed', async ($, on) => {
  const prompts: string[] = []
  const clock = chiefStubs(on)
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  // What the editor does with an edit nobody consumed: the splice.
  on('prompt.edit', ($, e) => ({
    text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end),
    cursor: e.start + e.inputText.length,
  }))
  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const type = (key: string) =>
    $.prompt.edit({ origin: { kind: 'composer' }, key: { key }, text: '', cursor: 0, start: 0, end: 0, inputText: key.length === 1 ? key : '' })
  // With no selection, j is typing.
  expect((await type('j')).text).toBe('j')

  await ui.press({ key: GATE })
  expect((await type('j')).text).toBe('')
  expect((await ui.find({ key: 'band-row-' + TOKEN }))?.props.backgroundColor).toBe('promptBorder')
  expect((await type('k')).text).toBe('')
  await type(' ')
  expect(await ui.find({ type: 'Text', text: /owner orchestrator/ })).toBeDefined()
  expect((await type('d')).text).toBe('')
  expect(prompts).toHaveLength(1)

  // Any other key is typing: it lands, and the selection goes.
  expect((await type('x')).text).toBe('x')
  expect((await ui.find({ key: 'band-row-' + GATE }))?.props.backgroundColor).toBeUndefined()

  // Thirty idle seconds after a digit, the prompt's keys are its own again.
  await ui.press({ key: 'p:notes' })
  await clock.advance(30000)
  expect((await type('d')).text).toBe('d')
  await ui.unmount()
})

test('pressing the selected row again opens it in place, and again closes it', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await ui.press({ key: 'p:borant' })
  expect(await ui.find({ type: 'Text', text: /Delete borant-2/ })).toBeUndefined()
  await ui.press({ key: 'p:borant' })
  // The project's epics, its orchestrator, all on the selection.
  expect(await ui.find({ type: 'Text', text: /Delete borant-2/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /orchestrator · online/ })).toBeDefined()
  expect((await ui.find({ key: 'band-row-p:borant-orch-0' }))?.props.backgroundColor).toBe('promptBorder')
  await ui.press({ key: 'p:borant' })
  expect(await ui.find({ type: 'Text', text: /Delete borant-2/ })).toBeUndefined()

  // An open needs-you item names its owner and channel, buttons on their own line.
  await ui.press({ key: GATE })
  await ui.press({ key: GATE })
  expect(await ui.find({ type: 'Text', text: /owner orchestrator/ })).toBeDefined()
  expect(await ui.find({ key: 'band-row-' + GATE + '-actions' })).toBeDefined()
  // j closes it: one open row, and only the selected one.
  await ui.press({ key: 'band-down' })
  expect(await ui.find({ type: 'Text', text: /owner orchestrator/ })).toBeUndefined()
  await ui.unmount()
})

test('a click selects a row, and a second click opens it', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const click = async () => {
    await $.ui.focus({ component: 'AbovePrompt', requestId: 'above-prompt', plugin: 'borant', element: 'p:borant', origin: { kind: 'person' } })
    await ui.press({ key: 'p:borant' })
  }
  await click()
  expect((await ui.find({ key: 'band-row-p:borant' }))?.props.backgroundColor).toBe('promptBorder')
  expect(await ui.find({ type: 'Text', text: /Delete borant-2/ })).toBeUndefined()
  await click()
  expect(await ui.find({ type: 'Text', text: /Delete borant-2/ })).toBeDefined()
  await ui.unmount()
})

test('Done submits a prompt, and shows each step in the buttons\' place', async ($, on) => {
  const prompts: string[] = []
  const toasts: string[] = []
  let live = JSON.parse(JSON.stringify(board))
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.toast', ($, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  on('process.run', () => ({ value: { exitCode: 0, stdout: JSON.stringify(live), stderr: '' } }))

  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await ui.press({ key: GATE })
  await ui.press({ key: 'done' })
  expect(prompts).toHaveLength(1)
  expect(prompts[0]).toContain(
    'Chris marked done: Create Hetzner bucket borant-hub-backup + 1Password item (personal-borant/01M5ZG00000000000000000015). ',
  )
  // The prompt says how: the hub lets only the owner close a plain task as done.
  expect(prompts[0]).toContain('send the owner of personal-borant a direct message')
  // The prompt landed: the status takes the buttons' place, with no Done to press twice.
  expect(await ui.find({ type: 'Text', text: /the chief is closing it/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'done' })).toBeUndefined()

  // The hub closes it: the row stays, marked closed, then goes.
  live = JSON.parse(JSON.stringify(live))
  for (const c of live.collections) {
    c.needs_you = (c.needs_you || []).filter((q: { task: string }) => q.task !== '01M5ZG00000000000000000015')
  }
  await clock.advance(5000)
  expect(await ui.find({ type: 'Text', text: /closed/ })).toBeDefined()
  expect(toasts).toContain('closed: Create Hetzner bucket borant-hub-backup + 1Password item')
  await clock.advance(15000)
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeUndefined()
  await ui.unmount()
})

test('a Done the hub has not closed after two minutes shows as still open, with Done again', async ($, on) => {
  const clock = chiefStubs(on)
  on('prompt.submit', ($, e) => ({ text: e.text }))
  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await ui.press({ key: GATE })
  await ui.press({ key: 'done' })
  await clock.advance(120000)
  // The status shows on the row whether or not it is selected; its button
  // only on the selected row.
  expect(await ui.find({ type: 'Text', text: /still open after 2m/ })).toBeDefined()
  await ui.press({ key: GATE })
  expect((await ui.find({ type: 'Button', key: 'done' }))?.props.label).toBe('Done again')
  await ui.unmount()
})

test('Ask orchestrator sends to the channel owner itself, and the chief is only told', async ($, on) => {
  const prompts: string[] = []
  const sends: string[][] = []
  const toasts: string[] = []
  chiefStubs(on, [], [], [], toasts)
  const session = mock.session(on)
  on('process.run', ($, e) => {
    sends.push((e.argv || []) as string[])
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })

  await $.session.start({ ...SESSION })
  const ui = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await ui.press({ key: TOKEN })
  await ui.press({ key: 'ask' })
  expect(sends).toHaveLength(1)
  expect(sends[0].slice(0, 3)).toEqual(['borant', 'send', 'orchestrator'])
  expect(sends[0][3]).toContain('Revoke old willisrocks gh token? (personal-borant/01M5ZG00000000000000000016)')
  expect(sends[0][3]).toContain('Answer the chief')
  expect(prompts).toHaveLength(0)
  expect(toasts).toContain('asked orchestrator: Revoke old willisrocks gh token?')
  expect(await ui.find({ type: 'Text', text: /asked orchestrator/ })).toBeDefined()
  expect((await ui.find({ type: 'Button', key: 'ask' }))?.props.label).toBe('Ask again')

  // The chief is told with a row it reads, not a prompt that starts a turn.
  const rows = session.appended().map((r) => JSON.stringify(r.message))
  expect(rows.filter((r) => r.includes('Do not answer it yourself'))).toHaveLength(1)
  await ui.unmount()
})

test('/cos help names the band keys, and /cos pane says the board is the band', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  const help = (await $.command.run({ command: 'cos', args: 'help' })).text
  for (const key of ['1-4', 'j k', 'space', 'enter', 'd ', 'a ', 'esc', 'ctrl+x tab', '/cos help']) expect(help).toContain(key)
  expect(help).not.toContain('/cos pane')
  expect((await $.command.run({ command: 'cos', args: 'pane' })).text).toContain('the band above the prompt')
})

test('an event from the host stream reads the board again at once', async ($, on) => {
  let reads = 0
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('process.run', () => {
    reads += 1
    return { value: { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } }
  })
  let spawned: string[] = []
  on('process.spawn', async function* ($, e) {
    spawned = (e.argv || []) as string[]
    yield { stream: 'stdout', text: '{"kind":"host.registered"}\n{"kind":"task.st' }
    yield { stream: 'stdout', text: 'atus","channel":"personal-borant"}\n' }
    return { value: { code: 0 } }
  })

  await $.session.start({ ...SESSION })
  expect(spawned).toEqual(['borant', 'events', '--follow', '--json'])
  const first = reads
  await clock.advance(300)
  expect(reads).toBe(first + 1)

  // The title row says how fresh the board is: this stream has ended, so
  // the 5 s timer reads it.
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /^polling/ })).toBeDefined()
  await band.unmount()
})
