// The design's sample swarm (docs/superpowers/specs/2026-11-02-chief-mod-v3/
// mock/data.mjs: 2 collections, 10 projects) as the files scripts/tui/borant-stub
// serves: board.json, channels.json, and tasks-<channel>.json, with times
// relative to now. The chief mod v3's videos and scenario run on it.
//
// Usage: node scripts/tui/swarm.mjs <stub-dir>

import fs from 'fs'
import path from 'path'
import { NEEDS, PROJECTS, IDLE } from '../../docs/superpowers/specs/2026-11-02-chief-mod-v3/mock/data.mjs'

const dir = process.argv[2]
if (!dir) {
  console.error('usage: swarm.mjs <stub-dir>')
  process.exit(2)
}
fs.mkdirSync(dir, { recursive: true })
const NOW = Date.now()
const MIN = 60000
let serial = 1
const ulid = () => '01M6SW' + String(serial++).padStart(20, '0')
const ago = (text) => {
  const m = /^(\d+)([mh])$/.exec(text || '')
  if (!m) return 5 * MIN
  return Number(m[1]) * (m[2] === 'h' ? 60 : 1) * MIN
}
const STATUS = { wait: 'needs_human', stuck: 'stuck', run: 'in_progress', silent: 'in_progress', ready: 'open', blocked: 'open', later: 'open', done: 'closed' }
const counts = () => ({ open: 0, ready: 0, running: 0, blocked: 0, stuck: 0, needs_human: 0, done: 0 })

const collections = { personal: { collection: 'personal', projects: [], needs_you: [], blocked: [] }, work: { collection: 'work', projects: [], needs_you: [], blocked: [] } }
const channels = []
for (const p of PROJECTS) {
  const channel = p.coll + '-' + p.name
  channels.push({ name: channel, kind: 'project', project: p.name, collection: p.coll, owner: p.orch, closed: null })
  const ids = new Map(p.tasks.map((t) => [t.id, ulid()]))
  const records = p.tasks.map((t) => {
    const age = ago(t.age)
    const blockedBy = t.s === 'later' && /after (.+)$/.exec(t.note || '') ? p.tasks.filter((x) => (t.note || '').includes(x.short) || (t.note || '').includes(x.title)).map((x) => ids.get(x.id)) : []
    return {
      id: ids.get(t.id),
      title: t.title,
      body: '',
      status: STATUS[t.s],
      close_reason: t.s === 'done' ? 'done' : null,
      close_message: null,
      attention_msg: null,
      claimed_by: ['run', 'wait', 'stuck', 'silent'].includes(t.s) && t.who && !t.who.startsWith('orch') && t.who !== 'orchestrator' ? t.who : null,
      deadline: null,
      attempts: 0,
      max_attempts: null,
      blocked_by: t.s === 'later' && t.note === 'after your approval' ? [ids.get('gate')].filter(Boolean) : blockedBy,
      parent: null,
      kind: 'task',
      issue: null,
      properties: t.pr ? { pr_url: 'https://github.com/willisrocks/' + p.name + '/pull/' + t.pr.slice(1) } : {},
      created: NOW - age - 30 * MIN,
      updated: NOW - age,
      create_key: null,
    }
  })
  fs.writeFileSync(path.join(dir, 'tasks-' + channel + '.json'), JSON.stringify(records, null, 1))
  collections[p.coll].projects.push({
    project: p.name,
    mirror_issues: false,
    // `orch-roger-emr` came back from a lid close with its turn frozen: the
    // hub's `agent_hung` rule flags it, and the board line says so.
    orchestrators: [{ agent: p.orch, online: !p.orchState.startsWith('offline'), busy: true, hung: p.name === 'roger-emr', host: 'macbook' }],
    sagas: [],
    epics: [],
    tasks: counts(),
  })
  for (const n of NEEDS.filter((x) => x.proj === p.name && x.task)) {
    const t = p.tasks.find((x) => x.id === n.task)
    const r = records.find((x) => x.id === ids.get(n.task))
    if (n.verb === 'unblock') continue
    r.attention_msg = (n.detail || []).join(' ').replace(/^"|"$/g, '')
    collections[p.coll].needs_you.push({ project: p.name, channel, task: r.id, title: t.title, status: r.status, attention_msg: r.attention_msg, since: NOW - ago(n.age), gate: n.verb === 'approve', owner: p.orch })
  }
  fs.writeFileSync(path.join(dir, 'tasks-' + channel + '.json'), JSON.stringify(records, null, 1))
}
// The blocked spawn of roger-support.
collections.work.blocked.push({ agent: 'helper-qa-preprod', channel: 'work-roger-support', project: 'roger-support', reason: 'missing bundle github-work', action: 'add bundle github-work to a host that the spawn may use, or spawn without it', since: NOW - 3 * 60 * MIN, spawner: 'orch-roger-support' })
for (const name of IDLE) {
  collections.personal.projects.push({ project: name, mirror_issues: false, orchestrators: [], sagas: [], epics: [], tasks: counts() })
}
// A task that waits on the person but is not on needs-you waits on other work.
for (const c of Object.values(collections)) {
  for (const p of c.projects) {
    const file = path.join(dir, 'tasks-' + c.collection + '-' + p.project + '.json')
    if (!fs.existsSync(file)) continue
    const list = JSON.parse(fs.readFileSync(file, 'utf8'))
    for (const t of list) if (t.status === 'needs_human' && !c.needs_you.some((q) => q.task === t.id)) t.status = 'in_progress'
    fs.writeFileSync(file, JSON.stringify(list, null, 1))
  }
}
fs.writeFileSync(path.join(dir, 'board.json'), JSON.stringify({ at: NOW, collections: [collections.personal, collections.work] }, null, 1))
fs.writeFileSync(path.join(dir, 'channels.json'), JSON.stringify(channels, null, 1))
// Each agent's activity (borant status --json): wkr-boop-2 went silent 31 minutes ago.
const agents = []
for (const p of PROJECTS) {
  agents.push({ agent: p.orch, host: 'macbook', busy: true, last_tool_at: NOW - 40000, last_tool: 'mcp borant task_list', tool_running_since: null })
  for (const t of p.tasks) {
    if (!t.who || t.who === p.orch || !['run', 'wait', 'stuck', 'silent'].includes(t.s)) continue
    agents.push(t.s === 'silent'
      ? { agent: t.who, host: 'macbook', busy: true, last_tool_at: NOW - 31 * MIN, last_tool: 'Bash npm test -- search', last_text_at: NOW - 31 * MIN, last_text: '3 ranking tests fail; reading the scorer', tool_running_since: null }
      : { agent: t.who, host: 'macbook', busy: true, last_tool_at: NOW - 20000, last_tool: 'Bash cargo test', tool_running_since: null })
  }
}
fs.writeFileSync(path.join(dir, 'status.json'), JSON.stringify({ hosts: [], agents, inboxes: [], at: NOW }))
// What the PR watch last read, by PR URL.
const STATE = {
  'ready to merge': { state: 'open', checks: { pass: {} }, checks_passed: 7, checks_total: 7, review: 'approved', review_requested: false },
  'review requested': { state: 'open', checks: { pass: {} }, checks_passed: 7, checks_total: 7, review: 'none', review_requested: true },
  'CI running': { state: 'open', checks: { pending: {} }, checks_passed: 3, checks_total: 7, review: 'none', review_requested: false },
}
const prs = {}
for (const p of PROJECTS) for (const t of p.tasks) if (t.pr && STATE[t.prState]) prs['https://github.com/willisrocks/' + p.name + '/pull/' + t.pr.slice(1)] = STATE[t.prState]
fs.writeFileSync(path.join(dir, 'prs.json'), JSON.stringify(prs))
fs.writeFileSync(path.join(dir, 'changes.jsonl'), '')
console.log('swarm: ' + PROJECTS.length + ' projects, ' + channels.length + ' channels in ' + dir)
