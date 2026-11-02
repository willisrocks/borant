// The preview's lines and the key field's edits, with plain values and no session.
import { expect, test } from 'claude-code/testing'
import board from '../fixtures/board.js'
import { bandItems } from '../board.js'
import { previewLines, titleLineCount, wrap } from '../preview.js'
import { inserted } from '../keys.js'

const text = (line: { text: string }[]) => line.map((s) => s.text).join('')

test('a word longer than a line goes on over the next lines when asked', () => {
  expect(wrap('X'.repeat(120), 50)).toEqual(['X'.repeat(49) + '…'])
  expect(wrap('X'.repeat(120), 50, true)).toEqual(['X'.repeat(50), 'X'.repeat(50), 'X'.repeat(20)])
  expect(titleLineCount('X'.repeat(120), 52)).toBe(3)
})

test('details keep the lines the title had when they opened: a longer title is cut, the fields stay', () => {
  const item = bandItems(board)[0]
  const at = (title: string) => {
    const lines = previewLines({ board, item: { ...item, q: { ...item.q, title } }, index: 0, count: 2, width: 52, more: true, titleHold: 2, tasks: {}, actions: [] })
    return lines.findIndex((l: { text: string }[]) => /project/.test(text(l)))
  }
  const short = at('Short title')
  expect(at('A much longer title that now runs over three or four lines of the card, and more, and more')).toBe(short)
})

test('a key field edit types only what it put in, also after Left', () => {
  expect(inserted('', 'j')).toBe('j')
  expect(inserted('j', 'jk')).toBe('k')
  expect(inserted('jk', 'jjk')).toBe('j')
  expect(inserted('jjk', 'jj')).toBe('')
  expect(inserted('', 'jjj')).toBe('jjj')
})
