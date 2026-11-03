// Text helpers shared by the chief mod (mod/model.js, mod/view.js) and the TUI
// test scripts: terminal cell widths, cutting text to a width, ages, and the
// board's needs-you questions in order. No mods API here.

/** Code points that take no cell: combining marks, a zero-width joiner,
 * variation selectors. */
function zeroWidth(cp) {
  return cp === 0x200d || (cp >= 0x300 && cp <= 0x36f) || (cp >= 0x1ab0 && cp <= 0x1aff) || (cp >= 0x20d0 && cp <= 0x20ff) || (cp >= 0xfe00 && cp <= 0xfe0f) || (cp >= 0xe0100 && cp <= 0xe01ef)
}

/** Ranges a terminal draws two cells wide: CJK, Hangul, full-width forms, and emoji. */
const WIDE = [
  [0x1100, 0x115f], [0x231a, 0x231b], [0x23e9, 0x23ec], [0x23f0, 0x23f0], [0x23f3, 0x23f3], [0x25fd, 0x25fe],
  [0x2614, 0x2615], [0x26a1, 0x26a1], [0x26aa, 0x26ab], [0x26bd, 0x26be], [0x26c4, 0x26c5], [0x26d4, 0x26d4],
  [0x26ea, 0x26ea], [0x26f2, 0x26f5], [0x26fa, 0x26fd], [0x2705, 0x2705], [0x270a, 0x270b], [0x2728, 0x2728],
  [0x274c, 0x274c], [0x274e, 0x274e], [0x2753, 0x2757], [0x2795, 0x2797], [0x27b0, 0x27b0], [0x27bf, 0x27bf],
  [0x2b1b, 0x2b1c], [0x2b50, 0x2b50], [0x2b55, 0x2b55], [0x2e80, 0x303e], [0x3041, 0x33ff], [0x3400, 0x4dbf],
  [0x4e00, 0x9fff], [0xa000, 0xa4cf], [0xac00, 0xd7a3], [0xf900, 0xfaff], [0xfe30, 0xfe4f], [0xff00, 0xff60],
  [0xffe0, 0xffe6], [0x1f300, 0x1f64f], [0x1f680, 0x1f6ff], [0x1f900, 0x1f9ff], [0x1fa70, 0x1faff], [0x20000, 0x3fffd],
]

/** The cells one character takes in a terminal: 0, 1, or 2. */
function charCells(ch) {
  const cp = ch.codePointAt(0)
  if (zeroWidth(cp)) return 0
  return WIDE.some(([a, b]) => cp >= a && cp <= b) ? 2 : 1
}

/** The cells a text takes in a terminal. */
export function cellsOf(value) {
  let n = 0
  for (const ch of String(value)) n += charCells(ch)
  return n
}

/** The head of a text that fits in `room` cells, and the cells it takes. */
function head(value, room) {
  let out = ''
  let used = 0
  for (const ch of String(value)) {
    const w = charCells(ch)
    if (used + w > room) break
    out += ch
    used += w
  }
  return { text: out, used }
}

/** Cut text to `width` cells, with an ellipsis. */
export function truncate(value, width) {
  if (width <= 0) return ''
  if (cellsOf(value) <= width) return String(value)
  if (width === 1) return '…'
  return head(value, width - 1).text + '…'
}

/** The instant of a board Timestamp, or null. The hub writes integer
 * milliseconds; an older fixture may hold an ISO string, so both read. */
function instant(at) {
  const ms =
    at instanceof Date ? at.getTime() : typeof at === 'string' && at.length >= 16 ? Date.parse(at) : at
  if (typeof ms !== 'number' || !Number.isFinite(ms)) return null
  const date = new Date(ms)
  return Number.isNaN(date.getTime()) ? null : date
}

/** The milliseconds of a board Timestamp, or null. */
function stamp(at) {
  const date = instant(at)
  return date ? date.getTime() : null
}

/** Every needs-you question, gates first and then the oldest. */
export function allQuestions(board) {
  const out = []
  for (const collection of board?.collections || []) {
    for (const q of collection.needs_you || []) out.push(q)
  }
  return out.sort((a, b) => {
    const gate = Number(Boolean(b.gate)) - Number(Boolean(a.gate))
    if (gate !== 0) return gate
    const since = (stamp(a.since) ?? 0) - (stamp(b.since) ?? 0)
    if (since !== 0) return since
    return String(a.title || '').localeCompare(String(b.title || ''))
  })
}

/** `45s`, `42m`, `1h 4m`, `3d`. */
export function ageText(ms) {
  const s = Math.max(0, Math.floor((Number(ms) || 0) / 1000))
  if (s < 60) return s + 's'
  const m = Math.floor(s / 60)
  if (m < 60) return m + 'm'
  const h = Math.floor(m / 60)
  if (h < 24) return h + 'h' + (m % 60 ? ' ' + (m % 60) + 'm' : '')
  return Math.floor(h / 24) + 'd'
}
