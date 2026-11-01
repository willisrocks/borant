// The chief's guard and its rule in the system prompt.
import { expect, mock, test } from 'claude-code/testing'
import { DELEGATION_RULE, projectWork } from '../guard.js'

const SESSION = { surface: 'terminal', isInteractive: true, cwd: '/work' } as const
const COMPOSE = {
  model: 'claude-opus-5-5',
  promptModel: 'claude-opus-5-5',
  surfaces: ['terminal'],
  tools: ['Bash'],
  outputStyle: null,
  traits: [],
} as const

test('the guard holds project work and passes reads', () => {
  for (const command of [
    './steve-688.sh inspect',
    'bash scripts/cancel-v2.sh --prod',
    'AWS_PROFILE=prod aws ecs run-task --cluster steve',
    'psql "$DATABASE_URL" -c "update jobs set state = 1"',
    'cargo test -p borant-core',
    'git push origin main',
    'gh pr merge 12 --squash',
    'sed -i s/a/b/ config.toml',
    'cat notes > notes-copy.txt',
    'echo x >> ./config.toml',
    // A Python script on stdin still runs Python.
    'P="/Users/chris/Library/Application Support/x/.claude.json"; python3 - "$P" <<\'EOF\'',
    'echo "a b" > "notes copy.txt"',
  ]) {
    expect(projectWork({ command })).not.toBeNull()
  }
  expect(projectWork({ command: 'sleep 600', run_in_background: true })).toBe('it runs in the background')
  for (const command of [
    'borant board --json',
    'git status',
    'git log --oneline -5',
    'gh pr view 12',
    'ls -la 2>&1 | head',
    'grep -rn TODO . > /dev/null',
    // Quotes keep a value with spaces in one word, and a > inside them is text.
    'P="/Users/chris/Library/Application Support/orca/x/.claude.json"; jq . "$P"',
    'grep -n "a > b; c | d" notes.md',
    'echo hi > "/tmp/with space/x.txt"',
    // A temporary folder or a scratchpad is no project file.
    'borant board --json > /tmp/board.json',
    'jq . board.json > /private/tmp/b.json',
    'ls > /var/folders/x1/T/claude-501/scratchpad/ls.txt',
    'echo hi > /tmp/claude-1000/proj/sess/scratchpad/note.md',
    'jq . board.json',
    // Only the person's laptop can install a release, and the chief does it.
    '~/.bin/install_borant.sh v0.18.6',
  ]) {
    expect(projectWork({ command })).toBeNull()
  }
})

test('a chief is refused project work, with no prompt, and only a chief', async ($, on) => {
  mock.env(on, { BORANT_ROLE: 'chief', BORANT_COS_BOARD: 'fixture' })
  mock.clock(on)
  on('command.register', () => ({ value: undefined }))
  on('session.start', () => ({ cwd: '/work' }))
  on('tool.check', () => ({ decision: 'allow' }))
  on('prompt.compose', () => ({ sections: [{ id: 'intro', text: 'You are Claude Code.', scope: 'shared' }] }))
  await $.session.start({ ...SESSION })

  const held = await $.tool.check({ tool: 'Bash', input: { command: './steve-688.sh inspect' } })
  expect(held.decision).toBe('deny')
  expect(held.reason).toContain('orchestrator runs it in a worker')
  expect((await $.tool.check({ tool: 'Bash', input: { command: 'borant board --json' } })).decision).toBe('allow')

  const { sections } = await $.prompt.compose(COMPOSE)
  const rule = sections.find((s) => s.id === 'borant:chief-delegation')
  expect(rule?.text).toBe(DELEGATION_RULE)
  expect(rule?.scope).toBe('session')
})

test('another session gets no guard and no rule', async ($, on) => {
  mock.env(on, {})
  mock.clock(on)
  on('session.start', () => ({ cwd: '/work' }))
  on('tool.check', () => ({ decision: 'allow' }))
  on('prompt.compose', () => ({ sections: [{ id: 'intro', text: 'You are Claude Code.', scope: 'shared' }] }))
  await $.session.start({ ...SESSION })
  expect((await $.tool.check({ tool: 'Bash', input: { command: './steve-688.sh inspect' } })).decision).toBe('allow')
  expect((await $.prompt.compose(COMPOSE)).sections.map((s) => s.id)).toEqual(['intro'])
})
