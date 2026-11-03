// What the hub does for the chief mod's actions, on the files that
// scripts/tui/borant-stub serves: an answer sets the task back to in progress,
// a close closes it as done, and a resume clears a blocked spawn. Each takes
// the task off needs-you. Usage: node stub-act.mjs <dir> answer|close <channel/task>
//                               node stub-act.mjs <dir> resume <agent>
import fs from 'fs'
import path from 'path'

const [dir, verb, ref] = process.argv.slice(2)
const boardFile = path.join(dir, 'board.json')
const board = JSON.parse(fs.readFileSync(boardFile, 'utf8'))
if (verb === 'resume') {
  for (const c of board.collections) c.blocked = (c.blocked || []).filter((b) => b.agent !== ref)
} else {
  const [channel, id] = ref.split('/')
  const file = path.join(dir, 'tasks-' + channel + '.json')
  const tasks = JSON.parse(fs.readFileSync(file, 'utf8'))
  for (const t of tasks) {
    if (t.id !== id) continue
    t.updated = Date.now()
    if (verb === 'answer') Object.assign(t, { status: 'in_progress', attention_msg: null })
    if (verb === 'close') Object.assign(t, { status: 'closed', close_reason: 'done' })
  }
  fs.writeFileSync(file, JSON.stringify(tasks, null, 1))
  for (const c of board.collections) c.needs_you = c.needs_you.filter((q) => q.task !== id)
}
fs.writeFileSync(boardFile, JSON.stringify(board, null, 1))

// The hub's feed and the task's history get the change, as the hub's log would.
const feedFile = path.join(dir, 'changes.jsonl')
// A hub's log sequence only grows; the stub's grows with the clock, so a seen
// mark that the mod kept from an earlier run never hides a new change.
const seq = Math.floor(Date.now() / 1000)
if (verb !== 'resume') {
  const [channel, id] = ref.split('/')
  const t = JSON.parse(fs.readFileSync(path.join(dir, 'tasks-' + channel + '.json'), 'utf8')).find((x) => x.id === id)
  const project = JSON.parse(fs.readFileSync(path.join(dir, 'channels.json'), 'utf8')).find((c) => c.name === channel)?.project
  const v = verb === 'answer' ? 'answered' : t && /^Approve/.test(t.title) ? 'gate_approved' : 'done'
  fs.appendFileSync(feedFile, JSON.stringify({ seq, at: Date.now(), project, channel, task: id, verb: v, text: t ? t.title : id, by: 'human' }) + '\n')
  const hf = path.join(dir, 'history-' + id + '.json')
  const h = fs.existsSync(hf) ? JSON.parse(fs.readFileSync(hf, 'utf8')) : []
  h.push({ seq, at: Date.now(), kind: verb === 'answer' ? 'answered' : 'status', status: verb === 'answer' ? 'in_progress' : 'closed', by: 'human', text: verb === 'answer' ? 'a person answered' : 'a person closed it as done' })
  fs.writeFileSync(hf, JSON.stringify(h))
}
