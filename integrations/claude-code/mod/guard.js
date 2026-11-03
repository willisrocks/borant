// The chief's guard: a pure check of one Bash call, so the tests call it with
// no session. The chief delegates project work to the project's orchestrator,
// which runs it in a worker. A long session loses its skill to compaction, so
// the mod states the rule in the system prompt and refuses project work that
// reaches the shell, with a reason the model reads. The person gets no
// prompt. Reads, and writes into temporary folders, stay allowed.

/** The rule the chief's system prompt carries, whatever compaction dropped. */
export const DELEGATION_RULE = [
  'You are a Borant chief of staff. You own a collection; orchestrators run its projects, and their workers do the work.',
  'Never do project work in this session or in a subagent: no builds, tests, merges, deploys, releases, migrations, prod or database operations, project scripts, or long-running shell commands. That holds after the person approves the action too: approval decides what happens, not who runs it. Send the approved action to the project\'s orchestrator as a direct message; it runs it in a worker.',
  'Sending a message the person approved (the slack CLI), and writing notes under your session scratchpad, are not project work.',
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
// Sending a message the person approved is not project work.
const ALLOWED_COMMANDS = new Set(['slack'])
// File commands that only touch scratch folders change no project file.
const FILE_OPS = new Set(['rm', 'mv', 'cp', 'tee', 'dd', 'chmod', 'chown'])

/**
 * The line without the bodies of its heredocs. A body is text for the command
 * (an issue description, a script on stdin), never a command of the line, and
 * a quoted tag (<<'EOF') makes it literal too. The `<<TAG` line stays, so the
 * command that reads the body is still checked. A heredoc with no closing
 * line stays as it is.
 */
function stripHeredocs(line) {
  let s = String(line || '')
  const open = /(^|[^<])<<(-?)[ \t]*(?:'([^'\n]+)'|"([^"\n]+)"|([A-Za-z_][A-Za-z0-9_]*))/g
  let from = 0
  for (;;) {
    open.lastIndex = from
    const m = open.exec(s)
    if (!m) return s
    const tag = m[3] ?? m[4] ?? m[5]
    const eol = s.indexOf('\n', m.index + m[0].length)
    from = m.index + m[0].length
    if (eol < 0) return s
    let at = eol + 1
    let end = -1
    while (at <= s.length) {
      let next = s.indexOf('\n', at)
      if (next < 0) next = s.length
      const text = s.slice(at, next)
      if ((m[2] ? text.trim() : text.replace(/\r$/, '')) === tag) {
        end = next
        break
      }
      if (next >= s.length) break
      at = next + 1
    }
    if (end < 0) continue
    // Keep the newline that ends the `<<TAG` line, drop the body and the tag.
    s = s.slice(0, eol + 1) + s.slice(Math.min(end + 1, s.length))
    from = eol + 1
  }
}

/**
 * The simple commands of a shell line, split on ;, &&, ||, |, and newlines
 * outside quotes and outside `$( )`. Inside double quotes `$(` opens a new
 * context, where quotes start fresh, as in the shell.
 */
function commandsOf(line) {
  const out = []
  let cur = ''
  // Open contexts: '"', "'", or '(' for $( ) and ( ).
  const stack = []
  const s = stripHeredocs(line)
  for (let i = 0; i < s.length; i += 1) {
    const c = s[i]
    const top = stack[stack.length - 1]
    if (c === '\\' && top !== "'" && i + 1 < s.length) {
      cur += c + s[i + 1]
      i += 1
    } else if (top === "'" || top === 'a') {
      if (c === "'") stack.pop()
      cur += c
    } else if (top === '"') {
      if (c === '"') stack.pop()
      else if (c === '$' && s[i + 1] === '(') {
        stack.push('(')
        cur += '$'
        i += 1
        cur += '('
        continue
      }
      cur += c
    } else if (c === '"' || c === "'") {
      // $'...' is ANSI-C quoting: a backslash escapes the next character.
      stack.push(c === "'" && s[i - 1] === '$' ? 'a' : c)
      cur += c
    } else if ((c === '$' && s[i + 1] === '(') || (top === '(' && c === '(')) {
      stack.push('(')
      cur += c
      if (c === '$') {
        i += 1
        cur += '('
      }
    } else if (top === '(') {
      if (c === ')') stack.pop()
      cur += c
    } else if (c === '\n' || c === ';' || c === '|' || (c === '&' && s[i + 1] === '&')) {
      if ((c === '|' || c === '&') && s[i + 1] === c) i += 1
      out.push(cur)
      cur = ''
    } else {
      cur += c
    }
  }
  out.push(cur)
  return out.map((c) => c.trim()).filter(Boolean)
}

/**
 * The words of a simple command, split on blanks outside quotes, quotes
 * removed. A `$( )` keeps its text as it is, quotes and blanks included.
 */
function split(command) {
  const out = []
  let cur = null
  const stack = []
  const s = String(command)
  for (let i = 0; i < s.length; i += 1) {
    const c = s[i]
    const top = stack[stack.length - 1]
    if (top === '(') {
      cur += c
      if (c === '"' || c === "'") {
        // A quoted run inside $( ) may hold a ")": skip to its end.
        const end = s.indexOf(c, i + 1)
        if (end > 0) {
          cur += s.slice(i + 1, end + 1)
          i = end
        }
      } else if (c === '(') stack.push('(')
      else if (c === ')') stack.pop()
    } else if (c === '\\' && top !== "'" && i + 1 < s.length) {
      cur = (cur ?? '') + s[i + 1]
      i += 1
    } else if (top === "'" || top === 'a') {
      if (c === "'") stack.pop()
      else cur += c
    } else if (top === '"') {
      if (c === '"') stack.pop()
      else if (c === '$' && s[i + 1] === '(') {
        stack.push('(')
        cur += '$('
        i += 1
      } else cur += c
    } else if (c === '"' || c === "'") {
      stack.push(c === "'" && s[i - 1] === '$' ? 'a' : c)
      cur = cur ?? ''
      if (c === "'" && s[i - 1] === '$' && cur === '$') cur = ''
    } else if (c === '$' && s[i + 1] === '(') {
      stack.push('(')
      cur = (cur ?? '') + '$('
      i += 1
    } else if (/\s/.test(c)) {
      if (cur !== null) out.push(cur)
      cur = null
    } else {
      cur = (cur ?? '') + c
    }
  }
  if (cur !== null) out.push(cur)
  return out
}

/** The first word, past `sudo`, `env`, and VAR=value prefixes. */
function words(command) {
  const all = split(command)
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
  const vars = assignments(line)
  for (const command of commandsOf(line)) {
    const w = words(command)
    const first = w[0] || ''
    const name = first.split('/').pop()
    if (ALLOWED_COMMANDS.has(name)) continue
    if (RUNS.has(name) && !(FILE_OPS.has(name) && scratchOnly(name, w, vars))) return 'it runs `' + name + '`'
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
    if (name === 'sed' && w.some((x) => x === '-i' || x.startsWith('-i')) && !sedScratchOnly(w, vars)) return 'it edits a file in place'
    // A redirect into a file writes it; /dev/null, 2>&1, and a temporary
    // folder do not count.
    // A `>` inside quotes is text; a quoted target keeps its spaces.
    const quoted = []
    const masked = command.replace(/\$'(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*"|'[^']*'/g, (q) => '\u0000' + (quoted.push(q.replace(/^\$?['"]|['"]$/g, '')) - 1) + '\u0000')
    const target = masked.match(/(?:^|[^0-9&])>>?\s*([^\s&|;]+)/)
    const path = target && target[1].replace(/\u0000(\d+)\u0000/g, (_, n) => quoted[Number(n)])
    if (path && !scratch(path, vars)) return 'it writes `' + path + '`'
  }
  return null
}

// Folders a redirect may write: devices, temporary folders (macOS keeps
// $TMPDIR under /var/folders), and a Claude Code scratchpad.
const SCRATCH_RE = /\/\.claude[^/]*\/projects\/[^/]+\/./
const SCRATCH = ['/dev/', '/tmp/', '/private/tmp/', '/var/folders/', '/private/var/folders/', '$TMPDIR/', '${TMPDIR}/']

/** True when a redirect into `path` writes no project file. */
function scratch(path, vars = {}) {
  let p = path.replace(/^["']|["']$/g, '')
  // A variable the line assigns, such as d=/tmp/.../scratchpad, then $d/x.txt.
  p = p.replace(/^\$(?:\{(\w+)\}|(\w+))(?=\/)/, (m, a, b) => vars[a ?? b] ?? m)
  if (p.split('/').includes('..')) return false
  return SCRATCH.some((dir) => p.startsWith(dir)) || p.includes('/scratchpad/') || SCRATCH_RE.test(p)
}

/** The NAME=value words of a line, quotes removed. */
function assignments(line) {
  const vars = {}
  for (const m of String(line).matchAll(/(?:^|[\s;&|])([A-Za-z_]\w*)=("[^"]*"|'[^']*'|[^\s;&|]*)/g)) {
    vars[m[1]] = m[2].replace(/^["']|["']$/g, '')
  }
  return vars
}

/** True when the file command writes only scratch paths. */
function scratchOnly(name, w, vars) {
  let args = w.slice(1).filter((a) => !a.startsWith('-') || a === '-')
  if (name === 'dd') args = args.map((a) => a.replace(/^(?:of|if)=/, '')).filter((a) => a.includes('/'))
  if (name === 'chmod' || name === 'chown') args = args.slice(1)
  if (name === 'cp') args = args.slice(-1)
  return args.length > 0 && args.every((a) => scratch(a, vars))
}

/** True when `sed -i` edits only scratch files (the words after the script). */
function sedScratchOnly(w, vars) {
  const args = w.slice(1).filter((a) => !a.startsWith('-'))
  const files = w.some((x) => x === '-e' || x === '-f' || x.startsWith('--expression')) ? args : args.slice(1)
  return files.length > 0 && files.every((a) => scratch(a, vars))
}

/** The reason the model reads for a call the guard refuses. */
export function guardReason(why) {
  return (
    'Chief guard: refused, this looks like project work (' + why + '). The project\'s orchestrator runs it in a worker, ' +
    'even after the person approved it: send it there as a direct message. Do not reword the command to get past this guard.'
  )
}
