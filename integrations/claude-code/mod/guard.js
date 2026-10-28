// The chief's guard: a pure check of one Bash call, so the tests call it with
// no session. The chief delegates project work to the project's orchestrator,
// which runs it in a worker. A long session loses its skill to compaction, so
// the mod states the rule in the system prompt and puts project work that
// reaches the shell to the person as a permission prompt. Reads stay allowed.

/** The rule the chief's system prompt carries, whatever compaction dropped. */
export const DELEGATION_RULE = [
  'You are a Borant chief of staff. You own a collection; orchestrators run its projects, and their workers do the work.',
  'Never do project work in this session or in a subagent: no builds, tests, merges, deploys, releases, migrations, prod or database operations, project scripts, or long-running shell commands. That holds after the person approves the action too: approval decides what happens, not who runs it. Send the approved action to the project\'s orchestrator as a direct message; it runs it in a worker.',
  'When the person presses "Ask orchestrator" on the board, the mod sends the question to the channel\'s owner itself. Do not answer it yourself; relay the owner\'s reply.',
  'Load the borant:chief-of-staff skill again if it is not in your context.',
].join('\n')

// Commands that change a system, run a project's code, or reach prod.
const RUNS = new Set([
  'psql', 'mysql', 'sqlite3', 'mongosh', 'redis-cli',
  'aws', 'gcloud', 'az', 'kubectl', 'helm', 'terraform', 'tofu', 'pulumi',
  'fly', 'flyctl', 'heroku', 'ssh', 'scp', 'rsync', 'docker', 'podman',
  'nixos-rebuild', 'systemctl', 'cargo', 'npm', 'pnpm', 'yarn', 'bun', 'npx',
  'just', 'make', 'go', 'python', 'python3', 'node', 'deno', 'uv', 'pip',
  'rm', 'mv', 'cp', 'tee', 'dd', 'chmod', 'chown',
])
// The ones that only change something with some subcommands.
const SUBCOMMANDS = {
  git: ['push', 'merge', 'rebase', 'commit', 'reset', 'cherry-pick', 'revert', 'tag', 'clean'],
  gh: ['pr merge', 'pr create', 'release', 'workflow run', 'run rerun', 'repo delete', 'api -X', 'api --method'],
}
// The chief installs Borant releases on the person's laptop: only it can.
const ALLOWED_SCRIPTS = ['install_borant.sh']

/** The simple commands of a shell line, split on ;, &&, ||, |, and newlines. */
function commandsOf(line) {
  return String(line || '')
    .split(/\n|;|&&|\|\||\|/)
    .map((c) => c.trim())
    .filter(Boolean)
}

/** The first word, past `sudo`, `env`, and VAR=value prefixes. */
function words(command) {
  const all = command.split(/\s+/)
  let i = 0
  while (i < all.length && (all[i] === 'sudo' || all[i] === 'env' || /^[A-Za-z_][A-Za-z0-9_]*=/.test(all[i]))) i += 1
  return all.slice(i)
}

/**
 * Why a Bash call looks like project work, or null for a read. `input` is the
 * Bash tool's arguments: `{ command, run_in_background }`.
 */
export function projectWork(input) {
  const line = String(input?.command || '')
  if (ALLOWED_SCRIPTS.some((s) => line.includes(s)) && !/[;&|]/.test(line.replace(/2>&1/g, ''))) return null
  if (input?.run_in_background) return 'it runs in the background'
  for (const command of commandsOf(line)) {
    const w = words(command)
    const first = w[0] || ''
    const name = first.split('/').pop()
    if (RUNS.has(name)) return 'it runs `' + name + '`'
    if (/\.(sh|bash|py|rb|js|ts)$/.test(name) || (first.includes('/') && !first.startsWith('/usr/') && !first.startsWith('/bin/'))) {
      if (!ALLOWED_SCRIPTS.includes(name)) return 'it runs the script `' + name + '`'
    }
    if ((name === 'bash' || name === 'sh' || name === 'zsh') && w[1] && !w[1].startsWith('-')) {
      return 'it runs the script `' + w[1].split('/').pop() + '`'
    }
    const sub = SUBCOMMANDS[name]
    if (sub) {
      const rest = w.slice(1).join(' ')
      const hit = sub.find((s) => rest === s || rest.startsWith(s + ' '))
      if (hit) return 'it runs `' + name + ' ' + hit + '`'
    }
    if (name === 'sed' && w.some((x) => x === '-i' || x.startsWith('-i'))) return 'it edits a file in place'
    // A redirect into a file writes it; /dev/null and 2>&1 do not.
    const target = command.match(/(?:^|[^0-9&])>>?\s*([^\s&|;]+)/)
    if (target && !target[1].startsWith('/dev/')) return 'it writes `' + target[1] + '`'
  }
  return null
}

/** The permission prompt's text for a call the guard holds. */
export function guardReason(why) {
  return (
    'Chief guard: this looks like project work (' + why + '). The project\'s orchestrator runs it in a worker, ' +
    'even after the person approved it: send it there. Allow only if the person asked the chief itself to run this exact command.'
  )
}
