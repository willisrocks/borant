// A stand-in for `borant mcp` in the chief mod's visual tests: an MCP server
// on stdio with the two tools a Done's prompt asks the chief to use. Each
// call goes to calls.log. task_close closes the task in the stub's files and
// raises an event, as the hub would; send only records the message, and the
// scenario plays the owner who closes the task.
//
// borant-stub runs it for `borant mcp`, with BORANT_STUB_DIR set.

import fs from 'fs'
import path from 'path'
import readline from 'readline'

const dir = process.env.BORANT_STUB_DIR
const file = (name) => path.join(dir, name)
const log = (line) => fs.appendFileSync(file('calls.log'), Date.now() / 1000 + ' mcp ' + line + '\n')

const TOOLS = [
  {
    name: 'task_close',
    description: 'Close a task when its work is done and verified.',
    inputSchema: {
      type: 'object',
      properties: {
        channel: { type: 'string' },
        id: { type: 'string' },
        reason: { type: 'string', description: 'One of: done, wontfix, duplicate, superseded.' },
        message: { type: 'string', description: 'What was done and how it was verified.' },
      },
      required: ['channel', 'id', 'reason', 'message'],
    },
  },
  {
    name: 'send',
    description: "Send a direct message to an agent, or a broadcast to '#channel'.",
    inputSchema: {
      type: 'object',
      properties: { to: { type: 'string' }, text: { type: 'string' } },
      required: ['to', 'text'],
    },
  },
]

function closeTask(channel, id) {
  const board = JSON.parse(fs.readFileSync(file('board.json')))
  for (const c of board.collections) c.needs_you = (c.needs_you || []).filter((q) => q.task !== id)
  fs.writeFileSync(file('board.json'), JSON.stringify(board))
  const tasks = file('tasks-' + channel + '.json')
  const list = fs.existsSync(tasks) ? JSON.parse(fs.readFileSync(tasks)) : []
  for (const t of list) if (t.id === id) t.status = 'closed'
  fs.writeFileSync(tasks, JSON.stringify(list))
  fs.appendFileSync(file('events.jsonl'), JSON.stringify({ kind: 'task.status', channel }) + '\n')
}

function call(name, args) {
  if (name === 'task_close') {
    log('task_close ' + args.channel + ' ' + args.id)
    closeTask(args.channel, args.id)
    return 'closed ' + args.channel + '/' + args.id
  }
  if (name === 'send') {
    log('send ' + args.to)
    return 'sent to ' + args.to
  }
  throw new Error('unknown tool ' + name)
}

const reply = (id, body) => process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, ...body }) + '\n')

readline.createInterface({ input: process.stdin }).on('line', (line) => {
  let msg
  try {
    msg = JSON.parse(line)
  } catch {
    return
  }
  if (msg.id === undefined) return
  switch (msg.method) {
    case 'initialize':
      return reply(msg.id, {
        result: { protocolVersion: msg.params?.protocolVersion || '2025-06-18', capabilities: { tools: {} }, serverInfo: { name: 'borant', version: 'stub' } },
      })
    case 'tools/list':
      return reply(msg.id, { result: { tools: TOOLS } })
    case 'tools/call':
      try {
        return reply(msg.id, { result: { content: [{ type: 'text', text: call(msg.params.name, msg.params.arguments || {}) }] } })
      } catch (err) {
        return reply(msg.id, { result: { content: [{ type: 'text', text: String(err.message) }], isError: true } })
      }
    case 'ping':
      return reply(msg.id, { result: {} })
    default:
      return reply(msg.id, { error: { code: -32601, message: 'method not found: ' + msg.method } })
  }
})
