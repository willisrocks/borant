// Compare the band in two plain-text captures (tui_shot's .plain.txt), for
// the scenario's "nothing moves across a live update" checks. A band row is a
// line that starts with a chip (`1: ` or three spaces) and a state glyph. Its
// place is its screen line, and its columns are the cells where text starts
// after two or more spaces. A row is known by its text before its first gap,
// 16 cells at most, so a row whose title grew at its end is still the same
// row. A row
// that moved up or down, or a column that moved, is a move; so is a band
// title row that moved up or down. Without --cells a row that is gone is a
// move too.
//
// Usage: node scripts/tui/columns.mjs [--cells] <before.plain.txt> <after.plain.txt>
// With --cells only the columns count: rows may come and go between them.
// Prints one line per row that moved and exits 1, or exits 0.

import fs from 'fs'

const ROW = /^(\d: |   )([▲●■○◇✓–]) (.*)$/u

function rows(file) {
  const out = new Map()
  const lines = fs.readFileSync(file, 'utf8').split('\n')
  const title = lines.findIndex((l) => /^── (chief of staff|cos) /.test(l))
  if (title >= 0 && !cellsOnly) out.set('(title row)', { line: title, cells: [], last: '' })
  for (const [at, line] of lines.entries()) {
    // The band is left of a docked pane's border.
    const band = line.split('│')[0]
    const m = band.match(ROW)
    if (!m) continue
    const cells = Array.from(band)
    const starts = []
    // The chip and the glyph (cells 0 to 4) are fixed; a row past nine has
    // no digit, which is no move.
    for (let i = 5; i < cells.length; i++) {
      if (cells[i] !== ' ' && cells[i - 1] === ' ' && cells[i - 2] === ' ') starts.push(i)
    }
    // Its name: the text before its first gap, at most 16 cells.
    const name = Array.from(m[3].split(/  /)[0]).slice(0, 16).join('')
    const last = starts.length ? cells.slice(starts[starts.length - 1]).join('').trim() : ''
    out.set(name, { line: cellsOnly ? null : at, cells: starts, last })
  }
  return out
}

const args = process.argv.slice(2)
const cellsOnly = args[0] === '--cells'
const [a, b] = cellsOnly ? args.slice(1) : args
const before = rows(a)
const after = rows(b)
let moved = 0
let same = 0
// A press status (`✓ asked`, `◷ closing`, `left the board`) may come or go in
// a row's tail: the longer row's last column, when the rows agree on every
// column before it. Any other difference is a move, so a project row that
// loses its orchestrator moves, whatever its glyph.
const STATUS = /^([◷✓→!?] |left the board)/u
const sameColumns = (x, y) => {
  if (x.cells.join() === y.cells.join()) return true
  const [short, long] = x.cells.length < y.cells.length ? [x, y] : [y, x]
  return long.cells.length === short.cells.length + 1 && long.cells.slice(0, -1).join() === short.cells.join() && STATUS.test(long.last)
}
const show = (v) => (v.line === null ? '' : 'line ' + v.line + ' ') + 'cells ' + v.cells.join(',')
for (const [name, x] of before) {
  const y = after.get(name)
  if (!y) {
    // Across a live update every row is held; at rest (--cells) a row may go.
    if (cellsOnly) continue
    moved++
    console.log('gone: ' + name + ' [' + show(x) + ']')
    continue
  }
  if (x.line === y.line && sameColumns(x, y)) same++
  else {
    moved++
    console.log('moved: ' + name + ' [' + show(x) + '] -> [' + show(y) + ']')
  }
}
console.log('rows compared: ' + (same + moved) + ', moved: ' + moved)
process.exit(moved || same + moved === 0 ? 1 : 0)
