// An ANSI screen capture (tmux capture-pane -e) as one HTML page, for a PNG
// where `freeze` is not installed: lib.sh renders the page with headless
// Chrome. It reads the SGR codes tmux writes: bold, dim, italic, underline,
// inverse, and 16, 256, and true colors.
//
// Usage: node scripts/tui/ansi-html.mjs <capture.txt> <out.html> <columns>

import fs from 'fs'
import { cellsOf } from '../../integrations/claude-code/mod/board.js'

const [src, dst, cols = '200'] = process.argv.slice(2)
const BASE = ['#1e1e1e', '#cd3131', '#0dbc79', '#e5e510', '#2472c8', '#bc3fbc', '#11a8cd', '#e5e5e5']
const BRIGHT = ['#666666', '#f14c4c', '#23d18b', '#f5f543', '#3b8eea', '#d670d6', '#29b8db', '#ffffff']
const FG = '#d4d4d4'
const BG = '#1e1e1e'

function color256(n) {
  if (n < 8) return BASE[n]
  if (n < 16) return BRIGHT[n - 8]
  if (n < 232) {
    const i = n - 16
    const v = (x) => (x === 0 ? 0 : 55 + x * 40)
    return `rgb(${v(Math.floor(i / 36))},${v(Math.floor(i / 6) % 6)},${v(i % 6)})`
  }
  const g = 8 + (n - 232) * 10
  return `rgb(${g},${g},${g})`
}

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
// A character outside ASCII can come from a fallback font of another width,
// which would move the rest of its line: each one gets the cells it takes.
const cellsHtml = (s) =>
  Array.from(s)
    .map((ch) => (ch.codePointAt(0) < 0x7f ? esc(ch) : `<i class="c${cellsOf(ch)}">${esc(ch)}</i>`))
    .join('')

function apply(st, params) {
  const p = params.length ? params : [0]
  for (let i = 0; i < p.length; i++) {
    const n = p[i]
    if (n === 0) Object.assign(st, { fg: null, bg: null, bold: false, dim: false, italic: false, under: false, inverse: false })
    else if (n === 1) st.bold = true
    else if (n === 2) st.dim = true
    else if (n === 3) st.italic = true
    else if (n === 4) st.under = true
    else if (n === 7) st.inverse = true
    else if (n === 22) st.bold = st.dim = false
    else if (n === 23) st.italic = false
    else if (n === 24) st.under = false
    else if (n === 27) st.inverse = false
    else if (n >= 30 && n <= 37) st.fg = BASE[n - 30]
    else if (n >= 90 && n <= 97) st.fg = BRIGHT[n - 90]
    else if (n >= 40 && n <= 47) st.bg = BASE[n - 40]
    else if (n >= 100 && n <= 107) st.bg = BRIGHT[n - 100]
    else if (n === 39) st.fg = null
    else if (n === 49) st.bg = null
    else if (n === 38 || n === 48) {
      const key = n === 38 ? 'fg' : 'bg'
      if (p[i + 1] === 5) {
        st[key] = color256(p[i + 2])
        i += 2
      } else if (p[i + 1] === 2) {
        st[key] = `rgb(${p[i + 2]},${p[i + 3]},${p[i + 4]})`
        i += 4
      }
    }
  }
}

function style(st) {
  let fg = st.fg || FG
  let bg = st.bg
  if (st.inverse) [fg, bg] = [bg || BG, fg]
  const css = [`color:${fg}`]
  if (bg) css.push(`background:${bg}`)
  if (st.bold) css.push('font-weight:bold')
  if (st.dim) css.push('opacity:0.55')
  if (st.italic) css.push('font-style:italic')
  if (st.under) css.push('text-decoration:underline')
  return css.join(';')
}

const st = {}
apply(st, [0])
const lines = fs.readFileSync(src, 'utf8').replace(/\n$/, '').split('\n')
const html = lines.map((line) => {
  let out = ''
  let run = ''
  const flush = () => {
    if (run) out += `<span style="${style(st)}">${cellsHtml(run)}</span>`
    run = ''
  }
  const re = /\x1b\[([0-9;:]*)([A-Za-z])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|([^\x1b]+)/g
  let m
  while ((m = re.exec(line))) {
    if (m[3] !== undefined) run += m[3]
    else if (m[2] === 'm') {
      flush()
      apply(st, m[1] === '' ? [] : m[1].split(/[;:]/).map(Number))
    }
  }
  flush()
  return `<div class="l">${out || ' '}</div>`
})

fs.writeFileSync(
  dst,
  `<!doctype html><meta charset="utf-8"><style>
html,body{margin:0;background:${BG}}
body{padding:12px;font:14px/17px Menlo,"SF Mono",monospace;color:${FG}}
.l{white-space:pre;height:17px;width:${Number(cols)}ch;overflow:hidden}
i{font-style:inherit;display:inline-block;overflow:hidden;vertical-align:top}
.c0{width:0}.c1{width:1ch}.c2{width:2ch}
</style>${html.join('\n')}`,
)
