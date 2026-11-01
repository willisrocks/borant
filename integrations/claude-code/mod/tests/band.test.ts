// The ledger band's rows, on the fixture and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import { rowText, stepKey, text } from '../board.js'
import { bandKeys, bandRows } from '../band.js'

type Row = { key: string; segments: { text: string; button?: { key: string; hotkey?: string } }[]; selected: boolean }
const lines = (rows: Row[]) => rows.map((r) => rowText(r.segments))
const GATE = 'q:personal-borant/01M5ZG00000000000000000015'

/** The fixture with one long project name, saga, and epic, and twelve projects. */
function crowded() {
  const b = structuredClone(board)
  const projects = b.collections[0].projects
  const base = projects.find((p) => p.project === 'borant')!
  for (let i = 0; i < 10; i += 1) {
    const p = structuredClone(base)
    p.project = i === 0 ? 'roger-emr-observability-dashboards' : 'p' + i
    if (i === 0) p.sagas[0].title = 'ROG-4199: LiteLLM proxy rollout across every environment'
    projects.push(p)
  }
  return b
}

test('the title row, one row per needs-you item, then one per project, each to the edge', () => {
  const rows = lines(bandRows(board, { width: 140, fresh: text('live', 'green') }))
  expect(rows).toHaveLength(5)
  expect(rows[0]).toMatch(/^── chief of staff ─ 2 need you · 2 active · 1 idle ─+ 1-4 select · \/cos help ─ live ─ \[ \? \] ─$/)
  expect(rows[1]).toMatch(/^1: ▲ Create Hetzner bucket borant-hub-backup \+ 1Password item +borant · blocks 2 epics · 42m/)
  expect(rows[3]).toMatch(/^3: borant +Elastic compute +▲ Hub backup to object storage +████░░░░░░ 2\/5 +2 blocked +orchestrator/)
  expect(rows[4]).toContain('3 open tasks')
  for (const row of rows) expect(Array.from(row).length).toBe(140)
  // The idle boopmark has no row; the header counts it.
  expect(rows.join('\n')).not.toContain('boopmark')
})

test('names are never cut while room exists; narrower, the band drops columns in order', () => {
  const b = crowded()
  const at = (width: number) => lines(bandRows(b, { width }))
  const wide = at(200)
  expect(wide.join('\n')).toContain('roger-emr-observability-dashboards')
  expect(wide[5]).toContain('orchestrator')
  expect(wide[5]).toContain('2 blocked')
  // 100 columns: no orchestrator, glyph counts, no saga; the name stays whole.
  const narrow = at(100)
  expect(narrow[0]).toMatch(/^── cos ─/)
  expect(narrow.join('\n')).toContain('roger-emr-observability-dashboards')
  expect(narrow[3]).toContain('■2')
  expect(narrow[3]).not.toContain('Elastic compute')
  expect(narrow[3]).not.toContain('orchestrator')
  for (const width of [200, 140, 100, 80, 60]) {
    for (const row of at(width)) expect(Array.from(row).length).toBe(width)
  }
})

test('digits stop at nine; a later row keeps its columns', () => {
  const rows = bandRows(crowded(), { width: 140 })
  const hotkeys = rows.flatMap((r) => r.segments.filter((s) => s.button?.hotkey).map((s) => s.button!.hotkey))
  expect(hotkeys).toEqual(['1', '2', '3', '4', '5', '6', '7', '8', '9'])
  const ninth = lines(rows)[9]
  const tenth = lines(rows)[10]
  expect(ninth.startsWith('9: ')).toBe(true)
  expect(tenth.startsWith('   p')).toBe(true)
  expect(tenth.indexOf('Elastic compute')).toBe(ninth.indexOf('Elastic compute'))
})

test('while the band holds the keys the hint names them, and j and k are Buttons', () => {
  const rows = bandRows(board, { width: 140, selected: GATE, focused: true, ring: true })
  expect(lines(rows)[0]).toContain('j: ↓ k: ↑ move · ⏎ open · d done · a ask · esc prompt')
  // From the prompt, after a digit, space opens and typing leaves.
  const typed = bandRows(board, { width: 140, selected: GATE, focused: true })
  expect(lines(typed)[0]).toContain('j: ↓ k: ↑ move · space open · d done · a ask · type to leave')
  const title = rows[0].segments.filter((s) => s.button?.hotkey).map((s) => s.button!.hotkey)
  expect(title).toEqual(['j', 'k'])
  expect(lines(rows)[1]).toContain('d: Done   a: Ask orchestrator')
  expect(rows[1].selected).toBe(true)
  // At 60 cells the hint shortens, and the keys stay.
  const tight = bandRows(board, { width: 60, selected: GATE, focused: true })
  expect(tight[0].segments.filter((s) => s.button?.hotkey).length).toBe(2)
  // The keys j and k walk, wrapping.
  const keys = bandKeys(board)
  expect(keys[0]).toBe(GATE)
  expect(stepKey(keys, keys[keys.length - 1], 1)).toBe(GATE)
})

test('the empty board says nothing needs you, in one dim row', () => {
  const calm = structuredClone(board)
  calm.collections[0].needs_you = []
  calm.collections[0].projects = calm.collections[0].projects.filter((p) => p.project === 'boopmark')
  const rows = lines(bandRows(calm, { width: 120 }))
  expect(rows[0]).toContain('✓ nothing needs you · 0 active · 1 idle')
  expect(rows[1].trim()).toBe('no project has open work')
  expect(rows).toHaveLength(2)
})

test('each count kind keeps its own place, and a zero leaves a blank', () => {
  const rows = lines(bandRows(crowded(), { width: 200 }))
  const borant = rows.find((r) => r.startsWith('3: borant'))!
  const notes = rows.find((r) => r.startsWith('4: notes'))!
  // borant has no running task: its blocked count sits under notes' blocked.
  expect(borant.indexOf('2 blocked')).toBe(notes.indexOf('1 blocked'))
})

test('a status never moves the title column, and the hint names only keys that act', () => {
  const pending = { 'personal-borant/01M5ZG00000000000000000015': { state: 'closing', at: 0, title: 'x' } }
  const asked = { 'personal-borant/01M5ZG00000000000000000016': { state: 'sent', owner: 'orch-roger-emr-observability-dashboards', time: '10:41' } }
  for (const width of [200, 100]) {
    const rows = bandRows(board, { width, selected: GATE, focused: true, pending, asked })
    const text = lines(rows)
    const titleAt = (row: string, title: string) => row.indexOf(title)
    expect(titleAt(text[1], 'Create Hetzner')).toBe(titleAt(text[2], 'Revoke old'))
    // The context column starts in one place on both rows, status or not.
    expect(text[1]).toContain('the chief is closing it')
    // Done is under way: the hint has no d, and the row no Done button.
    expect(text[0]).not.toContain(' d done')
    expect(rows[1].segments.some((s) => s.button?.key === 'done')).toBe(false)
    for (const row of text) expect(Array.from(row).length).toBe(width)
  }
  // Narrow, an ask's status drops the owner's name before the title gives way.
  const narrow = lines(bandRows(board, { width: 90, selected: 'q:personal-borant/01M5ZG00000000000000000016', focused: true, asked }))
  expect(narrow[2]).toContain('Revoke old willisrocks gh token?')
  expect(narrow[2]).toContain('✓ asked at 10:41')
})
