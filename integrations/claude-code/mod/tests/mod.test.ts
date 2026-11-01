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

  // The band is one ticker line: what needs you, then each project, each
  // with the digit that opens the pane on it, then how fresh the board is.
  const wide = { ...BAND, viewport: { ...BAND.viewport, columns: 200 }, props: { ...BAND.props, bodyColumns: 195 } }
  const ui = await $.ui.mount({ ...wide, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /\u25b2 2 need you/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^2: $/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^borant$/ })).toBeDefined()
  // The fixture source follows no event stream, so the board polls.
  expect(await ui.find({ type: 'Text', text: /^polling/ })).toBeDefined()
  // From 144 columns no Button has a digit hotkey: a bare digit at an empty
  // prompt would press it before the mod's prompt.edit hook sees the digit.
  expect((await ui.findAll({ type: 'Button' })).map((b) => b.props.key)).toEqual(['help'])
  await ui.unmount()

  // Below 144 the engine opens only an asked pane, so each entry is a Button
  // whose hotkey is its digit; the engine draws the `2: ` before it.
  const narrow = await $.ui.mount({ ...BAND, surface: 'terminal' })
  const buttons = (await narrow.findAll({ type: 'Button' })).map((b) => b.props.key + ':' + (b.props.hotkey || ''))
  expect(buttons).toEqual(['tick-1:1', 'tick-2:2', 'tick-3:3', 'help:'])
  expect(await narrow.find({ type: 'Text', text: /^2: $/ })).toBeUndefined()
  await narrow.unmount()

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
  expect(await ui.find({ type: 'Text', text: /2\/5/ })).toBeDefined()
  await ui.unmount()

  failing = true
  // `/cos` reads the board at once, so the failed read lands before the mount.
  expect((await $.command.run({ command: 'cos', args: '' })).text).toContain(
    'refusing to read the board as the person',
  )
  await clock.advance(15000)
  const empty = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await empty.find({ type: 'Text', text: /board unavailable/ })).toBeDefined()
  expect(await empty.find({ type: 'Text', text: /cannot resolve this session/ })).toBeDefined()
  expect(await empty.find({ type: 'Text', text: /2\/5/ })).toBeUndefined()
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

/** A key typed at the prompt, as the engine raises prompt.edit for it. */
function typed(text: string, key: string) {
  return {
    origin: { kind: 'composer' },
    key: { key },
    text,
    cursor: text.length,
    start: text.length,
    end: text.length,
    inputText: key,
  } as const
}

/** The keys of the pane lines drawn on the selection background. */
async function painted(ui: { drawn: () => Promise<unknown> }) {
  const out: string[] = []
  const walk = (node: unknown) => {
    if (!node || typeof node !== 'object') return
    const { props, children } = node as { props?: Record<string, unknown>; children?: unknown[] }
    if (props?.backgroundColor === 'promptBorder' && typeof props.key === 'string') out.push(props.key)
    for (const child of children || []) walk(child)
  }
  walk(await ui.drawn())
  return out
}

/** Whether a row's glyph Button holds the selection. */
async function selected(ui: { find: (q: object) => Promise<{ props: Record<string, unknown> } | undefined> }, key: string) {
  return (await ui.find({ type: 'Button', key }))?.props.autoFocus === true
}

test('the pane is one list: needs-you cards, then the projects', async ($, on) => {
  const opened: string[] = []
  const prompts: string[] = []
  chiefStubs(on, opened)
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })

  await $.session.start({ ...SESSION })
  expect(await $.command.run({ command: 'cos', args: 'pane' })).toEqual({})
  expect(opened).toEqual(['chief of staff'])

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await ui.find({ type: 'Text', text: /^NEEDS YOU$/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /^PROJECTS$/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /1 idle: boopmark/ })).toBeDefined()
  // The selected card alone draws its keys, so d and a are never two hotkeys.
  expect((await ui.findAll({ type: 'Button' })).filter((b) => b.props.hotkey === 'd').length).toBe(1)
  expect((await ui.find({ type: 'Button', key: 'done-0' }))?.props.hotkey).toBe('d')
  expect((await ui.find({ type: 'Button', key: 'ask-0' }))?.props.hotkey).toBe('a')
  // Its whole card sits on the selection background: title, meta, keys.
  expect(await painted(ui)).toEqual(['line-1', 'line-2', 'line-3'])

  await ui.press({ key: 'done-0' })
  expect(prompts).toHaveLength(1)
  expect(prompts[0]).toContain(
    'Chris marked done: Create Hetzner bucket borant-hub-backup + 1Password item (personal-borant/01M5ZG00000000000000000015). ',
  )
  // The prompt says how: the hub lets only the owner close a plain task as done.
  expect(prompts[0]).toContain('send the owner of personal-borant a direct message')
  await ui.unmount()
})

test('a digit at an empty prompt opens the pane with the keyboard, on that ticker entry', async ($, on) => {
  const opens: { title?: string; focus?: boolean; columns?: number; rows?: number }[] = []
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opens.push({ title: e.title, focus: e.focus, columns: e.columns, rows: e.rows })
    return { value: { isPlaced: true } }
  })
  on('ui.toast', () => ({ value: undefined }))
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  // The editor beneath the mod: the key goes in.
  on('prompt.edit', ($, e) => ({ text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end), cursor: e.start + e.inputText.length }))

  await $.session.start({ ...SESSION })
  // The digit is the mod's: the prompt stays empty.
  expect(await $.prompt.edit(typed('', '1'))).toEqual({ text: '', cursor: 0 })
  await clock.advance(0)
  expect(opens).toEqual([{ title: 'chief of staff', focus: true, columns: 88, rows: 18 }])
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await selected(ui, 'row-0')).toBe(true)

  // 3 is the second project, notes: unfolded, and selected.
  await $.prompt.edit(typed('', '3'))
  await clock.advance(0)
  expect(opens).toHaveLength(2)
  expect(await selected(ui, 'row-3')).toBe(true)
  expect(await ui.find({ type: 'Text', text: /plain tasks: \d+ open/ })).toBeDefined()

  // A digit with no entry, and one after text, are typing.
  expect((await $.prompt.edit(typed('', '9'))).text).toBe('9')
  expect((await $.prompt.edit(typed('a', '1'))).text).toBe('a1')
  await clock.advance(0)
  expect(opens).toHaveLength(2)
  await ui.unmount()
})

test('below 144 columns a ticker press opens the pane, then hands it the keyboard', async ($, on) => {
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

  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'tick-3' })
  expect(opens).toEqual([{ title: 'chief of staff', focus: true }])
  // The band draws its entries as text for the handoff, and the pane asks
  // again; once its drawing says it has the keys, it stops asking.
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  for (let i = 0; i < 6; i += 1) await clock.advance(100)
  expect(opens).toHaveLength(2)
  expect(await selected(pane, 'row-3')).toBe(true)
  for (let i = 0; i < 6; i += 1) await clock.advance(100)
  expect(await band.find({ type: 'Button', key: 'tick-3' })).toBeDefined()
  await pane.unmount()
  await band.unmount()
})

test('Ask orchestrator sends to the channel owner itself, and the chief is only told', async ($, on) => {
  const prompts: { text: string; context?: readonly string[] }[] = []
  const sends: string[][] = []
  const toasts: string[] = []
  chiefStubs(on, [], [], [], toasts)
  const session = mock.session(on)
  on('process.run', ($, e) => {
    sends.push((e.argv || []) as string[])
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('prompt.submit', ($, e) => {
    prompts.push({ text: e.text, context: e.context })
    return { text: e.text }
  })

  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'nav-down' })
  await ui.press({ key: 'ask-1' })
  // One send to the owner of the question's channel, and no prompt.
  expect(sends).toHaveLength(1)
  expect(sends[0].slice(0, 3)).toEqual(['borant', 'send', 'orchestrator'])
  expect(sends[0][3]).toContain('Revoke old willisrocks gh token? (personal-borant/01M5ZG00000000000000000016)')
  expect(sends[0][3]).toContain('Answer the chief')
  expect(prompts).toHaveLength(0)
  expect(toasts).toContain('asked orchestrator: Revoke old willisrocks gh token?')
  // The status takes the Ask button's place; Done stays.
  expect(await ui.find({ type: 'Text', text: /asked orchestrator/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'ask-1' })).toBeUndefined()
  expect(await ui.find({ type: 'Button', key: 'done-1' })).toBeDefined()

  // The chief is told with a row it reads, not a prompt that starts a turn.
  const rows = session.appended().map((r) => JSON.stringify(r.message))
  expect(rows.filter((r) => r.includes('Do not answer it yourself'))).toHaveLength(1)
  await ui.unmount()
})

test('the help opens from the ticker button and /cos help', async ($, on) => {
  const opened: string[] = []
  chiefStubs(on, opened)
  await $.session.start({ ...SESSION })

  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'help' })
  expect(opened).toEqual(['borant › help'])
  await band.unmount()

  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  for (const text of [/^ {2}1-3 /, /^ {2}j k /, /^ {2}d /, /^ {2}a /, /^ {2}enter /, /^ {2}h l /, /^ {2}esc /, /\/cos pane/]) {
    expect(await pane.find({ type: 'Text', text })).toBeDefined()
  }
  await pane.unmount()

  // `/cos help` prints the same text, and starts no turn.
  const answer = await $.command.run({ command: 'cos', args: 'help' })
  expect(answer.text).toContain('open the pane with the keyboard, on that ticker entry')
  expect(answer.text).toContain('/cos help')
})

test('j, k, and a focus move select the row across both sections', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  for (const [key, hotkey] of [['nav-down', 'j'], ['nav-up', 'k'], ['nav-fold', 'h'], ['nav-open', 'l']]) {
    expect((await ui.find({ key }))?.props.hotkey).toBe(hotkey)
  }
  expect(await selected(ui, 'row-0')).toBe(true)
  // Past the two cards, j goes on into the projects.
  await ui.press({ key: 'nav-down' })
  await ui.press({ key: 'nav-down' })
  expect(await selected(ui, 'row-2')).toBe(true)
  await ui.press({ key: 'nav-up' })
  expect(await selected(ui, 'row-1')).toBe(true)

  // When the engine moves the ring (Tab, a click), the selection follows.
  await $.ui.focus({ component: 'Pane', requestId: 'cos', plugin: 'borant', element: 'row-3', origin: { kind: 'person' } })
  expect(await selected(ui, 'row-3')).toBe(true)
  await ui.unmount()
})

test('Enter and l unfold a project, h folds it, also from an epic under it', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  // row-2 is borant, folded.
  await ui.press({ key: 'nav-down' })
  await ui.press({ key: 'nav-down' })
  expect(await ui.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeUndefined()
  await ui.press({ key: 'nav-open' })
  expect(await ui.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /2 done/ })).toBeDefined()
  await ui.press({ key: 'nav-fold' })
  expect(await ui.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeUndefined()
  // Enter (a press on the selected row) unfolds it too; h on an epic folds
  // its project and selects it.
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'nav-down' })
  expect(await selected(ui, 'row-3')).toBe(true)
  await ui.press({ key: 'nav-fold' })
  expect(await ui.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeUndefined()
  expect(await selected(ui, 'row-2')).toBe(true)
  await ui.unmount()
})

test('Enter walks a card to its task, an epic to its tasks, and esc walks back to close', async ($, on) => {
  const opened: string[] = []
  const closes: string[] = []
  chiefStubs(on, opened, closes)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  // Enter on the selected card opens its task, with the breadcrumb.
  await ui.press({ key: 'row-0' })
  expect(opened).toContain('borant › borant › Create Hetzner bucket borant-hub-backup + 1Password item'.replace('borant › borant', 'borant'))
  await ui.press({ key: 'nav-back' })

  // Unfold borant, then Enter on its epic Delete borant-2 opens its tasks.
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-4' })
  await ui.press({ key: 'row-4' })
  expect(opened).toContain('borant › Elastic compute › Delete borant-2')
  expect(await ui.find({ type: 'Text', text: /The retire of borant-2/ })).toBeDefined()
  // Enter on a child task opens its detail.
  await ui.press({ key: 'row-0' })
  expect(await ui.find({ type: 'Text', text: /unclaimed/ })).toBeDefined()

  // esc goes back one level at a time, then closes the pane at the root.
  await ui.press({ key: 'nav-back' })
  expect(await ui.find({ type: 'Text', text: /unclaimed/ })).toBeUndefined()
  await ui.press({ key: 'nav-back' })
  expect(closes).toEqual([])
  await ui.press({ key: 'nav-back' })
  expect(closes).toEqual(['cos'])
  await ui.unmount()
})

test('a click selects a row, and a second click opens it', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await $.ui.focus({ component: 'Pane', requestId: 'cos', plugin: 'borant', element: 'row-3', origin: { kind: 'person' } })
  await ui.press({ key: 'row-3' })
  expect(await selected(ui, 'row-3')).toBe(true)
  expect(await ui.find({ type: 'Text', text: /plain tasks: \d+ open/ })).toBeUndefined()
  await ui.press({ key: 'row-3' })
  expect(await ui.find({ type: 'Text', text: /plain tasks: \d+ open/ })).toBeDefined()
  await ui.unmount()
})

test('the task detail shows the owner, the blockers, the PR link, and the last update', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })

  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  // borant › Elastic compute › Hub backup › The backup job
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-3' })
  await ui.press({ key: 'row-3' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  expect(await ui.find({ type: 'Text', text: /updated\s+\d{4}-\d{2}-\d{2}/ })).toBeDefined()
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
  // borant's first epic, whose channel has no task list.
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-2' })
  await ui.press({ key: 'row-3' })
  await ui.press({ key: 'row-3' })
  expect(await ui.find({ type: 'Text', text: /tasks unavailable/ })).toBeDefined()
  expect(toasts).toEqual([])
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
  // The root reads no channel; the first card's task reads its own.
  expect(reads).toEqual([])
  await ui.press({ key: 'row-0' })
  expect(reads).toEqual(['personal-borant'])
  await ui.press({ key: 'nav-back' })
  await ui.press({ key: 'row-1' })
  await ui.press({ key: 'row-1' })
  expect(reads).toEqual(['personal-borant'])

  // The 5 s tick reads the open view's channel again.
  await clock.advance(5000)
  expect(reads).toEqual(['personal-borant', 'personal-borant'])

  // Back at the root, and closed, the tick reads nothing more.
  await ui.press({ key: 'nav-back' })
  await ui.press({ key: 'nav-back' })
  await clock.advance(5000)
  expect(reads).toEqual(['personal-borant', 'personal-borant'])
  await ui.unmount()
})

test('Done shows each step: the chief is closing it, closed, then the card goes', async ($, on) => {
  const prompts: string[] = []
  const toasts: string[] = []
  let live = JSON.parse(JSON.stringify(board))
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
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
  on('ui.close', () => ({ value: undefined }))
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) return { value: { exitCode: 0, stdout: '[]', stderr: '' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(live), stderr: '' } }
  })

  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'done-0' })
  expect(prompts).toHaveLength(1)
  // The prompt landed: the chief has it, and the hub has not closed the task.
  // The status takes the keys' place, so there is no Done to press twice.
  expect(await ui.find({ type: 'Text', text: /the chief is closing it/ })).toBeDefined()
  expect(await ui.find({ type: 'Button', key: 'done-0' })).toBeUndefined()

  // The hub closes it: the next read shows the card as closed for a while.
  live = JSON.parse(JSON.stringify(live))
  for (const c of live.collections) {
    c.needs_you = (c.needs_you || []).filter((q: { task: string }) => q.task !== '01M5ZG00000000000000000015')
  }
  await clock.advance(5000)
  expect(await ui.find({ type: 'Text', text: /closed/ })).toBeDefined()
  expect(toasts).toContain('closed: Create Hetzner bucket borant-hub-backup + 1Password item')
  // The closed card takes no selection: the next card has it, and its keys.
  expect(await selected(ui, 'row-0')).toBe(true)
  expect(await ui.find({ type: 'Button', key: 'done-1' })).toBeDefined()
  expect(await painted(ui)).not.toContain('line-1')
  // Then it goes.
  await clock.advance(15000)
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /closing it|closed/ })).toBeUndefined()
  await ui.unmount()
})

test('a Done the hub has not closed after two minutes shows as still open', async ($, on) => {
  const clock = chiefStubs(on)
  on('prompt.submit', ($, e) => ({ text: e.text }))
  await $.session.start({ ...SESSION })
  await $.command.run({ command: 'cos', args: 'pane' })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'done-0' })
  expect(await ui.find({ type: 'Text', text: /the chief is closing it/ })).toBeDefined()
  await clock.advance(120000)
  expect(await ui.find({ type: 'Text', text: /still open after 2m/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeDefined()
  expect((await ui.find({ type: 'Button', key: 'done-0' }))?.props.label).toBe('Done again')
  await ui.unmount()
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
  // The stream starts from a timer: a spawn started in a hook ends with it.
  await clock.advance(0)
  expect(spawned).toEqual(['borant', 'events', '--follow', '--json'])
  const first = reads
  await clock.advance(300)
  expect(reads).toBe(first + 1)

  // The ticker ends with how fresh the board is: the stream ended, so polling.
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /^polling/ })).toBeDefined()
  await band.unmount()
})
