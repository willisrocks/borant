// The mod in a session: the role gate, the band, the digest, the pane's
// navigation and buttons, and a board or task source that fails.
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
function chiefStubs(kit: unknown, opened: string[] = [], closes: string[] = [], focused: string[] = [], toasts: string[] = [], mode = 'fixture') {
  // The kit hands test files its own typed `on`, which only registers stubs.
  const on = kit as (name: string, hook: (...args: unknown[]) => unknown) => void

  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: mode })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opened.push(e.title + (e.focus ? ' focus' : ''))
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
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
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
  expect(await ui.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /2 need you/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /\u25a02/ })).toBeDefined()
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
  expect(await ui.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeDefined()
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
  expect(await empty.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeUndefined()
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

// The band's keys for the fixture's rows.
const GATE = 'q:personal-borant/01M5ZG00000000000000000015'
const BORANT = 'p:borant'

type Mounted = Awaited<ReturnType<Parameters<Parameters<typeof test>[1]>[0]['ui']['mount']>>
type Engine = Parameters<Parameters<typeof test>[1]>[0]

/** The board-mode key field the pane draws now: drawn afresh after each key. */
async function field(pane: Mounted) {
  const input = await pane.find({ type: 'Input' })
  if (!input) throw new Error('the pane draws no key field')
  return input
}

/** A key typed in board mode: the key field's text after it. */
async function typeKey($: Engine, pane: Mounted, key: string) {
  const input = await field(pane)
  await $.ui.input({ plugin: 'borant', key: String(input.key ?? input.props.key), text: key, kind: 'change' })
}

/** Enter in board mode: the key field's submit. */
async function enter($: Engine, pane: Mounted) {
  const input = await field(pane)
  await $.ui.input({ plugin: 'borant', key: String(input.key ?? input.props.key), text: '', kind: 'submit' })
}

/** The prompt Claude Code starts a plugin's prompt with, as 2.1.316 does. */
const PLUGIN_PROMPT = 'The borant plugin sent a message:\n'
const PLUGIN_NOTE = '\n\nA plugin submits between turns. Address the message above.'

test('a digit selects its row and enters board mode: the key field is the pane\'s only focusable element', async ($, on) => {
  const opens: { title?: string; focus?: boolean; closeOnEscape?: boolean }[] = []
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opens.push({ title: e.title, focus: e.focus, closeOnEscape: e.closeOnEscape })
    return { value: { isPlaced: true } }
  })
  on('ui.toast', () => ({ value: undefined }))
  on('ui.focus', () => ({ value: {} }))
  on('ui.scroll', () => ({ value: {} }))
  on('ui.close', () => ({ value: undefined }))

  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await band.find({ type: 'Button', key: GATE }))?.props.hotkey).toBe('1')
  expect((await band.find({ type: 'Button', key: BORANT }))?.props.hotkey).toBe('3')
  // At rest the band names the key that enters board mode, and no row is filled.
  expect((await band.find({ type: 'Button', key: 'board' }))?.props.hotkey).toBe('0')
  expect((await band.find({ key: 'band-row-1' }))?.props.backgroundColor).toBeUndefined()
  await band.press({ key: BORANT })
  // One open, asking for the keys, and Esc gives them back and closes it.
  expect(opens).toEqual([{ title: 'chief of staff', focus: true, closeOnEscape: true }])

  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /project · 1 of 2/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /Delete borant-2/ })).toBeDefined()
  // The row's title in the preview draws on the band row's selection background.
  expect((await pane.find({ key: 'pane-line-2' }))?.props.backgroundColor).toBe('userMessageBackground')
  // The key field holds the focus from the start and is the only control: a
  // Button would let an arrow or Tab take the focus, and a letter the prompt.
  const input = await field(pane)
  expect(input.props.autoFocus).toBe(true)
  expect(input.props.label).toBe('board mode')
  expect(String(input.props.placeholder)).toContain('esc exit')
  expect(await pane.findAll({ type: 'Button' })).toHaveLength(0)
  await clock.advance(1)

  // In board mode the band fills the selected row and says how to leave.
  expect((await band.find({ key: 'band-row-3' }))?.props.backgroundColor).toBe('userMessageBackground')
  expect((await band.find({ key: 'band-row-1' }))?.props.backgroundColor).toBeUndefined()
  expect(await band.find({ type: 'Text', text: /^board mode$/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /^ exit$/ })).toBeDefined()
  expect(await band.find({ type: 'Button', key: 'board' })).toBeUndefined()
  await pane.unmount()
  await band.unmount()
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

test('from 144 columns a digit at an empty prompt opens the preview with the keys; below, the chips carry the digits', async ($, on) => {
  const opens: { focus?: boolean }[] = []
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opens.push({ focus: e.focus })
    return { value: { isPlaced: true } }
  })
  on('ui.focus', () => ({ value: {} }))
  on('ui.close', () => ({ value: undefined }))
  // The engine's own edit: the key goes into the box.
  on('prompt.edit', ($, e) => ({ text: e.text.slice(0, e.start) + e.inputText + e.text.slice(e.end), cursor: e.start + e.inputText.length }))
  await $.session.start({ ...SESSION })

  // 200 columns: the chips draw their digits but carry no hotkey, so a bare
  // digit reaches prompt.edit, which takes it and opens the preview.
  const wide = await $.ui.mount({ ...BAND, viewport: { ...BAND.viewport, columns: 200 }, props: { ...BAND.props, bodyColumns: 200 }, surface: 'terminal' })
  expect((await wide.find({ type: 'Button', key: BORANT }))?.props.hotkey).toBeUndefined()
  expect(await $.prompt.edit(typed('', '3'))).toEqual({ text: '', cursor: 0 })
  expect(opens).toEqual([{ focus: true }])
  // A digit with no row, a digit after text, and a letter are typing.
  expect((await $.prompt.edit(typed('', '9'))).text).toBe('9')
  expect((await $.prompt.edit(typed('a', '1'))).text).toBe('a1')
  expect((await $.prompt.edit(typed('', 'x'))).text).toBe('x')
  expect(opens).toHaveLength(1)
  await wide.unmount()

  // 120 columns: an unasked pane is not placed there, so the chips keep the
  // digits as hotkeys, and the engine presses the chip instead.
  const narrow = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect((await narrow.find({ type: 'Button', key: BORANT }))?.props.hotkey).toBe('3')
  expect((await $.prompt.edit(typed('', '3'))).text).toBe('3')
  await narrow.unmount()
})

test('in board mode every key goes to the board: digits, j k, d, a, and keys it does not use', async ($, on) => {
  const prompts: string[] = []
  const sends: string[][] = []
  const toasts: string[] = []
  const clock = chiefStubs(on, [], [], [], toasts)
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  on('process.run', ($, e) => {
    sends.push((e.argv || []) as string[])
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'board' })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /needs you · 1 of 2/ })).toBeDefined()
  // d and a are named for a needs-you item.
  expect(String((await field(pane)).props.placeholder)).toMatch(/(^| )d( |$)/)
  await clock.advance(1)

  await typeKey($, pane, 'j')
  expect(await pane.find({ type: 'Text', text: /needs you · 2 of 2/ })).toBeDefined()
  expect((await band.find({ key: 'band-row-2' }))?.props.backgroundColor).toBe('userMessageBackground')
  // The field is drawn afresh, empty, after each key.
  expect((await field(pane)).props.value).toBe('')
  await typeKey($, pane, 'a')
  expect(sends.filter((a) => a[1] === 'send')).toHaveLength(1)
  expect(sends.find((a) => a[1] === 'send')?.[3]).toContain('Revoke old willisrocks gh token?')
  expect(await pane.find({ type: 'Text', text: /asked orchestrator/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /^✓ asked/ })).toBeDefined()

  // A digit selects its row; k wraps from the first row to the last.
  await typeKey($, pane, '1')
  expect(await pane.find({ type: 'Text', text: /needs you · 1 of 2/ })).toBeDefined()
  await typeKey($, pane, 'k')
  expect(await pane.find({ type: 'Text', text: /project · 2 of 2/ })).toBeDefined()
  // A project has no Done: d says so, and nothing is sent.
  expect(String((await field(pane)).props.placeholder)).not.toMatch(/(^| )d( |$)/)
  await typeKey($, pane, 'd')
  expect(prompts).toHaveLength(0)
  expect(toasts.some((t) => t.startsWith('d: Done needs'))).toBe(true)
  // A key the board does not use does nothing, and a digit with no row neither.
  await typeKey($, pane, 'x')
  await typeKey($, pane, '9')
  expect(await pane.find({ type: 'Text', text: /project · 2 of 2/ })).toBeDefined()
  // Two keys that land in one field before it is drawn again both count, in order.
  await typeKey($, pane, 'jd')
  expect(prompts).toHaveLength(1)
  expect(prompts[0]).toContain('Chris marked done: Create Hetzner bucket borant-hub-backup + 1Password item (personal-borant/01M5ZG00000000000000000015). ')
  expect(prompts[0]).toContain('send the owner of personal-borant a direct message')
  await pane.unmount()
  await band.unmount()
})

test('Enter shows details, j and k keep them open, and Enter goes back', async ($, on) => {
  const reads: string[] = []
  const clock = chiefStubs(on)
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) reads.push(argv[2])
    return { value: { exitCode: 0, stdout: '[]', stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /blockers/ })).toBeUndefined()
  expect((await field(pane)).props.submitLabel).toBe('details')
  await enter($, pane)
  expect(await pane.find({ type: 'Text', text: /blockers/ })).toBeDefined()
  expect((await field(pane)).props.submitLabel).toBe('back')

  // j and k move with the details open: the next row shows its details.
  await typeKey($, pane, 'j')
  await typeKey($, pane, 'j')
  expect(await pane.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /no child tasks|tasks unavailable|●|○/ })).toBeDefined()
  expect((await field(pane)).props.submitLabel).toBe('back')
  await enter($, pane)
  expect((await field(pane)).props.submitLabel).toBe('details')

  // The fixture source answers the tasks itself, and the tick reads nothing
  // with no channel open.
  await clock.advance(5000)
  expect(reads).toEqual([])
  await pane.unmount()
  await band.unmount()
})

test('Tab cannot move the ring off the key field, and a pane without the keys says board mode is off', async ($, on) => {
  const opens: string[] = []
  const clock = chiefStubs(on, opens)
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'board' })
  const on1 = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await clock.advance(1)
  expect(await band.find({ type: 'Text', text: /^board mode$/ })).toBeDefined()
  await on1.unmount()
  // Tab and Shift+Tab would put the ring on the pane's close mark, where a
  // letter goes to the prompt: the mod keeps it on the field.
  const tab = await $.ui.focus({ component: 'Pane', requestId: 'cos', origin: { kind: 'person' } })
  expect(tab.deny).toContain('board mode keeps the keys')
  // The pane can still lose the keys (ctrl+x tab into the band, a click on
  // the transcript): the engine draws it unfocused, and both sites say so.
  const off = await $.ui.mount({ ...PANE, props: { ...PANE.props, isFocused: false }, surface: 'terminal' })
  await clock.advance(1)
  expect((await field(off)).props.label).toBe('board mode off')
  expect(String((await field(off)).props.placeholder)).toContain('0 at an empty prompt')
  expect((await band.find({ type: 'Button', key: 'board' }))?.props.hotkey).toBe('0')
  expect((await band.find({ key: 'band-row-1' }))?.props.backgroundColor).toBeUndefined()
  // 0 asks for the keys again.
  await band.press({ key: 'board' })
  expect(opens.at(-1)).toBe('chief of staff focus')
  await off.unmount()
  await band.unmount()
})

test('an inline preview closes once the keys leave it; a docked one stays', async ($, on) => {
  const closes: string[] = []
  const clock = chiefStubs(on, [], closes)
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const docked = await $.ui.mount({ ...PANE, props: { ...PANE.props, isFocused: false }, surface: 'terminal' })
  await clock.advance(1000)
  expect(closes).toEqual([])
  await docked.unmount()

  const inline = await $.ui.mount({ ...PANE, props: { ...PANE.props, placement: 'inline', isFocused: true }, surface: 'terminal' })
  // Inline the card is compact: the key field, then the title.
  expect(await inline.find({ type: 'Text', text: /needs you · 1 of 2/ })).toBeUndefined()
  expect(await inline.find({ type: 'Input' })).toBeDefined()
  await inline.unmount()
  const idle = await $.ui.mount({ ...PANE, props: { ...PANE.props, placement: 'inline', isFocused: false }, surface: 'terminal' })
  await clock.advance(1000)
  expect(closes[0]).toBe('cos')
  await idle.unmount()
  await band.unmount()
})

test('? shows the help in board mode, and /cos help prints it', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'board' })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await typeKey($, pane, '?')
  expect(await pane.find({ type: 'Text', text: /enter board mode, from/ })).toBeDefined()
  expect((await field(pane)).props.submitLabel).toBe('back')
  await typeKey($, pane, '?')
  expect(await pane.find({ type: 'Text', text: /enter board mode, from/ })).toBeUndefined()
  await typeKey($, pane, '?')
  await enter($, pane)
  expect(await pane.find({ type: 'Text', text: /enter board mode, from/ })).toBeUndefined()
  await pane.unmount()
  await band.unmount()
  const answer = await $.command.run({ command: 'cos', args: 'help' })
  expect(answer.text).toContain('j k')
  expect(answer.text).toContain('esc')
  expect(answer.text).toContain('/cos help')
})

test('Done shows each step: the chief is closing it, closed, then the row goes', async ($, on) => {
  const prompts: string[] = []
  const toasts: string[] = []
  let live = JSON.parse(JSON.stringify(board))
  // The task's status as `borant tasks` lists it.
  let status = 'needs_human'
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
    if (argv.includes('tasks')) return { value: { exitCode: 0, stdout: JSON.stringify([{ id: '01M5ZG00000000000000000015', status }]), stderr: '' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(live), stderr: '' } }
  })

  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await clock.advance(1)
  // d d, the second before the first is drawn: one prompt.
  const input = await field(ui)
  await $.ui.input({ plugin: 'borant', key: String(input.key ?? input.props.key), text: 'dd', kind: 'change' })
  expect(prompts).toHaveLength(1)
  expect(await ui.find({ type: 'Text', text: /sending to the chief/ })).toBeDefined()
  // The chief's turn with the prompt starts, after the line Claude Code puts
  // before a plugin's prompt: the hub has not closed the task.
  await $.turn.start({ text: PLUGIN_PROMPT + prompts[0] + PLUGIN_NOTE, turnId: 't1' })
  expect(await ui.find({ type: 'Text', text: /the chief is closing it, since \d\d:\d\d/ })).toBeDefined()
  // While it is under way, d is not offered.
  expect(String((await field(ui)).props.placeholder)).not.toMatch(/(^| )d( |$)/)
  // The band row has the short form, in its fixed tail, and it does not count.
  expect(await band.find({ type: 'Text', text: /^◷ closing *$/ })).toBeDefined()

  // The hub closes it: the next read shows the row as closed for a while.
  status = 'closed'
  live = JSON.parse(JSON.stringify(live))
  for (const c of live.collections) {
    c.needs_you = (c.needs_you || []).filter((q: { task: string }) => q.task !== '01M5ZG00000000000000000015')
  }
  await clock.advance(5000)
  expect(await ui.find({ type: 'Text', text: /closed/ })).toBeDefined()
  expect(toasts).toContain('closed: Create Hetzner bucket borant-hub-backup + 1Password item')
  // Board mode holds its rows still: the closed row keeps its place, closed.
  await clock.advance(15000)
  expect(await band.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /^✓ closed *$/ })).toBeDefined()
  // Once board mode ends it goes.
  await ui.unmount()
  const off = await $.ui.mount({ ...PANE, props: { ...PANE.props, isFocused: false }, surface: 'terminal' })
  await clock.advance(1)
  expect(await band.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeUndefined()
  await off.unmount()
  await band.unmount()
})

test('a Done the hub has not closed after two minutes shows as still open, and d offers it again', async ($, on) => {
  const prompts: string[] = []
  const clock = chiefStubs(on)
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await typeKey($, ui, 'd')
  await $.turn.start({ text: PLUGIN_PROMPT + prompts[0] + PLUGIN_NOTE, turnId: 't1' })
  expect(await ui.find({ type: 'Text', text: /the chief is closing it/ })).toBeDefined()
  await clock.advance(120000)
  expect(await ui.find({ type: 'Text', text: /still open after 2m/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeDefined()
  expect(String((await field(ui)).props.placeholder)).toMatch(/(^| )d( |$)/)
  await typeKey($, ui, 'd')
  expect(prompts).toHaveLength(2)
  await ui.unmount()
  await band.unmount()
})

test('the event stream starts from a timer, reads the board at once on an event, and starts again when it ends', async ($, on) => {
  let reads = 0
  let hubDown = false
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'cli' })
  const clock = mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('process.run', () => {
    reads += 1
    if (hubDown) return { value: { exitCode: 1, stdout: '', stderr: 'hub unreachable' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } }
  })
  const spawns: string[][] = []
  on('process.spawn', async function* ($, e) {
    spawns.push((e.argv || []) as string[])
    yield { stream: 'stdout', text: '{"kind":"host.registered"}\n{"kind":"task.st' }
    yield { stream: 'stdout', text: 'atus","channel":"personal-borant"}\n' }
    return { value: { code: 0 } }
  })

  await $.session.start({ ...SESSION })
  // A spawn lives as long as the hook that starts it, so a timer starts it.
  expect(spawns).toHaveLength(0)
  await clock.advance(0)
  expect(spawns).toEqual([['borant', 'events', '--follow', '--json']])
  const first = reads
  await clock.advance(300)
  expect(reads).toBe(first + 1)

  // A read and the stream's state show nothing: nothing on the band moves.
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /reading|live|polling/ })).toBeUndefined()
  await band.unmount()
  // The stream ended: it starts again in 5 s.
  await clock.advance(5000)
  expect(spawns).toHaveLength(2)
  // The hub stops answering: the band says the board is unavailable.
  hubDown = true
  await clock.advance(5000)
  const later = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await later.find({ type: 'Text', text: /board unavailable/ })).toBeDefined()
  await later.unmount()
})

test('in the turn a Done started, a call that would ask the person is refused, and only in that turn', async ($, on) => {
  const prompts: string[] = []
  chiefStubs(on)
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  on('tool.check', () => ({ decision: 'ask' }))
  on('turn.complete', () => ({ text: '' }))
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })

  expect((await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })).decision).toBe('ask')
  await typeKey($, pane, 'd')
  // The Done is queued behind the person's own turn: that turn's asks are theirs.
  await $.turn.start({ text: 'what is left today?', turnId: 't0' })
  expect((await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })).decision).toBe('ask')
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: false, turnId: 't0', reason: 'answer' })
  // The Done's own turn starts, after Claude Code's line for a plugin's prompt.
  await $.turn.start({ text: PLUGIN_PROMPT + prompts[0] + PLUGIN_NOTE, turnId: 't1' })
  const held = await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })
  expect(held.decision).toBe('deny')
  expect(held.reason).toContain('borant MCP tools')
  // The turn ends: the person is back, and Claude Code decides as before.
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: false, turnId: 't1', reason: 'answer' })
  expect((await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })).decision).toBe('ask')
  await pane.unmount()
  await band.unmount()
})

test('a failed Ask can be sent again with a, and a a sends once', async ($, on) => {
  const sends: string[][] = []
  const toasts: string[] = []
  let fail = true
  let live = JSON.parse(JSON.stringify(board))
  chiefStubs(on, [], [], [], toasts, 'cli')
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv[1] === 'send') {
      sends.push(argv)
      return { value: { exitCode: fail ? 1 : 0, stdout: '', stderr: fail ? 'hub unreachable' : '' } }
    }
    if (argv.includes('tasks')) return { value: { exitCode: 0, stdout: '[]', stderr: '' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(live), stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })

  await typeKey($, pane, 'a')
  expect(sends).toHaveLength(1)
  expect(await pane.find({ type: 'Text', text: /ask failed/ })).toBeDefined()
  expect(String((await field(pane)).props.placeholder)).toMatch(/(^| )a( |$)/)
  // Again, and this time it lands; a a in one field sends once.
  fail = false
  const input = await field(pane)
  await $.ui.input({ plugin: 'borant', key: String(input.key ?? input.props.key), text: 'aa', kind: 'change' })
  expect(sends).toHaveLength(2)
  expect(String((await field(pane)).props.placeholder)).not.toMatch(/(^| )a( |$)/)
  await pane.unmount()
  await band.unmount()
})

test('when the selected item leaves the board, the row in its place takes no Done or Ask until selected', async ($, on) => {
  const prompts: string[] = []
  const toasts: string[] = []
  let live = JSON.parse(JSON.stringify(board))
  const clock = chiefStubs(on, [], [], [], toasts, 'cli')
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
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(String((await field(pane)).props.placeholder)).toMatch(/(^| )d( |$)/)

  // Someone else closes it. Board mode holds the row in place, marked, and d
  // does nothing on it; no other row moves into its place.
  live = JSON.parse(JSON.stringify(live))
  for (const c of live.collections) c.needs_you = (c.needs_you || []).filter((q: { task: string }) => q.task !== '01M5ZG00000000000000000015')
  await clock.advance(5000)
  expect(toasts).toContain('the selected item left the board')
  expect(await band.find({ type: 'Text', text: /^left the board *$/ })).toBeDefined()
  expect(String((await field(pane)).props.placeholder)).not.toMatch(/(^| )d( |$)/)
  await typeKey($, pane, 'd')
  expect(prompts).toHaveLength(0)
  // The next row still takes d.
  await typeKey($, pane, 'j')
  expect(String((await field(pane)).props.placeholder)).toMatch(/(^| )d( |$)/)
  // At rest the row has gone, and a row that slides into the selection's
  // place takes no Done until the person selects it.
  await pane.unmount()
  const off = await $.ui.mount({ ...PANE, props: { ...PANE.props, isFocused: false }, surface: 'terminal' })
  await clock.advance(1)
  expect(await band.find({ type: 'Text', text: /^left the board *$/ })).toBeUndefined()
  await off.unmount()
  await band.unmount()
})

test('a narrow docked preview draws the compact block and names its keys by letter', async ($, on) => {
  const opens: { columns?: number }[] = []
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('ui.render', () => ENGINE_ROW)
  on('ui.open', ($, e) => {
    opens.push({ columns: e.columns })
    return { value: { isPlaced: true } }
  })
  on('ui.focus', () => ({ value: {} }))
  on('ui.close', () => ({ value: undefined }))
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  // The preview asks for a width that fits a card, at most 45% of the
  // terminal (here 120 columns); a width the person kept wins.
  expect(opens[0].columns).toBe(54)
  const pane = await $.ui.mount({ ...PANE, props: { ...PANE.props, bodyColumns: 26 }, surface: 'terminal' })
  expect((await field(pane)).props.placeholder).toBe('1-9 j k d a ? · esc exit')
  // The compact block: no field labels, the title first.
  expect(await pane.find({ type: 'Text', text: /^ {2}owner/ })).toBeUndefined()
  await pane.unmount()
  await band.unmount()
})

test('an item that leaves needs-you without closing says so, not "closed"', async ($, on) => {
  const prompts: string[] = []
  const toasts: string[] = []
  let live = JSON.parse(JSON.stringify(board))
  const clock = chiefStubs(on, [], [], [], toasts, 'cli')
  on('prompt.submit', ($, e) => {
    prompts.push(e.text)
    return { text: e.text }
  })
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) return { value: { exitCode: 0, stdout: JSON.stringify([{ id: '01M5ZG00000000000000000015', status: 'in_progress' }]), stderr: '' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(live), stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await typeKey($, ui, 'd')
  await $.turn.start({ text: PLUGIN_PROMPT + prompts[0] + PLUGIN_NOTE, turnId: 't1' })
  live = JSON.parse(JSON.stringify(live))
  for (const c of live.collections) c.needs_you = (c.needs_you || []).filter((q: { task: string }) => q.task !== '01M5ZG00000000000000000015')
  await clock.advance(5000)
  expect(await ui.find({ type: 'Text', text: /no longer needs you, but not closed: in progress/ })).toBeDefined()
  expect(await band.find({ type: 'Text', text: /^→ not closed/ })).toBeDefined()
  expect(toasts).toContain('no longer needs you: Create Hetzner bucket borant-hub-backup + 1Password item')
  expect(toasts.some((t) => t.startsWith('closed:'))).toBe(false)
  await ui.unmount()
  await band.unmount()
})

test('a slow Ask never holds up the keys after it', async ($, on) => {
  let release: () => void = () => {}
  chiefStubs(on)
  on('process.run', async () => {
    await new Promise<void>((done) => (release = done))
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  // a, then j in one burst: j moves while the Ask is still sending.
  const input = await field(pane)
  const typing = $.ui.input({ plugin: 'borant', key: String(input.key ?? input.props.key), text: 'aj', kind: 'change' })
  await new Promise((r) => setTimeout(r, 20))
  expect(await pane.find({ type: 'Text', text: /needs you · 2 of 2/ })).toBeDefined()
  release()
  await typing
  await pane.unmount()
  await band.unmount()
})

/** A board, as `borant board --json` prints it, with one more needs-you item. */
function withQuestion(base: typeof board, title: string) {
  const next = JSON.parse(JSON.stringify(base))
  const c = next.collections[0]
  c.needs_you.push({ ...c.needs_you[0], task: '01M5ZG00000000000000099999', title })
  return next
}

test('a slow details read never holds up the keys', async ($, on) => {
  let release: () => void = () => {}
  chiefStubs(on, [], [], [], [], 'cli')
  on('process.run', async ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) await new Promise<void>((done) => (release = done))
    return { value: { exitCode: 0, stdout: argv.includes('tasks') ? '[]' : JSON.stringify(board), stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  // Enter starts a details read that waits; j moves at once.
  await enter($, pane)
  await typeKey($, pane, 'j')
  expect(await pane.find({ type: 'Text', text: /needs you · 2 of 2/ })).toBeDefined()
  release()
  await pane.unmount()
  await band.unmount()
})

test('board mode holds its rows still: a new item waits until esc', async ($, on) => {
  let live = JSON.parse(JSON.stringify(board))
  const clock = chiefStubs(on, [], [], [], [], 'cli')
  on('process.run', ($, e) => {
    const argv = (e.argv || []) as string[]
    return { value: { exitCode: 0, stdout: argv.includes('tasks') ? '[]' : JSON.stringify(live), stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  live = withQuestion(live, 'Arrived during board mode')
  await clock.advance(5000)
  expect(await band.find({ type: 'Text', text: /Arrived during board mode/ })).toBeUndefined()
  expect(await band.find({ type: 'Text', text: /\+1 after esc/ })).toBeDefined()
  await pane.unmount()
  const off = await $.ui.mount({ ...PANE, props: { ...PANE.props, isFocused: false }, surface: 'terminal' })
  await clock.advance(1)
  expect(await band.find({ type: 'Text', text: /Arrived during board mode/ })).toBeDefined()
  await off.unmount()
  await band.unmount()
})

test('a slow board read that ends after a newer one is dropped', async ($, on) => {
  let release: () => void = () => {}
  let reads = 0
  const clock = chiefStubs(on, [], [], [], [], 'cli')
  const newer = withQuestion(board, 'Only in the newer read')
  on('process.run', async ($, e) => {
    const argv = (e.argv || []) as string[]
    if (argv.includes('tasks')) return { value: { exitCode: 0, stdout: '[]', stderr: '' } }
    reads += 1
    // The second read is slow and old; the third is new.
    if (reads === 2) {
      await new Promise<void>((done) => (release = done))
      return { value: { exitCode: 0, stdout: JSON.stringify(board), stderr: '' } }
    }
    return { value: { exitCode: 0, stdout: JSON.stringify(reads === 1 ? board : newer), stderr: '' } }
  })
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await clock.advance(5000)
  await clock.advance(1100)
  await $.turn.start({ text: 'hi', turnId: 't1' })
  expect(await band.find({ type: 'Text', text: /Only in the newer read/ })).toBeDefined()
  release()
  await clock.advance(1)
  expect(await band.find({ type: 'Text', text: /Only in the newer read/ })).toBeDefined()
  await band.unmount()
})

