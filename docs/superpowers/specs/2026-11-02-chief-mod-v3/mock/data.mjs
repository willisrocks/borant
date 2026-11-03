// The sample swarm that every mockup from round 3 on draws. Every count on a
// screen is derived from these records. Now is 14:47. The person last marked
// the changes seen (x) at 14:02.

export const NOW = '14:47'
export const LOOK = '14:02'

// Task states: wait (needs the person), stuck, run (claimed, working), silent
// (claimed, no tool call for 30 minutes), ready (open, can start, nobody on
// it), blocked (cannot start: a blocked spawn or an offline owner), later
// (open, waits on another task), done.
export const PROJECTS = [
  { coll: 'work', name: 'roger-support', orch: 'orch-roger-support', orchState: 'online', tasks: [
    { id: 'rel', s: 'wait', title: 'Release v1.0.23', short: 'Release v1.0.23', who: 'orch-roger-support', age: '3h', note: 'waits on Preprod QA' },
    { id: 'qa', s: 'blocked', title: 'Preprod QA of v1.0.23', short: 'Preprod QA', who: 'helper-qa-preprod', age: '3h', note: 'spawn blocked' },
  ] },
  { coll: 'work', name: 'roger-emr', orch: 'orch-roger-emr', orchState: 'online', tasks: [
    { id: 'bill', s: 'stuck', title: 'Billing export: CSV for Medicare', short: 'Billing export', who: 'wkr-emr-7', age: '38m', note: 'needs the rounding rule' },
    { id: 'notes', s: 'run', title: 'Visit notes autosave', short: 'Notes autosave', who: 'wkr-emr-2', age: '6m', pr: '#2440', prState: 'ready to merge', checks: '7 of 7 passed', review: 'approved 14:36' },
    { id: 'oasis', s: 'run', title: 'OASIS export: M1033 field order', short: 'OASIS export', who: 'wkr-emr-4', age: '7m' },
    { id: 'fax', s: 'ready', title: 'Referral intake: fax OCR retry', short: 'Fax OCR retry' },
    { id: 'sched', s: 'ready', title: 'Scheduler: drag a visit to another day', short: 'Scheduler drag' },
    { id: 'audit', s: 'ready', title: 'Audit log export', short: 'Audit log' },
    { id: 'mcd', s: 'later', title: 'Billing export: Medicaid', short: 'Medicaid export', note: 'after Billing export' },
  ] },
  { coll: 'personal', name: 'boopmark', orch: 'orch-boopmark-2', orchState: 'online', tasks: [
    { id: 'e2e', s: 'wait', title: 'E2E: capture login flow', short: 'E2E login', who: 'wkr-boop-1', age: '12m', note: 'waits on you' },
    { id: 'search', s: 'silent', title: 'Search: rank by recency', short: 'Search ranking', who: 'wkr-boop-2', age: '31m', note: 'no tool call for 31m' },
    { id: 'pin', s: 'ready', title: 'Tag import from Pinboard', short: 'Pinboard import' },
    { id: 'dark', s: 'ready', title: 'Dark mode for the extension', short: 'Dark mode' },
  ] },
  { coll: 'personal', name: 'borant', orch: 'orchestrator', orchState: 'online', saga: { title: 'Elastic compute', note: 'epic 1 done, epic 2 running, epics 3 to 5 wait for your approval' }, doneBefore: 23, tasks: [
    { id: 'gate', s: 'wait', title: 'Approve epics 3 to 5 of saga Elastic compute', short: 'Approve epics 3-5', who: 'orchestrator', age: '42m', note: 'waits on you' },
    { id: 'backup', s: 'run', title: 'Hub backup to object storage', short: 'Hub backup', who: 'wkr-borant-3', age: '18m', pr: '#203', prState: 'CI running', checks: '3 of 7 done, none failed', review: 'not requested yet' },
    { id: 'live', s: 'run', title: 'Liveness sweep: retry failed resumes', short: 'Liveness sweep', who: 'wkr-borant-5', age: '4m', pr: '#204', prState: 'review requested', checks: '7 of 7 passed', review: 'requested 14:43, nobody yet' },
    { id: 'mod', s: 'run', title: 'Chief mod v3 design', short: 'Chief mod v3', who: 'mod-v3', age: '2m' },
    { id: 'move', s: 'later', title: 'Hub move to borant-hub', short: 'Hub move', note: 'after your approval' },
    { id: 'golden', s: 'later', title: 'Golden image for pool boxes', short: 'Golden image', note: 'after your approval' },
    { id: 'del', s: 'later', title: 'Delete borant-2', short: 'Delete borant-2', note: 'after Hub backup' },
    { id: 'chief', s: 'done', title: 'Never launch, relaunch, or move a chief', short: 'Never launch a chief', who: 'wkr-borant-1', age: '26m', pr: '#198', prState: 'merged' },
    { id: 'rel', s: 'done', title: 'Release v0.19.2', short: 'Release v0.19.2', who: 'orchestrator', age: '20m' },
  ] },
  { coll: 'personal', name: 'taglab', orch: 'orch-taglab', orchState: 'offline 1h', tasks: [
    { id: 'csv', s: 'blocked', title: 'Label export to CSV', short: 'Label export', note: 'orchestrator offline' },
    { id: 'ocr', s: 'blocked', title: 'Fix the flaky OCR test', short: 'Flaky OCR test', note: 'orchestrator offline' },
  ] },
  { coll: 'work', name: 'steve', orch: 'orch-steve', orchState: 'online', tasks: [
    { id: 'slack', s: 'run', title: 'Slack threading for replies', short: 'Slack threading', who: 'wkr-steve-1', age: '22m' },
    { id: 'dm', s: 'ready', title: 'DM digest', short: 'DM digest', isNew: true },
    { id: 'hook', s: 'ready', title: 'Retry webhooks', short: 'Retry webhooks', isNew: true },
    { id: 'lin', s: 'ready', title: 'Rate-limit the Linear sync', short: 'Linear rate limit', isNew: true },
  ] },
  { coll: 'work', name: 'roger-docs-app', orch: 'orch-docs-app', orchState: 'online', tasks: [
    { id: 'up', s: 'run', title: 'Upload progress bar', short: 'Upload progress', who: 'wkr-app-1', age: '15m', pr: '#88', prState: 'CI running', checks: '5 of 9 done, none failed', review: 'not requested yet' },
  ] },
  { coll: 'work', name: 'roger-docs-api', orch: 'orch-docs-api', orchState: 'online', tasks: [
    { id: 'rl', s: 'run', title: 'Rate-limit headers', short: 'Rate-limit headers', who: 'wkr-docs-1', age: '9m' },
    { id: 'ra', s: 'done', title: 'Retry-After on 429', short: 'Retry-After on 429', who: 'wkr-docs-1', age: '17m', pr: '#612', prState: 'merged' },
  ] },
]
export const IDLE = ['home-infra', 'litellm']

/** What needs the person, most urgent first. `why` says what waits on it. */
export const NEEDS = [
  { id: 'n0', g: '⊘', st: 'r', verb: 'unblock', proj: 'roger-support', task: 'qa', ask: 'Spawn helper-qa-preprod cannot start: no host has bundle github-work', why: 'holds release v1.0.23', who: 'chief-work', age: '3h',
    state: 'queued since 11:02, blocked: missing bundle github-work',
    detail: ['The hub queued helper-qa-preprod at 11:02 for "Preprod QA of v1.0.23".', 'No host offers bundle github-work, so no host can take it.'],
    fixes: [['u', 'retry the spawn now (after you add the bundle to a host)'], ['w', 'spawn it without bundle github-work'], ['a', 'ask chief-work to fix it']] },
  { id: 'n1', g: '■', st: 'r', verb: 'decide', proj: 'roger-emr', task: 'bill', ask: 'Billing export: round visit minutes per visit or per claim?', why: 'worker stopped', who: 'wkr-emr-7', age: '38m',
    state: 'stuck since 14:09: wkr-emr-7 stopped and asks for a rule',
    detail: ['3 claim fixtures fail on the rounding of visit minutes. "Round per visit', '(what we do today) or per claim (CMS manual 10.1)? Per claim changes 2', 'old exports."'],
    recommend: ['orch-roger-emr', 'Per claim: it matches CMS. Re-run the 2 old exports.'] },
  { id: 'n2', g: '▲', st: 'y', verb: 'answer', proj: 'boopmark', task: 'e2e', ask: 'E2E login: staging creds from bundle boopmark-staging, or a recorded fixture?', why: 'worker waits', who: 'wkr-boop-1', age: '12m',
    state: 'waits for your answer since 14:35',
    detail: ['"The login E2E needs a real account. Use the staging creds from bundle', 'boopmark-staging, or record a fixture once and replay it?"'],
    recommend: ['orch-boopmark-2', 'The fixture: no secret in CI, and the E2E runs offline.'] },
  { id: 'n3', g: '▲', st: 'y', verb: 'approve', proj: 'borant', task: 'gate', ask: 'Saga Elastic compute: start epics 3 to 5? They add 3 hosts, about €40 a month', why: 'epics 3 to 5 wait', who: 'orchestrator', age: '42m',
    state: 'waits for your approval since 14:05',
    detail: ['Epic 1 is done and epic 2 runs. Epics 3 to 5 (hub move, golden image,', 'pool join) need 3 new hosts. orchestrator asks before it spends money.'],
    recommend: ['orchestrator', 'Approve: the hosts are needed for the pool; I stop at epic 3 if it costs more.'] },
]

/** Work that went quiet. */
export const QUIET = [
  { id: 'q0', g: '◌', st: 'o', verb: 'silent', proj: 'boopmark', task: 'search', ask: 'Search: rank by recency: no tool call for 31m', who: 'wkr-boop-2', age: '31m' },
  { id: 'q1', g: '◌', st: 'o', verb: 'offline', proj: 'taglab', ask: 'orch-taglab offline: 2 tasks cannot start', who: 'orch-taglab', age: '1h' },
]

/** What changed since the person last pressed x, newest first. */
export const CHANGES = [
  { id: 'c0', g: '+', st: 'c', verb: 'new', proj: 'steve', text: '3 tasks: DM digest, Retry webhooks, Rate-limit the Linear sync', who: 'orch-steve', age: '9m' },
  { id: 'c1', g: '✓', st: 'g', verb: 'approved', proj: 'roger-emr', text: 'PR #2440 Visit notes autosave', who: 'a reviewer', age: '11m' },
  { id: 'c2', g: '✓', st: 'g', verb: 'merged', proj: 'roger-docs-api', text: 'PR #612 Retry-After on 429', who: 'wkr-docs-1', age: '17m' },
  { id: 'c3', g: '✓', st: 'g', verb: 'released', proj: 'borant', text: 'v0.19.2', who: 'orchestrator', age: '20m' },
  { id: 'c4', g: '✓', st: 'g', verb: 'merged', proj: 'borant', text: 'PR #198 Never launch, relaunch, or move a chief', who: 'wkr-borant-1', age: '26m' },
]

export const GLYPH = { wait: ['▲', 'y'], stuck: ['■', 'r'], run: ['●', 'b'], silent: ['◌', 'o'], ready: ['○', 't'], blocked: ['⊘', 'r'], later: ['◷', 'dim'], done: ['✓', 'g'] }

/** How close a PR is to merge: lower ships sooner. */
export const PR_RANK = { 'ready to merge': 0, 'review requested': 1, 'CI running': 2, merged: 9 }
export const PR_STYLE = { 'ready to merge': 'g', 'review requested': 'y', 'CI running': 'dim', merged: 'g' }

/** Open PRs, closest to merge first: the same order everywhere. */
export function ships() {
  return PROJECTS.flatMap((p) => p.tasks.filter((t) => t.pr && t.s !== 'done').map((t) => ({ proj: p.name, ...t })))
    .sort((a, b) => PR_RANK[a.prState] - PR_RANK[b.prState])
    .map((t, i) => ({ id: 'x' + i, ...t }))
}

/** A project's next PR: its open PR closest to merge. */
export function nextPr(p) {
  return ships().find((s) => s.proj === p.name) || null
}

/** Every agent with work: the orchestrators and each task's worker, once. */
export const agents = () => new Set([...PROJECTS.map((p) => p.orch), ...PROJECTS.flatMap((p) => p.tasks.filter((t) => t.who && t.s !== 'done').map((t) => t.who))])

export function project(name) {
  return PROJECTS.find((p) => p.name === name)
}
