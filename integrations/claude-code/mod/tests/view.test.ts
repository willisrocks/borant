// The views: exact widths, the band's row limit, and no hotkey at the prompt.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import channels from '../fixtures/channels.js'
import { tasks } from '../fixtures/tasks.js'
import { buildModel, snapshot } from '../model.js'
import { bandLines, keyLines, listEntries, needDetail, panelLines, topLine, width } from '../view.js'

const NOW = board.at
const model = () => buildModel({ board, channels, tasks, seen: snapshot(tasks, NOW - 3600000), now: NOW })

type Part = { segs?: { text: string }[]; btn?: { key: string; hotkey?: string; action?: string } }
type Line = { parts: Part[] }
const lineWidth = (l: Line) => width(l.parts.flatMap((p) => p.segs || []))
const textOf = (l: Line) => l.parts.flatMap((p) => p.segs || []).map((s) => s.text).join('')

test('every band line is exactly as wide as the band, at 115, 195, and 315 cells', () => {
  for (const w of [61, 115, 195, 315]) {
    for (const l of bandLines(model(), w, {})) expect(lineWidth(l)).toBe(w)
  }
})

test('the band names the hotkey, and no band Button has a hotkey', () => {
  const lines = bandLines(model(), 195, {})
  expect(textOf(lines[0])).toContain('ctrl+x b or /borant Borant keys')
  const buttons = lines.flatMap((l) => l.parts.filter((p) => p.btn).map((p) => p.btn))
  expect(buttons.find((b) => b?.key === 'open')?.action).toBe('app:cycleDiffBase')
  expect(buttons.filter((b) => b?.hotkey)).toEqual([])
})

test('a band taller than its room keeps maxRows and says how many rows are left', () => {
  const lines = bandLines(model(), 115, { maxRows: 4 })
  expect(lines).toHaveLength(4)
  expect(textOf(lines[3])).toContain('more rows')
})

test('the band uses the whole width: from 250 cells, triage left and projects right', () => {
  const lines = bandLines(model(), 315, {})
  expect(textOf(lines[1])).toContain('│')
  expect(textOf(lines[1])).toContain('PROJECT')
})

test('the panel is as tall as asked, with its key lines at the bottom', () => {
  const m = model()
  const lines = panelLines(90, 30, { top: topLine(90), entries: (w) => listEntries(m, w, m.needs[0].id), sel: m.needs[0].id, detail: (w) => needDetail(m.needs[0], m.projects[0], w), keys: [['r', 'answer', 'answer']] })
  expect(lines).toHaveLength(30)
  expect(textOf(lines.at(-1) as Line)).toContain('r: answer')
})

test('key lines are Buttons with their hotkey; a key with no action is text', () => {
  const [l] = keyLines([['r', 'answer', 'answer'], ['↑↓', 'move', null]], 80)
  expect(l.parts.filter((p) => p.btn).map((p) => p.btn?.hotkey)).toEqual(['r'])
})

test('in a 54-cell panel every key Button is still drawn: a missing one sends its key to the prompt', () => {
  const keys = [['r', 'answer', 'answer'], ['a', 'ask', 'ask'], ['m', 'message', 'message'], ['g', 'attach', 'attach'], ['d', 'close as done', 'done'], ['p', 'project', 'project'], ['j', 'down', 'move'], ['k', 'up', 'move'], ['1', 'needs', 'section'], ['2', 'offline', 'section'], ['3', 'ships', 'section'], ['4', 'changed', 'section'], ['5', 'projects', 'section'], ['x', 'seen', 'seen']]
  const drawn = keyLines(keys, 54).flatMap((l: Line) => l.parts.filter((p) => p.btn).map((p) => p.btn?.hotkey))
  expect(drawn).toEqual(keys.map((k) => k[0]))
})
