// Compare the band in two plain-text captures (tui_shot's .plain.txt), for
// the scenario's "nothing moves across a live update" checks. A band row is a
// line that starts with a chip (`1: ` or three spaces) and a state glyph. Its
// place is its screen line, and its columns are the cells where text starts
// after two or more spaces. A row is known by its first 16 cells after the
// glyph, so a row whose title grew at its end is still the same row. A row
// that moved up or down, or a column that moved, is a move; so is a band
// title row that moved up or down.
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
  if (title >= 0 && !cellsOnly) out.set('(title row)', 'line ' + title)
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
    const name = Array.from(m[3]).slice(0, 16).join('')
    out.set(name, (cellsOnly ? '' : 'line ' + at + ' ') + 'cells ' + starts.join(','))
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
for (const [name, cols] of before) {
  if (!after.has(name)) continue
  if (after.get(name) === cols) same++
  else {
    moved++
    console.log('moved: ' + name + ' [' + cols + '] -> [' + after.get(name) + ']')
  }
}
console.log('rows compared: ' + (same + moved) + ', moved: ' + moved)
process.exit(moved || same + moved === 0 ? 1 : 0)
