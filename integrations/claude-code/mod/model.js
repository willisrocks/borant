// The chief mod's model: a pure mapping from what the hub says (the board,
// the channel list, each channel's tasks) and what the mod keeps (the "seen"
// snapshot, the results of the person's actions) to what the views draw.
// No mods API here, so the tests call it with plain values.
//
// The model, every field derived, so every surface agrees:
//   needs     what needs the person, most urgent first (answer, decide,
//             approve, unblock); a finished action stays as a receipt row for
//             RECEIPT_MS and leaves the counts at once
//   quiet     work that went quiet: an orchestrator offline with open tasks
//   changes   what changed since the person last pressed x, newest first
//   ships     open PRs, newest activity first
//   projects  each project with work, its tasks by state; idle: the others

import { allQuestions } from './board.js'

/** A receipt row stays this long after its action, then leaves. */
export const RECEIPT_MS = 10000

const PR_NUMBER = /\/pull\/(\d+)/

/** `#123` from a PR URL, or `PR`. */
export function prLabel(url) {
  const m = PR_NUMBER.exec(String(url || ''))
  return m ? '#' + m[1] : 'PR'
}

function ms(at) {
  if (typeof at === 'number') return at
  const t = Date.parse(at)
  return Number.isFinite(t) ? t : null
}

/** The channels of each project: the board's sagas and epics, and every open channel that names the project. */
export function projectChannels(board, channels) {
  const out = new Map()
  const add = (project, channel) => {
    if (!project || !channel) return
    if (!out.has(project)) out.set(project, [])
    if (!out.get(project).includes(channel)) out.get(project).push(channel)
  }
  for (const c of board?.collections || []) {
    for (const p of c.projects || []) {
      out.set(p.project, out.get(p.project) || [])
      for (const s of p.sagas || []) if (s.state !== 'closed') add(p.project, s.channel)
      for (const e of p.epics || []) if (e.status !== 'closed') add(p.project, e.channel)
    }
  }
  for (const ch of channels || []) if (!ch.closed && ch.project && out.has(ch.project)) add(ch.project, ch.name)
  return out
}

/** Every channel the model reads tasks from. */
export function allChannels(board, channels) {
  return [...new Set([...projectChannels(board, channels).values()].flat())]
}

/** The state of one task line: wait, stuck, run, ready, later, blocked, done. */
export function taskState(task, byId, orchOffline) {
  if (task.status === 'closed') return 'done'
  if (task.status === 'needs_human') return 'wait'
  if (task.status === 'stuck') return 'stuck'
  if (task.status === 'in_progress') return 'run'
  const open = (task.blocked_by || []).map((id) => byId.get(id)).filter((t) => t && t.status !== 'closed')
  if (open.length) return 'later'
  if (orchOffline) return 'blocked'
  return 'ready'
}

const STATE_ORDER = { stuck: 0, blocked: 1, wait: 2, run: 3, ready: 4, later: 5, done: 6 }
const NEED_ORDER = { unblock: 0, decide: 1, answer: 2, approve: 3 }

/**
 * The model. `input`:
 *   board, channels, tasks ({ channel: Task[] | null }),
 *   seen ({ at, ids: { [taskId]: status } } or null: no snapshot yet),
 *   marks ({ receipts: { [itemId]: { text, at, item, index } }, asked: { [itemId]: { owner, at } }, changes: [...] }),
 *   now (ms).
 */
export function buildModel(input) {
  const { board, channels = [], tasks = {}, seen = null, marks = {}, now = 0 } = input
  const chans = projectChannels(board, channels)
  const receipts = marks.receipts || {}
  const asked = marks.asked || {}

  // Every task we know, by id, with its channel and project.
  const byId = new Map()
  const projectOf = new Map()
  for (const [project, list] of chans) {
    for (const ch of list) {
      for (const t of tasks[ch] || []) {
        byId.set(t.id, { ...t, channel: ch, project })
        projectOf.set(ch, project)
      }
    }
  }

  // Projects.
  const projects = []
  const idle = []
  for (const c of board?.collections || []) {
    for (const p of c.projects || []) {
      const orchs = p.orchestrators || []
      const orchOffline = orchs.length > 0 && orchs.every((o) => !o.online)
      const lines = []
      for (const ch of chans.get(p.project) || []) {
        for (const t of tasks[ch] || []) {
          if (t.kind === 'epic') continue
          const s = taskState(t, byId, orchOffline)
          const doneSince = s === 'done' && t.close_reason === 'done' && seen && (seen.ids[t.id] ?? 'new') !== 'closed'
          if (s === 'done' && !doneSince) continue
          const waits = s === 'later' ? (t.blocked_by || []).map((id) => byId.get(id)).filter((x) => x && x.status !== 'closed').map((x) => x.title) : []
          lines.push({
            id: t.id,
            channel: ch,
            title: t.title,
            state: s,
            who: t.claimed_by || (t.status === 'needs_human' || t.status === 'stuck' || t.status === 'in_progress' ? orchs[0]?.agent || '' : ''),
            updated: ms(t.updated),
            age: ms(t.updated) === null ? null : Math.max(0, now - ms(t.updated)),
            pr: t.properties?.pr_url ? { url: t.properties.pr_url, label: prLabel(t.properties.pr_url) } : null,
            note: s === 'later' ? 'after ' + waits.join(', ') : s === 'blocked' ? 'orchestrator offline' : t.attention_msg || '',
            isNew: Boolean(seen && !(t.id in seen.ids) && (ms(t.created) ?? 0) > seen.at),
          })
        }
      }
      lines.sort((a, b) => STATE_ORDER[a.state] - STATE_ORDER[b.state] || (a.updated ?? 0) - (b.updated ?? 0))
      const project = { name: p.project, collection: c.collection || '', orchestrators: orchs, orchOffline, tasks: lines, saga: (p.sagas || []).find((s) => s.state !== 'closed') || null }
      const work = lines.some((l) => l.state !== 'done')
      ;(work ? projects : idle).push(work ? project : p.project)
    }
  }
  const mark = (p) => {
    const has = (s) => p.tasks.some((t) => t.state === s)
    if (has('stuck')) return 'stuck'
    if (p.orchOffline) return 'offline'
    if (has('blocked')) return 'blocked'
    if (has('wait')) return 'wait'
    return 'run'
  }
  const RANK = { stuck: 0, blocked: 1, wait: 2, offline: 3, run: 4 }
  for (const p of projects) p.mark = mark(p)
  projects.sort((a, b) => RANK[a.mark] - RANK[b.mark] || a.name.localeCompare(b.name))

  // Needs you: the board's questions and gates, and its blocked spawns.
  let needs = []
  for (const q of allQuestions(board)) {
    const id = 'n:' + q.channel + '/' + q.task
    const t = byId.get(q.task)
    const verb = q.gate ? 'approve' : q.status === 'stuck' ? 'decide' : 'answer'
    const who = (t && t.claimed_by) || q.owner || ''
    needs.push({
      id,
      verb,
      project: q.project || projectOf.get(q.channel) || '',
      channel: q.channel,
      task: q.task,
      ask: q.title,
      detail: q.attention_msg || '',
      who,
      worker: (t && t.claimed_by) || '',
      owner: q.owner || '',
      why: verb === 'approve' ? 'saga waits' : verb === 'decide' ? 'worker stopped' : t && t.claimed_by ? 'worker waits' : 'orchestrator waits',
      since: ms(q.since),
      age: ms(q.since) === null ? null : Math.max(0, now - ms(q.since)),
      asked: asked[id] || null,
    })
  }
  for (const c of board?.collections || []) {
    for (const b of c.blocked || []) {
      const id = 'b:' + b.agent
      needs.push({
        id,
        verb: 'unblock',
        project: b.project || projectOf.get(b.channel) || '',
        channel: b.channel,
        task: null,
        agent: b.agent,
        ask: 'Spawn ' + b.agent + ' cannot start: ' + b.reason,
        detail: b.action ? 'Fix: ' + b.action : '',
        who: b.spawner || '',
        worker: '',
        owner: b.spawner || '',
        why: 'spawn blocked',
        since: ms(b.since),
        age: ms(b.since) === null ? null : Math.max(0, now - ms(b.since)),
        asked: asked[id] || null,
      })
    }
  }
  needs.sort((a, b) => NEED_ORDER[a.verb] - NEED_ORDER[b.verb] || (a.since ?? 0) - (b.since ?? 0))
  // A receipt replaces its row, or brings it back at its place while it lasts.
  for (const [id, r] of Object.entries(receipts)) {
    if (now - r.at >= RECEIPT_MS) continue
    const at = needs.findIndex((n) => n.id === id)
    const row = { ...(at >= 0 ? needs[at] : r.item), receipt: r.text }
    if (at >= 0) needs[at] = row
    else needs.splice(Math.min(r.index ?? needs.length, needs.length), 0, row)
  }

  // Quiet: an orchestrator offline while its project has open work.
  const quiet = projects
    .filter((p) => p.orchOffline)
    .map((p) => {
      const n = p.tasks.filter((t) => t.state !== 'done').length
      const orch = p.orchestrators.map((o) => o.agent).join(', ')
      return { id: 'o:' + p.name, verb: 'offline', project: p.name, ask: orch + ' offline: ' + n + (n === 1 ? ' task cannot start' : ' tasks cannot start'), who: orch, age: null }
    })

  // Changes since the snapshot: new tasks by channel, tasks done, and the person's own actions.
  const changes = []
  // A task the person acted on shows once: as their own change.
  const own = new Set((marks.changes || []).map((c) => c.task).filter(Boolean))
  if (seen) {
    const fresh = new Map()
    for (const t of byId.values()) {
      if (t.kind === 'epic') continue
      if (!(t.id in seen.ids) && (ms(t.created) ?? 0) > seen.at) {
        const key = t.channel
        if (!fresh.has(key)) fresh.set(key, [])
        fresh.get(key).push(t)
      } else if (t.status === 'closed' && t.close_reason === 'done' && (seen.ids[t.id] ?? 'closed') !== 'closed' && !own.has(t.id)) {
        const pr = t.properties?.pr_url ? prLabel(t.properties.pr_url) + ' ' : ''
        changes.push({ id: 'c:' + t.id, verb: /^release\b/i.test(t.title) ? 'released' : 'done', project: t.project, text: pr + t.title, who: t.claimed_by || '', at: ms(t.updated) })
      }
    }
    for (const [ch, list] of fresh) {
      const at = Math.max(...list.map((t) => ms(t.created) ?? 0))
      changes.push({ id: 'c:new:' + ch, verb: 'new', project: projectOf.get(ch) || '', text: list.length + (list.length === 1 ? ' task: ' : ' tasks: ') + list.map((t) => t.title).join(', '), who: '', at })
    }
  }
  for (const c of marks.changes || []) if (!seen || c.at > seen.at) changes.push(c)
  changes.sort((a, b) => (b.at ?? 0) - (a.at ?? 0))
  for (const c of changes) c.age = c.at === null || c.at === undefined ? null : Math.max(0, now - c.at)

  // Ships next: open tasks with a PR.
  const ships = []
  for (const p of projects) {
    for (const t of p.tasks) {
      if (!t.pr || t.state === 'done') continue
      ships.push({ id: 'x:' + t.channel + '/' + t.id, project: p.name, pr: t.pr, title: t.title, who: t.who, state: t.state, updated: t.updated })
    }
  }
  ships.sort((a, b) => (a.state === 'run' ? 0 : 1) - (b.state === 'run' ? 0 : 1) || (b.updated ?? 0) - (a.updated ?? 0))

  const agents = new Set()
  for (const p of projects) {
    for (const o of p.orchestrators) if (o.online) agents.add(o.agent)
    for (const t of p.tasks) if (t.who && t.state !== 'done') agents.add(t.who)
  }

  return {
    needs,
    quiet,
    changes,
    ships,
    projects,
    idle,
    agents: agents.size,
    open: needs.filter((n) => !n.receipt).length,
    seenAt: seen ? seen.at : null,
  }
}

/** The snapshot that x takes: every task's status now. */
export function snapshot(tasks, now) {
  const ids = {}
  for (const list of Object.values(tasks || {})) for (const t of list || []) ids[t.id] = t.status
  return { at: now, ids }
}

/** Every selectable item of the panel's list, in order: its id and kind. */
export function listItems(model) {
  return [
    ...model.needs.map((n) => ({ id: n.id, kind: 'need', section: 1 })),
    ...model.quiet.map((q) => ({ id: q.id, kind: 'quiet', section: 2 })),
    ...model.ships.map((s) => ({ id: s.id, kind: 'ship', section: 3 })),
    ...model.changes.map((c) => ({ id: c.id, kind: 'change', section: 4 })),
    ...model.projects.map((p) => ({ id: 'p:' + p.name, kind: 'project', section: 5 })),
  ]
}

/** The thing behind a list id. */
export function find(model, id) {
  if (!id) return null
  if (id.startsWith('n:') || id.startsWith('b:')) return { kind: 'need', item: model.needs.find((n) => n.id === id) }
  if (id.startsWith('o:')) return { kind: 'quiet', item: model.quiet.find((q) => q.id === id) }
  if (id.startsWith('x:')) return { kind: 'ship', item: model.ships.find((s) => s.id === id) }
  if (id.startsWith('c:')) return { kind: 'change', item: model.changes.find((c) => c.id === id) }
  if (id.startsWith('p:')) return { kind: 'project', item: model.projects.find((p) => 'p:' + p.name === id) }
  return null
}
