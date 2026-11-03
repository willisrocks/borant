// What the hub answers for the chief mod's newer reads, from the files that
// scripts/tui/borant-stub serves: `board --json --tasks` (task lines composed
// from tasks-<channel>.json at each read), `changes --since <seq|ms> --json`
// (changes.jsonl), and `task history <channel/id> --json`.
// Usage: node stub-read.mjs <dir> board | changes <since> | history <channel/id>
import fs from 'fs'
import path from 'path'

const [dir, what, arg] = process.argv.slice(2)
const read = (f, d) => (fs.existsSync(path.join(dir, f)) ? JSON.parse(fs.readFileSync(path.join(dir, f), 'utf8')) : d)
const channels = read('channels.json', [])

if (what === 'board') {
  const board = read('board.json', { collections: [] })
  const prs = read('prs.json', {})
  for (const c of board.collections) {
    for (const p of c.projects) {
      const offline = p.orchestrators.length && p.orchestrators.every((o) => !o.online)
      const tasks = channels.filter((ch) => ch.project === p.project).flatMap((ch) => read('tasks-' + ch.name + '.json', []).map((t) => ({ ...t, channel: ch.name })))
      const byId = new Map(tasks.map((t) => [t.id, t]))
      p.task_lines = tasks.filter((t) => t.status !== 'closed').map((t) => {
        const open = (t.blocked_by || []).map((id) => byId.get(id)).filter((x) => x && x.status !== 'closed')
        const start = t.status !== 'open' ? null : open.length ? 'later' : offline ? 'blocked' : 'ready'
        const waits = start === 'later' ? open.map((x) => (x.status === 'needs_human' ? 'your approval' : x.title)).join(', ') : start === 'blocked' ? 'orchestrator offline' : null
        return { channel: t.channel, task: t.id, title: t.title, kind: t.kind, status: t.status, start, waits_on: waits, claimed_by: t.claimed_by, since: t.updated, attention_msg: t.attention_msg, pr: t.properties.pr_url ? { url: t.properties.pr_url, ...(prs[t.properties.pr_url] || { state: 'open', checks: { none: {} }, review: 'none', review_requested: false }) } : null, version: t.updated }
      })
      p.recent_done = tasks.filter((t) => t.status === 'closed').sort((a, b) => b.updated - a.updated).slice(0, 10).map((t) => ({ channel: t.channel, task: t.id, title: t.title, closer: t.claimed_by, at: t.updated, pr_url: t.properties.pr_url || null }))
      p.done_since = p.recent_done.length
    }
  }
  console.log(JSON.stringify(board))
} else if (what === 'changes') {
  const n = Number(arg)
  const all = fs.existsSync(path.join(dir, 'changes.jsonl')) ? fs.readFileSync(path.join(dir, 'changes.jsonl'), 'utf8').split('\n').filter(Boolean).map((l) => JSON.parse(l)) : []
  const changes = all.filter((c) => (n < 1e11 ? c.seq > n : c.at > n)).sort((a, b) => b.seq - a.seq)
  console.log(JSON.stringify({ at: Date.now(), changes }))
} else if (what === 'history') {
  const [channel, id] = arg.split('/')
  const t = read('tasks-' + channel + '.json', []).find((x) => x.id === id)
  const extra = read('history-' + id + '.json', [])
  const out = !t ? [] : [
    { seq: 1, at: t.created, kind: 'created', status: 'open', by: null, text: 'created' },
    ...(t.claimed_by ? [{ seq: 2, at: t.created + 60000, kind: 'claimed', status: 'in_progress', by: { type: 'agent', id: t.claimed_by }, text: t.claimed_by + ' claimed the task' }] : []),
    ...(t.attention_msg ? [{ seq: 3, at: t.updated, kind: 'status', status: t.status, by: t.claimed_by ? { type: 'agent', id: t.claimed_by } : null, text: (t.claimed_by || 'the owner') + ' asked: ' + t.attention_msg }] : []),
    ...extra,
  ]
  console.log(JSON.stringify(out))
}
