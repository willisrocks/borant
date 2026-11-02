// Board mode's key field: what an edit typed. Pure, so the tests call it
// with plain values.

/**
 * What one edit put into a field: the text between what the old and the new
 * value share at their start and at their end. A key typed after Left lands
 * in the middle, and only that key is new.
 */
export function inserted(before, after) {
  const a = Array.from(before)
  const b = Array.from(after)
  let lead = 0
  while (lead < a.length && lead < b.length && a[lead] === b[lead]) lead += 1
  let trail = 0
  while (trail < a.length - lead && trail < b.length - lead && a[a.length - 1 - trail] === b[b.length - 1 - trail]) trail += 1
  return b.slice(lead, b.length - trail).join('')
}
