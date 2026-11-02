// Compare the band's columns in two plain-text captures (tui_shot's
// .plain.txt), for the scenario's "no column moves across a live update"
// check. A band row is a line that starts with a chip (`1: ` or three
// spaces) and a state glyph. Its columns are the cells where text starts
// after two or more spaces, and where its glyph is. A row is known by its
// first 16 cells after the glyph, so a row that moved down (a new row above
// it) or whose title grew at its end is still the same row.
//
// Usage: node scripts/tui/columns.mjs <before.plain.txt> <after.plain.txt>
// Prints one line per row that moved and exits 1, or exits 0.

import fs from 'fs'

const ROW = /^(\d: |   )([▲●■○◇✓]) (.*)$/u

function rows(file) {
  const out = new Map()
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
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
    out.set(name, starts.join(','))
  }
  return out
}

const [a, b] = process.argv.slice(2)
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
