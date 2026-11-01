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

test('a digit selects its row and opens the preview with the keys', async ($, on) => {
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
  expect((await band.find({ type: 'Button', key: GATE }))?.props.hotkey).toBe('1')
  expect((await band.find({ type: 'Button', key: BORANT }))?.props.hotkey).toBe('3')
  await band.press({ key: BORANT })
  // One open, asking for the keys: a bare digit leaves them with the prompt,
  // which hands them to the pane; no second ask, no handoff.
  expect(opens).toEqual([{ title: 'chief of staff', focus: true }])

  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /project · 1 of 2/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /Delete borant-2/ })).toBeDefined()
  // The row's title in the preview draws on the band row's selection background.
  expect((await pane.find({ key: 'pane-line-2' }))?.props.backgroundColor).toBe('userMessageBackground')
  expect((await pane.find({ key: 'pane-line-0' }))?.props.backgroundColor).toBeUndefined()
  // The pane carries the board's keys: j, k, and Enter on its "more".
  expect((await pane.find({ type: 'Button', key: 'pane-down' }))?.props.hotkey).toBe('j')
  expect((await pane.find({ type: 'Button', key: 'pane-up' }))?.props.hotkey).toBe('k')
  expect((await pane.find({ type: 'Button', key: 'pane-more' }))?.props.autoFocus).toBe(true)
  // No tabs and no tree: the pane only shows the band's selection.
  expect(await pane.find({ type: 'Button', key: 'tab-needs' })).toBeUndefined()
  // The pane's drawing says it has the keys; the band follows on the next tick.
  await clock.advance(1)

  // While the pane has the keys, the band draws the selected row filled and names the keys.
  expect((await band.find({ key: 'band-row-3' }))?.props.backgroundColor).toBe('userMessageBackground')
  expect((await band.find({ key: 'band-row-1' }))?.props.backgroundColor).toBeUndefined()
  expect(await band.find({ type: 'Text', text: /^ prompt$/ })).toBeDefined()
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

test('j and k move the band selection, the preview follows, and d and a act on it', async ($, on) => {
  const prompts: string[] = []
  const sends: string[][] = []
  const clock = chiefStubs(on)
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
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /needs you · 1 of 2/ })).toBeDefined()
  // Only the selected item has Done and Ask, in the pane and on its band row.
  expect((await pane.find({ type: 'Button', key: 'done' }))?.props.hotkey).toBe('d')
  expect((await pane.find({ type: 'Button', key: 'ask' }))?.props.hotkey).toBe('a')
  await clock.advance(1)
  expect(await band.findAll({ type: 'Button', key: 'done' })).toHaveLength(1)

  await pane.press({ key: 'pane-down' })
  expect(await pane.find({ type: 'Text', text: /needs you · 2 of 2/ })).toBeDefined()
  expect((await band.find({ key: 'band-row-2' }))?.props.backgroundColor).toBe('userMessageBackground')
  await pane.press({ key: 'ask' })
  expect(sends.filter((a) => a[1] === 'send')).toHaveLength(1)
  expect(sends.find((a) => a[1] === 'send')?.[3]).toContain('Revoke old willisrocks gh token?')
  expect(await pane.find({ type: 'Text', text: /asked orchestrator/ })).toBeDefined()

  // k wraps from the first row to the last.
  await pane.press({ key: 'pane-up' })
  await pane.press({ key: 'pane-up' })
  expect(await pane.find({ type: 'Text', text: /project · 2 of 2/ })).toBeDefined()
  // A project has no Done.
  expect(await pane.find({ type: 'Button', key: 'done' })).toBeUndefined()
  await pane.press({ key: 'pane-down' })
  await pane.press({ key: 'done' })
  expect(prompts).toHaveLength(1)
  expect(prompts[0]).toContain('Chris marked done: Create Hetzner bucket borant-hub-backup + 1Password item (personal-borant/01M5ZG00000000000000000015). ')
  expect(prompts[0]).toContain('send the owner of personal-borant a direct message')
  await pane.unmount()
  await band.unmount()
})

test('Enter shows more: the task of a needs-you item, the tasks of a project', async ($, on) => {
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
  await pane.press({ key: 'pane-more' })
  expect(await pane.find({ type: 'Text', text: /blockers/ })).toBeDefined()
  expect(await pane.find({ type: 'Text', text: /needs you/ })).toBeDefined()
  expect((await pane.find({ type: 'Button', key: 'pane-more' }))?.props.label).toBe('⏎ less')

  // Moving drops "more"; the selected row pressed again shows it.
  await pane.press({ key: 'pane-down' })
  await pane.press({ key: 'pane-down' })
  expect(await pane.find({ type: 'Text', text: /Hub backup to object storage/ })).toBeDefined()
  await band.press({ key: BORANT })
  expect(await pane.find({ type: 'Text', text: /no child tasks|tasks unavailable|●|○/ })).toBeDefined()
  expect((await pane.find({ type: 'Button', key: 'pane-more' }))?.props.label).toBe('⏎ less')

  // The fixture source answers the tasks itself, and the tick reads nothing
  // with no channel open.
  await pane.press({ key: 'pane-more' })
  await clock.advance(5000)
  expect(reads).toEqual([])
  await pane.unmount()
  await band.unmount()
})

test('the band holds the keys after ctrl+x tab, and lets them go', async ($, on) => {
  chiefStubs(on)
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  // At rest the hint names the digits and no row is filled.
  expect(await band.find({ type: 'Text', text: /1-4 select/ })).toBeDefined()
  expect((await band.find({ key: 'band-row-1' }))?.props.backgroundColor).toBeUndefined()
  // The ring starts on the selected row's digit.
  expect((await band.find({ type: 'Button', key: GATE }))?.props.autoFocus).toBe(true)

  await $.ui.focus({ component: 'AbovePrompt', requestId: 'above-prompt', plugin: 'borant', element: GATE, origin: { kind: 'person' } })
  expect((await band.find({ key: 'band-row-1' }))?.props.backgroundColor).toBe('userMessageBackground')
  expect((await band.find({ type: 'Button', key: 'band-down' }))?.props.hotkey).toBe('j')
  await band.press({ key: 'band-down' })
  expect((await band.find({ key: 'band-row-2' }))?.props.backgroundColor).toBe('userMessageBackground')

  // Every cell on the selection keeps its own color: the glyphs keep their
  // meaning, and the bar its green.
  await band.press({ key: 'band-down' })
  expect((await band.find({ key: 'band-row-3' }))?.props.backgroundColor).toBe('userMessageBackground')
  const blocked = await band.find({ type: 'Text', text: /^■2/ })
  expect(blocked?.props.color).toBe('red')
  for (const fill of await band.findAll({ type: 'Text', text: /^█/ })) expect(fill.props.color).toBe('green')

  // Esc: the ring leaves, the row goes back to normal colors.
  await $.ui.focus({ component: 'AbovePrompt', requestId: 'above-prompt', plugin: 'borant', origin: { kind: 'person' } })
  expect((await band.find({ key: 'band-row-3' }))?.props.backgroundColor).toBeUndefined()
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
  // Inline the card is compact, and the buttons sit in it.
  expect(await inline.find({ type: 'Text', text: /needs you · 1 of 2/ })).toBeUndefined()
  expect(await inline.find({ type: 'Button', key: 'done' })).toBeDefined()
  await inline.unmount()
  const idle = await $.ui.mount({ ...PANE, props: { ...PANE.props, placement: 'inline', isFocused: false }, surface: 'terminal' })
  await clock.advance(1000)
  expect(closes[0]).toBe('cos')
  await idle.unmount()
  await band.unmount()
})

test('the help opens from the band button and /cos help', async ($, on) => {
  const opened: string[] = []
  chiefStubs(on, opened)
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: 'help' })
  expect(opened).toEqual(['chief of staff'])
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })
  expect(await pane.find({ type: 'Text', text: /ctrl\+x tab/ })).toBeDefined()
  await pane.press({ key: 'pane-more' })
  expect(await pane.find({ type: 'Text', text: /ctrl\+x tab/ })).toBeUndefined()
  await pane.unmount()
  await band.unmount()
  const answer = await $.command.run({ command: 'cos', args: 'help' })
  expect(answer.text).toContain('j k')
  expect(answer.text).toContain('/cos help')
})

test('Done shows each step: the chief is closing it, closed, then the row goes', async ($, on) => {
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
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const ui = await $.ui.mount({ ...PANE, surface: 'terminal' })
  await ui.press({ key: 'done' })
  expect(prompts).toHaveLength(1)
  // The prompt landed: the chief has it, and the hub has not closed the task.
  expect(await ui.find({ type: 'Text', text: /the chief is closing it/ })).toBeDefined()
  // While it is under way, the row has no Done to press twice.
  expect(await ui.find({ type: 'Button', key: 'done' })).toBeUndefined()
  // The band row has the short form, in its fixed slot.
  expect(await band.find({ type: 'Text', text: /^◷ closing 0s/ })).toBeDefined()

  // The hub closes it: the next read shows the row as closed for a while.
  live = JSON.parse(JSON.stringify(live))
  for (const c of live.collections) {
    c.needs_you = (c.needs_you || []).filter((q: { task: string }) => q.task !== '01M5ZG00000000000000000015')
  }
  await clock.advance(5000)
  expect(await ui.find({ type: 'Text', text: /closed/ })).toBeDefined()
  expect(toasts).toContain('closed: Create Hetzner bucket borant-hub-backup + 1Password item')
  // Then it goes.
  await clock.advance(15000)
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeUndefined()
  expect(await ui.find({ type: 'Text', text: /closing it|closed/ })).toBeUndefined()
  await ui.unmount()
  await band.unmount()
})

test('a Done the hub has not closed after two minutes shows as still open', async ($, on) => {
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
  await ui.press({ key: 'done' })
  expect(await ui.find({ type: 'Text', text: /the chief is closing it/ })).toBeDefined()
  await clock.advance(120000)
  // The mark is short and goes before the title, so the row still names the item.
  expect(await ui.find({ type: 'Text', text: /still open after 2m/ })).toBeDefined()
  expect(await ui.find({ type: 'Text', text: /Create Hetzner bucket/ })).toBeDefined()
  expect((await ui.find({ type: 'Button', key: 'done' }))?.props.label).toBe('Done again')
  await ui.unmount()
  await band.unmount()
})

test('the event stream starts from a timer, reads the board at once on an event, and starts again when it ends', async ($, on) => {
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

  // A read shows "↻ reading" for at least 600 ms, so a fast one is seen.
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await band.find({ type: 'Text', text: /^↻ reading/ })).toBeDefined()
  await band.unmount()

  // The stream ended: the header says polling, and it starts again in 5 s.
  await clock.advance(600)
  const later = await $.ui.mount({ ...BAND, surface: 'terminal' })
  expect(await later.find({ type: 'Text', text: /^polling/ })).toBeDefined()
  await later.unmount()
  await clock.advance(5000)
  expect(spawns).toHaveLength(2)
})

test('in the turn a Done started, a call that would ask the person is refused', async ($, on) => {
  chiefStubs(on)
  on('prompt.submit', ($, e) => ({ text: e.text }))
  on('tool.check', () => ({ decision: 'ask' }))
  on('turn.complete', () => ({ text: '' }))
  await $.session.start({ ...SESSION })
  const band = await $.ui.mount({ ...BAND, surface: 'terminal' })
  await band.press({ key: GATE })
  const pane = await $.ui.mount({ ...PANE, surface: 'terminal' })

  expect((await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })).decision).toBe('ask')
  await pane.press({ key: 'done' })
  const held = await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })
  expect(held.decision).toBe('deny')
  expect(held.reason).toContain('borant MCP tools')
  // The turn ends: the person is back, and Claude Code decides as before.
  await $.turn.complete({ answer: '', durationMs: 1, isAborted: false, turnId: 't1', reason: 'answer' })
  expect((await $.tool.check({ tool: 'Bash', input: { command: 'borant --help' } })).decision).toBe('ask')
  await pane.unmount()
  await band.unmount()
})
