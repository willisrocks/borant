// Boards for the chief mod's visual tests, written where scripts/tui/borant-stub
// reads them. `fixture` is the mockup board; `big` adds the cases the fixture
// hides: many projects, long names, many needs-you items, idle projects; `empty`
// has projects and nothing open.
//
// Usage: node scripts/tui/board-variants.mjs <fixture|big|empty> <stub-dir>

import fs from 'fs'
import path from 'path'
import { board as fixtureBoard } from '../../integrations/claude-code/mod/fixtures/board.js'
import { tasks as fixtureTasks } from '../../integrations/claude-code/mod/fixtures/tasks.js'

const [variant = 'fixture', dir] = process.argv.slice(2)
if (!dir) {
  console.error('usage: board-variants.mjs <fixture|big|empty> <stub-dir>')
  process.exit(2)
}

const clone = (v) => JSON.parse(JSON.stringify(v))
const AT = fixtureBoard.at
const MIN = 60000
let serial = 900
const id = () => '01M5ZG' + String(serial++).padStart(20, '0')

function counts(c = {}) {
  return { open: 0, ready: 0, running: 0, blocked: 0, stuck: 0, needs_human: 0, done: 0, ...c }
}

function epic(channel, title, status, c) {
  return { channel, task: id(), title, status, issue: null, branch: null, pr_url: null, counts: counts(c) }
}

function project(name, orch, { saga, epics = [], tasks = {} } = {}) {
  const channel = 'personal-' + name
  const p = {
    project: name,
    mirror_issues: false,
    orchestrators: orch ? [{ agent: orch.agent, online: true, busy: Boolean(orch.busy), host: orch.host || null }] : [],
    sagas: [],
    epics: [],
    tasks: counts(tasks),
  }
  const list = epics.map(([title, status, c]) => epic(channel, title, status, c))
  if (saga) {
    p.sagas.push({
      channel,
      title: saga,
      issue: null,
      state: 'approved',
      gate: id(),
      epics: list,
      done: list.filter((e) => e.status === 'closed').length,
      total: list.length,
    })
  } else {
    p.epics = list
  }
  return p
}

function need(projectName, title, minutesAgo, attention, extra = {}) {
  return {
    project: projectName,
    channel: 'personal-' + projectName,
    task: id(),
    title,
    status: 'needs_human',
    attention_msg: attention,
    since: AT - minutesAgo * MIN,
    gate: false,
    owner: extra.owner || 'orch-' + projectName,
    ...extra,
  }
}

function big() {
  const b = clone(fixtureBoard)
  const c = b.collections[0]
  c.projects.push(
    project('roger-docs-api', { agent: 'orch-roger-docs', busy: true, host: 'fly-1' }, {
      saga: 'ROG-4199: LiteLLM proxy rollout across every environment',
      epics: [
        ['Route Bedrock models through the proxy with per-tenant keys', 'in_progress', { running: 2 }],
        ['Preprod smoke tests for the proxy', 'open', { open: 3 }],
        ['Prod cutover', 'open', { blocked: 1 }],
        ['Proxy image pinned in SST', 'closed', { done: 2 }],
      ],
      tasks: { running: 2, open: 3, blocked: 1, done: 2 },
    }),
    project('steve-agents', { agent: 'orch-steve', host: 'macbook' }, {
      saga: 'Nightly evals on Modal',
      epics: [
        ['Eval harness on Modal GPUs', 'in_progress', { running: 1, needs_human: 1 }],
        ['Scorecards in the scratchpad', 'open', { open: 2 }],
        ['Schedules for the eval runs', 'closed', { done: 1 }],
      ],
      tasks: { running: 1, open: 2, needs_human: 1, done: 1 },
    }),
    project('wellsky-knowledge-base', { agent: 'orch-wellsky', host: 'borant-1' }, { tasks: { open: 2 } }),
    project('agentbox', null, {
      epics: [['Sandboxed browser profiles per agent', 'in_progress', { running: 1 }]],
      tasks: { running: 1 },
    }),
    project('rich-brainstorm', { agent: 'orch-rb', host: 'macbook' }, {
      saga: 'Shared sessions',
      epics: [
        ['Session links that survive a restart', 'open', { blocked: 2 }],
        ['Live cursors', 'open', { open: 1 }],
      ],
      tasks: { blocked: 2, open: 1 },
    }),
    project('kata', { agent: 'orch-kata', host: 'borant-2' }, {
      epics: [['Attention states on cards', 'closed', { done: 4 }]],
      tasks: { done: 4 },
    }),
    project('xplainer', { agent: 'orch-xplainer', host: 'macbook' }, {
      epics: [['Narration voices per project', 'in_progress', { running: 1, stuck: 1 }]],
      tasks: { running: 1, stuck: 1 },
    }),
    project('roger-emr-observability-dashboards', { agent: 'orch-obs', busy: true, host: 'fly-2' }, {
      saga: 'Temporal queue health in Grafana',
      epics: [
        ['Queue depth panels', 'in_progress', { running: 1 }],
        ['Alert rules for stuck workflows', 'open', { open: 2 }],
      ],
      tasks: { running: 1, open: 2 },
    }),
    project('dotfiles', null, { tasks: {} }),
  )
  c.needs_you.push(
    need('roger-docs-api', 'Approve the prod cutover window for the LiteLLM proxy (Tuesday 06:00-07:00 UTC, preprod soak passed)', 95, 'gate · prod', { gate: true, owner: 'orch-roger-docs' }),
    need('steve-agents', 'Pick the GPU type for nightly evals: A10G is cheaper, H100 halves the run', 25, 'decision'),
    need('xplainer', 'Voice sample sounds wrong on long words; listen and say keep or redo', 12, 'stuck'),
    need('roger-emr-observability-dashboards', 'Grant the Grafana editor service account to the alerts folder', 180, 'access'),
  )
  return b
}

function empty() {
  const b = clone(fixtureBoard)
  for (const c of b.collections) {
    c.needs_you = []
    for (const p of c.projects) {
      for (const s of p.sagas) {
        s.state = 'closed'
        for (const e of s.epics) e.status = 'closed'
        s.done = s.total
      }
      for (const e of p.epics) e.status = 'closed'
      p.tasks = counts({ done: p.tasks.done || 0 })
    }
  }
  return b
}

const boards = { fixture: () => clone(fixtureBoard), big, empty }
if (!boards[variant]) {
  console.error('unknown variant: ' + variant)
  process.exit(2)
}
const out = boards[variant]()
fs.mkdirSync(dir, { recursive: true })
fs.writeFileSync(path.join(dir, 'board.json'), JSON.stringify(out))
for (const [channel, list] of Object.entries(fixtureTasks)) {
  fs.writeFileSync(path.join(dir, 'tasks-' + channel + '.json'), JSON.stringify(list))
}
console.log(variant + ': ' + out.collections[0].projects.length + ' projects, ' + out.collections[0].needs_you.length + ' need you')
